"""按行收发 JSON-RPC 的 MCP stdio 契约，分别检查窗口连接和独立扫描。"""

from pathlib import Path
import time
import unittest

from assertions import NO_VIEW, ContractAssertions
from support import CATEGORIES, LANGUAGES, PROTOCOLS, Workspace


TOOL_SPECS = {
    "diskmap_status": ((), (), {}),
    "diskmap_scan": (("path", "rescan", "wait_seconds", "apparent", "same_fs"), (),
                     {"rescan": False, "wait_seconds": 120, "apparent": False, "same_fs": False}),
    "diskmap_summary": (("path", "top"), (), {"top": 10}),
    "diskmap_list": (("path", "limit"), ("path",), {"limit": 40}),
    "diskmap_explain": (("path",), ("path",), {}),
    "diskmap_cleanup": (("path", "limit", "min_mb"), (), {"limit": 30, "min_mb": 1}),
    "diskmap_largest_files": (("path", "limit", "category"), (), {"limit": 20}),
    "diskmap_user_view": ((), (), {}),
    "diskmap_show": (("path", "note"), ("path",), {}),
}
TITLES = {
    "en": ("DiskMap status", "Scan disk", "Usage overview", "List folder", "Explain item", "Reclaimable space",
           "Largest files", "What the user is viewing", "Show in window"),
    "zh": ("DiskMap 状态", "扫描磁盘", "占用概览", "列出子项", "解释项目", "可清理空间",
           "最大的文件", "用户正在看什么", "在窗口中指给用户看"),
}


class McpContract(ContractAssertions, unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = Workspace()
        cls.addClassCleanup(cls.workspace.close)
        cls.fixture = cls.workspace.fixture

    def test_protocol_and_tool_schemas_both_languages(self):
        for language in LANGUAGES:
            with self.subTest(language=language):
                client = self.workspace.mcp(attach=False, language=language,
                                            env=self.workspace.environment("protocol-" + language, language))
                try:
                    for version in PROTOCOLS + ("1999-01-01",):
                        data = client.rpc("initialize", {"protocolVersion": version, "capabilities": {},
                                                          "clientInfo": {"name": "contract", "version": "1"}})["result"]
                        self.assertEqual(set(data), {"protocolVersion", "capabilities", "serverInfo", "instructions"})
                        self.assertEqual(data["protocolVersion"], version if version in PROTOCOLS else PROTOCOLS[0])
                        self.assertEqual(data["capabilities"], {"tools": {"listChanged": False}})
                        self.assertEqual(set(data["serverInfo"]), {"name", "title", "version"})
                        self.assertEqual(data["serverInfo"]["name"], "diskmap")
                        self.assertEqual(data["serverInfo"]["title"], "DiskMap" if language == "en" else "DiskMap 磁盘地图")
                        self.assertTrue(data["serverInfo"]["version"])
                        self.assertIn("diskmap_status", data["instructions"])
                        self.assertIn("Suggested flow" if language == "en" else "建议流程", data["instructions"])
                    tools = client.rpc("tools/list")["result"]["tools"]
                    self.assertEqual([tool["name"] for tool in tools], list(TOOL_SPECS))
                    for tool, title in zip(tools, TITLES[language]):
                        self.check_schema(tool, title)
                    client.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                    client.send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 999}})
                    client.send({"jsonrpc": "2.0", "id": "client-response", "result": {}})
                    client.send("")
                    # 以上通知/客户端响应不能污染下一条按 id 匹配的回复。
                    self.assertEqual(client.rpc("ping")["result"], {})
                    for method, key in (("resources/list", "resources"), ("prompts/list", "prompts"),
                                        ("resources/templates/list", "resourceTemplates")):
                        self.assertEqual(client.rpc(method)["result"], {key: []})
                    self.assertEqual(client.rpc("unknown/method")["error"],
                                     {"code": -32601, "message": "Method not found: unknown/method"})
                    self.assertEqual(client.rpc("tools/call", {"name": "unknown"})["error"],
                                     {"code": -32602, "message": "Unknown tool: unknown" if language == "en" else "未知工具：unknown"})
                    client.send("{invalid")
                    self.assertEqual(client.receive(), {"jsonrpc": "2.0", "id": None,
                                                         "error": {"code": -32700, "message": "Parse error"}})
                    client.send({"jsonrpc": "1.0", "id": "bad", "method": "ping"})
                    self.assertEqual(client.receive(), {"jsonrpc": "2.0", "id": "bad",
                                                         "error": {"code": -32600, "message": "Invalid Request"}})
                    client.send([{"jsonrpc": "2.0", "id": "batch-a", "method": "ping"},
                                 {"jsonrpc": "2.0", "method": "notifications/initialized"},
                                 {"jsonrpc": "2.0", "id": "batch-b", "method": "ping"}])
                    self.assertEqual(sorted([client.receive(), client.receive()], key=lambda row: row["id"]),
                                     [{"jsonrpc": "2.0", "id": name, "result": {}} for name in ("batch-a", "batch-b")])
                finally:
                    result = client.finish()
                self.assertEqual(result.returncode, 0, result.stderr)

    def check_schema(self, tool, title):
        self.assertEqual(set(tool), {"name", "title", "description", "inputSchema", "annotations"})
        self.assertEqual(tool["title"], title)
        self.assertIsInstance(tool["description"], str)
        self.assertTrue(tool["description"])
        props, required, defaults = TOOL_SPECS[tool["name"]]
        schema = tool["inputSchema"]
        self.assertEqual(set(schema), {"type", "properties", "required", "additionalProperties"})
        self.assertEqual(schema["type"], "object")
        self.assertEqual(set(schema["properties"]), set(props))
        self.assertEqual(schema["required"], list(required))
        self.assertIs(schema["additionalProperties"], False)
        types = {"rescan": "boolean", "apparent": "boolean", "same_fs": "boolean", "wait_seconds": "integer",
                 "top": "integer", "limit": "integer", "min_mb": "number"}
        for name, entry in schema["properties"].items():
            self.assertEqual(entry["type"], types.get(name, "string"))
            self.assertTrue(entry["description"])
            if name in defaults:
                self.assertEqual(entry["default"], defaults[name])
                self.assertIs(type(entry["default"]), type(defaults[name]))
            else:
                self.assertNotIn("default", entry)
            self.assertEqual(set(entry), {"type", "description"} | ({"default"} if name in defaults else set())
                             | ({"enum"} if name == "category" else set()))
            if name == "category":
                self.assertEqual(entry["enum"], list(CATEGORIES))
        self.assertEqual(tool["annotations"], {"title": title,
                                               "readOnlyHint": tool["name"] not in ("diskmap_scan", "diskmap_show"),
                                               "destructiveHint": False, "idempotentHint": True, "openWorldHint": False})

    def test_every_tool_window_and_background_both_languages(self):
        for language in LANGUAGES:
            for attach in (False, True):
                with self.subTest(language=language, attach=attach):
                    label = f"tools-{language}-{'window' if attach else 'background'}"
                    env = self.workspace.environment(label, language)
                    server = self.workspace.server(env) if attach else None
                    client = self.workspace.mcp(attach=attach, language=language, env=env)
                    try:
                        self.exercise_tools(client, server, language)
                    finally:
                        result = client.finish()
                        if server:
                            server.owner.close()
                    self.assertEqual(result.returncode, 0, result.stderr)
                    if not attach:
                        self.assertFalse((Path(env["DISKMAP_CACHE_DIR"]) / "instance.json").exists())

    def exercise_tools(self, client, server, language):
        f = self.fixture
        source = "window" if server else "background"

        def call(name, args=None):
            return self.assert_mcp_result(client.call(name, args or {}))

        client.rpc("initialize", {"protocolVersion": PROTOCOLS[0]})
        deadline = time.monotonic() + 30
        while True:
            status = call("diskmap_status")
            if status["ready"]:
                break
            if time.monotonic() > deadline:
                self.fail("MCP 初始化超时 / MCP initialization timed out")
            time.sleep(0.02)
        self.assert_agent_status(status, source=source)
        scanned = call("diskmap_scan", {"path": str(f.root), "apparent": True, "same_fs": True,
                                         "rescan": True, "wait_seconds": 30})
        self.assertEqual(set(scanned), {"connected_to", "reused_existing_scan", "scan", "summary"})
        self.assertEqual(scanned["connected_to"], source)
        self.assertIs(scanned["reused_existing_scan"], False)
        self.assert_summary(scanned["summary"], f, language=language, top=8)
        status = call("diskmap_status")
        self.assert_agent_status(status, f, source=source)
        self.assertEqual(scanned["scan"], status["scan"])
        self.assert_summary(call("diskmap_summary", {"top": 3}), f, language=language, top=3)
        self.assert_list(call("diskmap_list", {"path": str(f.root), "limit": 3}), f, language=language, limit=3)
        self.assert_explain_file(call("diskmap_explain", {"path": str(f.path("docs/Report.pdf"))}),
                                 f, "docs/Report.pdf", language=language)
        self.assert_cleanup(call("diskmap_cleanup", {"min_mb": 0}), f, language=language, min_bytes=0)
        self.assert_largest(call("diskmap_largest_files", {"limit": 3, "category": "cache"}),
                            f, language=language, limit=3, category="cache")
        reused = call("diskmap_scan", {"path": str(f.path("proj")), "apparent": True, "same_fs": True,
                                        "wait_seconds": 5})
        self.assertIs(reused["reused_existing_scan"], True)
        self.assertEqual(reused["scan"]["root"], str(f.root))
        self.assert_summary(reused["summary"], f, "proj", language=language, top=8)

        if server:
            self.assert_mcp_result(client.call("diskmap_user_view"), error=NO_VIEW[language])
            server.json("/api/ui-state", {"view": str(f.path("proj")), "selected": str(f.path("proj/Build.log")),
                                           "pinned": "cache"})
            view = call("diskmap_user_view")
            self.assertEqual(set(view), {"view", "selected", "highlighted_category", "updated_seconds_ago",
                                         "view_summary", "selected_detail"})
            self.assertEqual((view["view"], view["selected"], view["highlighted_category"]),
                             (str(f.path("proj")), str(f.path("proj/Build.log")), "cache"))
            self.assert_summary(view["view_summary"], f, "proj", language=language, top=5)
            self.assert_explain_file(view["selected_detail"], f, "proj/Build.log", language=language)
            shown = call("diskmap_show", {"path": str(f.path("docs")), "note": "这里是文档 / Documents"})
            self.assertEqual(shown, {"ok": True, "path": str(f.path("docs")), "window_connected": True,
                                     "message": "The DiskMap window now shows and highlights this item." if language == "en"
                                     else "已让 DiskMap 窗口跳转并高亮该项目。"})
            command = server.json("/api/status")["command"]
            self.assertEqual((command["id"], command["action"], command["path"], command["note"]),
                             (1, "show", str(f.path("docs")), "这里是文档 / Documents"))
        else:
            self.assert_mcp_result(client.call("diskmap_user_view"), error={
                "en": "The DiskMap window is not open, so what the user is looking at is unknown. "
                      "Use diskmap_summary and the other tools to analyze on your own.",
                "zh": "用户没有打开 DiskMap 窗口，无法知道用户正在看什么。可以用 diskmap_summary 等工具自行分析。",
            }[language])
            self.assert_mcp_result(client.call("diskmap_show", {"path": str(f.root)}), error={
                "en": "The DiskMap window is not open, so nothing can be shown there. Tell the user the path instead.",
                "zh": "用户没有打开 DiskMap 窗口，无法在窗口中显示。请直接把路径告诉用户。",
            }[language])
        missing = f.path("missing")
        for tool in ("diskmap_scan", "diskmap_summary", "diskmap_list", "diskmap_explain", "diskmap_cleanup",
                     "diskmap_largest_files"):
            message = ((f"Folder does not exist: {missing}" if language == "en" else f"文件夹不存在：{missing}")
                       if tool == "diskmap_scan" else
                       (f"This path is not in the scan results: {missing} (the scan covers {f.root})" if language == "en"
                        else f"扫描结果中没有这个路径：{missing}（当前扫描范围是 {f.root}）"))
            self.assert_mcp_result(client.call(tool, {"path": str(missing)}), error=message)
        if server:
            self.assert_mcp_result(client.call("diskmap_show", {"path": str(missing)}), error=(
                f"This path is not in the scan results: {missing}" if language == "en" else f"扫描结果中没有这个路径：{missing}"))
        for tool, args, message in (
                ("diskmap_summary", {"top": "oops"}, "invalid literal for int() with base 10: 'oops'"),
                ("diskmap_list", {"path": str(f.root), "limit": "oops"}, "invalid literal for int() with base 10: 'oops'"),
                ("diskmap_cleanup", {"min_mb": "oops"}, "could not convert string to float: 'oops'"),
                ("diskmap_largest_files", {"category": "unknown"},
                 ("Unknown category: unknown. Choose one of " if language == "en" else "未知类别：unknown，可选 ")
                 + ", ".join(CATEGORIES))):
            self.assert_mcp_result(client.call(tool, args), error=message)
        # 旧版对 status 的多余参数不按 schema 拒绝：这是无参数工具的既有行为。
        self.assert_agent_status(self.assert_mcp_result(client.call("diskmap_status", ["ignored"])), f, source=source)

    def test_no_attach_environment_overrides_registered_window(self):
        env = self.workspace.environment("no-attach-env")
        server = self.workspace.server(env)
        server.scan(self.fixture.root)
        env["DISKMAP_NO_ATTACH"] = "1"
        client = self.workspace.mcp(attach=True, env=env)
        try:
            data = self.assert_mcp_result(client.call("diskmap_status"))
            self.assertEqual(data["connected_to"], "background")
            self.assertIsNone(data["scan"])
        finally:
            result = client.finish()
            server.owner.close()
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
