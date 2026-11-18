"""直接从生成的数据检查结构和数值，不读取另一实现的输出。"""

import os
import stat
import time

from support import CATEGORIES, OS_NAME, human


LABELS = {
    "en": ("System core", "In use", "System files", "Settings", "Personal files", "Cache / temp", "App data / other"),
    "zh": ("系统核心", "正在使用", "系统文件", "配置文件", "用户数据", "缓存/临时", "其他/应用数据"),
}
CACHE_REASON = {"en": "Cache folder", "zh": "缓存文件夹"}
CACHE_ADVICE = {
    "en": "Cache / temporary files: usually safe to clean; programs recreate them as needed.",
    "zh": "缓存/临时文件：通常可以安全清理，程序会按需重新生成。",
}
NO_SCAN = {
    "en": "Nothing has been scanned yet. Call diskmap_scan on a folder or drive first.",
    "zh": "还没有扫描结果。请先调用 diskmap_scan 扫描一个文件夹或磁盘。",
}
NO_VIEW = {
    "en": "The DiskMap window is not open (or has no scan yet), so what the user is looking at is unknown.",
    "zh": "用户没有打开 DiskMap 窗口（或窗口里还没有扫描结果），无法获取用户正在看的内容。",
}


class ContractAssertions:
    def assert_number(self, value, *, integer=False):
        self.assertIn(type(value), (int,) if integer else (int, float))
        self.assertGreaterEqual(value, 0)

    def assert_json_headers(self, response):
        self.assertEqual(response.headers["content-type"], "application/json; charset=utf-8")
        self.assertEqual(response.headers["cache-control"], "no-store")
        self.assertEqual(response.headers["x-content-type-options"], "nosniff")
        self.assertEqual(int(response.headers["content-length"]), len(response.raw))

    def assert_error(self, response, status, message=None):
        self.assertEqual(response.status, status, response.raw)
        self.assert_json_headers(response)
        self.assertEqual(set(response.data), {"error"})
        self.assertIsInstance(response.data["error"], str)
        self.assertTrue(response.data["error"])
        if message is not None:
            self.assertEqual(response.data, {"error": message})

    def assert_status(self, data, fixture=None, *, state="done", apparent=True, cross_fs=False):
        base = {"ready", "os", "admin", "pkg", "procs", "command", "agent_seen_ago", "state"}
        scan = {"error", "root", "files", "dirs", "bytes", "denied", "denied_samples", "current", "elapsed",
                "apparent", "cross_fs", "fs_used", "mounts", "skipped"}
        self.assertEqual(set(data), base | (scan if fixture else set()))
        self.assertIs(data["ready"], True)
        self.assertEqual(data["os"], OS_NAME)
        self.assertIsInstance(data["admin"], bool)
        self.assertEqual(data["state"], state)
        self.assertEqual(set(data["pkg"]), {"manager", "label", "packages", "files", "error"})
        for key in ("packages", "files"):
            self.assert_number(data["pkg"][key], integer=True)
        self.assertEqual(set(data["procs"]), {"total", "visible", "files", "refreshing", "error"})
        for key in ("total", "visible", "files"):
            self.assert_number(data["procs"][key], integer=True)
        self.assertIsInstance(data["procs"]["refreshing"], bool)
        if data["agent_seen_ago"] is not None:
            self.assert_number(data["agent_seen_ago"])
        if fixture:
            self.assertEqual(data["root"], str(fixture.root))
            self.assertEqual((data["files"], data["dirs"]), fixture.counts())
            self.assertEqual(data["bytes"], fixture.size())
            self.assertEqual(data["denied"], 0)
            self.assertEqual(data["denied_samples"], [])
            self.assertIsNone(data["error"])
            self.assertEqual(data["current"], "")
            self.assertIs(data["apparent"], apparent)
            self.assertIs(data["cross_fs"], cross_fs)
            self.assert_number(data["elapsed"])
            self.assert_number(data["fs_used"], integer=True)
            self.assertEqual(data["mounts"], [])
            self.assertEqual(data["skipped"], [])

    def assert_agent_status(self, data, fixture=None, *, source=None, cross_fs=False):
        required = {"version", "os", "admin", "ready", "window_connected", "processes", "scan"}
        if source:
            required |= {"connected_to", "connected_to_label"}
            self.assertEqual(data["connected_to"], source)
            self.assertIsInstance(data["connected_to_label"], str)
            self.assertTrue(data["connected_to_label"])
        self.assertTrue(required <= data.keys(), data)
        self.assertTrue(set(data) <= required | {"user_view", "hint"}, data)
        self.assertEqual(data["os"], OS_NAME)
        self.assertIsInstance(data["version"], str)
        self.assertTrue(data["version"])
        self.assertIsInstance(data["admin"], bool)
        self.assertIsInstance(data["ready"], bool)
        self.assertIsInstance(data["window_connected"], bool)
        self.assertEqual(set(data["processes"]), {"total", "in_use_files", "visible_processes"})
        for value in data["processes"].values():
            self.assert_number(value, integer=True)
        if fixture is None:
            self.assertIsNone(data["scan"])
            self.assertIsInstance(data["hint"], str)
            return
        scan = data["scan"]
        self.assertEqual(set(scan), {"root", "state", "files", "dirs", "bytes", "size", "elapsed_seconds",
                                     "finished_at", "current", "denied_dirs", "disk_used_bytes", "size_mode",
                                     "includes_other_partitions"})
        self.assertEqual(scan["root"], str(fixture.root))
        self.assertEqual(scan["state"], "done")
        self.assertEqual((scan["files"], scan["dirs"]), fixture.counts())
        self.assertEqual(scan["bytes"], fixture.size())
        self.assertEqual(scan["size"], human(fixture.size()))
        self.assert_number(scan["elapsed_seconds"])
        time.strptime(scan["finished_at"], "%Y-%m-%d %H:%M:%S")
        self.assertIsNone(scan["current"])
        self.assertEqual(scan["denied_dirs"], 0)
        if scan["disk_used_bytes"] is not None:
            self.assert_number(scan["disk_used_bytes"], integer=True)
        self.assertEqual(scan["size_mode"], "apparent")
        self.assertIs(scan["includes_other_partitions"], cross_fs)

    def assert_entry(self, entry, fixture, relative, *, language="en", parent_size=None):
        kind = "file" if relative in fixture.sizes else "dir"
        size = fixture.size(relative)
        expected = {"path": str(fixture.path(relative)), "type": kind, "bytes": size, "size": human(size),
                    "category": "cache", "category_label": LABELS[language][5],
                    "reason": CACHE_REASON[language], "safety": "safe_to_clean"}
        for key, value in expected.items():
            self.assertEqual(entry[key], value, (relative, key, entry))
        required = set(expected) | {"advice"}
        if kind == "dir":
            required |= {"files", "dirs"}
            self.assertEqual((entry["files"], entry["dirs"]), fixture.counts(relative))
        if parent_size:
            required.add("percent_of_parent")
            self.assertEqual(entry["percent_of_parent"], round(size / parent_size * 100, 1))
        # Linux 的采集器也登记目录 FD；异步扫描中捕获的目录数属于实时进程信息。
        count = entry.get("in_use_files", 0)
        if count:
            self.assertEqual(kind, "dir")
            self.assert_number(count, integer=True)
            self.assertGreater(count, 0)
        suffix = ((f" {count} files inside are in use; close those programs before cleaning." if language == "en"
                   else f"其中有 {count} 个文件正被程序使用，清理前先关闭相关程序。") if count else "")
        self.assertEqual(entry["advice"], CACHE_ADVICE[language] + suffix)
        self.assertEqual(set(entry), required | ({"in_use_files"} if count else set()))

    def assert_list(self, data, fixture, relative="", *, language="en", limit=50):
        children = sorted(fixture.children(relative), key=lambda row: -row[1])
        self.assertEqual(set(data), {"path", "bytes", "size", "total_items", "shown", "items"})
        self.assertEqual(data["path"], str(fixture.path(relative)))
        self.assertEqual(data["bytes"], fixture.size(relative))
        self.assertEqual(data["size"], human(fixture.size(relative)))
        self.assertEqual(data["total_items"], len(children))
        self.assertEqual(data["shown"], min(len(children), limit))
        self.assertEqual(len(data["items"]), data["shown"])
        for item, (name, _, _) in zip(data["items"], children[:limit]):
            self.assert_entry(item, fixture, name, language=language, parent_size=fixture.size(relative))

    def assert_largest(self, data, fixture, relative="", *, language="en", limit=20, category=None):
        self.assertEqual(set(data), {"path", "category", "files"})
        self.assertEqual(data["path"], str(fixture.path(relative)))
        self.assertEqual(data["category"], category)
        expected = fixture.retained_files(relative)[:limit] if category in (None, "cache") else []
        self.assertEqual(len(data["files"]), len(expected))
        for entry, (name, _) in zip(data["files"], expected):
            self.assert_entry(entry, fixture, name, language=language)

    def assert_cleanup(self, data, fixture, relative="", *, language="en", limit=30, min_bytes=1 << 20):
        self.assertEqual(set(data), {"path", "reclaimable_bytes", "reclaimable_size", "candidates_shown",
                                     "candidates", "note"})
        self.assertEqual(data["path"], str(fixture.path(relative)))
        self.assertEqual(data["reclaimable_bytes"], fixture.size(relative))
        self.assertEqual(data["reclaimable_size"], human(fixture.size(relative)))
        expected = sorted([row for row in fixture.children(relative) if row[1] >= min_bytes],
                          key=lambda row: -row[1])[:limit]
        self.assertEqual(data["candidates_shown"], len(expected))
        self.assertEqual(len(data["candidates"]), len(expected))
        for entry, (name, _, _) in zip(data["candidates"], expected):
            self.assert_entry(entry, fixture, name, language=language)
        self.assertEqual(data["note"], {
            "en": "Suggestions only: DiskMap never deletes files. Get the user's confirmation before deleting; "
                  "close the programs using items marked in_use first.",
            "zh": "只是建议：DiskMap 不会删除任何文件。删除前请让用户确认；标记为 in_use 的项目需先关闭相关程序。",
        }[language])

    def assert_summary(self, data, fixture, relative="", *, language="en", top=10):
        size = fixture.size(relative)
        self.assertEqual(set(data), {"path", "bytes", "size", "files", "dirs", "category", "category_label",
                                     "reason", "by_category", "largest_children", "largest_files", "cleanup", "in_use"})
        self.assertEqual(data["path"], str(fixture.path(relative)))
        self.assertEqual(data["bytes"], size)
        self.assertEqual(data["size"], human(size))
        self.assertEqual((data["files"], data["dirs"]), fixture.counts(relative))
        self.assertEqual((data["category"], data["category_label"], data["reason"]),
                         ("cache", LABELS[language][5], CACHE_REASON[language]))
        self.assertEqual(data["by_category"], [{"category": "cache", "category_label": LABELS[language][5],
                                               "bytes": size, "size": human(size), "percent": 100.0}] if size else [])
        children = sorted(fixture.children(relative), key=lambda row: -row[1])[:top]
        self.assertEqual(len(data["largest_children"]), len(children))
        for entry, (name, _, _) in zip(data["largest_children"], children):
            self.assert_entry(entry, fixture, name, language=language, parent_size=size)
        self.assert_largest({"path": data["path"], "category": None, "files": data["largest_files"]},
                            fixture, relative, language=language, limit=top)
        self.assertEqual(set(data["cleanup"]), {"reclaimable_bytes", "reclaimable_size", "top_candidates"})
        self.assertEqual(data["cleanup"]["reclaimable_bytes"], size)
        self.assertEqual(data["cleanup"]["reclaimable_size"], human(size))
        clean = [row for row in children if row[1] >= 1 << 20][:min(top, 10)]
        self.assertEqual(len(data["cleanup"]["top_candidates"]), len(clean))
        for entry, (name, _, _) in zip(data["cleanup"]["top_candidates"], clean):
            self.assert_entry(entry, fixture, name, language=language)
        self.assertEqual(data["in_use"], {"files": 0, "bytes": 0, "size": "0 B", "largest": []})

    def assert_info(self, data, fixture, relative, *, language="en"):
        path = fixture.path(relative)
        kind = "file" if relative in fixture.sizes else "dir"
        self.assertEqual(data["path"], str(path))
        self.assertEqual(data["type"], kind)
        self.assertEqual(data["name"], path.name if relative else str(path))
        self.assertEqual(data["size"], fixture.size(relative))
        self.assertEqual(data["cls"], 5)
        self.assertEqual(data["reason"], CACHE_REASON[language])
        required = {"path", "name", "type", "size", "cls", "reason", "mtime", "mode"}
        if kind == "file":
            required |= {"static_cls", "pkg", "procs", "nprocs", "apparent"}
            self.assertEqual(data["static_cls"], 5)
            self.assertIsNone(data["pkg"])
            self.assertEqual(data["procs"], [])
            self.assertEqual(data["nprocs"], 0)
            self.assertEqual(data["apparent"], fixture.size(relative))
        else:
            required |= {"nfiles", "ndirs", "running", "denied"}
            self.assertEqual((data["nfiles"], data["ndirs"]), fixture.counts(relative))
            self.assert_number(data["running"], integer=True)
            self.assertIs(data["denied"], False)
        metadata = path.stat()
        self.assertEqual(data["mode"], stat.filemode(metadata.st_mode))
        self.assertEqual(data["mtime"], time.strftime("%Y-%m-%d %H:%M", time.localtime(metadata.st_mtime)))
        if os.name != "nt":
            import grp
            import pwd
            required |= {"owner", "group"}
            try:
                owner = pwd.getpwuid(metadata.st_uid).pw_name
            except KeyError:
                owner = str(metadata.st_uid)
            try:
                group = grp.getgrgid(metadata.st_gid).gr_name
            except KeyError:
                group = str(metadata.st_gid)
            self.assertEqual(data["owner"], owner)
            self.assertEqual(data["group"], group)
        self.assertEqual(set(data), required)

    def assert_mcp_result(self, result, *, error=None):
        self.assertIsInstance(result["isError"], bool)
        self.assertEqual(len(result["content"]), 1)
        self.assertEqual(set(result["content"][0]), {"type", "text"})
        self.assertEqual(result["content"][0]["type"], "text")
        if error is not None:
            self.assertIs(result["isError"], True)
            self.assertEqual(set(result), {"content", "isError"})
            self.assertEqual(result["content"][0]["text"], error)
            return None
        import json
        self.assertIs(result["isError"], False, result)
        self.assertEqual(set(result), {"content", "structuredContent", "isError"})
        self.assertEqual(json.loads(result["content"][0]["text"]), result["structuredContent"])
        return result["structuredContent"]

    def assert_explain_file(self, data, fixture, relative, *, language="en"):
        metadata = fixture.path(relative).stat()
        detail_keys = {"file_length_bytes", "modified", "mode"}
        if os.name != "nt":
            detail_keys |= {"owner", "group"}
        entry = {key: value for key, value in data.items() if key not in detail_keys}
        self.assert_entry(entry, fixture, relative, language=language)
        self.assertEqual(data["file_length_bytes"], fixture.size(relative))
        self.assertEqual(data["modified"], time.strftime("%Y-%m-%d %H:%M", time.localtime(metadata.st_mtime)))
        self.assertEqual(data["mode"], stat.filemode(metadata.st_mode))
        if os.name != "nt":
            import pwd
            import grp
            try:
                owner = pwd.getpwuid(metadata.st_uid).pw_name
            except KeyError:
                owner = str(metadata.st_uid)
            try:
                group = grp.getgrgid(metadata.st_gid).gr_name
            except KeyError:
                group = str(metadata.st_gid)
            self.assertEqual(data["owner"], owner)
            self.assertEqual(data["group"], group)
