//! 三套平台规则，逐项对应 legacy/backend/classify.py。
use super::ClassInfo;
use crate::i18n::Text;
use crate::model::Category;

pub(super) const LINUX_DIRS: &[(&str, Category, Text)] = &[
    (
        "/boot",
        Category::Core,
        Text::new(
            "引导分区：内核、initramfs 与引导程序",
            "Boot partition: kernel, initramfs and boot loader",
        ),
    ),
    (
        "/efi",
        Category::Core,
        Text::new("EFI 系统分区", "EFI system partition"),
    ),
    (
        "/usr/lib/modules",
        Category::Core,
        Text::new("内核驱动模块", "Kernel driver modules"),
    ),
    (
        "/usr/lib/firmware",
        Category::Core,
        Text::new("硬件固件", "Hardware firmware"),
    ),
    (
        "/lib/modules",
        Category::Core,
        Text::new("内核驱动模块", "Kernel driver modules"),
    ),
    (
        "/lib/firmware",
        Category::Core,
        Text::new("硬件固件", "Hardware firmware"),
    ),
    (
        "/etc",
        Category::Config,
        Text::new("系统配置目录 /etc", "System configuration /etc"),
    ),
    (
        "/usr",
        Category::System,
        Text::new("系统程序目录 /usr", "System programs /usr"),
    ),
    (
        "/usr/local",
        Category::System,
        Text::new(
            "本地安装的程序 /usr/local",
            "Locally installed programs /usr/local",
        ),
    ),
    (
        "/opt",
        Category::System,
        Text::new("第三方程序目录 /opt", "Third-party programs /opt"),
    ),
    (
        "/bin",
        Category::System,
        Text::new("系统程序目录", "System programs"),
    ),
    (
        "/sbin",
        Category::System,
        Text::new("系统程序目录", "System programs"),
    ),
    (
        "/lib",
        Category::System,
        Text::new("系统库目录", "System libraries"),
    ),
    (
        "/lib32",
        Category::System,
        Text::new("系统库目录", "System libraries"),
    ),
    (
        "/lib64",
        Category::System,
        Text::new("系统库目录", "System libraries"),
    ),
    (
        "/var",
        Category::Other,
        Text::new("程序运行数据 /var", "Program data /var"),
    ),
    (
        "/var/cache",
        Category::Cache,
        Text::new(
            "系统缓存 /var/cache（如软件包下载缓存）",
            "System cache /var/cache (e.g. downloaded packages)",
        ),
    ),
    (
        "/var/tmp",
        Category::Cache,
        Text::new("临时文件目录", "Temporary files"),
    ),
    (
        "/var/log",
        Category::Cache,
        Text::new("系统日志", "System logs"),
    ),
    (
        "/var/lib/systemd/coredump",
        Category::Cache,
        Text::new("程序崩溃转储", "Crash dumps"),
    ),
    (
        "/var/lib/pacman",
        Category::System,
        Text::new("pacman 软件包数据库", "pacman package database"),
    ),
    (
        "/var/lib/dpkg",
        Category::System,
        Text::new("dpkg 软件包数据库", "dpkg package database"),
    ),
    (
        "/var/lib/rpm",
        Category::System,
        Text::new("rpm 软件包数据库", "rpm package database"),
    ),
    (
        "/var/lib/flatpak",
        Category::System,
        Text::new("Flatpak 应用与运行时", "Flatpak apps and runtimes"),
    ),
    (
        "/var/lib/snapd",
        Category::System,
        Text::new("Snap 应用", "Snap apps"),
    ),
    (
        "/tmp",
        Category::Cache,
        Text::new("临时文件目录", "Temporary files"),
    ),
    (
        "/home",
        Category::User,
        Text::new("用户主目录", "Home folders"),
    ),
    (
        "/root",
        Category::User,
        Text::new("root 用户主目录", "Home folder of root"),
    ),
    (
        "/srv",
        Category::User,
        Text::new("服务数据 /srv", "Service data /srv"),
    ),
    (
        "/mnt",
        Category::User,
        Text::new("手动挂载的磁盘", "Manually mounted disks"),
    ),
    (
        "/media",
        Category::User,
        Text::new("可移动介质", "Removable media"),
    ),
    (
        "/lost+found",
        Category::Other,
        Text::new("文件系统修复残留", "Leftovers from file system repair"),
    ),
];

pub(super) const LINUX_FILES: &[(&str, Category, Text)] = &[
    (
        "/swapfile",
        Category::Core,
        Text::new("交换文件（虚拟内存）", "Swap file (virtual memory)"),
    ),
    (
        "/swap.img",
        Category::Core,
        Text::new("交换文件（虚拟内存）", "Swap file (virtual memory)"),
    ),
];

pub(super) const LINUX_HOME: &[(&str, Category, Text)] = &[
    (
        ".local",
        Category::User,
        Text::new("用户程序数据 ~/.local", "User program data ~/.local"),
    ),
    (
        ".local/share/Trash",
        Category::Cache,
        Text::new("回收站", "Trash"),
    ),
    (
        ".local/state",
        Category::Other,
        Text::new(
            "程序状态、历史记录与日志 ~/.local/state",
            "Program state, history and logs ~/.local/state",
        ),
    ),
    (
        ".local/bin",
        Category::Other,
        Text::new("用户自行安装的程序", "Programs installed by the user"),
    ),
    (
        ".local/share/pnpm/store",
        Category::Cache,
        Text::new("pnpm 依赖缓存", "pnpm package store"),
    ),
    (
        ".var",
        Category::User,
        Text::new("Flatpak 应用的用户数据", "User data of Flatpak apps"),
    ),
    (
        ".pki",
        Category::Config,
        Text::new("证书数据库", "Certificate database"),
    ),
    (
        ".password-store",
        Category::Config,
        Text::new("密码库", "Password store"),
    ),
    (
        ".mozilla",
        Category::Config,
        Text::new("Firefox 用户配置", "Firefox profile"),
    ),
    (
        ".thumbnails",
        Category::Cache,
        Text::new("缩略图缓存", "Thumbnail cache"),
    ),
];

pub(super) const MAC_DIRS: &[(&str, Category, Text)] = &[
    (
        "/System",
        Category::Core,
        Text::new(
            "macOS 系统卷（受系统完整性保护，只读）",
            "macOS system volume (read-only, protected by SIP)",
        ),
    ),
    (
        "/Library",
        Category::System,
        Text::new(
            "系统级程序资源 /Library",
            "System-wide program resources /Library",
        ),
    ),
    (
        "/Library/Caches",
        Category::Cache,
        Text::new("系统缓存", "System caches"),
    ),
    (
        "/Library/Logs",
        Category::Cache,
        Text::new("系统日志", "System logs"),
    ),
    (
        "/Library/Application Support",
        Category::Other,
        Text::new("程序共享数据", "Shared program data"),
    ),
    (
        "/Library/Preferences",
        Category::Config,
        Text::new("系统设置", "System settings"),
    ),
    (
        "/Applications",
        Category::System,
        Text::new("应用程序", "Applications"),
    ),
    (
        "/usr",
        Category::System,
        Text::new("系统程序目录 /usr", "System programs /usr"),
    ),
    (
        "/usr/local",
        Category::System,
        Text::new(
            "Homebrew（Intel）或本地安装的程序",
            "Homebrew (Intel) or locally installed programs",
        ),
    ),
    (
        "/bin",
        Category::Core,
        Text::new("系统基本命令", "Basic system commands"),
    ),
    (
        "/sbin",
        Category::Core,
        Text::new("系统管理命令", "System administration commands"),
    ),
    (
        "/opt",
        Category::System,
        Text::new("第三方程序 /opt", "Third-party programs /opt"),
    ),
    (
        "/opt/homebrew",
        Category::System,
        Text::new("Homebrew 安装的软件", "Software installed by Homebrew"),
    ),
    (
        "/private",
        Category::Other,
        Text::new("系统运行数据 /private", "System data /private"),
    ),
    (
        "/private/etc",
        Category::Config,
        Text::new("系统配置", "System configuration"),
    ),
    (
        "/private/var",
        Category::Other,
        Text::new("程序运行数据 /private/var", "Program data /private/var"),
    ),
    (
        "/private/var/vm",
        Category::Core,
        Text::new(
            "虚拟内存交换文件与睡眠镜像",
            "Virtual memory swap files and sleep image",
        ),
    ),
    (
        "/private/var/log",
        Category::Cache,
        Text::new("系统日志", "System logs"),
    ),
    (
        "/private/var/folders",
        Category::Cache,
        Text::new(
            "系统为各用户分配的临时与缓存目录",
            "Per-user temporary and cache folders",
        ),
    ),
    (
        "/private/var/tmp",
        Category::Cache,
        Text::new("临时文件", "Temporary files"),
    ),
    (
        "/private/tmp",
        Category::Cache,
        Text::new("临时文件", "Temporary files"),
    ),
    (
        "/private/var/db",
        Category::System,
        Text::new("系统数据库", "System databases"),
    ),
    (
        "/cores",
        Category::Cache,
        Text::new("程序崩溃转储", "Crash dumps"),
    ),
    (
        "/Users",
        Category::User,
        Text::new("用户主目录", "Home folders"),
    ),
    (
        "/Volumes",
        Category::User,
        Text::new("其他磁盘", "Other disks"),
    ),
];

pub(super) const MAC_HOME: &[(&str, Category, Text)] = &[
    (
        "Library",
        Category::Other,
        Text::new("应用数据 ~/Library", "Application data ~/Library"),
    ),
    (
        "Library/Caches",
        Category::Cache,
        Text::new(
            "应用缓存 ~/Library/Caches",
            "Application caches ~/Library/Caches",
        ),
    ),
    (
        "Library/Logs",
        Category::Cache,
        Text::new("应用日志", "Application logs"),
    ),
    (
        "Library/Preferences",
        Category::Config,
        Text::new(
            "应用设置 ~/Library/Preferences",
            "Application settings ~/Library/Preferences",
        ),
    ),
    (
        "Library/Application Support",
        Category::Other,
        Text::new("应用数据", "Application data"),
    ),
    (
        "Library/Containers",
        Category::Other,
        Text::new("沙盒应用的数据", "Data of sandboxed apps"),
    ),
    (
        "Library/Group Containers",
        Category::Other,
        Text::new("应用共享数据", "Data shared between apps"),
    ),
    (
        "Library/Mobile Documents",
        Category::User,
        Text::new("iCloud 云盘", "iCloud Drive"),
    ),
    ("Library/Mail", Category::User, Text::new("邮件", "Mail")),
    (
        "Library/Messages",
        Category::User,
        Text::new("信息记录", "Messages"),
    ),
    (
        "Library/Developer/Xcode/DerivedData",
        Category::Cache,
        Text::new("Xcode 编译缓存", "Xcode build cache"),
    ),
    (
        "Library/Developer/Xcode/iOS DeviceSupport",
        Category::Cache,
        Text::new(
            "Xcode 设备支持文件（可重新下载）",
            "Xcode device support files (re-downloadable)",
        ),
    ),
    (
        "Library/Developer/CoreSimulator",
        Category::Other,
        Text::new("iOS 模拟器", "iOS simulators"),
    ),
    (".Trash", Category::Cache, Text::new("废纸篓", "Trash")),
];

pub(super) const WIN_DIRS: &[(&str, Category, Text)] = &[
    (
        "{SR}",
        Category::System,
        Text::new("Windows 系统目录", "Windows system folder"),
    ),
    (
        "{SR}\\System32\\drivers",
        Category::Core,
        Text::new("设备驱动程序", "Device drivers"),
    ),
    (
        "{SR}\\System32\\config",
        Category::Core,
        Text::new("注册表数据库", "Registry database"),
    ),
    (
        "{SR}\\Boot",
        Category::Core,
        Text::new("Windows 启动文件", "Windows boot files"),
    ),
    (
        "{SR}\\WinSxS",
        Category::System,
        Text::new(
            "系统组件存储（含大量硬链接，显示值通常偏大；可用「磁盘清理」瘦身）",
            "Component store (many hard links, so the size shown is usually too high; shrink it with Disk Cleanup)",
        ),
    ),
    (
        "{SR}\\Installer",
        Category::System,
        Text::new(
            "已安装程序的安装包缓存（勿手动删除）",
            "Installer cache of installed programs (do not delete by hand)",
        ),
    ),
    (
        "{SR}\\Temp",
        Category::Cache,
        Text::new("系统临时文件", "System temporary files"),
    ),
    (
        "{SR}\\Logs",
        Category::Cache,
        Text::new("系统日志", "System logs"),
    ),
    (
        "{SR}\\Prefetch",
        Category::Cache,
        Text::new("预读取缓存", "Prefetch cache"),
    ),
    (
        "{SR}\\SoftwareDistribution\\Download",
        Category::Cache,
        Text::new("Windows 更新下载缓存", "Windows Update download cache"),
    ),
    (
        "{SR}\\LiveKernelReports",
        Category::Cache,
        Text::new("内核错误报告", "Kernel error reports"),
    ),
    (
        "{SR}\\Minidump",
        Category::Cache,
        Text::new("崩溃转储", "Crash dumps"),
    ),
    (
        "{PF}",
        Category::System,
        Text::new(
            "已安装的程序 Program Files",
            "Installed programs (Program Files)",
        ),
    ),
    (
        "{PF86}",
        Category::System,
        Text::new(
            "已安装的程序 Program Files (x86)",
            "Installed programs (Program Files (x86))",
        ),
    ),
    (
        "{PD}",
        Category::Other,
        Text::new(
            "程序共享数据 ProgramData",
            "Shared program data (ProgramData)",
        ),
    ),
    (
        "{PD}\\Package Cache",
        Category::Other,
        Text::new(
            "安装包缓存（卸载或修复程序时需要）",
            "Installer cache (needed to uninstall or repair programs)",
        ),
    ),
    (
        "{SD}\\Users",
        Category::User,
        Text::new("用户文件夹", "User folders"),
    ),
    (
        "{SD}\\$Recycle.Bin",
        Category::Cache,
        Text::new("回收站", "Recycle Bin"),
    ),
    (
        "{SD}\\System Volume Information",
        Category::Core,
        Text::new(
            "系统还原点与卷信息",
            "System restore points and volume information",
        ),
    ),
    (
        "{SD}\\Recovery",
        Category::Core,
        Text::new("Windows 恢复环境", "Windows Recovery Environment"),
    ),
    (
        "{SD}\\Boot",
        Category::Core,
        Text::new("启动文件", "Boot files"),
    ),
    (
        "{SD}\\EFI",
        Category::Core,
        Text::new("EFI 启动文件", "EFI boot files"),
    ),
    (
        "{SD}\\Windows.old",
        Category::Cache,
        Text::new(
            "旧版 Windows 备份（可用「磁盘清理」删除）",
            "Previous Windows installation (remove it with Disk Cleanup)",
        ),
    ),
    (
        "{SD}\\$WINDOWS.~BT",
        Category::Cache,
        Text::new("系统升级临时文件", "Windows upgrade temporary files"),
    ),
    (
        "{SD}\\$Windows.~WS",
        Category::Cache,
        Text::new("系统升级临时文件", "Windows upgrade temporary files"),
    ),
    (
        "{SD}\\$SysReset",
        Category::Cache,
        Text::new("系统重置临时文件", "System reset temporary files"),
    ),
    (
        "{SD}\\PerfLogs",
        Category::Cache,
        Text::new("性能日志", "Performance logs"),
    ),
];

pub(super) const WIN_FILES: &[(&str, Category, Text)] = &[
    (
        "{SD}\\pagefile.sys",
        Category::Core,
        Text::new("虚拟内存页面文件", "Virtual memory page file"),
    ),
    (
        "{SD}\\swapfile.sys",
        Category::Core,
        Text::new("虚拟内存交换文件", "Virtual memory swap file"),
    ),
    (
        "{SD}\\hiberfil.sys",
        Category::Core,
        Text::new(
            "休眠文件（以管理员运行 powercfg -h off 关闭休眠即可删除）",
            "Hibernation file (turning hibernation off with powercfg -h off as administrator removes it)",
        ),
    ),
    (
        "{SD}\\bootmgr",
        Category::Core,
        Text::new("启动管理器", "Boot manager"),
    ),
    (
        "{SD}\\BOOTNXT",
        Category::Core,
        Text::new("启动文件", "Boot files"),
    ),
];

pub(super) const WIN_HOME: &[(&str, Category, Text)] = &[
    (
        "AppData",
        Category::Other,
        Text::new("应用数据 AppData", "Application data (AppData)"),
    ),
    (
        "AppData/Local/Temp",
        Category::Cache,
        Text::new("临时文件", "Temporary files"),
    ),
    (
        "AppData/Local/Microsoft/Windows/INetCache",
        Category::Cache,
        Text::new("浏览器缓存", "Browser cache"),
    ),
    (
        "AppData/Local/CrashDumps",
        Category::Cache,
        Text::new("崩溃转储", "Crash dumps"),
    ),
    (
        "AppData/Local/Packages",
        Category::Other,
        Text::new("应用商店应用的数据", "Data of Microsoft Store apps"),
    ),
    (
        "AppData/Local/npm-cache",
        Category::Cache,
        Text::new("npm 下载缓存", "npm download cache"),
    ),
    (
        "AppData/Local/pip/cache",
        Category::Cache,
        Text::new("pip 下载缓存", "pip download cache"),
    ),
    (
        "AppData/Local/Yarn/Cache",
        Category::Cache,
        Text::new("Yarn 依赖缓存", "Yarn package cache"),
    ),
    (
        ".nuget/packages",
        Category::Cache,
        Text::new("NuGet 包缓存", "NuGet package cache"),
    ),
    (
        "OneDrive",
        Category::User,
        Text::new("OneDrive 云盘", "OneDrive"),
    ),
];

pub(super) const COMMON_HOME: &[(&str, Category, Text)] = &[
    (
        ".cache",
        Category::Cache,
        Text::new("用户缓存 ~/.cache", "User cache ~/.cache"),
    ),
    (
        ".config",
        Category::Config,
        Text::new("用户配置 ~/.config", "User settings ~/.config"),
    ),
    (
        ".ssh",
        Category::Config,
        Text::new(
            "SSH 密钥与配置（重要，勿删）",
            "SSH keys and settings (important, do not delete)",
        ),
    ),
    (
        ".gnupg",
        Category::Config,
        Text::new(
            "GPG 密钥（重要，勿删）",
            "GPG keys (important, do not delete)",
        ),
    ),
    (
        ".npm/_cacache",
        Category::Cache,
        Text::new("npm 下载缓存", "npm download cache"),
    ),
    (
        ".cargo/registry",
        Category::Cache,
        Text::new("Cargo 依赖下载缓存", "Cargo download cache"),
    ),
    (
        ".gradle/caches",
        Category::Cache,
        Text::new("Gradle 缓存", "Gradle cache"),
    ),
    (
        ".m2/repository",
        Category::Cache,
        Text::new("Maven 依赖缓存", "Maven repository cache"),
    ),
    (
        "go/pkg/mod",
        Category::Cache,
        Text::new("Go 模块缓存", "Go module cache"),
    ),
    (
        ".pnpm-store",
        Category::Cache,
        Text::new("pnpm 依赖缓存", "pnpm package store"),
    ),
    (
        ".yarn/cache",
        Category::Cache,
        Text::new("Yarn 依赖缓存", "Yarn package cache"),
    ),
];

pub const CACHE_DIR_NAMES: &[&str] = &[
    ".Trash",
    ".cache",
    ".gradle",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "Cache",
    "CachedData",
    "CachedExtensionVSIXs",
    "Caches",
    "Code Cache",
    "CrashDumps",
    "D3DSCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DerivedData",
    "GPUCache",
    "GrShaderCache",
    "INetCache",
    "Service Worker",
    "ShaderCache",
    "Trash",
    "__pycache__",
    "_cacache",
    "cache",
    "caches",
    "npm-cache",
    "thumbnails",
];
pub const CACHE_EXTS: &[&str] = &[
    ".log",
    ".tmp",
    ".temp",
    ".bak",
    ".old",
    ".swp",
    ".part",
    ".crdownload",
    ".dmp",
    "~",
];

pub const CLASSES: [ClassInfo; 7] = [
    ClassInfo {
        id: 0,
        key: "core",
        label: Text::new("系统核心", "System core"),
        desc: Text::new(
            "引导程序、内核与驱动、系统卷、虚拟内存文件。删除或改动可能导致无法开机。",
            "Boot loader, kernel and drivers, system volume, virtual memory files. Deleting or changing them can make the system unbootable.",
        ),
    },
    ClassInfo {
        id: 1,
        key: "running",
        label: Text::new("正在使用", "In use"),
        desc: Text::new(
            "正被运行中的程序执行、加载（动态库）或打开的文件。",
            "Files that a running program is executing, has loaded (libraries) or has open.",
        ),
    },
    ClassInfo {
        id: 2,
        key: "system",
        label: Text::new("系统文件", "System files"),
        desc: Text::new(
            "操作系统自带或已安装程序的文件，应通过卸载程序/包管理器移除，而不是手动删除。",
            "Files of the operating system or installed programs. Remove them by uninstalling (package manager / app settings), not by deleting them by hand.",
        ),
    },
    ClassInfo {
        id: 3,
        key: "config",
        label: Text::new("配置文件", "Settings"),
        desc: Text::new(
            "系统设置与用户设置、注册表、SSH/GPG 密钥等。",
            "System and user settings, registry, SSH/GPG keys and the like.",
        ),
    },
    ClassInfo {
        id: 4,
        key: "user",
        label: Text::new("用户数据", "Personal files"),
        desc: Text::new(
            "个人文件：文档、下载、项目、媒体等。",
            "Personal files: documents, downloads, projects, media.",
        ),
    },
    ClassInfo {
        id: 5,
        key: "cache",
        label: Text::new("缓存/临时", "Cache / temp"),
        desc: Text::new(
            "缓存、日志、临时文件、回收站、可重新下载的依赖包。通常可以安全清理。",
            "Caches, logs, temporary files, trash, re-downloadable packages. Usually safe to clean up.",
        ),
    },
    ClassInfo {
        id: 6,
        key: "other",
        label: Text::new("其他/应用数据", "App data / other"),
        desc: Text::new(
            "未归入以上类别的程序数据，例如数据库、容器镜像、AppData、~/Library 中的应用数据。",
            "Program data not in the categories above, e.g. databases, container images, AppData, application data in ~/Library.",
        ),
    },
];
