//! 软件包数据库：pacman、dpkg、rpm、Windows 已安装程序和 Homebrew。
//!
//! 文件按目录分组；扫描器在进入目录时取得编号，随后只用文件名二分查找。
//! 加载过程中允许缺失和损坏的数据库，不会因此使扫描失败。

mod loaders;
mod path;
#[cfg(any(windows, test))]
mod windows;

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::i18n::Text;
use crate::platform::Os;
use crate::platform::path::{cmp_path_key, cmp_paths, path_key};

/// 一个文件的软件包信息；仅备份登记而没有归属时，`owner` 为 `None`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileInfo<'a> {
    pub owner: Option<&'a str>,
    pub backup: bool,
}

#[derive(Debug)]
struct FileRecord {
    name: Box<[u8]>,
    owner: Option<NonZeroU32>,
    backup: bool,
}

/// 状态栏需要的软件包加载信息；字段与旧版 `status.pkg` 相同。
#[derive(Debug, serde::Serialize)]
pub struct PkgStatus<'a> {
    pub manager: Option<&'static str>,
    pub label: Option<&'a Text>,
    pub packages: usize,
    pub files: usize,
    pub error: Option<&'a Text>,
}

/// 可供多个扫描线程共享的只读软件包索引。
#[derive(Debug)]
pub struct PkgDb {
    os: Os,
    directory_index: HashMap<Vec<u8>, u32>,
    directories: Vec<Vec<FileRecord>>,
    packages: Vec<String>,
    package_index: HashMap<String, NonZeroU32>,
    apps: Vec<(PathBuf, String)>,
    nfiles: usize,
    pub manager: Option<&'static str>,
    pub label: Option<Text>,
    pub npkgs: usize,
    /// 加载耗时，单位为秒，与旧版 `load_time` 一致。
    pub load_time: f64,
    pub error: Option<Text>,
}

impl Default for PkgDb {
    fn default() -> Self {
        Self::new(Os::current())
    }
}

impl PkgDb {
    /// 构造空数据库，可用于注入样例；不读取环境或用户文件。
    pub fn new(os: Os) -> Self {
        Self {
            os,
            directory_index: HashMap::new(),
            directories: Vec::new(),
            packages: Vec::new(),
            package_index: HashMap::new(),
            apps: Vec::new(),
            nfiles: 0,
            manager: None,
            label: None,
            npkgs: 0,
            load_time: 0.0,
            error: None,
        }
    }

    /// 读取当前系统数据库。应在后台线程调用；读取错误保存在 `error` 中。
    pub fn load() -> Self {
        let started = Instant::now();
        let mut db = Self::default();
        #[cfg(windows)]
        db.read_windows();
        #[cfg(target_os = "macos")]
        db.read_homebrew(&[PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")]);
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let pacman = Path::new("/var/lib/pacman/local");
            let dpkg = Path::new("/var/lib/dpkg/info");
            if pacman.is_dir() {
                db.read_pacman(pacman);
            } else if dpkg.is_dir() {
                db.read_dpkg(dpkg);
            } else if loaders::command_exists("rpm") {
                db.read_rpm();
            }
        }
        db.load_time = started.elapsed().as_secs_f64();
        db
    }

    /// 从指定 pacman 本地数据库目录读取，便于测试和离线分析。
    pub fn load_pacman(root: &Path) -> Self {
        let started = Instant::now();
        let mut db = Self::new(Os::Linux);
        db.read_pacman(root);
        db.load_time = started.elapsed().as_secs_f64();
        db
    }

    /// 从指定 dpkg `info` 目录读取。
    pub fn load_dpkg(root: &Path) -> Self {
        let started = Instant::now();
        let mut db = Self::new(Os::Linux);
        db.read_dpkg(root);
        db.load_time = started.elapsed().as_secs_f64();
        db
    }

    /// 解析 `rpm -qa --qf '[%{FILENAMES}\t%{NAME}\t%{FILEFLAGS:fflags}\n]'` 的输出。
    pub fn load_rpm_output(output: &[u8]) -> Self {
        let started = Instant::now();
        let mut db = Self::new(Os::Linux);
        db.read_rpm_output(output);
        db.load_time = started.elapsed().as_secs_f64();
        db
    }

    /// 读取给定 Homebrew 前缀下的 `Cellar` 和 `Caskroom`，不执行 brew 命令。
    pub fn load_homebrew_roots(prefixes: &[PathBuf]) -> Self {
        let started = Instant::now();
        let mut db = Self::new(Os::Macos);
        db.read_homebrew(prefixes);
        db.load_time = started.elapsed().as_secs_f64();
        db
    }

    /// 登记文件归属；同一路径再次登记时替换归属，保留备份标志。
    pub fn insert_file(&mut self, path: &Path, owner: &str) {
        let owner = self.package_id(owner);
        self.insert_record(path.as_os_str().as_encoded_bytes(), Some(owner), false);
    }

    /// 登记配置备份文件。可以在文件归属登记之前调用。
    pub fn insert_backup(&mut self, path: &Path) {
        self.insert_record(path.as_os_str().as_encoded_bytes(), None, true);
    }

    /// 登记应用安装目录；保持原始路径用于构造分类规则。
    pub fn insert_app_dir(&mut self, path: &Path, name: &str) {
        if let Some((_, value)) = self.apps.iter_mut().find(|(p, _)| {
            cmp_paths(
                self.os,
                p.as_os_str().as_encoded_bytes(),
                path.as_os_str().as_encoded_bytes(),
            )
            .is_eq()
        }) {
            *value = name.to_owned();
        } else {
            self.apps.push((path.to_owned(), name.to_owned()));
        }
    }

    /// 安装目录及应用名称，对应旧版 `app_dirs`。
    pub fn app_dirs(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.apps
            .iter()
            .map(|(p, name)| (p.as_path(), name.as_str()))
    }

    /// 进入目录时查一次；返回可存入 `DirContext` 的小整数。
    pub fn directory_id(&self, path: &Path) -> Option<u32> {
        let bytes = path.as_os_str().as_encoded_bytes();
        let mut key = if self.os.case_insensitive() {
            Cow::Owned(path_key(self.os, bytes))
        } else {
            Cow::Borrowed(bytes)
        };
        // 保留所有已有分隔符：/a//file 与 /a/file 是不同的规则/数据库键。
        // 空父路径单独代表相对文件，而不是根目录。
        if !key.is_empty() && key.last() != Some(&(self.os.sep() as u8)) {
            key.to_mut().push(self.os.sep() as u8);
        }
        self.directory_index.get(key.as_ref()).copied()
    }

    /// 只查询名字，不拼完整路径、不分配内存；Windows 使用公共逐码元大写规则。
    pub fn lookup_in_dir(&self, id: u32, name: &OsStr) -> Option<FileInfo<'_>> {
        let files = self.directories.get(id as usize)?;
        let index = files
            .binary_search_by(|record| cmp_path_key(self.os, &record.name, name.as_encoded_bytes()))
            .ok()?;
        let record = &files[index];
        Some(FileInfo {
            owner: record
                .owner
                .map(|id| self.packages[id.get() as usize - 1].as_str()),
            backup: record.backup,
        })
    }

    /// 文件所属的软件包名；界面详情可以用完整路径查询。
    pub fn owner(&self, path: &Path) -> Option<&str> {
        self.lookup_path(path)?.owner
    }

    /// 路径是否被包管理器登记为配置备份文件。
    pub fn is_backup(&self, path: &Path) -> bool {
        self.lookup_path(path).is_some_and(|info| info.backup)
    }

    /// 有归属的文件数；dpkg/rpm 与旧版一样也会包含目录记录。
    pub fn file_count(&self) -> usize {
        self.nfiles
    }

    /// 已索引的目录数，便于性能测量。
    pub fn directory_count(&self) -> usize {
        self.directories.len()
    }

    /// 生成与旧版状态接口相同的字段。
    pub fn status(&self) -> PkgStatus<'_> {
        PkgStatus {
            manager: self.manager,
            label: self.label.as_ref(),
            packages: self.npkgs,
            files: self.nfiles,
            error: self.error.as_ref(),
        }
    }

    fn lookup_path(&self, path: &Path) -> Option<FileInfo<'_>> {
        let normalized = path_key(self.os, path.as_os_str().as_encoded_bytes());
        let (dir, name) = split_path(self.os, &normalized);
        let id = *self.directory_index.get(dir)?;
        let files = self.directories.get(id as usize)?;
        let index = files
            .binary_search_by(|record| record.name.as_ref().cmp(name))
            .ok()?;
        let record = &files[index];
        Some(FileInfo {
            owner: record
                .owner
                .map(|id| self.packages[id.get() as usize - 1].as_str()),
            backup: record.backup,
        })
    }

    fn package_id(&mut self, name: &str) -> NonZeroU32 {
        if let Some(&id) = self.package_index.get(name) {
            return id;
        }
        // 数据库数量远小于 u32 上限；不可信文件名不会控制这里的计数。
        let id = NonZeroU32::new(
            u32::try_from(self.packages.len() + 1).expect("软件包编号超出 u32 范围"),
        )
        .expect("软件包编号从 1 开始");
        self.packages.push(name.to_owned());
        self.package_index.insert(name.to_owned(), id);
        id
    }

    fn directory_slot(&mut self, dir: &[u8]) -> usize {
        if let Some(&id) = self.directory_index.get(dir) {
            return id as usize;
        }
        let id = u32::try_from(self.directories.len()).expect("软件包目录编号超出 u32 范围");
        self.directories.push(Vec::new());
        self.directory_index.insert(dir.to_vec(), id);
        id as usize
    }

    fn insert_record(&mut self, raw: &[u8], owner: Option<NonZeroU32>, backup: bool) {
        let normalized = if self.os.case_insensitive() {
            Cow::Owned(path_key(self.os, raw))
        } else {
            Cow::Borrowed(raw)
        };
        let (dir, name) = split_path(self.os, &normalized);
        let id = self.directory_slot(dir);
        let files = &mut self.directories[id];
        match files.binary_search_by(|record| record.name.as_ref().cmp(name)) {
            Ok(index) => {
                let record = &mut files[index];
                if owner.is_some() {
                    if record.owner.is_none() {
                        self.nfiles += 1;
                    }
                    record.owner = owner;
                }
                record.backup |= backup;
            }
            Err(index) => {
                files.insert(
                    index,
                    FileRecord {
                        name: name.into(),
                        owner,
                        backup,
                    },
                );
                self.nfiles += usize::from(owner.is_some());
            }
        }
    }

    fn push_record(&mut self, raw: &[u8], owner: Option<NonZeroU32>, backup: bool) {
        let normalized = if self.os.case_insensitive() {
            Cow::Owned(path_key(self.os, raw))
        } else {
            Cow::Borrowed(raw)
        };
        let (dir, name) = split_path(self.os, &normalized);
        let id = self.directory_slot(dir);
        self.directories[id].push(FileRecord {
            name: name.into(),
            owner,
            backup,
        });
    }

    fn finish_files(&mut self, last_owner_wins: bool) {
        self.nfiles = 0;
        for files in &mut self.directories {
            // 稳定排序保留包管理器的遍历顺序：pacman 后写覆盖，dpkg/rpm 首次登记优先。
            files.sort_by(|a, b| a.name.cmp(&b.name));
            files.dedup_by(|later, earlier| {
                if later.name != earlier.name {
                    return false;
                }
                earlier.backup |= later.backup;
                if earlier.owner.is_none() || (last_owner_wins && later.owner.is_some()) {
                    earlier.owner = later.owner;
                }
                true
            });
            self.nfiles += files.iter().filter(|record| record.owner.is_some()).count();
            files.shrink_to_fit();
        }
        self.directories.shrink_to_fit();
        self.directory_index.shrink_to_fit();
    }

    fn set_manager(&mut self, manager: &'static str) {
        self.manager = Some(manager);
        self.label = Some(Text::new(manager, manager));
    }

    fn record_error(&mut self, error: std::io::Error) {
        self.error = Some(if error.kind() == std::io::ErrorKind::TimedOut {
            Text::new("软件包查询超时", "Package query timed out")
        } else {
            Text::owned(
                format!("软件包数据库读取失败：{error}"),
                format!("Cannot read package database: {error}"),
            )
        });
    }
}

fn split_path(os: Os, path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().rposition(|&b| b == os.sep() as u8) {
        Some(index) => (&path[..index + 1], &path[index + 1..]),
        None => (&[], path),
    }
}

#[cfg(test)]
mod tests;
