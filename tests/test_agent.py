"""AI 接口测试：分析函数、MCP 协议、连接用户窗口、命令行报告。"""
import io
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from backend import analysis, instance  # noqa: E402
from backend.classify import USER  # noqa: E402
from backend.mcp_server import TOOLS, Backend, Server  # noqa: E402
from backend.server import serve  # noqa: E402


def write(path, size):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(b"x" * size)


def treat_as_user_data(app, root):
    """临时目录本身会被归为"缓存/临时"，测试里把它当作普通用户数据。"""
    app.ready.wait()
    app.classifier.dir_rules[app.classifier._k(root)] = (USER, "测试数据")


def make_tree(root):
    j = os.path.join
    write(j(root, "docs", "report.pdf"), 400_000)
    write(j(root, "proj", "cache", "blob.bin"), 2_500_000)      # 名为 cache 的文件夹 → 缓存
    write(j(root, "proj", "__pycache__", "m.pyc"), 1_200_000)
    write(j(root, "proj", "build.log"), 1_100_000)              # .log → 缓存
    write(j(root, "proj", "main.py"), 3_000)
    return 400_000 + 2_500_000 + 1_200_000 + 1_100_000 + 3_000


class AgentBase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.cache = tempfile.TemporaryDirectory()
        os.environ["DISKMAP_CACHE_DIR"] = cls.cache.name
        cls.root = os.path.join(cls.tmp.name, "data")
        cls.total = make_tree(cls.root)
        cls.httpd, cls.url, cls.app = serve(0, "agent-token")
        treat_as_user_data(cls.app, cls.root)
        cls.app.start_scan(cls.root, apparent=True)
        while cls.app.scan.state == "scanning":
            time.sleep(0.05)

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        os.environ.pop("DISKMAP_CACHE_DIR", None)
        cls.tmp.cleanup()
        cls.cache.cleanup()


class AnalysisTest(AgentBase):
    def test_summary(self):
        s = analysis.summary(self.app, None, 5)
        self.assertEqual(s["bytes"], self.total)
        self.assertEqual(sum(c["bytes"] for c in s["by_category"]), self.total)
        self.assertEqual(s["largest_files"][0]["path"], os.path.join(self.root, "proj", "cache", "blob.bin"))
        self.assertEqual(s["cleanup"]["reclaimable_bytes"], 2_500_000 + 1_200_000 + 1_100_000)
        for e in s["largest_children"]:
            self.assertIn(e["safety"], ("danger", "in_use", "uninstall_only", "caution", "ask_user", "safe_to_clean"))

    def test_cleanup_does_not_double_count(self):
        c = analysis.cleanup(self.app, os.path.join(self.root, "proj"), min_bytes=0)
        paths = {e["path"] for e in c["candidates"]}
        j = os.path.join
        self.assertEqual(paths, {j(self.root, "proj", "cache"), j(self.root, "proj", "__pycache__"),
                                 j(self.root, "proj", "build.log")})
        self.assertTrue(all(e["safety"] == "safe_to_clean" for e in c["candidates"]))

    def test_list_explain_largest(self):
        lst = analysis.list_dir(self.app, self.root)
        self.assertEqual([e["path"] for e in lst["items"]][0], os.path.join(self.root, "proj"))
        ex = analysis.explain(self.app, os.path.join(self.root, "docs", "report.pdf"))
        self.assertEqual((ex["type"], ex["bytes"]), ("file", 400_000))
        self.assertIn("advice", ex)
        big = analysis.largest_files(self.app, None, 2, category="cache")
        self.assertEqual(len(big["files"]), 2)
        self.assertTrue(all(f["category"] == "cache" for f in big["files"]))
        with self.assertRaises(analysis.AnalysisError):
            analysis.largest_files(self.app, None, 2, category="nonsense")
        with self.assertRaises(analysis.AnalysisError):
            analysis.explain(self.app, os.path.join(self.root, "missing"))

    def test_markdown_report(self):
        md = analysis.report_markdown(self.app, None, 5)
        self.assertIn("# DiskMap 磁盘分析", md)
        self.assertIn("可清理候选", md)
        self.assertIn("blob.bin", md)


class McpProtocolTest(AgentBase):
    def rpc(self, server, mid, method, params=None):
        server.out = io.BytesIO()
        msg = {"jsonrpc": "2.0", "id": mid, "method": method}
        if params is not None:
            msg["params"] = params
        server.handle(msg)
        return json.loads(server.out.getvalue().decode("utf-8"))

    def call(self, server, name, **args):
        r = self.rpc(server, 9, "tools/call", {"name": name, "arguments": args})["result"]
        return r, r.get("structuredContent")

    def test_protocol(self):
        srv = Server(Backend(attach=False), out=io.BytesIO())
        init = self.rpc(srv, 1, "initialize", {"protocolVersion": "2025-03-26"})["result"]
        self.assertEqual(init["protocolVersion"], "2025-03-26")
        self.assertIn("tools", init["capabilities"])
        init = self.rpc(srv, 1, "initialize", {"protocolVersion": "1999-01-01"})["result"]
        self.assertEqual(init["protocolVersion"], "2025-11-25")    # 不认识的版本：给出最新支持的
        tools = self.rpc(srv, 2, "tools/list")["result"]["tools"]
        self.assertEqual(len(tools), len(TOOLS))
        for t in tools:
            self.assertEqual(t["inputSchema"]["type"], "object")
        self.assertEqual(self.rpc(srv, 3, "ping")["result"], {})
        self.assertEqual(self.rpc(srv, 4, "no/such")["error"]["code"], -32601)
        self.assertEqual(self.rpc(srv, 5, "tools/call", {"name": "nope"})["error"]["code"], -32602)
        srv.out = io.BytesIO()
        srv.handle({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.assertEqual(srv.out.getvalue(), b"")                 # 通知不回复

    def test_tools_background(self):
        be = Backend(attach=False)
        be._local()
        treat_as_user_data(be.local[1], self.root)
        srv = Server(be, out=io.BytesIO())
        r, data = self.call(srv, "diskmap_scan", path=self.root, wait_seconds=30, apparent=True)
        self.assertFalse(r["isError"])
        self.assertEqual(data["connected_to"], "background")
        self.assertEqual(data["summary"]["bytes"], self.total)
        r, data = self.call(srv, "diskmap_cleanup", min_mb=0)
        self.assertEqual(data["reclaimable_bytes"], 2_500_000 + 1_200_000 + 1_100_000)
        r, data = self.call(srv, "diskmap_list", path=self.root, limit=1)
        self.assertEqual(data["shown"], 1)
        r, data = self.call(srv, "diskmap_largest_files", limit=1, category="user")
        self.assertTrue(data["files"][0]["path"].endswith("report.pdf"))
        r, _ = self.call(srv, "diskmap_explain", path="/definitely/not/scanned")
        self.assertTrue(r["isError"])
        r, _ = self.call(srv, "diskmap_user_view")
        self.assertTrue(r["isError"])                             # 没有打开窗口

    def test_attach_to_window(self):
        port = self.httpd.server_address[1]
        instance.write(port, "agent-token", "test")
        try:
            srv = Server(Backend(attach=True), out=io.BytesIO())
            r, st = self.call(srv, "diskmap_status")
            self.assertEqual(st["connected_to"], "window")
            self.assertEqual(st["scan"]["root"], self.root)
            # 模拟界面上报：用户正在看 proj，选中了 build.log
            self.app.last_seen = time.time()
            self.app.ui_state = {"view": os.path.join(self.root, "proj"),
                                 "selected": os.path.join(self.root, "proj", "build.log"),
                                 "pinned": None, "time": time.time()}
            r, view = self.call(srv, "diskmap_user_view")
            self.assertFalse(r["isError"], r)
            self.assertEqual(view["selected_detail"]["bytes"], 1_100_000)
            self.assertEqual(view["view_summary"]["path"], os.path.join(self.root, "proj"))
            r, shown = self.call(srv, "diskmap_show", path=os.path.join(self.root, "docs"), note="这里是文档")
            self.assertTrue(shown["ok"])
            self.assertEqual(self.app.command["path"], os.path.join(self.root, "docs"))
            self.assertEqual(self.app.command["note"], "这里是文档")
            self.assertIsNotNone(self.app.status()["agent_seen_ago"])
            # 已有扫描覆盖该路径：复用，不打断用户
            r, data = self.call(srv, "diskmap_scan", path=os.path.join(self.root, "proj"), apparent=True)
            self.assertTrue(data["reused_existing_scan"])
        finally:
            instance.remove()
            os.unlink(instance.instance_path()) if os.path.exists(instance.instance_path()) else None


class CliTest(unittest.TestCase):
    def test_analyze_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            total = make_tree(tmp)
            out = subprocess.run([sys.executable, os.path.join(ROOT, "diskmap.py"), "analyze", tmp,
                                  "--json", "--apparent", "-q", "--top", "3"],
                                 capture_output=True, timeout=120)
            self.assertEqual(out.returncode, 0, out.stderr.decode(errors="replace"))
            data = json.loads(out.stdout.decode("utf-8"))
            self.assertEqual(data["summary"]["bytes"], total)
            self.assertEqual(len(data["summary"]["largest_files"]), 3)

    def test_analyze_markdown_to_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            make_tree(os.path.join(tmp, "d"))
            report = os.path.join(tmp, "r.md")
            out = subprocess.run([sys.executable, os.path.join(ROOT, "diskmap.py"), "analyze",
                                  os.path.join(tmp, "d"), "-o", report, "-q"], capture_output=True, timeout=120)
            self.assertEqual(out.returncode, 0, out.stderr.decode(errors="replace"))
            with open(report, encoding="utf-8") as f:
                self.assertIn("可清理候选", f.read())


if __name__ == "__main__":
    unittest.main()
