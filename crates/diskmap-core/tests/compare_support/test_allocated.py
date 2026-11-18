"""Windows allocated 差异的离线约束测试；不在 Linux 上伪造或调用 Windows 系统 API。"""

import contextlib
import copy
import io
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

import t05_compare
from compare_support.allocated import (
    AGENT_ALLOCATED_LABELS, adapt_agent_allocated_bytes, allocation_evidence, legacy_human,
)


class AllocatedComparisonTests(unittest.TestCase):
    def setUp(self):
        self.evidence = allocation_evidence("fixture", 4096, [
            {"path": "small", "size": 17, "allocated": 24},
            {"path": "large", "size": 4097, "allocated": 8192},
            {"path": "zero", "size": 0, "allocated": 0},
        ])
        self.old = self.response(self.evidence["legacy_cluster_bytes"])
        self.new = self.response(self.evidence["apparent"] + 100)
        self.label = "agent switch allocated done"

    @staticmethod
    def response(size):
        return {"status": 200, "headers": {}, "body": {
            "version": "test", "scan": {
                "root": "fixture", "files": 3, "state": "done", "size_mode": "allocated",
                "bytes": size, "size": legacy_human(size), "elapsed_seconds": 0.0,
                "finished_at": None, "disk_used_bytes": None,
            }, "other": "strict"}}

    def adapt(self, label=None, *, windows=True):
        return adapt_agent_allocated_bytes(label or self.label, self.old, self.new,
                                           windows=windows, evidence=self.evidence)

    def test_actual_fixture_totals_and_cluster_are_independently_computed(self):
        with tempfile.TemporaryDirectory(prefix="diskmap-t16-allocation-") as directory:
            root = t05_compare.agent_fixture(Path(directory) / "fixture")
            rows = [{"path": str(path.relative_to(root)), "size": path.stat().st_size,
                     "allocated": 0} for path in root.rglob("*") if path.is_file()]
            evidence = allocation_evidence(root, 4096, rows)
            self.assertEqual((evidence["files"], evidence["apparent"], evidence["legacy_cluster_bytes"]),
                             (171, 251241, 704512))
            # 换簇大小会重新逐文件计算，不能把 Windows 一次观测的旧值写死。
            different = allocation_evidence(root, 8192, rows)
            self.assertEqual(different["legacy_cluster_bytes"], 170 * 8192)
            # 已观测的 Windows 数值：字节及文本一起通过，但仍只是一组口径差异。
            old, new = self.response(704512), self.response(575920)
            for response in (old, new):
                response["body"]["scan"].update(root=str(root), files=evidence["files"])
            self.assertEqual(old["body"]["scan"]["size"], "688 KB")
            self.assertEqual(new["body"]["scan"]["size"], "562 KB")
            self.assertEqual(adapt_agent_allocated_bytes(self.label, old, new,
                                                         windows=True, evidence=evidence), 1)
            self.assertEqual(old, new)

    def test_legacy_human_format_boundaries(self):
        cases = (
            (None, None), (0, "0 B"), (1023, "1023 B"), (1024, "1.00 KB"),
            (10 * 1024 - 1, "10.00 KB"), (10 * 1024, "10.0 KB"),
            (100 * 1024 - 1, "100.0 KB"), (100 * 1024, "100 KB"),
            (128 * 1024 + 512, "128 KB"), (129 * 1024 + 512, "130 KB"),
            (1024 ** 2 - 1, "1024 KB"), (1024 ** 2, "1.00 MB"),
            (1024 ** 3, "1.00 GB"), (1024 ** 4, "1.00 TB"),
            (1024 ** 5, "1.00 PB"), (1024 ** 6, "1024 PB"),
        )
        for size, expected in cases:
            with self.subTest(size=size):
                self.assertEqual(legacy_human(size), expected)

    def test_only_two_labels_and_only_windows_can_adapt(self):
        before = copy.deepcopy(self.old)
        for label in ("done allocated", "agent restore apparent done", self.label + " extra"):
            self.assertEqual(self.adapt(label), 0)
            self.assertEqual(self.old, before)
        self.assertEqual(self.adapt(windows=False), 0)
        self.assertEqual(self.old, before)
        for label in AGENT_ALLOCATED_LABELS:
            self.old = copy.deepcopy(before)
            self.assertEqual(self.adapt(label), 1)
            expected = copy.deepcopy(before)
            expected["body"]["scan"]["bytes"] = self.new["body"]["scan"]["bytes"]
            expected["body"]["scan"]["size"] = self.new["body"]["scan"]["size"]
            self.assertEqual(self.old, expected)

    def test_named_cases_require_allocated_and_finished_scans(self):
        for side in ("old", "new"):
            for field, invalid in (("size_mode", "apparent"), ("state", "scanning")):
                with self.subTest(side=side, field=field):
                    self.setUp()
                    getattr(self, side)["body"]["scan"][field] = invalid
                    before = copy.deepcopy((self.old, self.new))
                    with self.assertRaises(AssertionError):
                        self.adapt()
                    self.assertEqual((self.old, self.new), before)

    def test_either_side_with_wrong_size_is_rejected_before_adapting(self):
        for side in ("old", "new"):
            for invalid in ("0 B", "12 KB", "12.0 KiB", "12.0 KB ", None, 12, False):
                with self.subTest(side=side, size=invalid):
                    self.setUp()
                    getattr(self, side)["body"]["scan"]["size"] = invalid
                    before = copy.deepcopy((self.old, self.new))
                    with self.assertRaisesRegex(AssertionError, "scan.size"):
                        self.adapt()
                    self.assertEqual((self.old, self.new), before)

    def test_equal_bytes_with_wrong_size_still_fail(self):
        for side in ("old", "new", "both"):
            with self.subTest(side=side):
                self.setUp()
                self.new = copy.deepcopy(self.old)
                for name in (("old", "new") if side == "both" else (side,)):
                    getattr(self, name)["body"]["scan"]["size"] = "0 B"
                with self.assertRaisesRegex(AssertionError, "scan.size"):
                    self.adapt()

    def test_wrong_legacy_total_and_out_of_range_new_bytes_fail(self):
        self.old["body"]["scan"]["bytes"] -= 1
        self.old["body"]["scan"]["size"] = legacy_human(self.old["body"]["scan"]["bytes"])
        with self.assertRaises(AssertionError):
            self.adapt()
        self.old["body"]["scan"]["bytes"] += 1
        self.old["body"]["scan"]["size"] = legacy_human(self.old["body"]["scan"]["bytes"])
        for size in (self.evidence["apparent"] - 1, self.evidence["legacy_cluster_bytes"] + 1, True):
            self.new["body"]["scan"]["bytes"] = size
            self.new["body"]["scan"]["size"] = legacy_human(size)
            with self.assertRaises(AssertionError):
                self.adapt()

    def test_equal_bytes_are_still_verified_and_not_counted_as_a_difference(self):
        self.new = copy.deepcopy(self.old)
        self.assertEqual(self.adapt(), 0)
        self.old["body"]["scan"]["bytes"] -= 1
        self.new["body"]["scan"]["bytes"] -= 1
        with self.assertRaises(AssertionError):
            self.adapt()

    def test_api_totals_and_alignment_are_diagnostic_not_expected_scan_totals(self):
        self.assertEqual(self.evidence["compressed_api_bytes"], 8216)
        self.assertEqual(self.evidence["small_alignment"]["8"],
                         {"rounded_size_sum": 24, "api_matches": 1})
        self.assertNotEqual(self.new["body"]["scan"]["bytes"], self.evidence["compressed_api_bytes"])
        self.assertEqual(self.adapt(), 1)

    def test_comparison_counts_known_cases_but_rejects_other_field_differences(self):
        with (patch.object(t05_compare, "os", SimpleNamespace(name="nt")),
              patch.object(t05_compare, "fixture_spellings", return_value={}),
              patch.object(t05_compare, "windows_allocation_evidence", return_value=self.evidence),
              contextlib.redirect_stdout(io.StringIO())):
            check = t05_compare.Comparison(None, None, root="fixture")
            check.check(self.label, [self.old, self.new], "agent_status", 200)
            self.assertEqual((check.count, check.allocated_differences, check.field_difference_cases),
                             (1, 1, 1))
            self.assertEqual(check.failures, [])
            self.new["body"]["other"] = "must fail"
            check.check(self.label, [self.old, self.new], "agent_status", 200)
            self.assertEqual(len(check.failures), 1)
            self.assertIn("$.body.other", check.failures[0])
            self.assertEqual(check.allocated_differences, 1)
            self.assertEqual(self.old["body"]["scan"]["bytes"], self.evidence["legacy_cluster_bytes"])

    def test_service_cleanup_continues_after_one_stop_fails(self):
        class Log(io.StringIO):
            def reconfigure(self, **_kwargs):
                pass

        for failing in ("rust", "rust-agent"):
            with self.subTest(failing=failing):
                stopped = []

                class Backend:
                    def __init__(self, name, _command, directory):
                        self.name = name
                        self.cache = directory / name / "cache"

                    def stop(self):
                        stopped.append(self.name)
                        if self.name == failing:
                            raise RuntimeError("injected stop failure")

                with (patch.object(t05_compare, "Backend", Backend),
                      patch.object(t05_compare, "fixture", side_effect=lambda root: root),
                      patch.object(t05_compare, "agent_fixture", side_effect=lambda root: root),
                      patch.object(t05_compare, "instance_check"),
                      patch.object(t05_compare, "run_suite"),
                      patch.object(t05_compare, "run_agent_suite"),
                      patch.object(t05_compare.sys, "argv", ["t05_compare", "--binary", __file__]),
                      patch.object(t05_compare.sys, "stdout", Log()),
                      patch.object(t05_compare.sys, "stderr", Log())):
                    with self.assertRaisesRegex(RuntimeError, "injected stop failure"):
                        t05_compare.main()
                expected = ["rust", "legacy"]
                if failing == "rust-agent":
                    expected.extend(["rust-agent", "legacy-agent"])
                self.assertEqual(stopped, expected)


if __name__ == "__main__":
    unittest.main()
