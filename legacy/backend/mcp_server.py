"""MCP（Model Context Protocol）服务：让 Claude 等 AI 助手直接调用 DiskMap 的分析结果。

用法（由 AI 客户端启动，通过标准输入/输出通信）：
    DiskMap mcp              # 打包版
    python diskmap.py mcp    # 源码版

工作方式：
- 如果用户已经打开了 DiskMap 窗口，就连接到那个窗口，AI 看到的就是用户正在看的扫描结果，
  还能知道用户选中了什么，并能让窗口跳转高亮某个项目；
- 否则在本进程内启动一个不带界面的 DiskMap，按需扫描。
所有工具都是只读的，不会删除或修改任何文件。

只用标准库实现：换行分隔的 JSON-RPC 2.0（MCP stdio 传输）。
"""
import json
import os
import secrets
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from . import __version__, i18n, instance
from .i18n import L, T
from .osinfo import system_root

PROTOCOLS = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]

CATEGORIES = ["core", "running", "system", "config", "user", "cache", "other"]

INSTRUCTIONS = T("""DiskMap 分析磁盘空间占用，并按重要程度给每个文件/文件夹分类：
core 系统核心（不能动）、running 正被程序使用、system 系统/已安装程序（应卸载而非删除）、
config 配置、user 用户个人文件（删不删由用户决定）、cache 缓存/临时（通常可安全清理）、other 应用数据。
每个结果都附带 safety 字段（danger / in_use / uninstall_only / caution / ask_user / safe_to_clean）和中文建议。

建议流程：diskmap_status → （没有结果时）diskmap_scan → diskmap_summary → 用 diskmap_list 逐层深入，
或用 diskmap_cleanup 找可清理的空间。用户提到“这个”“我选中的”时用 diskmap_user_view。
需要给用户指出某个项目时用 diskmap_show（会在用户的 DiskMap 窗口中高亮）。
这些工具不会删除文件；建议删除前务必征得用户同意，并说明 safety 等级。""",
"""DiskMap measures disk usage and classifies every file and folder by importance:
core = system core (never touch), running = in use by a program, system = OS or installed software
(uninstall instead of deleting), config = settings, user = personal files (the user decides),
cache = cache/temp (usually safe to clean), other = application data.
Every result has a safety field (danger / in_use / uninstall_only / caution / ask_user / safe_to_clean)
and a short advice text.

Suggested flow: diskmap_status → (if there is no scan) diskmap_scan → diskmap_summary → drill down with
diskmap_list, or use diskmap_cleanup to find reclaimable space. When the user says "this" or "the selected
one", use diskmap_user_view. To point something out to the user, use diskmap_show (it highlights the item
in the user's DiskMap window). These tools never delete files; before suggesting any deletion, get the
user's consent and explain the safety level.""")

_PATH = {"type": "string", "description": T("文件或文件夹的绝对路径", "Absolute path of a file or folder")}
_OPT_PATH = {"type": "string", "description": T("文件夹绝对路径，省略时为扫描的根目录",
                                                "Absolute folder path; defaults to the scan root")}


def _tool(name, title, desc, props=None, required=(), read_only=True):
    return {
        "name": name, "title": title, "description": desc,
        "inputSchema": {"type": "object", "properties": props or {},
                        "required": list(required), "additionalProperties": False},
        "annotations": {"title": title, "readOnlyHint": read_only, "destructiveHint": False,
                        "idempotentHint": True, "openWorldHint": False},
    }


def _limit(zh, en, default):
    return {"type": "integer", "description": T(zh, en), "default": default}


TOOLS = [
    _tool("diskmap_status", T("DiskMap 状态", "DiskMap status"),
          T("查看当前是否已有扫描结果、扫描范围与进度、是否连接到了用户打开的 DiskMap 窗口，"
            "以及用户在窗口中正在看的位置。建议第一个调用。",
            "Whether a scan exists, its scope and progress, whether the user's DiskMap window is "
            "connected, and where the user is looking in it. Call this first.")),
    _tool("diskmap_scan", T("扫描磁盘", "Scan disk"),
          T("扫描一个文件夹或整个磁盘（Linux/macOS 用 /，Windows 如 C:\\）。"
            "如果已有覆盖该路径的扫描结果会直接复用（除非 rescan=true）。"
            "连接到用户的 DiskMap 窗口时，新扫描会显示在窗口中并替换当前结果。"
            "完成后返回该路径的概览；整盘扫描通常需要几秒到一两分钟。",
            "Scan a folder or a whole drive (/ on Linux/macOS, e.g. C:\\ on Windows). An existing scan "
            "that covers the path is reused unless rescan=true. When connected to the user's DiskMap "
            "window, a new scan appears there and replaces the current one. Returns an overview of the "
            "path when done; a whole drive usually takes a few seconds to a couple of minutes."),
          {"path": {"type": "string", "description": T("要扫描的文件夹，省略时为系统盘",
                                                       "Folder to scan; defaults to the system drive")},
           "rescan": {"type": "boolean", "description": T("忽略已有结果重新扫描", "Scan again even if results exist"),
                      "default": False},
           "wait_seconds": _limit("最多等待扫描完成的秒数（1-600）",
                                  "Maximum seconds to wait for the scan to finish (1-600)", 120),
           "apparent": {"type": "boolean", "description": T("按文件长度而不是实际占用空间统计",
                                                            "Count file length instead of space on disk"),
                        "default": False},
           "same_fs": {"type": "boolean", "description": T("不进入其他分区（类似 du -x）",
                                                           "Stay on one file system (like du -x)"),
                       "default": False}},
          read_only=False),
    _tool("diskmap_summary", T("占用概览", "Usage overview"),
          T("某个文件夹的占用概览：按重要程度分类的空间分布、最大的子项、最大的文件、"
            "可清理空间与候选、正被程序使用的大文件。",
            "Overview of a folder: space by importance category, largest children, largest files, "
            "reclaimable space and candidates, large files in use."),
          {"path": _OPT_PATH, "top": _limit("每个列表返回几项（1-50）", "Items per list (1-50)", 10)}),
    _tool("diskmap_list", T("列出子项", "List folder"),
          T("列出文件夹的直接子项（按大小从大到小），每项带类别、原因、占比、是否正在使用和安全建议。"
            "用于一层层往下找空间被谁占了。",
            "List a folder's direct children, largest first, each with category, reason, share, "
            "in-use state and safety advice. Use it to drill down level by level."),
          {"path": _PATH, "limit": _limit("最多返回几项（1-500）", "Maximum items (1-500)", 40)},
          required=["path"]),
    _tool("diskmap_explain", T("解释项目", "Explain item"),
          T("解释一个文件或文件夹：大小、重要程度类别和原因、所属软件包、正在使用它的进程、"
            "修改时间、能不能删的建议。",
            "Explain a file or folder: size, importance category and reason, owning package, processes "
            "using it, modification time, and whether it can be deleted."),
          {"path": _PATH}, required=["path"]),
    _tool("diskmap_cleanup", T("可清理空间", "Reclaimable space"),
          T("找出可以清理的缓存、临时文件、日志、回收站、可重新下载的依赖包等，按大小排序，"
            "并给出可回收的总空间。只提供建议，不会删除任何东西。",
            "Find caches, temporary files, logs, trash, re-downloadable packages and the like, largest "
            "first, with the total reclaimable space. Suggestions only; nothing is deleted."),
          {"path": _OPT_PATH,
           "limit": _limit("最多返回几项（1-200）", "Maximum items (1-200)", 30),
           "min_mb": {"type": "number", "description": T("只列出不小于这个大小（MB）的项目",
                                                         "Only list items of at least this size (MB)"),
                      "default": 1}}),
    _tool("diskmap_largest_files", T("最大的文件", "Largest files"),
          T("找出某个文件夹下（含所有子文件夹）最大的文件，可按类别筛选。",
            "Largest files under a folder (including all subfolders), optionally of one category."),
          {"path": _OPT_PATH,
           "limit": _limit("返回几项（1-200）", "Number of items (1-200)", 20),
           "category": {"type": "string", "enum": CATEGORIES, "description": T("只看某一类", "Only this category")}}),
    _tool("diskmap_user_view", T("用户正在看什么", "What the user is viewing"),
          T("获取用户在 DiskMap 窗口里正在查看的文件夹和选中的项目（附带概览和详情）。"
            "用户说“这个”“我选中的”“现在看的”时使用。需要用户打开了 DiskMap 窗口。",
            "The folder the user is viewing and the item selected in the DiskMap window, with an overview "
            "and details. Use when the user says \"this\", \"the selected one\" or \"what I'm looking at\". "
            "Requires the DiskMap window to be open.")),
    _tool("diskmap_show", T("在窗口中指给用户看", "Show in window"),
          T("让用户的 DiskMap 窗口跳转到并高亮某个文件或文件夹，可附一句说明（会显示在窗口里）。"
            "用来给用户指出你正在说的东西。",
            "Make the user's DiskMap window jump to and highlight a file or folder, with an optional short "
            "note shown in the window. Use it to point out what you are talking about."),
          {"path": _PATH, "note": {"type": "string", "description": T("显示给用户的简短说明",
                                                                      "Short note shown to the user")}},
          required=["path"], read_only=False),
]
TOOL_NAMES = {t["name"] for t in TOOLS}


class ToolError(Exception):
    pass


class Backend:
    """把工具调用转发到 DiskMap 的本地接口：优先用户打开的窗口，否则本进程内启动一个。"""

    def __init__(self, attach=True):
        self.attach = attach
        self.local = None
        self.lock = threading.Lock()
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))  # 不走代理

    def _request(self, ep, method, path, params=None, body=None, timeout=30):
        base, token = ep
        url = base + path
        if params:
            url += "?" + urllib.parse.urlencode({k: v for k, v in params.items() if v is not None})
        data = None
        headers = {"X-DiskMap-Token": token, "X-DiskMap-Lang": i18n.current()}
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with self.opener.open(req, timeout=timeout) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            try:
                msg = json.loads(e.read()).get("error")
            except ValueError:
                msg = None
            raise ToolError(msg or L(f"DiskMap 接口出错（HTTP {e.code}）", f"DiskMap API error (HTTP {e.code})")) from None

    def _gui(self):
        if not self.attach:
            return None
        info = instance.read()
        if not info:
            return None
        ep = (f"http://127.0.0.1:{info['port']}", info["token"])
        try:
            st = self._request(ep, "GET", "/api/agent/status", timeout=2)
        except (ToolError, OSError, ValueError):
            return None
        return ep, st

    def _local(self):
        with self.lock:
            if self.local is None:
                from .server import serve
                token = secrets.token_urlsafe(18)
                httpd, url, app = serve(0, token)
                self.local = ((url.split("/?")[0], token), app)
            return self.local[0]

    def endpoint(self):
        """返回 (接口, 来源)。用户窗口有扫描结果、或本进程还没扫描过时，优先用窗口。"""
        gui = self._gui()
        local_has_scan = self.local is not None and self.local[1].scan is not None
        if gui and (gui[1].get("scan") or not local_has_scan):
            return gui[0], "window"
        return self._local(), "background"

    def get(self, api, **params):
        ep, source = self.endpoint()
        return self._request(ep, "GET", api, params), source, ep

    def post(self, api, body, ep=None):
        if ep is None:
            ep, _ = self.endpoint()
        return self._request(ep, "POST", api, body=body)


# ------------------------------------------------------------------ 工具实现
def call_tool(be, name, args):
    args = args or {}
    if name == "diskmap_status":
        res, source, _ = be.get("/api/agent/status")
        res["connected_to"] = source
        res["connected_to_label"] = (
            L("用户打开的 DiskMap 窗口", "the DiskMap window the user has open") if source == "window"
            else L("后台扫描（用户没有打开 DiskMap 窗口，或窗口里还没有扫描）",
                   "background scan (the DiskMap window is not open, or has no scan yet)"))
        return res
    if name == "diskmap_scan":
        path = args.get("path") or system_root()
        ep, source = be.endpoint()
        st = be.post("/api/agent/scan", {"path": path, "rescan": bool(args.get("rescan")),
                                         "apparent": bool(args.get("apparent")),
                                         "same_fs": bool(args.get("same_fs"))}, ep=ep)
        reused = st.get("reused")
        wait = max(1, min(int(args.get("wait_seconds") or 120), 600))
        deadline = time.time() + wait
        state = None
        while time.time() < deadline:
            state = be._request(ep, "GET", "/api/agent/status")
            if not state.get("scan") or state["scan"]["state"] != "scanning":
                break
            time.sleep(0.5)
        scan = (state or {}).get("scan") or {}
        res = {"connected_to": source, "reused_existing_scan": bool(reused), "scan": scan}
        if scan.get("state") == "scanning":
            res["note"] = L(f"{wait} 秒内还没扫完（已统计 {scan.get('files')} 个文件）。"
                            "可以稍后再调用 diskmap_status；扫描进行中也能用 diskmap_summary 查看部分结果。",
                            f"Not finished within {wait} s ({scan.get('files')} files counted so far). "
                            "Call diskmap_status later; diskmap_summary already shows partial results.")
        else:
            res["summary"] = be._request(ep, "GET", "/api/agent/summary", {"path": path, "top": 8})
        return res
    if name == "diskmap_summary":
        return be.get("/api/agent/summary", path=args.get("path"), top=args.get("top", 10))[0]
    if name == "diskmap_list":
        return be.get("/api/agent/list", path=args.get("path"), limit=args.get("limit", 40))[0]
    if name == "diskmap_explain":
        return be.get("/api/agent/explain", path=args.get("path"))[0]
    if name == "diskmap_cleanup":
        return be.get("/api/agent/cleanup", path=args.get("path"), limit=args.get("limit", 30),
                      min_mb=args.get("min_mb", 1))[0]
    if name == "diskmap_largest_files":
        return be.get("/api/agent/largest", path=args.get("path"), limit=args.get("limit", 20),
                      category=args.get("category"))[0]
    if name == "diskmap_user_view":
        gui = be._gui()
        if not gui:
            raise ToolError(L("用户没有打开 DiskMap 窗口，无法知道用户正在看什么。可以用 diskmap_summary 等工具自行分析。",
                              "The DiskMap window is not open, so what the user is looking at is unknown. "
                              "Use diskmap_summary and the other tools to analyze on your own."))
        return be._request(gui[0], "GET", "/api/agent/view")
    if name == "diskmap_show":
        gui = be._gui()
        if not gui:
            raise ToolError(L("用户没有打开 DiskMap 窗口，无法在窗口中显示。请直接把路径告诉用户。",
                              "The DiskMap window is not open, so nothing can be shown there. "
                              "Tell the user the path instead."))
        return be._request(gui[0], "POST", "/api/agent/show",
                           body={"path": args.get("path"), "note": args.get("note")})
    raise ToolError(L(f"未知工具：{name}", f"Unknown tool: {name}"))


# ------------------------------------------------------------------ JSON-RPC / stdio
class Server:
    def __init__(self, backend, out=None):
        self.be = backend
        self.out = out or sys.stdout.buffer
        self.wlock = threading.Lock()
        self.protocol = PROTOCOLS[0]

    def send(self, msg):
        data = json.dumps(i18n.loc(msg), ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
        with self.wlock:
            self.out.write(data)
            self.out.flush()

    def reply(self, mid, result=None, error=None):
        msg = {"jsonrpc": "2.0", "id": mid}
        if error is not None:
            msg["error"] = error
        else:
            msg["result"] = result
        self.send(msg)

    def handle(self, msg):
        if not isinstance(msg, dict) or msg.get("jsonrpc") != "2.0":
            if isinstance(msg, dict) and "id" in msg:
                self.reply(msg["id"], error={"code": -32600, "message": "Invalid Request"})
            return
        method = msg.get("method")
        mid = msg.get("id")
        is_request = "id" in msg and method is not None
        if method is None:
            return   # 客户端发来的响应（本服务不发请求），忽略
        if not is_request:
            return   # 通知：initialized / cancelled 等，无需处理
        params = msg.get("params") or {}
        try:
            if method == "initialize":
                want = params.get("protocolVersion")
                self.protocol = want if want in PROTOCOLS else PROTOCOLS[0]
                return self.reply(mid, {
                    "protocolVersion": self.protocol,
                    "capabilities": {"tools": {"listChanged": False}},
                    "serverInfo": {"name": "diskmap", "title": L("DiskMap 磁盘地图", "DiskMap"),
                                   "version": __version__},
                    "instructions": INSTRUCTIONS,
                })
            if method == "ping":
                return self.reply(mid, {})
            if method == "tools/list":
                return self.reply(mid, {"tools": TOOLS})
            if method == "tools/call":
                name = params.get("name")
                if name not in TOOL_NAMES:
                    return self.reply(mid, error={"code": -32602, "message": L(f"未知工具：{name}", f"Unknown tool: {name}")})
                try:
                    data = call_tool(self.be, name, params.get("arguments"))
                except (ToolError, OSError, ValueError) as e:
                    return self.reply(mid, {"content": [{"type": "text", "text": str(e)}], "isError": True})
                except Exception as e:   # 意外错误也作为工具错误返回，让 AI 能看到原因
                    return self.reply(mid, {"content": [{"type": "text", "text": L(f"DiskMap 内部错误：{e!r}",
                                                                                 f"DiskMap internal error: {e!r}")}],
                                            "isError": True})
                text = json.dumps(data, ensure_ascii=False, indent=1)
                return self.reply(mid, {"content": [{"type": "text", "text": text}],
                                        "structuredContent": data, "isError": False})
            if method in ("resources/list", "prompts/list", "resources/templates/list"):
                key = {"resources/list": "resources", "prompts/list": "prompts",
                       "resources/templates/list": "resourceTemplates"}[method]
                return self.reply(mid, {key: []})
            return self.reply(mid, error={"code": -32601, "message": f"Method not found: {method}"})
        except Exception as e:  # pragma: no cover - 防御
            self.reply(mid, error={"code": -32603, "message": repr(e)})

    def serve(self, inp=None):
        inp = inp or sys.stdin.buffer
        workers = []
        for raw in inp:
            raw = raw.strip()
            if not raw:
                continue
            try:
                msg = json.loads(raw.decode("utf-8"))
            except (ValueError, UnicodeDecodeError):
                self.send({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "Parse error"}})
                continue
            batch = msg if isinstance(msg, list) else [msg]
            for m in batch:
                # 工具调用（尤其是扫描）可能较慢，放到线程里，期间仍能响应 ping
                if isinstance(m, dict) and m.get("method") == "tools/call":
                    t = threading.Thread(target=self.handle, args=(m,), daemon=True)
                    t.start()
                    workers.append(t)
                else:
                    self.handle(m)
            workers = [t for t in workers if t.is_alive()]
        for t in workers:   # 输入结束：等进行中的调用回复完（最多 10 秒）
            t.join(timeout=10)


def run(attach=True, lang=None):
    if sys.stdin is None or sys.stdout is None:
        return 2
    i18n.set_default(i18n.resolve(lang))
    out = sys.stdout.buffer
    sys.stdout = sys.stderr   # 标准输出专用于协议消息，任何意外的 print 都改到标准错误
    Server(Backend(attach=attach and not os.environ.get("DISKMAP_NO_ATTACH")), out=out).serve()
    return 0
