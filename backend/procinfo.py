"""收集"正在被运行中程序使用的文件"。

- exe：进程的可执行文件
- lib：被映射进内存的文件（动态库 .so / .dll、字体、数据库等）
- open：进程打开着的文件
- swap：系统正在使用的交换/页面文件

Linux 直接读 /proc（最快，不依赖第三方库）；Windows / macOS 用 psutil。
普通用户只能看到自己进程的详细信息；以 root / 管理员运行可以看到更多。
"""
import os
import shutil
import threading
import time

from .osinfo import IS_LINUX, IS_WIN, pkey, swapfiles

KIND_LABEL = {"exe": "正在执行", "lib": "已加载到内存", "open": "已打开", "swap": "交换文件"}
_KIND_RANK = {"exe": 0, "swap": 0, "lib": 1, "open": 2}
_SKIP_PREFIX = ("/proc/", "/sys/", "/dev/", "/memfd:", "/run/", "/SYSV")


def _usable(path):
    if not path:
        return False
    if IS_WIN:
        return len(path) > 3 and path[1] == ":" and path[2] in "\\/"
    return path[0] == "/" and not path.startswith(_SKIP_PREFIX)


def _adder(raw):
    def add(path, pid, comm, kind):
        if not _usable(path):
            return
        if path.endswith(" (deleted)"):
            path = path[:-10]
        slot = raw.get(path)
        if slot is None:
            raw[path] = {pid: (comm, kind)}
        else:
            old = slot.get(pid)
            if old is None or _KIND_RANK[kind] < _KIND_RANK[old[1]]:
                slot[pid] = (comm, kind)
    return add


def _collect_proc():
    """Linux：直接读取 /proc。"""
    raw = {}
    add = _adder(raw)
    nprocs = visible = 0
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        pid = int(name)
        base = "/proc/" + name
        try:
            with open(base + "/comm") as f:
                comm = f.read().strip()
        except OSError:
            continue
        nprocs += 1
        exe = None
        try:
            exe = os.readlink(base + "/exe")
        except OSError:
            try:
                with open(base + "/cmdline", "rb") as f:
                    argv0 = f.read().split(b"\0", 1)[0].decode(errors="surrogateescape")
                if argv0:
                    exe = argv0 if argv0.startswith("/") else shutil.which(argv0)
            except OSError:
                pass
        if exe:
            add(exe, pid, comm, "exe")
        try:
            with open(base + "/maps", errors="surrogateescape") as f:
                for line in f:
                    parts = line.split(None, 5)
                    if len(parts) == 6:
                        add(parts[5].rstrip("\n"), pid, comm, "lib")
            visible += 1
        except OSError:
            pass
        try:
            fds = os.listdir(base + "/fd")
        except OSError:
            fds = ()
        for fd in fds:
            try:
                add(os.readlink(base + "/fd/" + fd), pid, comm, "open")
            except OSError:
                pass
    return raw, nprocs, visible


def _collect_psutil(budget):
    """Windows / macOS（以及没有 /proc 的系统）：用 psutil。"""
    import psutil
    raw = {}
    add = _adder(raw)
    nprocs = visible = 0
    deadline = time.monotonic() + budget
    has_maps = hasattr(psutil.Process, "memory_maps")   # macOS 上没有
    for p in psutil.process_iter(["pid", "name"]):
        pid = p.info["pid"]
        if pid == 0:   # Windows 的"系统空闲进程"
            continue
        comm = p.info["name"] or str(pid)
        nprocs += 1
        try:
            exe = p.exe()
        except (psutil.Error, OSError):
            exe = None
        if exe:
            add(exe, pid, comm, "exe")
        if time.monotonic() > deadline:   # 进程很多时只收集可执行文件，避免等太久
            continue
        ok = False
        if has_maps:
            try:
                for m in p.memory_maps(grouped=True):
                    add(m.path, pid, comm, "lib")
                ok = True
            except (psutil.Error, OSError, ValueError):
                pass
        try:
            for f in p.open_files():
                add(f.path, pid, comm, "open")
            ok = True
        except (psutil.Error, OSError, ValueError):
            pass
        visible += ok
    return raw, nprocs, visible


class ProcInfo:
    def __init__(self):
        self.files = {}     # pkey(path) -> {pid: (comm, kind)}
        self.dirs = {}      # pkey(dir) -> 其下正在使用的文件数
        self.nprocs = 0
        self.visible = 0    # 能读取详细信息的进程数
        self.time = 0.0
        self.stamp = 0
        self.error = None
        self.refreshing = False
        self._lock = threading.Lock()

    def refresh(self, budget=12.0):
        with self._lock:
            self.refreshing = True
            t = time.monotonic()
            try:
                if IS_LINUX and os.path.isdir("/proc/self/fd"):
                    raw, n, v = _collect_proc()
                else:
                    raw, n, v = _collect_psutil(budget)
                add = _adder(raw)
                for s in swapfiles():
                    add(s, 0, "系统", "swap")
                self.install(raw)
                self.nprocs, self.visible = n, v
                self.error = None
            except Exception as e:   # 例如缺少 psutil：只是没有"正在使用"标注
                self.error = repr(e)
            finally:
                self.time = time.monotonic() - t
                self.refreshing = False
        return self

    def refresh_async(self):
        if not self.refreshing:
            threading.Thread(target=self.refresh, name="diskmap-procs", daemon=True).start()

    def install(self, raw):
        """登记 {路径: {pid: (程序名, 类型)}}，并统计每个上级文件夹。"""
        files = {}
        for path, slot in raw.items():
            k = pkey(path)
            if k in files:
                files[k].update(slot)
            else:
                files[k] = dict(slot)
        dirs = {}
        for path in files:
            d = os.path.dirname(path)
            while True:
                dirs[d] = dirs.get(d, 0) + 1
                parent = os.path.dirname(d)
                if parent == d:
                    break
                d = parent
        self.files, self.dirs = files, dirs
        self.stamp += 1

    def lookup(self, path):
        return self.files.get(pkey(path))

    def dir_count(self, path):
        return self.dirs.get(pkey(path), 0)

    def summary(self, path, limit=4):
        """'chrome ×12、niri 等 7 个程序' 这样的简短描述。"""
        slot = self.lookup(path)
        if not slot:
            return None
        counts = {}
        for comm, _kind in slot.values():
            counts[comm] = counts.get(comm, 0) + 1
        items = sorted(counts.items(), key=lambda kv: -kv[1])
        text = "、".join(f"{c} ×{n}" if n > 1 else c for c, n in items[:limit])
        if len(items) > limit:
            text += f" 等 {len(items)} 个程序"
        return text

    def details(self, path, limit=40):
        slot = self.lookup(path) or {}
        rows = sorted(slot.items(), key=lambda kv: (_KIND_RANK[kv[1][1]], kv[1][0], kv[0]))
        return [{"pid": pid, "comm": comm, "kind": KIND_LABEL[kind]}
                for pid, (comm, kind) in rows[:limit]], len(rows)


def read_swapfiles():   # 兼容旧接口
    return swapfiles()
