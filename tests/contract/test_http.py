"""真实 HTTP 服务的全部界面和 agent 路由。"""

from contextlib import ExitStack
import json
import mmap
import os
from pathlib import Path
import signal
import stat
import sys
import unittest

from assertions import LABELS, NO_SCAN, NO_VIEW, ContractAssertions
from support import (CATEGORIES, LANGUAGES, MISSING, OS_NAME, REPO, TOKEN, Workspace, allocated_sizes,
                     current_process_name, human, legacy_windows_proc_skip_reason, url, write)


class HttpContract(ContractAssertions, unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = Workspace()
        cls.addClassCleanup(cls.workspace.close)
        cls.fixture = cls.workspace.fixture
        cls.server = cls.workspace.server()
        cls.server.scan(cls.fixture.root)

    def setUp(self):
        self.f = self.fixture
        self.s = self.server
        status = self.s.json("/api/status")
        if (status.get("root"), status.get("state"), status.get("apparent"), status.get("cross_fs")) != (
                str(self.f.root), "done", True, False):
            self.s.scan(self.f.root)

    def error(self, route, status, message, body=MISSING, language="en"):
        response = self.s.request(route, body, headers={"X-DiskMap-Lang": language})
        self.assert_error(response, status, message)

    def test_authentication_all_routes(self):
        gets = ("meta", "status", "tree", "info", "agent/status", "agent/summary", "agent/list",
                "agent/explain", "agent/cleanup", "agent/largest", "agent/view", "agent/missing")
        posts = ("scan", "cancel", "refresh-procs", "reveal", "settings", "ui-state", "agent/scan", "agent/show")
        for route, body in [(name, MISSING) for name in gets] + [(name, {}) for name in posts]:
            for headers, error in (({"X-DiskMap-Token": None}, "bad token"),
                                   ({"X-DiskMap-Token": "wrong"}, "bad token"),
                                   ({"Host": "attacker.invalid"}, "bad host"),
                                   ({"Host": None}, "bad host")):
                with self.subTest(route=route, headers=headers):
                    self.assert_error(self.s.request("/api/" + route, body, headers=headers), 403, error)
        for host in (f"127.0.0.1:{self.s.port}", f"localhost:{self.s.port}", f"[::1]:{self.s.port}"):
            response = self.s.request("/api/status", headers={"Host": host})
            self.assertEqual(response.status, 200)
            self.assert_json_headers(response)
        for headers in ({"Host": "localhost"}, {"Host": f"localhost.evil:{self.s.port}"},
                        {"Host": f"127.0.0.1:{self.s.port} "}, {"Host": f"127.0.0.1:{self.s.port + 1}"}):
            self.assert_error(self.s.request("/api/status", headers=headers), 403, "bad host")
        self.assert_error(self.s.request(url("/api/status", t=TOKEN), headers={"X-DiskMap-Token": None}),
                          403, "bad token")
        self.assert_error(self.s.request("/api/status", headers={"X-DiskMap-Token": TOKEN + " "}),
                          403, "bad token")
        self.assert_error(self.s.request("/?t=" + TOKEN, headers={"Host": "evil"}), 403, "bad host")

    def test_frontend_and_static_files(self):
        for token in (None, "wrong"):
            response = self.s.request("/" if token is None else "/?t=" + token,
                                      headers={"X-DiskMap-Token": None})
            self.assertEqual(response.status, 403)
            self.assertIn("text/html", response.headers["content-type"])
            self.assertIn(b"access token", response.raw)
        for language in LANGUAGES:
            response = self.s.request("/?t=" + TOKEN, headers={"X-DiskMap-Token": None,
                                                              "X-DiskMap-Lang": language})
            self.assertEqual(response.status, 200)
            self.assertIn(f'<html lang="{"en" if language == "en" else "zh-CN"}" data-lang="{language}">'.encode(),
                          response.raw)
            self.assertIn(('"' + TOKEN + '"').encode(), response.raw)
            self.assertNotIn(b"__DISKMAP_TOKEN__", response.raw)
            self.assertNotIn(b"__DISKMAP_LANG__", response.raw)
            self.assertEqual(response.headers["referrer-policy"], "no-referrer")
            self.assertEqual(response.headers["cache-control"], "no-store")
        types = {"index.html": "text/html; charset=utf-8", "app.js": "text/javascript; charset=utf-8",
                 "style.css": "text/css; charset=utf-8", "favicon.png": "image/png"}
        for filename, content_type in types.items():
            response = self.s.request("/static/" + filename,
                                      headers={"X-DiskMap-Token": None, "Host": "arbitrary.invalid"})
            self.assertEqual(response.status, 200)
            self.assertEqual(response.raw, (REPO / "web" / filename).read_bytes())
            self.assertEqual(response.headers["content-type"], content_type)
            self.assertEqual(response.headers["cache-control"], "no-store")
            self.assertEqual(response.headers["x-content-type-options"], "nosniff")
        self.assertEqual(self.s.request("/static/../../app.js").raw, (REPO / "web/app.js").read_bytes())
        for route in ("/static/../README.md", "/static/../../Cargo.toml", "/static/%2e%2e%2fCargo.toml",
                      "/static/..%5cCargo.toml", "/static/no.js", "/static/app%2ejs", "/static/"):
            self.error(route, 404, "not found")

    def test_meta_both_languages(self):
        for language in LANGUAGES:
            response = self.s.request("/api/meta", headers={"X-DiskMap-Lang": language})
            self.assertEqual(response.status, 200)
            self.assert_json_headers(response)
            if language == "zh":
                self.assertIn("系统核心".encode("utf-8"), response.raw)
                self.assertNotIn(b"\\u7cfb", response.raw)
            data = response.data
            self.assertEqual(set(data), {"classes", "home", "disks", "os", "sep", "system_root", "admin",
                                         "version", "agent", "lang"})
            self.assertEqual(data["lang"], language)
            self.assertEqual(data["os"], OS_NAME)
            self.assertEqual(data["sep"], os.sep)
            self.assertEqual(data["home"], str(Path.home()))
            self.assertTrue(Path(data["system_root"]).is_absolute())
            self.assertIsInstance(data["admin"], bool)
            self.assertIsInstance(data["version"], str)
            self.assertTrue(data["version"])
            self.assertEqual(len(data["classes"]), 7)
            for index, entry in enumerate(data["classes"]):
                self.assertEqual(set(entry), {"id", "key", "label", "desc"})
                self.assertEqual((entry["id"], entry["key"], entry["label"]),
                                 (index, CATEGORIES[index], LABELS[language][index]))
                self.assertIsInstance(entry["desc"], str)
                self.assertTrue(entry["desc"])
            self.assertTrue(data["disks"])
            for disk in data["disks"]:
                self.assertTrue({"path", "total", "used", "free"} <= disk.keys(), disk)
                self.assertTrue(Path(disk["path"]).is_absolute())
                for key in ("total", "used", "free"):
                    self.assert_number(disk[key], integer=True)
                self.assertGreaterEqual(disk["total"], disk["used"])
            agent = data["agent"]
            self.assertEqual(set(agent), {"command", "claude_code", "json", "report"})
            self.assertEqual(agent["command"][-1], "mcp")
            self.assertEqual(json.loads(agent["json"]), {"mcpServers": {"diskmap": {
                "command": agent["command"][0], "args": agent["command"][1:]}}})
            self.assertIn(" mcp add ", agent["claude_code"])
            self.assertIn("analyze", agent["report"])

    def test_scan_status_and_no_modification(self):
        _, done = self.s.scan(self.f.root)
        self.assert_status(done, self.f)
        self.assert_agent_status(self.s.json("/api/agent/status"), self.f)
        response = self.s.request("/api/status")
        self.assert_json_headers(response)
        # 文件内容、数量和长度独立于接口检查，防止读接口意外写入测试树。
        self.assertEqual({str(p.relative_to(self.f.root)).replace(os.sep, "/")
                          for p in self.f.root.rglob("*") if p.is_file()}, set(self.f.sizes))
        for relative, size in self.f.sizes.items():
            self.assertEqual(self.f.path(relative).read_bytes(), b"x" * size)

    def test_tree_totals_grouping_and_fraction(self):
        self.s.refresh_procs()
        for relative in ("", "proj", "many", "empty"):
            for fraction in (0, 1e-6, 0.05, 1):
                with self.subTest(relative=relative, fraction=fraction):
                    data = self.s.get("/api/tree", path=self.f.path(relative), frac=fraction)
                    self.assertEqual(set(data), {"path", "root", "state", "totals", "tree"})
                    self.assertEqual((data["path"], data["root"], data["state"]),
                                     (str(self.f.path(relative)), str(self.f.root), "done"))
                    self.assertEqual(data["totals"], [0, 0, 0, 0, 0, self.f.size(relative), 0])
                    threshold = max(1, int(self.f.size(relative) * min(max(fraction, 1e-6), 0.05)))
                    self.check_tree(data["tree"], relative, threshold)
        self.assertEqual(self.s.get("/api/tree", frac=0)["tree"], self.s.get("/api/tree", frac=1e-6)["tree"])
        self.assertEqual(self.s.get("/api/tree", frac=1)["tree"], self.s.get("/api/tree", frac=0.05)["tree"])

    def check_tree(self, node, relative, threshold):
        files, dirs = self.f.counts(relative)
        for key, value in {"n": str(self.f.root) if not relative else self.f.path(relative).name,
                           "s": self.f.size(relative), "c": 5, "t": "d", "nf": files, "nd": dirs}.items():
            self.assertEqual(node[key], value)
        self.assertEqual(set(node), {"n", "s", "c", "t", "nf", "nd", "ch"} | ({"r"} if "r" in node else set()))
        if "r" in node:
            self.assert_number(node["r"], integer=True)
            self.assertGreater(node["r"], 0)
        rows = self.f.children(relative)
        file_rows = sorted([row for row in rows if row[2] == "file"], key=lambda row: -row[1])
        rows = [row for row in rows if row[2] == "dir"] + file_rows[:150]
        if len(file_rows) > 150:
            rows.append(("", sum(row[1] for row in file_rows[150:]), len(file_rows) - 150))
        expected, small_size, small_count = [], 0, 0
        for name, size, kind in rows:
            count = kind if type(kind) is int else 1
            if size < threshold:
                small_size += size
                small_count += count
            elif type(kind) is int:
                expected.append({"n": "", "s": size, "c": 5, "t": "a", "k": count})
            elif kind == "file":
                expected.append({"n": self.f.path(name).name, "s": size, "c": 5, "t": "f"})
            else:
                expected.append((name, size))
        if small_size or small_count:
            expected.append({"n": "", "s": small_size, "c": 5, "t": "a", "k": small_count})
        expected.sort(key=lambda row: -(row[1] if isinstance(row, tuple) else row["s"]))
        self.assertEqual(len(node["ch"]), len(expected))
        self.assertEqual(sum(child["s"] for child in node["ch"]), node["s"])
        for actual, wanted in zip(node["ch"], expected):
            if isinstance(wanted, tuple):
                self.check_tree(actual, wanted[0], threshold)
            else:
                self.assertEqual(actual, wanted)

    def test_info_and_complete_index_difference(self):
        for language in LANGUAGES:
            for relative in ("", "proj", "docs/Report.pdf", "中文 空格+&.txt", "empty/zero", "many/f159.bin"):
                self.assert_info(self.s.get("/api/info", path=self.f.path(relative), language=language),
                                 self.f, relative, language=language)
            hidden = url("/api/info", path=self.f.path("many/f000.bin"))
            if self.workspace.impl == "rust":  # T01/T05：完整索引保留小文件详情。
                self.assert_info(self.s.get("/api/info", path=self.f.path("many/f000.bin"), language=language),
                                 self.f, "many/f000.bin", language=language)
            else:
                self.error(hidden, 404, "No scan results for this path" if language == "en"
                           else "没有这个路径的扫描结果", language=language)

    def test_agent_analysis_both_languages(self):
        for language in LANGUAGES:
            for relative in ("", "proj", "docs", "empty"):
                with self.subTest(language=language, relative=relative):
                    params = {"path": self.f.path(relative), "language": language}
                    self.assert_summary(self.s.get("/api/agent/summary", top=3, **params),
                                        self.f, relative, language=language, top=3)
                    self.assert_list(self.s.get("/api/agent/list", limit=3, **params),
                                     self.f, relative, language=language, limit=3)
                    self.assert_cleanup(self.s.get("/api/agent/cleanup", min_mb=0, **params),
                                        self.f, relative, language=language, min_bytes=0)
                    self.assert_largest(self.s.get("/api/agent/largest", limit=3, **params),
                                        self.f, relative, language=language, limit=3)
            self.assert_explain_file(self.s.get("/api/agent/explain", path=self.f.path("docs/Report.pdf"),
                                               language=language), self.f, "docs/Report.pdf", language=language)
            self.assert_cleanup(self.s.get("/api/agent/cleanup", language=language), self.f, language=language)

    def test_agent_limits_categories_and_aggregation(self):
        for category in CATEGORIES:
            self.assert_largest(self.s.get("/api/agent/largest", category=category, limit=200),
                                self.f, limit=200, category=category)
        for text, limit in (("-9", 1), ("0", 1), ("٢", 2), ("３", 3), ("1_0", 10), ("9" * 60, 500)):
            self.assert_list(self.s.get("/api/agent/list", limit=text), self.f, limit=limit)
        self.assert_summary(self.s.get("/api/agent/summary", top=0), self.f, top=1)
        self.assert_cleanup(self.s.get("/api/agent/cleanup", min_mb=999999), self.f, min_bytes=999999 << 20)
        data = self.s.get("/api/agent/list", path=self.f.path("many"), limit=500)
        self.assertEqual((data["total_items"], data["shown"]), (151, 151))
        groups = [entry for entry in data["items"] if entry["type"] == "group"]
        hidden = [(name, size) for name, size in self.f.files_under("many").items()
                  if name not in dict(self.f.retained_files("many"))]
        self.assertEqual(len(groups), 1)
        self.assertEqual(groups[0], {"path": str(self.f.path("many")), "type": "group", "count": len(hidden),
                                     "bytes": sum(size for _, size in hidden), "size": human(sum(size for _, size in hidden)),
                                     "category": "cache", "category_label": LABELS["en"][5],
                                     "note": f"{len(hidden)} smaller files in {self.f.path('many')}, counted together",
                                     "percent_of_parent": round(sum(size for _, size in hidden) / self.f.size("many") * 100, 1)})
        self.assert_largest(self.s.get("/api/agent/largest", path=self.f.path("many"), limit=200),
                            self.f, "many", limit=200)

    def test_errors_and_error_json(self):
        missing = self.f.path("missing")
        for language in LANGUAGES:
            no_result = "No scan results for this path" if language == "en" else "没有这个路径的扫描结果"
            for route in ("tree", "info"):
                for path in (missing, self.f.root.parent):
                    self.error(url("/api/" + route, path=path), 404, no_result, language=language)
            self.error(url("/api/tree", path=self.f.path("docs/Report.pdf")), 404, no_result, language=language)
            for route in ("summary", "list", "explain", "cleanup", "largest"):
                message = (f"This path is not in the scan results: {missing} (the scan covers {self.f.root})"
                           if language == "en" else f"扫描结果中没有这个路径：{missing}（当前扫描范围是 {self.f.root}）")
                self.error(url("/api/agent/" + route, path=missing), 400, message, language=language)
            message = f"Folder does not exist: {missing}" if language == "en" else f"文件夹不存在：{missing}"
            for route in ("/api/scan", "/api/agent/scan"):
                self.error(route, 400, message, {"path": str(missing)}, language)
            category = ("Unknown category: unknown. Choose one of " if language == "en" else "未知类别：unknown，可选 ")
            self.error(url("/api/agent/largest", category="unknown"), 400,
                       category + ", ".join(CATEGORIES), language=language)
        for route, key in (("summary", "top"), ("list", "limit"), ("cleanup", "limit"), ("largest", "limit")):
            self.error(url("/api/agent/" + route, **{key: "oops"}), 400,
                       "invalid literal for int() with base 10: 'oops'")
        self.error("/api/tree?frac=oops", 400, "could not convert string to float: 'oops'")
        self.error("/api/missing", 404, "not found")
        self.error("/api/agent/missing", 404, "not found")
        self.error("/missing", 404, "not found")
        self.error("/api/missing", 404, "not found", {})
        self.error("/api/agent/show", 500, 'TypeError("\'int\' object is not subscriptable")',
                   {"path": str(self.f.root), "note": 123})
        # T05：Python 3.12 起 slice 可哈希；Rust 固定为 KeyError，不能泛化成任意 500。
        dict_error = ("TypeError(\"unhashable type: 'slice'\")" if self.workspace.impl == "legacy"
                      and sys.version_info < (3, 12) else "KeyError(slice(None, 200, None))")
        self.error("/api/agent/show", 500, dict_error, {"path": str(self.f.root), "note": {"x": 1}})

    def test_ui_state_show_and_bye(self):
        selected = self.f.path("proj/Build.log")
        for language in LANGUAGES:
            self.assertEqual(self.s.json("/api/ui-state", {"view": str(self.f.path("proj")),
                                                            "selected": str(selected), "pinned": "cache"}), {"ok": True})
            view = self.s.get("/api/agent/view", language=language)
            self.assertEqual(set(view), {"view", "selected", "highlighted_category", "updated_seconds_ago",
                                         "view_summary", "selected_detail"})
            self.assertEqual((view["view"], view["selected"], view["highlighted_category"]),
                             (str(self.f.path("proj")), str(selected), "cache"))
            self.assert_number(view["updated_seconds_ago"])
            self.assert_summary(view["view_summary"], self.f, "proj", language=language, top=5)
            self.assert_explain_file(view["selected_detail"], self.f, "proj/Build.log", language=language)
            before = self.s.json("/api/status")["command"]
            note = "中文🙂" * 100
            shown = self.s.json("/api/agent/show", {"path": str(selected), "note": note}, language=language)
            self.assertEqual(shown, {"ok": True, "path": str(selected), "window_connected": True,
                                     "message": "The DiskMap window now shows and highlights this item." if language == "en"
                                     else "已让 DiskMap 窗口跳转并高亮该项目。"})
            command = self.s.json("/api/status")["command"]
            self.assertEqual(set(command), {"id", "action", "path", "note", "time"})
            self.assertEqual((command["id"], command["action"], command["path"], command["note"]),
                             ((before["id"] if before else 0) + 1, "show", str(selected), note[:200]))
            self.assert_number(command["time"])
        self.assertEqual(self.s.request("/api/ui-state", raw=b"{broken").data, {"ok": True})
        view = self.s.json("/api/agent/view")
        self.assertEqual(set(view), {"view", "selected", "highlighted_category", "updated_seconds_ago"})
        for key in ("view", "selected", "highlighted_category"):
            self.assertIsNone(view[key])
        self.assert_error(self.s.request("/api/bye?t=wrong", {}, headers={"X-DiskMap-Token": None}), 403, "bad token")
        self.assert_error(self.s.request("/api/bye?t=" + TOKEN, {}, headers={"Host": "evil"}), 403, "bad token")
        response = self.s.request("/api/bye?t=" + TOKEN, {}, headers={"X-DiskMap-Token": None})
        self.assertEqual((response.status, response.raw), (204, b""))
        self.assertIsNone(self.s.owner.process.poll())

    def test_settings_persistence_and_precedence(self):
        settings = Path(self.s.env["DISKMAP_CONFIG_DIR"]) / "settings.json"
        for language in ("zh", "en"):
            self.assertEqual(self.s.request("/api/settings", {"lang": language}).data, {"lang": language})
            self.assertEqual(json.loads(settings.read_text(encoding="utf-8")), {"lang": language})
            self.assertEqual(self.s.request("/api/meta").data["lang"], language)
        self.error("/api/settings", 400, "unsupported language: xx", {"lang": "xx"})
        self.assertEqual(json.loads(settings.read_text(encoding="utf-8")), {"lang": "en"})
        self.assertEqual(self.s.request("/api/settings", {}).data, {"lang": "en"})
        # 新进程从同一份临时配置读设置；再单独核对 env 和 CLI 的更高优先级。
        self.s.request("/api/settings", {"lang": "zh"})
        self.addCleanup(lambda: self.s.request("/api/settings", {"lang": "en"}))
        for label, env_language, args, expected in (("saved", None, (), "zh"),
                                                   ("env", "en", (), "en"),
                                                   ("cli", "en", ("--lang", "zh"), "zh")):
            env = self.workspace.environment("settings-" + label)
            env["DISKMAP_CONFIG_DIR"] = self.s.env["DISKMAP_CONFIG_DIR"]
            if env_language is None:
                env.pop("DISKMAP_LANG")
            else:
                env["DISKMAP_LANG"] = env_language
            server = self.workspace.server(env, args)
            try:
                self.assertEqual(server.request("/api/meta").data["lang"], expected)
            finally:
                server.owner.close()

    def test_reveal_rejects_unscanned_path_without_opening_a_window(self):
        for language in LANGUAGES:
            admin = self.s.json("/api/meta")["admin"]
            message = ({"en": "Running as root: the file manager is not opened, to avoid starting a graphical program with root rights",
                        "zh": "以 root 身份运行时不打开文件管理器，以免以 root 权限启动图形程序"}[language]
                       if admin and os.name != "nt" else {"en": "Only paths in the scan results can be opened",
                                                          "zh": "只能打开扫描结果中的路径"}[language])
            self.error("/api/reveal", 400, message, {"path": str(self.f.root.parent)}, language)

    def test_cancel_and_restart(self):
        root = self.workspace.base / "cancel/cache"
        for i in range(1000):
            write(root / f"d{i:04}" / "one.bin", 17)
        initial = self.s.json("/api/scan", {"path": str(root), "apparent": True, "cross_fs": False})
        self.assertIn(initial["state"], ("scanning", "done"))
        response = self.s.json("/api/cancel", {})
        self.assertIn(response["state"], ("scanning", "cancelled", "done"))
        done = self.s.wait_done()
        self.assertIn(done["state"], ("cancelled", "done"))
        self.assertIsNone(done["error"])
        self.assertLessEqual(done["files"], 1000)
        self.assertLessEqual(done["dirs"], 1000)
        self.assertEqual(done["bytes"], done["files"] * 17)
        if done["state"] == "done":  # 请求抵达前已经扫完，不能假定线程调度。
            self.assertEqual((done["files"], done["dirs"], done["bytes"]), (1000, 1000, 17000))
        else:
            self.assertEqual(self.s.json("/api/agent/summary")["note"],
                             "The scan was stopped; these numbers are incomplete.")
        _, restarted = self.s.scan(self.f.root)
        self.assert_status(restarted, self.f)

    def test_agent_scan_reuse_options(self):
        for cross_fs in (False, True):
            initial, _ = self.s.scan(self.f.root, agent=True, cross_fs=cross_fs)
            self.assertIs(initial["reused"], False)
            initial, status = self.s.scan(self.f.path("proj"), agent=True, cross_fs=False, rescan=False)
            self.assertIs(initial["reused"], True)
            self.assert_agent_status(status, self.f, cross_fs=cross_fs)
        initial, _ = self.s.scan(self.f.root, agent=True, cross_fs=False)
        self.assertIs(initial["reused"], False)
        initial, status = self.s.scan(self.f.root, agent=True, cross_fs=True, rescan=False)
        self.assertIs(initial["reused"], False)
        self.assert_agent_status(status, self.f, cross_fs=True)
        initial, status = self.s.scan(self.f.root, agent=True, apparent=False, cross_fs=True, rescan=False)
        self.assertIs(initial["reused"], False)
        self.assertEqual(status["scan"]["size_mode"], "allocated")

    def test_allocated_bytes_from_filesystem_metadata(self):
        root = self.workspace.base / "allocated/cache"
        lengths = {"zero": 0, "resident.bin": 17, "small.bin": 777, "medium.bin": 8193, "large.bin": 300003}
        for name, size in lengths.items():
            write(root / name, size)
        expected = allocated_sizes(root, self.workspace.impl)
        self.assertEqual(set(expected), set(lengths))
        _, status = self.s.scan(root, apparent=False)
        self.assertEqual((status["state"], status["files"], status["dirs"], status["bytes"]),
                         ("done", len(lengths), 0, sum(expected.values())))
        agent = self.s.json("/api/agent/status")["scan"]
        self.assertEqual((agent["bytes"], agent["size"], agent["size_mode"]),
                         (sum(expected.values()), human(sum(expected.values())), "allocated"))
        for name, size in expected.items():
            info = self.s.get("/api/info", path=root / name)
            self.assertEqual((info["size"], info["apparent"]), (size, lengths[name]))
        _, apparent = self.s.scan(root, apparent=True)
        self.assertEqual(apparent["bytes"], sum(lengths.values()))

    def test_refresh_and_in_use_path_spelling(self):
        root = self.workspace.base / "in-use/cache"
        path = root / "MiXeD 数据.bin"
        write(path, 8193)
        held_skip_reason = None
        with ExitStack() as handles:
            stream = handles.enter_context(path.open("rb"))
            handles.enter_context(mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ))
            self.s.scan(root)
            refreshed, refresh_seconds = self.s.refresh_procs()
            self.assertIs(refreshed["procs"]["refreshing"], False)
            self.assertIsNone(refreshed["procs"]["error"])
            self.assertGreater(refreshed["procs"]["total"], 0)
            self.assertGreater(refreshed["procs"]["files"], 0)
            for language in LANGUAGES:
                info = self.s.get("/api/info", path=path, language=language)
                held_skip_reason = legacy_windows_proc_skip_reason(
                    impl=self.workspace.impl, windows=os.name == "nt", refresh_seconds=refresh_seconds,
                    in_use=info["cls"] == 1 or info["nprocs"] > 0 or bool(info["procs"]))
                if held_skip_reason:
                    break
                self.assertEqual((info["path"], info["size"], info["cls"], info["static_cls"]), (str(path), 8193, 1, 5))
                kind = (("已打开" if language == "zh" else "open") if sys.platform == "darwin"
                        else ("已加载到内存" if language == "zh" else "loaded into memory"))
                self.assertEqual(info["nprocs"], 1)
                self.assertEqual(info["procs"], [{"pid": os.getpid(), "comm": current_process_name(), "kind": kind}])
                summary = self.s.get("/api/agent/summary", language=language)
                self.assertEqual(summary["in_use"]["files"], 1)
                self.assertEqual(summary["in_use"]["bytes"], 8193)
                self.assertEqual(summary["in_use"]["size"], human(8193))
                expected_path = str(path).lower() if os.name == "nt" and self.workspace.impl == "legacy" else str(path)
                self.assertEqual(summary["in_use"]["largest"], [{"path": expected_path, "size": human(8193),
                                                                 "used_by": current_process_name()}])
                largest = self.s.get("/api/agent/largest", category="running", language=language)["files"]
                self.assertEqual(len(largest), 1)
                self.assertEqual((largest[0]["path"], largest[0]["bytes"], largest[0]["used_by"],
                                  largest[0]["category"], largest[0]["static_category"], largest[0]["safety"]),
                                 (str(path), 8193, current_process_name(), "running", "cache", "in_use"))
                self.assertEqual(summary["by_category"], [{"category": "running", "category_label": LABELS[language][1],
                                                           "bytes": 8193, "size": human(8193), "percent": 100.0}])
        # 暂缓 skipTest，先关闭句柄并完成第二次刷新，两个阶段使用相同的预算判定。
        refreshed, refresh_seconds = self.s.refresh_procs()
        self.assertIs(refreshed["procs"]["refreshing"], False)
        self.assertIsNone(refreshed["procs"]["error"])
        info = self.s.get("/api/info", path=path)
        closed_skip_reason = legacy_windows_proc_skip_reason(
            impl=self.workspace.impl, windows=os.name == "nt", refresh_seconds=refresh_seconds,
            in_use=info["cls"] == 1 or info["nprocs"] > 0 or bool(info["procs"]))
        if closed_skip_reason:
            self.skipTest(closed_skip_reason)
        self.assertEqual((info["path"], info["size"], info["cls"], info["static_cls"]), (str(path), 8193, 5, 5))
        self.assertEqual(info["nprocs"], 0)
        self.assertEqual(info["procs"], [])
        self.assertEqual(self.s.json("/api/agent/summary")["in_use"],
                         {"files": 0, "bytes": 0, "size": "0 B", "largest": []})
        if held_skip_reason:
            self.skipTest(held_skip_reason)

    @unittest.skipUnless(os.name == "nt", "Windows 路径别名；只在 Windows 运行 / Windows path aliases only")
    def test_windows_path_separators_and_case_aliases(self):
        for relative in ("proj", "docs/Report.pdf", "中文 空格+&.txt"):
            actual = self.f.path(relative)
            alias = str(actual).swapcase().replace("\\", "/")
            self.assert_info(self.s.get("/api/info", path=alias), self.f, relative)
            if relative in self.f.dirs:
                self.assertEqual(self.s.get("/api/tree", path=alias)["path"], str(actual))
                self.assert_list(self.s.get("/api/agent/list", path=alias), self.f, relative)
            else:
                self.assert_explain_file(self.s.get("/api/agent/explain", path=alias), self.f, relative)
            shown = self.s.json("/api/agent/show", {"path": alias})
            self.assertEqual(shown["path"], str(actual))
            self.assertEqual(self.s.json("/api/status")["command"]["path"], str(actual))

    def test_tree_depth_limit(self):
        root = self.workspace.base / "depth/cache"
        depth = 52
        leaf = root.joinpath(*(["d"] * depth))
        write(leaf / "payload.bin", 16384)
        _, status = self.s.scan(root)
        self.assertEqual((status["bytes"], status["files"], status["dirs"]), (16384, 1, depth))
        node = self.s.json("/api/tree")["tree"]
        for level in range(49):
            self.assertEqual((node["t"], node["s"], node["nf"], node["nd"]), ("d", 16384, 1, depth - level))
            if level == 48:
                self.assertEqual(node["m"], 1)
                self.assertNotIn("ch", node)
            else:
                self.assertEqual(len(node["ch"]), 1)
                node = node["ch"][0]

    def test_file_disappearing_after_scan(self):
        root = self.workspace.base / "gone/cache"
        path = root / "gone.bin"
        write(path, 8195)
        self.s.scan(root)
        path.unlink()  # 只删除本测试临时目录里自己生成的文件。
        info = self.s.get("/api/info", path=path)
        self.assertEqual((info["size"], info["type"], info["gone"]), (8195, "file", True))
        self.assertNotIn("mtime", info)
        for language in LANGUAGES:
            detail = self.s.get("/api/agent/explain", path=path, language=language)
            self.assertEqual(detail["bytes"], 8195)
            self.assertEqual(detail["note"], "This item was deleted or moved after the scan." if language == "en"
                             else "该项目在扫描后已被删除或移动。")

    @unittest.skipIf(os.name == "nt", "Unix 硬链接/符号链接；Windows 不要求创建链接的权限 / Unix links only")
    def test_unix_hardlinks_and_symlinks(self):
        root = self.workspace.base / "links/cache"
        write(root / "target.bin", 4097)
        os.link(root / "target.bin", root / "hard.bin")
        (root / "file-link").symlink_to("target.bin")
        outside = self.workspace.base / "outside"
        write(outside / "must-not-follow", 987654)
        (root / "dir-link").symlink_to(outside, target_is_directory=True)
        expected = 4097 + (root / "file-link").lstat().st_size + (root / "dir-link").lstat().st_size
        _, result = self.s.scan(root)
        self.assertEqual((result["files"], result["dirs"], result["bytes"]), (3, 0, expected))


class IdleAndLifecycleContract(ContractAssertions, unittest.TestCase):
    def test_idle_agent_heartbeat_instance_and_shutdown(self):
        workspace = Workspace()
        self.addCleanup(workspace.close)
        server = workspace.server()
        instance_path = Path(server.env["DISKMAP_CACHE_DIR"]) / "instance.json"
        instance = json.loads(instance_path.read_text(encoding="utf-8"))
        self.assertEqual(set(instance), {"port", "token", "pid", "version", "started"})
        self.assertEqual((instance["port"], instance["token"], instance["pid"]),
                         (server.port, TOKEN, server.owner.process.pid))
        self.assert_number(instance["started"])
        self.assertTrue(instance["version"])
        if os.name != "nt":
            self.assertEqual(stat.S_IMODE(instance_path.stat().st_mode), 0o600)
        for language in LANGUAGES:
            status = server.get("/api/agent/status", language=language)
            self.assert_agent_status(status)
            self.assertIs(status["window_connected"], False)
            for route in ("summary", "list", "explain", "cleanup", "largest"):
                self.assert_error(server.request("/api/agent/" + route, headers={"X-DiskMap-Lang": language}),
                                  400, NO_SCAN[language])
            self.assert_error(server.request("/api/agent/view", headers={"X-DiskMap-Lang": language}),
                              400, NO_VIEW[language])
        self.assertIs(server.json("/api/agent/status")["window_connected"], False)
        server.scan(workspace.fixture.root, agent=True)
        self.assertIs(server.json("/api/agent/status")["window_connected"], False)
        for route in ("/api/agent/summary", "/api/agent/list", "/api/agent/largest", "/api/agent/cleanup"):
            server.json(route)
        self.assertIs(server.json("/api/agent/status")["window_connected"], False)
        self.assert_status(server.json("/api/status"), workspace.fixture)
        self.assertIs(server.json("/api/agent/status")["window_connected"], True)
        if os.name != "nt":
            os.kill(server.owner.process.pid, signal.SIGINT)
            result = server.owner.collect(timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(instance_path.exists())

    @unittest.skipIf(os.name == "nt", "用 Unix SIGINT 检查进程正常退出 / Graceful Unix SIGINT")
    def test_old_server_does_not_remove_new_instance(self):
        workspace = Workspace()
        self.addCleanup(workspace.close)
        old = workspace.server()
        new = workspace.server()
        path = Path(workspace.env["DISKMAP_CACHE_DIR"]) / "instance.json"
        expected = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(expected["pid"], new.owner.process.pid)
        os.kill(old.owner.process.pid, signal.SIGINT)
        self.assertEqual(old.owner.collect(timeout=10).returncode, 0)
        self.assertEqual(json.loads(path.read_text(encoding="utf-8")), expected)
        self.assertEqual(new.json("/api/status")["state"], "idle")
        os.kill(new.owner.process.pid, signal.SIGINT)
        self.assertEqual(new.owner.collect(timeout=10).returncode, 0)
        self.assertFalse(path.exists())


if __name__ == "__main__":
    unittest.main()
