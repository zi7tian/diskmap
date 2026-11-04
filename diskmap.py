#!/usr/bin/env python3
"""DiskMap 磁盘地图 —— 用嵌套方框查看磁盘占用，并按重要程度着色。

用法：
    python3 diskmap.py              # 打开窗口，在界面里选择要扫描的位置
    python3 diskmap.py /home        # 打开窗口并立即扫描 /home（Windows 如 D:\\）
    python3 diskmap.py --browser    # 用默认浏览器而不是独立窗口
    sudo python3 diskmap.py --no-open /   # 以 root 扫描全部文件，只输出链接

界面窗口按以下顺序尝试，哪个可用就用哪个：
    1. pywebview（Windows 用 Edge WebView2，macOS 用系统 WebKit）
    2. Linux：GTK + WebKit2GTK（源码运行时直接调用；打包版借用系统自带的 Python）
    3. Chrome / Edge / Chromium 的"应用模式"窗口
    4. 默认浏览器（关闭标签页后程序自动退出）
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import threading
import time
import urllib.parse
import webbrowser

# 打包成无控制台的 Windows 程序时 stdout/stderr 为 None。替换成空设备时必须指定 UTF-8，
# 否则英文版 Windows 默认用 cp1252 编码，一输出中文就抛异常（窗口模式下会弹出错误框卡住）。
for _name in ("stdout", "stderr"):
    _stream = getattr(sys, _name)
    if _stream is None:
        setattr(sys, _name, open(os.devnull, "w", encoding="utf-8"))
    else:
        try:
            _stream.reconfigure(errors="replace")   # 控制台编码不支持中文时用 ? 代替而不是崩溃
        except (AttributeError, ValueError):
            pass

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from backend import __version__  # noqa: E402
from backend.osinfo import (FROZEN, IS_LINUX, IS_MAC, IS_WIN, clean_env,  # noqa: E402
                            clean_environ, is_admin, resource_dir, user_cache_dir)
from backend.server import serve  # noqa: E402

TITLE = "DiskMap 磁盘地图"
VERBOSE = False


def log(*a):
    if VERBOSE:
        print("[diskmap]", *a, file=sys.stderr, flush=True)


# ------------------------------------------------------------------ 窗口
def open_pywebview(url):
    if not (IS_WIN or IS_MAC):
        return "unavailable"
    try:
        import webview
    except ImportError:
        return "unavailable"
    try:
        webview.create_window(TITLE, url, width=1440, height=900, min_size=(820, 560),
                              background_color="#0E1014")
        webview.start(gui="edgechromium" if IS_WIN else None)
        return "done"
    except Exception as e:
        log("pywebview 失败：", repr(e))
        return "unavailable"


GTK_HELPER = r'''
import sys
try:
    import gi
    gi.require_version("Gtk", "3.0")
    try:
        gi.require_version("WebKit2", "4.1")
    except ValueError:
        gi.require_version("WebKit2", "4.0")
    from gi.repository import GLib, Gtk, WebKit2
except Exception:
    sys.exit(3)
GLib.set_prgname("diskmap")            # 窗口的应用 ID，对应 .desktop 里的 StartupWMClass
GLib.set_application_name(sys.argv[2])
if not Gtk.init_check(sys.argv)[0]:
    sys.exit(3)
win = Gtk.Window(title=sys.argv[2])
win.set_default_size(1440, 900)
try:
    win.set_icon_from_file(sys.argv[3])
except Exception:
    win.set_icon_name("drive-harddisk")
view = WebKit2.WebView()
s = view.get_settings()
s.set_javascript_can_access_clipboard(True)
view.load_uri(sys.argv[1])
win.add(view)
win.connect("destroy", Gtk.main_quit)
win.show_all()
try:
    Gtk.main()
except KeyboardInterrupt:
    pass
'''


def open_gtk(url):
    """Linux：GTK + WebKit2GTK 窗口。"""
    if not IS_LINUX or not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")):
        return "unavailable"
    if not FROZEN:
        # 源码运行：当前 Python 里可能就有 gi
        try:
            import gi
            gi.require_version("Gtk", "3.0")
            gi.require_version("WebKit2", "4.1")
        except (ImportError, ValueError):
            pass
        else:
            return _run_helper(sys.executable, url)
    # 打包版：借用系统 Python 的 gi（很多发行版桌面都自带）
    for py in ("/usr/bin/python3", shutil.which("python3")):
        if py and os.path.exists(py) and os.path.realpath(py) != os.path.realpath(sys.executable):
            r = _run_helper(py, url)
            if r != "unavailable":
                return r
    return "unavailable"


def _run_helper(python, url):
    try:
        icon = os.path.join(resource_dir(), "web", "favicon.png")
        p = subprocess.Popen([python, "-c", GTK_HELPER, url, TITLE, icon], env=clean_env(),
                             stdin=subprocess.DEVNULL,
                             stderr=None if VERBOSE else subprocess.DEVNULL)  # 屏蔽主题警告
    except OSError:
        return "unavailable"
    code = p.wait()
    log("GTK 窗口退出，代码", code)
    return "unavailable" if code == 3 else "done"


def _chromium_candidates():
    if IS_WIN:
        roots = [os.environ.get(k) for k in ("ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA")]
        rels = [r"Microsoft\Edge\Application\msedge.exe", r"Google\Chrome\Application\chrome.exe",
                r"BraveSoftware\Brave-Browser\Application\brave.exe"]
        return [os.path.join(r, rel) for rel in rels for r in roots if r]
    if IS_MAC:
        return [f"/Applications/{a}.app/Contents/MacOS/{a}" for a in
                ("Google Chrome", "Microsoft Edge", "Chromium", "Brave Browser")]
    names = ("google-chrome-stable", "google-chrome", "chromium", "chromium-browser",
             "microsoft-edge-stable", "microsoft-edge", "brave-browser", "brave")
    return [p for p in (shutil.which(n) for n in names) if p]


def open_chromium_app(url):
    """Chrome / Edge 的应用模式：没有地址栏的独立窗口。"""
    exe = next((p for p in _chromium_candidates() if os.path.isfile(p)), None)
    if not exe:
        return "unavailable"
    profile = os.path.join(user_cache_dir(), "browser-profile")
    os.makedirs(profile, exist_ok=True)
    args = [exe, f"--app={url}", f"--user-data-dir={profile}", "--no-first-run",
            "--no-default-browser-check", "--window-size=1440,900"]
    try:
        t0 = time.time()
        p = subprocess.Popen(args, env=clean_env(), stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except OSError:
        return "unavailable"
    p.wait()
    # 立刻退出说明窗口交给了已在运行的同一配置实例（例如同时开了两个 DiskMap）
    return "handoff" if time.time() - t0 < 3 else "done"


def wait_for_page_close(app, first_contact_timeout=120):
    """浏览器模式：页面关闭（或长时间没有心跳）后返回。"""
    t0 = time.time()
    while app.last_seen == 0:
        if time.time() - t0 > first_contact_timeout:
            return
        time.sleep(0.5)
    while True:
        time.sleep(1)
        now = time.time()
        if app.bye_at and app.bye_at > app.last_seen and now - app.bye_at > 8:
            return   # 页面已关闭，且没有被重新打开/刷新
        if now - app.last_seen > 180:
            return   # 后台标签页的定时器最慢约一分钟触发一次，留足余量


UI_OPENERS = {"webview": open_pywebview, "gtk": open_gtk, "chromium": open_chromium_app}


def run_ui(url, app, prefer_browser, only=None):
    if not prefer_browser:
        openers = [UI_OPENERS[only]] if only in UI_OPENERS else UI_OPENERS.values()
        for opener in openers:
            r = opener(url)
            log(opener.__name__, "→", r)
            if r == "done":
                return
            if r == "handoff":
                wait_for_page_close(app)
                return
    with clean_environ():
        webbrowser.open(url)
    wait_for_page_close(app)


# ------------------------------------------------------------------ 自检
def selftest(report_path=None, limit=150):
    """不打开窗口，走一遍完整流程：页面、扫描、树数据、详情、运行状态。供 CI 检验打包结果。

    无论成功、失败还是卡住（超过 limit 秒），都会写出报告并结束进程。
    """
    import tempfile
    import urllib.request
    results = []
    ok = True
    step = ["启动"]
    lock = threading.Lock()

    def check(name, cond, detail=""):
        nonlocal ok
        ok = ok and bool(cond)
        results.append({"check": name, "ok": bool(cond), "detail": str(detail)[:300]})

    def finish(code):
        with lock:
            report = {"ok": code == 0, "version": __version__, "platform": sys.platform,
                      "frozen": FROZEN, "results": results}
            text = json.dumps(report, ensure_ascii=False, indent=2)
            if report_path:
                with open(report_path, "w", encoding="utf-8") as f:
                    f.write(text)
            print(text, flush=True)
            os._exit(code)

    def watchdog():
        time.sleep(limit)
        check("超时", False, f"超过 {limit} 秒仍未完成，卡在：{step[0]}")
        finish(3)
    threading.Thread(target=watchdog, daemon=True).start()

    token = "selftest-token"
    httpd, url, app = serve(0, token)
    base = url.split("/?")[0]
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))  # 不走系统代理

    def call(path, body=None, auth=True):
        step[0] = path.split("?")[0]
        headers = {"X-DiskMap-Token": token} if auth else {}
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(base + path, data=data, headers=headers)
        with opener.open(req, timeout=30) as r:
            raw = r.read()
            return r.status, raw

    tmp = tempfile.mkdtemp(prefix="diskmap-selftest-")
    try:
        os.makedirs(os.path.join(tmp, "big"))
        os.makedirs(os.path.join(tmp, "many"))
        with open(os.path.join(tmp, "big", "data.bin"), "wb") as f:
            f.write(b"\0" * 300_000)
        for i in range(400):
            with open(os.path.join(tmp, "many", f"f{i}.txt"), "wb") as f:
                f.write(b"x" * 100)
        expected = 300_000 + 400 * 100

        st, html = call(f"/?t={token}", auth=False)
        check("首页", st == 200 and token.encode() in html)
        st, js = call("/static/app.js", auth=False)
        check("界面脚本", st == 200 and b"squarify" in js)
        st, raw = call("/api/meta")
        meta = json.loads(raw)
        check("元数据", len(meta["classes"]) == 7 and meta["sep"] == os.sep, meta.get("os"))
        check("磁盘列表", len(meta["disks"]) >= 1, [d["path"] for d in meta["disks"]])

        call("/api/scan", {"path": tmp, "apparent": True})
        status = {}
        for _ in range(300):
            status = json.loads(call("/api/status")[1])
            if status["state"] != "scanning":
                break
            time.sleep(0.1)
        check("扫描完成", status.get("state") == "done", status.get("error"))
        q = urllib.parse.quote(tmp)
        tree = json.loads(call(f"/api/tree?path={q}")[1])
        check("大小正确", tree["tree"]["s"] == expected, f'统计 {tree["tree"]["s"]}，应为 {expected}')
        names = sorted(c["n"] for c in tree["tree"]["ch"] if c["t"] == "d")
        check("子文件夹", names == ["big", "many"], names)
        fq = urllib.parse.quote(os.path.join(tmp, "big", "data.bin"))
        info = json.loads(call(f"/api/info?path={fq}")[1])
        check("文件详情", info["size"] == 300_000 and info["type"] == "file", info.get("reason"))
        st, raw = call("/api/refresh-procs", {})
        procs = json.loads(raw)["procs"]
        check("读取运行中的程序", procs["total"] > 0 and not procs["error"], procs)
        check("识别正在使用的文件", procs["files"] > 0, procs["files"])
    except BaseException as e:
        check("异常", False, repr(e))
    finally:
        step[0] = "清理"
        httpd.shutdown()
        shutil.rmtree(tmp, ignore_errors=True)
    finish(0 if ok else 1)


# ------------------------------------------------------------------ 入口
def main():
    global VERBOSE
    ap = argparse.ArgumentParser(description="DiskMap 磁盘地图：可视化磁盘占用并标注文件重要程度")
    ap.add_argument("path", nargs="?", help="启动后立即扫描的文件夹（例如 / 、/home 或 D:\\）")
    ap.add_argument("--port", type=int, default=int(os.environ.get("DISKMAP_PORT", 0)),
                    help="本地端口（默认自动选择）")
    ap.add_argument("--browser", action="store_true", help="用默认浏览器打开界面")
    ap.add_argument("--no-open", action="store_true", help="不打开任何窗口，只输出访问链接")
    ap.add_argument("--apparent", action="store_true", help="按文件大小而不是实际占用的磁盘空间统计")
    ap.add_argument("--same-fs", action="store_true", help="不进入其他分区（类似 du -x）")
    ap.add_argument("--verbose", action="store_true", help="输出调试信息")
    ap.add_argument("--ui", choices=["webview", "gtk", "chromium", "browser"],
                    default=os.environ.get("DISKMAP_UI") or None,
                    help="指定界面方式（默认自动选择）")
    ap.add_argument("--selftest", action="store_true", help=argparse.SUPPRESS)
    ap.add_argument("--report", help=argparse.SUPPRESS)
    ap.add_argument("--version", action="version", version=f"DiskMap {__version__}")
    ap.add_argument("--token", default=os.environ.get("DISKMAP_TOKEN"), help=argparse.SUPPRESS)
    args = ap.parse_args()
    VERBOSE = args.verbose

    if args.selftest:
        selftest(args.report)   # 内部会结束进程

    httpd, url, app = serve(args.port, args.token)
    if args.path:
        app.start_scan(args.path, apparent=args.apparent, cross_fs=not args.same_fs)
        url += "&" + urllib.parse.urlencode({"view": app.scan.root_path})

    print(f"DiskMap 已启动：{url}", flush=True)
    if not IS_WIN and is_admin() and not args.no_open:
        print("以 root 运行：为安全起见不自动打开窗口，请在普通用户的浏览器中打开上面的链接。", flush=True)
        args.no_open = True

    try:
        if args.no_open:
            print("按 Ctrl+C 退出。", flush=True)
            threading.Event().wait()
        else:
            run_ui(url, app, args.browser or args.ui == "browser", args.ui)
    except KeyboardInterrupt:
        pass
    finally:
        if app.scan:
            app.scan.cancel()
        httpd.shutdown()
    # 某些窗口库会留下非守护线程，直接结束进程
    sys.stdout.flush()
    os._exit(0)


if __name__ == "__main__":
    main()
