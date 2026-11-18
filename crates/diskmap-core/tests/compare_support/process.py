"""对比脚本拥有的子进程：严格 UTF-8、限时读取、失败及中断时回收整个进程组。"""

import os
import queue
import signal
import subprocess
import threading
import time


def stop_process(process, grace=0, force_group=False):
    """只回收调用者创建的 Popen；管道断开也不能跳过 terminate/kill/wait。"""
    try:
        # 不在这里 close(stdin)：另一线程可能阻塞写管道并持有锁，必须先结束进程。
        if process.poll() is None and grace:
            try:
                process.wait(timeout=grace)
            except subprocess.TimeoutExpired:
                pass
    finally:
        if process.poll() is None:
            if os.name == "nt":
                # 后代由 ProcessOwner 持有的 Job 回收，无需按 PID 再启动 taskkill。
                process.terminate()
            else:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                if os.name != "nt":
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                process.kill()
                process.wait(timeout=5)
        else:
            process.wait()
        if os.name != "nt" and force_group:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


class _WindowsJob:
    """关闭本脚本的 Job 即回收子孙；父进程先退出也不失去后代的所有权。"""
    def __init__(self):
        import ctypes
        from ctypes import wintypes

        class Basic(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_int64), ("job_time", ctypes.c_int64),
                        ("flags", wintypes.DWORD), ("minimum", ctypes.c_size_t),
                        ("maximum", ctypes.c_size_t), ("active", wintypes.DWORD),
                        ("affinity", ctypes.c_size_t), ("priority", wintypes.DWORD),
                        ("scheduling", wintypes.DWORD)]

        class Extended(ctypes.Structure):
            _fields_ = [("basic", Basic), ("io", ctypes.c_uint64 * 6),
                        ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                        ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]

        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        self.api.CreateJobObjectW.restype = wintypes.HANDLE
        self.api.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
        self.api.SetInformationJobObject.restype = wintypes.BOOL
        self.api.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        self.api.AssignProcessToJobObject.restype = wintypes.BOOL
        self.api.CloseHandle.argtypes = [wintypes.HANDLE]
        self.api.CloseHandle.restype = wintypes.BOOL
        self.handle = self.api.CreateJobObjectW(None, None)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not self.api.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            error = ctypes.WinError(ctypes.get_last_error())
            self.close()
            raise error

    def assign(self, process):
        import ctypes
        # Popen 持有已创建进程的真实句柄；不重新按可能复用的 PID 寻找外部进程。
        if not self.api.AssignProcessToJobObject(self.handle, int(process._handle)):
            raise ctypes.WinError(ctypes.get_last_error())

    def close(self):
        if self.handle:
            self.api.CloseHandle(self.handle)
            self.handle = None


class ProcessOwner:
    """构造失败也立即回收；stdout 行读取、最终收集均有明确超时。"""
    def __init__(self, command, *, env=None, cwd=None, **kwargs):
        self.process = None
        self.job = None
        self.output = queue.Queue()
        self.errors = []
        self.stdout = []
        self.reader_errors = []
        self.readers = []
        options = dict(stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                       text=True, encoding="utf-8", errors="strict", bufsize=1)
        options.update(kwargs)
        env = dict(os.environ if env is None else env, DISKMAP_TEST_MANAGED_SESSION="1")
        # 独立组让脚本的 KeyboardInterrupt 不抢先杀掉孩子，finally 可完整 wait 回收。
        options["start_new_session"] = os.name != "nt"
        if os.name == "nt":
            options["creationflags"] = options.get("creationflags", 0) | subprocess.CREATE_NEW_PROCESS_GROUP
        try:
            if os.name == "nt":
                self.job = _WindowsJob()
            self.process = subprocess.Popen(command, cwd=cwd, env=env, **options)
            if self.job is not None:
                self.job.assign(self.process)
            for stream, output in ((self.process.stdout, self.stdout), (self.process.stderr, self.errors)):
                thread = threading.Thread(target=self._read, args=(stream, output), daemon=True)
                self.readers.append(thread)
                thread.start()
        except BaseException:
            self.close()
            raise

    def _read(self, stream, output):
        try:
            for line in stream:
                output.append(line)
                if output is self.stdout:
                    self.output.put(line)
        except BaseException as error:
            self.reader_errors.append(error)
        finally:
            if output is self.stdout:
                self.output.put(None)

    def readline(self, timeout=60):
        if self.reader_errors:
            raise self.reader_errors[0]
        try:
            line = self.output.get(timeout=timeout)
        except queue.Empty:
            raise TimeoutError(f"子进程输出超时：{self.process.args}\n{''.join(self.errors)}") from None
        if line is None:
            if self.reader_errors:
                raise self.reader_errors[0]
            raise ChildProcessError(f"子进程已关闭输出：{self.process.args}\n{''.join(self.errors)}")
        return line

    def collect(self, timeout=120):
        deadline = time.monotonic() + timeout
        self.process.wait(timeout=timeout)
        for reader in self.readers:
            reader.join(timeout=max(0, deadline - time.monotonic()))
            if reader.is_alive():
                raise TimeoutError("子进程结束后管道仍未关闭")
        if self.reader_errors:
            raise self.reader_errors[0]
        return subprocess.CompletedProcess(self.process.args, self.process.returncode,
                                           ''.join(self.stdout), ''.join(self.errors))

    def close(self, grace=0):
        if self.process is None:
            if self.job is not None:
                self.job.close()
            return
        try:
            stop_process(self.process, grace, force_group=any(reader.is_alive() for reader in self.readers))
        finally:
            if self.job is not None:
                self.job.close()
            if self.process.poll() is None:
                self.process.kill()
            self.process.wait(timeout=5)
            for reader in self.readers:
                if reader.ident is not None:
                    reader.join(timeout=1)
            if any(reader.is_alive() for reader in self.readers):
                raise TimeoutError("回收进程后管道读取线程仍未退出，避免阻塞关闭流")
            for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
                if stream is not None and not stream.closed:
                    try:
                        stream.close()
                    except (OSError, BrokenPipeError):
                        pass

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def run_process(command, *, timeout=120, input=None, **kwargs):
    with ProcessOwner(command, **kwargs) as owner:
        errors = []
        def write_input():
            try:
                if input is not None:
                    owner.process.stdin.write(input)
                owner.process.stdin.close()
            except BaseException as error:
                errors.append(error)
        writer = threading.Thread(target=write_input, daemon=True)
        writer.start()
        deadline = time.monotonic() + timeout
        result = owner.collect(timeout)
        writer.join(timeout=max(0, deadline - time.monotonic()))
        if writer.is_alive():
            raise TimeoutError("子进程输入超时")
        if errors:
            raise errors[0]
        return result
