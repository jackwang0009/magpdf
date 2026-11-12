#!/usr/bin/env python3
"""把合成样本二值化成黑白两色，看二值化对识别的影响（V09 报告 4.6）。

计划 V09 说未达标时可以调整预处理，其中一项是二值化；黑白二值的扫描件本身也是常见的一种输入。
这里只是在评测之前改图，引擎里没有这一步。

做法：转成灰度，整页定一个阈值，比它暗的像素变黑，其余变白。阈值默认用大津法（让黑、白两类
之间的方差最大）按每张图的直方图算，也可以用 --threshold 给一个固定的值。红色印章按亮度是深灰，
二值化之后和黑字一样黑。整页一个阈值对付不了光照不匀，所以默认只处理扫描的变体，不处理仿拍照的。

从 out/images/ 里挑图，处理后写到 out/images-bin/（8 位灰度的 PNG，只有 0 和 255 两个值），文件名不变，
另写一份只含这些图的 Label.txt。之后：
  pwsh run_eval.ps1 -Name bin -Set images-bin
  python compare_runs.py out/default out/bin --group lang_variant

用法：python make_binary.py [--out out] [--variants clean_300 noise_300] [--threshold 128] [--jobs 4]
"""
from __future__ import annotations

import argparse
import os
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

from PIL import Image


def otsu_threshold(histogram: list[int]) -> int:
    """大津法：返回阈值 t，灰度值小于 t 的算黑。"""
    total = sum(histogram)
    sum_all = sum(i * n for i, n in enumerate(histogram))
    best_t, best_var = 128, -1.0
    count_dark, sum_dark = 0, 0
    for t in range(1, 256):
        count_dark += histogram[t - 1]
        sum_dark += (t - 1) * histogram[t - 1]
        count_light = total - count_dark
        if count_dark == 0 or count_light == 0:
            continue
        mean_dark = sum_dark / count_dark
        mean_light = (sum_all - sum_dark) / count_light
        between = count_dark * count_light * (mean_dark - mean_light) ** 2
        if between > best_var:
            best_t, best_var = t, between
    return best_t


def convert(job: tuple[str, str, int | None]) -> int:
    src, dst, fixed = job
    with Image.open(src) as img:
        gray = img.convert("L")
        t = fixed if fixed is not None else otsu_threshold(gray.histogram())
        gray.point([0 if v < t else 255 for v in range(256)]).save(dst, compress_level=3)
    return t


def main() -> None:
    ap = argparse.ArgumentParser(description="把样本二值化成黑白两色")
    ap.add_argument("--out", type=Path, default=Path(__file__).parent / "out")
    ap.add_argument("--variants", nargs="+", default=["clean_300", "noise_300"], help="图像主干名的结尾（变体_分辨率）")
    ap.add_argument("--threshold", type=int, help="固定的阈值（1–255）；不给就每张图用大津法算")
    ap.add_argument("--jobs", type=int, default=min(4, os.cpu_count() or 1))
    args = ap.parse_args()
    if args.threshold is not None and not 1 <= args.threshold <= 255:
        raise SystemExit("--threshold 要在 1 到 255 之间")
    src_dir, dst_dir = args.out / "images", args.out / "images-bin"
    dst_dir.mkdir(parents=True, exist_ok=True)
    picked = sorted(
        p for p in src_dir.iterdir()
        if p.is_file() and p.suffix.lower() == ".png" and any(p.stem.endswith(f"_{v}") for v in args.variants)
    )
    if not picked:
        raise SystemExit(f"{src_dir} 里没有这些变体的 PNG，先运行 gen_synthetic.py")
    jobs = [(str(p), str(dst_dir / p.name), args.threshold) for p in picked]
    if args.jobs > 1:
        with ProcessPoolExecutor(max_workers=args.jobs) as pool:
            thresholds = list(pool.map(convert, jobs, chunksize=4))
    else:
        thresholds = [convert(job) for job in jobs]
    names = {p.name for p in picked}
    lines = [line for line in (args.out / "Label.txt").read_text(encoding="utf-8").split("\n")
             if line.split("\t", 1)[0].rsplit("/", 1)[-1] in names]
    (dst_dir / "Label.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    ordered = sorted(thresholds)
    print(f"{len(picked)} 张图写入 {dst_dir}；阈值最小 {ordered[0]}，中位 {ordered[len(ordered) // 2]}，最大 {ordered[-1]}")


if __name__ == "__main__":
    main()
