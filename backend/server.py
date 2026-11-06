"""本地 HTTP 服务：给界面提供扫描、树数据、详情等接口。

只监听 127.0.0.1；页面地址带随机令牌，接口要求请求头携带同一令牌，
并校验 Host，防止其他网页或本机其他用户调用。
"""
import hmac
import json
import os
import secrets
import shlex
import stat
import subprocess
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from . import __version__
from . import analysis
from .classify import CLASSES, RUNNING, Classifier
from .mounts import disk_mounts, read_mounts
from .osinfo import (FROZEN, IS_MAC, IS_WIN, OS_NAME, SEP, clean_env, is_admin, join_path,
                     norm_path, pkey, resource_dir, swapfiles, system_root)
from .pkgdb import PkgDB
from .procinfo import ProcInfo
from .scanner import MOUNT, Scan

try:
    import grp
    import pwd
except ImportError:  # Windows
    grp = pwd = None

WEB_DIR = os.path.join(resource_dir(), "web")
CONTENT_TYPES = {".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8",
                 ".css": "text/css; charset=utf-8", ".svg": "image/svg+xml", ".png": "image/png"}


class App:
    def __init__(self):
        self.lock = threading.Lock()
        self.pkgdb = PkgDB()
        self.proc = ProcInfo()
        self.classifier = None
        self.ready = threading.Event()
        self.scan = None
        self.admin = is_admin()
        self.last_seen = 0.0   # 最近一次收到界面请求的时间（浏览器模式下用来判断窗口是否已关闭）
        self.bye_at = 0.0      # 页面关闭时发来的告别时间
        self.version = __version__
        self.os_name = OS_NAME
        self.ui_state = None   # 界面上报的"用户正在看什么"
        self.command = None    # AI 让界面执行的动作（如跳转高亮）
        self.agent_seen = 0.0  # 最近一次 AI 接口被调用的时间
        threading.Thread(target=self._init, name="diskmap-init", daemon=True).start()

    def _init(self):
        self.pkgdb.load()
        self.classifier = Classifier(self.pkgdb, swapfiles())
        self.ready.set()
        self.proc.refresh()

    # ---------------------------------------------------------------- 操作
    def start_scan(self, path, apparent=False, cross_fs=True):
        path = norm_path(path or system_root())
        if not os.path.isdir(path):
            raise ValueError(f"文件夹不存在：{path}")
        self.ready.wait()
        with self.lock:
            if self.scan is not None:
                self.scan.cancel()
            self.proc.refresh_async()
            self.scan = Scan(path, self.classifier, self.proc, apparent=apparent, cross_fs=cross_fs)
            self.scan.start()
        return self.status()

    def agent_scan(self, path, rescan=False, apparent=False, cross_fs=True):
        """AI 发起的扫描：已有覆盖该路径、且方式相同的结果时直接复用，避免打断用户。"""
        path = norm_path(path or system_root())
        s = self.scan
        if s is not None and not rescan and s.state in ("scanning", "done") \
                and s.apparent == apparent and (s.cross_fs or not cross_fs):
            root = pkey(s.root_path)
            prefix = root if root.endswith(SEP) else root + SEP
            if pkey(path) == root or pkey(path).startswith(prefix):
                if s.find(path)[0] is not None or s.state == "scanning":
                    return {"reused": True, **self.status()}
        return {"reused": False, **self.start_scan(path, apparent=apparent, cross_fs=cross_fs)}

    def status(self):
        s = self.scan
        base = {
            "ready": self.ready.is_set(),
            "os": OS_NAME,
            "admin": self.admin,
            "pkg": {"manager": self.pkgdb.manager, "label": self.pkgdb.label,
                    "packages": self.pkgdb.npkgs, "files": len(self.pkgdb.files),
                    "error": self.pkgdb.error},
            "procs": {"total": self.proc.nprocs, "visible": self.proc.visible,
                      "files": len(self.proc.files), "refreshing": self.proc.refreshing,
                      "error": self.proc.error},
            "command": self.command,
            "agent_seen_ago": round(time.time() - self.agent_seen, 1) if self.agent_seen else None,
        }
        if s is None:
            base["state"] = "idle"
            return base
        end = s.finished or time.time()
        base.update({
            "state": s.state, "error": s.error, "root": s.root_path,
            "files": s.nfiles, "dirs": s.ndirs, "bytes": s.bytes,
            "denied": s.denied, "denied_samples": s.denied_samples[:8],
            "current": s.current if s.state == "scanning" else "",
            "elapsed": round(end - s.started, 2), "apparent": s.apparent,
            "cross_fs": s.cross_fs, "fs_used": s.fs_used,
            "mounts": s.included_mounts, "skipped": s.skipped_mounts[:30],
        })
        return base

    def tree(self, path, frac):
        s = self.scan
        if s is None:
            return None
        res = s.view(path or s.root_path, frac=frac, pkgdb=self.pkgdb)
        if res is None:
            return None
        node, out = res
        return {"path": node.path, "root": s.root_path, "state": s.state,
                "totals": s.totals(node), "tree": out}

    def info(self, path):
        s = self.scan
        if s is None:
            return None
        node, f = s.find(path)
        if node is None:
            return None
        if f is None:
            d = node
            res = {"path": d.path, "name": d.name, "type": "dir", "size": d.size,
                   "nfiles": d.nfiles, "ndirs": d.ndirs, "cls": d.cls, "reason": d.reason,
                   "running": self.proc.dir_count(d.path), "denied": bool(d.flags & 1)}
            if d.flags & MOUNT:
                m = {pkey(k): v for k, v in read_mounts().items()}.get(pkey(d.path))
                if m:
                    res["mount"] = {"fstype": m["fstype"], "source": m["source"]}
            target = d.path
        else:
            target = join_path(node.path, f[0])
            cls, reason = self.classifier.explain_file(node.cls, node.reason, node.path, f[0], target)
            procs, nprocs = self.proc.details(target)
            res = {"path": target, "name": f[0], "type": "file", "size": f[1],
                   "cls": RUNNING if nprocs else cls, "static_cls": cls, "reason": reason,
                   "pkg": self.pkgdb.files.get(target), "procs": procs, "nprocs": nprocs}
        try:
            st = os.lstat(target)
            res["mtime"] = time.strftime("%Y-%m-%d %H:%M", time.localtime(st.st_mtime))
            res["mode"] = stat.filemode(st.st_mode)
            if pwd is not None:
                try:
                    res["owner"] = pwd.getpwuid(st.st_uid).pw_name
                except KeyError:
                    res["owner"] = str(st.st_uid)
                try:
                    res["group"] = grp.getgrgid(st.st_gid).gr_name
                except KeyError:
                    res["group"] = str(st.st_gid)
            if f is not None:
                res["apparent"] = st.st_size
        except OSError:
            res["gone"] = True
        return res

    def reveal(self, path):
        """在文件管理器中显示。"""
        if self.admin and not IS_WIN:
            raise ValueError("以 root 身份运行时不打开文件管理器，以免以 root 权限启动图形程序")
        s = self.scan
        if s is None or s.find(path)[0] is None:
            raise ValueError("只能打开扫描结果中的路径")
        path = norm_path(path)
        if not os.path.lexists(path):
            raise ValueError("该文件已不存在")
        is_dir = os.path.isdir(path) and not os.path.islink(path)
        if IS_WIN:
            if is_dir:
                os.startfile(path)
            else:
                subprocess.Popen(f'explorer /select,"{path}"')
            return
        quiet = dict(stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     stdin=subprocess.DEVNULL, start_new_session=True, env=clean_env())
        if IS_MAC:
            subprocess.Popen(["open", path] if is_dir else ["open", "-R", path], **quiet)
            return
        if is_dir:
            subprocess.Popen(["xdg-open", path], **quiet)
            return
        uri = "file://" + urllib.parse.quote(path)
        try:
            r = subprocess.run(
                ["gdbus", "call", "--session", "--dest", "org.freedesktop.FileManager1",
                 "--object-path", "/org/freedesktop/FileManager1",
                 "--method", "org.freedesktop.FileManager1.ShowItems", f"['{uri}']", ""],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5, env=clean_env())
            if r.returncode == 0:
                return
        except (OSError, subprocess.TimeoutExpired):
            pass
        subprocess.Popen(["xdg-open", os.path.dirname(path) or "/"], **quiet)


def agent_info():
    """告诉用户如何把 DiskMap 接入 AI 助手（MCP）。"""
    if FROZEN:
        base = [sys.executable]
    else:
        base = [sys.executable, os.path.join(resource_dir(), "diskmap.py")]
    cmd = base + ["mcp"]
    join = subprocess.list2cmdline if IS_WIN else shlex.join
    return {
        "command": cmd,
        "claude_code": "claude mcp add --scope user diskmap -- " + join(cmd),
        "json": json.dumps({"mcpServers": {"diskmap": {"command": cmd[0], "args": cmd[1:]}}},
                           ensure_ascii=False, indent=2),
        "report": join(base + ["analyze", system_root()]),
    }


AGENT_GET = {
    "/api/agent/status": lambda app, q: analysis.status(app),
    "/api/agent/summary": lambda app, q: analysis.summary(app, q.get("path"), q.get("top", 10)),
    "/api/agent/list": lambda app, q: analysis.list_dir(app, q.get("path"), q.get("limit", 50)),
    "/api/agent/explain": lambda app, q: analysis.explain(app, q.get("path") or ""),
    "/api/agent/cleanup": lambda app, q: analysis.cleanup(
        app, q.get("path"), q.get("limit", 30), int(float(q.get("min_mb", 1)) * (1 << 20))),
    "/api/agent/largest": lambda app, q: analysis.largest_files(
        app, q.get("path"), q.get("limit", 20), q.get("category") or None),
    "/api/agent/view": lambda app, q: analysis.user_view(app),
}


def make_handler(app, token, port_ref):
    allowed_hosts = None

    class Handler(BaseHTTPRequestHandler):
        server_version = "DiskMap"
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt, *args):  # 安静一点
            pass

        # --------------------------------------------------- 工具
        def _host_ok(self):
            nonlocal allowed_hosts
            if allowed_hosts is None:
                p = port_ref[0]
                allowed_hosts = {f"127.0.0.1:{p}", f"localhost:{p}", f"[::1]:{p}"}
            return self.headers.get("Host", "") in allowed_hosts

        def _send(self, code, body, ctype="application/json; charset=utf-8", extra=None):
            if isinstance(body, (dict, list)) or body is None:
                body = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode()
            elif isinstance(body, str):
                body = body.encode()
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            for k, v in (extra or {}).items():
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(body)

        def _err(self, code, msg):
            self._send(code, {"error": msg})

        def _api_ok(self):
            if not self._host_ok():
                self._err(403, "bad host")
                return False
            got = self.headers.get("X-DiskMap-Token", "")
            if not hmac.compare_digest(got, token):
                self._err(403, "bad token")
                return False
            return True

        def _body(self):
            n = int(self.headers.get("Content-Length") or 0)
            if n <= 0 or n > 1_000_000:
                return {}
            try:
                return json.loads(self.rfile.read(n))
            except ValueError:
                return {}

        # --------------------------------------------------- 路由
        def do_GET(self):
            url = urllib.parse.urlsplit(self.path)
            q = urllib.parse.parse_qs(url.query)
            is_agent = url.path.startswith("/api/agent/")
            if not is_agent:
                app.last_seen = time.time()
            if url.path == "/":
                if not self._host_ok():
                    return self._err(403, "bad host")
                if not hmac.compare_digest(q.get("t", [""])[0], token):
                    return self._send(403, "<meta charset=utf-8><p style='font:16px sans-serif;padding:2em'>"
                                      "请使用 DiskMap 启动时输出的完整链接打开（链接里包含访问令牌）。</p>",
                                      "text/html; charset=utf-8")
                with open(os.path.join(WEB_DIR, "index.html"), encoding="utf-8") as f:
                    html = f.read().replace("__DISKMAP_TOKEN__", token)
                return self._send(200, html, "text/html; charset=utf-8",
                                  {"Referrer-Policy": "no-referrer"})
            if url.path.startswith("/static/"):
                name = os.path.basename(url.path)
                full = os.path.join(WEB_DIR, name)
                ext = os.path.splitext(name)[1]
                if ext not in CONTENT_TYPES or not os.path.isfile(full):
                    return self._err(404, "not found")
                with open(full, "rb") as f:
                    return self._send(200, f.read(), CONTENT_TYPES[ext])
            if not url.path.startswith("/api/"):
                return self._err(404, "not found")
            if not self._api_ok():
                return
            try:
                if is_agent:
                    app.agent_seen = time.time()
                    fn = AGENT_GET.get(url.path)
                    if fn is None:
                        return self._err(404, "not found")
                    return self._send(200, fn(app, {k: v[0] for k, v in q.items()}))
                if url.path == "/api/meta":
                    return self._send(200, {"classes": CLASSES, "home": os.path.expanduser("~"),
                                            "disks": disk_mounts(), "os": OS_NAME, "sep": SEP,
                                            "system_root": system_root(), "admin": app.admin,
                                            "version": __version__, "agent": agent_info()})
                if url.path == "/api/status":
                    return self._send(200, app.status())
                if url.path == "/api/tree":
                    frac = float(q.get("frac", ["0.000025"])[0])
                    frac = min(max(frac, 1e-6), 0.05)
                    res = app.tree(q.get("path", [""])[0], frac)
                    if res is None:
                        return self._err(404, "没有这个路径的扫描结果")
                    return self._send(200, res)
                if url.path == "/api/info":
                    res = app.info(q.get("path", [""])[0])
                    if res is None:
                        return self._err(404, "没有这个路径的扫描结果")
                    return self._send(200, res)
            except (analysis.AnalysisError, ValueError) as e:
                return self._err(400, str(e))
            except Exception as e:  # pragma: no cover
                return self._err(500, repr(e))
            return self._err(404, "not found")

        def do_POST(self):
            url = urllib.parse.urlsplit(self.path)
            if url.path == "/api/bye":
                # 页面关闭时用 sendBeacon 发来（不能带自定义请求头，令牌放在查询参数里）
                q = urllib.parse.parse_qs(url.query)
                if self._host_ok() and hmac.compare_digest(q.get("t", [""])[0], token):
                    app.bye_at = time.time()
                    return self._send(204, b"")
                return self._err(403, "bad token")
            is_agent = url.path.startswith("/api/agent/")
            if not is_agent:
                app.last_seen = time.time()
            if not self._api_ok():
                return
            body = self._body()
            try:
                if url.path == "/api/agent/scan":
                    app.agent_seen = time.time()
                    return self._send(200, app.agent_scan(
                        body.get("path") or system_root(), rescan=bool(body.get("rescan")),
                        apparent=bool(body.get("apparent")), cross_fs=not body.get("same_fs")))
                if url.path == "/api/agent/show":
                    app.agent_seen = time.time()
                    return self._send(200, analysis.show(app, str(body.get("path", "")), body.get("note")))
                if url.path == "/api/ui-state":
                    app.ui_state = {"view": body.get("view"), "selected": body.get("selected"),
                                    "pinned": body.get("pinned"), "time": time.time()}
                    return self._send(200, {"ok": True})
                if url.path == "/api/scan":
                    return self._send(200, app.start_scan(body.get("path") or system_root(),
                                                          apparent=bool(body.get("apparent")),
                                                          cross_fs=body.get("cross_fs", True) is not False))
                if url.path == "/api/cancel":
                    if app.scan:
                        app.scan.cancel()
                    return self._send(200, app.status())
                if url.path == "/api/refresh-procs":
                    app.proc.refresh()
                    if app.scan:
                        app.scan._run_cache = None
                    return self._send(200, app.status())
                if url.path == "/api/reveal":
                    app.reveal(str(body.get("path", "")))
                    return self._send(200, {"ok": True})
            except ValueError as e:
                return self._err(400, str(e))
            except Exception as e:  # pragma: no cover
                return self._err(500, repr(e))
            return self._err(404, "not found")

    return Handler


def serve(port=0, token=None):
    """启动服务（后台线程），返回 (server, url, app)。"""
    token = token or secrets.token_urlsafe(18)
    app = App()
    port_ref = [0]
    try:
        httpd = ThreadingHTTPServer(("127.0.0.1", port), make_handler(app, token, port_ref))
    except OSError:
        httpd = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(app, token, port_ref))
    httpd.daemon_threads = True
    port_ref[0] = httpd.server_address[1]
    threading.Thread(target=httpd.serve_forever, name="diskmap-http", daemon=True).start()
    url = f"http://127.0.0.1:{port_ref[0]}/?t={token}"
    return httpd, url, app
