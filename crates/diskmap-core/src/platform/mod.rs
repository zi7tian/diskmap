//! 平台差异（T01 负责）。对应 legacy/backend/osinfo.py 和 mounts.py。
//!
//! 内容：系统类型 [`Os`]、主目录、缓存目录、设置目录、系统盘根目录、是否管理员、路径规范化（本文件），
//! 挂载点、磁盘列表和磁盘空间（[`mounts`]）。
//!
//! 交换文件 / 页面文件的位置由 T04 移植（旧版 `osinfo.swapfiles()`）。

pub mod mounts;
pub mod path;
mod upcase;

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// 操作系统类型，对应旧版的 `OS_NAME` 和分类器的 `flavor`。
///
/// 分类规则按它选择；测试时可以指定为其他系统，在 Linux 上也能测 Windows 和 macOS 的规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    /// 当前系统。Windows、macOS 以外的系统（包括 BSD）按 Linux 处理，与旧版相同。
    pub const fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }

    /// `"linux"`、`"macos"` 或 `"windows"`，与旧版 `OS_NAME`、`/api/meta` 的 `os` 字段一致。
    pub const fn key(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Macos => "macos",
            Os::Windows => "windows",
        }
    }

    /// 文件名是否不区分大小写（Windows）。查找、比较路径时要先折叠大小写。
    pub const fn case_insensitive(self) -> bool {
        matches!(self, Os::Windows)
    }

    /// 路径分隔符。
    pub const fn sep(self) -> char {
        match self {
            Os::Windows => '\\',
            _ => '/',
        }
    }
}

/// 当前用户的主目录。Unix 先看 `HOME`，Windows 先看 `USERPROFILE`，再问系统。
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|p| !p.as_os_str().is_empty())
}

/// 用户缓存目录，`instance.json` 放在这里。位置与旧版 `user_cache_dir()` 相同：
/// 环境变量 `DISKMAP_CACHE_DIR` 优先；否则 Linux 为 `$XDG_CACHE_HOME/diskmap`（默认 `~/.cache/diskmap`），
/// macOS 为 `~/Library/Caches/DiskMap`，Windows 为 `%LOCALAPPDATA%\DiskMap`。
pub fn cache_dir() -> PathBuf {
    app_dir(Os::current(), Kind::Cache, env_var, home_dir())
}

/// 用户设置目录，`settings.json` 放在这里，新旧版共用。位置与旧版 `user_config_dir()` 相同：
/// 环境变量 `DISKMAP_CONFIG_DIR` 优先；否则 Linux 为 `$XDG_CONFIG_HOME/diskmap`（默认 `~/.config/diskmap`），
/// macOS 为 `~/Library/Application Support/DiskMap`，Windows 为 `%APPDATA%\DiskMap`。
pub fn config_dir() -> PathBuf {
    app_dir(Os::current(), Kind::Config, env_var, home_dir())
}

/// 系统盘的根目录：Windows 为 `%SystemDrive%\`（默认 `C:\`），其他系统为 `/`。
pub fn system_root() -> PathBuf {
    if cfg!(windows) {
        let drive = env_var("SystemDrive").unwrap_or_else(|| OsString::from("C:"));
        let mut root = drive;
        root.push("\\");
        PathBuf::from(root)
    } else {
        PathBuf::from("/")
    }
}

/// 当前进程是否有管理员（Windows）或 root（其他系统）权限。
pub fn is_admin() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid 没有参数，总是成功，没有任何前置条件。
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(windows)]
    {
        // SAFETY: IsUserAnAdmin 没有参数，只查询当前进程的令牌。
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// 把用户输入的路径变成规范的绝对路径（旧版 `norm_path`）：去掉首尾空白，展开开头的 `~`，
/// Windows 上把 `C:` 补成 `C:\`（否则表示"C 盘的当前目录"），再转成绝对路径，
/// 并按字面去掉 `.`、`..`、重复和末尾的分隔符。不解析符号链接。
pub fn norm_path(input: &str) -> PathBuf {
    let s = input.trim();
    let mut path = match s.strip_prefix('~') {
        Some(rest)
            if rest.is_empty()
                || rest.starts_with('/')
                || (cfg!(windows) && rest.starts_with('\\')) =>
        {
            match home_dir() {
                Some(home) => home.join(rest.trim_start_matches(['/', '\\'])),
                None => PathBuf::from(s),
            }
        }
        _ => PathBuf::from(s),
    };
    if cfg!(windows) {
        let b = s.as_bytes();
        if b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
            path = PathBuf::from(format!("{s}\\"));
        }
    }
    let abs = if path.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or(path)
    } else {
        std::path::absolute(&path).unwrap_or(path)
    };
    lexical_normalize(&abs)
}

/// 按字面规范化绝对路径：去掉 `.`，`..` 回到上一级（不会越过根目录）。
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
        }
    }
    out
}

/// 读取环境变量；没有设置或为空时返回 `None`（旧版用 `os.environ.get(...) or 默认值`，空值也算没有设置）。
fn env_var(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

#[derive(Clone, Copy)]
enum Kind {
    Cache,
    Config,
}

/// 缓存目录和设置目录的计算，系统、环境变量和主目录都由参数传入，方便测试。
fn app_dir(
    os: Os,
    kind: Kind,
    env: impl Fn(&str) -> Option<OsString>,
    home: Option<PathBuf>,
) -> PathBuf {
    let over = match kind {
        Kind::Cache => "DISKMAP_CACHE_DIR",
        Kind::Config => "DISKMAP_CONFIG_DIR",
    };
    if let Some(dir) = env(over) {
        return PathBuf::from(dir);
    }
    // 没有主目录时和 Python 的 expanduser 一样，保留 "~"
    let home = home.unwrap_or_else(|| PathBuf::from("~"));
    match (os, kind) {
        (Os::Windows, Kind::Cache) => env("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("DiskMap"),
        (Os::Windows, Kind::Config) => env("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join("DiskMap"),
        (Os::Macos, Kind::Cache) => home.join("Library/Caches/DiskMap"),
        (Os::Macos, Kind::Config) => home.join("Library/Application Support/DiskMap"),
        (Os::Linux, Kind::Cache) => env("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
            .join("diskmap"),
        (Os::Linux, Kind::Config) => env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("diskmap"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            vars.iter()
                .find(|(k, v)| *k == name && !v.is_empty())
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[cfg(unix)]
    #[test]
    fn norm_path_matches_python_abspath() {
        assert_eq!(
            norm_path("  /usr/lib/../share/./doc/ "),
            PathBuf::from("/usr/share/doc")
        );
        assert_eq!(norm_path("/usr//lib/"), PathBuf::from("/usr/lib"));
        assert_eq!(norm_path("/.."), PathBuf::from("/"));
        assert_eq!(norm_path("/"), PathBuf::from("/"));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(norm_path("a/../b"), cwd.join("b"));
        assert_eq!(norm_path(""), cwd);
        if let Some(home) = home_dir() {
            assert_eq!(norm_path("~"), lexical_normalize(&home));
            assert_eq!(norm_path("~/x"), lexical_normalize(&home.join("x")));
        }
        assert_eq!(norm_path("~nobody/x"), cwd.join("~nobody/x"));
    }

    #[test]
    fn os_keys_match_legacy() {
        assert_eq!(Os::Linux.key(), "linux");
        assert_eq!(Os::Macos.key(), "macos");
        assert_eq!(Os::Windows.key(), "windows");
        assert!(Os::Windows.case_insensitive() && !Os::Linux.case_insensitive());
        #[cfg(target_os = "linux")]
        assert_eq!(Os::current(), Os::Linux);
    }

    #[test]
    fn override_variables_win() {
        let env = env_of(&[
            ("DISKMAP_CACHE_DIR", "/tmp/c"),
            ("DISKMAP_CONFIG_DIR", "/tmp/s"),
        ]);
        let home = Some(PathBuf::from("/home/u"));
        for os in [Os::Linux, Os::Macos, Os::Windows] {
            assert_eq!(
                app_dir(os, Kind::Cache, &env, home.clone()),
                PathBuf::from("/tmp/c")
            );
            assert_eq!(
                app_dir(os, Kind::Config, &env, home.clone()),
                PathBuf::from("/tmp/s")
            );
        }
    }

    #[test]
    fn linux_follows_xdg_and_ignores_empty_values() {
        let home = Some(PathBuf::from("/home/u"));
        let none = env_of(&[]);
        assert_eq!(
            app_dir(Os::Linux, Kind::Cache, &none, home.clone()),
            PathBuf::from("/home/u/.cache/diskmap")
        );
        assert_eq!(
            app_dir(Os::Linux, Kind::Config, &none, home.clone()),
            PathBuf::from("/home/u/.config/diskmap")
        );
        let xdg = env_of(&[
            ("XDG_CACHE_HOME", "/x/cache"),
            ("XDG_CONFIG_HOME", ""),
            ("DISKMAP_CACHE_DIR", ""),
        ]);
        assert_eq!(
            app_dir(Os::Linux, Kind::Cache, &xdg, home.clone()),
            PathBuf::from("/x/cache/diskmap")
        );
        assert_eq!(
            app_dir(Os::Linux, Kind::Config, &xdg, home),
            PathBuf::from("/home/u/.config/diskmap")
        );
    }

    #[test]
    fn macos_locations() {
        let home = Some(PathBuf::from("/Users/u"));
        let none = env_of(&[]);
        assert_eq!(
            app_dir(Os::Macos, Kind::Cache, &none, home.clone()),
            PathBuf::from("/Users/u/Library/Caches/DiskMap")
        );
        assert_eq!(
            app_dir(Os::Macos, Kind::Config, &none, home),
            PathBuf::from("/Users/u/Library/Application Support/DiskMap")
        );
    }

    #[test]
    fn real_directories_are_absolute_or_overridden() {
        // 只检查能算出结果；具体位置由上面的测试覆盖
        assert!(!cache_dir().as_os_str().is_empty());
        assert!(!config_dir().as_os_str().is_empty());
        #[cfg(unix)]
        assert_eq!(system_root(), PathBuf::from("/"));
    }
}
