#!/usr/bin/env python3
"""V11 背景修补原型：把人填好的打分表汇总成每类背景、每种方法的平均分。分是人打的，这里只做算术。

打分表是 sheets.py 出的 scores-template.csv 填好之后的文件：一行一个（样本，方法），后面三列是三位评分人的分（1–5 的整数）。
三个人可以填在同一个文件的三列里，也可以各填一份，一起传进来。没填的格子不算。

用不写方法名的那一套（sheets-blind/）打的分，第二列是 A、B、C……，加 --key sheets-blind/blind-key.csv 换回方法名。

输出：每类背景（样本名第一个字母 p、t、s）每种方法的打分个数、平均分、4 分以上的比例、2 分以下的比例；
同一格有两个以上的分时，另给评分人之间最多差几分的平均值（看几个人打得一不一致）。加 --json 另写一份 JSON。

用法：python score_summary.py <填好的.csv> [更多 ...] [--key blind-key.csv] [--json scores-summary.json]
"""
from __future__ import annotations

import argparse
import csv
from collections import defaultdict
from pathlib import Path
from typing import Any

from common import write_json

CATEGORY_BY_PREFIX = {"p": ("plain", "纯色背景"), "t": ("texture", "底纹背景"), "s": ("seal", "压着红章")}


def read_scores(paths: list[Path], key_path: Path | None) -> dict[tuple[str, str], list[int]]:
    """(样本, 方法) → 所有评分人给的分。"""
    key: dict[tuple[str, str], str] = {}
    if key_path is not None:
        with key_path.open(encoding="utf-8-sig", newline="") as f:
            for row in list(csv.reader(f))[1:]:
                key[(row[0], row[1])] = row[2]
    scores: dict[tuple[str, str], list[int]] = defaultdict(list)
    for path in paths:
        with path.open(encoding="utf-8-sig", newline="") as f:
            for line_no, row in enumerate(list(csv.reader(f))[1:], 2):
                if len(row) < 2 or not row[0].strip():
                    continue
                sid, column = row[0].strip(), row[1].strip()
                method = key.get((sid, column), column) if key else column
                if key and (sid, column) not in key:
                    raise SystemExit(f"{path.name} 第 {line_no} 行：对应表里没有（{sid}，{column}）")
                for cell in row[2:]:
                    cell = cell.strip()
                    if not cell:
                        continue
                    if cell not in {"1", "2", "3", "4", "5"}:
                        raise SystemExit(f"{path.name} 第 {line_no} 行：分数要是 1–5 的整数，这里是 {cell!r}")
                    scores[(sid, method)].append(int(cell))
    return scores


def summarize(scores: dict[tuple[str, str], list[int]]) -> dict[str, Any]:
    groups: dict[str, dict[str, list[list[int]]]] = defaultdict(lambda: defaultdict(list))
    for (sid, method), values in scores.items():
        category = CATEGORY_BY_PREFIX.get(sid[:1], ("other", "其他"))[0]
        groups[category][method].append(values)
        groups["all"][method].append(values)
    result: dict[str, Any] = {}
    for category, by_method in groups.items():
        result[category] = {}
        for method, cells in by_method.items():
            flat = [v for cell in cells for v in cell]
            spreads = [max(cell) - min(cell) for cell in cells if len(cell) >= 2]
            result[category][method] = {
                "samples": len(cells),
                "scores": len(flat),
                "mean": round(sum(flat) / len(flat), 2),
                "share_4_or_more": round(sum(1 for v in flat if v >= 4) / len(flat), 3),
                "share_2_or_less": round(sum(1 for v in flat if v <= 2) / len(flat), 3),
                "mean_spread": round(sum(spreads) / len(spreads), 2) if spreads else None,
            }
    return result


def main() -> None:
    ap = argparse.ArgumentParser(description="汇总填好的主观打分表")
    ap.add_argument("files", type=Path, nargs="+", help="填好的打分表（CSV）")
    ap.add_argument("--key", type=Path, help="不写方法名的那一套的对应表 blind-key.csv")
    ap.add_argument("--json", type=Path, help="汇总结果另写一份 JSON")
    args = ap.parse_args()

    scores = read_scores(args.files, args.key)
    if not scores:
        raise SystemExit("表里一个分都没有填")
    result = summarize(scores)
    names = {key: name for key, name in CATEGORY_BY_PREFIX.values()} | {"all": "三类合计", "other": "其他"}
    for category in ["plain", "texture", "seal", "other", "all"]:
        if category not in result:
            continue
        print(f"\n**{names[category]}**\n")
        print("| 方法 | 打分个数 | 平均分 | 4 分以上 | 2 分以下 | 评分人之间平均差几分 |")
        print("|---|---|---|---|---|---|")
        for method, s in sorted(result[category].items(), key=lambda kv: -kv[1]["mean"]):
            spread = "—" if s["mean_spread"] is None else f"{s['mean_spread']:.2f}"
            print(f"| {method} | {s['scores']} | {s['mean']:.2f} | {s['share_4_or_more'] * 100:.0f}% | {s['share_2_or_less'] * 100:.0f}% | {spread} |")
    if args.json:
        write_json(args.json, {"fmt": "magpdf-v11-scores/1", "files": [p.name for p in args.files], "summary": result})
        print(f"\n写了 {args.json}")


if __name__ == "__main__":
    main()
