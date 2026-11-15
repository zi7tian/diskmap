"""重要程度分类规则（Linux / macOS / Windows 各有一套路径规则）。

每个文件/文件夹会被归入一个"静态"类别（由路径、软件包归属决定），
"正在使用"则是运行时叠加上去的动态类别（见 procinfo.py）。
"""
import ntpath
import os
import posixpath

from .i18n import T
from .osinfo import OS_NAME

try:
    import pwd
except ImportError:  # Windows
    pwd = None

CORE, RUNNING, SYSTEM, CONFIG, USER, CACHE, OTHER = range(7)
NCLS = 7

CLASSES = [
    {"id": CORE, "key": "core", "label": T("系统核心", "System core"),
     "desc": T("引导程序、内核与驱动、系统卷、虚拟内存文件。删除或改动可能导致无法开机。",
               "Boot loader, kernel and drivers, system volume, virtual memory files. "
               "Deleting or changing them can make the system unbootable.")},
    {"id": RUNNING, "key": "running", "label": T("正在使用", "In use"),
     "desc": T("正被运行中的程序执行、加载（动态库）或打开的文件。",
               "Files that a running program is executing, has loaded (libraries) or has open.")},
    {"id": SYSTEM, "key": "system", "label": T("系统文件", "System files"),
     "desc": T("操作系统自带或已安装程序的文件，应通过卸载程序/包管理器移除，而不是手动删除。",
               "Files of the operating system or installed programs. Remove them by uninstalling "
               "(package manager / app settings), not by deleting them by hand.")},
    {"id": CONFIG, "key": "config", "label": T("配置文件", "Settings"),
     "desc": T("系统设置与用户设置、注册表、SSH/GPG 密钥等。",
               "System and user settings, registry, SSH/GPG keys and the like.")},
    {"id": USER, "key": "user", "label": T("用户数据", "Personal files"),
     "desc": T("个人文件：文档、下载、项目、媒体等。",
               "Personal files: documents, downloads, projects, media.")},
    {"id": CACHE, "key": "cache", "label": T("缓存/临时", "Cache / temp"),
     "desc": T("缓存、日志、临时文件、回收站、可重新下载的依赖包。通常可以安全清理。",
               "Caches, logs, temporary files, trash, re-downloadable packages. "
               "Usually safe to clean up.")},
    {"id": OTHER, "key": "other", "label": T("其他/应用数据", "App data / other"),
     "desc": T("未归入以上类别的程序数据，例如数据库、容器镜像、AppData、~/Library 中的应用数据。",
               "Program data not in the categories above, e.g. databases, container images, "
               "AppData, application data in ~/Library.")},
]

# 常用的原因文字
_R_SYSBIN = T("系统程序目录", "System programs")
_R_SYSLIB = T("系统库目录", "System libraries")
_R_MODULES = T("内核驱动模块", "Kernel driver modules")
_R_FIRMWARE = T("硬件固件", "Hardware firmware")
_R_TMP = T("临时文件目录", "Temporary files")
_R_TEMP = T("临时文件", "Temporary files")
_R_SYSLOG = T("系统日志", "System logs")
_R_COREDUMP = T("程序崩溃转储", "Crash dumps")
_R_SWAP = T("交换文件（虚拟内存）", "Swap file (virtual memory)")
_R_HOMES = T("用户主目录", "Home folders")
_R_TRASH = T("回收站", "Trash")
_R_APPDATA = T("应用数据", "Application data")
_R_NPM = T("npm 下载缓存", "npm download cache")
_R_PNPM = T("pnpm 依赖缓存", "pnpm package store")
_R_YARN = T("Yarn 依赖缓存", "Yarn package cache")
_R_UPGRADE = T("系统升级临时文件", "Windows upgrade temporary files")
_R_BOOT = T("启动文件", "Boot files")

# ---------------------------------------------------------------- Linux
_LINUX_DIRS = {
    "/boot": (CORE, T("引导分区：内核、initramfs 与引导程序", "Boot partition: kernel, initramfs and boot loader")),
    "/efi": (CORE, T("EFI 系统分区", "EFI system partition")),
    "/usr/lib/modules": (CORE, _R_MODULES),
    "/usr/lib/firmware": (CORE, _R_FIRMWARE),
    "/lib/modules": (CORE, _R_MODULES),
    "/lib/firmware": (CORE, _R_FIRMWARE),
    "/etc": (CONFIG, T("系统配置目录 /etc", "System configuration /etc")),
    "/usr": (SYSTEM, T("系统程序目录 /usr", "System programs /usr")),
    "/usr/local": (SYSTEM, T("本地安装的程序 /usr/local", "Locally installed programs /usr/local")),
    "/opt": (SYSTEM, T("第三方程序目录 /opt", "Third-party programs /opt")),
    "/bin": (SYSTEM, _R_SYSBIN),
    "/sbin": (SYSTEM, _R_SYSBIN),
    "/lib": (SYSTEM, _R_SYSLIB),
    "/lib32": (SYSTEM, _R_SYSLIB),
    "/lib64": (SYSTEM, _R_SYSLIB),
    "/var": (OTHER, T("程序运行数据 /var", "Program data /var")),
    "/var/cache": (CACHE, T("系统缓存 /var/cache（如软件包下载缓存）", "System cache /var/cache (e.g. downloaded packages)")),
    "/var/tmp": (CACHE, _R_TMP),
    "/var/log": (CACHE, _R_SYSLOG),
    "/var/lib/systemd/coredump": (CACHE, _R_COREDUMP),
    "/var/lib/pacman": (SYSTEM, T("pacman 软件包数据库", "pacman package database")),
    "/var/lib/dpkg": (SYSTEM, T("dpkg 软件包数据库", "dpkg package database")),
    "/var/lib/rpm": (SYSTEM, T("rpm 软件包数据库", "rpm package database")),
    "/var/lib/flatpak": (SYSTEM, T("Flatpak 应用与运行时", "Flatpak apps and runtimes")),
    "/var/lib/snapd": (SYSTEM, T("Snap 应用", "Snap apps")),
    "/tmp": (CACHE, _R_TMP),
    "/home": (USER, _R_HOMES),
    "/root": (USER, T("root 用户主目录", "Home folder of root")),
    "/srv": (USER, T("服务数据 /srv", "Service data /srv")),
    "/mnt": (USER, T("手动挂载的磁盘", "Manually mounted disks")),
    "/media": (USER, T("可移动介质", "Removable media")),
    "/lost+found": (OTHER, T("文件系统修复残留", "Leftovers from file system repair")),
}
_LINUX_FILES = {
    "/swapfile": (CORE, _R_SWAP),
    "/swap.img": (CORE, _R_SWAP),
}
_LINUX_HOME = {
    ".local": (USER, T("用户程序数据 ~/.local", "User program data ~/.local")),
    ".local/share/Trash": (CACHE, _R_TRASH),
    ".local/state": (OTHER, T("程序状态、历史记录与日志 ~/.local/state", "Program state, history and logs ~/.local/state")),
    ".local/bin": (OTHER, T("用户自行安装的程序", "Programs installed by the user")),
    ".local/share/pnpm/store": (CACHE, _R_PNPM),
    ".var": (USER, T("Flatpak 应用的用户数据", "User data of Flatpak apps")),
    ".pki": (CONFIG, T("证书数据库", "Certificate database")),
    ".password-store": (CONFIG, T("密码库", "Password store")),
    ".mozilla": (CONFIG, T("Firefox 用户配置", "Firefox profile")),
    ".thumbnails": (CACHE, T("缩略图缓存", "Thumbnail cache")),
}

# ---------------------------------------------------------------- macOS
_MAC_DIRS = {
    "/System": (CORE, T("macOS 系统卷（受系统完整性保护，只读）", "macOS system volume (read-only, protected by SIP)")),
    "/Library": (SYSTEM, T("系统级程序资源 /Library", "System-wide program resources /Library")),
    "/Library/Caches": (CACHE, T("系统缓存", "System caches")),
    "/Library/Logs": (CACHE, _R_SYSLOG),
    "/Library/Application Support": (OTHER, T("程序共享数据", "Shared program data")),
    "/Library/Preferences": (CONFIG, T("系统设置", "System settings")),
    "/Applications": (SYSTEM, T("应用程序", "Applications")),
    "/usr": (SYSTEM, T("系统程序目录 /usr", "System programs /usr")),
    "/usr/local": (SYSTEM, T("Homebrew（Intel）或本地安装的程序", "Homebrew (Intel) or locally installed programs")),
    "/bin": (CORE, T("系统基本命令", "Basic system commands")),
    "/sbin": (CORE, T("系统管理命令", "System administration commands")),
    "/opt": (SYSTEM, T("第三方程序 /opt", "Third-party programs /opt")),
    "/opt/homebrew": (SYSTEM, T("Homebrew 安装的软件", "Software installed by Homebrew")),
    "/private": (OTHER, T("系统运行数据 /private", "System data /private")),
    "/private/etc": (CONFIG, T("系统配置", "System configuration")),
    "/private/var": (OTHER, T("程序运行数据 /private/var", "Program data /private/var")),
    "/private/var/vm": (CORE, T("虚拟内存交换文件与睡眠镜像", "Virtual memory swap files and sleep image")),
    "/private/var/log": (CACHE, _R_SYSLOG),
    "/private/var/folders": (CACHE, T("系统为各用户分配的临时与缓存目录", "Per-user temporary and cache folders")),
    "/private/var/tmp": (CACHE, _R_TEMP),
    "/private/tmp": (CACHE, _R_TEMP),
    "/private/var/db": (SYSTEM, T("系统数据库", "System databases")),
    "/cores": (CACHE, _R_COREDUMP),
    "/Users": (USER, _R_HOMES),
    "/Volumes": (USER, T("其他磁盘", "Other disks")),
}
_MAC_HOME = {
    "Library": (OTHER, T("应用数据 ~/Library", "Application data ~/Library")),
    "Library/Caches": (CACHE, T("应用缓存 ~/Library/Caches", "Application caches ~/Library/Caches")),
    "Library/Logs": (CACHE, T("应用日志", "Application logs")),
    "Library/Preferences": (CONFIG, T("应用设置 ~/Library/Preferences", "Application settings ~/Library/Preferences")),
    "Library/Application Support": (OTHER, _R_APPDATA),
    "Library/Containers": (OTHER, T("沙盒应用的数据", "Data of sandboxed apps")),
    "Library/Group Containers": (OTHER, T("应用共享数据", "Data shared between apps")),
    "Library/Mobile Documents": (USER, T("iCloud 云盘", "iCloud Drive")),
    "Library/Mail": (USER, T("邮件", "Mail")),
    "Library/Messages": (USER, T("信息记录", "Messages")),
    "Library/Developer/Xcode/DerivedData": (CACHE, T("Xcode 编译缓存", "Xcode build cache")),
    "Library/Developer/Xcode/iOS DeviceSupport": (CACHE, T("Xcode 设备支持文件（可重新下载）", "Xcode device support files (re-downloadable)")),
    "Library/Developer/CoreSimulator": (OTHER, T("iOS 模拟器", "iOS simulators")),
    ".Trash": (CACHE, T("废纸篓", "Trash")),
}

# ---------------------------------------------------------------- Windows（{SD} 系统盘，{SR} Windows 目录）
_WIN_DIRS = {
    "{SR}": (SYSTEM, T("Windows 系统目录", "Windows system folder")),
    "{SR}\\System32\\drivers": (CORE, T("设备驱动程序", "Device drivers")),
    "{SR}\\System32\\config": (CORE, T("注册表数据库", "Registry database")),
    "{SR}\\Boot": (CORE, T("Windows 启动文件", "Windows boot files")),
    "{SR}\\WinSxS": (SYSTEM, T("系统组件存储（含大量硬链接，显示值通常偏大；可用「磁盘清理」瘦身）",
                               "Component store (many hard links, so the size shown is usually too high; "
                               "shrink it with Disk Cleanup)")),
    "{SR}\\Installer": (SYSTEM, T("已安装程序的安装包缓存（勿手动删除）",
                                  "Installer cache of installed programs (do not delete by hand)")),
    "{SR}\\Temp": (CACHE, T("系统临时文件", "System temporary files")),
    "{SR}\\Logs": (CACHE, _R_SYSLOG),
    "{SR}\\Prefetch": (CACHE, T("预读取缓存", "Prefetch cache")),
    "{SR}\\SoftwareDistribution\\Download": (CACHE, T("Windows 更新下载缓存", "Windows Update download cache")),
    "{SR}\\LiveKernelReports": (CACHE, T("内核错误报告", "Kernel error reports")),
    "{SR}\\Minidump": (CACHE, T("崩溃转储", "Crash dumps")),
    "{PF}": (SYSTEM, T("已安装的程序 Program Files", "Installed programs (Program Files)")),
    "{PF86}": (SYSTEM, T("已安装的程序 Program Files (x86)", "Installed programs (Program Files (x86))")),
    "{PD}": (OTHER, T("程序共享数据 ProgramData", "Shared program data (ProgramData)")),
    "{PD}\\Package Cache": (OTHER, T("安装包缓存（卸载或修复程序时需要）",
                                     "Installer cache (needed to uninstall or repair programs)")),
    "{SD}\\Users": (USER, T("用户文件夹", "User folders")),
    "{SD}\\$Recycle.Bin": (CACHE, T("回收站", "Recycle Bin")),
    "{SD}\\System Volume Information": (CORE, T("系统还原点与卷信息", "System restore points and volume information")),
    "{SD}\\Recovery": (CORE, T("Windows 恢复环境", "Windows Recovery Environment")),
    "{SD}\\Boot": (CORE, _R_BOOT),
    "{SD}\\EFI": (CORE, T("EFI 启动文件", "EFI boot files")),
    "{SD}\\Windows.old": (CACHE, T("旧版 Windows 备份（可用「磁盘清理」删除）",
                                   "Previous Windows installation (remove it with Disk Cleanup)")),
    "{SD}\\$WINDOWS.~BT": (CACHE, _R_UPGRADE),
    "{SD}\\$Windows.~WS": (CACHE, _R_UPGRADE),
    "{SD}\\$SysReset": (CACHE, T("系统重置临时文件", "System reset temporary files")),
    "{SD}\\PerfLogs": (CACHE, T("性能日志", "Performance logs")),
}
_WIN_FILES = {
    "{SD}\\pagefile.sys": (CORE, T("虚拟内存页面文件", "Virtual memory page file")),
    "{SD}\\swapfile.sys": (CORE, T("虚拟内存交换文件", "Virtual memory swap file")),
    "{SD}\\hiberfil.sys": (CORE, T("休眠文件（以管理员运行 powercfg -h off 关闭休眠即可删除）",
                                   "Hibernation file (turning hibernation off with powercfg -h off as "
                                   "administrator removes it)")),
    "{SD}\\bootmgr": (CORE, T("启动管理器", "Boot manager")),
    "{SD}\\BOOTNXT": (CORE, _R_BOOT),
}
_WIN_HOME = {
    "AppData": (OTHER, T("应用数据 AppData", "Application data (AppData)")),
    "AppData/Local/Temp": (CACHE, _R_TEMP),
    "AppData/Local/Microsoft/Windows/INetCache": (CACHE, T("浏览器缓存", "Browser cache")),
    "AppData/Local/CrashDumps": (CACHE, T("崩溃转储", "Crash dumps")),
    "AppData/Local/Packages": (OTHER, T("应用商店应用的数据", "Data of Microsoft Store apps")),
    "AppData/Local/npm-cache": (CACHE, _R_NPM),
    "AppData/Local/pip/cache": (CACHE, T("pip 下载缓存", "pip download cache")),
    "AppData/Local/Yarn/Cache": (CACHE, _R_YARN),
    ".nuget/packages": (CACHE, T("NuGet 包缓存", "NuGet package cache")),
    "OneDrive": (USER, T("OneDrive 云盘", "OneDrive")),
}

# 所有系统通用的主目录规则
_COMMON_HOME = {
    ".cache": (CACHE, T("用户缓存 ~/.cache", "User cache ~/.cache")),
    ".config": (CONFIG, T("用户配置 ~/.config", "User settings ~/.config")),
    ".ssh": (CONFIG, T("SSH 密钥与配置（重要，勿删）", "SSH keys and settings (important, do not delete)")),
    ".gnupg": (CONFIG, T("GPG 密钥（重要，勿删）", "GPG keys (important, do not delete)")),
    ".npm/_cacache": (CACHE, _R_NPM),
    ".cargo/registry": (CACHE, T("Cargo 依赖下载缓存", "Cargo download cache")),
    ".gradle/caches": (CACHE, T("Gradle 缓存", "Gradle cache")),
    ".m2/repository": (CACHE, T("Maven 依赖缓存", "Maven repository cache")),
    "go/pkg/mod": (CACHE, T("Go 模块缓存", "Go module cache")),
    ".pnpm-store": (CACHE, _R_PNPM),
    ".yarn/cache": (CACHE, _R_YARN),
}

# 出现在用户/数据区域时视为缓存的文件夹名
CACHE_DIR_NAMES = frozenset({
    ".cache", "cache", "Cache", "caches", "Caches", "__pycache__", ".pytest_cache",
    ".mypy_cache", ".ruff_cache", "Trash", ".Trash", "GPUCache", "Code Cache",
    "ShaderCache", "GrShaderCache", "DawnCache", "DawnGraphiteCache", "CachedData",
    "CachedExtensionVSIXs", "Service Worker", "thumbnails", ".gradle", "_cacache",
    "INetCache", "CrashDumps", "D3DSCache", "npm-cache", "DerivedData",
})

_R_HIDDEN = T("应用数据（主目录下的隐藏文件夹）", "Application data (hidden folder in the home folder)")
_R_PERSONAL = T("用户主目录中的个人文件", "Personal files in the home folder")
_R_CACHEDIR = T("缓存文件夹", "Cache folder")
_R_DRIVE = T("磁盘根目录", "Drive root")
_R_ROOT = T("根目录", "Root folder")

CACHE_EXTS = (".log", ".tmp", ".temp", ".bak", ".old", ".swp", ".part",
              ".crdownload", ".dmp", "~")


def _discover_homes(flavor, env):
    homes = set()
    if flavor == "windows":
        users = os.path.join(env.get("SystemDrive", "C:") + "\\", "Users")
        try:
            for e in os.scandir(users):
                if e.is_dir(follow_symlinks=False) and e.name not in ("All Users", "Default User"):
                    homes.add(e.path)
        except OSError:
            pass
        if env.get("USERPROFILE"):
            homes.add(env["USERPROFILE"])
        return homes
    base = "/Users" if flavor == "macos" else "/home"
    if pwd is not None:
        try:
            for pw in pwd.getpwall():
                d = pw.pw_dir.rstrip("/")
                if d.startswith(base + "/") or d in ("/root", "/var/root"):
                    homes.add("/private/var/root" if d == "/var/root" else d)
        except Exception:
            pass
    try:
        for e in os.scandir(base):
            if e.is_dir(follow_symlinks=False):
                homes.add(e.path)
    except OSError:
        pass
    if os.name != "nt":
        homes.add(os.path.expanduser("~").rstrip("/") or "/")
    return homes


class Classifier:
    def __init__(self, pkgdb=None, swapfiles=(), flavor=None, env=None, homes=None):
        self.flavor = flavor or OS_NAME
        self.win = self.flavor == "windows"
        self.pm = ntpath if self.win else posixpath
        self.pkg = pkgdb
        env = os.environ if env is None else env
        if homes is None:
            homes = _discover_homes(self.flavor, env)
        self.homes = {self._k(h) for h in homes}
        self.cache_names = frozenset(n.lower() for n in CACHE_DIR_NAMES) if self.win else CACHE_DIR_NAMES
        self.dir_rules = {}
        self.file_rules = {}

        if self.flavor == "windows":
            sd = env.get("SystemDrive", "C:")
            subst = {
                "{SD}": sd,
                "{SR}": env.get("SystemRoot") or sd + "\\Windows",
                "{PF86}": env.get("ProgramFiles(x86)") or sd + "\\Program Files (x86)",
                "{PF}": env.get("ProgramFiles") or sd + "\\Program Files",
                "{PD}": env.get("ProgramData") or sd + "\\ProgramData",
            }

            def expand(p):
                for k, v in subst.items():
                    p = p.replace(k, v)
                return p
            self._add_rules(self.dir_rules, {expand(k): v for k, v in _WIN_DIRS.items()})
            self._add_rules(self.file_rules, {expand(k): v for k, v in _WIN_FILES.items()})
            home_rules = {**_COMMON_HOME, **_WIN_HOME}
        elif self.flavor == "macos":
            self._add_rules(self.dir_rules, _MAC_DIRS)
            home_rules = {**_COMMON_HOME, **_MAC_HOME}
        else:
            self._add_rules(self.dir_rules, _LINUX_DIRS)
            self._add_rules(self.file_rules, _LINUX_FILES)
            home_rules = {**_COMMON_HOME, **_LINUX_HOME}
        for h in homes:
            for rel, rule in home_rules.items():
                self.dir_rules[self._k(self.pm.join(h, *rel.split("/")))] = rule
        for s in swapfiles:
            self.file_rules[self._k(s)] = (CORE, _R_SWAP)
        # Windows 注册表 / Homebrew 里登记的程序安装目录
        for path, name in (getattr(pkgdb, "app_dirs", None) or {}).items():
            self.dir_rules.setdefault(self._k(path), (SYSTEM, T(f"已安装的程序：{name}", f"Installed program: {name}")))

    def _k(self, p):
        if self.win:
            p = ntpath.normcase(p)
            return p.rstrip("\\") if len(p) > 3 else p
        return p.rstrip("/") or "/"

    def _add_rules(self, target, rules):
        for p, rule in rules.items():
            target[self._k(p)] = rule

    # ---- 文件夹 ----
    def dir_class(self, parent_cls, parent_reason, parent_path, name, path):
        win = self.win
        rule = self.dir_rules.get(path.lower() if win else path)
        if rule is not None:
            return rule
        if (parent_path.lower() if win else parent_path) in self.homes:
            if name.startswith("."):
                return (OTHER, _R_HIDDEN)
            return (USER, _R_PERSONAL)
        if parent_cls == CORE or parent_cls == SYSTEM:
            return (parent_cls, parent_reason)
        if (name.lower() if win else name) in self.cache_names:
            # 包括 ~/.config/Code/Cache 这类藏在配置目录里的 Electron 缓存
            return (CACHE, _R_CACHEDIR)
        return (parent_cls, parent_reason)

    def _root_rule(self, root):
        rule = self.dir_rules.get(self._k(root))
        if rule is not None:
            return rule
        return (OTHER, _R_DRIVE if self.win else _R_ROOT)

    def root_class(self, path):
        """计算任意文件夹的类别：从根目录逐级推导。"""
        pm = self.pm
        if self.win:
            path = path.replace("/", "\\")
        drive, rest = pm.splitdrive(path)
        cur = drive + pm.sep
        cls, reason = self._root_rule(cur)
        for comp in rest.split(pm.sep):
            if not comp:
                continue
            parent = cur
            cur = cur + comp if cur.endswith(pm.sep) else cur + pm.sep + comp
            cls, reason = self.dir_class(cls, reason, parent, comp, cur)
        return cls, reason

    # ---- 文件 ----
    def file_class(self, dir_cls, dir_path, name, path):
        win = self.win
        rule = self.file_rules.get(path.lower() if win else path)
        if rule is not None:
            return rule[0]
        pkg = self.pkg
        if pkg is not None and path in pkg.files:
            if dir_cls == CONFIG or path in pkg.backup:
                return CONFIG
            if dir_cls == CORE:
                return CORE
            return SYSTEM
        if dir_cls <= CONFIG or dir_cls == CACHE:  # CORE / SYSTEM / CONFIG / CACHE 直接继承
            return dir_cls
        lname = name.lower() if win else name
        if (dir_path.lower() if win else dir_path) in self.homes:
            if name.startswith(".") or (win and lname.startswith("ntuser.")):
                return CONFIG
        if lname.endswith(CACHE_EXTS):
            return CACHE
        return dir_cls

    def explain_file(self, dir_cls, dir_reason, dir_path, name, path):
        win = self.win
        rule = self.file_rules.get(path.lower() if win else path)
        if rule is not None:
            return rule
        pkg = self.pkg
        owner = pkg.files.get(path) if pkg is not None else None
        if owner is not None:
            if dir_cls == CONFIG or path in pkg.backup:
                return (CONFIG, T(f"软件包 {owner} 的配置文件", f"Configuration file of package {owner}"))
            if dir_cls == CORE:
                zh = str.__str__(dir_reason or "")
                en = getattr(dir_reason, "en", zh)
                return (CORE, T(f"由软件包 {owner} 安装 · {zh}", f"Installed by package {owner} · {en}"))
            return (SYSTEM, T(f"由软件包 {owner} 安装", f"Installed by package {owner}"))
        if dir_cls <= CONFIG or dir_cls == CACHE:
            return (dir_cls, dir_reason)
        lname = name.lower() if win else name
        if (dir_path.lower() if win else dir_path) in self.homes:
            if name.startswith("."):
                return (CONFIG, T("主目录下的点文件（通常是配置）", "Dotfile in the home folder (usually settings)"))
            if win and lname.startswith("ntuser."):
                return (CONFIG, T("用户注册表文件", "User registry file"))
        if lname.endswith(CACHE_EXTS):
            return (CACHE, T("日志/临时/备份类文件", "Log, temporary or backup file"))
        return (dir_cls, dir_reason)

    def explain(self, path, is_dir):
        """返回 (类别, 原因)。"""
        if is_dir:
            return self.root_class(path)
        pm = self.pm
        if self.win:
            path = path.replace("/", "\\")
        parent, name = pm.dirname(path), pm.basename(path)
        dcls, dreason = self.root_class(parent)
        return self.explain_file(dcls, dreason, parent, name, path)
