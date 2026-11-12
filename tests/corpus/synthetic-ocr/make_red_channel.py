#!/usr/bin/env python3
"""把彩色的合成样本换成“只取红色通道”的灰度图，试一试红章压字的预处理（V09 报告 4.6）。

红色印章在红色通道里接近白色，黑字还是黑的；取红色通道相当于把红章从图上拿掉。这里只是在评测
之前改图，看这样处理之后被印章压住的行能多认出多少，引擎里没有这一步。

从 out/images/ 里挑出彩色的变体（clean、rot 的 300 dpi，和 photo_12mp），处理后写到 out/images-red/，
文件名不变，另写一份只含这些图的 Label.txt。之后：
  pwsh run_eval.ps1 -Name redch -Set images-red
  python analyze.py out/redch

用法：python make_red_channel.py [--out out] [--variants clean_300 rot_300 photo_12mp] [--jobs 4]
"""
from __future__ import annotations

import argparse
import os
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

from PIL import Image


def convert(job: tuple[str, str]) -> None:
    src, dst = job
    with Image.open(src) as img:
        red = img.convert("RGB").getchannel("R")
        if dst.lower().endswith((".jpg", ".jpeg")):
            red.save(dst, quality=95)  # 照片已经压过一次，这里用高质量，尽量不再损失
        else:
            red.save(dst, compress_level=3)


def main() -> None:
    ap = argparse.ArgumentParser(description="把彩色样本换成只取红色通道的灰度图")
    ap.add_argument("--out", type=Path, default=Path(__file__).parent / "out")
    ap.add_argument("--variants", nargs="+", default=["clean_300", "rot_300", "photo_12mp"],
                    help="图像主干名的结尾（变体_分辨率）")
    ap.add_argument("--jobs", type=int, default=min(4, os.cpu_count() or 1))
    args = ap.parse_args()
    src_dir, dst_dir = args.out / "images", args.out / "images-red"
    dst_dir.mkdir(parents=True, exist_ok=True)
    picked = sorted(p for p in src_dir.iterdir() if p.is_file() and any(p.stem.endswith(f"_{v}") for v in args.variants))
    if not picked:
        raise SystemExit(f"{src_dir} 里没有这些变体的图，先运行 gen_synthetic.py")
    jobs = [(str(p), str(dst_dir / p.name)) for p in picked]
    if args.jobs > 1:
        with ProcessPoolExecutor(max_workers=args.jobs) as pool:
            list(pool.map(convert, jobs, chunksize=4))
    else:
        for job in jobs:
            convert(job)
    names = {p.name for p in picked}
    lines = [line for line in (args.out / "Label.txt").read_text(encoding="utf-8").split("\n")
             if line.split("\t", 1)[0].rsplit("/", 1)[-1] in names]
    (dst_dir / "Label.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{len(picked)} 张图写入 {dst_dir}")


if __name__ == "__main__":
    main()
