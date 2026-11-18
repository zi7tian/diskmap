//! 重要程度规则，对应 legacy/backend/classify.py。
//!
//! 扫描器先调用 [`Classifier::root_class`]；每读一个目录调用 [`Classifier::enter_dir`]，
//! 再为子目录调用 [`Classifier::dir_class`]，为文件调用 [`Classifier::file_class`]。
//! 后两者不分配内存；文件分类只查文件名，不构造完整路径。分类器和目录上下文可以跨线程共享。

mod homes;
mod rules;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::i18n::Text;
use crate::model::Category;
use crate::pkgdb::PkgDb;
use crate::platform::Os;
use crate::platform::path::{cmp_names, cmp_path_key as compare_normalized, path_key as normalize};

pub use rules::{CACHE_DIR_NAMES, CACHE_EXTS, CLASSES};

/// 图例及 `/api/meta` 的类别描述，JSON 字段与旧版 CLASSES 相同。
#[derive(Clone, Debug, Serialize)]
pub struct ClassInfo {
    pub id: u8,
    pub key: &'static str,
    pub label: Text,
    pub desc: Text,
}

/// 原因编号只在产生它的分类器内有效；目录节点不必重复保存文字。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReasonId(pub u32);

/// 一个目录的分类结果：类别和原因。
pub type DirClass = (Category, ReasonId);

type Rule = (Vec<u8>, DirClass);
type FileGroup = (Vec<u8>, Vec<Rule>);

const ROOT: ReasonId = ReasonId(0);
const DRIVE: ReasonId = ReasonId(1);
const HIDDEN: ReasonId = ReasonId(2);
const PERSONAL: ReasonId = ReasonId(3);
const CACHE_DIR: ReasonId = ReasonId(4);
const DOTFILE: ReasonId = ReasonId(5);
const REGISTRY: ReasonId = ReasonId(6);
const CACHE_FILE: ReasonId = ReasonId(7);
const SWAP: ReasonId = ReasonId(8);

/// 可注入的平台、环境变量、主目录。`homes: None` 才会读取用户数据库和目录。
#[derive(Clone, Debug)]
pub struct ClassifierOptions {
    pub os: Os,
    pub env: HashMap<String, String>,
    pub homes: Option<Vec<PathBuf>>,
}

impl Default for ClassifierOptions {
    fn default() -> Self {
        Self {
            os: Os::current(),
            env: std::env::vars_os()
                .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                .collect(),
            homes: None,
        }
    }
}

impl ClassifierOptions {
    /// 确定性的空环境与空主目录列表；用于跨平台规则测试或显式配置。
    pub fn for_os(os: Os) -> Self {
        Self {
            os,
            env: HashMap::new(),
            homes: Some(Vec::new()),
        }
    }
}

/// 正在读取的目录；只保存类别、原因及两个小编号和主目录标记，不持有路径或分配堆内存。
#[derive(Clone, Debug)]
pub struct DirContext {
    class: DirClass,
    file_group: Option<u32>,
    package_dir: Option<u32>,
    is_home: bool,
}

impl DirContext {
    /// 这个目录本身的分类结果。
    pub fn class(&self) -> DirClass {
        self.class
    }
}

/// 不可变规则索引，可由所有扫描线程共享。
#[derive(Clone, Debug)]
pub struct Classifier {
    os: Os,
    pkgdb: Option<Arc<PkgDb>>,
    homes: Vec<Vec<u8>>,
    cache_names: Vec<Vec<u8>>,
    dir_rules: Vec<Rule>,
    file_groups: Vec<FileGroup>,
    reasons: Vec<Text>,
}

impl Classifier {
    /// 当前系统的默认规则；软件包信息和正在使用的交换文件由调用方提供。
    pub fn new(pkgdb: Option<Arc<PkgDb>>, swapfiles: &[PathBuf]) -> Classifier {
        Self::with_options(pkgdb, swapfiles, ClassifierOptions::default())
    }

    /// 注入系统类型、环境及主目录，不采用宿主平台的路径解析语义。
    pub fn with_options(
        pkgdb: Option<Arc<PkgDb>>,
        swapfiles: &[PathBuf],
        options: ClassifierOptions,
    ) -> Classifier {
        let discovered = options
            .homes
            .unwrap_or_else(|| homes::discover(options.os, &options.env));
        let os = options.os;
        let mut homes: Vec<_> = discovered
            .iter()
            .map(|p| key(os, p.as_os_str().as_encoded_bytes()))
            .collect();
        homes.sort();
        homes.dedup();
        let mut cache_names: Vec<_> = CACHE_DIR_NAMES
            .iter()
            .map(|name| normalize(os, name.as_bytes()))
            .collect();
        cache_names.sort_unstable();
        cache_names.dedup();
        let mut c = Self {
            os,
            pkgdb,
            homes,
            cache_names,
            dir_rules: Vec::new(),
            file_groups: Vec::new(),
            reasons: vec![
                Text::new("根目录", "Root folder"),
                Text::new("磁盘根目录", "Drive root"),
                Text::new(
                    "应用数据（主目录下的隐藏文件夹）",
                    "Application data (hidden folder in the home folder)",
                ),
                Text::new(
                    "用户主目录中的个人文件",
                    "Personal files in the home folder",
                ),
                Text::new("缓存文件夹", "Cache folder"),
                Text::new(
                    "主目录下的点文件（通常是配置）",
                    "Dotfile in the home folder (usually settings)",
                ),
                Text::new("用户注册表文件", "User registry file"),
                Text::new("日志/临时/备份类文件", "Log, temporary or backup file"),
                Text::new("交换文件（虚拟内存）", "Swap file (virtual memory)"),
            ],
        };
        let mut dirs = HashMap::new();
        let mut files = HashMap::new();
        let (dir_specs, file_specs, home_specs) = match os {
            Os::Linux => (rules::LINUX_DIRS, rules::LINUX_FILES, rules::LINUX_HOME),
            Os::Macos => (rules::MAC_DIRS, &[][..], rules::MAC_HOME),
            Os::Windows => (rules::WIN_DIRS, rules::WIN_FILES, rules::WIN_HOME),
        };
        let sd = options
            .env
            .get("SystemDrive")
            .map(String::as_str)
            .unwrap_or("C:");
        let substitutions = [
            ("{SD}", sd.to_string()),
            (
                "{SR}",
                env_or(&options.env, "SystemRoot", &format!("{sd}\\Windows")),
            ),
            (
                "{PF86}",
                env_or(
                    &options.env,
                    "ProgramFiles(x86)",
                    &format!("{sd}\\Program Files (x86)"),
                ),
            ),
            (
                "{PF}",
                env_or(
                    &options.env,
                    "ProgramFiles",
                    &format!("{sd}\\Program Files"),
                ),
            ),
            (
                "{PD}",
                env_or(&options.env, "ProgramData", &format!("{sd}\\ProgramData")),
            ),
        ];
        for (target, specs) in [(&mut dirs, dir_specs), (&mut files, file_specs)] {
            for (path, category, reason) in specs {
                let mut path = (*path).to_string();
                if os == Os::Windows {
                    for (from, to) in &substitutions {
                        path = path.replace(from, to);
                    }
                }
                let id = c.intern(reason.clone());
                target.insert(key(os, path.as_bytes()), (*category, id));
            }
        }
        for h in &discovered {
            for (relative, category, reason) in rules::COMMON_HOME.iter().chain(home_specs) {
                let mut path = h.as_os_str().as_encoded_bytes().to_vec();
                if !path.is_empty() && !ends_separator(os, &path) {
                    path.push(os.sep() as u8);
                }
                path.extend(
                    relative
                        .bytes()
                        .map(|b| if b == b'/' { os.sep() as u8 } else { b }),
                );
                let id = c.intern(reason.clone());
                dirs.insert(key(os, &path), (*category, id));
            }
        }
        for path in swapfiles {
            files.insert(
                key(os, path.as_os_str().as_encoded_bytes()),
                (Category::Core, SWAP),
            );
        }
        if let Some(db) = c.pkgdb.clone() {
            for (path, name) in db.app_dirs() {
                let path = key(os, path.as_os_str().as_encoded_bytes());
                dirs.entry(path).or_insert_with(|| {
                    let id = c.intern(Text::owned(
                        format!("已安装的程序：{name}"),
                        format!("Installed program: {name}"),
                    ));
                    (Category::System, id)
                });
            }
        }
        c.dir_rules = dirs.into_iter().collect();
        c.dir_rules.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut groups: HashMap<Vec<u8>, Vec<Rule>> = HashMap::new();
        for (path, rule) in files {
            let (parent, name) = split_prefix(os, &path);
            groups
                .entry(parent.to_vec())
                .or_default()
                .push((name.to_vec(), rule));
        }
        c.file_groups = groups.into_iter().collect();
        c.file_groups.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        for (_, rules) in &mut c.file_groups {
            rules.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        }
        c
    }

    fn intern(&mut self, reason: Text) -> ReasonId {
        if let Some(i) = self.reasons.iter().position(|r| *r == reason) {
            return ReasonId(i as u32);
        }
        let id = ReasonId(self.reasons.len() as u32);
        self.reasons.push(reason);
        id
    }

    /// 从根目录逐级推导任意扫描起点的类别。重复分隔符被忽略，`.` 和 `..` 与旧版一样不折叠。
    pub fn root_class(&self, path: &Path) -> DirClass {
        self.root_bytes(path.as_os_str().as_encoded_bytes())
    }

    fn root_bytes(&self, path: &[u8]) -> DirClass {
        let path = if self.os == Os::Windows {
            std::borrow::Cow::Owned(
                path.iter()
                    .map(|&b| if b == b'/' { b'\\' } else { b })
                    .collect::<Vec<_>>(),
            )
        } else {
            std::borrow::Cow::Borrowed(path)
        };
        let drive = drive_end(self.os, &path);
        let mut cur = path[..drive].to_vec();
        cur.push(self.os.sep() as u8);
        let mut class = self.rule(&key(self.os, &cur)).unwrap_or((
            Category::Other,
            if self.os == Os::Windows { DRIVE } else { ROOT },
        ));
        for name in path[drive..]
            .split(|&b| b == self.os.sep() as u8)
            .filter(|s| !s.is_empty())
        {
            let is_home = self.is_home(&cur);
            if !ends_separator(self.os, &cur) {
                cur.push(self.os.sep() as u8);
            }
            cur.extend_from_slice(name);
            class = self.class_dir(class, is_home, name, &cur);
        }
        class
    }

    /// 开始读取一个目录；定位该目录对应的文件规则和软件包文件表。
    pub fn enter_dir(&self, class: DirClass, path: &Path) -> DirContext {
        let bytes = path.as_os_str().as_encoded_bytes();
        DirContext {
            class,
            file_group: self.file_group(bytes),
            package_dir: self.pkgdb.as_ref().and_then(|db| db.directory_id(path)),
            is_home: self.is_home(bytes),
        }
    }

    /// 子目录分类。只借用名称和完整路径；包括 Windows 大小写折叠在内均不分配。
    pub fn dir_class(&self, parent: &DirContext, name: &OsStr, path: &Path) -> DirClass {
        self.class_dir(
            parent.class,
            parent.is_home,
            name.as_encoded_bytes(),
            path.as_os_str().as_encoded_bytes(),
        )
    }

    fn class_dir(&self, parent: DirClass, is_home: bool, name: &[u8], path: &[u8]) -> DirClass {
        if let Some(rule) = self.rule(path) {
            return rule;
        }
        if is_home {
            return if name.starts_with(b".") {
                (Category::Other, HIDDEN)
            } else {
                (Category::User, PERSONAL)
            };
        }
        if matches!(parent.0, Category::Core | Category::System) {
            return parent;
        }
        if self
            .cache_names
            .binary_search_by(|n| compare_normalized(self.os, n, name))
            .is_ok()
        {
            return (Category::Cache, CACHE_DIR);
        }
        parent
    }

    /// 每个文件只查文件名；不分配、不拼接完整路径。
    pub fn file_class(&self, dir: &DirContext, name: &OsStr) -> Category {
        let bytes = name.as_encoded_bytes();
        if let Some(rule) = self.file_rule(dir.file_group, bytes) {
            return rule.0;
        }
        if let Some(info) = self
            .pkgdb
            .as_ref()
            .and_then(|db| db.lookup_in_dir(dir.package_dir?, name))
            && info.owner.is_some()
        {
            return package_class(dir.class.0, info.backup);
        }
        if inherits_file(dir.class.0) {
            return dir.class.0;
        }
        if dir.is_home
            && (bytes.starts_with(b".") || (self.os == Os::Windows && registry_file(bytes)))
        {
            return Category::Config;
        }
        if self.cache_file(bytes) {
            return Category::Cache;
        }
        dir.class.0
    }

    /// 原因编号对应的双语文字。
    pub fn reason(&self, id: ReasonId) -> &Text {
        self.reasons.get(id.0 as usize).unwrap_or(&self.reasons[0])
    }

    /// 按编号排列的中英分类原因。
    pub fn reasons(&self) -> &[Text] {
        &self.reasons
    }

    /// 克隆当前分类器，登记存盘的原因文字，并按原表顺序返回它们在副本中的编号。
    pub fn clone_with_reasons(&self, reasons: &[Text]) -> (Self, Vec<ReasonId>) {
        let mut result = self.clone();
        let mut ids: HashMap<_, _> = self
            .reasons
            .iter()
            .enumerate()
            .map(|(i, reason)| (reason, ReasonId(i as u32)))
            .collect();
        let mapping = reasons
            .iter()
            .map(|reason| {
                *ids.entry(reason).or_insert_with(|| {
                    let id = ReasonId(result.reasons.len() as u32);
                    result.reasons.push(reason.clone());
                    id
                })
            })
            .collect();
        (result, mapping)
    }

    /// 解释任意文件或目录（可以分配），包含包名及包管理器登记的配置文件信息。
    pub fn explain(&self, path: &Path, is_dir: bool) -> (Category, Text) {
        if is_dir {
            let (category, reason) = self.root_class(path);
            return (category, self.reason(reason).clone());
        }
        let original = path.as_os_str().as_encoded_bytes();
        let normalized_separators: Vec<_>;
        let bytes = if self.os == Os::Windows {
            normalized_separators = original
                .iter()
                .map(|&b| if b == b'/' { b'\\' } else { b })
                .collect();
            normalized_separators.as_slice()
        } else {
            original
        };
        let (parent, name) = split_parent(self.os, bytes);
        self.explain_file_bytes(self.root_bytes(parent), parent, name, path)
    }

    /// 已知父目录类别时的文件解释，对应旧版 `explain_file`。
    pub fn explain_file(
        &self,
        dir: DirClass,
        parent: &Path,
        name: &OsStr,
        path: &Path,
    ) -> (Category, Text) {
        self.explain_file_bytes(
            dir,
            parent.as_os_str().as_encoded_bytes(),
            name.as_encoded_bytes(),
            path,
        )
    }

    fn explain_file_bytes(
        &self,
        dir: DirClass,
        parent: &[u8],
        name: &[u8],
        path: &Path,
    ) -> (Category, Text) {
        if let Some((category, reason)) = self.file_rule_path(path.as_os_str().as_encoded_bytes()) {
            return (category, self.reason(reason).clone());
        }
        if let Some(db) = &self.pkgdb
            && let Some(owner) = db.owner(path)
        {
            let category = package_class(dir.0, db.is_backup(path));
            let reason = match category {
                Category::Config => Text::owned(
                    format!("软件包 {owner} 的配置文件"),
                    format!("Configuration file of package {owner}"),
                ),
                Category::Core => Text::owned(
                    format!("由软件包 {owner} 安装 · {}", self.reason(dir.1).zh),
                    format!("Installed by package {owner} · {}", self.reason(dir.1).en),
                ),
                _ => Text::owned(
                    format!("由软件包 {owner} 安装"),
                    format!("Installed by package {owner}"),
                ),
            };
            return (category, reason);
        }
        if inherits_file(dir.0) {
            return (dir.0, self.reason(dir.1).clone());
        }
        if self.is_home(parent) {
            if name.starts_with(b".") {
                return (Category::Config, self.reason(DOTFILE).clone());
            }
            if self.os == Os::Windows && registry_file(name) {
                return (Category::Config, self.reason(REGISTRY).clone());
            }
        }
        if self.cache_file(name) {
            return (Category::Cache, self.reason(CACHE_FILE).clone());
        }
        (dir.0, self.reason(dir.1).clone())
    }

    fn rule(&self, path: &[u8]) -> Option<DirClass> {
        self.dir_rules
            .binary_search_by(|r| compare_normalized(self.os, &r.0, path))
            .ok()
            .map(|i| self.dir_rules[i].1)
    }

    fn is_home(&self, path: &[u8]) -> bool {
        self.homes
            .binary_search_by(|h| compare_normalized(self.os, h, path))
            .is_ok()
    }

    fn file_group(&self, path: &[u8]) -> Option<u32> {
        // 与旧 join_path 相同：目录已有尾分隔符就直接接文件名，否则补一个。
        // 只在 enter_dir 发生分配；保留重复分隔符，不能把 /data//f 合并到 /data/f。
        if path.is_empty() || ends_separator(self.os, path) {
            self.file_group_prefix(path)
        } else {
            let mut prefix = path.to_vec();
            prefix.push(self.os.sep() as u8);
            self.file_group_prefix(&prefix)
        }
    }

    fn file_group_prefix(&self, prefix: &[u8]) -> Option<u32> {
        self.file_groups
            .binary_search_by(|r| compare_normalized(self.os, &r.0, prefix))
            .ok()
            .map(|i| i as u32)
    }

    fn file_rule_path(&self, path: &[u8]) -> Option<DirClass> {
        let (prefix, name) = split_prefix(self.os, path);
        self.file_rule(self.file_group_prefix(prefix), name)
    }

    fn file_rule(&self, group: Option<u32>, name: &[u8]) -> Option<DirClass> {
        let rules = &self.file_groups.get(group? as usize)?.1;
        rules
            .binary_search_by(|r| compare_normalized(self.os, &r.0, name))
            .ok()
            .map(|i| rules[i].1)
    }

    fn cache_file(&self, name: &[u8]) -> bool {
        if name.last() == Some(&b'~') {
            return true;
        }
        // 所有点后缀都从 ASCII 点开始；先切出完整后缀再按公共规则比较，
        // 避免 Unicode 映射改变字节长度时截断字符，也不为每个文件分配内存。
        let Some(dot) = name.iter().rposition(|&byte| byte == b'.') else {
            return false;
        };
        CACHE_EXTS
            .iter()
            .any(|ext| cmp_names(self.os, &name[dot..], ext.as_bytes()).is_eq())
    }
}

fn env_or(env: &HashMap<String, String>, name: &str, fallback: &str) -> String {
    env.get(name)
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .unwrap_or(fallback)
        .to_string()
}

fn package_class(parent: Category, backup: bool) -> Category {
    if parent == Category::Config || backup {
        Category::Config
    } else if parent == Category::Core {
        Category::Core
    } else {
        Category::System
    }
}

fn inherits_file(class: Category) -> bool {
    class <= Category::Config || class == Category::Cache
}

fn registry_file(input: &[u8]) -> bool {
    input
        .iter()
        .position(|&byte| byte == b'.')
        .is_some_and(|dot| cmp_names(Os::Windows, &input[..dot], b"ntuser").is_eq())
}

fn ends_separator(os: Os, path: &[u8]) -> bool {
    path.last()
        .is_some_and(|&b| b == os.sep() as u8 || (os == Os::Windows && b == b'/'))
}

fn key(os: Os, bytes: &[u8]) -> Vec<u8> {
    let mut key = normalize(os, bytes);
    if os == Os::Windows {
        let characters = std::str::from_utf8(&key).map_or(key.len(), |s| s.chars().count());
        if characters > 3 {
            while key.last() == Some(&b'\\') {
                key.pop();
            }
        }
    } else {
        while key.last() == Some(&b'/') {
            key.pop();
        }
        if key.is_empty() {
            key.push(b'/');
        }
    }
    key
}

/// `ntpath.splitdrive` 的驱动器与 UNC 前缀；不借用宿主系统的 Path 语义。
fn drive_end(os: Os, path: &[u8]) -> usize {
    if os != Os::Windows {
        return 0;
    }
    if path.starts_with(b"\\\\") {
        let start = if path
            .get(..8)
            .is_some_and(|s| cmp_names(os, s, b"\\\\?\\UNC\\").is_eq())
        {
            8
        } else {
            2
        };
        let Some(first) = path[start..]
            .iter()
            .position(|&b| b == b'\\')
            .map(|i| start + i)
        else {
            return path.len();
        };
        return path[first + 1..]
            .iter()
            .position(|&b| b == b'\\')
            .map(|i| first + 1 + i)
            .unwrap_or(path.len());
    }
    // Python 按字符判断第二个字符是否为冒号，注入测试中的非 ASCII 盘符也照此处理。
    if let Some(colon) = path.iter().position(|&b| b == b':')
        && (colon == 1 || std::str::from_utf8(&path[..colon]).is_ok_and(|s| s.chars().count() == 1))
    {
        colon + 1
    } else {
        0
    }
}

/// 保留完整目录前缀；用于按原始文件路径构造索引，与 dirname 的去尾分隔符不同。
fn split_prefix(os: Os, path: &[u8]) -> (&[u8], &[u8]) {
    let end = path
        .iter()
        .rposition(|&b| b == os.sep() as u8 || (os == Os::Windows && b == b'/'))
        .map_or(0, |i| i + 1);
    (&path[..end], &path[end..])
}

fn split_parent(os: Os, path: &[u8]) -> (&[u8], &[u8]) {
    let drive = drive_end(os, path);
    let end = path[drive..]
        .iter()
        .rposition(|&b| b == os.sep() as u8)
        .map(|i| drive + i + 1)
        .unwrap_or(drive);
    let mut head = end;
    if path[drive..end].iter().any(|&b| b != os.sep() as u8) {
        while head > drive && path[head - 1] == os.sep() as u8 {
            head -= 1;
        }
    }
    (&path[..head], &path[end..])
}

#[cfg(test)]
mod tests;
