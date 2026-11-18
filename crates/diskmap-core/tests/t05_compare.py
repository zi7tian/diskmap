#!/usr/bin/env python3
"""T05 HTTP 黑盒契约对比；所有写入和服务登记都放在独立临时目录。

先构建：cargo build -p diskmap-core --features server --example serve
再运行：python3 -B crates/diskmap-core/tests/t05_compare.py

默认固定软件包/进程为空，确保两个扫描看到相同分类与在用文件。
仅归一化：版本/可执行命令（分别验证形状）、磁盘实时 used/free、扫描 elapsed/fs_used，
最后一次显式进程刷新的 total/visible/files，
以及 tree.ch 中同大小项目的顺序。扫描刚启动或正在取消时只比较字段/类型和不变量，
不声称不同并行扫描调度下的中间计数相同。最终完成后的所有计数、分类、字段逐项比较。
另起空软件包/进程的两个服务，比较全部 AI 接口、扫描复用、用户视图与跳转命令。
AI 响应额外只归一化版本、完成时间/用时、实时磁盘使用量、UI/命令的时间戳与AI接入时间。
Python <3.12 对非空字典 note 的切片返回 TypeError；这两条双语500响应与新版固定的
KeyError 逐字核验、单独统计，其余错误消息及字段不归一化。
Windows 仅 agent switch allocated done / agent reuse default apparent done 的 scan.bytes/size
单独核验：旧字节数等于实际簇大小的逐文件取整和，新字节数介于 apparent 与旧值之间；
两侧 size 分别严格等于旧版 human(各自 bytes)，仍只计一次口径差异，其余字段严格。
GetCompressedFileSizeW 汇总和小文件对齐统计只作真机诊断，不假定它等于扫描总和。
Unix 另向本脚本创建的新服务子进程发送 SIGINT/SIGTERM，验证正常退出和实例清理。
"""

from __future__ import annotations

import argparse
import copy
from contextlib import ExitStack
import http.client
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
import urllib.parse
from compare_support.process import ProcessOwner
from compare_support.fields import adapt_proc_paths, fixture_spellings
from compare_support.allocated import (AGENT_ALLOCATED_LABELS, adapt_agent_allocated_bytes,
                           windows_allocation_evidence)


REPO = Path(__file__).resolve().parents[3]
TOKEN = "t05-contract_token-123"
MISSING = object()


def legacy_server():
    """在子进程中固定测试依赖，HTTP handler/扫描/分类仍调用原实现。"""
    sys.path.insert(0, str(REPO / "legacy"))
    from backend import i18n, instance, server
    from backend.classify import Classifier

    def initialize(app):
        app.classifier = Classifier(app.pkgdb, [])
        app.proc.refresh_async = lambda: None
        app.ready.set()

    server.App._init = initialize
    i18n.set_default("zh")
    httpd, url, app = server.serve(0, TOKEN)
    app.ready.wait()
    instance.write(httpd.server_address[1], TOKEN, app.version)
    print(url, flush=True)
    try:
        sys.stdin.readline()
    finally:
        instance.remove()
        if app.scan:
            app.scan.cancel()
        httpd.shutdown()
        httpd.server_close()


class Backend:
    def __init__(self, name, command, directory, *, new_session=False):
        self.name = name
        self.cache = directory / name / "cache"
        self.config = directory / name / "config"
        env = dict(os.environ, DISKMAP_CACHE_DIR=str(self.cache),
                   DISKMAP_CONFIG_DIR=str(self.config), DISKMAP_LANG="zh",
                   DISKMAP_TOKEN=TOKEN, PYTHONDONTWRITEBYTECODE="1",
                   PYTHONUTF8="1", PYTHONIOENCODING="utf-8")
        self.owner = ProcessOwner(command, cwd=REPO, env=env, start_new_session=new_session)
        self.process = self.owner.process
        self.errors = self.owner.errors
        try:
            deadline = time.monotonic() + 45
            self.url = None
            while time.monotonic() < deadline:
                line = self.owner.readline(timeout=max(0.01, deadline - time.monotonic()))
                match = re.search(r"http://127\.0\.0\.1:[0-9]+/\?t=[^\s]+", line)
                if match:
                    self.url = match.group(0)
                    self.port = urllib.parse.urlsplit(self.url).port
                    break
            if self.url is None:
                raise RuntimeError(f"{name} 未输出 URL：{''.join(self.errors)}")
        except BaseException:
            self.stop()
            raise

    def request(self, path, body=MISSING, *, method=None, headers=None, raw=None):
        headers = dict(headers or {})
        # None 表示明确省略某请求头，便于测试未授权路径。
        headers.setdefault("X-DiskMap-Token", TOKEN)
        headers.setdefault("Host", f"127.0.0.1:{self.port}")
        headers = {k: v.replace("{port}", str(self.port)) for k, v in headers.items()
                   if v is not None}
        if body is not MISSING:
            raw = json.dumps(body, ensure_ascii=False).encode()
            headers.setdefault("Content-Type", "application/json")
        method = method or ("POST" if raw is not None else "GET")
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=20)
        try:
            conn.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
            if raw is not None:
                headers.setdefault("Content-Length", str(len(raw)))
            for key, value in headers.items():
                conn.putheader(key, value)
            conn.endheaders(raw)
            response = conn.getresponse()
            data = response.read()
            content_type = response.getheader("Content-Type", "")
            if "application/json" in content_type and data:
                data = json.loads(data)
            return {"status": response.status, "body": data,
                    "headers": {key.lower(): value for key, value in response.getheaders()
                                if key.lower() in ("content-type", "cache-control",
                                                   "x-content-type-options", "referrer-policy")}}
        finally:
            conn.close()

    def wait_done(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            response = self.request("/api/status")
            assert response["status"] == 200, response
            if response["body"]["state"] != "scanning":
                return response
            time.sleep(0.01)
        raise AssertionError(f"{self.name} 扫描未结束")

    def wait_agent_done(self):
        """AI 轮询不触发普通界面的 last_seen。"""
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            response = self.request("/api/agent/status")
            assert response["status"] == 200, response
            scan = response["body"]["scan"]
            if scan is not None and scan["state"] != "scanning":
                return response
            time.sleep(0.01)
        raise AssertionError(f"{self.name} AI扫描未结束")

    def stop(self):
        try:
            if self.process.poll() is None:
                self.process.stdin.write("stop\n")
                self.process.stdin.flush()
        except (OSError, ValueError):
            pass
        finally:
            self.owner.close(grace=10)


def tree_order(node):
    if isinstance(node, dict):
        for key, value in node.items():
            tree_order(value)
            if key == "ch":
                value.sort(key=lambda child: (-child["s"], child["t"], child["n"],
                                               child["c"], child.get("k", 0)))
    elif isinstance(node, list):
        for child in node:
            tree_order(child)


def normalize(response, kind):
    response = copy.deepcopy(response)
    body = response["body"]
    if response["status"] != 200 or not isinstance(body, dict):
        return response
    if kind in ("agent_app_status", "agent_progress"):
        assert isinstance(body["agent_seen_ago"], (int, float))
        assert body["agent_seen_ago"] >= 0
        body["agent_seen_ago"] = "<AI接入时间>"
        if body["command"] is not None:
            assert type(body["command"]["time"]) in (int, float)
            assert body["command"]["time"] > 0
            body["command"]["time"] = "<命令时间>"
        kind = "progress" if kind == "agent_progress" else "status"
    if kind == "agent_status":
        assert isinstance(body["version"], str) and body["version"]
        body["version"] = "<版本>"
        scan = body["scan"]
        if scan is not None:
            assert type(scan["elapsed_seconds"]) in (int, float)
            assert scan["elapsed_seconds"] >= 0
            scan["elapsed_seconds"] = "<扫描耗时>"
            if scan["finished_at"] is not None:
                time.strptime(scan["finished_at"], "%Y-%m-%d %H:%M:%S")
                scan["finished_at"] = "<完成时间>"
            if scan["disk_used_bytes"] is not None:
                assert type(scan["disk_used_bytes"]) is int and scan["disk_used_bytes"] >= 0
                scan["disk_used_bytes"] = "<磁盘实时空间>"
    elif kind == "agent_view":
        assert type(body["updated_seconds_ago"]) in (int, float)
        assert body["updated_seconds_ago"] >= 0
        body["updated_seconds_ago"] = "<UI更新时间>"
    if kind in ("status", "progress", "refresh"):
        for key in ("elapsed", "fs_used"):
            if key in body:
                assert isinstance(body[key], (int, float)) and not isinstance(body[key], bool)
                body[key] = "<动态数值>"
        if kind == "progress":
            assert body["state"] in ("scanning", "done", "cancelled"), body
            assert body["error"] is None, body
            assert isinstance(body["current"], str)
            for key in ("files", "dirs", "bytes", "denied"):
                assert type(body[key]) is int and body[key] >= 0
            for key in ("denied_samples", "mounts", "skipped"):
                assert isinstance(body[key], list)
            for key in ("state", "current", "files", "dirs", "bytes", "denied",
                        "denied_samples", "mounts", "skipped"):
                body[key] = "<并发进度>"
        if kind == "refresh":
            procs = body["procs"]
            assert set(procs) == {"total", "visible", "files", "refreshing", "error"}, procs
            for key in ("total", "visible", "files"):
                assert type(procs[key]) is int and procs[key] >= 0
                procs[key] = "<实际进程>"
            assert procs["refreshing"] is False and procs["error"] is None, procs
    elif kind == "meta":
        assert isinstance(body["version"], str) and body["version"]
        body["version"] = "<版本>"
        agent = body["agent"]
        assert set(agent) == {"command", "claude_code", "json", "report"}, agent
        assert isinstance(agent["command"], list) and len(agent["command"]) >= 2
        assert all(isinstance(x, str) for x in agent["command"])
        assert agent["command"][-1] == "mcp", agent
        config = json.loads(agent["json"])
        assert config == {"mcpServers": {"diskmap": {
            "command": agent["command"][0], "args": agent["command"][1:]}}}, agent
        assert agent["claude_code"].startswith("claude mcp add --scope user diskmap -- ")
        assert "analyze" in agent["report"] and body["system_root"] in agent["report"]
        body["agent"] = "<各自可执行命令>"
        for disk in body["disks"]:
            for key in ("used", "free"):
                assert isinstance(disk[key], int) and disk[key] >= 0, disk
                disk[key] = "<磁盘实时空间>"
    tree_order(body)
    return response


def first_difference(a, b, path="$"):
    if type(a) is not type(b):
        return f"{path}: 类型 {type(a).__name__} != {type(b).__name__}；{a!r} != {b!r}"
    if isinstance(a, dict):
        if a.keys() != b.keys():
            return f"{path}: 字段旧={sorted(a)}, 新={sorted(b)}"
        for key in a:
            result = first_difference(a[key], b[key], f"{path}.{key}")
            if result:
                return result
    elif isinstance(a, list):
        if len(a) != len(b):
            return f"{path}: 长度旧={len(a)}, 新={len(b)}"
        for index, (left, right) in enumerate(zip(a, b)):
            result = first_difference(left, right, f"{path}[{index}]")
            if result:
                return result
    elif a != b:
        return f"{path}: 旧={str(a)[:300]!r}, 新={str(b)[:300]!r}"
    return None


class Comparison:
    def __init__(self, old, new, *, root):
        self.backends = (old, new)
        self.count = 0
        self.failures = []
        self.known_differences = 0
        self.version_differences = 0
        self.proc_path_differences = 0
        self.proc_path_cases = 0
        self.allocated_differences = 0
        self.field_difference_cases = 0
        self.allocation_evidence = None
        self.root = root
        self.spellings = fixture_spellings(root) if os.name == "nt" else {}

    def check(self, label, responses, kind=None, expected=None):
        self.count += 1
        try:
            if expected is not None:
                for response in responses:
                    assert response["status"] == expected, response
            normalized = [normalize(response, kind) for response in responses]
            changed = adapt_proc_paths(*normalized, windows=os.name == "nt", spellings=self.spellings)
            allocated = 0
            if os.name == "nt" and label in AGENT_ALLOCATED_LABELS:
                if self.allocation_evidence is None:
                    self.allocation_evidence = windows_allocation_evidence(self.root)
                    print("Windows allocated 独立诊断（API合计不作为扫描期望值）："
                          + json.dumps(self.allocation_evidence, ensure_ascii=False), flush=True)
                allocated = adapt_agent_allocated_bytes(label, *normalized, windows=True,
                                                       evidence=self.allocation_evidence)
            difference = first_difference(*normalized)
            if difference:
                raise AssertionError(difference)
            self.proc_path_differences += changed
            self.proc_path_cases += bool(changed)
            self.allocated_differences += allocated
            self.field_difference_cases += bool(changed or allocated)
            if allocated:
                print(f"KNOWN {label}: 仅 body.scan.bytes/size（同一口径差异）；"
                      f"旧={responses[0]['body']['scan']['bytes']}/{responses[0]['body']['scan']['size']}，"
                      f"新={responses[1]['body']['scan']['bytes']}/{responses[1]['body']['scan']['size']}，"
                      f"apparent={self.allocation_evidence['apparent']}，"
                      f"cluster={self.allocation_evidence['cluster']}。", flush=True)
        except (AssertionError, KeyError, TypeError) as error:
            self.failures.append(f"{label}: {error}")
            print(f"FAIL {self.failures[-1]}", flush=True)

    def request(self, label, path, body=MISSING, *, kind=None, expected=None, **kwargs):
        responses = [backend.request(path, body, **kwargs) for backend in self.backends]
        self.check(label, responses, kind, expected)
        return responses

    def check_slice_note_version(self, label, responses):
        """仅用于 Python <3.12 的非空 dict note，不接受其他异常或额外响应字段。"""
        self.count += 1
        try:
            expected_errors = ('TypeError("unhashable type: \'slice\'")',
                               'KeyError(slice(None, 200, None))')
            for response, error in zip(responses, expected_errors):
                assert response["status"] == 500, response
                assert response["body"] == {"error": error}, response
            old, new = copy.deepcopy(responses)
            old["body"]["error"] = new["body"]["error"]
            difference = first_difference(old, new)
            assert difference is None, difference
        except (AssertionError, KeyError, TypeError) as error:
            self.failures.append(f"{label}: {error}")
            print(f"FAIL {self.failures[-1]}", flush=True)
        else:
            self.version_differences += 1


def fixture(root):
    root.mkdir()
    for name, size in [("readme.txt", 193), ("中文 空格+&'.log", 3127),
                       ("zero", 0), ("gone.dat", 7193)]:
        (root / name).write_bytes(b"x" * size)
    (root / "empty").mkdir()
    many = root / "many"
    many.mkdir()
    # 每个文件的 apparent/allocated 大小均不同，避免前150名的并列排序歧义。
    for index in range(1, 166):
        (many / f"entry-{index:03}.bin").write_bytes(b"a" * (index * 4096 + 17))
    deep = root / "deep"
    deep.mkdir()
    for index in range(51):
        deep = deep / "d"
        deep.mkdir()
        (deep / "payload.bin").write_bytes(b"d" * 181)
    if os.name != "nt":
        (root / "link").symlink_to("readme.txt")
        (root / "dangling").symlink_to("missing-target")
    return root


def agent_fixture(root):
    """全部文件大小不同，使AI列表的排序比较不依赖 readdir 的并列顺序。"""
    root.mkdir()
    for name in ("sub", "cache", "many", "empty"):
        (root / name).mkdir()
    for name, size in (("document.txt", 901), ("notes.log", 2077), ("zero", 0),
                       ("sub/binary.bin", 4099), ("sub/settings.ini", 3031),
                       ("cache/temp.tmp", 6173)):
        (root / name).write_bytes(b"x" * size)
    for index in range(1, 166):
        (root / "many" / f"entry-{index:03}.bin").write_bytes(b"m" * (index * 17 + 13))
    return root


def instance_check(backend):
    path = backend.cache / "instance.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    assert set(data) == {"port", "token", "pid", "version", "started"}, data
    assert data["port"] == backend.port and data["token"] == TOKEN, data
    assert data["pid"] == backend.process.pid, data
    assert isinstance(data["started"], (int, float)) and data["started"] > 0
    assert isinstance(data["version"], str) and data["version"]
    if os.name != "nt":
        assert path.stat().st_mode & 0o777 == 0o600


def signal_shutdown_checks(binary, directory):
    """仅向本脚本创建的服务子进程发信号，不向进程组或外部进程发送。"""
    if os.name != "posix":
        # Windows 的 SIGTERM 是 TerminateProcess，不能用来验证 Unix 信号退出路径。
        return 0
    checked = 0
    for sig in (signal.SIGINT, signal.SIGTERM):
        backend = Backend(f"rust-{sig.name.lower()}",
                          [str(binary.resolve()), "--port", "0", "--token", TOKEN, "--fixture"],
                          directory, new_session=True)
        try:
            instance_check(backend)
            assert backend.request("/api/status")["status"] == 200
            # 不使用 --stdin-stop：退出必须由安装好的信号处理器完成。
            backend.process.send_signal(sig)
            try:
                code = backend.process.wait(timeout=10)
            except subprocess.TimeoutExpired as error:
                raise AssertionError(f"{sig.name} 后服务未在10秒内退出") from error
            assert code == 0, f"{sig.name} 退出码={code}，stderr={''.join(backend.errors)}"
            assert not (backend.cache / "instance.json").exists(), f"{sig.name} 退出后instance仍存在"
            checked += 1
        finally:
            backend.stop()
    return checked


def run_suite(check, root):
    q = lambda path: urllib.parse.quote(str(path), safe="")
    check.request("idle", "/api/status", kind="status", expected=200)
    check.request("cancel idle", "/api/cancel", {}, kind="status", expected=200)
    for lang in ("zh", "en"):
        headers = {"X-DiskMap-Lang": lang}
        check.request(f"meta {lang}", "/api/meta", headers=headers, kind="meta", expected=200)
        check.request(f"page {lang}", f"/?t={TOKEN}", headers=headers, expected=200)
        for route in ("tree", "info"):
            check.request(f"idle {route} {lang}", f"/api/{route}", headers=headers, expected=404)
        check.request(f"bad scan {lang}", "/api/scan", {"path": str(root / "absent")},
                      headers=headers, expected=400)
        check.request(f"bad reveal {lang}", "/api/reveal", {"path": str(root / "absent")},
                      headers=headers, expected=400)
    check.request("page missing token", "/", headers={"X-DiskMap-Token": None}, expected=403)
    check.request("page wrong token", "/?t=wrong", expected=403)
    check.request("page repeated token", f"/?t={TOKEN}&t=wrong", expected=200)
    check.request("page blank token ignored", f"/?t=&t={TOKEN}", expected=200)
    check.request("page bad host", f"/?t={TOKEN}", headers={"Host": "attacker.invalid"}, expected=403)
    check.request("unknown", "/missing", expected=404)
    for route in ("meta", "status", "tree", "info", "agent/status", "agent/unknown"):
        check.request(f"missing token {route}", f"/api/{route}",
                      headers={"X-DiskMap-Token": None}, expected=403)
        check.request(f"wrong token {route}", f"/api/{route}",
                      headers={"X-DiskMap-Token": "wrong"}, expected=403)
        check.request(f"bad host {route}", f"/api/{route}",
                      headers={"Host": "localhost.evil:{port}"}, expected=403)
    for route in ("scan", "cancel", "refresh-procs", "reveal", "settings", "ui-state",
                  "agent/scan", "agent/show"):
        check.request(f"post token {route}", f"/api/{route}", {},
                      headers={"X-DiskMap-Token": "wrong"}, expected=403)
        check.request(f"post host {route}", f"/api/{route}", {},
                      headers={"Host": "attacker.invalid"}, expected=403)
    for host in ("localhost:{port}", "[::1]:{port}"):
        check.request(f"allowed host {host}", "/api/status", headers={"Host": host},
                      kind="status", expected=200)
    check.request("missing host", "/api/status", headers={"Host": None}, expected=403)
    for suffix, label in ((" ", "space"), ("\t", "tab")):
        check.request(f"host trailing {label}", "/api/status",
                      headers={"Host": "127.0.0.1:{port}" + suffix}, expected=403)
        check.request(f"token trailing {label}", "/api/status",
                      headers={"X-DiskMap-Token": TOKEN + suffix}, expected=403)
    check.request("token query rejected", f"/api/status?t={TOKEN}",
                  headers={"X-DiskMap-Token": None}, expected=403)
    check.request("unknown api", "/api/unknown", expected=404)
    check.request("unknown post", "/api/unknown", {}, expected=404)
    check.request("unknown post array", "/api/unknown", [], expected=404)
    check.request("cancel array", "/api/cancel", [], kind="status", expected=200)
    check.request("settings array", "/api/settings", [], expected=200)
    for route in ("scan", "ui-state", "reveal"):
        check.request(f"invalid array {route}", f"/api/{route}", [], expected=500)
    for name in ("index.html", "app.js", "style.css", "favicon.png"):
        check.request(f"static {name}", f"/static/{name}", expected=200)
        # 旧版明确允许静态资源不带令牌、任意Host。
        check.request(f"public static {name}", f"/static/{name}",
                      headers={"X-DiskMap-Token": None, "Host": "attacker.invalid"}, expected=200)
    for path in ("/static/../README.md", "/static/../../Cargo.toml", "/static/%2e%2e%2fREADME.md",
                 "/static/..%5cCargo.toml", "/static/no.js", "/static/.env", "/static/",
                 "/static/app%2ejs"):
        check.request(f"static rejection {path}", path, expected=404)
    check.request("static basename", "/static/../../app.js", expected=200)
    for lang in ("zh", "en"):
        check.request(f"bad frac {lang}", "/api/tree?frac=oops",
                      headers={"X-DiskMap-Lang": lang}, expected=400)
    check.request("ui state empty", "/api/ui-state", {}, expected=200)
    check.request("ui state", "/api/ui-state", {"view": str(root), "selected": str(root / "readme.txt"),
                                               "pinned": True}, expected=200)
    check.request("malformed JSON", "/api/ui-state", raw=b"{broken", expected=200)
    for lang in ("en", "zh"):
        check.request(f"save {lang}", "/api/settings", {"lang": lang}, expected=200)
        check.request(f"saved meta {lang}", "/api/meta", kind="meta", expected=200)
        for backend in check.backends:
            assert json.loads((backend.config / "settings.json").read_text(encoding="utf-8")) == {"lang": lang}
    check.request("bad language", "/api/settings", {"lang": "xx"}, expected=400)
    check.request("settings empty", "/api/settings", {}, expected=200)
    check.request("language override", "/api/settings", {"lang": "en"},
                  headers={"X-DiskMap-Lang": "zh"}, expected=200)
    check.request("restore language", "/api/settings", {"lang": "zh"}, expected=200)

    for apparent in (True, False):
        # 暂时不对不同Windows实际占用算法声称相等；这是T01/T14明示的行为变化。
        if os.name == "nt" and not apparent:
            continue
        tag = "apparent" if apparent else "allocated"
        check.request(f"scan {tag}", "/api/scan",
                      {"path": str(root), "apparent": apparent, "cross_fs": False},
                      kind="progress", expected=200)
        check.check(f"done {tag}", [backend.wait_done() for backend in check.backends], "status", 200)
        for path in (root, root / "many", root / "deep", root / "empty"):
            for frac in ("0.000025", "0", "0.01", "1", "nan", "inf", "-inf", "０.５", "1_0"):
                check.request(f"tree {tag} {path.name} {frac}", f"/api/tree?path={q(path)}&frac={q(frac)}",
                              expected=400 if frac == "nan" else 200)
        check.request(f"default tree {tag}", "/api/tree", expected=200)
        check.request(f"empty frac {tag}", "/api/tree?frac=", expected=200)
        check.request(f"default info {tag}", "/api/info", expected=404)
        check.request(f"normalized tree {tag}", f"/api/tree?path={q(str(root) + '/deep/..')}", expected=200)
        for name in ("", "readme.txt", "中文 空格+&'.log", "zero", "empty", "many", "many/entry-165.bin"):
            for lang in ("zh", "en"):
                check.request(f"info {tag} {name} {lang}", f"/api/info?path={q(root / name)}",
                              headers={"X-DiskMap-Lang": lang}, expected=200)
        if os.name != "nt":
            for name in ("link", "dangling"):
                check.request(f"symlink {tag} {name}", f"/api/info?path={q(root / name)}", expected=200)
        for route in ("tree", "info"):
            check.request(f"missing {tag} {route}", f"/api/{route}?path={q(root / 'absent')}", expected=404)
            check.request(f"outside {tag} {route}", f"/api/{route}?path={q(root.parent)}", expected=404)
        check.request(f"file tree {tag}", f"/api/tree?path={q(root / 'readme.txt')}", expected=404)
        check.request(f"cancel done {tag}", "/api/cancel", {}, kind="status", expected=200)

        # T01 已明确保留完整索引：旧版丢掉小文件详情，新版仍然可以查到。
        hidden = f"/api/info?path={q(root / 'many/entry-001.bin')}"
        old, new = [backend.request(hidden) for backend in check.backends]
        assert old["status"] == 404, old
        assert new["status"] == 200 and new["body"]["name"] == "entry-001.bin", new
        assert new["body"]["size"] == (4113 if apparent else (root / "many/entry-001.bin").stat().st_blocks * 512)
        check.known_differences += 1

    (root / "gone.dat").unlink()
    check.request("gone file", f"/api/info?path={q(root / 'gone.dat')}", expected=200)
    check.request("gone reveal", "/api/reveal", {"path": str(root / "gone.dat")}, expected=400)
    check.request("rescan", "/api/scan", {"path": str(root), "apparent": True},
                  kind="progress", expected=200)
    check.request("cancel active", "/api/cancel", {}, kind="progress", expected=200)
    # 取消发生点由调度决定，逐个验证终止状态，不比较保留下来的目录数。
    for backend in check.backends:
        response = backend.wait_done()
        assert response["body"]["state"] in ("done", "cancelled"), response
    check.request("final scan", "/api/scan", {"path": str(root), "apparent": True},
                  kind="progress", expected=200)
    check.check("final done", [backend.wait_done() for backend in check.backends], "status", 200)
    # 显式刷新最后执行；进程总数/可见数/文件数随进程和后端本身变化，只比较结构。
    check.request("refresh procs", "/api/refresh-procs", {}, kind="refresh", expected=200)
    for path, headers, status in (("/api/bye", {}, 403),
                                  (f"/api/bye?t={TOKEN}", {"Host": "attacker.invalid"}, 403),
                                  (f"/api/bye?t={TOKEN}", {"X-DiskMap-Token": None}, 204)):
        check.request(f"bye {status} {headers}", path, method="POST", headers=headers, expected=status)


def run_agent_suite(check, root):
    def url(route, **params):
        return "/api/agent/" + route + ("?" + urllib.parse.urlencode(params) if params else "")

    def disconnected(label):
        responses = check.request(label, url("status"), kind="agent_status", expected=200)
        for response in responses:
            assert response["body"]["window_connected"] is False, response

    def finished(label):
        check.check(label, [backend.wait_agent_done() for backend in check.backends], "agent_status", 200)

    def scan(label, body, reused):
        responses = check.request(label, url("scan"), body, kind="agent_progress", expected=200)
        for response in responses:
            assert response["body"]["reused"] is reused, response
        finished(label + " done")

    def command(label, expected_id, path, note):
        responses = check.request(label, "/api/status", kind="agent_app_status", expected=200)
        for response in responses:
            value = response["body"]["command"]
            assert {key: value[key] for key in ("id", "action", "path", "note")} == {
                "id": expected_id, "action": "show", "path": str(path), "note": note}, value
            assert type(value["time"]) in (int, float) and value["time"] > 0
        return responses

    # 这里此前没有任何普通界面请求，AI status/错误/扫描均不能冒充界面心跳。
    disconnected("agent idle disconnected")
    for route in ("status", "summary", "list", "explain", "cleanup", "largest", "view"):
        check.request(f"agent GET token {route}", url(route),
                      headers={"X-DiskMap-Token": "wrong"}, expected=403)
        check.request(f"agent GET host {route}", url(route),
                      headers={"Host": "attacker.invalid"}, expected=403)
    for lang in ("zh", "en"):
        headers = {"X-DiskMap-Lang": lang}
        for route in ("status", "summary", "list", "explain", "cleanup", "largest", "view"):
            check.request(f"agent idle {route} {lang}", url(route), headers=headers,
                          kind="agent_status" if route == "status" else None,
                          expected=200 if route == "status" else 400)
        check.request(f"agent idle show {lang}", url("show"), {"path": str(root)},
                      headers=headers, expected=400)
        for route, params in (("summary", {"top": "oops"}), ("list", {"limit": "oops"}),
                              ("largest", {"limit": "oops", "category": "unknown"}),
                              ("cleanup", {"min_mb": "oops"})):
            check.request(f"agent idle error precedence {route} {lang}", url(route, **params),
                          headers=headers, expected=400)
        check.request(f"agent bad scan {lang}", url("scan"), {"path": str(root / "missing")},
                      headers=headers, expected=400)
    check.request("agent unknown GET", url("unknown"), expected=404)
    check.request("agent unknown POST", url("unknown"), {}, expected=404)
    disconnected("agent errors do not connect UI")
    scan("agent first scan", {"path": str(root), "apparent": True, "same_fs": True}, False)
    disconnected("agent scan does not connect UI")

    # 参数裁剪、错误优先级和数值转换均由HTTP实际处理，不直接调用分析函数。
    for lang in ("zh", "en"):
        headers = {"X-DiskMap-Lang": lang}
        for route in ("status", "summary", "list", "cleanup", "largest"):
            check.request(f"agent default {route} {lang}", url(route), headers=headers,
                          kind="agent_status" if route == "status" else None, expected=200)
        for relative in ("", "sub", "document.txt", "zero"):
            check.request(f"agent explain {relative} {lang}", url("explain", path=str(root / relative)),
                          headers=headers, expected=200)
        for route, key in (("summary", "top"), ("list", "limit"), ("cleanup", "limit"),
                           ("largest", "limit")):
            for value in ("0", "-5", "1", "1000", "+2", "2_0", "9999999999999999999999999"):
                check.request(f"agent numeric {route} {value} {lang}", url(route, **{key: value}),
                              headers=headers, expected=200)
            for value in ("oops", "1.5"):
                check.request(f"agent invalid numeric {route} {value} {lang}", url(route, **{key: value}),
                              headers=headers, expected=400)
            for relative in ("missing", "document.txt"):
                check.request(f"agent invalid dir {route} {relative} {lang}",
                              url(route, path=str(root / relative)), headers=headers, expected=400)
        for value in ("0", "-1", "0.001", "1000000000000000000000000"):
            check.request(f"agent min_mb {value} {lang}", url("cleanup", min_mb=value, limit=200),
                          headers=headers, expected=200)
        for route, params in (("summary", {"top": "１２"}), ("list", {"limit": "٢"}),
                              ("cleanup", {"min_mb": "０.５"}), ("largest", {"limit": "１_２"})):
            check.request(f"agent Unicode number {route} {lang}", url(route, **params),
                          headers=headers, expected=200)
        for value, status in (("oops", 400), ("nan", 400), ("inf", 500), ("-inf", 500)):
            check.request(f"agent invalid min_mb {value} {lang}", url("cleanup", min_mb=value),
                          headers=headers, expected=status)
        for category in ("core", "running", "system", "config", "user", "cache", "other", ""):
            check.request(f"agent category {category} {lang}", url("largest", category=category, limit=200),
                          headers=headers, expected=200)
        check.request(f"agent invalid category {lang}", url("largest", category="unknown"),
                      headers=headers, expected=400)
        for route in ("summary", "list", "cleanup", "largest"):
            check.request(f"agent dense {route} {lang}", url(route, path=str(root / "many")),
                          headers=headers, expected=200)
        check.request(f"agent missing explain {lang}", url("explain", path=str(root / "missing")),
                      headers=headers, expected=400)
        check.request(f"agent missing view {lang}", url("view"), headers=headers, expected=400)
    disconnected("agent all reads do not connect UI")

    # show在没有界面时仍登记；读取普通status后才把窗口视为连接。
    note = "中🙂e\u0301" * 70
    responses = check.request("agent disconnected show", url("show"),
                              {"path": str(root / "document.txt"), "note": note}, expected=200)
    for response in responses:
        assert response["body"]["window_connected"] is False, response
    disconnected("agent show does not connect UI")
    command("agent first command", 1, root / "document.txt", note[:200])
    check.request("agent connected status", url("status"), kind="agent_status", expected=200)

    identifier = 1
    for lang in ("zh", "en"):
        headers = {"X-DiskMap-Lang": lang}
        for value in (None, "短备注 / short note", 0, ["中", "en"] * 125):
            identifier += 1
            check.request(f"agent connected show {identifier} {lang}", url("show"),
                          {"path": str(root / "sub"), "note": value}, headers=headers, expected=200)
            expected_note = (value or "")[:200]
            command(f"agent command id {identifier}", identifier, root / "sub", expected_note)
        for body, status in (({"path": str(root / "missing"), "note": "invalid"}, 400),
                             ({"path": str(root / "sub"), "note": 123}, 500),
                             ({"path": str(root / "sub"), "note": {"x": 1}}, 500),
                             ({"path": 123}, 400), ({"path": None}, 400), ({}, 400), ([], 500)):
            label = f"agent show rejected {body} {lang}"
            if sys.version_info < (3, 12) and body == {"path": str(root / "sub"), "note": {"x": 1}}:
                responses = [backend.request(url("show"), body, headers=headers) for backend in check.backends]
                check.check_slice_note_version(label, responses)
            else:
                check.request(label, url("show"), body, headers=headers, expected=status)
        command(f"agent rejected shows preserve id {lang}", identifier, root / "sub", expected_note)
        for pinned in ("cache", "running", None, 5, {"custom": "value"}):
            state = {"view": str(root), "selected": str(root / "document.txt"), "pinned": pinned}
            check.request(f"agent set UI {pinned} {lang}", "/api/ui-state", state, expected=200)
            responses = check.request(f"agent view {pinned} {lang}", url("view"), headers=headers,
                                      kind="agent_view", expected=200)
            for response in responses:
                assert response["body"]["highlighted_category"] == pinned, response
                assert response["body"]["selected"] == state["selected"], response
                assert response["body"]["view"] == state["view"], response
                assert "view_summary" in response["body"] and "selected_detail" in response["body"]
        for state in ({"view": str(root / "missing"), "selected": str(root / "missing"), "pinned": "user"},
                      {"view": "", "selected": "", "pinned": None}, {}):
            check.request(f"agent stale UI state {state} {lang}", "/api/ui-state", state, expected=200)
            responses = check.request(f"agent stale view {state} {lang}", url("view"), headers=headers,
                                      kind="agent_view", expected=200)
            for response in responses:
                assert "view_summary" not in response["body"] and "selected_detail" not in response["body"]
        for view, selected in ((0, False), (False, []), ([], 0), (True, 42),
                               (42, True), ([1], ["relative", 7])):
            state = {"view": view, "selected": selected, "pinned": "cache"}
            label = f"agent raw UI {state} {lang}"
            check.request(label + " set", "/api/ui-state", state, expected=200)
            responses = check.request(label + " view", url("view"), headers=headers,
                                      kind="agent_view", expected=200)
            for response in responses:
                assert response["body"]["view"] == view and response["body"]["selected"] == selected
                assert "view_summary" not in response["body"] and "selected_detail" not in response["body"]
            responses = check.request(label + " status", url("status"), headers=headers,
                                      kind="agent_status", expected=200)
            for response in responses:
                assert response["body"]["user_view"] == {"view": view, "selected": selected}
        check.request(f"agent status UI {lang}", url("status"), headers=headers,
                      kind="agent_status", expected=200)

    # 成功完成后检查复用矩阵，避免把线程调度差异当成复用行为差异。
    for label, body, reused in (
        ("reuse root", {"path": str(root), "apparent": True, "same_fs": True}, True),
        ("reuse nested", {"path": str(root / "sub"), "apparent": True, "same_fs": True}, True),
        ("reuse file", {"path": str(root / "document.txt"), "apparent": True, "same_fs": True}, True),
        ("widen cross_fs", {"path": str(root), "apparent": True, "same_fs": False}, False),
        ("reuse narrower", {"path": str(root), "apparent": True, "same_fs": True}, True),
        ("forced rescan", {"path": str(root), "apparent": True, "same_fs": True, "rescan": True}, False),
        ("switch allocated", {"path": str(root), "apparent": False, "same_fs": True}, False),
        ("reuse default apparent", {"path": str(root), "same_fs": True}, True),
        ("restore apparent", {"path": str(root), "apparent": True, "same_fs": True}, False),
        ("truthy same_fs", {"path": str(root), "apparent": True, "same_fs": "yes", "rescan": 0}, True),
        ("truthy rescan", {"path": str(root), "apparent": True, "same_fs": True, "rescan": "yes"}, False),
    ):
        scan("agent " + label, body, reused)
    for lang in ("zh", "en"):
        headers = {"X-DiskMap-Lang": lang}
        for body, status in (({"path": str(root / "missing"), "apparent": True, "same_fs": True}, 400),
                             ({"path": 42}, 400), ([], 500)):
            check.request(f"agent scan rejected {body} {lang}", url("scan"), body,
                          headers=headers, expected=status)
        check.request(f"agent scan preserved {lang}", url("status"), headers=headers,
                      kind="agent_status", expected=200)


def main():
    # 包括 legacy 子模式；所有重定向日志均为 UTF-8，编解码错误仍直接失败。
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="strict")
    parser = argparse.ArgumentParser(description=__doc__)
    binary_name = "serve.exe" if os.name == "nt" else "serve"
    parser.add_argument("--binary", type=Path, default=REPO / "target/debug/examples" / binary_name)
    parser.add_argument("--legacy-server", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.legacy_server:
        legacy_server()
        return
    print(f"旧版解释器：Python {sys.version.split()[0]} ({sys.executable})；子进程 stdin/stdout UTF-8。", flush=True)
    if not args.binary.is_file():
        parser.error("请先 cargo build -p diskmap-core --features server --example serve")
    with tempfile.TemporaryDirectory(prefix="diskmap-t05-") as directory:
        directory = Path(directory)
        root = fixture(directory / "fixture")
        backends = []
        with ExitStack() as stack:
            backends.append(Backend("legacy", [sys.executable, "-B", str(Path(__file__).resolve()),
                                               "--legacy-server"], directory))
            stack.callback(backends[-1].stop)
            backends.append(Backend("rust", [str(args.binary.resolve()), "--port", "0", "--token", TOKEN,
                                             "--fixture", "--stdin-stop"], directory))
            stack.callback(backends[-1].stop)
            for backend in backends:
                instance_check(backend)
            check = Comparison(*backends, root=root)
            run_suite(check, root)
        for backend in backends:
            assert not (backend.cache / "instance.json").exists(), f"{backend.name} 退出后未删除instance"
        agent_root = agent_fixture(directory / "agent-fixture")
        agent_backends = []
        with ExitStack() as stack:
            agent_backends.append(Backend("legacy-agent", [sys.executable, "-B", str(Path(__file__).resolve()),
                                                            "--legacy-server"], directory))
            stack.callback(agent_backends[-1].stop)
            agent_backends.append(Backend("rust-agent", [str(args.binary.resolve()), "--port", "0", "--token", TOKEN,
                                                         "--fixture", "--stdin-stop"], directory))
            stack.callback(agent_backends[-1].stop)
            agent_check = Comparison(*agent_backends, root=agent_root)
            run_agent_suite(agent_check, agent_root)
        for backend in agent_backends:
            assert not (backend.cache / "instance.json").exists(), f"{backend.name} 退出后未删除instance"
        signals = signal_shutdown_checks(args.binary, directory)
        print(f"T05：{check.count} 组接口对比，{len(check.failures)} 组不一致；实例格式/0600/退出清理通过。")
        print(f"AI：{agent_check.count} 组接口对比，{len(agent_check.failures)} 组未解释差异；"
              "7个GET与scan/show、双语错误、扫描复用、UI/高亮、命令编号/备注截断均已覆盖。")
        total = check.count + agent_check.count
        failures = len(check.failures) + len(agent_check.failures)
        version_differences = agent_check.version_differences
        field_cases = check.field_difference_cases + agent_check.field_difference_cases
        allocated_cases = check.allocated_differences + agent_check.allocated_differences
        print(f"合计：{total - failures - version_differences - field_cases} 组一致，"
              f"{field_cases} 组已核验字段差异，"
              f"{version_differences} 组已核验的 Python 版本差异，{failures} 组未解释差异。")
        print(f"Windows procinfo路径已知差异：{check.proc_path_cases + agent_check.proc_path_cases} 组、"
              f"{check.proc_path_differences + agent_check.proc_path_differences} 个字段；"
              "仅summary.in_use.largest[*].path，Rust真实拼写另行核验。")
        print(f"Windows allocated已知差异：{allocated_cases} 组；仅两个指定完成状态的scan.bytes/size，"
              "旧字节数按实际簇核验，新字节数检查范围，两侧size严格等于旧版human(各自bytes)；"
              "每组仅计数一次，其余字段严格检查。")
        if version_differences:
            assert version_differences == 2
            print("版本差异仅限双语 agent/show 字典note：旧Python <3.12为TypeError(unhashable slice)，"
                  "新版固定为KeyError(slice(None, 200, None))；两者均500，命令状态及其余字段严格核验。")
        if signals:
            print("新服务 SIGINT/SIGTERM 两种退出均返回0，instance清理通过（仅向本脚本创建的子进程发送）。")
        else:
            print("当前平台不支持Unix信号测试；普通退出的instance清理已验证。")
        print(f"另验证 {check.known_differences} 组已知差别：旧版150项外文件详情404，新版完整索引200。")
        print("覆盖：首页/全部静态资源/meta/status/tree/info/scan/cancel/refresh-procs/reveal/settings/ui-state/bye；"
              "Host/令牌/穿越/中英/150项聚合/深度/空目录/链接/消失文件。")
        print("归一化：版本和命令、实时磁盘空间、elapsed/fs_used、最后显式刷新的进程计数、树同大小排序；"
              "扫描/取消瞬间只核对结构。")
        print("AI另归一化：完成时间/耗时、UI更新时间、command.time、agent_seen_ago；其余字段逐项比较。")
        if check.failures or agent_check.failures:
            raise SystemExit(1)


if __name__ == "__main__":
    main()
