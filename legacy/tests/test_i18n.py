"""语言测试：中英双语文本、按请求指定语言、界面保存设置、MCP 与命令行报告的英文输出。"""
import http.client
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
os.environ["DISKMAP_LANG"] = "zh"
os.environ.setdefault("DISKMAP_CONFIG_DIR", tempfile.mkdtemp(prefix="diskmap-test-config-"))

from backend import classify, i18n  # noqa: E402
from backend.classify import CLASSES, CORE, Classifier  # noqa: E402
from backend.i18n import L, T  # noqa: E402
from backend.mcp_server import INSTRUCTIONS, TOOLS, Backend, Server  # noqa: E402
from backend.server import serve  # noqa: E402

CJK = re.compile(r"[一-鿿]")


def write(path, size):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(b"x" * size)


def make_tree(root):
    write(os.path.join(root, "docs", "a.pdf"), 300_000)
    write(os.path.join(root, "cache", "blob.bin"), 200_000)


class Restore(unittest.TestCase):
    def tearDown(self):
        i18n.set_default("zh")


class TextTest(Restore):
    def test_bilingual_text(self):
        t = T("缓存", "Cache")
        self.assertEqual(t, "缓存")                      # 比较、哈希仍按中文
        i18n.set_default("en")
        self.assertEqual(str(t), "Cache")
        self.assertEqual(f"[{t}]", "[Cache]")
        self.assertEqual(i18n.loc({"a": [t, 1, None]}), {"a": ["Cache", 1, None]})
        self.assertEqual(L("中", "en"), "en")
        with i18n.use("zh"):
            self.assertEqual(str(t), "缓存")
            seen = []
            th = threading.Thread(target=lambda: seen.append(str(t)))   # 其他线程不受影响
            th.start()
            th.join()
            self.assertEqual(seen, ["Cache"])
        self.assertEqual(str(t), "Cache")

    def test_resolve(self):
        self.assertEqual(i18n.norm("zh_CN.UTF-8"), "zh")
        self.assertEqual(i18n.norm("en-US"), "en")
        self.assertIsNone(i18n.norm("fr_FR"))
        self.assertEqual(i18n.resolve("en"), "en")             # 命令行优先
        self.assertEqual(i18n.resolve(), "zh")                 # 测试里环境变量设为 zh
        self.assertEqual(i18n.pre_scan_argv(["/", "--lang", "en"]), "en")
        self.assertEqual(i18n.pre_scan_argv(["--lang=zh"]), "zh")
        self.assertIn(i18n.system_lang(), i18n.LANGS)

    def test_everything_has_english(self):
        tables = [classify._LINUX_DIRS, classify._LINUX_FILES, classify._LINUX_HOME, classify._MAC_DIRS,
                  classify._MAC_HOME, classify._WIN_DIRS, classify._WIN_FILES, classify._WIN_HOME,
                  classify._COMMON_HOME]
        for table in tables:
            for path, (_cls, why) in table.items():
                self.assertIsInstance(why, T, path)
                self.assertFalse(CJK.search(why.en), path)
        for c in CLASSES:
            self.assertFalse(CJK.search(c["label"].en + c["desc"].en))
        en = json.dumps(i18n.loc([TOOLS, INSTRUCTIONS], "en"), ensure_ascii=False)
        self.assertFalse(CJK.search(en), CJK.search(en) and en[CJK.search(en).start() - 60:][:120])

    def test_classifier(self):
        class Pkg:
            files = {"/boot/vmlinuz-linux": "linux"}
            backup = set()
            app_dirs = {}
        c = Classifier(Pkg(), flavor="linux", env={}, homes=["/home/u"])
        _cls, why = c.explain("/home/u/.cache/x", True)
        cls, kernel = c.explain("/boot/vmlinuz-linux", False)
        self.assertEqual(cls, CORE)
        self.assertEqual(why, "用户缓存 ~/.cache")
        self.assertIn("由软件包 linux 安装 · 引导分区", kernel)
        i18n.set_default("en")
        self.assertEqual(str(why), "User cache ~/.cache")
        self.assertEqual(str(kernel), "Installed by package linux · Boot partition: kernel, initramfs and boot loader")


class ServerLangTest(Restore):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        make_tree(cls.tmp.name)
        cls.httpd, cls.url, cls.app = serve(0, "lang-token")
        cls.port = cls.httpd.server_address[1]
        cls.app.start_scan(cls.tmp.name, apparent=True)
        while cls.app.scan.state == "scanning":
            time.sleep(0.05)

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.tmp.cleanup()

    def req(self, method, path, body=None, lang=None, token="lang-token"):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        headers = {"Host": f"127.0.0.1:{self.port}", "X-DiskMap-Token": token}
        if lang:
            headers["X-DiskMap-Lang"] = lang
        data = None
        if body is not None:
            data = json.dumps(body)
            headers["Content-Type"] = "application/json"
        c.request(method, path, data, headers)
        r = c.getresponse()
        raw = r.read()
        c.close()
        return r.status, raw

    def get(self, path, **kw):
        st, raw = self.req("GET", path, **kw)
        self.assertEqual(st, 200, raw)
        return json.loads(raw)

    def test_switch_and_persist(self):
        self.assertEqual(self.get("/api/meta")["lang"], "zh")
        st, raw = self.req("POST", "/api/settings", {"lang": "en"})
        self.assertEqual((st, json.loads(raw)), (200, {"lang": "en"}))
        meta = self.get("/api/meta")
        self.assertEqual(meta["lang"], "en")
        self.assertEqual(meta["classes"][0]["label"], "System core")
        with open(i18n.settings_path(), encoding="utf-8") as f:
            self.assertEqual(json.load(f)["lang"], "en")
        st, html = self.req("GET", "/?t=lang-token", token="")
        self.assertIn(b'data-lang="en"', html)
        self.assertIn(b'window.DISKMAP_LANG = "en"', html)
        self.assertEqual(self.req("POST", "/api/settings", {"lang": "fr"})[0], 400)
        self.assertEqual(self.req("POST", "/api/settings", {"lang": "zh"})[0], 200)
        self.assertEqual(self.get("/api/meta")["classes"][0]["label"], "系统核心")

    def test_header_override(self):
        en = self.get("/api/agent/summary", lang="en")
        zh = self.get("/api/agent/summary")
        labels = {c["category"]: c["category_label"] for c in en["by_category"]}
        self.assertEqual(labels.get("cache"), "Cache / temp")
        self.assertFalse(CJK.search(json.dumps(en["cleanup"], ensure_ascii=False)))
        self.assertIn("缓存", json.dumps(zh, ensure_ascii=False))
        st, raw = self.req("GET", "/api/info?path=/no/such/path", lang="en")
        self.assertEqual(st, 404)
        self.assertEqual(json.loads(raw)["error"], "No scan results for this path")


class McpAndCliTest(Restore):
    def test_mcp_english(self):
        i18n.set_default("en")
        with tempfile.TemporaryDirectory() as tmp:
            make_tree(tmp)
            srv = Server(Backend(attach=False), out=io.BytesIO())

            def rpc(mid, method, params=None):
                srv.out = io.BytesIO()
                srv.handle({"jsonrpc": "2.0", "id": mid, "method": method, "params": params or {}})
                return json.loads(srv.out.getvalue().decode("utf-8"))["result"]
            init = rpc(1, "initialize", {"protocolVersion": "2025-06-18"})
            self.assertTrue(init["instructions"].startswith("DiskMap measures"))
            tools = rpc(2, "tools/list")["tools"]
            self.assertFalse(CJK.search(json.dumps(tools, ensure_ascii=False)))
            res = rpc(3, "tools/call", {"name": "diskmap_scan",
                                        "arguments": {"path": tmp, "apparent": True, "wait_seconds": 30}})
            self.assertFalse(res["isError"], res)
            data = res["structuredContent"]
            self.assertEqual(data["summary"]["bytes"], 500_000)
            self.assertNotIn("connected_to_label", data)
            advice = [e["advice"] for e in data["summary"]["largest_children"]]
            self.assertTrue(advice and not any(CJK.search(a) for a in advice), advice)

    def test_cli_english(self):
        with tempfile.TemporaryDirectory() as tmp:
            make_tree(tmp)
            out = subprocess.run([sys.executable, os.path.join(ROOT, "diskmap.py"), "analyze", tmp,
                                  "--lang", "en", "-q"], capture_output=True, timeout=120)
            self.assertEqual(out.returncode, 0, out.stderr.decode(errors="replace"))
            md = out.stdout.decode("utf-8")
            self.assertIn("# DiskMap disk analysis", md)
            self.assertIn("## Cleanup candidates", md)
            self.assertNotIn("磁盘分析", md)
            out = subprocess.run([sys.executable, os.path.join(ROOT, "diskmap.py"), "--lang", "en", "--help"],
                                 capture_output=True, timeout=60)
            self.assertIn("--lang", out.stdout.decode("utf-8", errors="replace"))
            self.assertIn("see what uses your disk space", out.stdout.decode("utf-8", errors="replace"))


if __name__ == "__main__":
    unittest.main()
