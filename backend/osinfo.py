"""平台差异集中在这里：Windows / macOS / Linux。"""
import contextlib
import os
import sys

IS_WIN = os.name == "nt"
IS_MAC = sys.platform == "darwin"
IS_LINUX = sys.platform.startswith("linux")
OS_NAME = "windows" if IS_WIN else "macos" if IS_MAC else "linux"
SEP = os.sep
FROZEN = bool(getattr(sys, "frozen", False))

# Windows 文件名不区分大小写：比较/查找时统一用小写
if IS_WIN:
    pkey = os.path.normcase
else:
    def pkey(p):
        return p


def norm_path(p):
    p = os.path.expanduser(str(p).strip())
    if IS_WIN and len(p) == 2 and p[1] == ":":
        p += "\\"   # "C:" 在 Windows 上表示"C 盘的当前目录"，这里显然是想要盘符根目录
    return os.path.abspath(p)


def join_path(d, name):
    return d + name if d.endswith(SEP) else d + SEP + name


def is_root(p):
    return os.path.dirname(p) == p


def system_root():
    if IS_WIN:
        return os.environ.get("SystemDrive", "C:") + "\\"
    return "/"


def is_admin():
    if IS_WIN:
        try:
            import ctypes
            return bool(ctypes.windll.shell32.IsUserAnAdmin())
        except Exception:
            return False
    return os.geteuid() == 0


def resource_dir():
    """打包后资源在 sys._MEIPASS；源码运行时是项目根目录。"""
    base = getattr(sys, "_MEIPASS", None)
    if base:
        return base
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def user_cache_dir():
    if os.environ.get("DISKMAP_CACHE_DIR"):   # 测试或便携使用时可指定
        return os.environ["DISKMAP_CACHE_DIR"]
    if IS_WIN:
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~\\AppData\\Local")
        return os.path.join(base, "DiskMap")
    if IS_MAC:
        return os.path.expanduser("~/Library/Caches/DiskMap")
    return os.path.join(os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache"), "diskmap")


def cluster_size(path):
    """Windows 上文件按簇分配空间，用于估算"实际占用"。"""
    if not IS_WIN:
        return 0
    try:
        import ctypes
        spc, bps, fc, tc = (ctypes.c_ulong() for _ in range(4))
        drive = os.path.splitdrive(path)[0] + "\\"
        if ctypes.windll.kernel32.GetDiskFreeSpaceW(
                ctypes.c_wchar_p(drive), ctypes.byref(spc), ctypes.byref(bps),
                ctypes.byref(fc), ctypes.byref(tc)):
            return spc.value * bps.value or 4096
    except Exception:
        pass
    return 4096


def clean_env():
    """PyInstaller 打包的 Linux 程序会改写 LD_LIBRARY_PATH，
    启动系统程序（浏览器、文件管理器、系统 Python）前要还原，否则它们可能加载错误的库。"""
    env = dict(os.environ)
    if FROZEN and not IS_WIN:
        for var in ("LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"):
            orig = env.pop(var + "_ORIG", None)
            if orig is not None:
                env[var] = orig
            else:
                env.pop(var, None)
        for var in ("PYTHONHOME", "PYTHONPATH", "TCL_LIBRARY", "TK_LIBRARY", "SSL_CERT_FILE"):
            env.pop(var, None)
    return env


@contextlib.contextmanager
def clean_environ():
    """临时把 os.environ 换成 clean_env()（给 webbrowser 这类不接受 env 参数的模块用）。"""
    saved = dict(os.environ)
    try:
        os.environ.clear()
        os.environ.update(clean_env())
        yield
    finally:
        os.environ.clear()
        os.environ.update(saved)


def swapfiles():
    """系统正在使用的交换/页面文件。"""
    out = []
    if IS_LINUX:
        try:
            with open("/proc/swaps") as f:
                next(f, None)
                for line in f:
                    parts = line.split()
                    if len(parts) >= 2 and parts[1] == "file":
                        out.append(parts[0].replace("\\040", " "))
        except OSError:
            pass
    elif IS_WIN:
        sd = os.environ.get("SystemDrive", "C:")
        for name in ("pagefile.sys", "swapfile.sys"):
            p = f"{sd}\\{name}"
            try:
                if os.path.exists(p):
                    out.append(p)
            except OSError:
                pass
    elif IS_MAC:
        try:
            for name in os.listdir("/private/var/vm"):
                if name.startswith("swapfile"):
                    out.append("/private/var/vm/" + name)
        except OSError:
            pass
    return out
