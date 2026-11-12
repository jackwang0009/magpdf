#!/usr/bin/env python3
"""数一数检测把多少条标注行切成了几段，并给一个“接起来再比”的参考口径（V09 报告 4.7）。

规范的口径（`ocr-eval`）要求一条标注行和一个识别框交并比 ≥ 0.5 才算配上。检测把一行切成两三段时，
每一段都配不上，整行的字都算错，尽管字其实都认出来了。这里换一个看法：识别框的面积有一半以上落在
某条标注行的框里，就算这条行的一段；一条行的各段沿行的方向从左到右接起来（两段之间补一个空格），
再和标注算编辑距离。它不依赖整页的阅读顺序，页面歪了、转了 90° 也能用。

只作参考，不是规范里的指标：它不罚“切成几段”，也不看多出来的框。

用法：
  python joined_lines.py out/default [out/en-rec ...] [--group lang_set] [--same-images-as out/server] [--json 输出.json]
  每个目录是 run_eval.ps1 的输出（要有 pred/ 和 Label.txt）。--group 可给多次：lang_set（默认）、
  lang_variant、seal、font、kind。
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

from analyze import distance, strip_space, variant_set

Poly = list[list[float]]
GROUPS = ["lang_set", "lang_variant", "seal", "font", "kind"]


def signed_area(poly: Poly) -> float:
    s = 0.0
    for i, (x0, y0) in enumerate(poly):
        x1, y1 = poly[(i + 1) % len(poly)]
        s += x0 * y1 - x1 * y0
    return s / 2


def clip(subject: Poly, clipper: Poly) -> Poly:
    """Sutherland–Hodgman：用凸多边形 `clipper` 裁 `subject`，返回相交的部分。"""
    if signed_area(clipper) < 0:
        clipper = clipper[::-1]
    out = subject
    for i, (ax, ay) in enumerate(clipper):
        bx, by = clipper[(i + 1) % len(clipper)]
        points, out = out, []
        if not points:
            break

        def inside(p: list[float]) -> bool:
            return (bx - ax) * (p[1] - ay) - (by - ay) * (p[0] - ax) >= 0

        def crossing(p: list[float], q: list[float]) -> list[float]:
            dx, dy = q[0] - p[0], q[1] - p[1]
            den = (bx - ax) * dy - (by - ay) * dx
            if abs(den) < 1e-12:
                return q
            t = ((by - ay) * (p[0] - ax) - (bx - ax) * (p[1] - ay)) / den
            return [p[0] + t * dx, p[1] + t * dy]

        for j, p in enumerate(points):
            q = points[(j + 1) % len(points)]
            if inside(q):
                if not inside(p):
                    out.append(crossing(p, q))
                out.append(q)
            elif inside(p):
                out.append(crossing(p, q))
    return out


@dataclass
class Acc:
    lines: int = 0
    split: int = 0
    no_box: int = 0
    chars: int = 0
    edits: int = 0
    nospace_chars: int = 0
    nospace_edits: int = 0

    def row(self) -> dict[str, float | int]:
        return {
            "gt_lines": self.lines,
            "split_lines": self.split,
            "split_percent": round(100 * self.split / self.lines, 2) if self.lines else 0.0,
            "no_box_lines": self.no_box,
            "joined_char_accuracy": round(100 * (1 - self.edits / self.chars), 2) if self.chars else 0.0,
            "joined_char_accuracy_nospace": round(100 * (1 - self.nospace_edits / self.nospace_chars), 2) if self.nospace_chars else 0.0,
        }


def read_labels(path: Path) -> dict[str, list[dict[str, object]]]:
    labels: dict[str, list[dict[str, object]]] = {}
    for line in path.read_text(encoding="utf-8").split("\n"):
        if "\t" in line:
            name, anns = line.split("\t", 1)
            labels[Path(name).stem] = json.loads(anns)
    return labels


def measure(
    run: Path, meta: dict[str, dict[str, dict[str, object]]], only: set[str] | None = None
) -> dict[str, dict[str, dict[str, float | int]]]:
    """`only` 给了就只统计这些图。"""
    labels = read_labels(run / "Label.txt")
    groups: dict[str, dict[str, Acc]] = defaultdict(lambda: defaultdict(Acc))
    for pred_file in sorted((run / "pred").glob("*.json")):
        stem = pred_file.stem
        if stem not in labels or stem not in meta["images"] or (only is not None and stem not in only):
            continue
        info = meta["images"][stem]
        page = meta["pages"][str(info["page"])]
        lang = stem.split("_")[0]
        variant, res = str(info["variant"]), str(info["res"])
        vset = variant_set(variant, res)
        result = json.loads(pred_file.read_text(encoding="utf-8"))
        preds = [(line["points"], line["text"], abs(signed_area(line["points"]))) for line in result["lines"]]
        for ann, ann_meta in zip(labels[stem], page["anns"]):  # type: ignore[arg-type]
            if ann_meta["kind"] == "seal":
                continue
            text = str(ann["transcription"])
            quad: Poly = ann["points"]  # type: ignore[assignment]
            x0, y0 = min(p[0] for p in quad), min(p[1] for p in quad)
            x1, y1 = max(p[0] for p in quad), max(p[1] for p in quad)
            ux, uy = quad[1][0] - quad[0][0], quad[1][1] - quad[0][1]
            parts: list[tuple[float, str]] = []
            for points, pred_text, pred_area in preds:
                if pred_area <= 0:
                    continue
                if (max(p[0] for p in points) < x0 or min(p[0] for p in points) > x1
                        or max(p[1] for p in points) < y0 or min(p[1] for p in points) > y1):
                    continue
                overlap = clip([list(p) for p in points], quad)
                if len(overlap) >= 3 and abs(signed_area(overlap)) >= 0.5 * pred_area:
                    cx = sum(p[0] for p in points) / len(points)
                    cy = sum(p[1] for p in points) / len(points)
                    parts.append((cx * ux + cy * uy, pred_text))
            parts.sort()
            joined = " ".join(t for _, t in parts)
            g_ns, p_ns = strip_space(text), strip_space(joined)
            edits = distance(text, joined) if parts else len(text)
            nospace_edits = distance(g_ns, p_ns) if parts else len(g_ns)
            seal = "under_seal" if ann_meta.get("under_seal") else "clear"
            keys = {
                "lang_set": f"{lang}|{vset}",
                "lang_variant": f"{lang}|{variant}|{res}",
                "seal": f"{lang}|{vset}|{seal}",
                "font": f"{vset}|{ann_meta['font']}",
                "kind": f"{lang}|{vset}|{ann_meta['kind']}",
            }
            for group, key in keys.items():
                acc = groups[group][key]
                acc.lines += 1
                acc.chars += len(text)
                acc.edits += edits
                acc.nospace_chars += len(g_ns)
                acc.nospace_edits += nospace_edits
                if len(parts) >= 2:
                    acc.split += 1
                if not parts:
                    acc.no_box += 1
    return {g: {k: v.row() for k, v in sorted(rows.items())} for g, rows in groups.items()}


def main() -> None:
    ap = argparse.ArgumentParser(description="一条标注行被切成几段；各段接起来再比的字符准确率（参考口径）")
    ap.add_argument("runs", nargs="+", type=Path, help="run_eval.ps1 的输出目录")
    ap.add_argument("--meta", type=Path, default=Path(__file__).parent / "out" / "meta.json")
    ap.add_argument("--group", action="append", choices=GROUPS, help="打印哪几组；默认 lang_set")
    ap.add_argument("--same-images-as", type=Path, help="只统计这个输出目录里也有的图（对照实验只跑了一部分图时用）")
    ap.add_argument("--json", type=Path, help="把全部分组写成 JSON")
    args = ap.parse_args()
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    meta = json.loads(args.meta.read_text(encoding="utf-8"))
    only = {p.stem for p in (args.same_images_as / "pred").glob("*.json")} if args.same_images_as else None
    results = {}
    for run in args.runs:
        groups = measure(run, meta, only)
        results[run.name] = groups
        for name in args.group or ["lang_set"]:
            print(f"\n===== {run.name}：按 {name} =====")
            print(f"{'组':<32} {'标注行':>6} {'切成几段的':>8} {'占比%':>6} {'一个框也没有':>8} {'接起来的字符准确率':>10} {'不计空格':>8}")
            for key, row in groups.get(name, {}).items():
                print(f"{key:<34} {row['gt_lines']:>7} {row['split_lines']:>10} {row['split_percent']:>8.2f} {row['no_box_lines']:>10} "
                      f"{row['joined_char_accuracy']:>14.2f} {row['joined_char_accuracy_nospace']:>12.2f}")
    if args.json:
        args.json.write_text(json.dumps(results, ensure_ascii=False, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
