"""往返测试每晚报告的数据部分（V14）：读结果、按来源汇总、存取基线、和基线比较。

输入是 `magpdf-cli roundtrip --json` 的结果，可以再配上 `ops_diff.py --json` 的结果。页面在
roundtrip_report.py 里生成；这里不碰图片，也不写页面，方便单独测试。只用标准库。

统计口径（《P0 技术验证计划》V02、《测试计划》第 7 节）：

- 比对页 = 像素判定是“无差异”或“有差异”的页。原件就加载不了的页（页错误）、崩溃或超时的文件里的页不算比对页。
- 无对象页 = 判定为无差异、并且页上一个对象也没有的页（没有内容流可重写，无差异是必然的）。
  和 `magpdf-cli roundtrip` 汇总里的 `empty_passed` 是同一个数。
- 比例（全部比对页）= 无差异 ÷ 比对页。
- 比例（扣除无对象页）=（无差异 − 无对象页）÷（比对页 − 无对象页）。退步判定用这一个。
- 操作符级：只看既有像素判定、又做成了操作符级比对的页。“两项都无差异”= 像素无差异并且五类操作符差异都是 0。
  它的比例同样扣除无对象页。操作符级没做成的页（qpdf 打不开、页数对不上、没有保存结果）单独数，不算进分子分母。

和基线比较（《测试计划》第 7 节“退步判定”）：

- 报警：任一来源的比例（扣除无对象页）比基线低 0.5 个百分点以上（像素一项，两项都无差异一项，各算各的）；
  或者出现基线里没有的崩溃、超时。
- 和基线比的比例，两边都只拿“基线里有、这次也有”的文件算。语料加了文件（真实样本是陆续收进来的）不该让比例跟着变、
  平白报警；少了文件也不该让比例变好看。新文件、少了的文件单独列出来。
- 没到报警线但要处理：有新变坏的页（基线里无差异，这次不是）；基线里有的文件这次没跑到；
  有文件从正常变成打不开或保存失败；基线里有操作符级结果、这次没有。
- 新变好的页、基线里没有的新文件只提示，不算退步。
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Iterable

BASELINE_KIND = "magpdf-roundtrip-baseline"
BASELINE_FORMAT = 1

#: 操作符级差异的五类，和 ops_diff.py 每页的 counts 一致。
OPS_KEYS = ("color", "text", "gs", "clip", "events")
OPS_LABELS = {
    "color": "颜色",
    "text": "文字状态",
    "gs": "ExtGState",
    "clip": "剪裁",
    "events": "事件缺失/多出",
}

FILE_STATUS_LABELS = {
    "ok": "正常",
    "load_failed": "打开失败",
    "save_failed": "保存失败",
    "reopen_failed": "保存结果打不开",
    "crashed": "崩溃",
    "timed_out": "超时",
}

#: 页的像素判定。lost 只在比较时出现：这一边没有这一页（文件崩溃、超时、打不开，或者页数变了）。
PIXEL_LABELS = {"pass": "无差异", "diff": "有差异", "error": "页错误", "lost": "没有比对到"}

#: 差异页的归因线索，和 magpdf-engine 的 Hint::label 一致。
HINT_LABELS = {
    "shading_lost": "渐变丢失",
    "image_lost": "图像丢失",
    "type3": "Type3",
    "objects_changed": "对象数变化",
    "text_changed": "文字变化",
    "text_moved": "字距/文字位置",
    "color_changed": "颜色",
    "transparency": "透明/ExtGState",
    "text_clip": "文字裁剪",
    "unclassified": "待人工归类",
}

OPS_SKIP_LABELS = {
    "no saved file": "没有保存结果",
    "open: PdfError": "qpdf 打不开",
}

#: 《测试计划》第 7 节：任一来源的无差异比例比基线低 0.5 个百分点以上即报警。
DEFAULT_MAX_DROP_PP = 0.5

CRASH_STATUSES = ("crashed", "timed_out")


@dataclass
class Page:
    """一页的结果。前六项基线里也存；其余只有这一次的结果里有。"""

    number: int  # 从 1 开始
    pixel: str  # pass / diff / error
    objects: int = 0
    diff_pixels: int = 0
    largest_region: int = 0
    #: 操作符级五类差异里不为 0 的几类。None = 没做成操作符级比对；{} = 做了，没有差异。
    ops: dict[str, int] | None = None

    total_pixels: int = 0
    hints: list[str] = field(default_factory=list)
    error: str = ""
    #: 三张图在 --diff-dir 里的文件名：a（原件）、b（保存结果）、heat（差异热图）。
    images: dict[str, str] | None = None
    #: 宽高、包围盒、往返前后的对象统计、文字位置、颜色变化的对象数，页面的明细里用。
    detail: dict[str, Any] = field(default_factory=dict)
    #: ops_diff.py 每页的明细（color、text、gs、clip、missing、extra、folded 各一个计数表）。
    ops_detail: dict[str, dict[str, int]] = field(default_factory=dict)
    ops_error: str = ""

    @property
    def compared(self) -> bool:
        return self.pixel in ("pass", "diff")

    @property
    def empty(self) -> bool:
        """无对象页：无差异，并且页上没有对象。"""
        return self.pixel == "pass" and self.objects == 0

    @property
    def ops_clean(self) -> bool | None:
        if self.ops is None:
            return None
        return not any(self.ops.values())

    @property
    def strict_pass(self) -> bool:
        """像素无差异，操作符级也做了并且没有差异。"""
        return self.pixel == "pass" and self.ops_clean is True


@dataclass
class FileResult:
    path: str  # 相对语料目录的路径，分隔符是 /
    source: str  # 来源，已经带上语料集的前缀
    status: str  # ok / load_failed / save_failed / reopen_failed / crashed / timed_out
    pages: list[Page] = field(default_factory=list)
    error: str = ""
    exit_code: int | None = None
    #: 这个文件整个没做成操作符级比对的原因。
    ops_note: str = ""


@dataclass
class Snapshot:
    """一个语料集一次运行的结果，或者一份基线。"""

    name: str
    files: list[FileResult] = field(default_factory=list)
    meta: dict[str, Any] = field(default_factory=dict)
    has_ops: bool = False

    def page_count(self) -> int:
        return sum(len(f.pages) for f in self.files)


def clean_error(text: Any) -> str:
    """错误信息只留工具自己写的那一段。

    隔离模式下崩溃的文件，错误信息后面跟着子进程标准错误的末尾（“；stderr：…”），那一段可能带出文档里的东西，
    页面上不显示；要看去原始的 JSON 里看。
    """
    if not text:
        return ""
    head = str(text).split("；stderr：", 1)[0].strip()
    return head if len(head) <= 200 else head[:200] + "…"


def _page_from_record(record: dict[str, Any]) -> Page:
    diff = record.get("diff") or {}
    stats = record.get("stats") or {}
    detail: dict[str, Any] = {
        "width": record.get("width"),
        "height": record.get("height"),
        "bbox": diff.get("bbox"),
        "stats": stats,
        "stats_after": record.get("stats_after"),
        "text_shift": record.get("text_shift"),
        "color_changed": record.get("color_changed"),
    }
    images = record.get("diff_images")
    return Page(
        number=int(record.get("index", 0)) + 1,
        pixel=str(record.get("status") or "error"),
        objects=int(stats.get("objects") or 0),
        diff_pixels=int(diff.get("diff_pixels") or 0),
        largest_region=int(diff.get("largest_region") or 0),
        total_pixels=int(diff.get("total_pixels") or 0),
        hints=[str(h) for h in record.get("hints") or []],
        error=clean_error(record.get("error")),
        images={k: str(images[k]) for k in ("a", "b", "heat") if images.get(k)} if isinstance(images, dict) else None,
        detail=detail,
    )


def _attach_ops(result: FileResult, record: dict[str, Any] | None) -> None:
    """把 ops_diff.py 的一个文件记录挂到各页上。

    ops_diff.py 用 qpdf 读文件，页是按序号和 PDFium 对应的。两边数出来的页数不一样时（损坏的页树，各有各的修法）
    序号对不上，这个文件整个不用。
    """
    if record is None:
        result.ops_note = "操作符级结果里没有这个文件"
        return
    if "pages" not in record:
        reason = str(record.get("skipped") or record.get("error") or "没有结果")
        result.ops_note = OPS_SKIP_LABELS.get(reason, reason)
        return
    entries = [p for p in record["pages"] if int(p.get("page") or 0) >= 1]
    if len(entries) != len(record["pages"]) or len(entries) != len(result.pages):
        result.ops_note = f"页数对不上（qpdf {len(entries)} 页，PDFium {len(result.pages)} 页）"
        return
    by_number = {int(p["page"]): p for p in entries}
    for page in result.pages:
        entry = by_number.get(page.number)
        if entry is None:
            continue
        if "error" in entry:
            page.ops_error = "qpdf 读这一页出错"
            continue
        counts = entry.get("counts") or {}
        page.ops = {k: int(counts.get(k) or 0) for k in OPS_KEYS if int(counts.get(k) or 0)}
        page.ops_detail = {
            k: {str(name): int(n) for name, n in (entry.get(k) or {}).items()}
            for k in ("color", "text", "gs", "clip", "missing", "extra", "folded")
            if entry.get(k)
        }


def load_run(
    roundtrip: dict[str, Any],
    ops: dict[str, Any] | None = None,
    *,
    name: str,
    source_prefix: str = "",
    root_source: str = "",
) -> Snapshot:
    """把一次 `roundtrip --json`（和可选的 `ops_diff.py --json`）读成 Snapshot。

    来源 = `source_prefix` + 命令行工具给的来源（输入目录下的第一级子目录名，直接放在输入目录下的文件用输入目录的名字）。
    给了 `root_source` 时，直接放在输入目录下的文件改用它：来源名就不跟着输入目录叫什么变了
    （PDFium 的 `testing/resources` 拷到别的名字的目录里，来源还是 `resources`）。
    """
    ops_by_path: dict[str, dict[str, Any]] = {}
    if ops is not None:
        for record in ops.get("files") or []:
            ops_by_path[str(record.get("path"))] = record
    files: list[FileResult] = []
    for entry in roundtrip.get("files") or []:
        source = str(entry.get("source") or "")
        if root_source and "/" not in str(entry["path"]):
            source = root_source
        result = FileResult(
            path=str(entry["path"]),
            source=source_prefix + source,
            status=str(entry.get("status") or "ok"),
            pages=[_page_from_record(p) for p in entry.get("pages") or []],
            error=clean_error(entry.get("error")),
            exit_code=entry.get("exit_code"),
        )
        if ops is not None:
            _attach_ops(result, ops_by_path.get(result.path))
        files.append(result)
    files.sort(key=lambda f: (f.source, f.path))
    tool = roundtrip.get("tool") or {}
    options = roundtrip.get("options") or {}
    meta: dict[str, Any] = {
        "cli_version": tool.get("version"),
        "pdfium_version": tool.get("pdfium"),
        "thresholds": options.get("thresholds"),
        "scale": options.get("scale"),
        "calibrate": bool(options.get("calibrate")),
        "isolate": bool(options.get("isolate")),
        "timeout_s": options.get("timeout_s"),
        "jobs": options.get("jobs"),
        "started_unix_s": roundtrip.get("started_unix_s"),
        "elapsed_ms": roundtrip.get("elapsed_ms"),
    }
    return Snapshot(name=name, files=files, meta=meta, has_ops=ops is not None)


# ---------------------------------------------------------------------------
# 按来源汇总


def ratio(numerator: int, denominator: int) -> float | None:
    return numerator / denominator if denominator > 0 else None


@dataclass
class Summary:
    """一个来源（或合计）的汇总。"""

    files: int = 0
    load_failed: int = 0
    save_failed: int = 0
    reopen_failed: int = 0
    crashed: int = 0
    timed_out: int = 0
    pages: int = 0  # 有记录的页：比对页 + 页错误
    passed: int = 0
    diff: int = 0
    errored: int = 0
    empty_passed: int = 0
    #: 差异页按第一条线索计数（只有这一次的结果有线索）。
    hints: dict[str, int] = field(default_factory=dict)

    ops_pages: int = 0  # 比对页里做成了操作符级比对的
    ops_missing: int = 0  # 比对页里没做成的
    ops_diff_pages: int = 0
    pixel_pass_ops_diff: int = 0
    strict_pass: int = 0
    strict_pass_empty: int = 0
    ops_counts: dict[str, int] = field(default_factory=dict)  # 各类差异的处数
    ops_count_pages: dict[str, int] = field(default_factory=dict)  # 各类差异涉及的页数

    @property
    def compared(self) -> int:
        return self.passed + self.diff

    @property
    def with_objects(self) -> int:
        """扣除无对象页之后的比对页。"""
        return self.compared - self.empty_passed

    @property
    def passed_with_objects(self) -> int:
        return self.passed - self.empty_passed

    @property
    def ratio_all(self) -> float | None:
        return ratio(self.passed, self.compared)

    @property
    def ratio_nonempty(self) -> float | None:
        return ratio(self.passed_with_objects, self.with_objects)

    @property
    def strict_with_objects(self) -> int:
        return self.ops_pages - self.strict_pass_empty

    @property
    def strict_passed_with_objects(self) -> int:
        return self.strict_pass - self.strict_pass_empty

    @property
    def strict_ratio_nonempty(self) -> float | None:
        return ratio(self.strict_passed_with_objects, self.strict_with_objects)

    def add_file(self, file: FileResult) -> None:
        self.files += 1
        if file.status in ("load_failed", "save_failed", "reopen_failed", "crashed", "timed_out"):
            setattr(self, file.status, getattr(self, file.status) + 1)
        for page in file.pages:
            self.pages += 1
            if page.pixel == "pass":
                self.passed += 1
                if page.objects == 0:
                    self.empty_passed += 1
            elif page.pixel == "diff":
                self.diff += 1
                primary = page.hints[0] if page.hints else ""
                if primary:
                    self.hints[primary] = self.hints.get(primary, 0) + 1
            else:
                self.errored += 1
            if not page.compared:
                continue
            if page.ops is None:
                self.ops_missing += 1
                continue
            self.ops_pages += 1
            if any(page.ops.values()):
                self.ops_diff_pages += 1
                if page.pixel == "pass":
                    self.pixel_pass_ops_diff += 1
                for key, count in page.ops.items():
                    if count:
                        self.ops_counts[key] = self.ops_counts.get(key, 0) + count
                        self.ops_count_pages[key] = self.ops_count_pages.get(key, 0) + 1
            elif page.pixel == "pass":
                self.strict_pass += 1
                if page.objects == 0:
                    self.strict_pass_empty += 1

    def add(self, other: "Summary") -> None:
        for name in (
            "files",
            "load_failed",
            "save_failed",
            "reopen_failed",
            "crashed",
            "timed_out",
            "pages",
            "passed",
            "diff",
            "errored",
            "empty_passed",
            "ops_pages",
            "ops_missing",
            "ops_diff_pages",
            "pixel_pass_ops_diff",
            "strict_pass",
            "strict_pass_empty",
        ):
            setattr(self, name, getattr(self, name) + getattr(other, name))
        for mine, theirs in (
            (self.hints, other.hints),
            (self.ops_counts, other.ops_counts),
            (self.ops_count_pages, other.ops_count_pages),
        ):
            for key, count in theirs.items():
                mine[key] = mine.get(key, 0) + count

    def to_json(self) -> dict[str, Any]:
        return {
            "files": self.files,
            "load_failed": self.load_failed,
            "save_failed": self.save_failed,
            "reopen_failed": self.reopen_failed,
            "crashed": self.crashed,
            "timed_out": self.timed_out,
            "pages": self.pages,
            "compared": self.compared,
            "passed": self.passed,
            "diff": self.diff,
            "errored": self.errored,
            "empty_passed": self.empty_passed,
            "ratio_all": self.ratio_all,
            "with_objects": self.with_objects,
            "passed_with_objects": self.passed_with_objects,
            "ratio_nonempty": self.ratio_nonempty,
            "hints": dict(sorted(self.hints.items())),
            "ops_pages": self.ops_pages,
            "ops_missing": self.ops_missing,
            "ops_diff_pages": self.ops_diff_pages,
            "pixel_pass_ops_diff": self.pixel_pass_ops_diff,
            "strict_pass": self.strict_pass,
            "strict_with_objects": self.strict_with_objects,
            "strict_passed_with_objects": self.strict_passed_with_objects,
            "strict_ratio_nonempty": self.strict_ratio_nonempty,
            "ops_counts": {k: self.ops_counts.get(k, 0) for k in OPS_KEYS},
            "ops_count_pages": {k: self.ops_count_pages.get(k, 0) for k in OPS_KEYS},
        }


def summarize(files: Iterable[FileResult]) -> dict[str, Summary]:
    """按来源汇总，键按来源名排序。"""
    by_source: dict[str, Summary] = {}
    for file in files:
        by_source.setdefault(file.source, Summary()).add_file(file)
    return dict(sorted(by_source.items()))


def total_of(summaries: Iterable[Summary]) -> Summary:
    """合计。只作参考：方案 13.1 要求按来源分别报告。"""
    total = Summary()
    for summary in summaries:
        total.add(summary)
    return total


# ---------------------------------------------------------------------------
# 基线


def baseline_page(page: Page) -> dict[str, Any]:
    out: dict[str, Any] = {"page": page.number, "pixel": page.pixel, "objects": page.objects}
    if page.diff_pixels:
        out["diff_pixels"] = page.diff_pixels
    if page.largest_region:
        out["largest_region"] = page.largest_region
    if page.ops is not None:
        out["ops"] = {k: page.ops[k] for k in OPS_KEYS if page.ops.get(k)}
    return out


def dumps_baseline(snapshot: Snapshot, meta: dict[str, Any]) -> str:
    """把一次结果写成基线文本。

    只存每页的判定和几个数，不存图、不存耗时。一页占一行，文件按来源和路径排序：基线更新时用 `git diff`
    能直接看出哪几页的判定变了。
    """

    def dump(value: Any) -> str:
        return json.dumps(value, ensure_ascii=False)

    lines = ["{", f' "kind": {dump(BASELINE_KIND)},', f' "format": {BASELINE_FORMAT},', f' "set": {dump(snapshot.name)},']
    meta_text = json.dumps(meta, ensure_ascii=False, indent=1, sort_keys=True)
    lines.append(' "meta": ' + meta_text.replace("\n", "\n ") + ",")
    lines.append(' "files": [')
    files = sorted(snapshot.files, key=lambda f: (f.source, f.path))
    for i, file in enumerate(files):
        comma = "," if i + 1 < len(files) else ""
        head = f'  {{"path": {dump(file.path)}, "source": {dump(file.source)}, "status": {dump(file.status)}, "pages": ['
        if not file.pages:
            lines.append(head + "]}" + comma)
            continue
        lines.append(head)
        for j, page in enumerate(file.pages):
            lines.append("   " + dump(baseline_page(page)) + ("," if j + 1 < len(file.pages) else ""))
        lines.append("  ]}" + comma)
    lines.append(" ]")
    lines.append("}")
    return "\n".join(lines) + "\n"


class BaselineError(ValueError):
    """基线文件不是这个工具写的，或者版本不认识。"""


def loads_baseline(text: str) -> Snapshot:
    data = json.loads(text)
    if not isinstance(data, dict) or data.get("kind") != BASELINE_KIND:
        raise BaselineError("不是往返测试的基线文件（kind 不对）")
    if data.get("format") != BASELINE_FORMAT:
        raise BaselineError(f"基线格式版本 {data.get('format')!r} 不认识，这个工具只认 {BASELINE_FORMAT}")
    files: list[FileResult] = []
    for entry in data.get("files") or []:
        pages = [
            Page(
                number=int(p["page"]),
                pixel=str(p["pixel"]),
                objects=int(p.get("objects") or 0),
                diff_pixels=int(p.get("diff_pixels") or 0),
                largest_region=int(p.get("largest_region") or 0),
                ops={str(k): int(v) for k, v in p["ops"].items()} if isinstance(p.get("ops"), dict) else None,
            )
            for p in entry.get("pages") or []
        ]
        files.append(
            FileResult(path=str(entry["path"]), source=str(entry["source"]), status=str(entry["status"]), pages=pages)
        )
    meta = data.get("meta") if isinstance(data.get("meta"), dict) else {}
    return Snapshot(name=str(data.get("set") or ""), files=files, meta=meta, has_ops=bool(meta.get("has_ops")))


# ---------------------------------------------------------------------------
# 和基线比较


@dataclass
class PageChange:
    source: str
    path: str
    number: int
    before: str  # pass / diff / error / lost
    after: str
    before_page: Page | None = None
    after_page: Page | None = None

    def to_json(self) -> dict[str, Any]:
        return {"source": self.source, "path": self.path, "page": self.number, "before": self.before, "after": self.after}


@dataclass
class FileChange:
    source: str
    path: str
    before: str
    after: str

    def to_json(self) -> dict[str, Any]:
        return {"source": self.source, "path": self.path, "before": self.before, "after": self.after}


@dataclass
class SourceDelta:
    """一个来源和基线比的结果。

    `base`、`cur` 是基线和这次各自的汇总，都只算两边都有的文件；这个来源没有两边都有的文件时是 None。
    所以语料变了的时候，`cur` 的比例和报告第 1 节里这个来源的比例（算全部文件）可以不一样。
    """

    source: str
    base: Summary | None
    cur: Summary | None
    added_files: int = 0  # 这次有、基线里没有的文件，不算进 base、cur
    removed_files: int = 0  # 基线里有、这次没有的文件，同样不算
    #: 这次减基线，百分点；任一边没有比例时是 None。
    delta_pp: float | None = None
    strict_delta_pp: float | None = None
    newly_failed: int = 0
    newly_passed: int = 0
    ops_newly_failed: int = 0
    ops_newly_passed: int = 0
    alarm: bool = False

    @property
    def common_files(self) -> int:
        return self.cur.files if self.cur is not None else 0

    def to_json(self) -> dict[str, Any]:
        return {
            "scope": "common_files",
            "common_files": self.common_files,
            "added_files": self.added_files,
            "removed_files": self.removed_files,
            "baseline_ratio_nonempty": self.base.ratio_nonempty if self.base else None,
            "ratio_nonempty": self.cur.ratio_nonempty if self.cur else None,
            "delta_pp": self.delta_pp,
            "baseline_strict_ratio_nonempty": self.base.strict_ratio_nonempty if self.base else None,
            "strict_ratio_nonempty": self.cur.strict_ratio_nonempty if self.cur else None,
            "strict_delta_pp": self.strict_delta_pp,
            "newly_failed": self.newly_failed,
            "newly_passed": self.newly_passed,
            "ops_newly_failed": self.ops_newly_failed,
            "ops_newly_passed": self.ops_newly_passed,
            "alarm": self.alarm,
        }


@dataclass
class Comparison:
    """一个语料集和它的基线比出来的结果。"""

    set_name: str
    max_drop_pp: float = DEFAULT_MAX_DROP_PP
    newly_failed: list[PageChange] = field(default_factory=list)  # 像素：基线里无差异，这次不是
    newly_passed: list[PageChange] = field(default_factory=list)  # 像素：基线里不是无差异，这次是
    other_changes: list[PageChange] = field(default_factory=list)  # 有差异、页错误、没比对到之间的变化
    ops_newly_failed: list[PageChange] = field(default_factory=list)  # 像素两边都无差异，操作符级从没有差异变成有
    ops_newly_passed: list[PageChange] = field(default_factory=list)
    ops_lost: int = 0  # 基线里有操作符级结果、这次没有的页数
    file_changes: list[FileChange] = field(default_factory=list)  # 两边都有的文件，状态变了
    added_files: list[FileResult] = field(default_factory=list)  # 这次有、基线里没有
    removed_files: list[FileResult] = field(default_factory=list)  # 基线里有、这次没有
    sources: dict[str, SourceDelta] = field(default_factory=dict)
    alarms: list[str] = field(default_factory=list)
    worse: list[str] = field(default_factory=list)
    notices: list[str] = field(default_factory=list)

    @property
    def new_crashes(self) -> list[FileChange]:
        return [c for c in self.file_changes if c.after in CRASH_STATUSES and c.before not in CRASH_STATUSES]

    @property
    def worsened_files(self) -> list[FileChange]:
        """从正常变成不正常，崩溃和超时除外（那两种另算）。"""
        return [c for c in self.file_changes if c.before == "ok" and c.after != "ok" and c.after not in CRASH_STATUSES]

    @property
    def level(self) -> str:
        if self.alarms:
            return "alarm"
        if self.worse:
            return "worse"
        return "ok"


def _pp(value: float) -> str:
    return f"{value * 100:.1f}%"


def _meta_notices(base: Snapshot, cur: Snapshot) -> list[str]:
    notices: list[str] = []
    b, c = base.meta, cur.meta
    if b.get("thresholds") and c.get("thresholds") and b.get("thresholds") != c.get("thresholds"):
        notices.append("判定阈值和基线不一样，两边的判定不能直接比。")
    if b.get("scale") and c.get("scale") and b.get("scale") != c.get("scale"):
        notices.append(f"渲染倍率和基线不一样（基线 {b.get('scale')}，这次 {c.get('scale')}）。")
    if b.get("pdfium_dll_sha256") and c.get("pdfium_dll_sha256") and b.get("pdfium_dll_sha256") != c.get("pdfium_dll_sha256"):
        notices.append("PDFium 库和基线用的不是同一份（pdfium.dll 的 SHA-256 不同）。")
    elif b.get("pdfium_version") and c.get("pdfium_version") and b.get("pdfium_version") != c.get("pdfium_version"):
        notices.append(f"PDFium 版本和基线不一样（基线 {b.get('pdfium_version')}，这次 {c.get('pdfium_version')}）。")
    b_corpus, c_corpus = b.get("corpus") or {}, c.get("corpus") or {}
    if b_corpus.get("sha256") and c_corpus.get("sha256") and b_corpus.get("sha256") != c_corpus.get("sha256"):
        notices.append(
            f"语料和基线不是同一份（基线 {b_corpus.get('files')} 个文件，这次 {c_corpus.get('files')} 个；内容指纹不同）。"
        )
    if b.get("machine") and c.get("machine") and b.get("machine") != c.get("machine"):
        notices.append(
            f"基线是在另一台机器上生成的（基线 {b.get('machine')}，这次 {c.get('machine')}）；没有嵌入的字体换成什么，各台机器不一样。"
        )
    return notices


def compare(base: Snapshot, cur: Snapshot, *, max_drop_pp: float = DEFAULT_MAX_DROP_PP) -> Comparison:
    """这次的结果和基线比。页按“文件路径 + 页码”对应。"""
    result = Comparison(set_name=cur.name, max_drop_pp=max_drop_pp)
    base_files = {f.path: f for f in base.files}
    cur_files = {f.path: f for f in cur.files}
    result.removed_files = [base_files[p] for p in sorted(base_files.keys() - cur_files.keys())]
    result.added_files = [cur_files[p] for p in sorted(cur_files.keys() - base_files.keys())]
    common = sorted(base_files.keys() & cur_files.keys())

    # 比例只拿两边都有的文件算。基线里的文件按这次的来源名归类：来源改了名（换了前缀、挪了目录）也照样比得上。
    base_summary: dict[str, Summary] = {}
    cur_summary: dict[str, Summary] = {}
    renamed = 0
    for path in common:
        source = cur_files[path].source
        base_summary.setdefault(source, Summary()).add_file(base_files[path])
        cur_summary.setdefault(source, Summary()).add_file(cur_files[path])
        if base_files[path].source != source:
            renamed += 1
    added_count: dict[str, int] = {}
    for file in result.added_files:
        added_count[file.source] = added_count.get(file.source, 0) + 1
    removed_count: dict[str, int] = {}
    for file in result.removed_files:
        removed_count[file.source] = removed_count.get(file.source, 0) + 1
    for source in sorted(cur_summary.keys() | added_count.keys() | removed_count.keys()):
        result.sources[source] = SourceDelta(
            source,
            base_summary.get(source),
            cur_summary.get(source),
            added_files=added_count.get(source, 0),
            removed_files=removed_count.get(source, 0),
        )
    corpus_changed = bool(result.added_files or result.removed_files)

    def delta_of(source: str) -> SourceDelta:
        return result.sources[source]

    compare_ops = base.has_ops and cur.has_ops
    for path in common:
        b_file, c_file = base_files[path], cur_files[path]
        source = c_file.source
        if b_file.status != c_file.status:
            result.file_changes.append(FileChange(source, path, b_file.status, c_file.status))
        b_pages = {p.number: p for p in b_file.pages}
        c_pages = {p.number: p for p in c_file.pages}
        for number in sorted(b_pages.keys() | c_pages.keys()):
            bp, cp = b_pages.get(number), c_pages.get(number)
            before = bp.pixel if bp else "lost"
            after = cp.pixel if cp else "lost"
            change = PageChange(source, path, number, before, after, bp, cp)
            if before != after:
                if before == "pass":
                    result.newly_failed.append(change)
                    delta_of(source).newly_failed += 1
                elif after == "pass":
                    result.newly_passed.append(change)
                    delta_of(source).newly_passed += 1
                else:
                    result.other_changes.append(change)
                continue
            if not compare_ops or bp is None or cp is None or not cp.compared:
                continue
            if bp.ops is not None and cp.ops is None:
                result.ops_lost += 1
                continue
            if before != "pass" or bp.ops_clean is None or cp.ops_clean is None:
                continue
            if bp.ops_clean and not cp.ops_clean:
                result.ops_newly_failed.append(change)
                delta_of(source).ops_newly_failed += 1
            elif not bp.ops_clean and cp.ops_clean:
                result.ops_newly_passed.append(change)
                delta_of(source).ops_newly_passed += 1

    limit = max_drop_pp / 100.0
    for source, delta in result.sources.items():
        if delta.base is None or delta.cur is None:
            continue
        scope = f"（只算两边都有的 {delta.common_files} 个文件）" if corpus_changed else ""
        rb, rc = delta.base.ratio_nonempty, delta.cur.ratio_nonempty
        if rb is not None and rc is not None:
            delta.delta_pp = (rc - rb) * 100.0
            if rb - rc > limit + 1e-12:
                delta.alarm = True
                result.alarms.append(
                    f"{source}：无差异比例（扣除无对象页）从 {_pp(rb)} 降到 {_pp(rc)}，"
                    f"低了 {(rb - rc) * 100:.2f} 个百分点，超过 {max_drop_pp:g} 个百分点的线{scope}"
                )
        if not compare_ops:
            continue
        sb, sc = delta.base.strict_ratio_nonempty, delta.cur.strict_ratio_nonempty
        if sb is not None and sc is not None:
            delta.strict_delta_pp = (sc - sb) * 100.0
            if sb - sc > limit + 1e-12:
                delta.alarm = True
                result.alarms.append(
                    f"{source}：像素和操作符都无差异的比例（扣除无对象页）从 {_pp(sb)} 降到 {_pp(sc)}，"
                    f"低了 {(sb - sc) * 100:.2f} 个百分点，超过 {max_drop_pp:g} 个百分点的线{scope}"
                )

    crashes = result.new_crashes
    if crashes:
        result.alarms.append(f"{len(crashes)} 个文件新出现崩溃或超时（基线里没有）")

    if result.newly_failed:
        result.worse.append(f"{len(result.newly_failed)} 页新变坏（基线里像素无差异，这次不是）")
    if result.ops_newly_failed:
        result.worse.append(f"{len(result.ops_newly_failed)} 页操作符级新变坏（像素仍无差异，操作符级从没有差异变成有）")
    if result.removed_files:
        result.worse.append(f"基线里的 {len(result.removed_files)} 个文件这次没有跑到（语料少了文件，或者基线该更新了）")
    worsened = result.worsened_files
    if worsened:
        result.worse.append(f"{len(worsened)} 个文件从正常变成打不开或保存失败")
    if base.has_ops and not cur.has_ops:
        result.worse.append("基线里有操作符级结果，这次没有（操作符级比对没做成）")
    elif result.ops_lost:
        result.worse.append(f"{result.ops_lost} 页基线里有操作符级结果，这次没有")

    result.notices.extend(_meta_notices(base, cur))
    if cur.has_ops and not base.has_ops:
        result.notices.append("基线里没有操作符级结果，这次的操作符级结果没有东西可比。")
    if result.newly_passed or result.ops_newly_passed:
        result.notices.append(
            f"有 {len(result.newly_passed)} 页新变好、{len(result.ops_newly_passed)} 页操作符级新变好；"
            "确认之后更新基线，这些页以后再变坏才报得出来。"
        )
    if result.added_files:
        result.notices.append(
            f"有 {len(result.added_files)} 个文件不在基线里：它们的结果照常列在下面各节，但没有东西可比，"
            "也不算进和基线比的比例；更新基线后才受保护。"
        )
    if renamed:
        result.notices.append(f"有 {renamed} 个文件的来源名和基线里不一样，比较时按这次的来源名归类。")
    return result
