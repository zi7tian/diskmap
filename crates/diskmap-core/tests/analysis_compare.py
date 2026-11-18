#!/usr/bin/env python3
"""T06 新旧分析对照；默认运行 Rust 测试，--reference 为测试提供旧版 JSON。

测试用实际扫描的同一个临时树；版本和时钟固定，scandir 仅按名字排序以固定平局。
不启动 App、HTTP 服务、进程采集或软件包加载；所有设置路径均在测试临时目录。
"""
import argparse
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
import time
from types import MethodType, SimpleNamespace
from compare_support.process import run_process

ROOT = Path(__file__).resolve().parents[3]
NOW = 1_700_000_001.25


def reference(root):
    os.environ["DISKMAP_CACHE_DIR"] = str(root / "isolated-cache")
    os.environ["DISKMAP_CONFIG_DIR"] = str(root / "isolated-config")
    sys.path.insert(0, str(ROOT / "legacy"))
    from backend import analysis, scanner
    from backend.classify import Classifier
    from backend.i18n import loc, use
    from backend.osinfo import OS_NAME
    from backend.pkgdb import PkgDB
    from backend.procinfo import ProcInfo
    from backend.server import App

    # 旧版目录保存 readdir 次序，新版完整索引按名字排序。仅固定该输入差异，
    # 保留旧分析函数的稳定排序、heapq 平局、150 分组和全部文字逻辑。
    real_scandir = os.scandir

    class SortedScandir:
        def __init__(self, path):
            with real_scandir(path) as entries:
                self.entries = sorted(entries, key=lambda entry: entry.name.lower() if os.name == "nt" else entry.name)

        def __enter__(self):
            return self

        def __iter__(self):
            return iter(self.entries)

        def __exit__(self, *_):
            return False

    scanner_os = vars(os).copy()
    scanner_os["scandir"] = SortedScandir
    scanner.os = SimpleNamespace(**scanner_os)
    analysis.time = SimpleNamespace(time=lambda: NOW, strftime=time.strftime, localtime=time.localtime)

    def app_for(path, busy):
        pkg = PkgDB()
        homes, swapfiles = [str(path.parent)], []
        if path.name == "classes":
            pkg.files[str(path / "installed.bin")] = "test-package"
            pkg.files[str(path / "settings.ini")] = "test-package"
            pkg.backup.add(str(path / "settings.ini"))
            swapfiles.append(str(path / "core.sys"))
            homes.append(str(path))
        classifier = Classifier(pkg, swapfiles, flavor=OS_NAME, env={}, homes=homes)
        proc = ProcInfo()
        if busy:
            slot = {100 + i: (f"worker{i % 3}", "exe" if i % 2 == 0 else "open") for i in range(12)}
            solo = {7: ("viewer", "open")}
            raw = ({str(path / "f000.data"): slot, str(path / "f160.log"): solo} if path.name == "dense" else
                   {str(path / "proj/Build.log"): slot, str(path / "docs/Report.pdf"): solo})
            proc.install(raw)
        proc.nprocs, proc.visible = 12, 10
        scan = scanner.Scan(str(path), classifier, proc, apparent=True, cross_fs=False)
        scan._run()
        scan.state = "done"
        scan.finished = 1_700_000_000.0
        scan.started = scan.finished - 12.25
        scan.fs_used = 0
        ready = threading.Event()
        ready.set()
        app = SimpleNamespace(scan=scan, proc=proc, classifier=classifier, pkgdb=pkg, version="test",
                              os_name=OS_NAME, admin=False, ready=ready, last_seen=0.0, ui_state=None,
                              command=None, lock=threading.Lock())
        app.info = MethodType(App.info, app)
        return app

    def call(fn, *args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except analysis.AnalysisError as error:
            return {"error": str(error)}

    def common(app, path):
        path = str(path)
        missing = os.path.join(path, "missing")
        return {
            "status": analysis.status(app),
            "summary": call(analysis.summary, app, None, 50),
            "summary_low": call(analysis.summary, app, "", -3),
            "list": call(analysis.list_dir, app, None, 1000),
            "list_low": call(analysis.list_dir, app, None, -3),
            "explain_root": call(analysis.explain, app, path),
            "cleanup": call(analysis.cleanup, app, None, 1000, 0),
            "cleanup_filtered": call(analysis.cleanup, app, None, 1, (1 << 64) - 1),
            "largest": call(analysis.largest_files, app, None, 1000),
            "largest_low": call(analysis.largest_files, app, None, -3),
            "largest_cache": call(analysis.largest_files, app, None, 200, "cache"),
            "largest_user": call(analysis.largest_files, app, None, 200, "user"),
            "largest_core": call(analysis.largest_files, app, None, 200, "core"),
            "largest_system": call(analysis.largest_files, app, None, 200, "system"),
            "largest_config": call(analysis.largest_files, app, None, 200, "config"),
            "largest_other": call(analysis.largest_files, app, None, 200, "other"),
            "largest_running": call(analysis.largest_files, app, None, 200, "running"),
            "invalid_category": call(analysis.largest_files, app, None, 20, "nonsense"),
            "empty_category": call(analysis.largest_files, app, None, 20, ""),
            "missing_summary": call(analysis.summary, app, missing, 5),
            "missing_explain": call(analysis.explain, app, missing),
            "missing_show": call(analysis.show, app, missing),
            "user_view_missing": call(analysis.user_view, app),
            "report_json": call(analysis.report_json, app, None, 5),
            "report_markdown": analysis.report_markdown(app, None, 5),
        }

    def extras(app, path):
        def p(name):
            return str(path / name)

        output = {
            "explain_file": call(analysis.explain, app, p("docs/Report.pdf")),
            "explain_used": call(analysis.explain, app, p("proj/Build.log")),
            "explain_dir": call(analysis.explain, app, p("proj")),
            "list_file": call(analysis.list_dir, app, p("docs/Report.pdf"), 50),
            "cleanup_cache_root": call(analysis.cleanup, app, p("proj/cache"), 30, 0),
            "show_disconnected": call(analysis.show, app, p("docs/Report.pdf"), "没有窗口"),
        }
        app.last_seen = NOW
        app.ui_state = {"view": p("proj"), "selected": p("proj/Build.log"), "pinned": "cache", "time": NOW - 2.25}
        output["user_view"] = call(analysis.user_view, app)
        output["status_with_view"] = analysis.status(app)
        output["show_connected"] = call(analysis.show, app, p("docs"), "中🦀" * 105)
        output["command"] = app.command
        app.last_seen = NOW - 15.0
        output["window_at_15"] = analysis.status(app)
        app.last_seen = NOW - 30.0
        output["view_at_30"] = call(analysis.user_view, app)
        app.last_seen = NOW - 30.01
        output["view_after_30"] = call(analysis.user_view, app)
        app.last_seen = NOW
        app.ui_state = {"view": p("missing"), "selected": p("missing"), "pinned": None, "time": NOW}
        output["view_missing_paths"] = call(analysis.user_view, app)
        app.ui_state = None
        app.last_seen = 0.0
        app.scan.state = "scanning"
        app.scan.finished = None
        app.scan.started = NOW - 12.25
        app.scan.current = ""
        output["initial_status"] = analysis.status(app)
        for state in ("scanning", "cancelled", "error"):
            app.scan.state = state
            app.scan.finished = None
            app.scan.started = NOW - 12.25
            app.scan.current = str(path)
            output[state] = {
                "status": analysis.status(app),
                "summary": call(analysis.summary, app, None, 5),
                "list": call(analysis.list_dir, app, None, 5),
                "cleanup": call(analysis.cleanup, app, None, 5, 0),
                "largest": call(analysis.largest_files, app, None, 5),
                "markdown": analysis.report_markdown(app, None, 5),
            }
        app.scan.find(p("docs"))[0].flags |= scanner.DENIED
        app.scan.state = "done"
        app.scan.finished = 1_700_000_000.0
        app.scan.started = app.scan.finished - 12.25
        app.scan.current = ""
        app.scan.denied = 1
        output["denied"] = {"status": analysis.status(app), "explain": call(analysis.explain, app, p("docs")),
                            "markdown": analysis.report_markdown(app, None, 5)}
        app.scan = None
        output["no_scan"] = {"status": analysis.status(app), "summary": call(analysis.summary, app, None, 5),
                             "explain": call(analysis.explain, app, p("docs/Report.pdf")),
                             "show": call(analysis.show, app, p("docs/Report.pdf"))}
        return output

    result = {}
    for language in ("zh", "en"):
        with use(language):
            output = {}
            for name, busy in (("basic", False), ("busy", True), ("dense", False), ("dense_busy", True), ("ties", False), ("empty", False), ("classes", False)):
                directory = "basic" if name == "busy" else "dense" if name == "dense_busy" else name
                path = root / directory
                app = app_for(path, busy)
                output[name] = common(app, path)
                if name in ("basic", "busy"):
                    output[name]["extras"] = extras(app, path)
                if name == "classes":
                    output[name]["explanations"] = [call(analysis.explain, app, str(path / file)) for file in
                                                   ("core.sys", "installed.bin", "settings.ini", ".config-file",
                                                    ".app/state.bin", "personal.txt", ".cache/blob.dat")]
            output["human"] = [analysis.human(n) for n in (0, 1, 1023, 1024, 10_239, 10_240, 102_399, 102_400,
                                                           1_048_575, 1_048_576, 1 << 50, (1 << 64) - 1)]
            result[language] = loc(output)
    return result


def main():
    # 重定向到 SSH/PowerShell 日志时也固定 UTF-8，不沿用中文 Windows 的 GBK。
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="strict")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference", type=Path, help="内部模式：读取 Rust 测试创建的目录并输出旧版结果")
    args = parser.parse_args()
    if args.reference is not None:
        json.dump(reference(args.reference), sys.stdout, ensure_ascii=False)
        return 0
    with tempfile.TemporaryDirectory(prefix="diskmap-analysis-env-") as temporary:
        environment = dict(os.environ, DISKMAP_CACHE_DIR=os.path.join(temporary, "cache"),
                           DISKMAP_CONFIG_DIR=os.path.join(temporary, "config"), PYTHONDONTWRITEBYTECODE="1",
                           PYTHONUTF8="1", PYTHONIOENCODING="utf-8")
        result = run_process(["cargo", "test", "-p", "diskmap-core", "--test", "analysis_legacy", "--", "--nocapture"],
                             cwd=ROOT, env=environment, timeout=300)
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
        return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
