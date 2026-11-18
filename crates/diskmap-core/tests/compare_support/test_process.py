"""对比脚本子进程在构造异常、超时与中断时的回收。"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
import runpy
from unittest.mock import patch

from compare_support.process import ProcessOwner, run_process


class Children(unittest.TestCase):
    def test_service_constructor_failure_reaps(self):
        root = Path(__file__).resolve().parents[4]
        http = runpy.run_path(str(root / "crates/diskmap-core/tests/t05_compare.py"))
        cli = runpy.run_path(str(root / "crates/diskmap-cli/tests/cli_compare.py"))
        created = []
        real_popen = subprocess.Popen
        def record(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            created.append(process)
            return process
        command = [sys.executable, "-B", "-c", "print('invalid startup',flush=True)"]
        with tempfile.TemporaryDirectory(prefix="diskmap-t16-bad-start-") as directory:
            with patch("compare_support.process.subprocess.Popen", side_effect=record):
                with self.assertRaises(ChildProcessError):
                    http["Backend"]("invalid", command, Path(directory))
                with self.assertRaises(ChildProcessError):
                    cli["Window"](command, cli["environment"](Path(directory), "en"), new=True)
        self.assertEqual(len(created), 2)
        self.assertTrue(all(process.poll() is not None for process in created))
        self.assertTrue(all(stream.closed for process in created for stream in (process.stdin, process.stdout, process.stderr)))

    def test_exception_and_keyboard_interrupt_reap(self):
        for error in (RuntimeError("injected"), KeyboardInterrupt()):
            with self.assertRaises(type(error)):
                with ProcessOwner([sys.executable, "-B", "-c", "import time; print('ready',flush=True); time.sleep(60)"]) as owner:
                    self.assertEqual(owner.readline(5), "ready\n")
                    process = owner.process
                    raise error
            self.assertIsNotNone(process.poll())
            self.assertTrue(all(stream.closed for stream in (process.stdin, process.stdout, process.stderr)))

    def test_constructor_reader_start_failure_reaps(self):
        created = []
        real_popen = subprocess.Popen
        def record(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            created.append(process)
            return process
        with patch("compare_support.process.subprocess.Popen", side_effect=record):
            with patch("compare_support.process.threading.Thread.start", side_effect=RuntimeError("injected start failure")):
                with self.assertRaisesRegex(RuntimeError, "injected start failure"):
                    ProcessOwner([sys.executable, "-B", "-c", "import time; time.sleep(60)"])
        self.assertTrue(created)
        self.assertTrue(all(process.poll() is not None for process in created))
        self.assertTrue(all(stream.closed for process in created for stream in (process.stdin, process.stdout, process.stderr)))

    def test_timeout_when_child_never_reads_input(self):
        created = []
        real_popen = subprocess.Popen
        def record(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            created.append(process)
            return process
        started = time.monotonic()
        with patch("compare_support.process.subprocess.Popen", side_effect=record):
            with self.assertRaises(subprocess.TimeoutExpired):
                run_process([sys.executable, "-B", "-c", "import time; time.sleep(60)"],
                            input="x" * 2_000_000, timeout=0.2)
        self.assertLess(time.monotonic() - started, 10)
        self.assertTrue(all(process.poll() is not None for process in created))

    def test_parent_exits_with_descendant_holding_pipes(self):
        code = "import subprocess,sys; p=subprocess.Popen([sys.executable,'-B','-c','import time; time.sleep(60)']); print(p.pid,flush=True)"
        started = time.monotonic()
        descendant = None
        previous = None
        if sys.platform.startswith("linux"):
            # 仅此回归临时收养自己造的孙进程；容器 PID 1 不一定及时 wait 僵尸。
            import ctypes
            libc = ctypes.CDLL(None, use_errno=True)
            libc.prctl.argtypes = [ctypes.c_int] + [ctypes.c_ulong] * 4
            libc.prctl.restype = ctypes.c_int
            previous = ctypes.c_int()
            self.assertEqual(libc.prctl(37, ctypes.addressof(previous), 0, 0, 0), 0)
            self.assertEqual(libc.prctl(36, 1, 0, 0, 0), 0)
        try:
            with ProcessOwner([sys.executable, "-B", "-c", code]) as owner:
                descendant = int(owner.readline(5))
                owner.process.wait(timeout=5)
                with self.assertRaises(TimeoutError):
                    owner.collect(timeout=0.1)
            self.assertLess(time.monotonic() - started, 5)
            self.assertTrue(all(not thread.is_alive() for thread in owner.readers))
        finally:
            if previous is not None:
                try:
                    if descendant is not None:
                        deadline = time.monotonic() + 3
                        while time.monotonic() < deadline:
                            pid, _ = os.waitpid(descendant, os.WNOHANG)
                            if pid == descendant:
                                break
                            time.sleep(0.01)
                        else:
                            self.fail("自己创建的孙进程未及时终止并回收")
                        self.assertFalse(Path(f"/proc/{descendant}").exists())
                finally:
                    self.assertEqual(libc.prctl(36, previous.value, 0, 0, 0), 0)

    def test_utf8_collects_stdout_and_stderr(self):
        result = run_process([sys.executable, "-B", "-c", "import sys; print('中文🦀'); print('诊断',file=sys.stderr)"],
                             env=dict(os.environ, PYTHONUTF8="1", PYTHONIOENCODING="utf-8"), timeout=5)
        self.assertEqual(result.stdout, "中文🦀\n")
        self.assertEqual(result.stderr, "诊断\n")


if __name__ == "__main__":
    unittest.main()
