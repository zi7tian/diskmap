"""与实现无关的期望值计算，包含 Linux 上可验证的 Windows 记录解析。"""

import itertools
import unittest

from support import DIRECTORY_RECORD, directory_records, human, legacy_windows_proc_skip_reason


class OracleContract(unittest.TestCase):
    def test_legacy_windows_proc_budget_skip_conditions(self):
        expected_skips = {("legacy", True, 11.9, False), ("legacy", True, 12.0, False),
                          ("legacy", True, 30.0, False)}
        cases = itertools.product(("rust", "legacy"), (False, True), (0.0, 11.899, 11.9, 12.0, 30.0), (False, True))
        for impl, windows, seconds, in_use in cases:
            with self.subTest(impl=impl, windows=windows, seconds=seconds, in_use=in_use):
                reason = legacy_windows_proc_skip_reason(
                    impl=impl, windows=windows, refresh_seconds=seconds, in_use=in_use)
                if (impl, windows, seconds, in_use) in expected_skips:
                    self.assertIn("12 秒预算（容差 0.1 秒）", reason)
                    self.assertIn("12 s budget", reason)
                    self.assertIn("legacy/backend/procinfo.py", reason)
                    self.assertIn("ProcInfo.refresh(budget=12.0)", reason)
                    self.assertIn("_collect_psutil", reason)
                    self.assertIn("只收可执行文件", reason)
                    self.assertIn("only executables", reason)
                    self.assertIn("skips mapped/open files", reason)
                else:
                    self.assertIsNone(reason)

    def test_human_units_and_precision(self):
        for size, expected in ((0, "0 B"), (1023, "1023 B"), (1024, "1.00 KB"), (10240, "10.0 KB"),
                               (102400, "100 KB"), (1048576, "1.00 MB"), (1 << 50, "1.00 PB")):
            self.assertEqual(human(size), expected)
        self.assertIsNone(human(None))

    def test_windows_directory_record_offsets_unicode_and_multiple_entries(self):
        self.assertEqual(DIRECTORY_RECORD.size, 68)
        name = "MiXeD 数据.bin".encode("utf-16-le")
        length = (68 + len(name) + 7) // 8 * 8
        first = DIRECTORY_RECORD.pack(length, 7, 1, 2, 3, 4, 777, 1024, 0x20, len(name), 0) + name
        second_name = "zero".encode("utf-16-le")
        second = DIRECTORY_RECORD.pack(0, 8, 1, 2, 3, 4, 0, 0, 0x20, len(second_name), 0) + second_name
        raw = first.ljust(length, b"\0") + second
        self.assertEqual(directory_records(raw), [("MiXeD 数据.bin", 777, 1024, 0x20), ("zero", 0, 0, 0x20)])

    def test_windows_directory_record_rejects_truncation_and_invalid_offsets(self):
        cases = (b"\0" * 67, DIRECTORY_RECORD.pack(0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0),
                 DIRECTORY_RECORD.pack(72, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0) + b"\0" * 16,
                 DIRECTORY_RECORD.pack(71, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0) + b"\0" * 16)
        for raw in cases:
            with self.assertRaises(ValueError):
                directory_records(raw)


if __name__ == "__main__":
    unittest.main()
