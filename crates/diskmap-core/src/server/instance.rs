//! 与 Python 版共用的运行实例登记；只有此文件和用户显式设置允许写盘。

use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

static FILE_LOCK: Mutex<()> = Mutex::new(());

/// 当前用户的实例文件位置。
pub fn path() -> PathBuf {
    crate::platform::cache_dir().join("instance.json")
}

/// 从平台缓存目录读取；格式错误或没有实例时返回 `None`。
pub fn read() -> Option<Value> {
    read_at(&path())
}

/// 指定路径版本，供隔离测试及嵌入程序使用。
pub fn read_at(path: &Path) -> Option<Value> {
    let data: Value = serde_json::from_reader(BufReader::new(open_shared(path).ok()?)).ok()?;
    let port = data.get("port")?;
    if !(port.is_i64() || port.is_u64()) || !data.get("token")?.is_string() {
        return None;
    }
    Some(data)
}

fn open_shared(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        // CLI 的 MCP 发现也调用 read/read_at；读取不能阻止其他窗口替换登记文件。
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    options.open(path)
}

/// 先写权限为 0600 的临时文件，再原子替换实例登记。
pub fn write(port: u16, token: &str, version: &str) -> io::Result<()> {
    write_at(&path(), port, token, version)
}

/// 指定路径的原子登记。其他线程不会看到部分 JSON。
pub fn write_at(path: &Path, port: u16, token: &str, version: &str) -> io::Result<()> {
    let _lock = FILE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        super::random_token()?
    ));
    let temporary = PathBuf::from(temporary);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        let data = json!({"port": port, "token": token, "pid": std::process::id(), "version": version,
            "started": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()});
        serde_json::to_writer(&mut file, &data)?;
        file.flush()?;
        drop(file);
        replace(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 只移除当前进程的登记，保留后来启动的其他窗口。
pub fn remove() {
    remove_at(&path());
}

/// 指定路径版本；不存在、权限不足或属于其他进程时不修改。
pub fn remove_at(path: &Path) {
    let _lock = FILE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    if read_at(path).and_then(|data| data.get("pid").and_then(Value::as_u64))
        == Some(u64::from(std::process::id()))
    {
        let _ = fs::remove_file(path);
    }
}

#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    // 最低版本 Rust 1.90 的 std 已先尝试 MoveFileExW；拒绝访问时改用
    // FileRenameInfoEx + POSIX_SEMANTICS | REPLACE_IF_EXISTS，允许持有 DELETE 共享的读者
    // 继续读旧快照。不支持扩展重命名时保留经典调用的错误；不另写一套 Win32 重命名。
    // 未共享 DELETE 的旧版 Python 读者仍可能短暂阻止替换，因此保留有界重试。
    replace_with_retry(std::time::Duration::from_secs(1), || fs::rename(from, to))
}

#[cfg(any(windows, test))]
fn replace_with_retry(
    budget: std::time::Duration,
    mut replace: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + budget;
    loop {
        match replace() {
            Ok(()) => return Ok(()),
            Err(error) => {
                // Win32 ERROR_ACCESS_DENIED / SHARING_VIOLATION / LOCK_VIOLATION。
                // 仅这些短暂占用值得重试，路径无效等其他错误立即返回。纯函数也能在 Linux 注入这些错误测边界。
                if !matches!(error.raw_os_error(), Some(5 | 32 | 33)) {
                    return Err(error);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
                if Instant::now() >= deadline {
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
