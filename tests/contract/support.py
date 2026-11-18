"""契约测试的进程、临时数据及系统元数据；不导入任何 DiskMap 实现。"""

from __future__ import annotations

import atexit
from contextlib import ExitStack
from dataclasses import dataclass
import http.client
import json
import os
from pathlib import Path
import re
import signal
import struct
import sys
import tempfile
import time
import urllib.parse


REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "crates/diskmap-core/tests"))
from compare_support.process import ProcessOwner  # noqa: E402


MISSING = object()
TOKEN = "contract-token-ASCII_123"
LANGUAGES = ("en", "zh")
CATEGORIES = ("core", "running", "system", "config", "user", "cache", "other")
PROTOCOLS = ("2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05")
OS_NAME = {"win32": "windows", "darwin": "macos"}.get(sys.platform, "linux")
LEGACY_PROC_BUDGET_SECONDS = 12.0
LEGACY_PROC_BUDGET_TOLERANCE_SECONDS = 0.1


def legacy_windows_proc_skip_reason(*, impl, windows, refresh_seconds, in_use):
    """旧版预算耗尽且未识别测试文件时，给出整项跳过的原因。"""
    threshold = LEGACY_PROC_BUDGET_SECONDS - LEGACY_PROC_BUDGET_TOLERANCE_SECONDS
    if impl != "legacy" or not windows or in_use or refresh_seconds < threshold:
        return None
    return (
        f"旧版 Windows 进程刷新耗时 {refresh_seconds:.3f} 秒，达到 12 秒预算（容差 0.1 秒），"
        "且未识别测试文件在用。legacy/backend/procinfo.py 中 ProcInfo.refresh(budget=12.0) 调用的 "
        "_collect_psutil（119–120 行）超预算后只收可执行文件，跳过映射和打开文件。 / "
        f"Legacy Windows process refresh took {refresh_seconds:.3f} s, reaching its 12 s budget "
        "(0.1 s tolerance), and the fixture was not detected in use. In legacy/backend/procinfo.py, "
        "ProcInfo.refresh(budget=12.0) calls _collect_psutil (lines 119–120), which collects only "
        "executables after the budget and skips mapped/open files."
    )


def _exit_on_signal(signum, _frame):
    # 将普通终止转成 Python 退出，保证 atexit 先结束服务/MCP，再删临时数据。
    raise SystemExit(128 + signum)


for _signal in (signal.SIGTERM, getattr(signal, "SIGHUP", signal.SIGTERM)):
    if signal.getsignal(_signal) == signal.SIG_DFL:
        signal.signal(_signal, _exit_on_signal)


def current_process_name():
    """从宿主的只读进程接口取得测试进程名字，而不是从被测服务取期望值。"""
    if sys.platform.startswith("linux"):
        return Path("/proc/self/comm").read_text(encoding="utf-8").strip()
    if sys.platform == "darwin":
        import ctypes
        library = ctypes.CDLL("/usr/lib/libproc.dylib")
        library.proc_name.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        library.proc_name.restype = ctypes.c_int
        buffer = ctypes.create_string_buffer(1024)
        if library.proc_name(os.getpid(), buffer, len(buffer)) <= 0:
            raise OSError("无法读取当前进程名 / Cannot read current process name")
        return buffer.value.decode("utf-8")
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetModuleFileNameW.argtypes = [wintypes.HMODULE, wintypes.LPWSTR, wintypes.DWORD]
    kernel.GetModuleFileNameW.restype = wintypes.DWORD
    buffer = ctypes.create_unicode_buffer(32768)
    length = kernel.GetModuleFileNameW(None, buffer, len(buffer))
    if not length or length >= len(buffer):
        raise ctypes.WinError(ctypes.get_last_error())
    return Path(buffer.value).name


def implementation():
    name = os.environ.get("DISKMAP_CONTRACT_IMPL", "rust")
    if name not in ("rust", "legacy"):
        raise ValueError("DISKMAP_CONTRACT_IMPL 必须是 rust 或 legacy / must be rust or legacy")
    return name


def command():
    if implementation() == "legacy":
        return [sys.executable, "-B", str(REPO / "legacy/diskmap.py")]
    override = os.environ.get("DISKMAP_CONTRACT_BIN")
    suffix = ".exe" if os.name == "nt" else ""
    paths = ([Path(override).expanduser()] if override else
             [REPO / f"target/{mode}/diskmap-cli{suffix}" for mode in ("debug", "release")])
    for path in paths:
        if path.is_file():
            return [str(path.resolve())]
    raise FileNotFoundError(
        "找不到 diskmap-cli；先运行 cargo build -p diskmap-cli，或设置 DISKMAP_CONTRACT_BIN。\n"
        "Cannot find diskmap-cli; run cargo build -p diskmap-cli or set DISKMAP_CONTRACT_BIN.\n"
        + "\n".join(map(str, paths)))


def url(route, **params):
    return route + ("?" + urllib.parse.urlencode({k: str(v) for k, v in params.items()}) if params else "")


def human(size):
    """契约规定的二进制单位和精度；只从字节数计算，不读取旧版函数。"""
    if size is None:
        return None
    if size < 1024:
        return f"{size} B"
    value = size
    for unit in ("KB", "MB", "GB", "TB", "PB"):
        value /= 1024
        if value < 1024 or unit == "PB":
            digits = 0 if value >= 100 else 1 if value >= 10 else 2
            return f"{value:.{digits}f} {unit}"
    raise AssertionError("大小超出单位范围 / Size outside unit range")


def write(path, size):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as stream:
        chunk = b"x" * min(size, 65536)
        remaining = size
        while remaining:
            count = min(remaining, len(chunk))
            stream.write(chunk[:count])
            remaining -= count


class Fixture:
    """cache 名字使三个系统的分类均确定；不同大小避免枚举次序平局。"""

    def __init__(self, base):
        self.root = base / "cache"
        self.sizes = {
            "docs/Report.pdf": 400_001,
            "proj/cache/blob.bin": 2_500_003,
            "proj/__pycache__/m.pyc": 1_200_005,
            "proj/Build.log": 1_100_007,
            "proj/main.py": 3_009,
            "中文 空格+&.txt": 41_011,
            "empty/zero": 0,
        }
        self.sizes.update({f"many/f{i:03}.bin": 1000 + i * 3 for i in range(160)})
        for relative, size in self.sizes.items():
            write(self.path(relative), size)
        self.dirs = {str(Path(relative).parent).replace(os.sep, "/") for relative in self.sizes}
        self.dirs.discard(".")
        self.dirs |= {"proj"}

    def path(self, relative=""):
        return self.root.joinpath(*relative.split("/")) if relative else self.root

    def files_under(self, relative=""):
        prefix = relative + "/" if relative else ""
        return {name: size for name, size in self.sizes.items() if name.startswith(prefix)}

    def size(self, relative=""):
        return self.sizes[relative] if relative in self.sizes else sum(self.files_under(relative).values())

    def counts(self, relative=""):
        prefix = relative + "/" if relative else ""
        return (len(self.files_under(relative)),
                sum(name.startswith(prefix) and name != relative for name in self.dirs))

    def children(self, relative=""):
        prefix = relative + "/" if relative else ""
        names = {name[len(prefix):].split("/", 1)[0] for name in self.files_under(relative)}
        return [(prefix + name, self.size(prefix + name),
                 "file" if prefix + name in self.sizes else "dir") for name in names]

    def retained_files(self, relative=""):
        # 每个目录各保留最大的 150 个文件；期望由完整 fixture 推出。
        files = self.files_under(relative)
        parents = {str(Path(name).parent) for name in files}
        kept = []
        for parent in parents:
            siblings = [(name, size) for name, size in files.items() if str(Path(name).parent) == parent]
            kept.extend(sorted(siblings, key=lambda row: -row[1])[:150])
        return sorted(kept, key=lambda row: -row[1])


class Workspace:
    def __init__(self):
        self.command = command()
        self.impl = implementation()
        self.stack = ExitStack()
        self.closed = False
        try:
            self.base = Path(self.stack.enter_context(tempfile.TemporaryDirectory(prefix="diskmap-contract-"))).resolve()
            self.fixture = Fixture(self.base / "fixture")
            self.env = self.environment("main")
            # 注册晚于 TemporaryDirectory 的 finalizer：中断时先回收进程，再删除目录。
            atexit.register(self.close)
        except BaseException:
            self.stack.close()
            raise

    def environment(self, label, language="en"):
        base = self.base / label
        for name in ("cache", "config", "tmp"):
            (base / name).mkdir(parents=True, exist_ok=True)
        env = dict(os.environ, DISKMAP_CACHE_DIR=str(base / "cache"),
                   DISKMAP_CONFIG_DIR=str(base / "config"), TMPDIR=str(base / "tmp"),
                   TMP=str(base / "tmp"), TEMP=str(base / "tmp"), DISKMAP_LANG=language,
                   DISKMAP_TOKEN=TOKEN, DISKMAP_PORT="0", PYTHONUTF8="1",
                   PYTHONIOENCODING="utf-8", PYTHONUNBUFFERED="1", PYTHONDONTWRITEBYTECODE="1")
        env.pop("DISKMAP_NO_ATTACH", None)
        return env

    def process(self, args, env=None):
        owner = ProcessOwner(self.command + args, cwd=REPO, env=env or self.env)
        self.stack.callback(owner.close)
        return owner

    def server(self, env=None, args=()):
        return Server(self, env or self.env, args)

    def mcp(self, *, attach, language="en", env=None):
        return Mcp(self, attach=attach, language=language, env=env)

    def close(self):
        if not self.closed:
            self.closed = True
            try:
                self.stack.close()
            finally:
                atexit.unregister(self.close)


@dataclass
class Response:
    status: int
    headers: dict
    raw: bytes

    @property
    def data(self):
        return json.loads(self.raw.decode("utf-8"))


class Server:
    def __init__(self, workspace, env, args=()):
        self.env = env
        self.owner = workspace.process((["serve"] if workspace.impl == "rust" else [])
                                       + ["--no-open"] + list(args), env)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            line = self.owner.readline(timeout=max(0.01, deadline - time.monotonic()))
            found = re.search(r"http://127\.0\.0\.1:[0-9]+/\?t=[^\s]+", line)
            if found:
                self.url = found.group()
                self.port = urllib.parse.urlsplit(self.url).port
                break
        else:
            raise TimeoutError("服务没有输出链接 / Server did not print its URL")
        self.wait_ready()

    def request(self, path, body=MISSING, *, method=None, headers=None, raw=None):
        headers = dict(headers or {})
        headers.setdefault("Host", f"127.0.0.1:{self.port}")
        headers.setdefault("X-DiskMap-Token", TOKEN)
        if body is not MISSING:
            raw = json.dumps(body, ensure_ascii=False).encode("utf-8")
            headers.setdefault("Content-Type", "application/json")
        method = method or ("POST" if body is not MISSING or raw is not None else "GET")
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=20)
        try:
            connection.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
            if raw is not None:
                headers.setdefault("Content-Length", str(len(raw)))
            for key, value in headers.items():
                if value is not None:
                    connection.putheader(key, value)
            connection.endheaders(raw)
            response = connection.getresponse()
            return Response(response.status, {k.lower(): v for k, v in response.getheaders()}, response.read())
        finally:
            connection.close()

    def get(self, route, *, language="en", **params):
        return self.json(url(route, **params), language=language)

    def json(self, path, body=MISSING, *, language="en"):
        response = self.request(path, body, headers={"X-DiskMap-Lang": language})
        if response.status != 200:
            raise AssertionError(f"HTTP {response.status}: {response.raw!r} ({path})")
        return response.data

    def wait_ready(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.json("/api/agent/status")["ready"]:
                return
            time.sleep(0.02)
        raise TimeoutError("初始化超时 / Initialization timed out")

    def wait_done(self, *, agent=False):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            result = self.json("/api/agent/status" if agent else "/api/status")
            scan = result["scan"] if agent else result
            if scan is not None and scan["state"] != "scanning":
                return result
            time.sleep(0.01)
        raise TimeoutError("扫描超时 / Scan timed out")

    def scan(self, path, *, apparent=True, cross_fs=False, agent=False, rescan=True):
        body = {"path": str(path), "apparent": apparent}
        body.update({"same_fs": not cross_fs, "rescan": rescan} if agent else {"cross_fs": cross_fs})
        initial = self.json("/api/agent/scan" if agent else "/api/scan", body)
        return initial, self.wait_done(agent=agent)

    def refresh_procs(self):
        """返回刷新状态和显式 POST 请求耗时，不计等待已有采集的时间。"""
        # start_scan 同时触发异步采集；先等这一轮结束，再显式刷新，才能检查固定持有的映射。
        deadline = time.monotonic() + 30
        while self.json("/api/status")["procs"]["refreshing"]:
            if time.monotonic() >= deadline:
                raise TimeoutError("进程采集超时 / Process collection timed out")
            time.sleep(0.02)
        started = time.monotonic()
        result = self.json("/api/refresh-procs", {})
        request_seconds = time.monotonic() - started
        while result["procs"]["refreshing"]:
            if time.monotonic() >= deadline:
                raise TimeoutError("进程刷新超时 / Process refresh timed out")
            time.sleep(0.02)
            result = self.json("/api/status")
        return result, request_seconds


class Mcp:
    def __init__(self, workspace, *, attach, language, env):
        self.owner = workspace.process(["mcp", "--lang", language] + ([] if attach else ["--no-attach"]), env)
        self.serial = 0

    def send(self, message):
        line = message if isinstance(message, str) else json.dumps(message, ensure_ascii=False)
        self.owner.process.stdin.write(line + "\n")
        self.owner.process.stdin.flush()

    def receive(self, timeout=30):
        return json.loads(self.owner.readline(timeout))

    def rpc(self, method, params=MISSING):
        self.serial += 1
        message = {"jsonrpc": "2.0", "id": self.serial, "method": method}
        if params is not MISSING:
            message["params"] = params
        self.send(message)
        response = self.receive()
        if response.get("jsonrpc") != "2.0" or response.get("id") != self.serial:
            raise AssertionError(f"MCP 请求/回复不匹配 / Mismatched response: {response}")
        return response

    def call(self, name, args=MISSING):
        params = {"name": name}
        if args is not MISSING:
            params["arguments"] = args
        return self.rpc("tools/call", params)["result"]

    def finish(self):
        try:
            self.owner.process.stdin.close()
            return self.owner.collect(timeout=15)
        finally:
            self.owner.close()


# FILE_FULL_DIR_INFORMATION 的固定字段，使用明确宽度，也可在 Linux 检查解析器。
DIRECTORY_RECORD = struct.Struct("<IIqqqqqqIII")


def directory_records(raw):
    records = []
    offset = 0
    while True:
        if len(raw) - offset < DIRECTORY_RECORD.size:
            raise ValueError("目录记录不完整 / Truncated directory record")
        fields = DIRECTORY_RECORD.unpack_from(raw, offset)
        following, logical, allocated, attrs, length = fields[0], fields[6], fields[7], fields[8], fields[9]
        end = offset + DIRECTORY_RECORD.size + length
        if length % 2 or end > len(raw) or (following and (following % 8 or offset + following < end
                                                         or offset + following >= len(raw))):
            raise ValueError("目录记录边界错误 / Invalid directory record boundary")
        name = raw[offset + DIRECTORY_RECORD.size:end].decode("utf-16-le", "surrogatepass")
        records.append((name, logical, allocated, attrs))
        if not following:
            return records
        offset += following


def allocated_sizes(root, impl):
    """Unix 用 st_blocks；Windows 分别计算旧版簇取整、新版目录分配元数据口径（T01/T14）。"""
    if os.name != "nt":
        return {path.name: path.stat().st_blocks * 512 for path in root.iterdir() if path.is_file()}
    import ctypes
    from ctypes import wintypes

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetDiskFreeSpaceW.argtypes = [wintypes.LPCWSTR] + [ctypes.POINTER(wintypes.DWORD)] * 4
    kernel.GetDiskFreeSpaceW.restype = wintypes.BOOL
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                                  wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.GetFileInformationByHandleEx.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    kernel.GetFileInformationByHandleEx.restype = wintypes.BOOL
    kernel.GetCompressedFileSizeW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.DWORD)]
    kernel.GetCompressedFileSizeW.restype = wintypes.DWORD
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL

    if impl == "legacy":
        sectors, sector_bytes, free, total = (wintypes.DWORD() for _ in range(4))
        # 旧版以盘符根目录查询簇大小；目录挂载卷也不能改成按文件所在卷计算。
        if not kernel.GetDiskFreeSpaceW(root.anchor, ctypes.byref(sectors), ctypes.byref(sector_bytes),
                                       ctypes.byref(free), ctypes.byref(total)):
            raise ctypes.WinError(ctypes.get_last_error())
        cluster = sectors.value * sector_bytes.value
        if cluster <= 0:
            raise AssertionError("卷簇大小必须为正 / Volume cluster size must be positive")
        return {p.name: (p.stat().st_size + cluster - 1) // cluster * cluster
                for p in root.iterdir() if p.is_file()}

    # FILE_LIST_DIRECTORY；共享读/写/删除；OPEN_EXISTING；FILE_FLAG_BACKUP_SEMANTICS。
    handle = kernel.CreateFileW(str(root), 1, 7, None, 3, 0x02000000, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    sizes = {}
    try:
        buffer = ctypes.create_string_buffer(65536)
        while kernel.GetFileInformationByHandleEx(handle, 14, buffer, len(buffer)):
            for name, logical, allocated, attrs in directory_records(buffer.raw):
                if name in (".", "..") or attrs & 0x10:
                    continue
                if attrs & (0x400000 | 0x40000 | 0x1000):
                    raise AssertionError("刚写入的测试文件必须在本地 / Fresh fixture must be local")
                if allocated < 0 or logical < 0:
                    raise AssertionError("文件大小不能为负 / File sizes must be nonnegative")
                if allocated == 0 and logical:
                    high = wintypes.DWORD()
                    ctypes.set_last_error(0)
                    low = kernel.GetCompressedFileSizeW(str(root / name), ctypes.byref(high))
                    error = ctypes.get_last_error()
                    if low == 0xFFFFFFFF and error:
                        raise ctypes.WinError(error)
                    allocated = (high.value << 32) | low
                sizes[name] = allocated
        error = ctypes.get_last_error()
        if error != 18:  # ERROR_NO_MORE_FILES；不能静默回退成估算值。
            raise ctypes.WinError(error)
    finally:
        kernel.CloseHandle(handle)
    expected = {path.name for path in root.iterdir() if path.is_file()}
    if sizes.keys() != expected:
        raise AssertionError(f"目录元数据没有覆盖全部测试文件 / Missing file metadata: {sizes.keys()} != {expected}")
    return sizes
