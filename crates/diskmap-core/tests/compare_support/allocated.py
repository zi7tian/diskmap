"""仅核验 T05 两个 Windows allocated 完成状态；磁盘 API 数值仅用于独立诊断。"""

import ast
import ctypes
from ctypes import wintypes
from functools import lru_cache
import os
from pathlib import Path


AGENT_ALLOCATED_LABELS = frozenset((
    "agent switch allocated done",
    "agent reuse default apparent done",
))


@lru_cache(maxsize=1)
def _legacy_human():
    """只编译旧版无依赖的 human 函数，不导入 backend 或触发语言/设置初始化。"""
    source = Path(__file__).resolve().parents[4] / "legacy/backend/analysis.py"
    module = ast.parse(source.read_text(encoding="utf-8"), filename=str(source))
    functions = [node for node in module.body if isinstance(node, ast.FunctionDef)
                 and node.name == "human"]
    assert len(functions) == 1, "旧版 analysis.py 必须恰有一个 human 函数"
    function = functions[0]
    assert not function.decorator_list and not function.args.defaults, (
        "旧版 human 新增了定义期依赖，需重新审查提取方式")
    namespace = {"__builtins__": {}}
    exec(compile(ast.Module(body=[function], type_ignores=[]), str(source), "exec"), namespace)
    return namespace["human"]


def legacy_human(size):
    """使用仓库中的旧版函数作为 oracle；不复制或另写大小格式化规则。"""
    return _legacy_human()(size)


def round_up(size, alignment):
    return (size + alignment - 1) // alignment * alignment


def allocation_evidence(root, cluster, rows):
    """纯算术部分可在 Linux 用生成的 fixture 校验；不从扫描响应反推分配规则。"""
    assert type(cluster) is int and cluster > 0, cluster
    for row in rows:
        assert type(row["size"]) is int and row["size"] >= 0, row
        assert type(row["allocated"]) is int and row["allocated"] >= 0, row
    small = [row for row in rows if 0 < row["size"] < cluster]
    alignments = []
    alignment = 1
    while alignment <= cluster:
        alignments.append(alignment)
        alignment *= 2
    return {
        "root": str(root), "cluster": cluster, "files": len(rows),
        "apparent": sum(row["size"] for row in rows),
        "legacy_cluster_bytes": sum(round_up(row["size"], cluster) for row in rows),
        "compressed_api_bytes": sum(row["allocated"] for row in rows),
        "small_files": small,
        "small_alignment": {
            str(alignment): {
                "rounded_size_sum": sum(round_up(row["size"], alignment) for row in small),
                "api_matches": sum(row["allocated"] == round_up(row["size"], alignment)
                                   for row in small),
            }
            for alignment in alignments
        },
    }


def windows_allocation_evidence(root):
    """读取实际卷的簇大小和文件实际占用；API 失败必须报错，不回退到写死的 4096。"""
    assert os.name == "nt", "仅 Windows 调用磁盘 API"
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetVolumePathNameW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD]
    kernel.GetVolumePathNameW.restype = wintypes.BOOL
    kernel.GetDiskFreeSpaceW.argtypes = [wintypes.LPCWSTR] + [ctypes.POINTER(wintypes.DWORD)] * 4
    kernel.GetDiskFreeSpaceW.restype = wintypes.BOOL
    kernel.GetCompressedFileSizeW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.DWORD)]
    kernel.GetCompressedFileSizeW.restype = wintypes.DWORD
    volume = ctypes.create_unicode_buffer(32768)
    if not kernel.GetVolumePathNameW(str(root), volume, len(volume)):
        raise ctypes.WinError(ctypes.get_last_error())
    sectors, sector_bytes, free_clusters, total_clusters = (wintypes.DWORD() for _ in range(4))
    if not kernel.GetDiskFreeSpaceW(volume.value, ctypes.byref(sectors), ctypes.byref(sector_bytes),
                                   ctypes.byref(free_clusters), ctypes.byref(total_clusters)):
        raise ctypes.WinError(ctypes.get_last_error())
    rows = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        high = wintypes.DWORD()
        ctypes.set_last_error(0)
        low = kernel.GetCompressedFileSizeW(str(path), ctypes.byref(high))
        error = ctypes.get_last_error()
        if low == 0xFFFFFFFF and error:
            raise ctypes.WinError(error)
        rows.append({"path": str(path.relative_to(root)), "size": path.stat().st_size,
                     "allocated": (high.value << 32) | low})
    evidence = allocation_evidence(root, sectors.value * sector_bytes.value, rows)
    evidence["volume"] = volume.value
    return evidence


def adapt_agent_allocated_bytes(label, old, new, *, windows, evidence):
    """仅白名单的 scan.bytes 及其 size；返回口径差异组数，不按两个字段重复计数。"""
    if not windows or label not in AGENT_ALLOCATED_LABELS:
        return 0
    before, after = old["body"]["scan"], new["body"]["scan"]
    for response, scan in ((old, before), (new, after)):
        assert response["status"] == 200, response
        assert scan["state"] == "done" and scan["size_mode"] == "allocated", scan
        assert scan["root"] == evidence["root"] and scan["files"] == evidence["files"], scan
        assert type(scan["bytes"]) is int, scan
        expected_size = legacy_human(scan["bytes"])
        assert scan["size"] == expected_size, (
            f"{'旧版' if response is old else '新版'} scan.size 不等于旧版 human(各自bytes)："
            f"bytes={scan['bytes']}，size={scan['size']!r}，期望={expected_size!r}")
    assert before["bytes"] == evidence["legacy_cluster_bytes"], (
        f"旧版 allocated 不等于实际簇大小 {evidence['cluster']} 的逐文件取整和："
        f"旧={before['bytes']}，独立计算={evidence['legacy_cluster_bytes']}")
    assert evidence["apparent"] <= after["bytes"] <= before["bytes"], (
        f"新版 allocated 超出 fixture 约束：apparent={evidence['apparent']}，"
        f"新={after['bytes']}，旧={before['bytes']}")
    if before["bytes"] == after["bytes"]:
        return 0
    before["bytes"] = after["bytes"]
    before["size"] = after["size"]
    return 1
