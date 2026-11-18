"""已知路径差异的字段边界与真实拼写核验。"""
import copy
import unittest

from compare_support.fields import adapt_proc_paths
from compare_support.windows_paths import windows_path_key


class Fields(unittest.TestCase):
    def test_only_proc_field_with_true_spelling(self):
        actual = {"path": r"C:\Data", "files": 1, "dirs": 0, "by_category": [],
                  "largest_children": [], "cleanup": {},
                  "in_use": {"largest": [{"path": r"C:\Data\Report.pdf", "used_by": "reader"}]}}
        expected = copy.deepcopy(actual)
        expected["in_use"]["largest"][0]["path"] = "c:/data/report.pdf"
        spelling = {windows_path_key(r"C:\Data\Report.pdf"): r"C:\Data\Report.pdf"}
        self.assertEqual(adapt_proc_paths(expected, actual, windows=False, spellings=spelling), 0)
        self.assertNotEqual(expected, actual)
        self.assertEqual(adapt_proc_paths(expected, actual, windows=True, spellings=spelling), 1)
        self.assertEqual(expected, actual)
        expected["path"] = "c:/data"
        self.assertEqual(adapt_proc_paths(expected, actual, windows=True, spellings=spelling), 0)
        self.assertNotEqual(expected, actual)
        actual["in_use"]["largest"][0]["path"] = "c:/data/report.pdf"
        with self.assertRaises(AssertionError):
            adapt_proc_paths(expected, actual, windows=True, spellings=spelling)


if __name__ == "__main__":
    unittest.main()
