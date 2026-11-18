//! 打开文件管理器；只接受当前扫描范围内仍存在的路径，不解释 shell 命令。

use std::path::Path;
use std::process::{Command, Stdio};

use crate::i18n::Text;
use crate::platform;

use super::App;

pub(super) fn reveal(app: &App, input: &str) -> Result<(), Text> {
    if app.admin() && !cfg!(windows) {
        return Err(Text::new(
            "以 root 身份运行时不打开文件管理器，以免以 root 权限启动图形程序",
            "Running as root: the file manager is not opened, to avoid starting a graphical program with root rights",
        ));
    }
    let path = platform::norm_path(input);
    if !app
        .current_scan()
        .is_some_and(|s| s.index().find(&path).is_some())
    {
        return Err(Text::new(
            "只能打开扫描结果中的路径",
            "Only paths in the scan results can be opened",
        ));
    }
    let st = std::fs::symlink_metadata(&path)
        .map_err(|_| Text::new("该文件已不存在", "This file no longer exists"))?;
    launch(&path, st.is_dir() && !st.file_type().is_symlink()).map_err(|error| {
        Text::owned(
            format!("无法打开文件管理器：{error}"),
            format!("Could not open the file manager: {error}"),
        )
    })
}

fn quiet(command: &mut Command) -> &mut Command {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: 子进程在 exec 前只调用 async-signal-safe 的 setsid，不分配内存或获取锁。
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    command
}

fn detached(command: &mut Command) -> std::io::Result<()> {
    let mut child = quiet(command).spawn()?;
    // 回收子进程，避免长期运行的服务留下僵尸进程。
    let _ = std::thread::Builder::new()
        .name("diskmap-reveal".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

#[cfg(windows)]
fn launch(path: &Path, is_dir: bool) -> std::io::Result<()> {
    let mut command = Command::new("explorer.exe");
    if !is_dir {
        command.arg("/select,");
    }
    detached(command.arg(path))
}

#[cfg(target_os = "macos")]
fn launch(path: &Path, is_dir: bool) -> std::io::Result<()> {
    let mut command = Command::new("open");
    if !is_dir {
        command.arg("-R");
    }
    detached(command.arg(path))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn launch(path: &Path, is_dir: bool) -> std::io::Result<()> {
    use std::time::{Duration, Instant};
    if is_dir {
        return detached(Command::new("xdg-open").arg(path));
    }
    let uri = file_uri(path);
    let mut command = Command::new("gdbus");
    command.args([
        "call",
        "--session",
        "--dest",
        "org.freedesktop.FileManager1",
        "--object-path",
        "/org/freedesktop/FileManager1",
        "--method",
        "org.freedesktop.FileManager1.ShowItems",
        &format!("['{uri}']"),
        "",
    ]);
    if let Ok(mut child) = quiet(&mut command).spawn() {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        return Ok(());
                    }
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    }
    detached(Command::new("xdg-open").arg(path.parent().unwrap_or(Path::new("/"))))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn file_uri(path: &Path) -> String {
    use std::fmt::Write;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}
