"""在 Linux/macOS 上检查已解包的程序，所有子进程使用隔离的缓存和设置。"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def run(command, env, **kwargs):
    result = subprocess.run(
        [str(part) for part in command], env=env, capture_output=True,
        text=True, encoding="utf-8", timeout=240, **kwargs,
    )
    if result.stderr:
        print(result.stderr, end="")
    if result.returncode:
        print(result.stdout, end="")
        result.check_returncode()
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--desktop", type=Path)
    parser.add_argument("--appimage", action="store_true")
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--work", type=Path, required=True)
    args = parser.parse_args()
    args.work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="selftest-", dir=args.work) as temporary:
        work = Path(temporary)
        env = os.environ.copy()
        env.update({
            "DISKMAP_CACHE_DIR": str(work / "cache"),
            "DISKMAP_CONFIG_DIR": str(work / "config"),
            "DISKMAP_LANG": "en", "DISKMAP_NO_ATTACH": "1",
            "TMPDIR": str(work), "TMP": str(work), "TEMP": str(work),
        })
        programs = [[args.cli.resolve()]]
        if args.desktop:
            desktop = [args.desktop.resolve()]
            if args.appimage:
                desktop.append("--appimage-extract-and-run")
            programs.append(desktop)
        for index, program in enumerate(programs):
            version = run([*program, "--version"], env)
            if version.strip() != f"DiskMap {args.version}":
                raise ValueError(f"版本不符 / Unexpected version: {program}, {version!r}")
            report = work / f"report-{index}.json"
            run([*program, "--selftest", "--report", report], env)
            data = json.loads(report.read_text(encoding="utf-8"))
            if data.get("ok") is not True or data.get("frozen") is not True:
                raise ValueError(f"自检报告失败 / Self-test report failed: {data}")
            ping = '{"jsonrpc":"2.0","id":1,"method":"ping"}\n'
            reply = run([*program, "mcp", "--no-attach"], env, input=ping)
            messages = [json.loads(line) for line in reply.splitlines() if line.strip()]
            if messages != [{"jsonrpc": "2.0", "id": 1, "result": {}}]:
                raise ValueError(f"MCP 回复不符 / Unexpected MCP reply: {reply!r}")
            if (work / "cache/instance.json").exists():
                raise ValueError("实例登记未清理 / Instance registration was not cleaned up")
            print(f"自检通过 / Package self-test passed — {program[0]}")


if __name__ == "__main__":
    main()
