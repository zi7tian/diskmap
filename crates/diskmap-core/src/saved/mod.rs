//! 已完成扫描的本地缓存（T12）。调用方决定何时保存、打开或删除结果。

mod format;

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::classify::Classifier;
use crate::platform::{self, Os, path::path_key};
use crate::scan::{Progress, Scan, ScanState};

use format::{Codec, Header};

const MAX_SAVED: usize = 5;
const NAME_PREFIX: &str = "scan-";
const NAME_SUFFIX: &str = ".dmap";
const TEMP_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// 一份存盘结果的文件头。进度为 Done，含完成时间。
#[derive(Clone, Debug)]
pub struct SavedScan {
    /// 交给 `Store::load` 或 `Store::delete` 的编号。同一起点替换时沿用原编号。
    pub id: String,
    /// 写入文件的 DiskMap 版本。
    pub version: String,
    /// 这份索引使用的路径比较规则。
    pub os: Os,
    pub progress: Progress,
}

/// 存盘结果的位置。默认使用 `platform::cache_dir()/saved`。
#[derive(Clone, Debug)]
pub struct Store {
    directory: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self::with_cache_dir(platform::cache_dir())
    }
}

impl Store {
    /// 在指定缓存目录下使用 `saved/`，供嵌入调用和隔离测试用。
    pub fn with_cache_dir(cache_dir: impl AsRef<Path>) -> Self {
        Self {
            directory: cache_dir.as_ref().join("saved"),
        }
    }

    /// 保存一次成功完成的扫描。用扫描完成时间判断新旧，较早完成的结果可能直接被淘汰。
    pub fn save(&self, scan: &Scan) -> io::Result<()> {
        self.save_with_codec(scan, Codec::Lz4)
    }

    /// 列出最近的结果，按完成时间从新到旧。只读文件头，索引内容在打开时校验。
    pub fn list(&self) -> io::Result<Vec<SavedScan>> {
        let Some(_lock) = self.lock()? else {
            return Ok(Vec::new());
        };
        self.list_locked()
    }

    /// 打开已保存的扫描。编号不存在或文件损坏时返回 `None`，损坏文件会从缓存移除。
    /// 保留保存时的目录分类原因，文件说明仍使用传入分类器的当前规则及软件包库。
    pub fn load(&self, id: &str, classifier: Arc<Classifier>) -> io::Result<Option<Scan>> {
        let path = self.path(id)?;
        let Some(_lock) = self.lock()? else {
            return Ok(None);
        };
        read_entry(&path, |file| {
            let header = format::read_header(file)?;
            let mut index = format::read_index(file, &header)?;
            let (classifier, mapping) = classifier.clone_with_reasons(&header.reasons);
            index.remap_reasons(&mapping)?;
            Scan::from_saved(index, header.progress, Arc::new(classifier))
        })
    }

    /// 删除指定编号的结果。不存在时返回 `false`。
    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let path = self.path(id)?;
        let Some(_lock) = self.lock()? else {
            return Ok(false);
        };
        remove_entry(&path)
    }

    fn save_with_codec(&self, scan: &Scan, codec: Codec) -> io::Result<()> {
        let progress = scan.progress();
        if progress.state != ScanState::Done {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "只能保存已完成的扫描 / Only completed scans can be saved",
            ));
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.directory)?;
        self.check_directory()?;
        let (temporary, mut file, id) = Temporary::create(&self.directory)?;
        let os = {
            let index = scan.index();
            format::write(
                &mut file,
                &index,
                &progress,
                scan.classifier().reasons(),
                codec,
            )?;
            index.os()
        };
        file.sync_all()?;

        // 写索引时无需锁住缓存目录。发布、比较完成时间和淘汰在同一把跨进程的锁内。
        let _lock = self.lock()?.ok_or(io::ErrorKind::NotFound)?;
        let entries = self.list_locked()?;
        drop(file);
        let key = root_key(os, &progress);
        if let Some(previous) = entries
            .iter()
            .find(|entry| root_key(entry.os, &entry.progress) == key)
        {
            if previous.progress.finished_at <= progress.finished_at {
                temporary.publish(&self.path(&previous.id)?)?;
            }
        } else {
            let oldest = entries.get(MAX_SAVED - 1);
            if oldest.is_some_and(|entry| entry.progress.finished_at > progress.finished_at) {
                return Ok(());
            }
            let destination = self.path(&id)?;
            match fs::symlink_metadata(&destination) {
                Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
            temporary.publish(&destination)?;
            if let Some(oldest) = oldest {
                remove_entry(&self.path(&oldest.id)?)?;
            }
        }
        Ok(())
    }

    fn list_locked(&self) -> io::Result<Vec<SavedScan>> {
        let mut entries = Vec::new();
        let now = SystemTime::now();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name
                .strip_prefix('.')
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(valid_id)
            {
                let _ = remove_stale_temporary(&entry.path(), now);
                continue;
            }
            let Some(id) = file_id(name) else {
                continue;
            };
            if let Some(header) = read_entry(&entry.path(), format::read_header)? {
                let Header {
                    version,
                    os,
                    progress,
                    ..
                } = header;
                entries.push(SavedScan {
                    id: id.to_owned(),
                    version,
                    os,
                    progress,
                });
            }
        }
        entries.sort_unstable_by(|a, b| {
            b.progress
                .finished_at
                .cmp(&a.progress.finished_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        // 发布后进程若突然退出，下一次列出或保存时补做淘汰。
        let mut roots = HashSet::new();
        let mut retained = Vec::new();
        for entry in entries {
            if retained.len() < MAX_SAVED && roots.insert(root_key(entry.os, &entry.progress)) {
                retained.push(entry);
            } else {
                remove_entry(&self.path(&entry.id)?)?;
            }
        }
        Ok(retained)
    }

    fn path(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "保存结果的编号无效 / Invalid saved scan ID",
            ));
        }
        Ok(self
            .directory
            .join(format!("{NAME_PREFIX}{id}{NAME_SUFFIX}")))
    }

    fn check_directory(&self) -> io::Result<()> {
        let metadata = fs::symlink_metadata(&self.directory)?;
        if !metadata.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o7777 != 0o700 {
                fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }

    fn lock(&self) -> io::Result<Option<File>> {
        match self.check_directory() {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        let file = file_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join(".lock"))?;
        if !file.metadata()?.is_file() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        file.lock()?;
        Ok(Some(file))
    }
}

fn root_key(os: Os, progress: &Progress) -> (Os, Vec<u8>) {
    (
        os,
        path_key(os, progress.root.as_os_str().as_encoded_bytes()),
    )
}

fn file_id(name: &str) -> Option<&str> {
    let id = name.strip_prefix(NAME_PREFIX)?.strip_suffix(NAME_SUFFIX)?;
    valid_id(id).then_some(id)
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn regular_file(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_entry<T>(
    path: &Path,
    read: impl FnOnce(&mut File) -> io::Result<T>,
) -> io::Result<Option<T>> {
    if !regular_file(path)? {
        return Ok(None);
    }
    let mut file = match file_options().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let result = read(&mut file);
    drop(file);
    match result {
        Ok(entry) => Ok(Some(entry)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
            ) =>
        {
            remove_entry(path)?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn remove_entry(path: &Path) -> io::Result<bool> {
    if !regular_file(path)? {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// 本函数持有缓存目录锁，写入方会把临时文件锁持有到取得目录锁后才释放。
fn remove_stale_temporary(path: &Path, now: SystemTime) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || !now
            .duration_since(metadata.modified()?)
            .is_ok_and(|age| age > TEMP_MAX_AGE)
    {
        return Ok(());
    }
    let file = file_options().read(true).write(true).open(path)?;
    if file.try_lock().is_ok() {
        drop(file);
        fs::remove_file(path)?;
    }
    Ok(())
}

fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

struct Temporary(Option<PathBuf>);

impl Temporary {
    fn create(directory: &Path) -> io::Result<(Self, File, String)> {
        loop {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
            let id = format!("{:032x}", u128::from_le_bytes(bytes));
            let path = directory.join(format!(".{id}.tmp"));
            match file_options().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    let temporary = Self(Some(path));
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(fs::Permissions::from_mode(0o600))?;
                    }
                    file.lock()?;
                    return Ok((temporary, file, id));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error),
            }
        }
    }

    fn publish(mut self, path: &Path) -> io::Result<()> {
        fs::rename(self.0.as_ref().expect("临时文件尚未发布"), path)?;
        self.0 = None;
        Ok(())
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests;
