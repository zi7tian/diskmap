//! Linux 的只读 `/proc` 采集器，对应旧版 `_collect_proc`。

use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use super::{Collector, Kind};

pub(super) fn collect(out: &Collector) -> io::Result<()> {
    collect_at(Path::new("/proc"), out)
}

/// 测试可传入临时目录中的伪 `/proc`；真实采集不读写任何测试数据。
fn collect_at(root: &Path, out: &Collector) -> io::Result<()> {
    let search_path = std::env::var_os("PATH");
    // 先关闭根目录句柄再检查自己的 fd；旧版 os.listdir 同样先取完名字，
    // 否则会把采集器临时打开的 /proc 根目录误计为一个正在使用的文件。
    let mut processes = Vec::new();
    for entry in fs::read_dir(root)? {
        if out.expired() {
            break;
        }
        let Ok(entry) = entry else { continue };
        let Some(pid) = parse_pid(entry.file_name().as_bytes()) else {
            continue;
        };
        processes.push((pid, entry.path()));
    }
    for (pid, base) in processes {
        if out.expired() {
            break;
        }
        let Ok(comm) = fs::read(base.join("comm")) else {
            continue;
        };
        let comm = String::from_utf8_lossy(&comm);
        // Python str.strip 还把 U+001C..U+001F 当作空白；保留旧版程序名显示方式。
        let comm =
            comm.trim_matches(|ch: char| ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch));
        out.process_seen();
        if out.expired() {
            break;
        }
        let exe = fs::read_link(base.join("exe")).ok().or_else(|| {
            if out.expired() {
                return None;
            }
            let mut argv0 = Vec::new();
            BufReader::new(File::open(base.join("cmdline")).ok()?)
                .read_until(0, &mut argv0)
                .ok()?;
            if argv0.last() == Some(&0) {
                argv0.pop();
            }
            resolve_argv0(&argv0, search_path.as_deref(), || out.expired())
        });
        if let Some(exe) = exe {
            out.add(exe, pid, comm, Kind::Exe);
        }
        if out.expired() {
            break;
        }
        if collect_maps(&base.join("maps"), pid, comm, out).unwrap_or(false) {
            out.process_visible();
        }
        if out.expired() {
            break;
        }
        let Ok(fds) = fs::read_dir(base.join("fd")) else {
            continue;
        };
        for fd in fds {
            if out.expired() {
                break;
            }
            let Ok(fd) = fd else { continue };
            if let Ok(path) = fs::read_link(fd.path()) {
                // 只读链接文字，不打开所指的文件；管道、设备由公共过滤器排除。
                out.add(path, pid, comm, Kind::Open);
            }
        }
    }
    Ok(())
}

fn parse_pid(name: &[u8]) -> Option<u32> {
    if name.is_empty() {
        return None;
    }
    name.iter().try_fold(0_u32, |pid, &digit| {
        digit
            .is_ascii_digit()
            .then_some(u32::from(digit.wrapping_sub(b'0')))
            .and_then(|digit| pid.checked_mul(10)?.checked_add(digit))
    })
}

/// 只有完整读完 maps 才算「可以读取详细信息」，和旧版 visible 的含义一致。
fn collect_maps(path: &Path, pid: u32, comm: &str, out: &Collector) -> io::Result<bool> {
    let mut maps = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    loop {
        if out.expired() {
            return Ok(false);
        }
        line.clear();
        if maps.read_until(b'\n', &mut line)? == 0 {
            return Ok(true);
        }
        if let Some(path) = maps_path(&line) {
            out.add(Path::new(OsStr::from_bytes(path)), pid, comm, Kind::Lib);
        }
    }
}

/// 等价于内核 maps 格式上的 `line.split(None, 5)[5].rstrip("\n")`。
/// 名字内部的空格和 `\012` 等内核转义原样保留，不转为 UTF-8 字符串。
fn maps_path(mut line: &[u8]) -> Option<&[u8]> {
    for _ in 0..5 {
        line = line.trim_ascii_start();
        let end = line.iter().position(u8::is_ascii_whitespace)?;
        line = &line[end..];
    }
    line = line.trim_ascii_start();
    while let Some(rest) = line.strip_suffix(b"\n") {
        line = rest;
    }
    (!line.is_empty()).then_some(line)
}

/// `exe` 不可读时，和旧版一样只看 argv[0]；相对程序名从采集进程自己的 PATH 查找。
fn resolve_argv0(
    argv0: &[u8],
    search_path: Option<&OsStr>,
    expired: impl Fn() -> bool,
) -> Option<PathBuf> {
    if argv0.is_empty() || expired() {
        return None;
    }
    let path = Path::new(OsStr::from_bytes(argv0));
    if argv0.starts_with(b"/") {
        return Some(path.to_path_buf());
    }
    // shutil.which 对含目录分隔符的参数不搜索 PATH，返回原本的拼写。
    if argv0.contains(&b'/') {
        return is_executable(path).then(|| path.to_path_buf());
    }
    let search_path = search_path.unwrap_or_else(|| OsStr::new("/bin:/usr/bin"));
    // 与 shutil.which 一样，显式空 PATH 不代表当前目录。
    if search_path.is_empty() {
        return None;
    }
    for directory in std::env::split_paths(search_path) {
        if expired() {
            return None;
        }
        let candidate = directory.join(path);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: c_path 是有效且以 NUL 结尾的路径，access 只查询当前真实用户的执行权限。
    let executable = unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 };
    executable && fs::metadata(path).is_ok_and(|metadata| !metadata.is_dir())
}

pub(super) fn swapfiles() -> Vec<PathBuf> {
    fs::read("/proc/swaps")
        .map(|bytes| parse_swaps(&bytes))
        .unwrap_or_default()
}

fn parse_swaps(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|&b| b == b'\n')
        .skip(1)
        .filter_map(|line| {
            let mut columns = line
                .split(u8::is_ascii_whitespace)
                .filter(|part| !part.is_empty());
            let path = columns.next()?;
            if columns.next()? != b"file" {
                return None;
            }
            let mut decoded = Vec::with_capacity(path.len());
            let mut rest = path;
            while !rest.is_empty() {
                if let Some(after) = rest.strip_prefix(b"\\040") {
                    decoded.push(b' ');
                    rest = after;
                } else {
                    decoded.push(rest[0]);
                    rest = &rest[1..];
                }
            }
            Some(PathBuf::from(OsString::from_vec(decoded)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Os;
    use crate::procinfo::ProcInfo;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "diskmap-procinfo-linux-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn maps_preserve_spaces_invalid_bytes_and_kernel_escapes() {
        assert_eq!(
            maps_path(b"7f00-7f10 r-xp 0000 08:00 1    /tmp/a b\xff  \n"),
            Some(b"/tmp/a b\xff  ".as_slice())
        );
        assert_eq!(
            maps_path(b"7f00-7f10 r--p 0000 08:00 1 /tmp/a\\012b (deleted)\n"),
            Some(b"/tmp/a\\012b (deleted)".as_slice())
        );
        assert_eq!(maps_path(b"7f00-7f10 rw-p 0000 00:00 0\n"), None);
        assert_eq!(maps_path(b"7f00-7f10 rw-p 0000 00:00 0    \n"), None);
        assert_eq!(maps_path(b"truncated\n"), None);
    }

    #[test]
    fn swaps_match_legacy_file_filter_and_space_escape() {
        let swaps = parse_swaps(
            b"Filename Type Size Used Priority\n/dev/zram0 partition 1 0 1\n/tmp/swap\\040one file 1 0 -1\n/tmp/\xffswap file 1 0 -1\n/tmp/a\\011b file 1 0 -1\nbroken\n",
        );
        let paths: Vec<_> = swaps
            .iter()
            .map(|path| path.as_os_str().as_bytes())
            .collect();
        assert_eq!(
            paths,
            [
                b"/tmp/swap one".as_slice(),
                b"/tmp/\xffswap".as_slice(),
                b"/tmp/a\\011b".as_slice(),
            ]
        );
        assert!(parse_swaps(b"").is_empty());
        assert!(parse_swaps(b"Filename Type Size Used Priority\n").is_empty());
    }

    #[test]
    fn pid_parsing_ignores_non_process_names_and_overflow() {
        assert_eq!(parse_pid(b"42"), Some(42));
        assert_eq!(parse_pid(b"0042"), Some(42));
        for name in [b"".as_slice(), b"self", b"12x", b"4294967296"] {
            assert_eq!(parse_pid(name), None);
        }
    }

    #[test]
    fn command_fallback_searches_only_executable_non_directories() {
        let dir = TempDir::new();
        let command = dir.0.join(OsStr::from_bytes(b"cmd\xff"));
        fs::write(&command, b"fixture").unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = dir.0.join("directory");
        fs::create_dir(&directory).unwrap();
        assert_eq!(
            resolve_argv0(b"cmd\xff", Some(dir.0.as_os_str()), || false),
            Some(command.clone())
        );
        assert!(resolve_argv0(b"directory", Some(dir.0.as_os_str()), || false).is_none());
        assert!(resolve_argv0(b"cmd\xff", Some(OsStr::new("")), || false).is_none());
        assert!(resolve_argv0(b"cmd\xff", Some(dir.0.as_os_str()), || true).is_none());
        fs::set_permissions(&command, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(resolve_argv0(b"cmd\xff", Some(dir.0.as_os_str()), || false).is_none());
        assert_eq!(
            resolve_argv0(b"/missing/absolute", None, || false),
            Some(PathBuf::from("/missing/absolute"))
        );
    }

    fn fake_process(root: &Path, pid: u32, comm: &[u8]) -> PathBuf {
        let base = root.join(pid.to_string());
        fs::create_dir_all(base.join("fd")).unwrap();
        fs::write(base.join("comm"), comm).unwrap();
        base
    }

    fn write_maps(path: &Path, names: &[&Path]) {
        let mut maps = Vec::new();
        for name in names {
            maps.extend_from_slice(b"7f00-7f10 r--p 0000 08:00 1 ");
            maps.extend_from_slice(name.as_os_str().as_bytes());
            maps.push(b'\n');
        }
        fs::write(path, maps).unwrap();
    }

    #[test]
    fn fake_proc_covers_exe_maps_fds_fallback_disappearing_and_unreadable_entries() {
        let temp = TempDir::new();
        let root = temp.0.join("proc");
        fs::create_dir(&root).unwrap();
        let data = temp.0.join("data");
        let exe = data.join("demo");
        let library = data.join(OsStr::from_bytes(b"lib with spaces\xff"));
        let mut deleted_library = library.clone().into_os_string();
        deleted_library.push(" (deleted)");
        let deleted_library = PathBuf::from(deleted_library);
        let opened = data.join("opened");
        let fallback = data.join("from command line");
        let maps_failed_open = data.join("maps failed but fd readable");
        let invalid_comm_exe = data.join("invalid comm");

        let first = fake_process(&root, 100, b"  demo\n");
        symlink(&exe, first.join("exe")).unwrap();
        write_maps(
            &first.join("maps"),
            &[
                &exe,
                &library,
                &library,
                Path::new("[heap]"),
                Path::new("/dev/zero"),
                Path::new("/run/ignored"),
                Path::new("/SYSV123"),
                Path::new("/memfd:name (deleted)"),
            ],
        );
        for (fd, target) in [
            ("1", exe.as_path()),
            ("2", deleted_library.as_path()),
            ("3", opened.as_path()),
            ("4", Path::new("socket:[42]")),
            ("5", Path::new("pipe:[43]")),
        ] {
            symlink(target, first.join("fd").join(fd)).unwrap();
        }
        // 不是符号链接的条目模拟已经失效的 fd，不能让其余进程采集失败。
        fs::write(first.join("fd/6"), b"").unwrap();

        let second = fake_process(&root, 101, b"fallback\n");
        let mut command = fallback.as_os_str().as_bytes().to_vec();
        command.extend_from_slice(b"\0--ignored\0");
        fs::write(second.join("cmdline"), command).unwrap();
        fs::write(second.join("maps"), b"").unwrap();

        let third = fake_process(&root, 102, b"maps unreadable\n");
        fs::create_dir(third.join("maps")).unwrap();
        symlink(&maps_failed_open, third.join("fd/1")).unwrap();

        // 用目录代替文件模拟读取失败，即使测试以 root 运行也不会误判为成功。
        let unreadable = root.join("103");
        fs::create_dir_all(unreadable.join("comm")).unwrap();
        symlink(data.join("must not appear"), unreadable.join("exe")).unwrap();
        fs::create_dir(root.join("104")).unwrap();
        fs::create_dir(root.join("self")).unwrap();

        let invalid_comm = fake_process(&root, 105, b"bad\xffname\n");
        symlink(&invalid_comm_exe, invalid_comm.join("exe")).unwrap();
        fs::write(invalid_comm.join("maps"), b"").unwrap();

        let procinfo = ProcInfo::with_os(Os::Linux);
        assert!(procinfo.refresh_using(Duration::from_secs(5), move |out| collect_at(&root, out)));
        let snapshot = procinfo.snapshot();
        assert!(snapshot.error.is_none());
        assert!(!snapshot.timed_out);
        assert_eq!(snapshot.nprocs, 4);
        assert_eq!(snapshot.visible, 3);
        assert_eq!(snapshot.file_count(), 6);
        assert_eq!(snapshot.dir_count(&data), 6);
        assert_eq!(snapshot.dir_count(Path::new("/")), 6);
        let usage = snapshot.lookup(&exe).unwrap().get(100).unwrap();
        assert_eq!(usage.kind, Kind::Exe);
        assert_eq!(usage.comm.en, "demo");
        assert_eq!(
            snapshot.lookup(&library).unwrap().get(100).unwrap().kind,
            Kind::Lib
        );
        assert_eq!(
            snapshot.lookup(&opened).unwrap().get(100).unwrap().kind,
            Kind::Open
        );
        assert_eq!(
            snapshot.lookup(&fallback).unwrap().get(101).unwrap().kind,
            Kind::Exe
        );
        assert_eq!(
            snapshot
                .lookup(&maps_failed_open)
                .unwrap()
                .get(102)
                .unwrap()
                .kind,
            Kind::Open
        );
        assert_eq!(
            snapshot
                .lookup(&invalid_comm_exe)
                .unwrap()
                .get(105)
                .unwrap()
                .comm
                .en,
            "bad\u{fffd}name"
        );
    }
}
