#!/usr/bin/env python3
"""把 `magpdf-cli ocr-eval --details` 的明细按变体、分辨率、行的种类分开统计，并给错误分类（V09）。

`ocr-eval` 自己只按页面语言汇总。这里用 out/meta.json（gen_synthetic.py 写的）知道每张图是哪个变体、
哪种分辨率，每条标注行是标题、字段、单元格、句子还是小字，用的什么字体字号，再分组算字符准确率和
文字行召回率（口径同《测试样本收集与脱敏规范》7.1）。

每组除了规范里的两个指标，另给三个只作参考的口径：不计空格；不计空格并且全角半角算同一个字；
页级（整页的标注和识别结果各按阅读顺序接起来、去掉空白再比，一行切成几个框不影响它）。

错误分类：把每一行的标注和识别结果对齐（编辑距离的回溯），每一处编辑归到一类：
  line_missed   整行没有匹配上的识别结果（漏检，或识别置信度太低被丢掉，或框对不上）
  line_garbled  匹配上了，但错了一半以上（多半是整行读反了）
  space         少了或多了空格
  width         全角半角互换（“：”和“:”、“，”和“,”、“（”和“(”）
  case          大小写
  alnum         字母数字之间认错（O 和 0、l 和 1）
  punct         其余的标点错
  cjk           汉字认错、漏字、多字
  other         其余

用法：
  python analyze.py out/default [out/nocls ...] [--json 汇总.json] [--examples 5]
  每个参数是 run_eval.ps1 的一个输出目录（里面要有 details.json）。
"""
from __future__ import annotations

import argparse
import json
import sys
import unicodedata
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path

CLASSES = ["line_missed", "line_garbled", "space", "width", "case", "alnum", "punct", "cjk", "other"]


@dataclass
class Acc:
    """一组里累计的量。"""

    images: set[str] = field(default_factory=set)
    gt_lines: int = 0
    matched: int = 0
    gt_chars: int = 0
    edits: int = 0
    nospace_chars: int = 0
    nospace_edits: int = 0
    folded_edits: int = 0

    def add(self, image: str, line: "Line") -> None:
        self.images.add(image)
        self.gt_lines += 1
        self.gt_chars += line.chars
        self.edits += line.edits
        self.nospace_chars += line.nospace_chars
        self.nospace_edits += line.nospace_edits
        self.folded_edits += line.folded_edits
        if line.matched:
            self.matched += 1

    # 页级口径（ocr-eval 算好的）：整页的标注和识别结果各按阅读顺序接起来、去掉空白再比，
    # 不受一行切成几个框的影响。只在按图分的组里有意义。
    page_chars: int = 0
    page_edits: int = 0
    pred_lines: int = 0
    false_positives: int = 0

    def add_page(self, metrics: dict[str, int]) -> None:
        self.page_chars += metrics["page_gt_chars"]
        self.page_edits += metrics["page_edit_distance"]
        self.pred_lines += metrics["pred_lines"]
        self.false_positives += metrics["false_positives"]

    def row(self) -> dict[str, object]:
        def pct(err: int, total: int) -> float | None:
            return round(100 * (1 - err / total), 2) if total else None

        row: dict[str, object] = {
            "images": len(self.images),
            "gt_lines": self.gt_lines,
            "matched_lines": self.matched,
            "line_recall": round(100 * self.matched / self.gt_lines, 2) if self.gt_lines else None,
            "gt_chars": self.gt_chars,
            "edit_distance": self.edits,
            "char_accuracy": pct(self.edits, self.gt_chars),
            "char_accuracy_nospace": pct(self.nospace_edits, self.nospace_chars),
            "char_accuracy_nospace_folded": pct(self.folded_edits, self.nospace_chars),
        }
        if self.page_chars:
            row["page_char_accuracy"] = pct(self.page_edits, self.page_chars)
            row["pred_lines"] = self.pred_lines
            row["unmatched_pred_lines"] = self.false_positives
        return row


@dataclass
class Line:
    """一条标注行和它的识别结果比出来的量；每行只算一次，再加到各个分组里。"""

    chars: int
    edits: int
    matched: bool
    nospace_chars: int
    nospace_edits: int
    folded_edits: int

    @staticmethod
    def of(text: str, pred: str | None, edits: int) -> "Line":
        g = strip_space(text)
        if pred is None:
            return Line(len(text), edits, False, len(g), len(g), len(g))
        p = strip_space(pred)
        if g == p:
            return Line(len(text), edits, True, len(g), 0, 0)
        return Line(len(text), edits, True, len(g), distance(g, p), distance(fold(g), fold(p)))


def strip_space(s: str) -> str:
    return "".join(c for c in s if not c.isspace())


def fold(s: str) -> str:
    """全角半角归一（NFKC），只用在放宽的口径里。"""
    return unicodedata.normalize("NFKC", s)


def distance(a: str, b: str) -> int:
    prev = list(range(len(b) + 1))
    for ca in a:
        cur = [prev[0] + 1]
        for j, cb in enumerate(b):
            cur.append(min(prev[j] + (ca != cb), prev[j + 1] + 1, cur[j] + 1))
        prev = cur
    return prev[-1]


def align(a: str, b: str) -> list[tuple[str, str]]:
    """对齐后不相同的位置：(标注里的字, 识别出的字)，缺的一边是空串。"""
    n, m = len(a), len(b)
    d = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n + 1):
        d[i][0] = i
    for j in range(m + 1):
        d[0][j] = j
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            d[i][j] = min(d[i - 1][j - 1] + (a[i - 1] != b[j - 1]), d[i - 1][j] + 1, d[i][j - 1] + 1)
    ops: list[tuple[str, str]] = []
    i, j = n, m
    while i > 0 or j > 0:
        if i > 0 and j > 0 and d[i][j] == d[i - 1][j - 1] + (a[i - 1] != b[j - 1]):
            if a[i - 1] != b[j - 1]:
                ops.append((a[i - 1], b[j - 1]))
            i, j = i - 1, j - 1
        elif i > 0 and d[i][j] == d[i - 1][j] + 1:
            ops.append((a[i - 1], ""))
            i -= 1
        else:
            ops.append(("", b[j - 1]))
            j -= 1
    ops.reverse()
    return ops


def is_cjk(ch: str) -> bool:
    return "\u2e80" <= ch <= "\u9fff" and not unicodedata.category(ch).startswith("P")


def is_punct(ch: str) -> bool:
    return unicodedata.category(ch)[0] in "PS"


def classify(g: str, p: str) -> str:
    if g == " " or p == " ":
        return "space"
    if g and p:
        if fold(g) == fold(p):
            return "width"
        if g.lower() == p.lower():
            return "case"
        if is_cjk(g) or is_cjk(p):
            return "cjk"
        if g.isalnum() and p.isalnum():
            return "alnum"
        if is_punct(g) or is_punct(p):
            return "punct"
        return "other"
    ch = g or p
    if is_cjk(ch):
        return "cjk"
    if is_punct(ch):
        return "punct"
    if ch.isalnum():
        return "alnum"
    return "other"


def variant_set(variant: str, res: str) -> str:
    if variant == "photo":
        return f"photo_{res}"
    if variant in ("up180", "cw90", "ccw90"):
        return "orient"
    return f"scan{res}"


def analyze(run: Path, meta: dict[str, dict[str, dict[str, object]]], examples: int) -> dict[str, object]:
    details = json.loads((run / "details.json").read_text(encoding="utf-8"))
    groups: dict[str, dict[str, Acc]] = defaultdict(lambda: defaultdict(Acc))
    classes: dict[str, Counter[str]] = defaultdict(Counter)
    pairs: dict[str, Counter[str]] = defaultdict(Counter)
    shown: dict[str, list[dict[str, object]]] = defaultdict(list)
    garbled: list[dict[str, object]] = []
    for img in details:
        stem = Path(img["image"]).stem
        info = meta["images"][stem]
        page = meta["pages"][str(info["page"])]
        lang = img["lang"]
        variant, res = str(info["variant"]), str(info["res"])
        vset = variant_set(variant, res)
        anns = page["anns"]
        for g, key in (("lang_variant", f"{lang}|{variant}|{res}"), ("lang_set", f"{lang}|{vset}"), ("set", vset)):
            groups[g][key].add_page(img["metrics"])
        for line, match in zip(img["lines"], img["metrics"]["matches"]):
            ann = anns[match["gt_index"]]  # type: ignore[index]
            text, pred, edits = line["gt_text"], line["pred_text"], line["edit_distance"]
            keys = {
                "lang_variant": f"{lang}|{variant}|{res}",
                "lang_set": f"{lang}|{vset}",
                "set": vset,
                "kind": f"{lang}|{vset}|{ann['kind']}",
                "size": f"{lang}|{vset}|{ann['size']:g}pt",
                "font": f"{vset}|{ann['font']}",
                "doc": f"{lang}|{vset}|{page['doc']}",
                "leading": f"{vset}|{page['leading']}",
                "ruled_cell": f"{vset}|{'ruled' if page['ruled'] else 'plain'}" if ann["kind"] == "cell" else "",
                "seal": f"{lang}|{vset}|{'under_seal' if ann['under_seal'] else 'clear'}",
            }
            measured = Line.of(text, pred, edits)
            for g, key in keys.items():
                if key:
                    groups[g][key].add(stem, measured)
            ckey = f"{lang}|{vset}"
            if pred is None:
                classes[ckey]["line_missed"] += edits
                cls_of_line = "line_missed"
            elif edits * 2 > len(text):
                classes[ckey]["line_garbled"] += edits
                cls_of_line = "line_garbled"
                garbled.append({"image": stem, "kind": ann["kind"], "gt": text, "pred": pred})
            else:
                cls_of_line = ""
                for g_ch, p_ch in align(text, pred):
                    c = classify(g_ch, p_ch)
                    classes[ckey][c] += 1
                    pairs[ckey][f"{g_ch or '∅'}→{p_ch or '∅'}"] += 1
                    if len(shown[f"{ckey}|{c}"]) < examples:
                        shown[f"{ckey}|{c}"].append({"image": stem, "gt": text, "pred": pred})
            if cls_of_line and len(shown[f"{ckey}|{cls_of_line}"]) < examples:
                shown[f"{ckey}|{cls_of_line}"].append({"image": stem, "kind": ann["kind"], "gt": text, "pred": pred})
    summary_path = run / "summary.json"
    return {
        "run": run.name,
        "summary": json.loads(summary_path.read_text(encoding="utf-8-sig")) if summary_path.exists() else None,
        "groups": {g: {k: v.row() for k, v in sorted(rows.items())} for g, rows in groups.items()},
        "error_classes": {k: {c: v[c] for c in CLASSES if v[c]} for k, v in sorted(classes.items())},
        "top_confusions": {k: dict(v.most_common(25)) for k, v in sorted(pairs.items())},
        "examples": dict(sorted(shown.items())),
        "garbled_lines": garbled,
    }


def print_table(title: str, rows: dict[str, dict[str, object]]) -> None:
    print(f"\n{title}")
    print(f"{'组':<28} {'图':>4} {'行':>6} {'行召回':>7} {'字符数':>7} {'字符准确率':>9} {'不计空格':>8} {'再不计全半角':>10} {'页级':>6}")
    for key, r in rows.items():
        def f(v: object) -> str:
            return "   n/a" if v is None else f"{v:6.2f}"

        print(f"{key:<30} {r['images']:>4} {r['gt_lines']:>6} {f(r['line_recall']):>8} {r['gt_chars']:>8} "
              f"{f(r['char_accuracy']):>10} {f(r['char_accuracy_nospace']):>10} {f(r['char_accuracy_nospace_folded']):>12} "
              f"{f(r.get('page_char_accuracy')):>8}")


def main() -> None:
    ap = argparse.ArgumentParser(description="按变体、行的种类统计 OCR 评测明细，并给错误分类")
    ap.add_argument("runs", nargs="+", type=Path, help="run_eval.ps1 的输出目录")
    ap.add_argument("--meta", type=Path, default=Path(__file__).parent / "out" / "meta.json")
    ap.add_argument("--json", type=Path, help="把全部结果写成一个 JSON（每个目录另写一份 breakdown.json）")
    ap.add_argument("--examples", type=int, default=5, help="每类错误留几个例子")
    ap.add_argument("--quiet", action="store_true", help="不打印表")
    args = ap.parse_args()
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    meta = json.loads(args.meta.read_text(encoding="utf-8"))
    results = []
    for run in args.runs:
        result = analyze(run, meta, args.examples)
        (run / "breakdown.json").write_text(json.dumps(result, ensure_ascii=False, indent=1), encoding="utf-8")
        results.append(result)
        if args.quiet:
            continue
        print(f"\n===== {run.name} =====")
        groups = result["groups"]
        assert isinstance(groups, dict)
        print_table("按语言、变体、分辨率", groups["lang_variant"])
        print_table("按语言、样本集", groups["lang_set"])
        print_table("按行的种类", groups["kind"])
        print("\n错误分类（编辑次数）")
        for key, counts in result["error_classes"].items():  # type: ignore[union-attr]
            print(f"  {key:<22} " + "  ".join(f"{c}={n}" for c, n in counts.items()))
    if args.json:
        args.json.write_text(json.dumps(results, ensure_ascii=False, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
