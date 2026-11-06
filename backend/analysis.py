"""给 AI 助手（以及命令行报告）用的分析接口。

把扫描树整理成精炼的结构化结果：每一项都带大小、重要程度类别、分类原因、
是否正被程序使用、以及"能不能删"的安全建议。所有函数都只读，不会改动任何文件。
"""
import heapq
import time

from .classify import CACHE, CLASSES, CONFIG, CORE, NCLS, OTHER, RUNNING, SYSTEM, USER
from .osinfo import SEP, join_path, pkey
from .scanner import DENIED, MOUNT

KEYS = [c["key"] for c in CLASSES]
LABELS = [c["label"] for c in CLASSES]

# 安全等级（机器可读） + 建议（给人看的）
SAFETY = {
    CORE: ("danger", "系统核心：不要删除或修改，否则可能无法开机。"),
    RUNNING: ("in_use", "正被程序使用：如需删除，先关闭相关程序。"),
    SYSTEM: ("uninstall_only", "系统或已安装程序的文件：应通过包管理器/系统设置卸载对应软件，不要手动删除。"),
    CONFIG: ("caution", "配置文件：删除会丢失相应程序的设置，确认不再需要再删。"),
    USER: ("ask_user", "用户的个人文件：是否删除只能由用户决定，请先询问。"),
    CACHE: ("safe_to_clean", "缓存/临时文件：通常可以安全清理，程序会按需重新生成。"),
    OTHER: ("caution", "应用数据：删除前确认对应程序已不再使用。"),
}


class AnalysisError(ValueError):
    pass


def human(n):
    if n is None:
        return None
    if n < 1024:
        return f"{n} B"
    for unit in ("KB", "MB", "GB", "TB", "PB"):
        n /= 1024
        if n < 1024 or unit == "PB":
            return f"{n:.0f} {unit}" if n >= 100 else f"{n:.1f} {unit}" if n >= 10 else f"{n:.2f} {unit}"


def _pct(a, b):
    return round(a / b * 100, 1) if b else 0.0


def _cat(cls):
    return {"category": KEYS[cls], "category_label": LABELS[cls]}


def _safety(cls, in_use=0):
    level, advice = SAFETY[cls]
    if in_use and cls != RUNNING:
        advice += f"其中有 {in_use} 个文件正被程序使用，清理前先关闭相关程序。"
    return {"safety": level, "advice": advice}


def _scan(app):
    s = app.scan
    if s is None:
        raise AnalysisError("还没有扫描结果。请先调用 diskmap_scan 扫描一个文件夹或磁盘。")
    return s


def _dir(app, path):
    s = _scan(app)
    node, f = s.find(path or s.root_path)
    if node is None:
        raise AnalysisError(f"扫描结果中没有这个路径：{path}（当前扫描范围是 {s.root_path}）")
    if f is not None:
        raise AnalysisError(f"这是一个文件而不是文件夹：{path}。查看文件请用 diskmap_explain。")
    return node


def _scan_note(s):
    if s.state == "scanning":
        return "扫描仍在进行中，以下数据还不完整。"
    if s.state == "cancelled":
        return "扫描被中途停止，以下数据不完整。"
    return None


# ------------------------------------------------------------------ 条目
def dir_entry(app, d, parent_size=None):
    in_use = app.proc.dir_count(d.path)
    e = {"path": d.path, "type": "dir", "bytes": d.size, "size": human(d.size),
         **_cat(d.cls), "reason": d.reason, "files": d.nfiles, "dirs": d.ndirs}
    if parent_size:
        e["percent_of_parent"] = _pct(d.size, parent_size)
    if in_use:
        e["in_use_files"] = in_use
    if d.flags & DENIED:
        e["denied"] = True
    if d.flags & MOUNT:
        e["mount_point"] = True
    e.update(_safety(d.cls, in_use))
    return e


def file_entry(app, d, f, parent_size=None):
    path = join_path(d.path, f[0])
    static = f[2]
    used_by = app.proc.summary(path)
    cls = RUNNING if used_by else static
    _, reason = app.classifier.explain_file(d.cls, d.reason, d.path, f[0], path)
    e = {"path": path, "type": "file", "bytes": f[1], "size": human(f[1]), **_cat(cls),
         "reason": reason}
    if parent_size:
        e["percent_of_parent"] = _pct(f[1], parent_size)
    if used_by:
        e["used_by"] = used_by
        e["static_category"] = KEYS[static]
    pkg = app.pkgdb.files.get(path)
    if pkg:
        e["package"] = pkg
    e.update(_safety(cls))
    return e


def _group_entry(d, f, parent_size=None):
    e = {"path": d.path, "type": "group", "count": f[3], "bytes": f[1], "size": human(f[1]),
         **_cat(f[2]), "note": f"{d.path} 中 {f[3]} 个较小的文件合并统计"}
    if parent_size:
        e["percent_of_parent"] = _pct(f[1], parent_size)
    return e


def _walk_dirs(d):
    stack = [d]
    while stack:
        x = stack.pop()
        yield x
        stack.extend(x.dirs.values())


def _walk_files(d):
    for x in _walk_dirs(d):
        for f in x.files:
            if len(f) == 3:
                yield x, f


# ------------------------------------------------------------------ 查询
def status(app):
    s = app.scan
    now = time.time()
    out = {"version": app.version, "os": app.os_name, "admin": app.admin,
           "ready": app.ready.is_set(),
           "window_connected": bool(app.last_seen and now - app.last_seen < 15),
           "processes": {"total": app.proc.nprocs, "in_use_files": len(app.proc.files),
                         "visible_processes": app.proc.visible}}
    if s is None:
        out["scan"] = None
        out["hint"] = "还没有扫描。调用 diskmap_scan 开始，例如扫描整个系统盘。"
        return out
    end = s.finished or now
    out["scan"] = {
        "root": s.root_path, "state": s.state, "files": s.nfiles, "dirs": s.ndirs,
        "bytes": s.bytes, "size": human(s.bytes), "elapsed_seconds": round(end - s.started, 1),
        "finished_at": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(s.finished)) if s.finished else None,
        "current": s.current if s.state == "scanning" else None,
        "denied_dirs": s.denied, "disk_used_bytes": s.fs_used or None,
        "size_mode": "apparent" if s.apparent else "allocated",
        "includes_other_partitions": s.cross_fs,
    }
    if s.denied:
        out["scan"]["denied_note"] = (f"有 {s.denied} 个文件夹因权限不足没有统计；"
                                      "以管理员/root 身份运行 DiskMap 可以看到全部。")
    if app.ui_state:
        out["user_view"] = {k: app.ui_state.get(k) for k in ("view", "selected")}
    return out


def summary(app, path=None, top=10):
    s = _scan(app)
    d = _dir(app, path)
    top = max(1, min(int(top), 50))
    totals = s.totals(d)
    total = d.size or 1
    by_cat = [{**_cat(c), "bytes": totals[c], "size": human(totals[c]), "percent": _pct(totals[c], total)}
              for c in range(NCLS) if totals[c] > 0]
    by_cat.sort(key=lambda x: -x["bytes"])
    children = list_dir(app, d.path, top)["items"]
    largest = largest_files(app, d.path, top)["files"]
    clean = cleanup(app, d.path, limit=min(top, 10))
    running = [h for h in s.running_hits() if pkey(h[0]).startswith(_prefix(d.path))]
    running.sort(key=lambda h: -h[1])
    out = {
        "path": d.path, "bytes": d.size, "size": human(d.size), "files": d.nfiles, "dirs": d.ndirs,
        **_cat(d.cls), "reason": d.reason,
        "by_category": by_cat,
        "largest_children": children,
        "largest_files": largest,
        "cleanup": {"reclaimable_bytes": clean["reclaimable_bytes"],
                    "reclaimable_size": clean["reclaimable_size"],
                    "top_candidates": clean["candidates"]},
        "in_use": {"files": len(running), "bytes": sum(h[1] for h in running),
                   "size": human(sum(h[1] for h in running)),
                   "largest": [{"path": p, "size": human(sz), "used_by": app.proc.summary(p)}
                               for p, sz, _ in running[:5]]},
    }
    note = _scan_note(s)
    if note:
        out["note"] = note
    return out


def _prefix(path):
    return pkey(path if path.endswith(SEP) else path + SEP)


def list_dir(app, path, limit=50):
    s = _scan(app)
    d = _dir(app, path)
    limit = max(1, min(int(limit), 500))
    items = [(c.size, "d", c) for c in d.dirs.values()] + [(f[1], "f", f) for f in d.files]
    items.sort(key=lambda t: -t[0])
    out = []
    for size, kind, obj in items[:limit]:
        if kind == "d":
            out.append(dir_entry(app, obj, d.size))
        elif len(obj) == 4:
            out.append(_group_entry(d, obj, d.size))
        else:
            out.append(file_entry(app, d, obj, d.size))
    res = {"path": d.path, "bytes": d.size, "size": human(d.size), "total_items": len(items),
           "shown": len(out), "items": out}
    note = _scan_note(s)
    if note:
        res["note"] = note
    return res


def explain(app, path):
    s = _scan(app)
    info = app.info(path)
    if info is None:
        raise AnalysisError(f"扫描结果中没有这个路径：{path}（当前扫描范围是 {s.root_path}）")
    cls = info["cls"]
    out = {"path": info["path"], "type": info["type"], "bytes": info["size"], "size": human(info["size"]),
           **_cat(cls), "reason": info.get("reason")}
    if info["type"] == "dir":
        node, _ = s.find(info["path"])
        out.update(files=info["nfiles"], dirs=info["ndirs"])
        if info.get("running"):
            out["in_use_files"] = info["running"]
        out.update(_safety(cls, info.get("running", 0)))
        out["largest_children"] = list_dir(app, info["path"], 8)["items"]
        if node is not None and node.parent is not None:
            out["percent_of_parent"] = _pct(node.size, node.parent.size)
    else:
        if info.get("static_cls") is not None and info["static_cls"] != cls:
            out["static_category"] = KEYS[info["static_cls"]]
        if info.get("pkg"):
            out["package"] = info["pkg"]
        if info.get("nprocs"):
            out["used_by"] = info["procs"][:10]
            out["used_by_count"] = info["nprocs"]
        out.update(_safety(cls))
        if info.get("apparent") is not None:
            out["file_length_bytes"] = info["apparent"]
    for k in ("mtime", "mode", "owner", "group", "mount"):
        if info.get(k):
            out["modified" if k == "mtime" else k] = info[k]
    if info.get("gone"):
        out["note"] = "该项目在扫描后已被删除或移动。"
    if info.get("denied"):
        out["note"] = "没有权限读取此文件夹，大小未计入。"
    return out


def cleanup(app, path=None, limit=30, min_bytes=1 << 20):
    """可清理候选：缓存/临时类的最大子树（不重复计算），以及零散的日志/临时文件。"""
    s = _scan(app)
    d = _dir(app, path)
    limit = max(1, min(int(limit), 200))
    cands = []   # (size, kind, dir, file)
    if d.cls == CACHE:
        # 起点本身就是缓存目录：列出它的直接子项，粒度更细
        for c in d.dirs.values():
            cands.append((c.size, "d", c, None))
        for f in d.files:
            cands.append((f[1], "f", d, f))
    else:
        stack = [d]
        while stack:
            x = stack.pop()
            for c in x.dirs.values():
                if c.cls == CACHE:
                    cands.append((c.size, "d", c, None))   # 不再深入，避免重复计算
                else:
                    stack.append(c)
            if x.cls != CACHE:
                for f in x.files:
                    if f[2] == CACHE:
                        cands.append((f[1], "f", x, f))
    total = sum(c[0] for c in cands)
    cands = [c for c in cands if c[0] >= min_bytes]
    cands.sort(key=lambda c: -c[0])
    items = []
    for size, kind, x, f in cands[:limit]:
        if kind == "d":
            items.append(dir_entry(app, x))
        elif len(f) == 4:
            items.append(_group_entry(x, f))
        else:
            items.append(file_entry(app, x, f))
    res = {"path": d.path, "reclaimable_bytes": total, "reclaimable_size": human(total),
           "candidates_shown": len(items), "candidates": items,
           "note": "只是建议：DiskMap 不会删除任何文件。删除前请让用户确认；标记为 in_use 的项目需先关闭相关程序。"}
    scan_note = _scan_note(s)
    if scan_note:
        res["note"] = scan_note + res["note"]
    return res


def largest_files(app, path=None, limit=20, category=None):
    s = _scan(app)
    d = _dir(app, path)
    limit = max(1, min(int(limit), 200))
    if category is not None and category not in KEYS:
        raise AnalysisError(f"未知类别：{category}，可选 {', '.join(KEYS)}")
    cid = KEYS.index(category) if category else None
    if cid == RUNNING:
        prefix = _prefix(d.path)
        hits = sorted((h for h in s.running_hits() if pkey(h[0]).startswith(prefix)), key=lambda h: -h[1])
        files = []
        for p, _sz, _c in hits[:limit]:
            parent, f = s.find(p)
            if f is not None:
                files.append(file_entry(app, parent, f))
    else:
        gen = ((f[1], i, x, f) for i, (x, f) in enumerate(_walk_files(d))
               if cid is None or f[2] == cid)
        files = [file_entry(app, x, f) for _sz, _i, x, f in heapq.nlargest(limit, gen)]
    res = {"path": d.path, "category": category, "files": files}
    note = _scan_note(s)
    if note:
        res["note"] = note
    return res


def user_view(app):
    st = app.ui_state
    if not st or not app.last_seen or time.time() - app.last_seen > 30:
        raise AnalysisError("用户没有打开 DiskMap 窗口（或窗口里还没有扫描结果），无法获取用户正在看的内容。")
    out = {"view": st.get("view"), "selected": st.get("selected"),
           "highlighted_category": st.get("pinned"),
           "updated_seconds_ago": round(time.time() - st.get("time", 0), 1)}
    if st.get("view"):
        try:
            out["view_summary"] = summary(app, st["view"], top=5)
        except AnalysisError:
            pass
    if st.get("selected"):
        try:
            out["selected_detail"] = explain(app, st["selected"])
        except AnalysisError:
            pass
    return out


def show(app, path, note=None):
    s = _scan(app)
    node, f = s.find(path)
    if node is None:
        raise AnalysisError(f"扫描结果中没有这个路径：{path}")
    target = node.path if f is None else join_path(node.path, f[0])
    with app.lock:
        prev = app.command["id"] if app.command else 0
        app.command = {"id": prev + 1, "action": "show", "path": target,
                       "note": (note or "")[:200], "time": time.time()}
    connected = bool(app.last_seen and time.time() - app.last_seen < 15)
    return {"ok": True, "path": target, "window_connected": connected,
            "message": "已让 DiskMap 窗口跳转并高亮该项目。" if connected
            else "已记录，但 DiskMap 窗口当前似乎没有打开，用户打开后才能看到。"}


# ------------------------------------------------------------------ 命令行报告
def report_markdown(app, path=None, top=15):
    s = _scan(app)
    st = status(app)["scan"]
    sm = summary(app, path, top)
    cl = cleanup(app, path, limit=top)
    L = []
    L.append(f"# DiskMap 磁盘分析：{sm['path']}")
    L.append("")
    L.append(f"- 总大小：**{sm['size']}**（{sm['files']:,} 个文件，{sm['dirs']:,} 个文件夹）")
    L.append(f"- 扫描：{st['finished_at'] or '进行中'}，用时 {st['elapsed_seconds']} 秒，"
             f"按{'文件大小' if s.apparent else '实际占用空间'}统计")
    if st.get("denied_dirs"):
        L.append(f"- 注意：{st['denied_note']}")
    if sm.get("note"):
        L.append(f"- 注意：{sm['note']}")
    L.append(f"- 可清理（缓存/临时）：约 **{cl['reclaimable_size']}**")
    L.append(f"- 正被程序使用的文件：{sm['in_use']['files']} 个，共 {sm['in_use']['size']}")
    L.append("")
    L.append("## 按重要程度")
    L.append("")
    L.append("| 类别 | 大小 | 占比 |")
    L.append("|---|---:|---:|")
    for c in sm["by_category"]:
        L.append(f"| {c['category_label']} | {c['size']} | {c['percent']}% |")
    L.append("")
    L.append("## 最大的子项")
    L.append("")
    L.append("| 路径 | 大小 | 占比 | 类别 | 原因 |")
    L.append("|---|---:|---:|---|---|")
    for e in sm["largest_children"]:
        name = e["note"] if e["type"] == "group" else e["path"]
        L.append(f"| `{name}` | {e['size']} | {e.get('percent_of_parent', '')}% | "
                 f"{e['category_label']} | {e.get('reason') or ''} |")
    L.append("")
    L.append("## 最大的文件")
    L.append("")
    L.append("| 文件 | 大小 | 类别 | 说明 |")
    L.append("|---|---:|---|---|")
    for e in sm["largest_files"]:
        extra = e.get("used_by") and f"正被 {e['used_by']} 使用" or e.get("package") and f"软件包 {e['package']}" or e.get("reason") or ""
        L.append(f"| `{e['path']}` | {e['size']} | {e['category_label']} | {extra} |")
    L.append("")
    L.append("## 可清理候选")
    L.append("")
    if cl["candidates"]:
        L.append("| 路径 | 大小 | 原因 | 备注 |")
        L.append("|---|---:|---|---|")
        for e in cl["candidates"]:
            name = e["note"] if e["type"] == "group" else e["path"]
            busy = f"{e['in_use_files']} 个文件正在使用" if e.get("in_use_files") else ""
            L.append(f"| `{name}` | {e['size']} | {e.get('reason') or ''} | {busy} |")
    else:
        L.append("没有找到明显的缓存/临时文件。")
    L.append("")
    L.append("> DiskMap 只读取磁盘，不会删除任何文件。以上清理建议请在确认后手动处理。")
    return "\n".join(L) + "\n"


def report_json(app, path=None, top=15):
    return {"status": status(app), "summary": summary(app, path, top),
            "cleanup": cleanup(app, path, limit=top)}
