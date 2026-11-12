#!/usr/bin/env python3
"""往返测试的报告页面（V14）：读 `magpdf-cli roundtrip --json` 的结果，生成一个静态 HTML 页面，并和基线比较。

用法一（每晚的入口脚本 scripts/nightly-roundtrip.ps1 用这种）：
    python tests/corpus/tools/roundtrip_report.py --run tests/out/nightly/run.json \
        --out tests/out/nightly/report.html --summary tests/out/nightly/summary.json \
        --markdown tests/out/nightly/summary.md --baseline-out tests/out/nightly/baseline-candidate

用法二（手里只有一份 roundtrip 的 JSON 时）：
    python tests/corpus/tools/roundtrip_report.py --roundtrip tests/out/x.json [--ops tests/out/x-ops.json] \
        [--diff-dir tests/out/x-diff] [--name NAME] [--source-prefix public/pdfium/] \
        [--baseline tests/baselines/roundtrip/NAME.json] --out tests/out/x-report.html

页面里有：按来源的汇总表（全部比对页、扣除无对象页两种口径）、和基线的比较（新变坏的页、新变好的页、各来源比例的变化）、
没通过的页（差异像素数、最大连通区域、三张图）、操作符级比对、崩溃超时和打不开的文件。口径和退步判定的规则见
roundtrip_model.py 开头。

页面是一个文件：样式写在页面里，没有脚本，不引用外网的东西；缩略图内嵌，点开看的原图是相对路径（--diff-dir 里的 PNG），
所以整个输出目录一起拿走才点得开原图。页面里只有文件的相对路径、页码和数字，不放文档里的文字；崩溃文件错误信息里
子进程标准错误的那一段、操作符级明细里的专色名和字体名都去掉了。

退出码：0 没有退步（或者没有基线）；2 到了报警线；3 有退步但没到报警线；1 工具自己出错。`--fail-on alarm` 时 3 改为 0，
`--fail-on never` 时 2、3 都改为 0。

缩略图用 Pillow（MIT-CMU，tests/requirements-test.txt 里已有）；没装时不内嵌缩略图，只给链接。
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import html
import io
import json
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable
from urllib.parse import quote

sys.path.insert(0, str(Path(__file__).resolve().parent))

from corpus_fingerprint import corpus_fingerprint  # noqa: E402
from roundtrip_model import (  # noqa: E402
    DEFAULT_MAX_DROP_PP,
    FILE_STATUS_LABELS,
    HINT_LABELS,
    OPS_KEYS,
    OPS_LABELS,
    PIXEL_LABELS,
    BaselineError,
    Comparison,
    FileResult,
    Page,
    PageChange,
    Snapshot,
    SourceDelta,
    Summary,
    compare,
    dumps_baseline,
    load_run,
    loads_baseline,
    summarize,
    total_of,
)

SUMMARY_KIND = "magpdf-roundtrip-report-summary"
RUN_KIND = "magpdf-roundtrip-nightly-run"

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_ALARM = 2
EXIT_WORSE = 3

LEVEL_TITLES = {
    "alarm": "有退步，到了报警线",
    "worse": "有退步，没到报警线",
    "ok": "和基线相比没有退步",
    "none": "没有基线，没有做退步判定",
}


# ---------------------------------------------------------------------------
# 输入


@dataclass
class SetInput:
    """一个语料集这一次的输入文件。"""

    name: str
    title: str
    roundtrip: Path
    source_prefix: str = ""
    ops: Path | None = None
    ops_error: str = ""
    diff_dir: Path | None = None
    calibrate: Path | None = None
    baseline: Path | None = None
    corpus_dir: Path | None = None
    seconds: dict[str, float] = field(default_factory=dict)


@dataclass
class Calibration:
    """校准轮（`roundtrip --calibrate`：不重写内容流，只保存再重开）的结果，应当全部无差异。"""

    compared: int = 0
    failed: list[tuple[str, int]] = field(default_factory=list)  # (文件, 页码)
    crashed: int = 0


@dataclass
class SetReport:
    spec: SetInput
    snapshot: Snapshot
    baseline: Snapshot | None = None
    baseline_problem: str = ""
    comparison: Comparison | None = None
    calibration: Calibration | None = None
    _baseline_pages: dict[tuple[str, int], Page] | None = field(default=None, repr=False)

    @property
    def baseline_pages(self) -> dict[tuple[str, int], Page]:
        """基线里的页，按（文件路径，页码）查。"""
        if self._baseline_pages is None:
            files = self.baseline.files if self.baseline is not None else []
            self._baseline_pages = {(f.path, p.number): p for f in files for p in f.pages}
        return self._baseline_pages

    @property
    def baseline_status(self) -> dict[str, str]:
        """基线里各文件的状态。"""
        return {f.path: f.status for f in self.baseline.files} if self.baseline is not None else {}


@dataclass
class Report:
    sets: list[SetReport]
    env: dict[str, Any] = field(default_factory=dict)
    generated: str = ""
    max_drop_pp: float = DEFAULT_MAX_DROP_PP
    alarms: list[str] = field(default_factory=list)
    worse: list[str] = field(default_factory=list)
    notices: list[str] = field(default_factory=list)

    @property
    def level(self) -> str:
        if self.alarms:
            return "alarm"
        if self.worse:
            return "worse"
        if any(s.comparison is not None for s in self.sets):
            return "ok"
        return "none"

    def by_source(self) -> dict[str, Summary]:
        return summarize(f for s in self.sets for f in s.snapshot.files)

    def source_delta(self, source: str) -> SourceDelta | None:
        for s in self.sets:
            if s.comparison is not None and source in s.comparison.sources:
                return s.comparison.sources[source]
        return None


def read_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8-sig") as f:
        return json.load(f)


def load_set(spec: SetInput) -> SetReport:
    ops = read_json(spec.ops) if spec.ops is not None and spec.ops.is_file() else None
    snapshot = load_run(read_json(spec.roundtrip), ops, name=spec.name, source_prefix=spec.source_prefix)
    if spec.corpus_dir is not None and spec.corpus_dir.is_dir():
        snapshot.meta["corpus"] = corpus_fingerprint(spec.corpus_dir)
    report = SetReport(spec=spec, snapshot=snapshot)
    if spec.calibrate is not None and spec.calibrate.is_file():
        cal = load_run(read_json(spec.calibrate), None, name=spec.name, source_prefix=spec.source_prefix)
        result = Calibration()
        for file in cal.files:
            if file.status in ("crashed", "timed_out"):
                result.crashed += 1
            for page in file.pages:
                if page.compared:
                    result.compared += 1
                    if page.pixel != "pass":
                        result.failed.append((file.path, page.number))
        report.calibration = result
    if spec.baseline is not None:
        if spec.baseline.is_file():
            try:
                report.baseline = loads_baseline(spec.baseline.read_text(encoding="utf-8-sig"))
            except (BaselineError, ValueError, KeyError, TypeError) as e:
                report.baseline_problem = f"基线读不了：{e}"
        else:
            report.baseline_problem = "基线文件不存在"
    return report


def build_report(specs: list[SetInput], env: dict[str, Any], max_drop_pp: float = DEFAULT_MAX_DROP_PP) -> Report:
    report = Report(sets=[], env=env, max_drop_pp=max_drop_pp)
    report.generated = dt.datetime.now().astimezone().isoformat(timespec="seconds")
    many = len(specs) > 1
    for spec in specs:
        current = load_set(spec)
        report.sets.append(current)
        for key in ("pdfium_dll_sha256", "machine"):
            if env.get(key):
                current.snapshot.meta[key] = env[key]
        prefix = f"{spec.title}：" if many else ""
        if current.baseline is not None:
            current.comparison = compare(current.baseline, current.snapshot, max_drop_pp=max_drop_pp)
            report.alarms += [prefix + text for text in current.comparison.alarms]
            report.worse += [prefix + text for text in current.comparison.worse]
            report.notices += [prefix + text for text in current.comparison.notices]
        else:
            report.notices.append(f"{prefix}{current.baseline_problem or '没有给基线'}，这一批没有做退步判定。")
        if spec.ops_error:
            report.notices.append(f"{prefix}操作符级比对没做成（{spec.ops_error}）。")
        cal = current.calibration
        if cal is not None and (cal.failed or cal.crashed):
            report.worse.append(
                f"{prefix}校准轮没有全部通过：{len(cal.failed)} 页有差异、{cal.crashed} 个文件崩溃或超时"
                "（不重写内容流也有差异，说明渲染或比对本身有噪声，这一次的判定不可靠）"
            )
    return report


def baseline_meta(current: SetReport, report: Report) -> dict[str, Any]:
    """写进基线的那几项：够判断“是不是同一种条件下跑的”，不放会每次都变的东西（耗时）。"""
    meta = current.snapshot.meta
    git = report.env.get("git") or {}
    return {
        "generated": report.generated,
        "commit": git.get("commit") or "",
        "cli_version": meta.get("cli_version"),
        "pdfium_version": meta.get("pdfium_version"),
        "pdfium_dll_sha256": meta.get("pdfium_dll_sha256") or "",
        "machine": meta.get("machine") or "",
        "thresholds": meta.get("thresholds"),
        "scale": meta.get("scale"),
        "corpus": meta.get("corpus") or {},
        "has_ops": current.snapshot.has_ops,
        "source_prefix": current.spec.source_prefix,
    }


# ---------------------------------------------------------------------------
# 小工具


def esc(value: Any) -> str:
    return html.escape(str(value), quote=True)


def num(value: int) -> str:
    return f"{value:,}"


def pct(value: float | None, digits: int = 1) -> str:
    return "—" if value is None else f"{value * 100:.{digits}f}%"


def small_pct(value: float | None) -> str:
    """差异像素占页面的比例：小的数多留几位（0.063%）。"""
    if value is None:
        return "—"
    p = value * 100
    if p >= 10:
        return f"{p:.1f}%"
    if p >= 1:
        return f"{p:.2f}%"
    return f"{p:.3f}%"


def signed_pp(value: float | None) -> str:
    if value is None:
        return "—"
    if abs(value) < 0.005:
        return "0"
    return f"{value:+.2f}".replace("-", "−")


def local_time(text: str) -> str:
    try:
        return dt.datetime.fromisoformat(text).strftime("%Y-%m-%d %H:%M")
    except (TypeError, ValueError):
        return str(text or "")


_PAREN = re.compile(r"\([^()]*\)")


def safe_ops_key(key: str) -> str:
    """操作符级明细的条目名里去掉文档带来的名字。

    `Separation(专色名,alt=…)`、`DeviceN(…)`、`Missing(资源名)`、`ICCBased(N=3,摘要)` 括号里的东西都去掉，
    只留颜色空间的种类；字体换了的条目整条换成“字体换了”。数值（Tc、Tw、ca 的前后值）留着。
    """
    if key.startswith("font "):
        return "字体换了"
    previous = None
    while previous != key:
        previous = key
        key = _PAREN.sub("", key)
    return key


def safe_ops_detail(detail: dict[str, dict[str, int]]) -> dict[str, dict[str, int]]:
    out: dict[str, dict[str, int]] = {}
    for group, counter in detail.items():
        merged: dict[str, int] = {}
        for key, count in counter.items():
            name = safe_ops_key(key)
            merged[name] = merged.get(name, 0) + count
        out[group] = dict(sorted(merged.items(), key=lambda item: (-item[1], item[0])))
    return out


def ops_brief(page: Page) -> str:
    """一页的操作符级结果，一句话。"""
    if page.ops is None:
        return "没做" + (f"（{page.ops_error}）" if page.ops_error else "")
    parts = [f"{OPS_LABELS[k]} {num(page.ops[k])}" for k in OPS_KEYS if page.ops.get(k)]
    return "、".join(parts) if parts else "没有差异"


def hint_text(hints: list[str]) -> str:
    return "、".join(HINT_LABELS.get(h, h) for h in hints) if hints else "—"


class Thumbnails:
    """把 --diff-dir 里的 PNG 缩成小图内嵌进页面。没装 Pillow、或者超过张数上限时返回 None。"""

    def __init__(self, width: int, max_pages: int) -> None:
        self.width = width
        self.budget = max_pages
        self.note = ""
        self._cache: dict[Path, str | None] = {}
        self._image: Any = None
        if max_pages > 0:
            try:
                from PIL import Image  # noqa: PLC0415

                self._image = Image
            except ImportError:
                self.note = "没有装 Pillow，缩略图没有内嵌，只有原图的链接。"

    def take_page(self) -> bool:
        """占用一页的名额（一页三张图）。"""
        if self._image is None or self.budget <= 0:
            return False
        self.budget -= 1
        return True

    def data_uri(self, path: Path) -> str | None:
        if path not in self._cache:
            self._cache[path] = self._encode(path)
        return self._cache[path]

    def _encode(self, path: Path) -> str | None:
        if self._image is None or not path.is_file():
            return None
        try:
            with self._image.open(path) as source:
                image = source.convert("RGB")
            image.thumbnail((self.width, self.width * 2))
            buffer = io.BytesIO()
            image.save(buffer, format="JPEG", quality=72, optimize=True)
        except (OSError, ValueError):
            return None
        return "data:image/jpeg;base64," + base64.b64encode(buffer.getvalue()).decode("ascii")


def href_of(target: Path, out_dir: Path) -> str:
    """从页面所在目录到目标文件的相对链接。不在同一个盘上时只能给绝对的 file 链接。

    两边都先 resolve：临时目录常带 8.3 短名（RUNNER~1），一边展开一边没展开，算出来的相对路径是错的。
    """
    target = target.resolve()
    try:
        rel = os.path.relpath(target, out_dir.resolve())
    except ValueError:
        return target.as_uri()
    return quote(Path(rel).as_posix())


# ---------------------------------------------------------------------------
# 页面

CSS = """
:root { --line:#d0d5dd; --soft:#f2f4f7; --text:#101828; --sub:#475467; --bad:#b42318; --badbg:#fee4e2;
  --warn:#b54708; --warnbg:#fef0c7; --good:#067647; --goodbg:#d1fadf; --info:#175cd3; --infobg:#d1e9ff; }
* { box-sizing: border-box; }
body { margin: 0; padding: 20px 28px 60px; color: var(--text); background: #fff;
  font: 14px/1.55 "Segoe UI", "Microsoft YaHei", "PingFang SC", sans-serif; }
h1 { font-size: 22px; margin: 0 0 4px; }
h2 { font-size: 18px; margin: 34px 0 10px; padding-top: 10px; border-top: 2px solid var(--line); }
h3 { font-size: 15px; margin: 20px 0 8px; }
p { margin: 6px 0; }
a { color: var(--info); }
code { font-family: Consolas, "Cascadia Mono", monospace; font-size: 13px; background: var(--soft); padding: 0 3px; border-radius: 3px; }
.sub { color: var(--sub); }
.banner { margin: 14px 0 6px; padding: 12px 16px; border-radius: 8px; border: 1px solid var(--line); }
.banner h2 { border: 0; margin: 0 0 4px; padding: 0; font-size: 19px; }
.banner ul { margin: 4px 0 0; padding-left: 20px; }
.banner.alarm { background: var(--badbg); border-color: var(--bad); }
.banner.alarm h2 { color: var(--bad); }
.banner.worse { background: var(--warnbg); border-color: var(--warn); }
.banner.worse h2 { color: var(--warn); }
.banner.ok { background: var(--goodbg); border-color: var(--good); }
.banner.ok h2 { color: var(--good); }
.banner.none { background: var(--soft); }
.notices { margin: 8px 0; padding: 8px 16px; border-left: 4px solid var(--info); background: #f5faff; }
.notices ul { margin: 2px 0; padding-left: 18px; }
nav { margin: 12px 0; }
nav a { margin-right: 14px; }
.scroll { overflow-x: auto; }
table { border-collapse: collapse; margin: 6px 0 10px; }
th, td { border: 1px solid var(--line); padding: 4px 8px; text-align: left; vertical-align: top; }
th { background: var(--soft); font-weight: 600; white-space: nowrap; }
td.n, th.n { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
tr.total td { background: #fafafa; font-weight: 600; }
tr.subtotal td { background: #fafafa; }
td.path { font-family: Consolas, "Cascadia Mono", monospace; font-size: 13px; word-break: break-all; min-width: 180px; }
.tag { display: inline-block; padding: 0 6px; border-radius: 10px; font-size: 12px; white-space: nowrap; }
.tag.bad { background: var(--badbg); color: var(--bad); }
.tag.warn { background: var(--warnbg); color: var(--warn); }
.tag.good { background: var(--goodbg); color: var(--good); }
.tag.info { background: var(--infobg); color: var(--info); }
.down { color: var(--bad); font-weight: 600; }
.up { color: var(--good); font-weight: 600; }
td.shots { white-space: nowrap; min-width: 250px; }
td.shots img { height: 96px; width: auto; max-width: 150px; border: 1px solid var(--line); background: #fff; vertical-align: top; margin-right: 3px; }
td.shots a.open { font-size: 12px; margin-right: 6px; }
details { margin: 8px 0; }
summary { cursor: pointer; font-weight: 600; }
dl.kv { display: grid; grid-template-columns: max-content 1fr; gap: 2px 14px; margin: 6px 0; }
dl.kv dt { color: var(--sub); }
dl.kv dd { margin: 0; }
.viewer { display: none; position: fixed; inset: 0; z-index: 20; overflow: auto; padding: 14px 22px 30px; background: #101828; color: #f2f4f7; }
.viewer:target { display: block; }
.viewer a { color: #84caff; }
.viewer h3 { margin: 0 0 6px; font-size: 16px; word-break: break-all; }
.viewer .close { float: right; font-size: 15px; padding: 2px 12px; border: 1px solid #84caff; border-radius: 6px; text-decoration: none; }
.viewer .figs { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 12px; margin-top: 10px; }
.viewer figure { margin: 0; }
.viewer figcaption { margin-bottom: 4px; }
.viewer img { width: 100%; height: auto; background: #fff; border: 1px solid #475467; }
.viewer dl.kv dt { color: #98a2b3; }
.foot { margin-top: 40px; color: var(--sub); font-size: 12px; }
"""


class HtmlBuilder:
    def __init__(self, report: Report, out_dir: Path, thumbs: Thumbnails) -> None:
        self.report = report
        self.out_dir = out_dir
        self.thumbs = thumbs
        self.viewers: list[str] = []
        self.viewer_ids: dict[tuple[str, str, int], str] = {}
        self.row_seq = 0

    # ---- 各种单元格 -----------------------------------------------------

    def shots_cell(self, current: SetReport, file: FileResult, page: Page, row_id: str) -> str:
        """三张图的单元格：内嵌缩略图，点开是整页大小的三张图和这一页的明细。"""
        if not page.images or current.spec.diff_dir is None:
            return '<td class="shots sub">没有图</td>'
        key = (current.spec.name, file.path, page.number)
        viewer_id = self.viewer_ids.get(key)
        if viewer_id is None:
            viewer_id = f"v{len(self.viewers) + 1}"
            self.viewer_ids[key] = viewer_id
            self.viewers.append(self.viewer(current, file, page, viewer_id, row_id))
        embed = self.thumbs.take_page()
        cells: list[str] = []
        for name, label in (("a", "原件"), ("b", "保存结果"), ("heat", "差异")):
            image = page.images.get(name)
            if not image:
                continue
            uri = self.thumbs.data_uri(current.spec.diff_dir / image) if embed else None
            if uri:
                cells.append(f'<a href="#{viewer_id}" title="{label}，点开看大图"><img alt="{label}" src="{uri}"></a>')
            else:
                cells.append(f'<a class="open" href="#{viewer_id}">{label}</a>')
        return f'<td class="shots">{"".join(cells)}</td>'

    def viewer(self, current: SetReport, file: FileResult, page: Page, viewer_id: str, row_id: str) -> str:
        assert page.images is not None and current.spec.diff_dir is not None
        figs: list[str] = []
        for name, label in (("a", "原件"), ("b", "保存结果（重新生成内容流之后）"), ("heat", "差异热图（红：超过阈值；黄：阈值以内）")):
            image = page.images.get(name)
            if not image:
                continue
            link = href_of(current.spec.diff_dir / image, self.out_dir)
            figs.append(
                f'<figure><figcaption>{label} · <a href="{esc(link)}" target="_blank" rel="noopener">单独打开</a></figcaption>'
                f'<img loading="lazy" alt="{label}（原图在输出目录的 diff 子目录里，和页面一起拿走才看得到）" src="{esc(link)}"></figure>'
            )
        return (
            f'<div class="viewer" id="{viewer_id}"><a class="close" href="#{row_id}">关闭</a>'
            f"<h3>{esc(file.source)} / {esc(file.path)} · 第 {page.number} 页</h3>"
            f"{self.page_detail(page)}"
            f'<div class="figs">{"".join(figs)}</div></div>'
        )

    def page_detail(self, page: Page) -> str:
        d = page.detail
        rows: list[tuple[str, str]] = []
        total = page.total_pixels
        rows.append(
            (
                "差异像素",
                f"{num(page.diff_pixels)}（占页面 {small_pct(page.diff_pixels / total if total else None)}），"
                f"最大连通区域 {num(page.largest_region)} 像素",
            )
        )
        if d.get("width") and d.get("height"):
            size = f"{d['width']:g} × {d['height']:g} pt"
            if d.get("bbox"):
                x0, y0, x1, y1 = d["bbox"]
                size += f"；差异包围盒（像素）({x0}, {y0})–({x1}, {y1})"
            rows.append(("页面", size))
        stats, after = d.get("stats") or {}, d.get("stats_after") or {}
        if stats:
            labels = (("objects", "对象"), ("text", "文字"), ("path", "路径"), ("image", "图像"), ("shading", "渐变"), ("form", "Form"))
            parts = []
            for key, label in labels:
                before = stats.get(key, 0)
                if after and after.get(key, before) != before:
                    parts.append(f"{label} {before} → {after.get(key)}")
                else:
                    parts.append(f"{label} {before}")
            rows.append(("对象（往返前 → 后）", "，".join(parts)))
        shift = d.get("text_shift")
        if shift:
            rows.append(
                (
                    "文字位置",
                    f"字符 {shift.get('chars_before')} → {shift.get('chars_after')}，挪了位置的 {shift.get('moved')} 个，"
                    f"最多挪了 {float(shift.get('max_shift_pt') or 0):.1f} pt",
                )
            )
        if d.get("color_changed") is not None:
            rows.append(("颜色变了的对象", str(d["color_changed"])))
        rows.append(("归因线索（工具推的，不是确诊）", hint_text(page.hints)))
        rows.append(("操作符级", ops_brief(page)))
        for group, counter in safe_ops_detail(page.ops_detail).items():
            if group == "folded" or not counter:
                continue
            label = {"missing": "缺失的绘制事件", "extra": "多出的绘制事件"}.get(group, OPS_LABELS.get(group, group))
            top = list(counter.items())[:6]
            text = "，".join(f"{name} ×{count}" for name, count in top)
            if len(counter) > len(top):
                text += f"，另有 {len(counter) - len(top)} 种"
            rows.append((f"　{label}", text))
        return '<dl class="kv">' + "".join(f"<dt>{esc(k)}</dt><dd>{esc(v)}</dd>" for k, v in rows) + "</dl>"

    def next_row(self) -> str:
        self.row_seq += 1
        return f"r{self.row_seq}"

    # ---- 顶部 -----------------------------------------------------------

    def header(self) -> str:
        env = self.report.env
        git = env.get("git") or {}
        first = self.report.sets[0].snapshot.meta if self.report.sets else {}
        bits = [f"生成于 {local_time(self.report.generated)}"]
        if git.get("commit"):
            branch = f"（{git.get('branch')}）" if git.get("branch") else ""
            dirty = "，工作区有没提交的改动" if git.get("dirty") else ""
            bits.append(f"提交 {str(git.get('commit'))[:10]}{branch}{dirty}")
        if first.get("cli_version"):
            bits.append(f"magpdf-cli {first.get('cli_version')}")
        if first.get("pdfium_version"):
            bits.append(f"PDFium {first.get('pdfium_version')}")
        if env.get("machine"):
            bits.append(f"机器 {env.get('machine')}")
        return f'<h1>MagPDF 往返测试报告</h1><p class="sub">{esc(" · ".join(bits))}</p>'

    def banner(self) -> str:
        report = self.report
        level = report.level
        items = [f"<li>{esc(text)}</li>" for text in report.alarms]
        items += [f"<li>{esc(text)}</li>" for text in report.worse]
        body = f"<ul>{''.join(items)}</ul>" if items else ""
        if level == "ok":
            body = "<p>各来源的比例都没有低过报警线，没有新变坏的页，没有新的崩溃和超时。</p>"
        elif level == "none":
            body = "<p>下面的数字只是这一次的结果，没有和上一次比。</p>"
        elif level == "worse":
            body += f"<p>各来源的比例没有比基线低 {report.max_drop_pp:g} 个百分点以上，也没有新的崩溃和超时，所以没到《测试计划》第 7 节的报警线；上面这些仍然要处理。</p>"
        out = f'<div class="banner {level}"><h2>{LEVEL_TITLES[level]}</h2>{body}</div>'
        if report.notices:
            out += '<div class="notices"><strong>提示</strong><ul>' + "".join(f"<li>{esc(t)}</li>" for t in report.notices) + "</ul></div>"
        return out

    # ---- 1. 按来源汇总 --------------------------------------------------

    def delta_cell(self, source: str) -> str:
        delta = self.report.source_delta(source)
        if delta is None:
            return '<td class="n sub">—</td>'
        if delta.base is None:
            return '<td class="n"><span class="tag info">基线里没有</span></td>'
        if delta.delta_pp is None:
            return '<td class="n sub">—</td>'
        text = signed_pp(delta.delta_pp)
        if delta.delta_pp < -1e-9:
            cls = "down"
        elif delta.delta_pp > 1e-9:
            cls = "up"
        else:
            cls = ""
        return f'<td class="n {cls}">{text}</td>'

    def summary_row(self, name: str, s: Summary, *, cls: str = "", delta: str = '<td class="n sub">—</td>') -> str:
        hints = sorted(s.hints.items(), key=lambda item: (-item[1], item[0]))[:3]
        hint = "，".join(f"{HINT_LABELS.get(k, k)} {v}" for k, v in hints) or "—"
        cells = [
            f"<td>{esc(name)}</td>",
            f'<td class="n">{num(s.files)}</td>',
            f'<td class="n">{num(s.pages)}</td>',
            f'<td class="n">{num(s.compared)}</td>',
            f'<td class="n">{num(s.passed)}</td>',
            f'<td class="n">{num(s.diff)}</td>',
            f'<td class="n">{pct(s.ratio_all)}</td>',
            f'<td class="n">{num(s.with_objects)}</td>',
            f'<td class="n">{num(s.passed_with_objects)}</td>',
            f'<td class="n"><strong>{pct(s.ratio_nonempty)}</strong></td>',
            delta,
            f'<td class="n">{num(s.errored)}</td>',
            f'<td class="n">{num(s.load_failed)}</td>',
            f'<td class="n">{num(s.save_failed + s.reopen_failed)}</td>',
            f'<td class="n">{_alert_num(s.crashed)}</td>',
            f'<td class="n">{_alert_num(s.timed_out)}</td>',
            f"<td>{esc(hint)}</td>",
        ]
        return f'<tr class="{cls}">{"".join(cells)}</tr>'

    def summary_section(self) -> str:
        report = self.report
        head = (
            "<tr><th>来源</th><th class=n>文件</th><th class=n>页数</th><th class=n>比对页</th><th class=n>无差异</th>"
            "<th class=n>有差异</th><th class=n>比例<br>（全部比对页）</th><th class=n>有对象的<br>比对页</th>"
            "<th class=n>其中<br>无差异</th><th class=n>比例<br>（扣除无对象页）</th><th class=n>比基线<br>（百分点）</th>"
            "<th class=n>页错误</th><th class=n>打开失败</th><th class=n>保存或<br>重开失败</th><th class=n>崩溃</th>"
            "<th class=n>超时</th><th>差异页的主要线索</th></tr>"
        )
        rows: list[str] = []
        for current in report.sets:
            by_source = summarize(current.snapshot.files)
            for source, s in by_source.items():
                rows.append(self.summary_row(source, s, delta=self.delta_cell(source)))
            if len(by_source) > 1 and len(report.sets) > 1:
                rows.append(self.summary_row(f"{current.spec.title} 小计", total_of(by_source.values()), cls="subtotal"))
        all_sources = report.by_source()
        if len(all_sources) > 1:
            rows.append(self.summary_row("合计（仅供参考）", total_of(all_sources.values()), cls="total"))
        note = (
            '<p class="sub">比对页 = 像素判定出了结果的页（无差异 + 有差异）；页错误是原件这一页本来就加载不了，'
            "崩溃、超时、打开失败的文件里的页都不算比对页。无对象页 = 无差异并且页上没有任何对象的页，"
            "“扣除无对象页”就是把它们从分子分母里都去掉；退步判定用这一列。判定“无差异”的阈值见最后一节。"
            "合计只作参考：各来源要分开看（方案 13.1）。</p>"
        )
        return f'<h2 id="summary">1. 按来源汇总（像素比对）</h2><div class="scroll"><table>{head}{"".join(rows)}</table></div>{note}'

    # ---- 2. 和基线比较 --------------------------------------------------

    def change_rows(self, current: SetReport, changes: list[PageChange], *, with_shots: bool) -> str:
        files = {f.path: f for f in current.snapshot.files}
        rows: list[str] = []
        for change in changes:
            row_id = self.next_row()
            before, after = change.before_page, change.after_page
            file = files.get(change.path)
            if after is not None and after.pixel == "diff":
                now = f"差异像素 {num(after.diff_pixels)}，最大连通区域 {num(after.largest_region)}"
            elif after is not None and after.pixel == "error":
                now = after.error or "页错误"
            elif after is None and file is not None:
                now = f"文件{FILE_STATUS_LABELS.get(file.status, file.status)}，没有这一页的结果"
            else:
                now = "—"
            was = ""
            if before is not None and before.pixel == "diff":
                was = f"差异像素 {num(before.diff_pixels)}，最大连通区域 {num(before.largest_region)}"
            ops_now = ops_brief(after) if after is not None and current.snapshot.has_ops else "—"
            shots = ""
            if with_shots:
                shots = (
                    self.shots_cell(current, file, after, row_id)
                    if file is not None and after is not None and after.images
                    else '<td class="shots sub">—</td>'
                )
            rows.append(
                f'<tr id="{row_id}"><td>{esc(change.source)}</td><td class="path">{esc(change.path)}</td>'
                f'<td class="n">{change.number}</td>'
                f"<td>{PIXEL_LABELS.get(change.before, change.before)}{'（' + was + '）' if was else ''}</td>"
                f"<td>{PIXEL_LABELS.get(change.after, change.after)}</td><td>{esc(now)}</td><td>{esc(ops_now)}</td>{shots}</tr>"
            )
        return "".join(rows)

    def change_table(self, current: SetReport, changes: list[PageChange], *, with_shots: bool) -> str:
        head = "<tr><th>来源</th><th>文件</th><th class=n>页</th><th>基线里</th><th>这次</th><th>这次的数</th><th>这次的操作符级</th>"
        head += "<th>原件 / 保存结果 / 差异</th></tr>" if with_shots else "</tr>"
        return f'<div class="scroll"><table>{head}{self.change_rows(current, changes, with_shots=with_shots)}</table></div>'

    def ops_change_table(self, changes: list[PageChange]) -> str:
        rows = []
        for change in changes:
            before = ops_brief(change.before_page) if change.before_page else "—"
            after = ops_brief(change.after_page) if change.after_page else "—"
            rows.append(
                f'<tr><td>{esc(change.source)}</td><td class="path">{esc(change.path)}</td><td class="n">{change.number}</td>'
                f"<td>{esc(before)}</td><td>{esc(after)}</td></tr>"
            )
        head = "<tr><th>来源</th><th>文件</th><th class=n>页</th><th>基线里的操作符级差异</th><th>这次</th></tr>"
        return f'<div class="scroll"><table>{head}{"".join(rows)}</table></div>'

    def baseline_section(self) -> str:
        report = self.report
        out = ['<h2 id="baseline">2. 和基线比较</h2>']
        out.append(
            f'<p class="sub">规则（《测试计划》第 7 节）：任一来源的比例（扣除无对象页）比基线低 {report.max_drop_pp:g} 个百分点以上，'
            "或者出现基线里没有的崩溃、超时，算到了报警线；有新变坏的页但没到这条线，算“有退步，没到报警线”。"
            "新变好的页不算退步，但要更新基线。基线怎么存、怎么更新见 <code>tests/baselines/README.md</code>。</p>"
        )
        for current in report.sets:
            out.append(f"<h3>{esc(current.spec.title)}</h3>")
            cmp = current.comparison
            if cmp is None or current.baseline is None:
                out.append(f'<p><span class="tag warn">没有比</span> {esc(current.baseline_problem or "没有给基线")}。</p>')
                continue
            meta = current.baseline.meta
            bits = []
            if current.spec.baseline is not None:
                bits.append(f"基线文件 <code>{esc(current.spec.baseline.name)}</code>")
            if meta.get("generated"):
                bits.append(f"生成于 {esc(local_time(str(meta.get('generated'))))}")
            if meta.get("commit"):
                bits.append(f"提交 {esc(str(meta.get('commit'))[:10])}")
            if meta.get("pdfium_version"):
                bits.append(f"PDFium {esc(meta.get('pdfium_version'))}")
            if meta.get("machine"):
                bits.append(f"机器 {esc(meta.get('machine'))}")
            bits.append(f"{num(len(current.baseline.files))} 个文件、{num(current.baseline.page_count())} 页")
            out.append(f'<p class="sub">{"；".join(bits)}。</p>')

            head = (
                "<tr><th>来源</th><th class=n>基线的比例<br>（扣除无对象页）</th><th class=n>这次</th><th class=n>变化<br>（百分点）</th>"
                "<th class=n>新变坏的页</th><th class=n>新变好的页</th>"
                "<th class=n>两项都无差异：<br>基线的比例</th><th class=n>这次</th><th class=n>变化<br>（百分点）</th>"
                "<th class=n>操作符级<br>新变坏</th><th class=n>操作符级<br>新变好</th><th>结论</th></tr>"
            )
            rows = []
            for source, delta in cmp.sources.items():
                if delta.cur is None:
                    verdict = '<span class="tag warn">这次没有这个来源</span>'
                elif delta.base is None:
                    verdict = '<span class="tag info">基线里没有</span>'
                elif delta.alarm:
                    verdict = '<span class="tag bad">低过报警线</span>'
                elif delta.newly_failed or delta.ops_newly_failed:
                    verdict = '<span class="tag warn">有新变坏的页</span>'
                elif delta.newly_passed or delta.ops_newly_passed:
                    verdict = '<span class="tag good">变好了</span>'
                else:
                    verdict = "没有变化"
                rows.append(
                    f"<tr><td>{esc(source)}</td>"
                    f'<td class="n">{pct(delta.base.ratio_nonempty if delta.base else None)}</td>'
                    f'<td class="n">{pct(delta.cur.ratio_nonempty if delta.cur else None)}</td>'
                    f"{_delta_td(delta.delta_pp)}"
                    f'<td class="n">{_alert_num(delta.newly_failed)}</td><td class="n">{_good_num(delta.newly_passed)}</td>'
                    f'<td class="n">{pct(delta.base.strict_ratio_nonempty if delta.base else None)}</td>'
                    f'<td class="n">{pct(delta.cur.strict_ratio_nonempty if delta.cur else None)}</td>'
                    f"{_delta_td(delta.strict_delta_pp)}"
                    f'<td class="n">{_alert_num(delta.ops_newly_failed)}</td><td class="n">{_good_num(delta.ops_newly_passed)}</td>'
                    f"<td>{verdict}</td></tr>"
                )
            out.append(f'<div class="scroll"><table>{head}{"".join(rows)}</table></div>')

            if cmp.newly_failed:
                out.append(f'<h3><span class="tag bad">新变坏</span> {len(cmp.newly_failed)} 页：基线里像素无差异，这次不是</h3>')
                out.append(self.change_table(current, cmp.newly_failed, with_shots=True))
            if cmp.ops_newly_failed:
                out.append(f'<h3><span class="tag bad">操作符级新变坏</span> {len(cmp.ops_newly_failed)} 页：像素仍无差异，操作符级从没有差异变成有</h3>')
                out.append(self.ops_change_table(cmp.ops_newly_failed))
            changed = cmp.file_changes
            if changed:
                rows = []
                for c in changed:
                    if c.after in ("crashed", "timed_out") and c.before not in ("crashed", "timed_out"):
                        tag = '<span class="tag bad">新的崩溃或超时</span>'
                    elif c.before == "ok":
                        tag = '<span class="tag warn">变差</span>'
                    elif c.after == "ok":
                        tag = '<span class="tag good">恢复正常</span>'
                    else:
                        tag = ""
                    rows.append(
                        f'<tr><td>{esc(c.source)}</td><td class="path">{esc(c.path)}</td>'
                        f"<td>{FILE_STATUS_LABELS.get(c.before, c.before)}</td><td>{FILE_STATUS_LABELS.get(c.after, c.after)}</td><td>{tag}</td></tr>"
                    )
                out.append(f"<h3>文件状态变了的 {len(changed)} 个</h3>")
                out.append(
                    '<div class="scroll"><table><tr><th>来源</th><th>文件</th><th>基线里</th><th>这次</th><th></th></tr>'
                    + "".join(rows)
                    + "</table></div>"
                )
            if cmp.newly_passed:
                out.append(f'<h3><span class="tag good">新变好</span> {len(cmp.newly_passed)} 页：基线里不是无差异，这次像素无差异</h3>')
                out.append(self.change_table(current, cmp.newly_passed, with_shots=False))
            if cmp.ops_newly_passed:
                out.append(f'<h3><span class="tag good">操作符级新变好</span> {len(cmp.ops_newly_passed)} 页</h3>')
                out.append(self.ops_change_table(cmp.ops_newly_passed))
            if cmp.other_changes:
                out.append(
                    f"<details><summary>判定变了、但两边都不是无差异的 {len(cmp.other_changes)} 页（有差异、页错误、没有比对到之间的变化）</summary>"
                    + self.change_table(current, cmp.other_changes, with_shots=False)
                    + "</details>"
                )
            for label, files in (("基线里有、这次没有跑到的文件", cmp.removed_files), ("这次有、基线里没有的文件", cmp.added_files)):
                if files:
                    items = "".join(f'<tr><td>{esc(f.source)}</td><td class="path">{esc(f.path)}</td></tr>' for f in files)
                    out.append(f"<details><summary>{label}（{len(files)} 个）</summary><table><tr><th>来源</th><th>文件</th></tr>{items}</table></details>")
            if not (cmp.newly_failed or cmp.ops_newly_failed or cmp.newly_passed or cmp.ops_newly_passed or changed or cmp.other_changes or cmp.removed_files or cmp.added_files):
                out.append("<p>每一页的判定都和基线一样。</p>")
        return "".join(out)

    # ---- 3. 没通过的页 --------------------------------------------------

    def baseline_note(self, current: SetReport, file: FileResult, page: Page) -> str:
        if current.baseline is None:
            return '<span class="sub">—</span>'
        if file.path not in current.baseline_status:
            return '<span class="tag info">基线里没有这个文件</span>'
        before = current.baseline_pages.get((file.path, page.number))
        if before is None:
            return '<span class="tag info">基线里没有这一页</span>'
        if before.pixel == "pass":
            return '<span class="tag bad">新变坏</span>'
        if before.pixel == "diff":
            if before.diff_pixels != page.diff_pixels:
                arrow = "↓" if page.diff_pixels < before.diff_pixels else "↑"
                return f"原来就有差异；差异像素 {num(before.diff_pixels)} → {num(page.diff_pixels)} {arrow}"
            return "原来就有差异，数没变"
        return f"基线里是{PIXEL_LABELS.get(before.pixel, before.pixel)}"

    def failed_section(self) -> str:
        out = ['<h2 id="failed">3. 没通过的页（像素有差异）</h2>']
        total = 0
        for current in self.report.sets:
            newly = {(c.path, c.number) for c in current.comparison.newly_failed} if current.comparison else set()
            by_source: dict[str, list[tuple[FileResult, Page]]] = {}
            for file in current.snapshot.files:
                for page in file.pages:
                    if page.pixel == "diff":
                        by_source.setdefault(file.source, []).append((file, page))
            for source, items in by_source.items():
                total += len(items)
                items.sort(key=lambda item: ((item[0].path, item[1].number) not in newly, item[0].path, item[1].number))
                rows = []
                for file, page in items:
                    row_id = self.next_row()
                    share = page.diff_pixels / page.total_pixels if page.total_pixels else None
                    rows.append(
                        f'<tr id="{row_id}"><td class="path">{esc(file.path)}</td><td class="n">{page.number}</td>'
                        f'<td class="n">{num(page.diff_pixels)}</td><td class="n">{small_pct(share)}</td>'
                        f'<td class="n">{num(page.largest_region)}</td><td>{esc(hint_text(page.hints))}</td>'
                        f"<td>{esc(ops_brief(page) if current.snapshot.has_ops else '—')}</td>"
                        f"<td>{self.baseline_note(current, file, page)}</td>"
                        f"{self.shots_cell(current, file, page, row_id)}</tr>"
                    )
                out.append(f"<h3>{esc(source)}：{len(items)} 页</h3>")
                out.append(
                    '<div class="scroll"><table><tr><th>文件</th><th class=n>页</th><th class=n>差异像素</th><th class=n>占页面</th>'
                    "<th class=n>最大连通区域<br>（像素）</th><th>线索</th><th>操作符级</th><th>和基线比</th>"
                    "<th>原件 / 保存结果 / 差异（点开看大图和明细）</th></tr>" + "".join(rows) + "</table></div>"
                )
        if total == 0:
            out.append("<p>没有。</p>")
        elif self.thumbs.note:
            out.append(f'<p class="sub">{esc(self.thumbs.note)}</p>')
        return "".join(out)

    # ---- 4. 操作符级 ----------------------------------------------------

    def ops_row(self, name: str, s: Summary, *, cls: str = "", delta: str = '<td class="n sub">—</td>') -> str:
        counts = "".join(
            f'<td class="n">{num(s.ops_counts.get(k, 0))}（{num(s.ops_count_pages.get(k, 0))} 页）</td>' for k in OPS_KEYS
        )
        return (
            f'<tr class="{cls}"><td>{esc(name)}</td><td class="n">{num(s.compared)}</td><td class="n">{num(s.ops_pages)}</td>'
            f'<td class="n">{num(s.ops_missing)}</td><td class="n">{num(s.ops_diff_pages)}</td>'
            f'<td class="n">{num(s.pixel_pass_ops_diff)}</td><td class="n">{num(s.strict_pass)}</td>'
            f'<td class="n">{num(s.strict_with_objects)}</td><td class="n">{num(s.strict_passed_with_objects)}</td>'
            f'<td class="n"><strong>{pct(s.strict_ratio_nonempty)}</strong></td>{delta}{counts}</tr>'
        )

    def ops_section(self) -> str:
        report = self.report
        out = ['<h2 id="ops">4. 操作符级比对</h2>']
        if not any(s.snapshot.has_ops for s in report.sets):
            out.append("<p>这一次没有操作符级比对的结果（没有给 <code>ops_diff.py --json</code> 的输出）。</p>")
            return "".join(out)
        out.append(
            '<p class="sub">像素比对看不出颜色空间被换算成等价的 RGB、叠印参数和文字状态丢失；<code>ops_diff.py</code> '
            "把往返前后的内容流按绘制事件对齐，比较颜色空间、文字状态、ExtGState、剪裁和事件本身。"
            "按方案 13.2，“像素一致但操作符变了”的页算差异页，所以这里另报一个“两项都无差异”的比例。</p>"
        )
        head = (
            "<tr><th>来源</th><th class=n>比对页</th><th class=n>做成了<br>操作符级比对</th><th class=n>没做成</th>"
            "<th class=n>有操作符<br>差异的页</th><th class=n>其中像素<br>无差异</th><th class=n>两项都<br>无差异</th>"
            "<th class=n>有对象的页</th><th class=n>其中两项<br>都无差异</th><th class=n>比例<br>（扣除无对象页）</th>"
            "<th class=n>比基线<br>（百分点）</th>" + "".join(f"<th class=n>{OPS_LABELS[k]}</th>" for k in OPS_KEYS) + "</tr>"
        )
        rows = []
        for current in report.sets:
            if not current.snapshot.has_ops:
                continue
            by_source = summarize(current.snapshot.files)
            for source, s in by_source.items():
                delta = report.source_delta(source)
                rows.append(self.ops_row(source, s, delta=_delta_td(delta.strict_delta_pp if delta else None)))
            if len(by_source) > 1:
                rows.append(self.ops_row(f"{current.spec.title} 小计", total_of(by_source.values()), cls="subtotal"))
        out.append(f'<div class="scroll"><table>{head}{"".join(rows)}</table></div>')
        # 操作符级不出全部语料集的合计：各语料集的性质差得远，加在一起没有意义。
        out.append(
            '<p class="sub">后五列是差异的处数，括号里是涉及的页数。“没做成”的页不算进这张表的比例：保存结果没写出来（文件打不开、崩溃）、'
            "qpdf 打不开原件、qpdf 和 PDFium 数出来的页数不一样（页码对不上，整个文件不用）。</p>"
        )

        for current in report.sets:
            if not current.snapshot.has_ops:
                continue
            pages = [(f, p) for f in current.snapshot.files for p in f.pages if p.pixel == "pass" and p.ops and any(p.ops.values())]
            if pages:
                rows = []
                for file, page in pages:
                    detail = safe_ops_detail(page.ops_detail)
                    parts = []
                    for group in ("color", "text", "gs", "clip", "missing", "extra"):
                        counter = detail.get(group) or {}
                        for name, count in list(counter.items())[:2]:
                            label = {"missing": "缺失 ", "extra": "多出 ", "clip": "剪裁 "}.get(group, "")
                            parts.append(f"{label}{name} ×{count}")
                    rows.append(
                        f'<tr><td>{esc(file.source)}</td><td class="path">{esc(file.path)}</td><td class="n">{page.number}</td>'
                        f"<td>{esc(ops_brief(page))}</td><td>{esc('；'.join(parts[:6]))}</td></tr>"
                    )
                out.append(
                    f"<details><summary>{esc(current.spec.title)}：像素无差异但操作符有差异的 {len(pages)} 页</summary>"
                    '<div class="scroll"><table><tr><th>来源</th><th>文件</th><th class=n>页</th><th>差异处数</th><th>主要是什么</th></tr>'
                    + "".join(rows)
                    + "</table></div></details>"
                )
            skipped: dict[str, list[FileResult]] = {}
            for file in current.snapshot.files:
                if file.ops_note and any(p.compared for p in file.pages):
                    skipped.setdefault(file.ops_note, []).append(file)
            page_errors = [(f, p) for f in current.snapshot.files for p in f.pages if p.compared and p.ops_error]
            if skipped or page_errors:
                rows = []
                for reason, files in sorted(skipped.items()):
                    for file in files:
                        compared = sum(1 for p in file.pages if p.compared)
                        rows.append(f'<tr><td>{esc(file.source)}</td><td class="path">{esc(file.path)}</td><td class="n">{compared}</td><td>{esc(reason)}</td></tr>')
                for file, page in page_errors:
                    rows.append(f'<tr><td>{esc(file.source)}</td><td class="path">{esc(file.path)}</td><td class="n">1</td><td>第 {page.number} 页：{esc(page.ops_error)}</td></tr>')
                out.append(
                    f"<details><summary>{esc(current.spec.title)}：操作符级没做成的文件和页（{len(rows)} 条）</summary>"
                    '<div class="scroll"><table><tr><th>来源</th><th>文件</th><th class=n>比对页</th><th>原因</th></tr>' + "".join(rows) + "</table></div></details>"
                )
        return "".join(out)

    # ---- 5. 没能比对的 --------------------------------------------------

    def files_section(self) -> str:
        out = ['<h2 id="files">5. 崩溃、超时、打不开的文件和加载不了的页</h2>']
        any_rows = False
        for current in self.report.sets:
            groups: dict[str, list[FileResult]] = {}
            for file in current.snapshot.files:
                if file.status != "ok":
                    groups.setdefault(file.status, []).append(file)
            errors = [(f, p) for f in current.snapshot.files for p in f.pages if p.pixel == "error"]
            if not groups and not errors:
                continue
            any_rows = True
            out.append(f"<h3>{esc(current.spec.title)}</h3>")
            base_status = current.baseline_status
            for status in ("crashed", "timed_out", "save_failed", "reopen_failed", "load_failed"):
                files = groups.get(status) or []
                if not files:
                    continue
                rows = []
                for file in files:
                    code = f"0x{file.exit_code & 0xFFFFFFFF:08X}" if file.exit_code is not None else "—"
                    if not base_status:
                        note = "—"
                    elif file.path not in base_status:
                        note = '<span class="tag info">基线里没有这个文件</span>'
                    elif base_status[file.path] == file.status:
                        note = "基线里也是"
                    else:
                        note = f'<span class="tag bad">基线里是{FILE_STATUS_LABELS.get(base_status[file.path], base_status[file.path])}</span>'
                    rows.append(
                        f'<tr><td>{esc(file.source)}</td><td class="path">{esc(file.path)}</td><td>{esc(code)}</td>'
                        f"<td>{esc(file.error)}</td><td>{note}</td></tr>"
                    )
                table = (
                    '<div class="scroll"><table><tr><th>来源</th><th>文件</th><th>退出码</th><th>工具给的原因</th><th>和基线比</th></tr>'
                    + "".join(rows)
                    + "</table></div>"
                )
                title = f"{FILE_STATUS_LABELS.get(status, status)}：{len(files)} 个文件"
                if status == "load_failed":
                    out.append(f"<details><summary>{title}（原件就打不开：要口令或者格式损坏，和往返无关）</summary>{table}</details>")
                else:
                    out.append(f"<h3>{title}</h3>{table}")
            if errors:
                rows = [
                    f'<tr><td>{esc(f.source)}</td><td class="path">{esc(f.path)}</td><td class="n">{p.number}</td><td>{esc(p.error)}</td></tr>'
                    for f, p in errors
                ]
                out.append(
                    f"<details><summary>页错误：{len(errors)} 页（原件这一页本来就加载不了或渲染不了，不算比对页）</summary>"
                    '<div class="scroll"><table><tr><th>来源</th><th>文件</th><th class=n>页</th><th>工具给的原因</th></tr>' + "".join(rows) + "</table></div></details>"
                )
        if not any_rows:
            out.append("<p>没有。</p>")
        out.append('<p class="sub">崩溃的文件，子进程标准错误的内容没有放到页面上，要看去原始的 roundtrip JSON 里看。</p>')
        return "".join(out)

    # ---- 6. 这一次怎么跑的 ----------------------------------------------

    def run_section(self) -> str:
        report = self.report
        env = report.env
        out = ['<h2 id="run">6. 这一次是怎么跑的</h2>']
        rows = []
        for current in report.sets:
            spec, meta = current.spec, current.snapshot.meta
            corpus = meta.get("corpus") or {}
            cal = current.calibration
            if cal is None:
                cal_text = "没有跑"
            elif cal.failed or cal.crashed:
                cal_text = f"{num(cal.compared)} 页里 {len(cal.failed)} 页有差异，{cal.crashed} 个文件崩溃或超时"
            else:
                cal_text = f"{num(cal.compared)} 页全部无差异"
            seconds = spec.seconds
            timing = "，".join(
                f"{label} {seconds[key]:.1f} s"
                for key, label in (("roundtrip", "往返"), ("calibrate", "校准"), ("ops_diff", "操作符级"))
                if key in seconds
            )
            rows.append(
                f"<tr><td>{esc(spec.title)}</td><td>{esc(spec.name)}</td><td>{esc(spec.source_prefix or '—')}</td>"
                f'<td class="n">{num(len(current.snapshot.files))}</td><td class="n">{num(current.snapshot.page_count())}</td>'
                f"<td>{esc(str(corpus.get('sha256', ''))[:16] or '—')}</td><td>{esc(cal_text)}</td><td>{esc(timing or '—')}</td></tr>"
            )
        out.append(
            '<div class="scroll"><table><tr><th>语料集</th><th>名字（基线文件名）</th><th>来源前缀</th><th class=n>文件</th><th class=n>页</th>'
            "<th>语料指纹（前 16 位）</th><th>校准轮（不重写内容流）</th><th>耗时</th></tr>" + "".join(rows) + "</table></div>"
        )
        first = report.sets[0].snapshot.meta if report.sets else {}
        thresholds = first.get("thresholds") or {}
        kv: list[tuple[str, str]] = []
        if thresholds:
            kv.append(
                (
                    "判定“无差异”",
                    f"渲染倍率 {first.get('scale')}（144 dpi 是 2）；任一颜色通道差值大于 {thresholds.get('channel_tolerance')} 的像素算差异像素；"
                    f"差异像素少于页面的 {float(thresholds.get('max_diff_ratio') or 0) * 100:g}%，并且最大连通差异区域不超过 "
                    f"{thresholds.get('max_region_pixels')} 个像素，算无差异（《P0 技术验证计划》V02）",
                )
            )
        kv.append(("隔离与超时", f"每个文件一个子进程；单个文件超时 {first.get('timeout_s')} 秒；同时 {first.get('jobs')} 个子进程"))
        git = env.get("git") or {}
        if git.get("commit"):
            kv.append(("代码", f"{git.get('commit')}{'（' + str(git.get('branch')) + '）' if git.get('branch') else ''}{'，工作区有没提交的改动' if git.get('dirty') else ''}"))
        cli = env.get("cli") or {}
        if cli:
            kv.append(("命令行工具", f"{cli.get('version') or ''}　SHA-256 {str(cli.get('sha256') or '')[:16]}…"))
        if env.get("pdfium_dll_sha256"):
            kv.append(("PDFium", f"{first.get('pdfium_version') or ''}　pdfium.dll 的 SHA-256 {str(env.get('pdfium_dll_sha256'))[:16]}…"))
        if env.get("machine"):
            detail = "，".join(str(env[k]) for k in ("os", "cpu") if env.get(k))
            kv.append(("机器", f"{env.get('machine')}{'（' + detail + '）' if detail else ''}"))
        if env.get("started"):
            kv.append(("开始时间", local_time(str(env.get("started")))))
        out.append('<dl class="kv">' + "".join(f"<dt>{esc(k)}</dt><dd>{esc(v)}</dd>" for k, v in kv) + "</dl>")
        return "".join(out)

    def build(self) -> str:
        body = [
            self.header(),
            self.banner(),
            '<nav><a href="#summary">1. 按来源汇总</a><a href="#baseline">2. 和基线比较</a><a href="#failed">3. 没通过的页</a>'
            '<a href="#ops">4. 操作符级比对</a><a href="#files">5. 崩溃、超时、打不开</a><a href="#run">6. 怎么跑的</a></nav>',
            self.summary_section(),
            self.baseline_section(),
            self.failed_section(),
            self.ops_section(),
            self.files_section(),
            self.run_section(),
            '<p class="foot">由 tests/corpus/tools/roundtrip_report.py 生成。页面不引用外部的脚本、样式和图片；'
            "原图是相对路径，在输出目录各语料集的 diff 子目录里。</p>",
        ]
        title = f"MagPDF 往返测试报告 {local_time(self.report.generated)}"
        return (
            '<!doctype html>\n<html lang="zh-CN">\n<head>\n<meta charset="utf-8">\n'
            '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
            f"<title>{esc(title)}</title>\n<style>{CSS}</style>\n</head>\n<body>\n"
            + "\n".join(body)
            + "\n"
            + "\n".join(self.viewers)
            + "\n</body>\n</html>\n"
        )


def _alert_num(value: int) -> str:
    return f'<span class="down">{num(value)}</span>' if value else "0"


def _good_num(value: int) -> str:
    return f'<span class="up">{num(value)}</span>' if value else "0"


def _delta_td(value: float | None) -> str:
    if value is None:
        return '<td class="n sub">—</td>'
    cls = "down" if value < -1e-9 else "up" if value > 1e-9 else ""
    return f'<td class="n {cls}">{signed_pp(value)}</td>'


def render_html(report: Report, out_dir: Path, *, thumb_width: int = 200, max_thumb_pages: int = 300) -> str:
    return HtmlBuilder(report, out_dir, Thumbnails(thumb_width, max_thumb_pages)).build()


# ---------------------------------------------------------------------------
# Markdown 和 JSON 摘要


def render_markdown(report: Report) -> str:
    """给流水线的运行摘要和文字报告用的表（和页面第 1 节是同一张）。"""
    lines = [f"## 往返测试：{LEVEL_TITLES[report.level]}", ""]
    for text in report.alarms + report.worse:
        lines.append(f"- {text}")
    for text in report.notices:
        lines.append(f"- 提示：{text}")
    if report.alarms or report.worse or report.notices:
        lines.append("")
    lines.append(
        "| 来源 | 文件 | 页数 | 比对页 | 无差异 | 比例（全部比对页） | 有对象的比对页 | 其中无差异 | 比例（扣除无对象页） "
        "| 比基线（百分点） | 页错误 | 打开失败 | 保存或重开失败 | 崩溃 | 超时 |"
    )
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")

    def row(name: str, s: Summary, delta: str) -> str:
        return (
            f"| {name} | {s.files} | {s.pages} | {s.compared} | {s.passed} | {pct(s.ratio_all)} | {s.with_objects} "
            f"| {s.passed_with_objects} | {pct(s.ratio_nonempty)} | {delta} | {s.errored} | {s.load_failed} "
            f"| {s.save_failed + s.reopen_failed} | {s.crashed} | {s.timed_out} |"
        )

    for current in report.sets:
        by_source = summarize(current.snapshot.files)
        for source, s in by_source.items():
            delta = report.source_delta(source)
            lines.append(row(source, s, signed_pp(delta.delta_pp) if delta else "—"))
        if len(by_source) > 1 and len(report.sets) > 1:
            lines.append(row(f"{current.spec.title} 小计", total_of(by_source.values()), "—"))
    all_sources = report.by_source()
    if len(all_sources) > 1:
        lines.append(row("合计（仅供参考）", total_of(all_sources.values()), "—"))

    if any(s.snapshot.has_ops for s in report.sets):
        lines += [
            "",
            "| 来源 | 做成操作符级比对的页 | 没做成 | 有操作符差异的页 | 其中像素无差异 | 两项都无差异 | 比例（扣除无对象页） "
            "| 颜色 | 文字状态 | ExtGState | 剪裁 | 事件缺失/多出 |",
            "|---|---|---|---|---|---|---|---|---|---|---|---|",
        ]
        for current in report.sets:
            if not current.snapshot.has_ops:
                continue
            for source, s in summarize(current.snapshot.files).items():
                counts = " | ".join(f"{s.ops_counts.get(k, 0)}（{s.ops_count_pages.get(k, 0)} 页）" for k in OPS_KEYS)
                lines.append(
                    f"| {source} | {s.ops_pages} | {s.ops_missing} | {s.ops_diff_pages} | {s.pixel_pass_ops_diff} "
                    f"| {s.strict_pass} | {pct(s.strict_ratio_nonempty)} | {counts} |"
                )

    for current in report.sets:
        cmp = current.comparison
        if cmp is None:
            continue
        for title, changes in (("新变坏的页", cmp.newly_failed), ("操作符级新变坏的页", cmp.ops_newly_failed), ("新变好的页", cmp.newly_passed)):
            if not changes:
                continue
            lines += ["", f"**{current.spec.title}：{title}（{len(changes)}）**", ""]
            for change in changes[:30]:
                lines.append(
                    f"- `{change.source}` `{change.path}` 第 {change.number} 页："
                    f"{PIXEL_LABELS.get(change.before, change.before)} → {PIXEL_LABELS.get(change.after, change.after)}"
                )
            if len(changes) > 30:
                lines.append(f"- …… 另有 {len(changes) - 30} 页，见报告页面")
    return "\n".join(lines) + "\n"


def summary_json(report: Report, exit_code: int) -> dict[str, Any]:
    by_source: dict[str, Any] = {}
    for source, s in report.by_source().items():
        entry = s.to_json()
        delta = report.source_delta(source)
        entry["baseline"] = delta.to_json() if delta is not None else None
        by_source[source] = entry
    sets = []
    for current in report.sets:
        cmp = current.comparison
        cal = current.calibration
        sets.append(
            {
                "name": current.spec.name,
                "title": current.spec.title,
                "source_prefix": current.spec.source_prefix,
                "files": len(current.snapshot.files),
                "pages": current.snapshot.page_count(),
                "has_ops": current.snapshot.has_ops,
                "meta": current.snapshot.meta,
                "seconds": current.spec.seconds,
                "total": total_of(summarize(current.snapshot.files).values()).to_json(),
                "calibration": None if cal is None else {"compared": cal.compared, "failed": len(cal.failed), "crashed": cal.crashed},
                "baseline": None
                if cmp is None or current.baseline is None
                else {
                    "file": str(current.spec.baseline) if current.spec.baseline else "",
                    "meta": current.baseline.meta,
                    "level": cmp.level,
                    "newly_failed": [c.to_json() for c in cmp.newly_failed],
                    "newly_passed": [c.to_json() for c in cmp.newly_passed],
                    "other_changes": [c.to_json() for c in cmp.other_changes],
                    "ops_newly_failed": [c.to_json() for c in cmp.ops_newly_failed],
                    "ops_newly_passed": [c.to_json() for c in cmp.ops_newly_passed],
                    "ops_lost": cmp.ops_lost,
                    "file_changes": [c.to_json() for c in cmp.file_changes],
                    "added_files": [f.path for f in cmp.added_files],
                    "removed_files": [f.path for f in cmp.removed_files],
                },
                "baseline_problem": current.baseline_problem,
            }
        )
    return {
        "kind": SUMMARY_KIND,
        "format": 1,
        "generated": report.generated,
        "level": report.level,
        "exit_code": exit_code,
        "max_drop_pp": report.max_drop_pp,
        "alarms": report.alarms,
        "worse": report.worse,
        "notices": report.notices,
        "env": report.env,
        "by_source": by_source,
        "total": total_of(report.by_source().values()).to_json(),
        "sets": sets,
    }


def exit_code_of(level: str, fail_on: str) -> int:
    if level == "alarm" and fail_on in ("any", "alarm"):
        return EXIT_ALARM
    if level == "worse" and fail_on == "any":
        return EXIT_WORSE
    return EXIT_OK


# ---------------------------------------------------------------------------
# 命令行


def specs_from_run(run_path: Path) -> tuple[list[SetInput], dict[str, Any]]:
    """读入口脚本写的 run.json。里面的相对路径都相对 run.json 所在的目录。"""
    data = read_json(run_path)
    if not isinstance(data, dict) or data.get("kind") != RUN_KIND:
        raise ValueError(f"{run_path} 不是 nightly-roundtrip.ps1 写的 run.json")
    base = run_path.resolve().parent

    def resolve(value: Any) -> Path | None:
        if not value:
            return None
        path = Path(str(value))
        return path if path.is_absolute() else base / path

    specs: list[SetInput] = []
    for entry in data.get("sets") or []:
        roundtrip = resolve(entry.get("roundtrip"))
        if roundtrip is None:
            raise ValueError(f"run.json 里语料集 {entry.get('name')} 没有 roundtrip")
        specs.append(
            SetInput(
                name=str(entry["name"]),
                title=str(entry.get("title") or entry["name"]),
                roundtrip=roundtrip,
                source_prefix=str(entry.get("source_prefix") or ""),
                ops=resolve(entry.get("ops")),
                ops_error=str(entry.get("ops_error") or ""),
                diff_dir=resolve(entry.get("diff_dir")),
                calibrate=resolve(entry.get("calibrate")),
                baseline=resolve(entry.get("baseline")),
                corpus_dir=resolve(entry.get("corpus_dir")),
                seconds={str(k): float(v) for k, v in (entry.get("seconds") or {}).items()},
            )
        )
    env = {k: data.get(k) for k in ("started", "git", "machine", "os", "cpu", "ci", "cli", "pdfium_dll_sha256", "options") if data.get(k) is not None}
    return specs, env


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="往返测试的报告页面：按来源汇总、没通过的页、和基线比较",
        epilog="退出码：0 没有退步或没有基线；2 到了报警线；3 有退步但没到报警线；1 工具出错。",
    )
    parser.add_argument("--run", type=Path, help="scripts/nightly-roundtrip.ps1 写的 run.json（可以有多个语料集）")
    parser.add_argument("--roundtrip", type=Path, help="单独一份 magpdf-cli roundtrip --json 的结果")
    parser.add_argument("--ops", type=Path, help="同一批的 ops_diff.py --json 结果")
    parser.add_argument("--diff-dir", type=Path, help="同一批的 roundtrip --diff-dir 目录")
    parser.add_argument("--name", default="run", help="语料集的名字（也是基线文件名）")
    parser.add_argument("--source-prefix", default="", help="加在来源名前面，例如 public/pdfium/")
    parser.add_argument("--corpus-dir", type=Path, help="语料目录，用来算语料指纹")
    parser.add_argument("--baseline", type=Path, help="基线文件")
    parser.add_argument("--out", type=Path, required=True, help="写出的 HTML 页面")
    parser.add_argument("--summary", type=Path, help="写出机器可读的摘要 JSON")
    parser.add_argument("--markdown", type=Path, help="写出 Markdown 摘要（汇总表和新变坏的页）")
    parser.add_argument("--baseline-out", type=Path, help="把这一次的结果按基线格式写到这个目录（<名字>.json），审过之后可以拿去换基线")
    parser.add_argument("--max-drop-pp", type=float, default=DEFAULT_MAX_DROP_PP, help="报警线：比例比基线低多少个百分点以上（默认 0.5）")
    parser.add_argument("--fail-on", choices=("any", "alarm", "never"), default="any", help="什么情况退出码非 0：any 有退步就算（默认）；alarm 只有到报警线才算；never 都不算")
    parser.add_argument("--require-baseline", action="store_true", help="有语料集没有基线时退出码 1")
    parser.add_argument("--thumb-width", type=int, default=200, help="内嵌缩略图的宽度（像素）")
    parser.add_argument("--max-thumb-pages", type=int, default=300, help="最多给多少页内嵌缩略图；0 是不内嵌")
    args = parser.parse_args(list(argv) if argv is not None else None)

    try:
        if args.run is not None:
            specs, env = specs_from_run(args.run)
        elif args.roundtrip is not None:
            specs = [
                SetInput(
                    name=args.name,
                    title=args.name,
                    roundtrip=args.roundtrip,
                    source_prefix=args.source_prefix,
                    ops=args.ops,
                    diff_dir=args.diff_dir,
                    baseline=args.baseline,
                    corpus_dir=args.corpus_dir,
                )
            ]
            env = {}
        else:
            parser.error("需要 --run，或者 --roundtrip")
        if not specs:
            print("run.json 里没有语料集", file=sys.stderr)
            return EXIT_ERROR
        report = build_report(specs, env, args.max_drop_pp)
    except (OSError, ValueError, KeyError) as e:
        print(f"读输入出错：{e}", file=sys.stderr)
        return EXIT_ERROR

    code = exit_code_of(report.level, args.fail_on)
    out_dir = args.out.resolve().parent
    out_dir.mkdir(parents=True, exist_ok=True)
    page = render_html(report, out_dir, thumb_width=args.thumb_width, max_thumb_pages=args.max_thumb_pages)
    args.out.write_text(page, encoding="utf-8", newline="\n")
    if args.markdown is not None:
        args.markdown.parent.mkdir(parents=True, exist_ok=True)
        args.markdown.write_text(render_markdown(report), encoding="utf-8", newline="\n")
    if args.baseline_out is not None:
        args.baseline_out.mkdir(parents=True, exist_ok=True)
        for current in report.sets:
            text = dumps_baseline(current.snapshot, baseline_meta(current, report))
            (args.baseline_out / f"{current.spec.name}.json").write_text(text, encoding="utf-8", newline="\n")
    missing = [s.spec.title for s in report.sets if s.baseline is None]
    if args.require_baseline and missing:
        code = EXIT_ERROR
    if args.summary is not None:
        args.summary.parent.mkdir(parents=True, exist_ok=True)
        text = json.dumps(summary_json(report, code), ensure_ascii=False, indent=1)
        args.summary.write_text(text + "\n", encoding="utf-8", newline="\n")

    print(f"往返测试：{LEVEL_TITLES[report.level]}")
    for text in report.alarms + report.worse:
        print(f"  - {text}")
    for text in report.notices:
        print(f"  提示：{text}")
    if args.require_baseline and missing:
        print(f"  没有基线的语料集：{'、'.join(missing)}（--require-baseline）", file=sys.stderr)
    print(f"报告页面：{args.out}")
    return code


if __name__ == "__main__":
    sys.exit(main())
