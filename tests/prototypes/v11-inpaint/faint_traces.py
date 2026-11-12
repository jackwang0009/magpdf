#!/usr/bin/env python3
"""V11 背景修补原型：量“差得不多、但连成字形”的浅痕。一条命令：python tests/prototypes/v11-inpaint/faint_traces.py

evaluate.py 的“超过阈值的像素”只数和无字对照图差了 24 以上的像素。有一种痕迹它数不到：补上去的颜色和周围的纸只差
几级到十几级，但连成一片，形状就是原来那行字，人看得出来（fill-local 在带装订阴影的 p14 上就是这样，那一项是 0.0%）。
这里换一种量法把它补上：

  结果和无字对照图的亮度差（带符号）先用 σ = --sigma（默认 2）像素的高斯抹平。噪声每个像素各不相同，抹平之后
  剩不到 1 级；连成片的色差抹不掉。再数抹平之后绝对值超过 --level（默认 6）的像素，在评价范围里占多少。
  评价范围和 evaluate.py 一样：修补用的掩膜，并上看得见的字形像素。

“噪声底”（ref_nonoise 对 ref）按同样的量法也算一遍，应当是 0：用来确认这个量法没有把噪声算成痕迹。

两点限制：
- 这个数只说明“有没有连成片的色差”，不说明人看着能不能接受，代替不了主观分。
- 它适合没有细密图案的背景（纯色、渐变、水印字）。规则的网纹、点阵上，补出来的图案只要和对照图错开一两个像素，
  抹平之后也剩得下差，这个数就高，那不一定是看得出来的痕迹；这两类背景上的数只能和别的方法比着看。

要先跑过 run_all.py（用它的样本、掩膜和各方法的结果）。结果打在屏幕上，--json 另存一份。

用法：python faint_traces.py [--out tests/out/v11] [--mask auto-d3] [--methods 方法 ...] [--level 6] [--sigma 2]
                            [--each] [--json 文件]
"""
from __future__ import annotations

import argparse
from pathlib import Path
from typing import Any

import cv2
import numpy as np
import numpy.typing as npt

from common import DEFAULT_OUT, Image, imread, read_json, write_json
from evaluate import VISIBLE_ALPHA

DEFAULT_METHODS = ["fill-flat", "fill-local", "telea-r3", "fsr-fast", "shiftmap", "lama"]
CATEGORIES = [("plain", "纯色背景"), ("texture", "底纹背景"), ("seal", "压着红章")]


def faint_fraction(result: Image, ref: Image, region: npt.NDArray[np.bool_], level: float, sigma: float) -> float:
    """region 里，抹平之后的亮度差超过 level 的像素占多少。"""
    luma_result = cv2.cvtColor(result, cv2.COLOR_BGR2GRAY).astype(np.float32)
    luma_ref = cv2.cvtColor(ref, cv2.COLOR_BGR2GRAY).astype(np.float32)
    smooth = np.abs(cv2.GaussianBlur(luma_result - luma_ref, (0, 0), sigma))
    return float(np.mean(smooth[region] > level))


def measure(out_dir: Path, mask_key: str, methods: list[str], level: float, sigma: float) -> dict[str, Any]:
    samples = read_json(out_dir / "samples" / "index.json")["samples"]
    available: list[str] = []
    for method in methods:
        if (out_dir / "results" / mask_key / method / "index.json").exists():
            available.append(method)
        else:
            print(f"没有 {mask_key}/{method} 的结果，跳过")
    indexes = {m: read_json(out_dir / "results" / mask_key / m / "index.json")["samples"] for m in available}
    rows: list[dict[str, Any]] = []
    for meta in samples:
        sid = meta["sid"]
        sample_dir = out_dir / "samples" / sid
        img = imread(sample_dir / "input.png")
        ref = imread(sample_dir / "ref.png")
        mask = imread(out_dir / "maskset" / mask_key / f"{sid}.png", gray=True)
        region = (mask > 0) | (imread(sample_dir / "gt_alpha.png", gray=True) >= VISIBLE_ALPHA)
        values = {"floor": faint_fraction(imread(sample_dir / "ref_nonoise.png"), ref, region, level, sigma)}
        for method in available:
            x0, y0, x1, y1 = indexes[method][sid]["rect"]
            full = img.copy()
            full[y0:y1, x0:x1] = imread(out_dir / "results" / mask_key / method / f"{sid}.png")
            values[method] = faint_fraction(full, ref, region, level, sigma)
        rows.append(
            {
                "sid": sid,
                "category": meta["category"],
                "tag": meta["tag"],
                "faint": {name: round(value, 4) for name, value in values.items()},
            }
        )
    return {
        "fmt": "magpdf-v11-faint/1",
        "mask": mask_key,
        "level": level,
        "sigma": sigma,
        "methods": ["floor", *available],
        "samples": rows,
    }


def summarize(rows: list[dict[str, Any]], method: str) -> str:
    """这些样本上的平均，括号里是最差的那个样本。"""
    values = [(row["faint"][method], row["sid"]) for row in rows]
    worst, worst_sid = max(values)
    mean = float(np.mean([value for value, _ in values]))
    return f"{mean * 100:5.1f}%（{worst * 100:4.1f}% {worst_sid}）"


def print_tables(result: dict[str, Any], each: bool) -> None:
    methods: list[str] = result["methods"]
    rows: list[dict[str, Any]] = result["samples"]
    print(
        f"掩膜 {result['mask']}；亮度差用 σ={result['sigma']:g} 的高斯抹平之后超过 {result['level']:g} 的像素占评价范围的多少。"
        "每格是各样本的平均，括号里是最差的样本。floor 是噪声底。"
    )
    header = "".ljust(14) + "".join(name.rjust(22) for name in methods)
    for category, label in CATEGORIES:
        subset = [row for row in rows if row["category"] == category]
        if not subset:
            continue
        print(f"\n{label}（{len(subset)} 行）")
        print(header)
        print("全部".ljust(12) + "".join(summarize(subset, m).rjust(22) for m in methods))
        for tag in dict.fromkeys(row["tag"] for row in subset):
            tagged = [row for row in subset if row["tag"] == tag]
            print(f"{tag}（{len(tagged)}）".ljust(14) + "".join(summarize(tagged, m).rjust(22) for m in methods))
        if each:
            for row in subset:
                cells = "".join(f"{row['faint'][m] * 100:21.1f}%" for m in methods)
                print(f"  {row['sid']} {row['tag']}".ljust(14) + cells)


def main() -> None:
    parser = argparse.ArgumentParser(description="量差得不多、但连成字形的浅痕")
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT, help="run_all.py 的输出目录")
    parser.add_argument("--mask", default="auto-d3", help="用哪一套掩膜的结果")
    parser.add_argument("--methods", nargs="+", default=DEFAULT_METHODS, help="看哪几种方法")
    parser.add_argument("--level", type=float, default=6.0, help="抹平之后的亮度差超过多少算痕迹（0–255）")
    parser.add_argument("--sigma", type=float, default=2.0, help="抹平用的高斯的 σ（像素）")
    parser.add_argument("--each", action="store_true", help="把每个样本的数也打出来")
    parser.add_argument("--json", type=Path, default=None, help="结果另存成 JSON")
    args = parser.parse_args()

    result = measure(args.out, args.mask, args.methods, args.level, args.sigma)
    print_tables(result, args.each)
    if args.json is not None:
        write_json(args.json, result)
        print(f"\n写了 {args.json}")


if __name__ == "__main__":
    main()
