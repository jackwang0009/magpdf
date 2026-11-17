"""struct_check.py 的单元测试。

运行：python -X dev -m unittest discover -s tests/corpus/tools -p "test_*.py"

要 pikepdf（tests/requirements-test.txt 里有），没装时整个跳过。测试用的 PDF 都是现做的小文件。
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

try:
    import pikepdf
except ImportError:  # pragma: no cover - 流水线上都装了
    pikepdf = None  # type: ignore[assignment]

if pikepdf is not None:
    import struct_check  # noqa: E402
    from ops_diff import saved_name  # noqa: E402


def make_pdf(path: Path, content: bytes = b"0 0 100 100 re f") -> None:
    """一页、一条内容流的小文件。不用对象流，交叉引用表是明文的，方便下面动手脚。"""
    path.parent.mkdir(parents=True, exist_ok=True)
    with pikepdf.new() as pdf:
        pdf.add_blank_page(page_size=(200, 200))
        pdf.pages[0].Contents = pdf.make_stream(content)
        pdf.save(path, object_stream_mode=pikepdf.ObjectStreamMode.disable, compress_streams=False)


def break_xref(path: Path) -> None:
    """把 startxref 指到别处：qpdf 找不到交叉引用表，重建之后照样打得开。"""
    data = path.read_bytes()
    at = data.rindex(b"startxref")
    path.write_bytes(data[:at] + b"startxref\n7\n%%EOF\n")


def break_stream_length(path: Path) -> None:
    """把内容流的 /Length 改错：qpdf 会自己把长度找回来。"""
    data = path.read_bytes()
    at = data.index(b"/Length ")
    end = data.index(b"\n", at) if data.index(b"\n", at) < data.index(b">>", at) else data.index(b">>", at)
    path.write_bytes(data[:at] + b"/Length 3 " + data[end:])


@unittest.skipIf(pikepdf is None, "没有装 pikepdf")
class KindTests(unittest.TestCase):
    def test_messages_are_sorted_by_what_they_say(self) -> None:
        f = Path("in") / "a.pdf"
        cases = {
            f"WARNING: {f}: file is damaged": "xref",
            f"WARNING: {f} (offset 1234): xref not found": "xref",
            f"WARNING: {f} (trailer, offset 99): trailer dictionary lacks /Size key": "xref",
            f"WARNING: {f} (xref stream, offset 5): Cross-reference stream data has the wrong size; expected = 6; actual = 3": "xref",
            f"{f}: unable to find /Root dictionary": "xref",
            f"WARNING: {f} (object 5 0, offset 77): stream dictionary lacks /Length key": "stream",
            f"WARNING: {f} (object 5 0, offset 77): expected endstream": "stream",
            f"WARNING: {f} (offset 77): error decoding stream data for object 8 0: Pl_DCT::decompress: JPEG data is corrupt": "stream",
            f"WARNING: {f} (object 5 0, offset 77): expected endobj": "object",
            f"WARNING: {f} (object 5 0, offset 77): unknown token while reading object; treating as string": "object",
            f"WARNING: {f} object stream 3 (object 9 0, offset 12): dictionary has duplicated key /AcroForm; last occurrence overrides earlier ones": "object",
            f"WARNING: {f}, object 4 0 at offset 60: kid 0 (from 0) Resources is missing or invalid; repairing": "pages",
            f"{f}: root of pages tree has no /Kids array": "pages",
            f"{f} (object 2 0): Loop detected in /Pages structure (getAllPages)": "pages",
            "WARNING: page object 3 0: item index 1 (from 0): ignoring non-stream in an array of streams": "pages",
            "content stream (content stream object 4 0): errors while decoding content stream": "content",
            "password required": "password",
            f"WARNING: {f}: something qpdf has not said before": "other",
        }
        for message, expected in cases.items():
            self.assertEqual(struct_check.kind_of(message, f), expected, message)

    def test_the_file_name_and_the_location_do_not_decide_the_kind(self) -> None:
        # 文件名里有 xref、pages，位置里有 object stream：归类只看冒号后面那句说明。
        f = Path("xref") / "pages tree" / "content stream.pdf"
        self.assertEqual(struct_check.kind_of(f"WARNING: {f} (object 5 0, offset 77): expected endobj", f), "object")
        self.assertEqual(struct_check.kind_of(f"WARNING: {f} object stream 3 (object 9 0, offset 12): expected endstream", f), "stream")
        self.assertEqual(struct_check.kind_of(f"WARNING: {f}: something qpdf has not said before", f), "other")


@unittest.skipIf(pikepdf is None, "没有装 pikepdf")
class QpdfCommandTests(unittest.TestCase):
    """--qpdf：`qpdf --check` 这个命令的退出码和输出怎么读。流水线上没有装 qpdf，这里只测读法。"""

    def test_exit_codes_and_output_of_qpdf_check(self) -> None:
        clean = "checking a.pdf\nPDF Version: 1.4\nFile is not encrypted\nFile is not linearized\nNo syntax or stream encoding errors found; the file may still contain\nerrors that qpdf cannot detect\n"
        self.assertEqual(struct_check.parse_qpdf_check(0, clean), ("ok", []))

        warned = (
            "checking a.pdf\nPDF Version: 1.7\nFile is not encrypted\nFile is not linearized\n"
            "WARNING: a.pdf, object 3 0 at offset 207: kid 0 (from 0) Resources is missing or invalid; repairing\n"
            "qpdf: operation succeeded with warnings\n"
        )
        status, messages = struct_check.parse_qpdf_check(3, warned)
        self.assertEqual(status, "problems")
        self.assertEqual(messages, ["WARNING: a.pdf, object 3 0 at offset 207: kid 0 (from 0) Resources is missing or invalid; repairing"])
        self.assertEqual(struct_check.kind_of(messages[0], Path("a.pdf")), "pages")

        # 有错误、文件没打开：打不开。警告也留着，错误排在前面。
        unopened = (
            "WARNING: a.pdf: file is damaged\nWARNING: a.pdf: Attempting to reconstruct cross-reference table\n"
            "qpdf: a.pdf: root of pages tree has no /Kids array\n"
        )
        status, messages = struct_check.parse_qpdf_check(2, unopened)
        self.assertEqual((status, len(messages)), ("unreadable", 3))
        self.assertEqual(messages[0], "a.pdf: root of pages tree has no /Kids array")
        self.assertEqual([struct_check.kind_of(m, Path("a.pdf")) for m in messages], ["pages", "xref", "xref"])

        # 有错误、文件已经打开：查到一半出错。
        halfway = "checking a.pdf\nPDF Version: 1.4\nFile is not encrypted\nERROR: content stream (content stream object 4 0): errors while decoding content stream\n"
        status, messages = struct_check.parse_qpdf_check(2, halfway)
        self.assertEqual((status, [struct_check.kind_of(m) for m in messages]), ("error", ["content"]))

        # 崩溃之类别的退出码：记成 error，不当成没问题。
        self.assertEqual(struct_check.parse_qpdf_check(-1073741819, "")[0], "error")
        self.assertEqual(struct_check.parse_qpdf_check(3, "")[0], "problems")


@unittest.skipIf(pikepdf is None, "没有装 pikepdf")
class CheckFileTests(unittest.TestCase):
    def test_a_sound_file_has_no_problems(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "good.pdf"
            make_pdf(path)
            self.assertEqual(struct_check.check_file(path), {"status": "ok", "problems": 0, "kinds": {}})

    def test_damage_qpdf_can_work_around_counts_as_problems(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "xref" / "pages.pdf"  # 目录名和文件名都是会误导归类的词
            make_pdf(path)
            break_xref(path)
            side = struct_check.check_file(path)
            self.assertEqual(side["status"], "problems")
            self.assertGreater(side["problems"], 0)
            self.assertEqual(set(side["kinds"]), {"xref"}, "交叉引用表坏了，不该因为文件名归到页树里")
            self.assertEqual(sum(side["kinds"].values()), side["problems"])
            self.assertNotIn("messages", side, "默认不带 qpdf 的原话")

            make_pdf(path)
            break_stream_length(path)
            side = struct_check.check_file(path)
            self.assertEqual(side["status"], "problems")
            self.assertIn("stream", side["kinds"])

    def test_a_file_qpdf_cannot_open_is_unreadable(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "junk.pdf"
            path.write_bytes(b"this is not a pdf at all\n" * 20)
            side = struct_check.check_file(path)
            self.assertEqual(side["status"], "unreadable")
            self.assertGreaterEqual(side["problems"], 1)
            self.assertEqual(struct_check.check_file(Path(tmp) / "missing.pdf")["status"], "unreadable")

    def test_original_messages_are_opt_in(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bad.pdf"
            make_pdf(path)
            break_xref(path)
            side = struct_check.check_file(path, with_messages=True)
            self.assertEqual(len(side["messages"]), side["problems"])
            self.assertTrue(any("xref" in message or "damaged" in message for message in side["messages"]))


@unittest.skipIf(pikepdf is None, "没有装 pikepdf")
class RoundtripModeTests(unittest.TestCase):
    def run_main(self, *args: str) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = struct_check.main(list(args))
        return code, out.getvalue()

    def test_originals_are_paired_with_saved_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            corpus, saved = root / "corpus", root / "saved"
            entries: list[dict[str, Any]] = []
            # 来源是命令行工具给的：输入目录下的第一级子目录名，直接放在输入目录下的文件用输入目录的名字。
            for rel, source in (("good.pdf", "corpus"), ("repaired.pdf", "corpus"), ("sub/both.pdf", "sub"), ("sub/broken.pdf", "sub"), ("crash.pdf", "corpus")):
                make_pdf(corpus / rel)
                entries.append({"path": rel, "source": source, "status": "ok", "pages": []})
                if rel != "crash.pdf":  # 崩溃的文件没有保存结果
                    make_pdf(saved / saved_name(source, rel))
            break_xref(corpus / "repaired.pdf")  # 原件有问题，保存结果是好的
            break_xref(corpus / "sub" / "both.pdf")
            break_xref(saved / saved_name("sub", "sub/both.pdf"))  # 两边都有问题
            break_stream_length(saved / saved_name("sub", "sub/broken.pdf"))  # 原件是好的，保存结果被写坏了
            roundtrip = root / "roundtrip.json"
            roundtrip.write_text(json.dumps({"options": {"root": str(corpus)}, "files": entries}), encoding="utf-8")

            out = root / "out" / "struct.json"
            code, text = self.run_main("--roundtrip", str(roundtrip), "--save-dir", str(saved), "--json", str(out))
            self.assertEqual(code, 0, text)
            result = json.loads(out.read_text(encoding="utf-8"))
            self.assertEqual((result["kind"], result["version"]), (struct_check.RESULT_KIND, struct_check.VERSION))
            self.assertEqual(result["tool"]["pikepdf"], pikepdf.__version__)
            by_path = {record["path"]: record for record in result["files"]}
            self.assertEqual([record["path"] for record in result["files"]], [e["path"] for e in entries], "顺序和 roundtrip 的结果一样")
            classes = {
                path: struct_check.class_of(record["original"], record["saved"]) for path, record in by_path.items() if "saved" in record
            }
            self.assertEqual(classes, {"good.pdf": "ok_ok", "repaired.pdf": "bad_ok", "sub/both.pdf": "bad_bad", "sub/broken.pdf": "ok_bad"})
            self.assertEqual(by_path["crash.pdf"]["skipped"], "no saved file")
            self.assertEqual(
                result["totals"],
                {
                    "corpus": {"files": 3, "checked": 2, "ok_ok": 1, "bad_ok": 1, "not_checked": 1},
                    "sub": {"files": 2, "checked": 2, "bad_bad": 1, "ok_bad": 1},
                },
            )
            # 结果里只有状态和计数：不带 qpdf 的原话，也就不带路径、对象号。
            for record in result["files"]:
                for side in ("original", "saved"):
                    self.assertEqual(set(record.get(side, {})) - {"status", "problems", "kinds", "undecoded"}, set())
            self.assertNotIn(str(corpus), json.dumps(result["files"]))
            self.assertIn("| sub | 2 | 2 | 0 | 0 | 1 | 1 | 0 | 0 |", text)
            self.assertIn("sub/broken.pdf", text, "被写坏的文件要在屏幕上点名")

            code, text = self.run_main("--roundtrip", str(roundtrip), "--save-dir", str(saved), "--max-list", "0")
            self.assertEqual(code, 0)
            self.assertIn("另有 1 个", text)

    def test_a_single_pair(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_pdf(root / "a.pdf")
            make_pdf(root / "b.pdf")
            break_xref(root / "b.pdf")
            out = root / "pair.json"
            code, _ = self.run_main(str(root / "a.pdf"), str(root / "b.pdf"), "--json", str(out), "--messages")
            self.assertEqual(code, 0)
            record = json.loads(out.read_text(encoding="utf-8"))["files"][0]
            self.assertEqual((record["original"]["status"], record["saved"]["status"]), ("ok", "problems"))
            self.assertTrue(record["saved"]["messages"])


if __name__ == "__main__":
    unittest.main()
