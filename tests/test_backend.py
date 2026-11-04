"""后端测试：python -m unittest discover -s tests

在 Windows / macOS / Linux 上都能运行（CI 会在三个系统上各跑一遍）。
各系统的分类规则用 ntpath / posixpath 在任何系统上都能测试。
"""
import http.client
import importlib.util
import json
import os
import sys
import tempfile
import time
import unittest
import urllib.parse

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from backend.classify import CACHE, CONFIG, CORE, OTHER, RUNNING, SYSTEM, USER, Classifier  # noqa: E402
from backend.mounts import disk_mounts, read_mounts  # noqa: E402
from backend.osinfo import IS_WIN  # noqa: E402
from backend.procinfo import ProcInfo, _collect_psutil  # noqa: E402
from backend.scanner import KEEP_FILES, Scan  # noqa: E402
from backend.server import serve  # noqa: E402

HAVE_PSUTIL = importlib.util.find_spec("psutil") is not None


class FakePkg:
    def __init__(self, files=None, backup=(), app_dirs=None):
        self.files = files or {}
        self.backup = set(backup)
        self.app_dirs = app_dirs or {}


class FakeProc(ProcInfo):
    def __init__(self, paths=()):
        super().__init__()
        self.install({p: {123: ("demo", "exe")} for p in paths})


def write(path, size):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(b"x" * size)


class LinuxRulesTest(unittest.TestCase):
    def setUp(self):
        self.c = Classifier(FakePkg({"/usr/bin/ls": "coreutils", "/etc/pacman.conf": "pacman",
                                     "/usr/lib/modules/x/a.ko": "linux"},
                                    backup={"/etc/pacman.conf"}),
                            flavor="linux", env={}, homes=["/home/alice"])

    def test_dirs(self):
        e = self.c.explain
        self.assertEqual(e("/boot/grub", True)[0], CORE)
        self.assertEqual(e("/etc/X11", True)[0], CONFIG)
        self.assertEqual(e("/usr/share/icons/cache", True)[0], SYSTEM)  # 系统区域不按名字判断缓存
        self.assertEqual(e("/var/cache/pacman", True)[0], CACHE)
        self.assertEqual(e("/var/lib/docker", True)[0], OTHER)
        self.assertEqual(e("/home/alice/Documents", True)[0], USER)
        self.assertEqual(e("/home/alice/.cache/x", True)[0], CACHE)
        self.assertEqual(e("/home/alice/.rustup", True)[0], OTHER)
        self.assertEqual(e("/home/alice/.config/Code/Cache", True)[0], CACHE)
        self.assertEqual(e("/home/alice/proj/__pycache__", True)[0], CACHE)
        self.assertEqual(e("/home/alice/.local/state/x", True)[0], OTHER)

    def test_files(self):
        e = self.c.explain
        self.assertEqual(e("/usr/bin/ls", False), (SYSTEM, "由软件包 coreutils 安装"))
        self.assertEqual(e("/etc/pacman.conf", False)[0], CONFIG)
        self.assertEqual(e("/usr/lib/modules/x/a.ko", False)[0], CORE)
        self.assertEqual(e("/home/alice/.bashrc", False)[0], CONFIG)
        self.assertEqual(e("/home/alice/notes/run.log", False)[0], CACHE)
        self.assertEqual(e("/home/alice/notes/a.txt", False)[0], USER)
        self.assertEqual(e("/swapfile", False)[0], CORE)


class WindowsRulesTest(unittest.TestCase):
    def setUp(self):
        env = {"SystemDrive": "C:", "SystemRoot": "C:\\Windows",
               "ProgramFiles": "C:\\Program Files", "ProgramFiles(x86)": "C:\\Program Files (x86)",
               "ProgramData": "C:\\ProgramData"}
        self.c = Classifier(FakePkg(app_dirs={"C:\\Program Files\\Foo": "Foo 编辑器"}),
                            flavor="windows", env=env, homes=["C:\\Users\\Bob"])

    def test_dirs(self):
        e = self.c.explain
        self.assertEqual(e("C:\\Windows\\System32\\drivers\\etc", True)[0], CORE)
        self.assertEqual(e("c:\\windows\\system32", True)[0], SYSTEM)       # 不区分大小写
        self.assertEqual(e("C:\\Windows\\Temp\\x", True)[0], CACHE)
        self.assertEqual(e("C:\\Windows\\SoftwareDistribution\\Download", True)[0], CACHE)
        cls, why = e("C:\\Program Files\\Foo\\bin", True)
        self.assertEqual(cls, SYSTEM)
        self.assertIn("Foo 编辑器", why)
        self.assertEqual(e("C:\\Program Files (x86)\\Bar", True)[0], SYSTEM)
        self.assertEqual(e("C:\\Users\\Bob\\AppData\\Local\\Temp", True)[0], CACHE)
        self.assertEqual(e("C:\\Users\\Bob\\AppData\\Local\\Programs", True)[0], OTHER)
        self.assertEqual(e("C:\\Users\\Bob\\AppData\\Roaming\\Code\\Cache", True)[0], CACHE)
        self.assertEqual(e("C:\\Users\\Bob\\Documents", True)[0], USER)
        self.assertEqual(e("C:\\Users\\bob\\.ssh", True)[0], CONFIG)
        self.assertEqual(e("C:\\$Recycle.Bin\\S-1-5-21", True)[0], CACHE)
        self.assertEqual(e("C:\\System Volume Information", True)[0], CORE)
        self.assertEqual(e("D:\\", True)[0], OTHER)
        self.assertEqual(e("D:\\Games\\x", True)[0], OTHER)

    def test_files(self):
        e = self.c.explain
        self.assertEqual(e("C:\\pagefile.sys", False)[0], CORE)
        self.assertEqual(e("C:\\hiberfil.sys", False)[0], CORE)
        self.assertEqual(e("C:\\Users\\Bob\\NTUSER.DAT", False)[0], CONFIG)
        self.assertEqual(e("C:\\Users\\Bob\\Downloads\\setup.LOG", False)[0], CACHE)
        self.assertEqual(e("C:/Users/Bob/Desktop/a.txt", False)[0], USER)   # 正斜杠也能识别


class MacRulesTest(unittest.TestCase):
    def setUp(self):
        self.c = Classifier(FakePkg(app_dirs={"/opt/homebrew/Cellar/git": "git（Homebrew）"}),
                            flavor="macos", env={}, homes=["/Users/carol"])

    def test_dirs(self):
        e = self.c.explain
        self.assertEqual(e("/System/Library/Frameworks", True)[0], CORE)
        self.assertEqual(e("/Applications/Safari.app", True)[0], SYSTEM)
        self.assertEqual(e("/Library/Caches/x", True)[0], CACHE)
        self.assertEqual(e("/Users/carol/Library/Caches/com.example", True)[0], CACHE)
        self.assertEqual(e("/Users/carol/Library/Preferences", True)[0], CONFIG)
        self.assertEqual(e("/Users/carol/Library/Application Support/Code/Cache", True)[0], CACHE)
        self.assertEqual(e("/Users/carol/Library/Application Support/Code/User", True)[0], OTHER)
        self.assertEqual(e("/Users/carol/Library/Developer/Xcode/DerivedData", True)[0], CACHE)
        self.assertEqual(e("/Users/carol/.Trash", True)[0], CACHE)
        self.assertEqual(e("/Users/carol/Movies", True)[0], USER)
        self.assertEqual(e("/private/var/vm", True)[0], CORE)
        self.assertEqual(e("/private/var/folders/ab/cd", True)[0], CACHE)
        cls, why = e("/opt/homebrew/Cellar/git/2.50/bin", True)
        self.assertEqual(cls, SYSTEM)
        self.assertIn("git", why)


class ScanTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        r = self.root = self.tmp.name
        j = os.path.join
        write(j(r, "big", "a.bin"), 300_000)
        write(j(r, "big", "b.bin"), 100_000)
        write(j(r, "many", "keep.dat"), 50_000)
        for i in range(KEEP_FILES + 40):
            write(j(r, "many", f"f{i:03d}.txt"), 10)
        write(j(r, "deep", "x", "y", "z", "leaf.bin"), 20_000)
        self.hardlink = os.name == "posix"   # Windows 的目录枚举不提供硬链接计数
        if self.hardlink:
            os.link(j(r, "big", "a.bin"), j(r, "hardlink.bin"))
        os.makedirs(j(r, "empty"))
        self.running = j(r, "many", "f160.txt")   # 会被合并的小文件，但正在使用 → 应保留
        self.proc = FakeProc([self.running, j(r, "big", "b.bin")])
        self.scan = Scan(r, Classifier(FakePkg()), self.proc, apparent=True)
        self.scan._run()
        self.scan.state = "done"

    def tearDown(self):
        self.tmp.cleanup()

    def test_sizes_and_counts(self):
        s = self.scan
        expected = 300_000 + 100_000 + 50_000 + (KEEP_FILES + 40) * 10 + 20_000
        self.assertEqual(s.root.size, expected)   # 硬链接只计一次
        self.assertEqual(s.root.size, sum(s.root.csz))
        self.assertEqual(s.find(os.path.join(self.root, "deep"))[0].size, 20_000)
        self.assertEqual(s.find(os.path.join(self.root, "deep", "x", "y"))[0].ndirs, 1)

    def test_aggregation_keeps_running_files(self):
        many, _ = self.scan.find(os.path.join(self.root, "many"))
        names = [f[0] for f in many.files if len(f) == 3]
        agg = [f for f in many.files if len(f) == 4]
        self.assertIn("keep.dat", names)
        self.assertIn("f160.txt", names)
        self.assertEqual(len(agg), 1)
        self.assertEqual(sum(f[1] for f in many.files), many.size)
        self.assertEqual(many.nfiles, KEEP_FILES + 41)

    def test_view_consistency(self):
        node, out = self.scan.view(self.root, frac=0.01)

        def check(o):
            if "ch" in o:
                self.assertEqual(sum(c["s"] for c in o["ch"]), o["s"], o["n"])
                for c in o["ch"]:
                    check(c)
        check(out)
        big = next(c for c in out["ch"] if c["n"] == "big")
        b = next(c for c in big["ch"] if c["n"] == "b.bin")
        self.assertEqual(b["c"], RUNNING)
        self.assertEqual(b["p"], "demo")
        self.assertEqual(big["r"], 1)

    def test_totals_overlay(self):
        t = self.scan.totals(self.scan.root)
        self.assertEqual(sum(t), self.scan.root.size)
        self.assertEqual(t[RUNNING], 100_000 + 10)

    def test_find(self):
        j = os.path.join
        node, f = self.scan.find(j(self.root, "big", "b.bin"))
        self.assertEqual(f[:2], ("b.bin", 100_000))
        if self.hardlink:
            # 硬链接：先遇到的路径（根目录下的 hardlink.bin）被计入，a.bin 不重复计算
            self.assertIsNotNone(self.scan.find(j(self.root, "hardlink.bin"))[1])
            self.assertEqual(self.scan.find(j(self.root, "big", "a.bin")), (None, None))
        self.assertEqual(self.scan.find(j(self.root, "no", "such", "file")), (None, None))
        if IS_WIN:
            self.assertIsNotNone(self.scan.find(j(self.root, "BIG", "B.BIN"))[1])


class PlatformTest(unittest.TestCase):
    def test_mounts(self):
        self.assertTrue(read_mounts())
        disks = disk_mounts()
        self.assertTrue(disks, "至少应发现一个磁盘")
        self.assertTrue(any(d["system"] for d in disks))

    def test_procinfo(self):
        p = ProcInfo().refresh(budget=5)
        self.assertIsNone(p.error)
        self.assertGreater(p.nprocs, 0)
        self.assertTrue(p.files)
        # 自己这个进程的可执行文件一定能读到（macOS 上 sys.executable 可能只是个跳板，所以按 PID 找）
        mine = [k for k, slot in p.files.items() if os.getpid() in slot]
        self.assertTrue(mine, "当前进程使用的文件应被识别为正在使用")
        self.assertGreater(p.dir_count(os.path.dirname(mine[0])), 0)

    @unittest.skipUnless(HAVE_PSUTIL, "未安装 psutil")
    def test_psutil_collector(self):
        raw, n, _ = _collect_psutil(5)
        self.assertGreater(n, 0)
        self.assertTrue(raw)


class ServerTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        write(os.path.join(cls.tmp.name, "sub", "file.bin"), 4096)
        cls.httpd, cls.url, cls.app = serve(0, "tok123")
        cls.port = cls.httpd.server_address[1]

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.tmp.cleanup()

    def req(self, method, path, body=None, token="tok123", host=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        headers = {"Host": host or f"127.0.0.1:{self.port}"}
        if token:
            headers["X-DiskMap-Token"] = token
        data = None
        if body is not None:
            data = json.dumps(body)
            headers["Content-Type"] = "application/json"
        c.request(method, path, data, headers)
        r = c.getresponse()
        raw = r.read()
        c.close()
        return r.status, raw

    def test_security(self):
        self.assertEqual(self.req("GET", "/api/status", token=None)[0], 403)
        self.assertEqual(self.req("GET", "/api/status", token="wrong")[0], 403)
        self.assertEqual(self.req("GET", "/api/status", host="evil.example:80")[0], 403)
        self.assertEqual(self.req("GET", "/", token=None)[0], 403)
        st, html = self.req("GET", "/?t=tok123", token=None)
        self.assertEqual(st, 200)
        self.assertIn(b'"tok123"', html)
        self.assertEqual(self.req("GET", "/static/../backend/server.py", token=None)[0], 404)
        self.assertEqual(self.req("POST", "/api/bye?t=wrong", token=None)[0], 403)

    def test_heartbeat(self):
        before = time.time()
        self.req("GET", "/api/status")
        self.assertGreaterEqual(self.app.last_seen, before)
        self.assertEqual(self.req("POST", "/api/bye?t=tok123", token=None)[0], 204)
        self.assertGreaterEqual(self.app.bye_at, before)

    def test_scan_flow(self):
        st, raw = self.req("POST", "/api/scan", {"path": self.tmp.name, "apparent": True})
        self.assertEqual(st, 200, raw)
        for _ in range(100):
            status = json.loads(self.req("GET", "/api/status")[1])
            if status["state"] != "scanning":
                break
            time.sleep(0.05)
        self.assertEqual(status["state"], "done")
        q = urllib.parse.quote(self.tmp.name)
        tree = json.loads(self.req("GET", f"/api/tree?path={q}")[1])
        self.assertEqual(tree["tree"]["s"], 4096)
        fq = urllib.parse.quote(os.path.join(self.tmp.name, "sub", "file.bin"))
        info = json.loads(self.req("GET", f"/api/info?path={fq}")[1])
        self.assertEqual(info["type"], "file")
        self.assertEqual(info["size"], 4096)
        meta = json.loads(self.req("GET", "/api/meta")[1])
        self.assertEqual(meta["sep"], os.sep)
        self.assertIn(meta["os"], ("linux", "macos", "windows"))
        self.assertEqual(self.req("POST", "/api/reveal", {"path": "/etc/passwd"})[0], 400)


if __name__ == "__main__":
    unittest.main()
