"""重要程度分类规则（Linux / macOS / Windows 各有一套路径规则）。

每个文件/文件夹会被归入一个"静态"类别（由路径、软件包归属决定），
"正在使用"则是运行时叠加上去的动态类别（见 procinfo.py）。
"""
import ntpath
import os
import posixpath

from .osinfo import OS_NAME

try:
    import pwd
except ImportError:  # Windows
    pwd = None

CORE, RUNNING, SYSTEM, CONFIG, USER, CACHE, OTHER = range(7)
NCLS = 7

CLASSES = [
    {"id": CORE, "key": "core", "label": "系统核心",
     "desc": "引导程序、内核与驱动、系统卷、虚拟内存文件。删除或改动可能导致无法开机。"},
    {"id": RUNNING, "key": "running", "label": "正在使用",
     "desc": "正被运行中的程序执行、加载（动态库）或打开的文件。"},
    {"id": SYSTEM, "key": "system", "label": "系统文件",
     "desc": "操作系统自带或已安装程序的文件，应通过卸载程序/包管理器移除，而不是手动删除。"},
    {"id": CONFIG, "key": "config", "label": "配置文件",
     "desc": "系统设置与用户设置、注册表、SSH/GPG 密钥等。"},
    {"id": USER, "key": "user", "label": "用户数据",
     "desc": "个人文件：文档、下载、项目、媒体等。"},
    {"id": CACHE, "key": "cache", "label": "缓存/临时",
     "desc": "缓存、日志、临时文件、回收站、可重新下载的依赖包。通常可以安全清理。"},
    {"id": OTHER, "key": "other", "label": "其他/应用数据",
     "desc": "未归入以上类别的程序数据，例如数据库、容器镜像、AppData、~/Library 中的应用数据。"},
]

# ---------------------------------------------------------------- Linux
_LINUX_DIRS = {
    "/boot": (CORE, "引导分区：内核、initramfs 与引导程序"),
    "/efi": (CORE, "EFI 系统分区"),
    "/usr/lib/modules": (CORE, "内核驱动模块"),
    "/usr/lib/firmware": (CORE, "硬件固件"),
    "/lib/modules": (CORE, "内核驱动模块"),
    "/lib/firmware": (CORE, "硬件固件"),
    "/etc": (CONFIG, "系统配置目录 /etc"),
    "/usr": (SYSTEM, "系统程序目录 /usr"),
    "/usr/local": (SYSTEM, "本地安装的程序 /usr/local"),
    "/opt": (SYSTEM, "第三方程序目录 /opt"),
    "/bin": (SYSTEM, "系统程序目录"),
    "/sbin": (SYSTEM, "系统程序目录"),
    "/lib": (SYSTEM, "系统库目录"),
    "/lib32": (SYSTEM, "系统库目录"),
    "/lib64": (SYSTEM, "系统库目录"),
    "/var": (OTHER, "程序运行数据 /var"),
    "/var/cache": (CACHE, "系统缓存 /var/cache（如软件包下载缓存）"),
    "/var/tmp": (CACHE, "临时文件目录"),
    "/var/log": (CACHE, "系统日志"),
    "/var/lib/systemd/coredump": (CACHE, "程序崩溃转储"),
    "/var/lib/pacman": (SYSTEM, "pacman 软件包数据库"),
    "/var/lib/dpkg": (SYSTEM, "dpkg 软件包数据库"),
    "/var/lib/rpm": (SYSTEM, "rpm 软件包数据库"),
    "/var/lib/flatpak": (SYSTEM, "Flatpak 应用与运行时"),
    "/var/lib/snapd": (SYSTEM, "Snap 应用"),
    "/tmp": (CACHE, "临时文件目录"),
    "/home": (USER, "用户主目录"),
    "/root": (USER, "root 用户主目录"),
    "/srv": (USER, "服务数据 /srv"),
    "/mnt": (USER, "手动挂载的磁盘"),
    "/media": (USER, "可移动介质"),
    "/lost+found": (OTHER, "文件系统修复残留"),
}
_LINUX_FILES = {
    "/swapfile": (CORE, "交换文件（虚拟内存）"),
    "/swap.img": (CORE, "交换文件（虚拟内存）"),
}
_LINUX_HOME = {
    ".local": (USER, "用户程序数据 ~/.local"),
    ".local/share/Trash": (CACHE, "回收站"),
    ".local/state": (OTHER, "程序状态、历史记录与日志 ~/.local/state"),
    ".local/bin": (OTHER, "用户自行安装的程序"),
    ".local/share/pnpm/store": (CACHE, "pnpm 依赖缓存"),
    ".var": (USER, "Flatpak 应用的用户数据"),
    ".pki": (CONFIG, "证书数据库"),
    ".password-store": (CONFIG, "密码库"),
    ".mozilla": (CONFIG, "Firefox 用户配置"),
    ".thumbnails": (CACHE, "缩略图缓存"),
}

# ---------------------------------------------------------------- macOS
_MAC_DIRS = {
    "/System": (CORE, "macOS 系统卷（受系统完整性保护，只读）"),
    "/Library": (SYSTEM, "系统级程序资源 /Library"),
    "/Library/Caches": (CACHE, "系统缓存"),
    "/Library/Logs": (CACHE, "系统日志"),
    "/Library/Application Support": (OTHER, "程序共享数据"),
    "/Library/Preferences": (CONFIG, "系统设置"),
    "/Applications": (SYSTEM, "应用程序"),
    "/usr": (SYSTEM, "系统程序目录 /usr"),
    "/usr/local": (SYSTEM, "Homebrew（Intel）或本地安装的程序"),
    "/bin": (CORE, "系统基本命令"),
    "/sbin": (CORE, "系统管理命令"),
    "/opt": (SYSTEM, "第三方程序 /opt"),
    "/opt/homebrew": (SYSTEM, "Homebrew 安装的软件"),
    "/private": (OTHER, "系统运行数据 /private"),
    "/private/etc": (CONFIG, "系统配置"),
    "/private/var": (OTHER, "程序运行数据 /private/var"),
    "/private/var/vm": (CORE, "虚拟内存交换文件与睡眠镜像"),
    "/private/var/log": (CACHE, "系统日志"),
    "/private/var/folders": (CACHE, "系统为各用户分配的临时与缓存目录"),
    "/private/var/tmp": (CACHE, "临时文件"),
    "/private/tmp": (CACHE, "临时文件"),
    "/private/var/db": (SYSTEM, "系统数据库"),
    "/cores": (CACHE, "程序崩溃转储"),
    "/Users": (USER, "用户主目录"),
    "/Volumes": (USER, "其他磁盘"),
}
_MAC_HOME = {
    "Library": (OTHER, "应用数据 ~/Library"),
    "Library/Caches": (CACHE, "应用缓存 ~/Library/Caches"),
    "Library/Logs": (CACHE, "应用日志"),
    "Library/Preferences": (CONFIG, "应用设置 ~/Library/Preferences"),
    "Library/Application Support": (OTHER, "应用数据"),
    "Library/Containers": (OTHER, "沙盒应用的数据"),
    "Library/Group Containers": (OTHER, "应用共享数据"),
    "Library/Mobile Documents": (USER, "iCloud 云盘"),
    "Library/Mail": (USER, "邮件"),
    "Library/Messages": (USER, "信息记录"),
    "Library/Developer/Xcode/DerivedData": (CACHE, "Xcode 编译缓存"),
    "Library/Developer/Xcode/iOS DeviceSupport": (CACHE, "Xcode 设备支持文件（可重新下载）"),
    "Library/Developer/CoreSimulator": (OTHER, "iOS 模拟器"),
    ".Trash": (CACHE, "废纸篓"),
}

# ---------------------------------------------------------------- Windows（{SD} 系统盘，{SR} Windows 目录）
_WIN_DIRS = {
    "{SR}": (SYSTEM, "Windows 系统目录"),
    "{SR}\\System32\\drivers": (CORE, "设备驱动程序"),
    "{SR}\\System32\\config": (CORE, "注册表数据库"),
    "{SR}\\Boot": (CORE, "Windows 启动文件"),
    "{SR}\\WinSxS": (SYSTEM, "系统组件存储（含大量硬链接，显示值通常偏大；可用「磁盘清理」瘦身）"),
    "{SR}\\Installer": (SYSTEM, "已安装程序的安装包缓存（勿手动删除）"),
    "{SR}\\Temp": (CACHE, "系统临时文件"),
    "{SR}\\Logs": (CACHE, "系统日志"),
    "{SR}\\Prefetch": (CACHE, "预读取缓存"),
    "{SR}\\SoftwareDistribution\\Download": (CACHE, "Windows 更新下载缓存"),
    "{SR}\\LiveKernelReports": (CACHE, "内核错误报告"),
    "{SR}\\Minidump": (CACHE, "崩溃转储"),
    "{PF}": (SYSTEM, "已安装的程序 Program Files"),
    "{PF86}": (SYSTEM, "已安装的程序 Program Files (x86)"),
    "{PD}": (OTHER, "程序共享数据 ProgramData"),
    "{PD}\\Package Cache": (OTHER, "安装包缓存（卸载或修复程序时需要）"),
    "{SD}\\Users": (USER, "用户文件夹"),
    "{SD}\\$Recycle.Bin": (CACHE, "回收站"),
    "{SD}\\System Volume Information": (CORE, "系统还原点与卷信息"),
    "{SD}\\Recovery": (CORE, "Windows 恢复环境"),
    "{SD}\\Boot": (CORE, "启动文件"),
    "{SD}\\EFI": (CORE, "EFI 启动文件"),
    "{SD}\\Windows.old": (CACHE, "旧版 Windows 备份（可用「磁盘清理」删除）"),
    "{SD}\\$WINDOWS.~BT": (CACHE, "系统升级临时文件"),
    "{SD}\\$Windows.~WS": (CACHE, "系统升级临时文件"),
    "{SD}\\$SysReset": (CACHE, "系统重置临时文件"),
    "{SD}\\PerfLogs": (CACHE, "性能日志"),
}
_WIN_FILES = {
    "{SD}\\pagefile.sys": (CORE, "虚拟内存页面文件"),
    "{SD}\\swapfile.sys": (CORE, "虚拟内存交换文件"),
    "{SD}\\hiberfil.sys": (CORE, "休眠文件（以管理员运行 powercfg -h off 关闭休眠即可删除）"),
    "{SD}\\bootmgr": (CORE, "启动管理器"),
    "{SD}\\BOOTNXT": (CORE, "启动文件"),
}
_WIN_HOME = {
    "AppData": (OTHER, "应用数据 AppData"),
    "AppData/Local/Temp": (CACHE, "临时文件"),
    "AppData/Local/Microsoft/Windows/INetCache": (CACHE, "浏览器缓存"),
    "AppData/Local/CrashDumps": (CACHE, "崩溃转储"),
    "AppData/Local/Packages": (OTHER, "应用商店应用的数据"),
    "AppData/Local/npm-cache": (CACHE, "npm 下载缓存"),
    "AppData/Local/pip/cache": (CACHE, "pip 下载缓存"),
    "AppData/Local/Yarn/Cache": (CACHE, "Yarn 依赖缓存"),
    ".nuget/packages": (CACHE, "NuGet 包缓存"),
    "OneDrive": (USER, "OneDrive 云盘"),
}

# 所有系统通用的主目录规则
_COMMON_HOME = {
    ".cache": (CACHE, "用户缓存 ~/.cache"),
    ".config": (CONFIG, "用户配置 ~/.config"),
    ".ssh": (CONFIG, "SSH 密钥与配置（重要，勿删）"),
    ".gnupg": (CONFIG, "GPG 密钥（重要，勿删）"),
    ".npm/_cacache": (CACHE, "npm 下载缓存"),
    ".cargo/registry": (CACHE, "Cargo 依赖下载缓存"),
    ".gradle/caches": (CACHE, "Gradle 缓存"),
    ".m2/repository": (CACHE, "Maven 依赖缓存"),
    "go/pkg/mod": (CACHE, "Go 模块缓存"),
    ".pnpm-store": (CACHE, "pnpm 依赖缓存"),
    ".yarn/cache": (CACHE, "Yarn 依赖缓存"),
}

# 出现在用户/数据区域时视为缓存的文件夹名
CACHE_DIR_NAMES = frozenset({
    ".cache", "cache", "Cache", "caches", "Caches", "__pycache__", ".pytest_cache",
    ".mypy_cache", ".ruff_cache", "Trash", ".Trash", "GPUCache", "Code Cache",
    "ShaderCache", "GrShaderCache", "DawnCache", "DawnGraphiteCache", "CachedData",
    "CachedExtensionVSIXs", "Service Worker", "thumbnails", ".gradle", "_cacache",
    "INetCache", "CrashDumps", "D3DSCache", "npm-cache", "DerivedData",
})

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
            self.file_rules[self._k(s)] = (CORE, "交换文件（虚拟内存）")
        # Windows 注册表 / Homebrew 里登记的程序安装目录
        for path, name in (getattr(pkgdb, "app_dirs", None) or {}).items():
            self.dir_rules.setdefault(self._k(path), (SYSTEM, f"已安装的程序：{name}"))

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
                return (OTHER, "应用数据（主目录下的隐藏文件夹）")
            return (USER, "用户主目录中的个人文件")
        if parent_cls == CORE or parent_cls == SYSTEM:
            return (parent_cls, parent_reason)
        if (name.lower() if win else name) in self.cache_names:
            # 包括 ~/.config/Code/Cache 这类藏在配置目录里的 Electron 缓存
            return (CACHE, "缓存文件夹")
        return (parent_cls, parent_reason)

    def _root_rule(self, root):
        rule = self.dir_rules.get(self._k(root))
        if rule is not None:
            return rule
        return (OTHER, "磁盘根目录" if self.win else "根目录")

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
                return (CONFIG, f"软件包 {owner} 的配置文件")
            if dir_cls == CORE:
                return (CORE, f"由软件包 {owner} 安装 · {dir_reason}")
            return (SYSTEM, f"由软件包 {owner} 安装")
        if dir_cls <= CONFIG or dir_cls == CACHE:
            return (dir_cls, dir_reason)
        lname = name.lower() if win else name
        if (dir_path.lower() if win else dir_path) in self.homes:
            if name.startswith("."):
                return (CONFIG, "主目录下的点文件（通常是配置）")
            if win and lname.startswith("ntuser."):
                return (CONFIG, "用户注册表文件")
        if lname.endswith(CACHE_EXTS):
            return (CACHE, "日志/临时/备份类文件")
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
