#!/usr/bin/env python3
"""V11 背景修补原型：调固定的命令行工具跑 OCR，看目标行还认不认得出来。

两个用处：

1. 对带字的原图跑一遍，取识别出的行框。产品里只有 OCR 给的行框，所以算掩膜时用的是这里得到的框，
   不是造样本时留下的真值框；OCR 没有在目标行的位置找到字时才退回真值框（记在结果里）。
2. 对修补之后的图再跑，看目标行的位置上还能读出多少原文。

命令行工具是 `magpdf-cli ocr <图片目录> --json <输出目录> --models-dir <模型目录>`，一次处理一个目录。

单独运行本脚本：对 50 个样本的带字原图和无字对照图跑 OCR，写 <out>/ocr-input.json。
用法：python ocr.py [--out tests/out/v11] [--cli ...] [--models-dir ...]
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import cv2
import numpy as np
import numpy.typing as npt

from common import (
    DEFAULT_CLI,
    DEFAULT_MODELS_DIR,
    DEFAULT_OUT,
    expand_quad,
    fill_quads,
    imread,
    quad_height,
    quad_to_array,
    read_json,
    write_json,
)


@dataclass
class OcrLine:
    quad: npt.NDArray[np.float32]  # 4×2：左上、右上、右下、左下
    text: str
    confidence: float


def run_cli(images_dir: Path, json_dir: Path, cli: Path, models_dir: Path, log_path: Path) -> float:
    """对一个目录里的图片跑 OCR，每张图在 json_dir 里得到一个同名的 .json。返回用了多少秒。

    工具的输出（每张图一行统计，不含识别出的文字）写进 log_path，不往终端打。
    """
    if not cli.exists():
        raise FileNotFoundError(f"找不到 OCR 命令行工具：{cli}")
    json_dir.mkdir(parents=True, exist_ok=True)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    start = time.perf_counter()
    with log_path.open("w", encoding="utf-8") as log:
        proc = subprocess.run(
            [str(cli), "ocr", str(images_dir), "--json", str(json_dir), "--models-dir", str(models_dir)],
            stdout=log,
            stderr=subprocess.STDOUT,
            check=False,
        )
    if proc.returncode != 0:
        raise RuntimeError(f"OCR 失败（退出码 {proc.returncode}），日志见 {log_path}")
    return time.perf_counter() - start


def load_lines(json_path: Path) -> list[OcrLine]:
    data = read_json(json_path)
    return [
        OcrLine(quad=quad_to_array(line["points"]), text=str(line["text"]), confidence=float(line["confidence"]))
        for line in data["lines"]
    ]


def quad_area(quad: npt.NDArray[np.float32]) -> float:
    return float(cv2.contourArea(quad.reshape(-1, 1, 2)))


def overlap_fraction(inner: npt.NDArray[np.float32], outer: npt.NDArray[np.float32]) -> float:
    """inner 这个框有多大比例落在 outer 里。"""
    area = quad_area(inner)
    if area <= 0:
        return 0.0
    inter, _ = cv2.intersectConvexConvex(inner.reshape(-1, 1, 2), outer.reshape(-1, 1, 2))
    return float(inter) / area


def lines_on_target(lines: list[OcrLine], target: npt.NDArray[np.float32]) -> list[OcrLine]:
    """挑出落在目标行位置上的识别结果，按从左到右排。

    判定：识别出的行框有一半以上落在目标行的框里（目标行的框上下各放宽两成行高，左右各放宽一个行高）。
    邻行的框和目标行隔着行距，够不上一半。
    """
    h = quad_height(target)
    zone = expand_quad(target, pad_x=h, pad_y=0.2 * h)
    hits = [line for line in lines if overlap_fraction(line.quad, zone) >= 0.5]
    hits.sort(key=lambda line: float(line.quad[:, 0].min()))
    return hits


def _strip(text: str) -> str:
    return "".join(text.split())


def similarity(a: str, b: str) -> float:
    """两段文字有多像：1 − 编辑距离 ÷ 较长一段的长度，先去掉空白。两段都是空的算 0。"""
    a, b = _strip(a), _strip(b)
    if not a or not b:
        return 0.0
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return 1.0 - prev[-1] / max(len(a), len(b))


def read_target(json_path: Path, target: npt.NDArray[np.float32], original: str) -> dict[str, Any]:
    """一张图的 OCR 结果里，目标行位置上读出了什么、和原文有多像。"""
    hits = lines_on_target(load_lines(json_path), target)
    text = "".join(line.text for line in hits)
    return {
        "lines": len(hits),
        "chars": len(_strip(text)),
        "similarity": round(similarity(text, original), 4),
        "quads": [[[round(float(x), 1), round(float(y), 1)] for x, y in line.quad] for line in hits],
    }


def link_or_copy(src: Path, dst: Path) -> None:
    """同一个盘上用硬链接，省得把图再写一遍；不行就复制。"""
    dst.parent.mkdir(parents=True, exist_ok=True)
    if dst.exists():
        dst.unlink()
    try:
        os.link(src, dst)
    except OSError:
        shutil.copyfile(src, dst)


def ocr_inputs(out_dir: Path, cli: Path, models_dir: Path) -> dict[str, Any]:
    """对带字原图和无字对照图跑 OCR，写 ocr-input.json。"""
    samples = read_json(out_dir / "samples" / "index.json")["samples"]
    work = out_dir / "ocr" / f"_images-{os.getpid()}"
    result: dict[str, Any] = {"fmt": "magpdf-v11-ocr-input/1", "cli": str(cli), "samples": {}}
    seconds: dict[str, float] = {}
    for name in ("input", "ref"):
        img_dir = work / name
        if img_dir.exists():
            shutil.rmtree(img_dir)
        for m in samples:
            link_or_copy(out_dir / "samples" / m["sid"] / f"{name}.png", img_dir / f"{m['sid']}.png")
        seconds[name] = run_cli(img_dir, out_dir / "ocr" / name, cli, models_dir, out_dir / "ocr" / f"{name}.log")
        shutil.rmtree(img_dir)
    for m in samples:
        target = quad_to_array(m["quad"])
        entry: dict[str, Any] = {
            name: read_target(out_dir / "ocr" / name / f"{m['sid']}.json", target, m["text"]) for name in ("input", "ref")
        }
        # 算掩膜用的行框：OCR 的框（按算掩膜时的幅度放宽）盖住了目标行 95% 以上的字形像素就用它。
        # 没找到目标行、或者只找到一段（压着章的行会这样）时退回真值框，相当于用户自己框选了这一行。
        quads = [quad_to_array(q) for q in entry["input"]["quads"]]
        glyphs = imread(out_dir / "samples" / m["sid"] / "gt_mask.png", gray=True) > 0
        zone = fill_quads(glyphs.shape, [expand_quad(q, 0.3 * quad_height(q), 0.25 * quad_height(q)) for q in quads]) > 0
        cover = float(np.count_nonzero(glyphs & zone)) / max(float(np.count_nonzero(glyphs)), 1.0)
        entry["ocr_cover"] = round(cover, 4)
        if cover >= 0.95:
            entry["box_source"] = "ocr"
            entry["boxes"] = entry["input"]["quads"]
        else:
            entry["box_source"] = "gt-partial" if quads else "gt-none"
            entry["boxes"] = [m["quad"]]
        result["samples"][m["sid"]] = entry
    result["seconds"] = {k: round(v, 1) for k, v in seconds.items()}
    write_json(out_dir / "ocr-input.json", result)
    return result


def main() -> None:
    ap = argparse.ArgumentParser(description="对样本的带字原图和无字对照图跑 OCR")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--cli", type=Path, default=DEFAULT_CLI)
    ap.add_argument("--models-dir", type=Path, default=DEFAULT_MODELS_DIR)
    args = ap.parse_args()
    result = ocr_inputs(args.out, args.cli, args.models_dir)
    entries = result["samples"]
    sims = [e["input"]["similarity"] for e in entries.values()]
    floor = [e["ref"]["similarity"] for e in entries.values()]
    fallback = {sid: e["box_source"] for sid, e in entries.items() if e["box_source"] != "ocr"}
    print(
        f"带字原图：{len(sims)} 行里读对九成以上的 {sum(s >= 0.9 for s in sims)} 行，平均相似度 {np.mean(sims):.3f}；"
        f"无字对照图平均 {np.mean(floor):.3f}；OCR 没找到或没找全目标行、退回真值框的：{fallback or '无'}"
    )


if __name__ == "__main__":
    main()
