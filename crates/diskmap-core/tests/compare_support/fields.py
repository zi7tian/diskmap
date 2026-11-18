"""仅核验旧 procinfo.normcase 直接产生的字段；扫描索引路径和文本保持逐字比较。"""
from compare_support.windows_paths import windows_path_key


def fixture_spellings(root):
    """测试树的真实枚举拼写，用于独立断言 Rust 没有把路径也变为小写。"""
    return {windows_path_key(str(path)): str(path) for path in root.rglob("*") if path.is_file()}


def adapt_proc_paths(old, new, *, windows, spellings):
    """只允许 summary.in_use.largest[*].path；嵌套报告/视图/MCP仍是同一字段。"""
    if not windows:
        return 0
    changed = 0
    if isinstance(old, dict) and isinstance(new, dict):
        summary_fields = {"by_category", "largest_children", "cleanup", "in_use", "path", "files", "dirs"}
        if summary_fields <= old.keys() and summary_fields <= new.keys():
            before, after = old["in_use"].get("largest"), new["in_use"].get("largest")
            if isinstance(before, list) and isinstance(after, list) and len(before) == len(after):
                for left, right in zip(before, after):
                    if not isinstance(left, dict) or not isinstance(right, dict):
                        continue
                    a, b = left.get("path"), right.get("path")
                    if not isinstance(a, str) or not isinstance(b, str):
                        continue
                    key = windows_path_key(b)
                    assert spellings.get(key) == b, f"Rust proc路径未保留测试树真实拼写：{b!r}"
                    if a != b and windows_path_key(a) == key:
                        left["path"] = b
                        changed += 1
        for key in old.keys() & new.keys():
            changed += adapt_proc_paths(old[key], new[key], windows=windows, spellings=spellings)
    elif isinstance(old, list) and isinstance(new, list) and len(old) == len(new):
        for left, right in zip(old, new):
            changed += adapt_proc_paths(left, right, windows=windows, spellings=spellings)
    return changed
