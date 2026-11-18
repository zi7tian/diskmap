#!/usr/bin/env python3
"""T07 CLI 黑盒新旧对比；先 cargo build -p diskmap-cli，再运行本文件。

使用真实 legacy/diskmap.py mcp/analyze 与 diskmap-cli 子进程，扫描相同临时树。
覆盖中英、四协议、窗口/后台、全部工具正常及错误参数、协议错误与 JSON/Markdown 报告。
只归一化版本、扫描时间、实时磁盘空间、全局进程计数和 UI 更新时间；不删分析字段。
MCP 文本内容先验证等于 structuredContent，再按 JSON 比较（忽略对象键顺序/缩进）。
所有 cache/config、报告、实例登记及输入数据都位于本脚本创建的临时目录。
Linux 测试期间只读打开自建树的全部目录，让新旧进程看到相同的目录 FD 集合，
避免异步进程采集偶然捕获扫描器的短暂目录 FD；保留全部 in_use 字段和建议。
"""

from __future__ import annotations

import argparse
import contextlib
import copy
import http.client
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import time
import urllib.parse

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "crates/diskmap-core/tests"))
from compare_support.process import ProcessOwner, run_process
from compare_support.fields import adapt_proc_paths, fixture_spellings
PROTOCOLS = ("2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05")
TOKEN = "cli-compare-token"
MISSING = object()


def environment(base, language):
    env = dict(os.environ, DISKMAP_CACHE_DIR=str(base / "cache"),
               DISKMAP_CONFIG_DIR=str(base / "config"), DISKMAP_LANG=language,
               DISKMAP_TOKEN=TOKEN, DISKMAP_PORT="0", PYTHONDONTWRITEBYTECODE="1",
               PYTHONUNBUFFERED="1", PYTHONUTF8="1", PYTHONIOENCODING="utf-8")
    env.pop("DISKMAP_NO_ATTACH", None)
    return env


class Process:
    def __init__(self, command, env):
        self.owner = ProcessOwner(command, cwd=REPO, env=env)
        self.process = self.owner.process
        self.errors = self.owner.errors

    def line(self, timeout=90):
        return self.owner.readline(timeout)

    def send(self, line):
        self.process.stdin.write(line + "\n")
        self.process.stdin.flush()

    def stop(self):
        try:
            if not self.process.stdin.closed:
                self.process.stdin.close()
        except (OSError, ValueError):
            pass
        finally:
            self.owner.close(grace=12 if "mcp" in self.process.args else 0)


class Mcp(Process):
    def __init__(self, command, env, attach):
        super().__init__(command + ["mcp"] + ([] if attach else ["--no-attach"]), env)
        self.serial = 0

    def rpc(self, method, params=MISSING):
        self.serial += 1
        message = {"jsonrpc": "2.0", "id": self.serial, "method": method}
        if params is not MISSING:
            message["params"] = params
        self.send(json.dumps(message, ensure_ascii=False))
        response = json.loads(self.line())
        assert response["jsonrpc"] == "2.0" and response["id"] == self.serial, response
        return response

    def call(self, name, args=MISSING):
        params = {"name": name}
        if args is not MISSING:
            params["arguments"] = args
        return self.rpc("tools/call", params)

    def ready(self):
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            response = self.call("diskmap_status")
            data = response["result"].get("structuredContent")
            if data and data["ready"]:
                return
            time.sleep(0.05)
        raise AssertionError("后台初始化超时")


class Window(Process):
    def __init__(self, command, env, new):
        super().__init__(command + (["serve"] if new else []) + ["--no-open"], env)
        try:
            self.wait_ready()
        except BaseException:
            self.stop()
            raise

    def wait_ready(self):
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            line = self.line()
            found = re.search(r"http://127\.0\.0\.1:[0-9]+/\?t=[^\s]+", line)
            if found:
                self.port = urllib.parse.urlsplit(found.group()).port
                break
        else:
            raise AssertionError("窗口未输出链接")
        while time.monotonic() < deadline:
            if self.request("/api/agent/status")["ready"]:
                return
            time.sleep(0.05)
        raise AssertionError("窗口初始化超时")

    def request(self, path, body=MISSING):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=90)
        headers = {"X-DiskMap-Token": TOKEN, "Content-Type": "application/json"}
        encoded = None if body is MISSING else json.dumps(body).encode()
        try:
            connection.request("GET" if body is MISSING else "POST", path, encoded, headers)
            response = connection.getresponse()
            data = response.read()
            assert response.status == 200, (response.status, data)
            return json.loads(data)
        finally:
            connection.close()

    def view(self, root, missing=False):
        self.request("/api/ui-state", {"view": str(root / ("missing" if missing else "proj")),
                                      "selected": str(root / "proj/Build.log"), "pinned": "cache"})


def make_tree(root):
    # 大小均不同，避免旧版枚举顺序与新版按名字排序引起平局。
    sizes = {"docs/Report.pdf": 400_001, "proj/cache/blob.bin": 2_500_003,
             "proj/__pycache__/m.pyc": 1_200_005, "proj/Build.log": 1_100_007,
             "proj/main.py": 3_009, "中文 空格.txt": 41_011, "empty/zero": 0}
    for relative, size in sizes.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"x" * size)
    return sum(sizes.values())


def normalize(value):
    value = copy.deepcopy(value)
    if isinstance(value, dict):
        if "structuredContent" in value:
            assert value["isError"] is False, value
            assert value["content"][0]["type"] == "text", value
            assert len(value["content"]) == 1, value
            text = json.loads(value["content"][0]["text"])
            assert text == value["structuredContent"], "MCP text 和 structuredContent 不一致"
            value["content"][0]["text"] = text
        for key, item in value.items():
            if key == "version":
                assert isinstance(item, str) and item
                value[key] = "<版本>"
            elif key == "processes" and isinstance(item, dict):
                assert set(item) == {"total", "in_use_files", "visible_processes"}, item
                assert all(type(number) is int and number >= 0 for number in item.values()), item
                value[key] = {field: "<全局进程统计>" for field in item}
            elif key in ("elapsed_seconds", "updated_seconds_ago"):
                assert type(item) in (int, float) and item >= 0, (key, item)
                value[key] = "<耗时>"
            elif key == "finished_at" and item is not None:
                time.strptime(item, "%Y-%m-%d %H:%M:%S")
                value[key] = "<完成时间>"
            elif key == "disk_used_bytes" and item is not None:
                assert type(item) is int and item >= 0
                value[key] = "<磁盘实时占用>"
            else:
                value[key] = normalize(item)
    elif isinstance(value, list):
        value = [normalize(item) for item in value]
    return value


def markdown(text):
    text, count = re.subn(r"(?m)^(- 扫描：)[^，]+，用时 [0-9.]+ 秒(，按.*)$", r"\1<时间>，用时 <耗时> 秒\2", text)
    if not count:
        text, count = re.subn(r"(?m)^(- Scan: )[^,]+, took [0-9.]+ s(, measured by .*)$", r"\1<时间>, took <耗时> s\2", text)
    assert count == 1, text
    return text


class Checks:
    def __init__(self):
        self.total = 0
        self.failures = []
        self.proc_path_differences = 0
        self.proc_path_cases = 0
        self.spellings = {}

    def same(self, label, old, new, transform=normalize):
        self.total += 1
        try:
            left, right = transform(old), transform(new)
            changed = adapt_proc_paths(left, right, windows=os.name == "nt", spellings=self.spellings)
            if left != right:
                import difflib
                dump = lambda value: json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True).splitlines()
                difference = "\n".join(difflib.unified_diff(dump(left), dump(right), fromfile="旧版", tofile="新版", n=2))
                self.failures.append(label)
                print(f"FAIL {label}\n{difference[:6000]}", flush=True)
            else:
                self.proc_path_differences += changed
                self.proc_path_cases += bool(changed)
        except Exception as error:
            self.failures.append(label)
            print(f"FAIL {label}: {error}", flush=True)


def compare_protocol(checks, clients, label, version):
    def rpc(method, params=MISSING):
        results = [client.rpc(method, params) for client in clients]
        checks.same(label + "/" + method, *results)
        return results
    results = rpc("initialize", {"protocolVersion": version})
    for result in results:
        assert result["result"]["protocolVersion"] == version, result
    rpc("tools/list")
    for method in ("ping", "resources/list", "prompts/list", "resources/templates/list", "unknown/method"):
        rpc(method)
    rpc("tools/call", {"name": "unknown_tool"})
    for client in clients:
        client.send('{"jsonrpc":"2.0","method":"notifications/initialized"}')
        client.send('{"jsonrpc":"2.0","id":"response","result":{}}')
    rpc("ping")
    for client in clients:
        client.send("{invalid")
    checks.same(label + "/parse-error", *[json.loads(client.line()) for client in clients])
    for client in clients:
        client.send('{"jsonrpc":"1.0","id":"bad","method":"ping"}')
    checks.same(label + "/invalid-request", *[json.loads(client.line()) for client in clients])
    for client in clients:
        client.send('[{"jsonrpc":"2.0","id":"batch-a","method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":"batch-b","method":"ping"}]')
    results = [sorted([json.loads(client.line()), json.loads(client.line())], key=lambda item: item["id"]) for client in clients]
    checks.same(label + "/batch", *results)


def compare_tools(checks, clients, windows, root, label, total):
    def call(name, args=MISSING, suffix=""):
        responses = [client.call(name, args) for client in clients]
        checks.same(label + "/" + name + suffix, *responses)
        return responses
    call("diskmap_status")
    results = call("diskmap_scan", {"path": str(root), "apparent": True, "same_fs": True,
                                   "wait_seconds": 30, "rescan": True})
    for result in results:
        data = result["result"]["structuredContent"]
        assert data["summary"]["bytes"] == total, data
        assert data["connected_to"] == ("window" if windows else "background"), data
    call("diskmap_status", {"ignored": True}, "/ignored-argument")
    cases = [
        ("diskmap_summary", {"path": str(root), "top": 3}),
        ("diskmap_list", {"path": str(root), "limit": 40}),
        ("diskmap_explain", {"path": str(root / "docs/Report.pdf")}),
        ("diskmap_cleanup", {"path": str(root), "limit": 30, "min_mb": 0}),
        ("diskmap_largest_files", {"path": str(root), "limit": 20}),
        ("diskmap_largest_files", {"category": "cache", "limit": 2}),
        ("diskmap_user_view", {}),
        ("diskmap_show", {"path": str(root / "docs"), "note": "这里是文档 / Documents"}),
    ]
    if windows:
        for window in windows:
            window.view(root)
    for name, args in cases:
        call(name, args)
    call("diskmap_scan", {"path": str(root / "proj"), "apparent": True, "same_fs": True}, "/reuse")
    errors = [
        ("diskmap_scan", {"path": str(root / "missing")}),
        ("diskmap_summary", {"path": str(root / "missing")}),
        ("diskmap_summary", {"top": "oops"}),
        ("diskmap_list", {"path": str(root), "limit": "oops"}),
        ("diskmap_list", {"path": str(root / "docs/Report.pdf")}),
        ("diskmap_explain", {"path": str(root / "missing")}),
        ("diskmap_explain", {}),
        ("diskmap_cleanup", {"min_mb": "oops"}),
        ("diskmap_cleanup", {"limit": "oops"}),
        ("diskmap_largest_files", {"category": "nonsense"}),
        ("diskmap_largest_files", {"limit": "oops"}),
        ("diskmap_show", {"path": str(root / "missing")}),
        ("diskmap_show", {}),
        # 旧版无参数工具忽略多余参数，不强制按 schema 拒绝。
        ("diskmap_status", ["invalid"]),
        ("diskmap_user_view", ["invalid"]),
    ]
    for number, (name, args) in enumerate(errors):
        call(name, args, f"/error-{number}")
    for name in ("diskmap_summary", "diskmap_list", "diskmap_cleanup", "diskmap_largest_files"):
        args = {"path": str(root), "top": -1, "limit": -1}
        call(name, args, "/clamped")
    if windows:
        for window in windows:
            window.view(root, missing=True)
        call("diskmap_user_view", {}, "/missing-view")
        for window in windows:
            window.view(root)


def compare_analyze(checks, commands, envs, root, temporary, language):
    for apparent in (False, True):
        for as_json in (False, True):
            for output_file in (False, True):
                results = []
                for index, (command, env) in enumerate(zip(commands, envs)):
                    args = ["analyze", str(root), "--top", "3", "--same-fs", "-q", "--lang", language]
                    if apparent:
                        args.append("--apparent")
                    if as_json:
                        args.append("--json")
                    output = temporary / f"report-{index}.txt"
                    if output_file:
                        args.extend(["-o", str(output)])
                    result = run_process(command + args, cwd=REPO, env=env, timeout=120)
                    assert result.returncode == 0, (command, result.stderr)
                    assert result.stderr == "", result.stderr
                    if output_file:
                        assert result.stdout == "", result.stdout
                    text = output.read_text(encoding="utf-8") if output_file else result.stdout
                    results.append(json.loads(text) if as_json else text)
                label = f"{language}/analyze/{'apparent' if apparent else 'allocated'}/{'json' if as_json else 'markdown'}/{'file' if output_file else 'stdout'}"
                checks.same(label, *results, transform=normalize if as_json else markdown)
    for top in ("9" * 100, "-" + "9" * 100, "٣", "３", "1_0"):
        reports = []
        for command, env in zip(commands, envs):
            result = run_process(command + ["analyze", str(root), "--json", "--apparent", "-q", "--top", top],
                                 cwd=REPO, env=env, timeout=120)
            assert result.returncode == 0, (top, result.stderr)
            reports.append(json.loads(result.stdout))
        checks.same(f"{language}/analyze/numeric-top/{top}", *reports)
    for args in (["--top", "oops"], ["--lang", "xx"], [str(root / "missing")], ["--unknown"]):
        results = [run_process(command + ["analyze"] + args, cwd=REPO, env=env, timeout=120)
                   for command, env in zip(commands, envs)]
        checks.same(f"{language}/analyze/exit/{args}", results[0].returncode, results[1].returncode)
        for result in results:
            assert not result.stdout and result.stderr, result


def main():
    # 重定向到 SSH/PowerShell 日志时也固定 UTF-8，不沿用中文 Windows 的 GBK。
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="strict")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPO / "target/debug" / ("diskmap-cli.exe" if os.name == "nt" else "diskmap-cli"))
    args = parser.parse_args()
    print(f"旧版解释器：Python {sys.version.split()[0]} ({sys.executable})；子进程 stdin/stdout UTF-8。", flush=True)
    commands = [[sys.executable, "-B", str(REPO / "legacy/diskmap.py")], [str(args.binary.resolve())]]
    checks = Checks()
    with tempfile.TemporaryDirectory(prefix="diskmap-cli-compare-") as directory, contextlib.ExitStack() as handles:
        temporary = Path(directory)
        root = temporary / "data"
        total = make_tree(root)
        checks.spellings = fixture_spellings(root) if os.name == "nt" else {}
        if sys.platform.startswith("linux"):
            # procinfo 按唯一绝对路径计数；扫描器再次打开目录不会改变这个稳定集合。
            for path in [root] + [item for item in root.rglob("*") if item.is_dir()]:
                handles.callback(os.close, os.open(path, os.O_RDONLY | os.O_DIRECTORY))
        for language in ("zh", "en"):
            envs = [environment(temporary / language / version, language) for version in ("old", "new")]
            compare_analyze(checks, commands, envs, root, temporary, language)
            for mode in ("background", "window"):
                with contextlib.ExitStack() as stack:
                    windows = []
                    if mode == "window":
                        for index, (command, env) in enumerate(zip(commands, envs)):
                            window = Window(command, env, new=bool(index))
                            stack.callback(window.stop)
                            windows.append(window)
                    for version in PROTOCOLS:
                        with contextlib.ExitStack() as protocol_stack:
                            clients = []
                            for command, env in zip(commands, envs):
                                client = Mcp(command, env, attach=bool(windows))
                                protocol_stack.callback(client.stop)
                                client.ready()
                                client.serial = 0
                                clients.append(client)
                            label = f"{language}/{mode}/{version}"
                            compare_protocol(checks, clients, label, version)
                            compare_tools(checks, clients, windows, root, label, total)
                            print(f"完成 {label}（累计 {checks.total} 组）", flush=True)
        print(f"共 {checks.total} 组对比：{checks.total - len(checks.failures) - checks.proc_path_cases} 组全等，"
              f"{checks.proc_path_cases} 组仅已知Windows proc路径差异，{len(checks.failures)} 组未解释差异。")
        print(f"Windows procinfo路径已知差异：{checks.proc_path_differences} 个字段；"
              "仅summary.in_use.largest[*].path，索引路径和Markdown仍逐字比较。")
        if checks.failures:
            print("差异：" + ", ".join(checks.failures))
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
