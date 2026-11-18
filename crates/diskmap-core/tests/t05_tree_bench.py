#!/usr/bin/env python3
"""顺序测量一个 serve 二进制的 /api/tree；目标目录只读，cache/config 放在临时目录。

旧无缓存与新缓存分别运行，不能同时扫描同一大目录：
  python3 -B crates/diskmap-core/tests/t05_tree_bench.py \
    --binary /tmp/diskmap-t05-before/serve-release --label before-release --cache-mode off --root /
  python3 -B crates/diskmap-core/tests/t05_tree_bench.py \
    --binary target/release/examples/serve --label after-release --cache-mode on --root /

默认两个 frac 为 1.67e-5、1e-6，每阶段测 7 次，输出 JSON 到标准输出、进度到标准错误。
first_request记录预热之前的单次请求；只有首个frac的这一次是扫描后的首次tree请求。
repeat 阶段先预热同一请求；cache-mode=on 时，每次 miss 前请求9个不同的相邻浮点 frac，
逐出最多8条的缓存。off 表示旧无缓存版本，repeat/miss 两阶段都不是真正命中缓存。
各阶段响应逐字节比较；不把节点顺序、字段或数字差别归一化掉。
可选 --cold-repeat N：常规测量后，每个frac分别重新扫描N次，等完成/进程刷新停止后只请求一次树，
记录投影与响应缓存均未预热的冷启动中位数及每次扫描规模；活动磁盘重扫之间不比较响应字节。

HTTP全程包含连接、accept轮询、服务生成/序列化、发送和接收，不含客户端JSON解析。
Server-Timing: tree;dur=<毫秒> 单独记录服务内时间（App.tree_bytes至得到响应字节，排除socket写出）。
旧二进制没有此头时服务内时间为null，绝不从HTTP时间减去固定20ms来冒充服务内时间。
另测 /api/status HTTP中位数作为调度/传输参考；也不拿它相减。
"""

from __future__ import annotations

import argparse
from collections import deque
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import platform
import queue
import re
import secrets
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse


REPO = Path(__file__).resolve().parents[3]
DEFAULT_FRACS = (1.67e-5, 1e-6)


def server_timing(value):
    if value is None:
        return None
    for metric in value.split(","):
        parts = metric.strip().split(";")
        if parts[0].strip() != "tree":
            continue
        for part in parts[1:]:
            key, separator, duration = part.strip().partition("=")
            if key == "dur" and separator:
                milliseconds = float(duration.strip('"'))
                if not math.isfinite(milliseconds) or milliseconds < 0:
                    raise ValueError(f"无效服务计时 / Invalid Server-Timing: {value}")
                return milliseconds
    raise ValueError(f"缺少 tree;dur / Missing tree;dur: {value}")


class Service:
    def __init__(self, binary, directory, lang, timeout):
        self.cache = directory / "cache"
        self.config = directory / "config"
        self.token = secrets.token_urlsafe(18)
        self.timeout = timeout
        self.lang = lang
        self.stderr = deque(maxlen=30)
        env = dict(os.environ, DISKMAP_CACHE_DIR=str(self.cache), DISKMAP_CONFIG_DIR=str(self.config),
                   DISKMAP_LANG=lang, DISKMAP_TOKEN=self.token)
        command = [str(binary), "--port", "0", "--token", self.token, "--lang", lang, "--stdin-stop"]
        self.process = subprocess.Popen(command, cwd=REPO, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding="utf-8", bufsize=1)
        lines = queue.Queue()

        def read_stdout():
            for line in self.process.stdout:
                lines.put(line)

        def read_stderr():
            for line in self.process.stderr:
                self.stderr.append(line.rstrip())

        threading.Thread(target=read_stdout, daemon=True).start()
        threading.Thread(target=read_stderr, daemon=True).start()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                line = lines.get(timeout=0.1)
            except queue.Empty:
                if self.process.poll() is not None:
                    raise RuntimeError(f"serve 启动失败 / Startup failed: {'; '.join(self.stderr)}")
                continue
            match = re.search(r"http://127\.0\.0\.1:([0-9]+)/\?t=", line)
            if match:
                self.port = int(match.group(1))
                return
        self.stop()
        raise TimeoutError("等待服务链接超时 / Waiting for serve URL timed out")

    def request(self, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        method = "GET" if body is None else "POST"
        headers = {"X-DiskMap-Token": self.token, "X-DiskMap-Lang": self.lang}
        if data is not None:
            headers["Content-Type"] = "application/json"
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=self.timeout)
        try:
            started = time.perf_counter_ns()
            conn.request(method, path, body=data, headers=headers)
            response = conn.getresponse()
            headers_at = time.perf_counter_ns()
            raw = response.read()
            finished = time.perf_counter_ns()
            if response.status != 200:
                raise RuntimeError(f"HTTP {response.status} {path}: {raw[:500]!r}")
            return raw, {
                "http_ms": (finished - started) / 1_000_000,
                "headers_ms": (headers_at - started) / 1_000_000,
                "server_ms": server_timing(response.getheader("Server-Timing")),
            }
        finally:
            conn.close()

    def status(self):
        return json.loads(self.request("/api/status")[0])

    def scan(self, root, apparent, cross_fs):
        started = time.monotonic()
        self.request("/api/scan", {"path": str(root), "apparent": apparent, "cross_fs": cross_fs})
        deadline = started + self.timeout
        previous_print = 0
        settled = 0
        while time.monotonic() < deadline:
            status = self.status()
            if status["state"] in ("error", "cancelled"):
                raise RuntimeError(f"扫描未完成 / Scan did not finish: {status}")
            if status["state"] == "done" and status["ready"] and not status["procs"]["refreshing"]:
                settled += 1
                if settled == 2:
                    return status, (time.monotonic() - started) * 1000
            else:
                settled = 0
            if time.monotonic() - previous_print >= 2:
                print(f"扫描 / Scanning: {status.get('files', 0):,} files, "
                      f"{status.get('dirs', 0):,} dirs, {status.get('state')}, "
                      f"procs_refreshing={status['procs']['refreshing']}", file=sys.stderr, flush=True)
                previous_print = time.monotonic()
            time.sleep(0.1)
        raise TimeoutError("扫描或进程刷新超时 / Scan or process refresh timed out")

    def stop(self):
        if self.process.poll() is None:
            try:
                self.process.stdin.write("stop\n")
                self.process.stdin.flush()
                self.process.wait(timeout=10)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                # 只结束本脚本创建的服务，不查找或结束其他进程。
                self.process.terminate()
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=5)


def tree_stats(raw):
    data = json.loads(raw)
    if data["state"] != "done":
        raise AssertionError(f"只能测完成的扫描 / Expected finished scan: {data['state']}")
    counts = {"d": 0, "f": 0, "a": 0}
    stack = [(data["tree"], 0)]
    depth = 0
    while stack:
        node, level = stack.pop()
        counts[node["t"]] += 1
        depth = max(depth, level)
        stack.extend((child, level + 1) for child in node.get("ch", ()))
    return {"response_bytes": len(raw), "nodes": sum(counts.values()), "node_types": counts,
            "max_depth": depth, "view_bytes": data["tree"]["s"],
            "response_sha256": hashlib.sha256(raw).hexdigest()}


def summary(samples):
    def column(key):
        values = [sample[key] for sample in samples]
        present = [value for value in values if value is not None]
        if not present:
            return None
        if len(present) != len(values):
            raise AssertionError(f"计时头不稳定 / Inconsistent timing header: {key}")
        return {"median": round(statistics.median(present), 4), "min": round(min(present), 4),
                "max": round(max(present), 4), "samples": [round(value, 4) for value in present]}
    return {key: column(key) for key in ("http_ms", "headers_ms", "server_ms")}


def tree_url(path, frac):
    return "/api/tree?" + urllib.parse.urlencode({"path": str(path), "frac": repr(frac)})


def eviction_fractions(frac):
    direction = -math.inf if frac == 0.05 else math.inf
    result = []
    current = frac
    for _ in range(9):
        current = math.nextafter(current, direction)
        if not 1e-6 <= current <= 0.05:
            raise AssertionError("逐出参数超出接口范围 / Eviction frac outside API bounds")
        result.append(current)
    return result


def measure(service, path, frac, repeat, cache_mode):
    url = tree_url(path, frac)
    reference, first_timing = service.request(url)
    stats = tree_stats(reference)
    phases = {}
    for phase in ("repeat", "miss"):
        samples = []
        for _ in range(repeat):
            if phase == "miss" and cache_mode == "on":
                for other_frac in eviction_fractions(frac):
                    service.request(tree_url(path, other_frac))
            raw, timing = service.request(url)
            if raw != reference:
                raise AssertionError(f"{frac} {phase} 响应字节改变 / Response bytes changed")
            samples.append(timing)
        phases[phase] = {"expected_cache": "disabled" if cache_mode == "off" else
                         ("hit" if phase == "repeat" else "miss"), **summary(samples)}
    return {"frac": frac, "samples_per_phase": repeat, **stats,
            "first_request": {key: None if value is None else round(value, 4)
                              for key, value in first_timing.items()},
            "cache_eligible_under_64_mib": len(reference) <= 64 * 1024 * 1024, **phases}


def measure_cold(service, root, view, frac, repeat, apparent, cross_fs):
    timings = []
    samples = []
    for iteration in range(repeat):
        print(f"冷启动 / Cold miss: frac={frac}, {iteration + 1}/{repeat}",
              file=sys.stderr, flush=True)
        status, scan_wall_ms = service.scan(root, apparent, cross_fs)
        # scan() 只轮询status；此请求是新扫描后的首个tree，不预热任何目录投影。
        raw, timing = service.request(tree_url(view, frac))
        stats = tree_stats(raw)
        final = service.status()
        if final["state"] != "done" or final["procs"]["refreshing"] or final["procs"] != status["procs"]:
            raise AssertionError("冷启动测量期间状态变化 / State changed during cold measurement")
        timings.append(timing)
        samples.append({"iteration": iteration + 1,
                        "timing": {key: None if value is None else round(value, 4)
                                   for key, value in timing.items()},
                        "scan": {key: status[key] for key in
                                 ("root", "files", "dirs", "bytes", "denied", "elapsed", "procs")},
                        "scan_wall_ms": round(scan_wall_ms, 3), **stats})
    return {"expected_cache": "cold projection and response", "rescan_count": repeat,
            "response_bytes_compared_across_rescans": False, **summary(timings), "runs": samples}


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, required=True, help="指定serve可执行文件 / serve executable")
    parser.add_argument("--label", help="报告名称，如before-debug / report label")
    parser.add_argument("--cache-mode", choices=("off", "on"), required=True)
    parser.add_argument("--root", type=Path, default=Path(os.path.abspath(os.sep)))
    parser.add_argument("--view", type=Path, help="查看的子目录，默认扫描根 / viewed directory")
    parser.add_argument("--repeat", type=int, default=7)
    parser.add_argument("--cold-repeat", type=int, default=0,
                        help="每个frac重扫后只测一次tree，0关闭 / fresh scans per fraction")
    parser.add_argument("--frac", type=float, action="append", help="替换默认frac，可重复 / override fractions")
    parser.add_argument("--apparent", action="store_true")
    parser.add_argument("--same-fs", action="store_true")
    parser.add_argument("--lang", choices=("zh", "en"), default="zh")
    parser.add_argument("--timeout", type=float, default=600, help="启动/扫描/请求最大秒数 / timeout seconds")
    args = parser.parse_args()
    args.binary = args.binary.expanduser().resolve()
    args.root = Path(os.path.abspath(args.root.expanduser()))
    args.view = Path(os.path.abspath(args.view.expanduser())) if args.view else args.root
    args.frac = args.frac or list(DEFAULT_FRACS)
    if not args.binary.is_file() or not args.root.is_dir():
        parser.error("binary必须是文件、root必须是目录 / binary must be a file; root must be a directory")
    if not 1 <= args.repeat <= 100 or not 0 <= args.cold_repeat <= 100 or args.timeout <= 0:
        parser.error("repeat应为1..100、cold-repeat为0..100、timeout>0 / invalid repeat or timeout")
    if any(not math.isfinite(frac) or not 1e-6 <= frac <= 0.05 for frac in args.frac):
        parser.error("frac必须在1e-6..0.05 / fraction outside API bounds")
    return args


def main():
    args = parse_args()
    with args.binary.open("rb") as file:
        binary_digest = hashlib.file_digest(file, "sha256").hexdigest()
    report = {
        "label": args.label or args.binary.name, "binary": str(args.binary), "binary_sha256": binary_digest,
        "root": str(args.root), "view": str(args.view), "cache_mode": args.cache_mode,
        "lang": args.lang, "apparent": args.apparent, "cross_fs": not args.same_fs,
        "cold_repeat": args.cold_repeat,
        "platform": platform.platform(), "cpu_count": os.cpu_count(), "python": platform.python_version(),
        "measured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "method": {"http": "new TCP connection; through response.read; excludes JSON decoding",
                   "headers": "through complete HTTP response headers; includes accept wait",
                   "server": "Server-Timing tree;dur, or null when unavailable; never inferred from HTTP",
                   "miss": "9 adjacent distinct frac values before each sample when cache is enabled",
                   "cold_miss": "fresh scan, wait done and process refresh, first tree request; no cross-scan byte comparison",
                   "status_control": "reference only; no subtraction from tree RTT"},
    }
    print(f"{report['label']}: {args.root}, cache={args.cache_mode}, repeat={args.repeat}",
          file=sys.stderr, flush=True)
    with tempfile.TemporaryDirectory(prefix="diskmap-t05-tree-bench-") as directory:
        service = Service(args.binary, Path(directory), args.lang, args.timeout)
        try:
            status, scan_wall_ms = service.scan(args.root, args.apparent, not args.same_fs)
            report["scan"] = {key: status[key] for key in ("root", "files", "dirs", "bytes", "denied", "elapsed", "procs")}
            report["scan_wall_ms"] = round(scan_wall_ms, 3)
            print(f"完成 / Done: {status['files']:,} files, {status['dirs']:,} dirs, "
                  f"{status['bytes']:,} bytes, scan={status['elapsed']}s", file=sys.stderr, flush=True)
            controls = [service.request("/api/status")[1] for _ in range(args.repeat)]
            report["status_control"] = summary(controls)
            report["results"] = []
            for frac in args.frac:
                row = measure(service, args.view, frac, args.repeat, args.cache_mode)
                row["first_request_is_first_tree_after_scan"] = not report["results"]
                report["results"].append(row)
                print(f"frac={frac}: {row['nodes']:,} nodes, {row['response_bytes']:,} bytes; "
                      f"HTTP repeat={row['repeat']['http_ms']['median']:.3f}ms, "
                      f"miss={row['miss']['http_ms']['median']:.3f}ms", file=sys.stderr, flush=True)
            final = service.status()
            if final["state"] != "done" or final["procs"]["refreshing"] or final["procs"] != status["procs"]:
                raise AssertionError("测量期间扫描/进程状态变化 / State changed during measurement")
            if args.cold_repeat:
                for row in report["results"]:
                    row["cold_miss"] = measure_cold(service, args.root, args.view, row["frac"],
                                                    args.cold_repeat, args.apparent, not args.same_fs)
                    print(f"frac={row['frac']}: 冷启动HTTP中位 / cold HTTP median="
                          f"{row['cold_miss']['http_ms']['median']:.3f}ms", file=sys.stderr, flush=True)
        finally:
            service.stop()
        if service.process.returncode != 0 or (service.cache / "instance.json").exists():
            raise AssertionError("服务未正常退出并清理实例 / Service did not exit and clean up normally")
        report["startup_stderr"] = list(service.stderr)
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
