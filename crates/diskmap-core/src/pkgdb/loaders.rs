//! 包管理器数据库格式解析。测试通过目录或命令输出注入，不改进程环境。

use std::collections::HashSet;
use std::fs;
#[cfg(any(all(test, unix), not(any(windows, target_os = "macos"))))]
use std::io::{self, Read};
use std::path::{Path, PathBuf};
#[cfg(any(all(test, unix), not(any(windows, target_os = "macos"))))]
use std::process::{Command, Stdio};
#[cfg(any(all(test, unix), not(any(windows, target_os = "macos"))))]
use std::time::{Duration, Instant};

use super::PkgDb;
use crate::i18n::Text;

/// 与 Python 文本文件相同：CRLF、裸 CR 和 LF 都是换行，CRLF 只产生一个边界。
fn universal_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = Some(bytes);
    std::iter::from_fn(move || {
        let bytes = rest.take()?;
        if let Some(end) = bytes.iter().position(|&b| b == b'\r' || b == b'\n') {
            let skip = 1 + usize::from(bytes[end] == b'\r' && bytes.get(end + 1) == Some(&b'\n'));
            rest = Some(&bytes[end + skip..]);
            Some(&bytes[..end])
        } else {
            Some(bytes)
        }
    })
}

/// Python str.split() 同时识别 Unicode 空白；无效 UTF-8 字节作为普通文件名字节保留。
fn first_field(bytes: &[u8]) -> Option<&[u8]> {
    let width_and_space = |bytes: &[u8]| {
        super::path::first_char(bytes).map_or((1, false), |(c, width)| {
            (
                width,
                c.is_whitespace() || matches!(c, '\u{001c}'..='\u{001f}'),
            )
        })
    };
    let mut start = 0;
    while start < bytes.len() {
        let (width, space) = width_and_space(&bytes[start..]);
        if !space {
            break;
        }
        start += width;
    }
    let mut end = start;
    while end < bytes.len() {
        let (width, space) = width_and_space(&bytes[end..]);
        if space {
            break;
        }
        end += width;
    }
    (start < end).then_some(&bytes[start..end])
}

impl PkgDb {
    pub(super) fn read_pacman(&mut self, root: &Path) {
        self.set_manager("pacman");
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                self.record_error(error);
                return;
            }
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Ok(data) = fs::read(entry.path().join("files")) else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let package = name.rsplitn(3, '-').last().unwrap_or(&name);
            let owner = self.package_id(package);
            self.npkgs += 1;
            let mut section = &b""[..];
            let mut absolute = Vec::new();
            for line in universal_lines(&data) {
                if line.is_empty() {
                    section = &[];
                } else if line.first() == Some(&b'%') && line.last() == Some(&b'%') {
                    section = line;
                } else if section == b"%FILES%" && line.last() != Some(&b'/') {
                    absolute.clear();
                    absolute.push(b'/');
                    absolute.extend_from_slice(line);
                    self.push_record(&absolute, Some(owner), false);
                } else if section == b"%BACKUP%" {
                    absolute.clear();
                    absolute.push(b'/');
                    absolute
                        .extend_from_slice(line.split(|&b| b == b'\t').next().unwrap_or_default());
                    self.push_record(&absolute, None, true);
                }
            }
        }
        self.finish_files(true);
    }

    pub(super) fn read_dpkg(&mut self, root: &Path) {
        self.set_manager("dpkg");
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                self.record_error(error);
                return;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(package) = name.strip_suffix(".list") {
                self.npkgs += 1;
                let package = package.split(':').next().unwrap_or(package);
                let owner = self.package_id(package);
                let Ok(data) = fs::read(entry.path()) else {
                    continue;
                };
                for line in universal_lines(&data) {
                    if !line.is_empty() && line != b"/." {
                        self.push_record(line, Some(owner), false);
                    }
                }
            } else if name.ends_with(".conffiles") {
                let Ok(data) = fs::read(entry.path()) else {
                    continue;
                };
                for line in universal_lines(&data) {
                    if let Some(path) = first_field(line) {
                        self.push_record(path, None, true);
                    }
                }
            }
        }
        self.finish_files(false);
    }

    pub(super) fn read_rpm_output(&mut self, output: &[u8]) {
        self.set_manager("rpm");
        let mut packages = HashSet::new();
        for line in universal_lines(output) {
            let mut parts = line.split(|&b| b == b'\t');
            let Some(path) = parts.next() else {
                continue;
            };
            let Some(package) = parts.next() else {
                continue;
            };
            let package = String::from_utf8_lossy(package);
            let owner = self.package_id(&package);
            packages.insert(owner);
            let backup = parts.next().is_some_and(|flags| flags.contains(&b'c'));
            self.push_record(path, Some(owner), backup);
        }
        self.npkgs = packages.len();
        self.finish_files(false);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub(super) fn read_rpm(&mut self) {
        self.set_manager("rpm");
        let mut command = Command::new("rpm");
        command.args([
            "-qa",
            "--qf",
            "[%{FILENAMES}\t%{NAME}\t%{FILEFLAGS:fflags}\n]",
        ]);
        match command_stdout(&mut command, Duration::from_secs(120)) {
            Ok(output) => self.read_rpm_output(&output),
            Err(error) => self.record_error(error),
        }
    }

    pub(super) fn read_homebrew(&mut self, prefixes: &[PathBuf]) {
        for prefix in prefixes {
            for sub in ["Cellar", "Caskroom"] {
                let Ok(entries) = fs::read_dir(prefix.join(sub)) else {
                    continue;
                };
                self.set_manager("brew");
                self.label = Some(Text::new("Homebrew", "Homebrew"));
                for entry in entries.flatten() {
                    // Homebrew 只登记实际目录，不跟随软链接。
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        self.insert_app_dir(
                            &entry.path(),
                            &format!("{}（Homebrew）", entry.file_name().to_string_lossy()),
                        );
                    }
                }
            }
        }
        self.npkgs = self.apps.len();
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn command_exists(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            fs::metadata(dir.join(name))
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
    })
}

/// 并发排空管道，避免大量 rpm 输出填满管道；超时后回收进程和读取线程。
#[cfg(any(all(test, unix), not(any(windows, target_os = "macos"))))]
fn command_stdout(command: &mut Command, timeout: Duration) -> io::Result<Vec<u8>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other(
            "子进程输出管道不可用 / Child stdout is unavailable",
        ));
    };
    let reader = match std::thread::Builder::new()
        .name("diskmap-rpm-output".into())
        .spawn(move || {
            let mut output = Vec::new();
            stdout.read_to_end(&mut output).map(|_| output)
        }) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(_)) => break Ok(()),
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                break Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            Err(error) => break Err(error),
        }
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let output = reader
        .join()
        .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
    result?;
    output
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn command_output_and_timeout_are_handled() {
        let output = command_stdout(
            Command::new("sh").args(["-c", "printf 'demo' "]),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output, b"demo");
        let error = command_stdout(
            Command::new("sh").args(["-c", "exec sleep 10"]),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
