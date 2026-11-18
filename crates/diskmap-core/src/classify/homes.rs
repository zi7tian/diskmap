//! 主目录发现：系统用户数据库、标准用户目录，以及当前用户的主目录。

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::platform::{self, Os};

/// 默认发现过程；分类器的显式 `homes` 选项会完全跳过这里。
pub(super) fn discover(os: Os, env: &HashMap<String, String>) -> Vec<PathBuf> {
    let accounts = if os == Os::Windows {
        Vec::new()
    } else {
        account_homes()
    };
    // 和旧版一样，模拟 Unix 规则时只有 Unix 宿主才追加当前主目录。
    let current = if os != Os::Windows && cfg!(unix) {
        platform::home_dir()
    } else {
        None
    };
    discover_from(os, env, &scan_root(os, env), accounts, current)
}

fn scan_root(os: Os, env: &HashMap<String, String>) -> PathBuf {
    match os {
        Os::Windows => {
            let drive = env.get("SystemDrive").map(String::as_str).unwrap_or("C:");
            PathBuf::from(format!("{drive}\\Users"))
        }
        Os::Macos => PathBuf::from("/Users"),
        Os::Linux => PathBuf::from("/home"),
    }
}

/// 注入扫描位置、账户记录和当前主目录，使测试不依赖本机用户和环境变量。
fn discover_from(
    os: Os,
    env: &HashMap<String, String>,
    scan_root: &Path,
    accounts: impl IntoIterator<Item = PathBuf>,
    current: Option<PathBuf>,
) -> Vec<PathBuf> {
    // PathBuf 的相等比较会折叠重复分隔符和点组件；旧版使用原始字符串集合。
    let mut homes = BTreeSet::<OsString>::new();
    if os != Os::Windows {
        let prefix: &[u8] = if os == Os::Macos {
            b"/Users/"
        } else {
            b"/home/"
        };
        for home in accounts {
            let home = trim_unix_separators(home, false);
            let bytes = home.as_os_str().as_encoded_bytes();
            if bytes == b"/var/root" {
                homes.insert(OsString::from("/private/var/root"));
            } else if bytes.starts_with(prefix) || bytes == b"/root" {
                homes.insert(home.into_os_string());
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(scan_root) {
        for entry in entries.flatten() {
            // file_type 不跟随符号链接，和旧版 is_dir(follow_symlinks=False) 相同。
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if os == Os::Windows
                && [b"All Users".as_slice(), b"Default User"]
                    .iter()
                    .any(|alias| {
                        platform::path::cmp_names(os, entry.file_name().as_encoded_bytes(), alias)
                            .is_eq()
                    })
            {
                continue;
            }
            homes.insert(entry.path().into_os_string());
        }
    }

    if os == Os::Windows {
        if let Some(profile) = env.get("USERPROFILE").filter(|profile| !profile.is_empty()) {
            homes.insert(OsString::from(profile));
        }
    } else if let Some(home) = current {
        homes.insert(trim_unix_separators(home, true).into_os_string());
    }
    homes.into_iter().map(PathBuf::from).collect()
}

fn trim_unix_separators(path: PathBuf, root_if_empty: bool) -> PathBuf {
    let bytes = path.as_os_str().as_encoded_bytes();
    let len = bytes
        .iter()
        .rposition(|&byte| byte != b'/')
        .map_or(0, |i| i + 1);
    if len == 0 && root_if_empty {
        return PathBuf::from("/");
    }
    if len == bytes.len() {
        return path;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..len]))
    }
    #[cfg(not(unix))]
    {
        // 非 Unix 宿主只在注入测试中处理这些路径；无法解码时保留原始路径。
        match std::str::from_utf8(&bytes[..len]) {
            Ok(value) => PathBuf::from(value),
            Err(_) => path,
        }
    }
}

#[cfg(unix)]
fn account_homes() -> Vec<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::Mutex;

    // getpwent 的游标与返回缓冲区属于 libc，所有分类器的枚举必须串行。
    // 这里直接使用用户数据库接口；Linux 会经过 NSS，macOS 会经过目录服务，
    // 与 Python pwd.getpwall 相同，不局限于 /etc/passwd 中的本地账户。
    static PASSWD_LOCK: Mutex<()> = Mutex::new(());
    let _guard = PASSWD_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    struct CloseDatabase;
    impl Drop for CloseDatabase {
        fn drop(&mut self) {
            // SAFETY: 枚举期间一直持有 PASSWD_LOCK；结束本次由 setpwent 打开的用户数据库。
            unsafe { libc::endpwent() };
        }
    }

    // SAFETY: PASSWD_LOCK 使本模块中所有使用同一 libc 游标的操作串行化。
    unsafe { libc::setpwent() };
    let _close = CloseDatabase;
    let mut homes = Vec::new();
    loop {
        // SAFETY: 锁仍然被持有；getpwent 返回空指针表示遍历结束或数据库读取失败。
        let entry = unsafe { libc::getpwent() };
        if entry.is_null() {
            break;
        }
        // SAFETY: 非空返回值由 libc 初始化，pw_dir 在下一次用户数据库调用前有效。
        // 立即复制其以 NUL 结尾的字节，不把 libc 的静态缓冲区借用带出本次循环。
        let home = unsafe {
            let directory = (*entry).pw_dir;
            if directory.is_null() {
                continue;
            }
            PathBuf::from(std::ffi::OsStr::from_bytes(
                CStr::from_ptr(directory).to_bytes(),
            ))
        };
        homes.push(home);
    }
    homes
}

#[cfg(not(unix))]
fn account_homes() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "diskmap-homes-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("创建测试临时目录失败：{error}"),
                }
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn paths(values: &[&str]) -> Vec<PathBuf> {
        values.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn account_filter_and_root_mapping_match_legacy() {
        let temp = TempDir::new();
        let accounts = paths(&[
            "/home/alice/",
            "/home/alice///",
            "/home/bob",
            "/homeish/outsider",
            "/Users/carol/",
            "/root/",
            "/var/root///",
            "/var/lib/daemon",
            "/",
            "",
        ]);
        let env = HashMap::new();
        let linux = discover_from(Os::Linux, &env, &temp.0, accounts.clone(), None);
        assert_eq!(
            linux,
            paths(&["/home/alice", "/home/bob", "/private/var/root", "/root"])
        );
        let macos = discover_from(Os::Macos, &env, &temp.0, accounts, None);
        assert_eq!(
            macos,
            paths(&["/Users/carol", "/private/var/root", "/root"])
        );
    }

    #[test]
    fn account_path_spelling_is_not_normalized() {
        let temp = TempDir::new();
        let homes = discover_from(
            Os::Linux,
            &HashMap::new(),
            &temp.0,
            paths(&["/home/alice", "/home/./alice", "/home//alice"]),
            None,
        );
        let actual: Vec<_> = homes.into_iter().map(PathBuf::into_os_string).collect();
        let expected: Vec<_> = ["/home/./alice", "/home//alice", "/home/alice"]
            .into_iter()
            .map(OsString::from)
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn directory_scan_skips_files_and_symbolic_links() {
        let temp = TempDir::new();
        let alice = temp.0.join("alice");
        std::fs::create_dir(&alice).unwrap();
        std::fs::create_dir(temp.0.join(".hidden")).unwrap();
        std::fs::write(temp.0.join("ordinary-file"), b"test").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&alice, temp.0.join("linked-home")).unwrap();
        let homes = discover_from(Os::Linux, &HashMap::new(), &temp.0, Vec::new(), None);
        assert_eq!(homes, vec![temp.0.join(".hidden"), alice]);
    }

    #[test]
    fn windows_excludes_only_legacy_aliases_and_adds_profile() {
        let temp = TempDir::new();
        for name in ["all users", "DeFaUlT UsEr", "Default", "Public", "Bob"] {
            std::fs::create_dir(temp.0.join(name)).unwrap();
        }
        let env = HashMap::from([("USERPROFILE".to_string(), "R:\\Profiles\\Alice".to_string())]);
        let homes = discover_from(
            Os::Windows,
            &env,
            &temp.0,
            paths(&["/home/ignored"]),
            Some(PathBuf::from("/current/ignored")),
        );
        let expected = BTreeSet::from([
            temp.0.join("Bob"),
            temp.0.join("Default"),
            temp.0.join("Public"),
            PathBuf::from("R:\\Profiles\\Alice"),
        ]);
        assert_eq!(homes.into_iter().collect::<BTreeSet<_>>(), expected);
    }

    #[test]
    fn unreadable_directory_keeps_accounts_and_current_home() {
        let temp = TempDir::new();
        let missing = temp.0.join("absent");
        let homes = discover_from(
            Os::Linux,
            &HashMap::new(),
            &missing,
            paths(&["/home/alice"]),
            Some(PathBuf::from("/custom/home///")),
        );
        assert_eq!(homes, paths(&["/custom/home", "/home/alice"]));
        let root = discover_from(
            Os::Linux,
            &HashMap::new(),
            &missing,
            Vec::new(),
            Some(PathBuf::from("///")),
        );
        assert_eq!(root, paths(&["/"]));
        let windows = discover_from(
            Os::Windows,
            &HashMap::from([("USERPROFILE".to_string(), String::new())]),
            &missing,
            Vec::new(),
            None,
        );
        assert!(windows.is_empty());
    }

    #[test]
    fn scan_roots_use_target_platform_separators() {
        let env = HashMap::from([("SystemDrive".to_string(), "R:".to_string())]);
        assert_eq!(scan_root(Os::Windows, &env), PathBuf::from("R:\\Users"));
        assert_eq!(
            scan_root(Os::Windows, &HashMap::new()),
            PathBuf::from("C:\\Users")
        );
        assert_eq!(scan_root(Os::Macos, &env), PathBuf::from("/Users"));
        assert_eq!(scan_root(Os::Linux, &env), PathBuf::from("/home"));
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "APFS 要求文件名是合法 UTF-8，无法创建本测试的非法字节名称"
    )]
    fn non_utf8_accounts_and_directory_names_are_preserved() {
        use std::os::unix::ffi::OsStrExt;
        let temp = TempDir::new();
        let name = std::ffi::OsStr::from_bytes(b"user-\xff");
        let discovered = temp.0.join(name);
        std::fs::create_dir(&discovered).unwrap();
        let account = PathBuf::from(std::ffi::OsStr::from_bytes(b"/home/user-\xff///"));
        let homes = discover_from(Os::Linux, &HashMap::new(), &temp.0, vec![account], None);
        assert!(homes.contains(&discovered));
        assert!(homes.contains(&PathBuf::from(std::ffi::OsStr::from_bytes(
            b"/home/user-\xff"
        ))));
    }
}
