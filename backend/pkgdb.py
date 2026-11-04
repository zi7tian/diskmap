"""读取包管理器数据库，得到"哪个文件属于哪个软件包"。

- Linux：pacman（Arch 系）、dpkg（Debian/Ubuntu 系）、rpm（Fedora/openSUSE 系），精确到每个文件
- Windows：注册表里登记的已安装程序及其安装目录
- macOS：Homebrew 安装的软件目录
"""
import os
import shutil
import subprocess
import sys
import time

from .osinfo import IS_MAC, IS_WIN


class PkgDB:
    def __init__(self):
        self.files = {}      # 绝对路径 -> 软件包名
        self.backup = set()  # 包管理器登记为"配置文件"的路径
        self.app_dirs = {}   # 程序安装目录 -> 程序名（Windows 注册表 / Homebrew）
        self.manager = None
        self.label = None    # 界面上显示的名称
        self.npkgs = 0
        self.load_time = 0.0
        self.error = None

    def load(self):
        t = time.monotonic()
        try:
            if IS_WIN:
                self._load_windows()
            elif IS_MAC:
                self._load_homebrew()
            elif os.path.isdir("/var/lib/pacman/local"):
                self._load_pacman("/var/lib/pacman/local")
            elif os.path.isdir("/var/lib/dpkg/info"):
                self._load_dpkg("/var/lib/dpkg/info")
            elif shutil.which("rpm"):
                self._load_rpm()
        except Exception as e:  # 数据库读取失败不影响扫描，只是少了"系统文件"判断
            self.error = str(e)
        if self.manager and not self.label:
            self.label = self.manager
        self.load_time = time.monotonic() - t
        return self

    def _load_pacman(self, root):
        self.manager = "pacman"
        files, backup = self.files, self.backup
        for entry in os.scandir(root):
            if not entry.is_dir():
                continue
            pkg = sys.intern(entry.name.rsplit("-", 2)[0])
            try:
                with open(entry.path + "/files", encoding="utf-8", errors="surrogateescape") as f:
                    data = f.read()
            except OSError:
                continue
            self.npkgs += 1
            section = None
            for line in data.split("\n"):
                if not line:
                    section = None
                elif line[0] == "%" and line[-1] == "%":
                    section = line
                elif section == "%FILES%":
                    if line[-1] != "/":
                        files["/" + line] = pkg
                elif section == "%BACKUP%":
                    backup.add("/" + line.split("\t", 1)[0])

    def _load_dpkg(self, root):
        self.manager = "dpkg"
        files, backup = self.files, self.backup
        for entry in os.scandir(root):
            name = entry.name
            if name.endswith(".list"):
                pkg = sys.intern(name[:-5].split(":", 1)[0])
                self.npkgs += 1
                try:
                    with open(entry.path, encoding="utf-8", errors="surrogateescape") as f:
                        for line in f:
                            p = line.rstrip("\n")
                            if p and p != "/.":
                                files.setdefault(p, pkg)
                except OSError:
                    pass
            elif name.endswith(".conffiles"):
                try:
                    with open(entry.path, encoding="utf-8", errors="surrogateescape") as f:
                        for line in f:
                            if line.strip():
                                backup.add(line.split()[0])
                except OSError:
                    pass

    def _load_rpm(self):
        self.manager = "rpm"
        out = subprocess.run(
            ["rpm", "-qa", "--qf", "[%{FILENAMES}\t%{NAME}\t%{FILEFLAGS:fflags}\n]"],
            capture_output=True, text=True, errors="surrogateescape", timeout=120,
        ).stdout
        pkgs = set()
        for line in out.split("\n"):
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            pkg = sys.intern(parts[1])
            pkgs.add(pkg)
            self.files.setdefault(parts[0], pkg)
            if len(parts) > 2 and "c" in parts[2]:
                self.backup.add(parts[0])
        self.npkgs = len(pkgs)

    def _load_windows(self):
        import winreg
        self.manager = "windows"
        self.label = "已安装的程序"
        sub = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
        sources = [
            (winreg.HKEY_LOCAL_MACHINE, winreg.KEY_WOW64_64KEY),
            (winreg.HKEY_LOCAL_MACHINE, winreg.KEY_WOW64_32KEY),
            (winreg.HKEY_CURRENT_USER, 0),
        ]
        names = set()
        env = os.environ
        sd = env.get("SystemDrive", "C:")
        too_broad = {os.path.normcase(p) for p in (
            env.get("ProgramFiles") or sd + "\\Program Files",
            env.get("ProgramFiles(x86)") or sd + "\\Program Files (x86)",
            env.get("ProgramData") or sd + "\\ProgramData",
            env.get("SystemRoot") or sd + "\\Windows",
            env.get("LOCALAPPDATA") or "", env.get("APPDATA") or "",
            sd + "\\Users", env.get("USERPROFILE") or "",
        ) if p}
        for hive, view in sources:
            try:
                root = winreg.OpenKey(hive, sub, 0, winreg.KEY_READ | view)
            except OSError:
                continue
            with root:
                for i in range(winreg.QueryInfoKey(root)[0]):
                    try:
                        with winreg.OpenKey(root, winreg.EnumKey(root, i), 0,
                                            winreg.KEY_READ | view) as k:
                            name = winreg.QueryValueEx(k, "DisplayName")[0]
                            loc = winreg.QueryValueEx(k, "InstallLocation")[0]
                    except OSError:
                        continue
                    if not name or not isinstance(loc, str):
                        continue
                    loc = loc.strip().strip('"').rstrip("\\/")
                    # 排除写成盘符根目录或 Program Files 本身的异常登记
                    if len(loc) < 4 or not os.path.isabs(loc) or os.path.normcase(loc) in too_broad:
                        continue
                    self.app_dirs.setdefault(loc, str(name))
                    names.add(name)
        self.npkgs = len(names)

    def _load_homebrew(self):
        for prefix in ("/opt/homebrew", "/usr/local"):
            for sub in ("Cellar", "Caskroom"):
                base = f"{prefix}/{sub}"
                try:
                    entries = list(os.scandir(base))
                except OSError:
                    continue
                self.manager = "brew"
                self.label = "Homebrew"
                for e in entries:
                    if e.is_dir(follow_symlinks=False):
                        self.app_dirs[e.path] = f"{e.name}（Homebrew）"
        self.npkgs = len(self.app_dirs)
