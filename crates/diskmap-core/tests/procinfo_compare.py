"""Linux/Windows 实机对照；先构建 release procinfo 示例，两个采集进程同时存活。"""
import argparse
import contextlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
from collections import Counter

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
from compare_support.windows_paths import windows_path_bytes_key, windows_path_key
from compare_support.process import ProcessOwner, run_process


def windows_path_sets(before, after, new):
    """集合比较只更改 Windows 的路径键，其他指标仍用原采集值。"""
    old_paths = {windows_path_key(path) for path in before.files}
    after_paths = {windows_path_key(path) for path in after.files}
    new_paths = {windows_path_bytes_key(bytes.fromhex(path["path_hex"])) for path in new["paths"]}
    return old_paths, after_paths, new_paths


def windows_mapping_source(complete_query, state, region_type):
    """空闲区域的 Type 未定义；不能将卸载竞态里的旧值误报成确定的映像/数据证据。"""
    if not complete_query or state not in (0x1000, 0x2000):
        return "memory_maps_unverified"
    return {0x1000000: "memory_maps_image", 0x40000: "memory_maps_other"}.get(
        region_type, "memory_maps_unverified")


def windows_probe(tasks):
    """补采样只读来源证据；按实际 MEM_IMAGE 分类，不按 .dll/.exe 后缀猜测。"""
    import ctypes
    from ctypes import wintypes
    import psutil

    class MemoryInfo(ctypes.Structure):
        _fields_ = [("base", ctypes.c_void_p), ("allocation_base", ctypes.c_void_p),
                    ("allocation_protect", wintypes.DWORD), ("region_size", ctypes.c_size_t),
                    ("state", wintypes.DWORD), ("protect", wintypes.DWORD), ("type", wintypes.DWORD)]

    # x64 的 PartitionId 位于自然对齐填充中；x86 使用 28 字节的标准布局。
    assert ctypes.sizeof(MemoryInfo) == (48 if ctypes.sizeof(ctypes.c_void_p) == 8 else 28)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    kernel.VirtualQueryEx.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.POINTER(MemoryInfo), ctypes.c_size_t]
    kernel.VirtualQueryEx.restype = ctypes.c_size_t
    records, errors = {}, []
    by_pid = {}
    for task in tasks:
        records[task["key"]] = []
        for process in task["processes"]:
            by_pid.setdefault(process["pid"], set()).add(task["key"])
    for pid, wanted in by_pid.items():
        if pid == 0:
            continue
        handle = None
        try:
            process = psutil.Process(pid)
            try:
                for entry in process.open_files():
                    key = windows_path_key(entry.path).hex()
                    if key in wanted:
                        records[key].append({"pid": pid, "path": entry.path, "source": "open_files"})
            except (psutil.Error, OSError, ValueError) as error:
                errors.append({"pid": pid, "operation": "open_files", "error": str(error)})
            # 只查询内存区域类型，不读取进程内存，不要求写入或 VM_READ 权限。
            handle = kernel.OpenProcess(0x0400, False, pid)
            if not handle:
                errors.append({"pid": pid, "operation": "OpenProcess(PROCESS_QUERY_INFORMATION)",
                               "winerror": ctypes.get_last_error()})
            for entry in process.memory_maps(grouped=False):
                key = windows_path_key(entry.path).hex()
                if key not in wanted:
                    continue
                region = MemoryInfo()
                address = int(str(entry.addr).split("-", 1)[0], 16)
                count = kernel.VirtualQueryEx(handle, address, ctypes.byref(region), ctypes.sizeof(region)) if handle else 0
                source = windows_mapping_source(count == ctypes.sizeof(region), region.state, region.type)
                evidence = {"pid": pid, "path": entry.path, "source": source, "address": str(entry.addr)}
                if count:
                    evidence["region_type"] = hex(region.type)
                elif handle:
                    evidence["winerror"] = ctypes.get_last_error()
                records[key].append(evidence)
        except (psutil.Error, OSError, ValueError, AttributeError) as error:
            errors.append({"pid": pid, "operation": "memory_maps", "error": str(error)})
        finally:
            if handle:
                kernel.CloseHandle(handle)
    return {"records": records, "errors": errors}


def windows_source_report(tasks, stable, probe):
    """类别互斥但来源可重叠；映像优先，不能被同一文件的句柄证据掩盖。"""
    counts = Counter({"open_files": 0, "memory_maps_image": 0, "memory_maps_other": 0})
    source_counts, samples, stable_images, stable_data = Counter(), {}, 0, 0
    for task in tasks:
        evidence = probe.get("records", {}).get(task["key"], [])
        sources = {entry["source"] for entry in evidence}
        for process in task["processes"]:
            kind = process["kind"]
            if kind == "lib":
                # 旧快照只标 lib；补查成功才确定是映像还是普通映射。
                if not any(entry["pid"] == process["pid"] and entry["source"].startswith("memory_maps_")
                           for entry in evidence):
                    sources.add("memory_maps_unverified")
            else:
                sources.add({"open": "open_files", "exe": "executable", "swap": "swap"}.get(kind, "unknown"))
        category = next((name for name in ("memory_maps_image", "executable", "swap", "memory_maps_unverified",
                         "memory_maps_other", "open_files", "unknown") if name in sources), "unknown")
        counts[category] += 1
        if category == "memory_maps_image" and task["key"] in stable:
            stable_images += 1
        if category == "memory_maps_other" and task["key"] in stable:
            stable_data += 1
        source_counts.update(sources)
        bucket = samples.setdefault(category, [])
        if len(bucket) < 20:
            # 同一路径可能有很多句柄，截断样例前先保留映像证据。
            displayed = sorted(evidence, key=lambda entry: entry["source"] != "memory_maps_image")[:20]
            bucket.append({**task, "stable_before_and_after": task["key"] in stable,
                           "sources": sorted(sources), "evidence": displayed})
    if counts["memory_maps_image"]:
        conclusion = "image_mappings_missing_review_required"
        explanation = "发现旧版独有的 MEM_IMAGE，需复核映像采集和竞态。 / MEM_IMAGE misses require image-collection and race review."
    elif counts["executable"] or counts["swap"]:
        conclusion = "executable_or_swap_missing_review_required"
        explanation = "存在可执行文件或交换文件差异，需单独复核。 / Executable or swap differences require review."
    elif counts["memory_maps_other"]:
        conclusion = "data_mappings_missing_review_required"
        explanation = "发现旧版独有的 MEM_MAPPED；数据映射已实现，需复核采集和竞态。 / MEM_MAPPED is supported; data-mapping misses require collection and race review."
    elif counts["memory_maps_unverified"] or counts["unknown"]:
        conclusion = "unclassified_sources_require_review"
        explanation = "部分映射类型无法核实，不能判定剩余差异仅为未实现的句柄表。 / Unverified sources prevent a handles-only conclusion."
    elif tasks:
        conclusion = "only_unsupported_handles_observed"
        explanation = "本轮已分类差异仅见尚未实现的文件句柄，映像和数据映射遗漏均为零。 / Only unsupported file handles observed; image and data-mapping misses are zero."
    else:
        conclusion = "no_missing_paths"
        explanation = "本轮旧版路径无遗漏。 / No old-only paths in this sample."
    return {"category_counts": dict(counts), "overlapping_source_counts": dict(source_counts),
            "stable_image_missing": stable_images,
            "stable_data_missing": stable_data,
            "samples": samples, "image_missing_samples": samples.get("memory_maps_image", []),
            "probe_errors": probe.get("errors", []), "conclusion": conclusion, "explanation": explanation,
            "sampling_limit": "按路径比较，进程权限、退出/PID 复用和映像装卸仍有竞态；MEM_IMAGE 不等于加载器模块。 / Path-level samples retain permission, process/PID and unload races; MEM_IMAGE is not identical to a loader module."}


def windows_conclusion(results):
    """最后一行取所有轮次中需最先复核的结论；计数是观测次数，不冒充唯一路径数。"""
    priority = ["image_mappings_missing_review_required", "executable_or_swap_missing_review_required",
                "data_mappings_missing_review_required", "unclassified_sources_require_review",
                "only_unsupported_handles_observed", "no_missing_paths"]
    reports = [row["old_only_sources"] for row in results]
    worst = min(reports, key=lambda report: priority.index(report["conclusion"]))
    images = sum(report["category_counts"]["memory_maps_image"] for report in reports)
    stable = sum(report["stable_image_missing"] for report in reports)
    data = sum(report["category_counts"]["memory_maps_other"] for report in reports)
    stable_data = sum(report["stable_data_missing"] for report in reports)
    return (f"Windows procinfo 结论 / conclusion: {worst['conclusion']}; {worst['explanation']} "
            f"映像遗漏观测 / image-miss observations: {images}; "
            f"前后均存在 / present before and after: {stable}. "
            f"数据映射遗漏观测 / data-mapping-miss observations: {data}; "
            f"前后均存在 / present before and after: {stable_data}. "
            "采样权限与竞态边界见 JSON / Sampling permission and race limits are recorded in JSON.")


def windows_missing_report(before, after, missing, stable):
    tasks = {}
    after_processes = {}
    for path, slot in after.files.items():
        after_processes.setdefault(windows_path_key(path).hex(), set()).update(slot)
    for path, slot in before.files.items():
        key = windows_path_key(path)
        if key not in missing:
            continue
        entry = tasks.setdefault(key.hex(), {"key": key.hex(), "path": path, "processes": []})
        entry["processes"].extend({"pid": pid, "comm": str(comm), "kind": kind,
                                    "present_after": pid in after_processes.get(key.hex(), set())}
                                   for pid, (comm, kind) in slot.items())
    tasks = list(tasks.values())
    probe = {"records": {}, "errors": []}
    if tasks:
        try:
            # 在独立、限时子进程里补查，不把 psutil/权限阻塞算入采集器耗时。
            result = run_process([sys.executable, "-B", __file__, "--windows-evidence"],
                                 input=json.dumps(tasks, ensure_ascii=True), timeout=60, env=os.environ)
            if result.returncode:
                raise ChildProcessError(result.stderr)
            probe = json.loads(result.stdout)
        except (OSError, ValueError, TimeoutError, subprocess.TimeoutExpired) as error:
            probe["errors"].append({"operation": "source_probe", "error": str(error)})
    return windows_source_report(tasks, {key.hex() for key in stable}, probe)


def selftest():
    """Linux 离线验证 Windows 分类，不访问真实 Windows 或真实进程。"""
    from types import SimpleNamespace
    compile(WORKER, "<Linux mapping fixture>", "exec")
    compile(WORKER_WINDOWS, "<Windows mapping fixture>", "exec")
    for state in (0x1000, 0x2000):
        assert windows_mapping_source(True, state, 0x1000000) == "memory_maps_image"
        assert windows_mapping_source(True, state, 0x40000) == "memory_maps_other"
    assert windows_mapping_source(True, 0x10000, 0x1000000) == "memory_maps_unverified"
    assert windows_mapping_source(True, 0x1000, 0x20000) == "memory_maps_unverified"
    assert windows_mapping_source(False, 0x1000, 0x1000000) == "memory_maps_unverified"
    key = windows_path_key("C:/Mixed/Σ.dll").hex()
    task = {"key": key, "path": "c:/mixed/ς.DLL", "processes": [{"pid": 4, "kind": "lib"}]}
    def report(*sources):
        return windows_source_report([task], {key}, {"records": {key: [
            {"pid": 4, "source": source} for source in sources]}})
    image = report("open_files", "memory_maps_image")
    assert image["category_counts"] == {"open_files": 0, "memory_maps_image": 1, "memory_maps_other": 0}
    assert image["overlapping_source_counts"] == {"open_files": 1, "memory_maps_image": 1}
    assert image["image_missing_samples"][0]["stable_before_and_after"]
    assert image["conclusion"] == "image_mappings_missing_review_required"
    assert image["stable_image_missing"] == 1
    assert report(*(["open_files"] * 30), "memory_maps_image")["image_missing_samples"][0]["evidence"][0]["source"] == "memory_maps_image"
    assert "image_mappings_missing_review_required" in windows_conclusion([
        {"old_only_sources": windows_source_report([], set(), {})}, {"old_only_sources": image}])
    assert report("open_files")["conclusion"] == "unclassified_sources_require_review"
    data = report("memory_maps_other", "open_files")
    assert data["conclusion"] == "data_mappings_missing_review_required"
    assert data["stable_data_missing"] == 1
    assert data["category_counts"]["open_files"] == 0
    assert report("memory_maps_other", "memory_maps_unverified")["conclusion"] == "unclassified_sources_require_review"
    assert "data_mappings_missing_review_required" in windows_conclusion([
        {"old_only_sources": windows_source_report([], set(), {})}, {"old_only_sources": data}])
    assert windows_source_report([], set(), {})["conclusion"] == "no_missing_paths"
    task["processes"][0]["kind"] = "exe"
    assert report()["conclusion"] == "executable_or_swap_missing_review_required"
    task["processes"][0]["kind"] = "open"
    assert report()["conclusion"] == "only_unsupported_handles_observed"
    # 只使用 T15 等价关系；不合并 ß/ẞ、İ/i、增补字符或不同路径组件。
    paths = ["c:/Mixed/ς.dll", "c:/raw/\ud800", "c:/raw/\udc80", "c:/raw/🦀", "c:/raw/�"]
    before = SimpleNamespace(files=dict.fromkeys(paths))
    new = {"paths": [{"path_hex": path.encode("utf-8", "surrogatepass").hex()} for path in
                     ["C:\\MIXED\\Σ.DLL", *paths[1:]]]}
    old, after, current = windows_path_sets(before, before, new)
    assert old == after == current and len(old) == 5
    for left, right in [("ß", "ẞ"), ("İ", "i\u0307"), ("𐐀", "𐐨"), ("C:/a/../b", "C:/b")]:
        assert windows_path_key(left) != windows_path_key(right)
    print("Windows 路径键和来源分类离线检查通过 / Windows path-key and source classification checks passed")


def main():
    # 重定向到 SSH/PowerShell 日志时也固定 UTF-8，不沿用中文 Windows 的 GBK。
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="strict")
    if sys.argv[1:] == ["--selftest"]:
        selftest()
        return
    if sys.argv[1:] == ["--windows-evidence"]:
        assert os.name == "nt", "来源补采样只在 Windows 上执行 / Evidence probe is Windows-only"
        print(json.dumps(windows_probe(json.load(sys.stdin)), ensure_ascii=True))
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("rounds", nargs="?", type=int, default=5)
    parser.add_argument("--processes", type=int, default=0)
    parser.add_argument("--files-per-process", type=int, default=64)
    args = parser.parse_args()
    rounds = args.rounds
    if rounds < 1 or args.processes < 0 or args.files_per_process < 1:
        parser.error("计数必须为正数，进程数可以为零")
    with tempfile.TemporaryDirectory(prefix="diskmap-procinfo-compare-") as temp:
        os.environ["DISKMAP_CACHE_DIR"] = str(Path(temp) / "cache")
        os.environ["DISKMAP_CONFIG_DIR"] = str(Path(temp) / "config")
        os.environ["PYTHONDONTWRITEBYTECODE"] = "1"
        os.environ["PYTHONUTF8"] = "1"
        os.environ["PYTHONIOENCODING"] = "utf-8"
        sys.path.insert(0, str(ROOT / "legacy"))

        with contextlib.ExitStack() as workers:
            for number in range(args.processes):
                directory = Path(temp) / f"worker-{number}"
                directory.mkdir()
                for item in range(args.files_per_process):
                    (directory / f"file-{item}").write_bytes(bytes(4096))
                worker = workers.enter_context(ProcessOwner(
                    [sys.executable, "-c", WORKER_WINDOWS if os.name == "nt" else WORKER, str(directory)],
                    env=os.environ,
                ))
                assert worker.readline(timeout=30).strip() == "ready"
            compare(rounds, args.processes, args.files_per_process)


# 每个测试进程只打开、映射由父进程在临时目录生成的文件。
WORKER = """import mmap, os, sys
files = [open(os.path.join(sys.argv[1], n), 'rb') for n in os.listdir(sys.argv[1])]
maps = [mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) for f in files]
print('ready', flush=True)
sys.stdin.readline()
"""

# Windows 的测试视图在 ready 前关闭文件和 section 句柄，只留下只读映射；
# 这样不会把 memory_maps 应发现的文件误靠 open_files 收到。
WORKER_WINDOWS = """import ctypes, os, sys
from ctypes import wintypes
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                              wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
kernel.CreateFileW.restype = wintypes.HANDLE
kernel.CreateFileMappingW.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD,
                                     wintypes.DWORD, wintypes.DWORD, wintypes.LPCWSTR]
kernel.CreateFileMappingW.restype = wintypes.HANDLE
kernel.MapViewOfFile.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.DWORD,
                                wintypes.DWORD, ctypes.c_size_t]
kernel.MapViewOfFile.restype = ctypes.c_void_p
kernel.CloseHandle.argtypes = [wintypes.HANDLE]
kernel.CloseHandle.restype = wintypes.BOOL
kernel.UnmapViewOfFile.argtypes = [ctypes.c_void_p]
kernel.UnmapViewOfFile.restype = wintypes.BOOL
views = []
try:
    for name in os.listdir(sys.argv[1]):
        file = kernel.CreateFileW(os.path.join(sys.argv[1], name), 0x80000000, 7, None, 3, 0x80, None)
        if file == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        section = None
        try:
            section = kernel.CreateFileMappingW(file, None, 2, 0, 0, None)
            if not section:
                raise ctypes.WinError(ctypes.get_last_error())
            view = kernel.MapViewOfFile(section, 4, 0, 0, 0)
            if not view:
                raise ctypes.WinError(ctypes.get_last_error())
            views.append(view)
        finally:
            if section:
                kernel.CloseHandle(section)
            kernel.CloseHandle(file)
    print('ready', flush=True)
    sys.stdin.readline()
finally:
    for view in views:
        kernel.UnmapViewOfFile(view)
"""


def compare(rounds, processes, files_per_process):
    from backend.procinfo import ProcInfo

    binary = ROOT / "target/release/examples" / ("procinfo.exe" if os.name == "nt" else "procinfo")
    with ProcessOwner([str(binary), "--interactive", "--files"], env=os.environ) as child:
        ready = json.loads(child.readline(timeout=30))
        assert ready["ready"]
        old_times, new_times, results = [], [], []
        for _ in range(rounds):
            before = ProcInfo().refresh()
            child.process.stdin.write("refresh\n")
            child.process.stdin.flush()
            new = json.loads(child.readline(timeout=60))
            after = ProcInfo().refresh()
            assert before.error is None and after.error is None
            assert new["started"] and new["error"] is None and not new["timed_out"]
            encode = (lambda path: path.encode("utf-8", "surrogatepass")) if os.name == "nt" else os.fsencode
            decode = (lambda path: path.decode("utf-8", "surrogatepass")) if os.name == "nt" else os.fsdecode
            old_paths = {encode(p) for p in before.files}
            after_paths = {encode(p) for p in after.files}
            new_paths = {bytes.fromhex(p["path_hex"]) for p in new["paths"]}
            raw_common = old_paths & new_paths
            if os.name == "nt":
                old_paths, after_paths, new_paths = windows_path_sets(before, after, new)
            common = old_paths & new_paths
            stable = old_paths & after_paths
            row = {
                "old_files": len(old_paths), "new_files": len(new_paths),
                "intersection": len(common), "union": len(old_paths | new_paths),
                "old_coverage": len(common) / max(1, len(old_paths)),
                "new_coverage": len(common) / max(1, len(new_paths)),
                "stable_old_missing": sorted(p.hex() for p in stable - new_paths),
                "old_only": sorted(decode(p) for p in old_paths - new_paths),
                "new_only": sorted(decode(p) for p in new_paths - old_paths),
                "old_nprocs": before.nprocs, "new_nprocs": new["nprocs"],
                "old_visible": before.visible, "new_visible": new["visible"],
                "old_seconds": before.time, "new_seconds": new["time"],
            }
            if os.name == "nt":
                # 显示原始写法，比较使用 T15 键；不向 Linux 报告增加兼容例外。
                row["old_only"] = sorted(path for path in before.files if windows_path_key(path) not in new_paths)
                row["new_only"] = sorted(bytes.fromhex(path["path_hex"]).decode("utf-8", "surrogatepass")
                                         for path in new["paths"] if windows_path_bytes_key(bytes.fromhex(path["path_hex"])) not in old_paths)
                row["path_comparison"] = "T15 platform::path::path_key(Windows)"
                row["raw_intersection"] = len(raw_common)
                row["path_key_matched_spelling_differences"] = len(common) - len({windows_path_bytes_key(path) for path in raw_common})
                row["old_only_sources"] = windows_missing_report(before, after, old_paths - new_paths, stable)
            old_times.append(before.time)
            new_times.append(new["time"])
            results.append(row)
        print(json.dumps({"fixture_processes": processes, "fixture_files": processes * files_per_process, "rounds": results,
            "old_median_seconds": statistics.median(old_times),
            "new_median_seconds": statistics.median(new_times)}, ensure_ascii=True, indent=2))
        if os.name == "nt":
            print(windows_conclusion(results))
        child.process.stdin.write("quit\n")
        child.process.stdin.flush()
        result = child.collect(timeout=15)
        assert result.returncode == 0, result.stderr

if __name__ == "__main__":
    main()
