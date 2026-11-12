#!/usr/bin/env python3
"""把两次评测限制在同一批图上并排比（V09 报告第 4 节各个对照用的就是它）。

对照实验大多只跑一部分图（比如服务器版模型只跑 300 dpi 的 144 张）。直接拿它的汇总去和全量的默认配置比，
比的不是同一批图。这里取两个输出目录里都有的图，两边各自只统计这些图，再按组并排列出文字行召回率、
字符准确率（口径同 analyze.py），并给出各类错误的编辑次数。

用法：
  python compare_runs.py out/default out/server [out/server-det ...] [--group lang_set] [--json 对比.json]
  第一个目录是基准，后面每个目录分别和它比。--group 可以给多次，可选的组见 analyze.py（lang_variant、
  lang_set、set、kind、size、font、doc、leading、ruled_cell、seal）。
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from analyze import CLASSES, analyze, image_stems

METRICS = ["line_recall", "char_accuracy", "char_accuracy_nospace", "char_accuracy_nospace_folded"]


def fmt(v: object) -> str:
    return "   n/a" if v is None else f"{float(v):6.2f}"  # type: ignore[arg-type]


def diff(a: object, b: object) -> str:
    if a is None or b is None:
        return "   n/a"
    return f"{float(b) - float(a):+6.2f}"  # type: ignore[arg-type]


def compare(
    base: Path,
    other: Path,
    meta: dict[str, dict[str, dict[str, object]]],
    group_names: list[str],
) -> dict[str, object]:
    common = image_stems(base) & image_stems(other)
    a = analyze(base, meta, 0, common)
    b = analyze(other, meta, 0, common)
    ga, gb = a["groups"], b["groups"]
    ea, eb = a["error_classes"], b["error_classes"]
    assert isinstance(ga, dict) and isinstance(gb, dict) and isinstance(ea, dict) and isinstance(eb, dict)
    print(f"\n===== {other.name} 对 {base.name}：两边都有的图 {len(common)} 张 =====")
    out_groups: dict[str, dict[str, object]] = {}
    for name in group_names:
        rows_a, rows_b = ga.get(name, {}), gb.get(name, {})
        print(f"\n按 {name}（每格：{base.name} → {other.name}）")
        print(f"{'组':<30} {'图':>4} {'行':>6}  {'行召回':<16} {'字符准确率':<22} {'不计空格':<16} {'再不计全半角':<16}")
        out_rows: dict[str, object] = {}
        for key in sorted(set(rows_a) | set(rows_b)):
            ra, rb = rows_a.get(key, {}), rows_b.get(key, {})
            cells = [f"{fmt(ra.get(m))} →{fmt(rb.get(m))}" for m in METRICS]
            print(f"{key:<32} {ra.get('images', 0):>4} {ra.get('gt_lines', 0):>6}  {cells[0]:<18} "
                  f"{cells[1]} ({diff(ra.get('char_accuracy'), rb.get('char_accuracy'))})   {cells[2]:<18} {cells[3]:<18}")
            out_rows[key] = {"base": ra, "other": rb}
        out_groups[name] = out_rows
    print("\n各类错误的编辑次数（基准 → 对照）")
    for key in sorted(set(ea) | set(eb)):
        ca, cb = ea.get(key, {}), eb.get(key, {})
        cells = [f"{c}={ca.get(c, 0)}→{cb.get(c, 0)}" for c in CLASSES if ca.get(c, 0) or cb.get(c, 0)]
        print(f"  {key:<22} " + "  ".join(cells))
    return {
        "base": base.name,
        "other": other.name,
        "common_images": len(common),
        "groups": out_groups,
        "error_classes": {"base": ea, "other": eb},
    }


def main() -> None:
    ap = argparse.ArgumentParser(description="两次 OCR 评测在同一批图上并排比")
    ap.add_argument("base", type=Path, help="基准的输出目录（一般是 out/default）")
    ap.add_argument("others", nargs="+", type=Path, help="要比的输出目录")
    ap.add_argument("--meta", type=Path, default=Path(__file__).parent / "out" / "meta.json")
    ap.add_argument("--group", action="append", help="按哪一组列（可多次给）；默认 lang_set 和 seal")
    ap.add_argument("--json", type=Path, help="把对比结果写成 JSON")
    args = ap.parse_args()
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    meta = json.loads(args.meta.read_text(encoding="utf-8"))
    group_names = args.group or ["lang_set", "seal"]
    results = [compare(args.base, other, meta, group_names) for other in args.others]
    if args.json:
        args.json.write_text(json.dumps(results, ensure_ascii=False, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
