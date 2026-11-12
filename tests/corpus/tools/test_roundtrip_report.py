"""roundtrip_model.py、roundtrip_report.py、corpus_fingerprint.py 的单元测试。

运行：python -X dev -m unittest discover -s tests/corpus/tools -p "test_*.py"

只用标准库；缩略图那一条要 Pillow，没装时跳过。
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any, Iterable

sys.path.insert(0, str(Path(__file__).resolve().parent))

import roundtrip_model as model  # noqa: E402
import roundtrip_report as report  # noqa: E402
from corpus_fingerprint import corpus_fingerprint  # noqa: E402

THRESHOLDS = {"channel_tolerance": 24, "max_diff_ratio": 0.0001, "max_region_pixels": 16}


def page(
    index: int,
    status: str = "pass",
    *,
    objects: int = 1,
    diff_pixels: int = 0,
    largest: int = 0,
    hints: Iterable[str] = (),
    images: dict[str, str] | None = None,
    error: str | None = None,
) -> dict[str, Any]:
    """`roundtrip --json` 里的一页。"""
    total = 40000
    record: dict[str, Any] = {
        "index": index,
        "width": 100.0,
        "height": 100.0,
        "status": status,
        "error": error,
        "diff": None
        if status == "error"
        else {"diff_pixels": diff_pixels, "total_pixels": total, "diff_ratio": diff_pixels / total, "largest_region": largest, "bbox": None},
        "stats": {"objects": objects, "text": 0, "path": objects, "image": 0, "shading": 0, "form": 0},
        "stats_after": None,
        "text_shift": None,
        "color_changed": None,
        "hints": list(hints),
        "timings": {},
    }
    if images is not None:
        record["diff_images"] = images
    return record


def diff_page(index: int, *, objects: int = 1, hints: Iterable[str] = ("color_changed",), images: dict[str, str] | None = None) -> dict[str, Any]:
    return page(index, "diff", objects=objects, diff_pixels=500, largest=400, hints=hints, images=images)


def file(path: str, source: str, pages: list[dict[str, Any]], *, status: str = "ok", error: str | None = None, exit_code: int | None = None) -> dict[str, Any]:
    return {
        "path": path,
        "source": source,
        "status": status,
        "error": error,
        "exit_code": exit_code,
        "original_bytes": 100,
        "saved_bytes": 100,
        "page_count": len(pages),
        "pages": pages,
        "timings": {},
    }


def roundtrip(files: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "tool": {"name": "magpdf-cli roundtrip", "version": "0.1.0", "pdfium": "159.0.8138.0"},
        "options": {"root": "corpus", "isolate": True, "timeout_s": 60, "jobs": 4, "scale": 2.0, "thresholds": THRESHOLDS, "calibrate": False},
        "started_unix_s": 1794449568,
        "elapsed_ms": 1000,
        "files": files,
    }


def ops_page(number: int, **counts: int) -> dict[str, Any]:
    """`ops_diff.py --json` 里的一页；counts 里没写的类别是 0。"""
    full = {key: int(counts.get(key, 0)) for key in model.OPS_KEYS}
    return {"page": number, "counts": full, "color": {"fill DeviceCMYK→DeviceRGB": full["color"]} if full["color"] else {}}


def ops(files: list[dict[str, Any]]) -> dict[str, Any]:
    return {"files": files}


def many_pass_files(count: int, source: str = "office-word", *, objects: int = 1, prefix: str = "f") -> list[dict[str, Any]]:
    return [file(f"{prefix}{i:04d}.pdf", source, [page(0, objects=objects)]) for i in range(count)]


def snapshot(files: list[dict[str, Any]], ops_json: dict[str, Any] | None = None, *, name: str = "set", prefix: str = "") -> model.Snapshot:
    return model.load_run(roundtrip(files), ops_json, name=name, source_prefix=prefix)


def as_baseline(snap: model.Snapshot, **meta: Any) -> model.Snapshot:
    """写成基线文本再读回来，和真实的流程一样。"""
    return model.loads_baseline(model.dumps_baseline(snap, {"has_ops": snap.has_ops, **meta}))


class SummaryTests(unittest.TestCase):
    def test_counts_follow_the_cli_conventions(self) -> None:
        files = [
            file("a.pdf", "office-word", [page(0, objects=5), page(1, objects=0), diff_page(2, objects=3, hints=("text_moved", "transparency"))]),
            file("b.pdf", "office-word", [page(0, objects=1)]),
            file("c.pdf", "design", [diff_page(0, objects=9, hints=("shading_lost",))]),
            file("d.pdf", "design", [], status="load_failed", error="打开文档失败：Password"),
            file("e.pdf", "public", [], status="crashed", error="子进程异常退出（0x80000003）", exit_code=-2147483645),
            file("f.pdf", "public", [page(0, "error", objects=0, error="加载页面失败：Other(0)")]),
        ]
        by_source = model.summarize(snapshot(files).files)
        self.assertEqual(list(by_source), ["design", "office-word", "public"])

        word = by_source["office-word"]
        self.assertEqual((word.files, word.pages, word.compared, word.passed, word.diff), (2, 4, 4, 3, 1))
        self.assertEqual(word.empty_passed, 1)
        self.assertEqual(word.ratio_all, 0.75)
        # 扣除无对象页：比对页 4 − 1 = 3，其中无差异 3 − 1 = 2。
        self.assertEqual((word.with_objects, word.passed_with_objects), (3, 2))
        self.assertAlmostEqual(word.ratio_nonempty or 0.0, 2 / 3)
        self.assertEqual(word.hints, {"text_moved": 1}, "差异页只按第一条线索计数")

        design = by_source["design"]
        self.assertEqual((design.files, design.load_failed, design.diff, design.ratio_all), (2, 1, 1, 0.0))

        public = by_source["public"]
        self.assertEqual((public.crashed, public.errored, public.compared), (1, 1, 0))
        self.assertIsNone(public.ratio_all, "没有比对页时没有比例")
        self.assertIsNone(public.ratio_nonempty)

        total = model.total_of(by_source.values())
        self.assertEqual((total.files, total.passed, total.diff, total.errored, total.crashed, total.load_failed), (6, 3, 2, 1, 1, 1))

    def test_empty_pages_do_not_inflate_the_nonempty_ratio(self) -> None:
        # 公开测试集的情形：三分之一的页没有对象。10 页有对象、其中 1 页有差异，另有 90 页空白页。
        files = many_pass_files(9) + [file("bad.pdf", "office-word", [diff_page(0)])] + many_pass_files(90, objects=0, prefix="empty")
        s = model.summarize(snapshot(files).files)["office-word"]
        self.assertAlmostEqual(s.ratio_all or 0.0, 0.99)
        self.assertAlmostEqual(s.ratio_nonempty or 0.0, 0.9)

    def test_diff_page_without_objects_stays_in_the_denominator(self) -> None:
        # 保存失败时整份文件的页都记为有差异，其中没有对象的页也是实打实的失败，不能当“无对象页”扣掉。
        files = [file("a.pdf", "scan", [diff_page(0, objects=0), page(1, objects=2)], status="save_failed")]
        s = model.summarize(snapshot(files).files)["scan"]
        self.assertEqual((s.empty_passed, s.with_objects, s.passed_with_objects, s.save_failed), (0, 2, 1, 1))
        self.assertEqual(s.ratio_nonempty, 0.5)

    def test_source_prefix_keeps_pdfium_subdirectories_apart(self) -> None:
        files = [file("a.pdf", "resources", [page(0)]), file("pixel/b.pdf", "pixel", [page(0)])]
        snap = snapshot(files, prefix="public/pdfium/")
        self.assertEqual(sorted(model.summarize(snap.files)), ["public/pdfium/pixel", "public/pdfium/resources"])

    def test_operator_level_counts(self) -> None:
        files = [
            file("clean.pdf", "s", [page(0, objects=2)]),
            file("empty.pdf", "s", [page(0, objects=0)]),
            file("gray.pdf", "s", [page(0, objects=4)]),  # 像素无差异，颜色空间变了
            file("bad.pdf", "s", [diff_page(0, objects=3)]),
            file("nosave.pdf", "s", [page(0, objects=1)]),  # 没有保存结果
            file("broken.pdf", "s", [page(0, objects=1)]),  # qpdf 读页出错
            file("pages.pdf", "s", [page(0, objects=1), page(1, objects=1)]),  # 两边数出的页数不一样
            file("gone.pdf", "s", [page(0, objects=1)]),  # 操作符级结果里没有这个文件
        ]
        ops_json = ops(
            [
                {"path": "clean.pdf", "source": "s", "pages": [ops_page(1)]},
                {"path": "empty.pdf", "source": "s", "pages": [ops_page(1)]},
                {"path": "gray.pdf", "source": "s", "pages": [ops_page(1, color=6)]},
                {"path": "bad.pdf", "source": "s", "pages": [ops_page(1, color=2, clip=1)]},
                {"path": "nosave.pdf", "source": "s", "skipped": "no saved file"},
                {"path": "broken.pdf", "source": "s", "pages": [{"page": 1, "error": "PdfError"}]},
                {"path": "pages.pdf", "source": "s", "pages": [ops_page(1), ops_page(2), ops_page(3)]},
            ]
        )
        snap = snapshot(files, ops_json)
        s = model.summarize(snap.files)["s"]
        self.assertEqual(s.compared, 9)
        self.assertEqual((s.ops_pages, s.ops_missing), (4, 5))
        self.assertEqual((s.ops_diff_pages, s.pixel_pass_ops_diff, s.strict_pass, s.strict_pass_empty), (2, 1, 2, 1))
        # 两项都无差异、扣除无对象页：做成操作符级比对的 4 页里去掉 1 页空白页，剩 3 页，其中 1 页两项都无差异。
        self.assertEqual((s.strict_with_objects, s.strict_passed_with_objects), (3, 1))
        self.assertAlmostEqual(s.strict_ratio_nonempty or 0.0, 1 / 3)
        self.assertEqual(s.ops_counts, {"color": 8, "clip": 1})
        self.assertEqual(s.ops_count_pages, {"color": 2, "clip": 1})

        notes = {f.path: f.ops_note for f in snap.files}
        self.assertEqual(notes["nosave.pdf"], "没有保存结果")
        self.assertIn("页数对不上", notes["pages.pdf"])
        self.assertIn("没有这个文件", notes["gone.pdf"])
        broken = next(f for f in snap.files if f.path == "broken.pdf")
        self.assertIsNone(broken.pages[0].ops)
        self.assertTrue(broken.pages[0].ops_error)

    def test_error_text_drops_child_stderr(self) -> None:
        text = "子进程异常退出（0x80000003）；stderr：thread main panicked: 发货人 SAMPLE CO LTD"
        files = [file("x.pdf", "s", [], status="crashed", error=text, exit_code=-2147483645)]
        snap = snapshot(files)
        self.assertEqual(snap.files[0].error, "子进程异常退出（0x80000003）")


class BaselineTests(unittest.TestCase):
    def test_roundtrip_through_text(self) -> None:
        files = [
            file("a.pdf", "s", [page(0, objects=5), diff_page(1, objects=3)]),
            file("b.pdf", "s", [], status="crashed"),
            file("c.pdf", "s", [page(0, "error", objects=0)]),
        ]
        ops_json = ops([{"path": "a.pdf", "source": "s", "pages": [ops_page(1), ops_page(2, color=4, gs=1)]}])
        snap = snapshot(files, ops_json, name="synthetic")
        text = model.dumps_baseline(snap, {"has_ops": True, "commit": "abc"})

        self.assertTrue(text.endswith("\n") and "\r" not in text)
        page_lines = [line for line in text.splitlines() if line.lstrip().startswith('{"page"')]
        self.assertEqual(len(page_lines), 3, "一页一行，git diff 才看得出是哪一页变了")
        self.assertNotIn("hints", text)
        self.assertNotIn("diff_images", text)

        back = model.loads_baseline(text)
        self.assertEqual(back.name, "synthetic")
        self.assertTrue(back.has_ops)
        self.assertEqual(back.meta["commit"], "abc")
        self.assertEqual([(f.path, f.status, len(f.pages)) for f in back.files], [("a.pdf", "ok", 2), ("b.pdf", "crashed", 0), ("c.pdf", "ok", 1)])
        second = back.files[0].pages[1]
        self.assertEqual((second.pixel, second.objects, second.diff_pixels, second.largest_region, second.ops), ("diff", 3, 500, 400, {"color": 4, "gs": 1}))
        self.assertEqual(back.files[0].pages[0].ops, {}, "做了操作符级比对、没有差异，存成空表")
        self.assertIsNone(back.files[2].pages[0].ops, "没做操作符级比对的页不写 ops")

        same = model.compare(back, snap)
        self.assertEqual((same.level, same.newly_failed, same.newly_passed, same.file_changes), ("ok", [], [], []))
        self.assertEqual(model.dumps_baseline(back, {"has_ops": True, "commit": "abc"}), text, "读回来再写，逐字节相同")

    def test_rejects_foreign_json(self) -> None:
        with self.assertRaises(model.BaselineError):
            model.loads_baseline(json.dumps(roundtrip([])))
        with self.assertRaises(model.BaselineError):
            model.loads_baseline(json.dumps({"kind": model.BASELINE_KIND, "format": 99, "files": []}))


class CompareTests(unittest.TestCase):
    def test_one_new_failure_below_the_alarm_line(self) -> None:
        base = as_baseline(snapshot(many_pass_files(400)))
        files = many_pass_files(400)
        files[7] = file("f0007.pdf", "office-word", [diff_page(0)])
        result = model.compare(base, snapshot(files))
        self.assertEqual(result.level, "worse")
        self.assertEqual([(c.path, c.number, c.before, c.after) for c in result.newly_failed], [("f0007.pdf", 1, "pass", "diff")])
        self.assertEqual(result.alarms, [])
        delta = result.sources["office-word"]
        self.assertAlmostEqual(delta.delta_pp or 0.0, -0.25)
        self.assertEqual((delta.newly_failed, delta.alarm), (1, False))

    def test_ratio_drop_over_half_a_point_is_an_alarm(self) -> None:
        base = as_baseline(snapshot(many_pass_files(100)))
        files = many_pass_files(100)
        files[3] = file("f0003.pdf", "office-word", [diff_page(0)])
        result = model.compare(base, snapshot(files))
        self.assertEqual(result.level, "alarm")
        self.assertEqual(len(result.alarms), 1)
        self.assertIn("office-word", result.alarms[0])
        self.assertTrue(result.sources["office-word"].alarm)
        self.assertTrue(result.worse, "新变坏的页照样列出来")

    def test_exactly_half_a_point_is_not_over_the_line(self) -> None:
        # 《测试计划》写的是“低 0.5 个百分点以上”：正好 0.5 不算。
        base = as_baseline(snapshot(many_pass_files(200)))
        files = many_pass_files(200)
        files[0] = file("f0000.pdf", "office-word", [diff_page(0)])
        result = model.compare(base, snapshot(files))
        self.assertAlmostEqual(result.sources["office-word"].delta_pp or 0.0, -0.5)
        self.assertEqual(result.level, "worse")
        looser = model.compare(base, snapshot(files), max_drop_pp=0.4)
        self.assertEqual(looser.level, "alarm")

    def test_alarm_uses_the_ratio_without_empty_pages(self) -> None:
        empties = many_pass_files(1000, objects=0, prefix="empty")
        base = as_baseline(snapshot(many_pass_files(10) + empties))
        files = many_pass_files(10)
        files[0] = file("f0000.pdf", "office-word", [diff_page(0)])
        result = model.compare(base, snapshot(files + empties))
        delta = result.sources["office-word"]
        self.assertAlmostEqual(delta.delta_pp or 0.0, -10.0)  # 全部比对页的口径下只掉 0.1 个百分点
        self.assertEqual(result.level, "alarm")

    def test_each_source_is_judged_on_its_own(self) -> None:
        base_files = many_pass_files(1000, "office-word") + many_pass_files(10, "design", prefix="d")
        base = as_baseline(snapshot(base_files))
        files = many_pass_files(1000, "office-word") + many_pass_files(10, "design", prefix="d")
        files[-1] = file("d0009.pdf", "design", [diff_page(0)])
        result = model.compare(base, snapshot(files))
        self.assertEqual(result.level, "alarm", "合起来只掉 0.1 个百分点，但 design 这一类掉了 10 个")
        self.assertFalse(result.sources["office-word"].alarm)
        self.assertTrue(result.sources["design"].alarm)

    def test_improvement_is_not_a_regression(self) -> None:
        base_files = many_pass_files(20)
        base_files[2] = file("f0002.pdf", "office-word", [diff_page(0)])
        result = model.compare(as_baseline(snapshot(base_files)), snapshot(many_pass_files(20)))
        self.assertEqual(result.level, "ok")
        self.assertEqual([(c.path, c.before, c.after) for c in result.newly_passed], [("f0002.pdf", "diff", "pass")])
        self.assertGreater(result.sources["office-word"].delta_pp or 0.0, 0)
        self.assertTrue(any("更新基线" in text for text in result.notices))

    def test_new_crash_is_an_alarm_and_its_pages_count_as_newly_failed(self) -> None:
        base_files = many_pass_files(1000) + [file("big.pdf", "office-word", [page(0), page(1), diff_page(2)])]
        now_files = many_pass_files(1000) + [file("big.pdf", "office-word", [], status="crashed", exit_code=-1073741819)]
        result = model.compare(as_baseline(snapshot(base_files)), snapshot(now_files))
        self.assertEqual(result.level, "alarm")
        self.assertEqual([(c.path, c.before, c.after) for c in result.new_crashes], [("big.pdf", "ok", "crashed")])
        self.assertEqual([(c.number, c.before, c.after) for c in result.newly_failed], [(1, "pass", "lost"), (2, "pass", "lost")])
        self.assertEqual([(c.number, c.before, c.after) for c in result.other_changes], [(3, "diff", "lost")])
        # 崩溃的文件不算比对页，比例反而升了一点——所以崩溃要单独算报警。
        self.assertGreater(result.sources["office-word"].delta_pp or 0.0, 0)

    def test_timeout_counts_like_a_crash_and_known_crashes_do_not_repeat(self) -> None:
        base_files = [file("a.pdf", "s", [page(0)]), file("known.pdf", "s", [], status="crashed")]
        now_files = [file("a.pdf", "s", [], status="timed_out"), file("known.pdf", "s", [], status="crashed")]
        result = model.compare(as_baseline(snapshot(base_files)), snapshot(now_files))
        self.assertEqual([c.path for c in result.new_crashes], ["a.pdf"])
        self.assertEqual(result.level, "alarm")

    def test_fixed_crash_shows_up_as_improvement(self) -> None:
        base_files = [file("a.pdf", "s", [], status="crashed"), file("b.pdf", "s", [page(0)])]
        now_files = [file("a.pdf", "s", [page(0), diff_page(1)]), file("b.pdf", "s", [page(0)])]
        result = model.compare(as_baseline(snapshot(base_files)), snapshot(now_files))
        self.assertEqual(result.new_crashes, [])
        self.assertEqual([(c.path, c.number, c.before, c.after) for c in result.newly_passed], [("a.pdf", 1, "lost", "pass")])
        self.assertEqual([(c.number, c.before, c.after) for c in result.other_changes], [(2, "lost", "diff")])
        # 这个来源的比例从 100% 掉到 66.7%（多出来一页有差异的页），比例规则照样报警：语料没变，这是真的掉了。
        self.assertEqual(result.level, "alarm")

    def test_file_that_stops_opening(self) -> None:
        base_files = many_pass_files(1000) + [file("x.pdf", "office-word", [diff_page(0)])]
        now_files = many_pass_files(1000) + [file("x.pdf", "office-word", [], status="load_failed")]
        result = model.compare(as_baseline(snapshot(base_files)), snapshot(now_files))
        self.assertEqual([(c.before, c.after) for c in result.worsened_files], [("ok", "load_failed")])
        self.assertEqual(result.newly_failed, [], "基线里这一页本来就有差异")
        self.assertEqual(result.level, "worse")

    def test_page_error_transitions(self) -> None:
        base_files = many_pass_files(1000) + [file("x.pdf", "office-word", [page(0), diff_page(1)])]
        now_files = many_pass_files(1000) + [file("x.pdf", "office-word", [page(0, "error", objects=0), page(1, "error", objects=0)])]
        result = model.compare(as_baseline(snapshot(base_files)), snapshot(now_files))
        self.assertEqual([(c.number, c.before, c.after) for c in result.newly_failed], [(1, "pass", "error")])
        self.assertEqual([(c.number, c.before, c.after) for c in result.other_changes], [(2, "diff", "error")])

    def test_corpus_changes(self) -> None:
        base = as_baseline(snapshot(many_pass_files(10)))
        added = model.compare(base, snapshot(many_pass_files(10) + [file("new.pdf", "office-word", [diff_page(0)])]))
        self.assertEqual([f.path for f in added.added_files], ["new.pdf"])
        self.assertEqual(added.newly_failed, [], "基线里没有的文件没有东西可比")
        # 新文件带来的差异页让这个来源的比例掉了 9 个百分点，比例规则报警：这是在提醒基线该更新了。
        self.assertEqual(added.level, "alarm")

        only_pass = model.compare(base, snapshot(many_pass_files(10) + [file("new.pdf", "office-word", [page(0)])]))
        self.assertEqual(only_pass.level, "ok")
        self.assertTrue(any("不在基线里" in text for text in only_pass.notices))

        removed = model.compare(base, snapshot(many_pass_files(9)))
        self.assertEqual([f.path for f in removed.removed_files], ["f0009.pdf"])
        self.assertEqual(removed.level, "worse", "基线里的文件没跑到，不能当没事")

    def test_operator_level_regression_with_pixels_unchanged(self) -> None:
        def run(color: int) -> model.Snapshot:
            files = many_pass_files(400)
            ops_files = [{"path": f["path"], "source": f["source"], "pages": [ops_page(1)]} for f in files]
            ops_files[5] = {"path": files[5]["path"], "source": "office-word", "pages": [ops_page(1, color=color)]}
            return snapshot(files, ops(ops_files))

        worse = model.compare(as_baseline(run(0)), run(3))
        self.assertEqual(worse.newly_failed, [])
        self.assertEqual([(c.path, c.number) for c in worse.ops_newly_failed], [("f0005.pdf", 1)])
        self.assertEqual(worse.level, "worse")
        self.assertAlmostEqual(worse.sources["office-word"].strict_delta_pp or 0.0, -0.25)
        self.assertAlmostEqual(worse.sources["office-word"].delta_pp or 0.0, 0.0)

        better = model.compare(as_baseline(run(3)), run(0))
        self.assertEqual([(c.path, c.number) for c in better.ops_newly_passed], [("f0005.pdf", 1)])
        self.assertEqual(better.level, "ok")

        still_dirty = model.compare(as_baseline(run(3)), run(7))
        self.assertEqual((still_dirty.ops_newly_failed, still_dirty.ops_newly_passed, still_dirty.level), ([], [], "ok"))

    def test_operator_level_ratio_alarm(self) -> None:
        def run(dirty: int) -> model.Snapshot:
            files = many_pass_files(100)
            ops_files = [
                {"path": f["path"], "source": f["source"], "pages": [ops_page(1, color=1 if i < dirty else 0)]} for i, f in enumerate(files)
            ]
            return snapshot(files, ops(ops_files))

        result = model.compare(as_baseline(run(0)), run(2))
        self.assertEqual(result.level, "alarm")
        self.assertIn("像素和操作符都无差异", result.alarms[0])
        self.assertAlmostEqual(result.sources["office-word"].strict_delta_pp or 0.0, -2.0)

    def test_losing_the_operator_level_results_is_not_silent(self) -> None:
        files = many_pass_files(5)
        with_ops = snapshot(files, ops([{"path": f["path"], "source": f["source"], "pages": [ops_page(1, color=2)]} for f in files]))
        without = snapshot(files)
        result = model.compare(as_baseline(with_ops), without)
        self.assertEqual(result.level, "worse")
        self.assertEqual(result.ops_newly_passed, [], "这次没有操作符级结果，不能算成变好")

        partial_ops = [{"path": f["path"], "source": f["source"], "pages": [ops_page(1, color=2)]} for f in files[:-1]]
        partial_ops.append({"path": files[-1]["path"], "source": "office-word", "skipped": "no saved file"})
        partial = model.compare(as_baseline(with_ops), snapshot(files, ops(partial_ops)))
        self.assertEqual((partial.ops_lost, partial.level), (1, "worse"))

        first_time = model.compare(as_baseline(without), with_ops)
        self.assertEqual(first_time.level, "ok")
        self.assertTrue(any("没有操作符级结果" in text for text in first_time.notices))

    def test_notices_when_conditions_differ(self) -> None:
        base = as_baseline(
            snapshot(many_pass_files(3)),
            pdfium_dll_sha256="aaa",
            machine="JACK",
            thresholds=THRESHOLDS,
            scale=2.0,
            corpus={"files": 3, "bytes": 10, "sha256": "111"},
        )
        current = snapshot(many_pass_files(3))
        current.meta.update(pdfium_dll_sha256="bbb", machine="runner", corpus={"files": 3, "bytes": 10, "sha256": "222"})
        result = model.compare(base, current)
        text = "\n".join(result.notices)
        self.assertIn("PDFium 库和基线用的不是同一份", text)
        self.assertIn("语料和基线不是同一份", text)
        self.assertIn("另一台机器", text)
        self.assertEqual(result.level, "ok", "条件不同只提示，不改判定")


class FingerprintTests(unittest.TestCase):
    def test_depends_on_names_and_bytes_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            one, two = Path(tmp) / "one", Path(tmp) / "two"
            for root, order in ((one, ("a.pdf", "sub/b.PDF")), (two, ("sub/b.PDF", "a.pdf"))):
                for rel in order:
                    target = root / rel
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(b"%PDF-1.7\n" + rel.encode())
            (two / "readme.txt").write_text("不是 PDF，不算", encoding="utf-8")
            first, second = corpus_fingerprint(one), corpus_fingerprint(two)
            self.assertEqual(first, second)
            self.assertEqual(first["files"], 2)

            # autocrlf 把换行改成 CRLF：文件名、文件数都不变，指纹变了。
            (two / "a.pdf").write_bytes(b"%PDF-1.7\r\na.pdf")
            self.assertNotEqual(corpus_fingerprint(two)["sha256"], first["sha256"])


class ReportTests(unittest.TestCase):
    def test_operator_detail_names_are_stripped(self) -> None:
        self.assertEqual(report.safe_ops_key("fill Separation(ACME Red,alt=DeviceCMYK)→DeviceRGB"), "fill Separation→DeviceRGB")
        self.assertEqual(report.safe_ops_key("image Indexed(Separation(ACME Red,alt=DeviceCMYK),hival=3)→DeviceRGB"), "image Indexed→DeviceRGB")
        self.assertEqual(report.safe_ops_key("font TrueType:ABCDEF+ACMESans→Type1:Helvetica"), "字体换了")
        self.assertEqual(report.safe_ops_key("Tc 3→0"), "Tc 3→0")
        merged = report.safe_ops_detail({"color": {"fill Separation(A,alt=DeviceCMYK)→DeviceRGB": 2, "fill Separation(B,alt=DeviceCMYK)→DeviceRGB": 3}})
        self.assertEqual(merged, {"color": {"fill Separation→DeviceRGB": 5}})

    def test_exit_codes(self) -> None:
        self.assertEqual(report.exit_code_of("ok", "any"), 0)
        self.assertEqual(report.exit_code_of("none", "any"), 0)
        self.assertEqual(report.exit_code_of("worse", "any"), 3)
        self.assertEqual(report.exit_code_of("alarm", "any"), 2)
        self.assertEqual(report.exit_code_of("worse", "alarm"), 0)
        self.assertEqual(report.exit_code_of("alarm", "alarm"), 2)
        self.assertEqual(report.exit_code_of("alarm", "never"), 0)

    def run_main(self, *args: str) -> tuple[int, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = report.main(list(args))
        return code, out.getvalue() + err.getvalue()

    def write_run(self, root: Path, files: list[dict[str, Any]], *, with_images: bool = False) -> None:
        root.mkdir(parents=True, exist_ok=True)
        (root / "roundtrip.json").write_text(json.dumps(roundtrip(files), ensure_ascii=False), encoding="utf-8")
        ops_files = []
        for f in files:
            if f["status"] != "ok":
                ops_files.append({"path": f["path"], "source": f["source"], "skipped": "no saved file"})
                continue
            pages = []
            for p in f["pages"]:
                detail = {"fill Separation(ACME Red,alt=DeviceCMYK)→DeviceRGB": 2} if p["status"] == "diff" else {}
                pages.append({"page": p["index"] + 1, "counts": {k: (2 if k == "color" and detail else 0) for k in model.OPS_KEYS}, "color": detail})
            ops_files.append({"path": f["path"], "source": f["source"], "pages": pages})
        (root / "ops.json").write_text(json.dumps(ops(ops_files), ensure_ascii=False), encoding="utf-8")
        if with_images:
            from PIL import Image  # noqa: PLC0415

            (root / "diff").mkdir(exist_ok=True)
            for f in files:
                for p in f["pages"]:
                    for name in (p.get("diff_images") or {}).values():
                        Image.new("RGB", (300, 400), (200, 30, 30)).save(root / "diff" / name)

    def sample_files(self, *, broken: bool) -> list[dict[str, Any]]:
        images = {"a": "s__bad__p001_a.png", "b": "s__bad__p001_b.png", "heat": "s__bad__p001_diff.png"}
        files = many_pass_files(30, "office-word")
        files.append(file("bad.pdf", "office-word", [diff_page(0, images=images)] if broken else [page(0)]))
        files.append(file("known.pdf", "office-word", [diff_page(0)]))
        crash_text = "子进程异常退出（0x80000003）；stderr：panicked at 发货人 SAMPLE CO LTD"
        files.append(file("crash.pdf", "office-word", [], status="crashed", error=crash_text, exit_code=-2147483645))
        return files

    def test_single_run_end_to_end(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.write_run(root / "good", self.sample_files(broken=False))
            common = ("--name", "office", "--out", str(root / "report.html"), "--summary", str(root / "summary.json"), "--markdown", str(root / "summary.md"))
            good = ("--roundtrip", str(root / "good" / "roundtrip.json"), "--ops", str(root / "good" / "ops.json"))

            # 第一次：没有基线，只出报告，顺手写出候选基线。
            code, text = self.run_main(*good, *common, "--baseline-out", str(root / "baselines"))
            self.assertEqual(code, 0, text)
            self.assertIn("没有基线", text)
            baseline = root / "baselines" / "office.json"
            self.assertTrue(baseline.is_file())
            page_html = (root / "report.html").read_text(encoding="utf-8")
            self.assertIn('class="banner none"', page_html)

            # 没有基线、又要求必须有：退出码 1。
            code, _ = self.run_main(*good, *common, "--require-baseline")
            self.assertEqual(code, 1)

            # 和自己的基线比：没有退步。
            code, text = self.run_main(*good, *common, "--baseline", str(baseline))
            self.assertEqual(code, 0, text)
            page_html = (root / "report.html").read_text(encoding="utf-8")
            self.assertIn('class="banner ok"', page_html)
            self.assertIn("每一页的判定都和基线一样", page_html)

            # 一页变坏：31 页有对象的页里掉 1 页，超过 0.5 个百分点，报警。
            self.write_run(root / "bad", self.sample_files(broken=True))
            bad = ("--roundtrip", str(root / "bad" / "roundtrip.json"), "--ops", str(root / "bad" / "ops.json"), "--diff-dir", str(root / "bad" / "diff"))
            code, text = self.run_main(*bad, *common, "--baseline", str(baseline))
            self.assertEqual(code, 2, text)
            page_html = (root / "report.html").read_text(encoding="utf-8")
            self.assertIn('class="banner alarm"', page_html)
            self.assertLess(page_html.index('class="banner alarm"'), page_html.index('id="summary"'), "退步要写在页面最上面")
            self.assertIn("新变坏", page_html)
            self.assertIn("bad.pdf", page_html)
            summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual((summary["level"], summary["exit_code"]), ("alarm", 2))
            newly = summary["sets"][0]["baseline"]["newly_failed"]
            self.assertEqual([(c["path"], c["page"], c["before"], c["after"]) for c in newly], [("bad.pdf", 1, "pass", "diff")])
            source = summary["by_source"]["office-word"]
            self.assertEqual((source["compared"], source["passed"], source["crashed"]), (32, 30, 1))
            self.assertAlmostEqual(source["baseline"]["delta_pp"], (30 / 32 - 31 / 32) * 100)
            markdown = (root / "summary.md").read_text(encoding="utf-8")
            self.assertIn("| office-word | 33 | 32 | 32 | 30 |", markdown)
            self.assertIn("bad.pdf", markdown)

            # 只在到报警线时才失败、或者完全不失败的两种设置。
            self.assertEqual(self.run_main(*bad, *common, "--baseline", str(baseline), "--fail-on", "alarm")[0], 2)
            self.assertEqual(self.run_main(*bad, *common, "--baseline", str(baseline), "--fail-on", "never")[0], 0)
            self.assertEqual(self.run_main(*bad, *common, "--baseline", str(baseline), "--max-drop-pp", "5")[0], 3)
            self.assertEqual(self.run_main(*bad, *common, "--baseline", str(baseline), "--max-drop-pp", "5", "--fail-on", "alarm")[0], 0)

    def test_page_is_self_contained_and_leaks_no_document_text(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.write_run(root, self.sample_files(broken=True))
            code, text = self.run_main(
                "--roundtrip", str(root / "roundtrip.json"), "--ops", str(root / "ops.json"), "--diff-dir", str(root / "diff"), "--out", str(root / "report.html")
            )
            self.assertEqual(code, 0, text)
            page_html = (root / "report.html").read_text(encoding="utf-8")
            lowered = page_html.lower()
            for needle in ("http://", "https://", "<script", "<link", "@import", "url("):
                self.assertNotIn(needle, lowered, f"页面里不该有 {needle}")
            self.assertNotIn("SAMPLE CO LTD", page_html, "子进程标准错误里的东西不上页面")
            self.assertNotIn("ACME Red", page_html, "专色名不上页面")
            self.assertIn("fill Separation→DeviceRGB", page_html)
            self.assertIn("0x80000003", page_html)

    def test_thumbnails_are_embedded_and_originals_linked(self) -> None:
        try:
            import PIL  # noqa: F401, PLC0415
        except ImportError:
            self.skipTest("没有装 Pillow")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.write_run(root / "run", self.sample_files(broken=True), with_images=True)
            args = ("--roundtrip", str(root / "run" / "roundtrip.json"), "--diff-dir", str(root / "run" / "diff"), "--out", str(root / "report.html"))
            code, text = self.run_main(*args)
            self.assertEqual(code, 0, text)
            page_html = (root / "report.html").read_text(encoding="utf-8")
            self.assertEqual(page_html.count("data:image/jpeg;base64,"), 3, "一页三张缩略图")
            self.assertIn('src="run/diff/s__bad__p001_a.png"', page_html, "原图用相对路径")
            self.assertIn('href="run/diff/s__bad__p001_diff.png"', page_html)

            code, _ = self.run_main(*args, "--max-thumb-pages", "0")
            self.assertEqual(code, 0)
            page_html = (root / "report.html").read_text(encoding="utf-8")
            self.assertNotIn("data:image", page_html)
            self.assertIn('href="run/diff/s__bad__p001_a.png"', page_html, "不内嵌缩略图时还有链接")

    def test_run_manifest_with_two_sets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.write_run(root / "synthetic", [file("a.pdf", "synthetic", [page(0), diff_page(1)])])
            self.write_run(root / "public", [file("x.pdf", "resources", [page(0)]), file("pixel/y.pdf", "pixel", [page(0)])])
            calibrate = roundtrip([file("x.pdf", "resources", [page(0)]), file("pixel/y.pdf", "pixel", [diff_page(0)])])
            (root / "public" / "calibrate.json").write_text(json.dumps(calibrate), encoding="utf-8")
            base_dir = root / "baselines"
            base_dir.mkdir()
            base = snapshot([file("a.pdf", "synthetic", [page(0), diff_page(1)])], name="synthetic")
            (base_dir / "synthetic.json").write_text(model.dumps_baseline(base, {"has_ops": False}), encoding="utf-8")
            run = {
                "kind": report.RUN_KIND,
                "format": 1,
                "started": "2026-11-12T02:00:00+08:00",
                "git": {"commit": "ff6b0ac0000", "branch": "main", "dirty": False},
                "machine": "runner-1",
                "pdfium_dll_sha256": "abc",
                "sets": [
                    {"name": "synthetic", "title": "自制样本", "source_prefix": "", "roundtrip": "synthetic/roundtrip.json", "ops": "synthetic/ops.json",
                     "baseline": str(base_dir / "synthetic.json"), "seconds": {"roundtrip": 0.9}},
                    {"name": "public-pdfium", "title": "PDFium 公开测试集", "source_prefix": "public/pdfium/", "roundtrip": "public/roundtrip.json",
                     "ops": "public/ops.json", "calibrate": "public/calibrate.json", "baseline": str(base_dir / "public-pdfium.json")},
                ],
            }
            (root / "run.json").write_text(json.dumps(run, ensure_ascii=False), encoding="utf-8")
            common = ("--run", str(root / "run.json"), "--out", str(root / "report.html"), "--summary", str(root / "summary.json"), "--baseline-out", str(root / "candidate"))

            code, text = self.run_main(*common)
            self.assertEqual(code, 3, text)  # 校准轮有一页有差异
            self.assertIn("校准轮没有全部通过", text)
            summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(sorted(summary["by_source"]), ["public/pdfium/pixel", "public/pdfium/resources", "synthetic"])
            self.assertEqual(summary["sets"][1]["calibration"], {"compared": 2, "failed": 1, "crashed": 0})
            self.assertEqual(summary["sets"][1]["baseline_problem"], "基线文件不存在")
            self.assertEqual(summary["sets"][0]["baseline"]["level"], "ok", "基线里没有操作符级结果、这次有：只提示")
            page_html = (root / "report.html").read_text(encoding="utf-8")
            self.assertIn("PDFium 公开测试集 小计", page_html)
            self.assertIn("合计（仅供参考）", page_html)
            self.assertIn('class="banner worse"', page_html)

            candidate = model.loads_baseline((root / "candidate" / "public-pdfium.json").read_text(encoding="utf-8"))
            self.assertEqual(candidate.meta["commit"], "ff6b0ac0000")
            self.assertEqual(candidate.meta["pdfium_dll_sha256"], "abc")
            self.assertEqual(candidate.meta["machine"], "runner-1")
            self.assertEqual(candidate.meta["source_prefix"], "public/pdfium/")
            self.assertEqual({f.source for f in candidate.files}, {"public/pdfium/resources", "public/pdfium/pixel"})

            code, _ = self.run_main(*common, "--require-baseline")
            self.assertEqual(code, 1, "有一个语料集没有基线")


if __name__ == "__main__":
    unittest.main()
