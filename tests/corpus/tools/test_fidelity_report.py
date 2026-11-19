"""fidelity_report.py 的单元测试。

运行：python -X dev -m unittest discover -s tests/corpus/tools -p "test_*.py"
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import fidelity_report as fr  # noqa: E402

SHARED = "这一页的内容流与别处共用"
PICTURE = "重新生成内容流后页面的画面变了"


def result(*files: dict[str, Any], version: str = "159.0.8138.0") -> dict[str, Any]:
    """命令行工具 `fidelity --json` 的写法（页序号从 0 开始）。"""
    return {"kind": "magpdf-fidelity", "format": 1, "pdfium_version": version, "files": list(files)}


def file(path: str, *reasons: str | None, status: str = "ok", error: str | None = None) -> dict[str, Any]:
    entry: dict[str, Any] = {
        "path": path,
        "status": status,
        "pages": [
            {"index": i, "editable": r is None, **({"reason": r} if r else {})} for i, r in enumerate(reasons)
        ],
    }
    if error:
        entry["error"] = error
    return entry


def baseline(*files: dict[str, Any], meta: dict[str, Any] | None = None) -> dict[str, Any]:
    return fr.to_baseline(result(*files), "synthetic", meta or {"machine": "M"})


class CompareTest(unittest.TestCase):
    def test_same_result_has_no_changes(self) -> None:
        old = baseline(file("a.pdf", None, SHARED))
        cmp = fr.compare(old, baseline(file("a.pdf", None, SHARED)))
        self.assertFalse(cmp.worse)
        self.assertEqual((cmp.newly_overlay, cmp.newly_editable, cmp.reason_changed), ([], [], []))

    def test_editable_page_becoming_overlay_only_is_worse(self) -> None:
        cmp = fr.compare(baseline(file("a.pdf", None, None)), baseline(file("a.pdf", None, PICTURE)))
        self.assertTrue(cmp.worse)
        self.assertEqual(cmp.newly_overlay, [("a.pdf", 2, PICTURE)])

    def test_better_pages_and_changed_reasons_are_only_notices(self) -> None:
        cmp = fr.compare(
            baseline(file("a.pdf", PICTURE, SHARED)),
            baseline(file("a.pdf", None, PICTURE)),
        )
        self.assertFalse(cmp.worse)
        self.assertEqual(cmp.newly_editable, [("a.pdf", 1, PICTURE)])
        self.assertEqual(cmp.reason_changed, [("a.pdf", 2, SHARED, PICTURE)])

    def test_file_crashing_is_worse_and_its_pages_are_not_counted_twice(self) -> None:
        cmp = fr.compare(
            baseline(file("a.pdf", None, None)),
            baseline(file("a.pdf", status="crashed", error="子进程异常退出（0x80000003）")),
        )
        self.assertTrue(cmp.worse)
        self.assertEqual(cmp.file_worse, [("a.pdf", "ok", "crashed")])
        self.assertEqual(cmp.removed_pages, [])

    def test_missing_files_and_pages_are_worse_new_ones_are_not(self) -> None:
        cmp = fr.compare(
            baseline(file("a.pdf", None, None), file("b.pdf", None)),
            baseline(file("a.pdf", None), file("c.pdf", None)),
        )
        self.assertTrue(cmp.worse)
        self.assertEqual(cmp.removed_files, ["b.pdf"])
        self.assertEqual(cmp.added_files, ["c.pdf"])
        self.assertEqual(cmp.removed_pages, [("a.pdf", 2)])
        only_new = fr.compare(baseline(file("a.pdf", None)), baseline(file("a.pdf", None, None), file("c.pdf", None)))
        self.assertFalse(only_new.worse)
        self.assertEqual(only_new.added_pages, [("a.pdf", 2)])

    def test_environment_changes_are_reported(self) -> None:
        cmp = fr.compare(
            baseline(file("a.pdf", None), meta={"machine": "M", "pdfium_dll_sha256": "aa"}),
            baseline(file("a.pdf", None), meta={"machine": "N", "pdfium_dll_sha256": "bb"}),
        )
        self.assertFalse(cmp.worse)
        self.assertEqual({k for k, _, _ in cmp.meta_changes}, {"machine", "pdfium_dll_sha256"})


class BaselineFileTest(unittest.TestCase):
    def test_dump_is_valid_json_one_page_per_line_and_round_trips(self) -> None:
        current = baseline(file("b.pdf", SHARED), file("a.pdf", None, PICTURE), file("c.pdf", status="error", error="打开文档失败：Format"))
        text = fr.dump_baseline(current)
        self.assertEqual(json.loads(text), current)
        self.assertEqual([f["path"] for f in current["files"]], ["a.pdf", "b.pdf", "c.pdf"])
        self.assertIn('\n   {"page": 2, "editable": false, "reason": "' + PICTURE + '"}', text)
        self.assertEqual(json.loads(fr.dump_baseline(baseline())), baseline())


class CommandLineTest(unittest.TestCase):
    def run_main(self, *args: str) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = fr.main(list(args))
        return code, out.getvalue()

    def test_exit_codes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            res = d / "fidelity.json"
            base = d / "base.json"
            res.write_text(json.dumps(result(file("a.pdf", None, None))), encoding="utf-8")
            common = ("--set", "synthetic", "--result", str(res), "--baseline", str(base))

            code, out = self.run_main(*common)
            self.assertEqual(code, 0)
            self.assertIn("没有基线", out)
            self.assertEqual(self.run_main(*common, "--require-baseline")[0], 1)

            self.assertEqual(self.run_main(*common, "--update-baseline")[0], 0)
            self.assertTrue(base.is_file())
            self.assertEqual(self.run_main(*common)[0], 0)

            res.write_text(json.dumps(result(file("a.pdf", None, PICTURE))), encoding="utf-8")
            md = d / "summary.md"
            cand = d / "cand.json"
            code, out = self.run_main(*common, "--markdown", str(md), "--candidate", str(cand))
            self.assertEqual(code, 3)
            self.assertIn("`a.pdf` 第 2 页", md.read_text(encoding="utf-8"))
            self.assertEqual(json.loads(cand.read_text(encoding="utf-8"))["files"][0]["pages"][1]["editable"], False)
            self.assertEqual(self.run_main(*common, "--fail-on", "alarm")[0], 0)
            self.assertEqual(self.run_main(*common, "--fail-on", "never")[0], 0)


if __name__ == "__main__":
    unittest.main()
