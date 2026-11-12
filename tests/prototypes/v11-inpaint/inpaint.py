#!/usr/bin/env python3
"""V11 背景修补原型：各种修补方法，对 50 个样本各跑一遍（方案 6.4）。

方法（--list 看全部）：

  fill-flat         用周围的底色直接填：整行取一个颜色（掩膜外面像素的中位数）
  fill-local        用周围的底色直接填：每个像素取它附近的底色（盖掉字之后做两遍大窗口中值），底色不匀、渐变时跟得上
  fill-local-grain  同上，再按纸面颗粒的大小补上噪声，免得填过的地方比周围光滑
  telea-r{1,3,5,9}  cv2.inpaint 的 Telea 算法（快速行进法），半径 1、3、5、9
  ns-r{1,3,5,9}     cv2.inpaint 的 Navier-Stokes 算法，半径 1、3、5、9
  fsr-fast、fsr-best、shiftmap
                    OpenCV contrib 的 xphoto 模块里另外三种传统修复算法（频率选择重建的快、慢两档，移位图）。
                    要装 opencv-contrib-python-headless；当前环境里没有 cv2.xphoto 时自动跳过
  lama              LaMa 模型（ONNX，CPU），沿着这一行排 512×512 的窗口；没有模型文件或 onnxruntime 时自动跳过

掩膜（--masks）的名字是“来源-d扩几个像素”：auto 是 masks.py 从 OCR 行框里算出来的，gt 是真值掩膜，
gray 是不分颜色的灰度二值化（只为了和 auto 比，说明压着章的行为什么要按颜色分）。

所有方法都只改掩膜里的像素，掩膜外面一个像素也不动。每个结果存成“补丁”：掩膜外接矩形（四周让出 8 像素）
那一块修补之后的样子，和产品里把补丁图像叠在原图上的做法一样（方案 6.4）。

输出：<out>/results/<掩膜>/<方法>/<编号>.png 和 index.json（补丁的位置、耗时）；用到的掩膜在 <out>/maskset/<掩膜>/。

用法：python inpaint.py [--out tests/out/v11] [--masks auto-d3 gt-d3] [--methods telea-r3 ...] [--families fill opencv xphoto lama]
"""
from __future__ import annotations

import argparse
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

import cv2
import numpy as np

from common import DEFAULT_LAMA_MODEL, DEFAULT_OUT, Image, Mask, bounding_rect, dilate, imread, imwrite, read_json, write_json
from masks import DEFAULT_DILATE

PATCH_PAD = 8


def _odd(value: float) -> int:
    n = int(round(value))
    return n if n % 2 == 1 else n + 1


def _work_rect(mask: Mask, context: int) -> tuple[int, int, int, int]:
    return bounding_rect(mask, pad=context)


def fill_flat(img: Image, mask: Mask) -> Image:
    x0, y0, x1, y1 = _work_rect(mask, 24)
    crop, m = img[y0:y1, x0:x1], mask[y0:y1, x0:x1] > 0
    color = np.median(crop[~m].reshape(-1, 3), axis=0)
    out = img.copy()
    out[y0:y1, x0:x1][m] = np.round(color).astype(np.uint8)
    return out


def _local_background(crop: Image, m: np.ndarray) -> Image:
    """每个像素附近的底色：先用整块的中位数盖掉掩膜里的像素，做大窗口中值；再用得到的结果盖一遍、再做一次。"""
    k = _odd(float(np.clip(0.6 * np.count_nonzero(m.any(axis=1)), 21, 61)))
    est = crop.copy()
    est[m] = np.round(np.median(crop[~m].reshape(-1, 3), axis=0)).astype(np.uint8)
    for _ in range(2):
        local = cv2.medianBlur(est, k)
        est = crop.copy()
        est[m] = local[m]
    return cv2.medianBlur(est, k)


def fill_local(img: Image, mask: Mask) -> Image:
    x0, y0, x1, y1 = _work_rect(mask, 40)
    crop, m = img[y0:y1, x0:x1], mask[y0:y1, x0:x1] > 0
    out = img.copy()
    out[y0:y1, x0:x1][m] = _local_background(crop, m)[m]
    return out


def fill_local_grain(img: Image, mask: Mask) -> Image:
    x0, y0, x1, y1 = _work_rect(mask, 40)
    crop, m = img[y0:y1, x0:x1], mask[y0:y1, x0:x1] > 0
    base = _local_background(crop, m).astype(np.float32)
    # 纸面颗粒有多大：掩膜外面的像素减去自己的平滑版本，取绝对值的中位数（不怕里面夹着别的字和线条）
    gray = cv2.cvtColor(crop, cv2.COLOR_BGR2GRAY).astype(np.float32)
    residual = gray - cv2.GaussianBlur(gray, (0, 0), 1.5)
    sigma = 1.4826 * float(np.median(np.abs(residual[~m]))) / 0.93
    rng = np.random.default_rng(int(np.count_nonzero(m)))  # 种子取掩膜的像素数，结果可以复现
    grain = rng.normal(0.0, sigma, m.shape).astype(np.float32)
    out = img.copy()
    filled = np.clip(np.round(base + grain[..., None]), 0, 255).astype(np.uint8)
    out[y0:y1, x0:x1][m] = filled[m]
    return out


def _cv_inpaint(radius: float, flag: int) -> Callable[[Image, Mask], Image]:
    def run(img: Image, mask: Mask) -> Image:
        x0, y0, x1, y1 = _work_rect(mask, 32)
        out = img.copy()
        out[y0:y1, x0:x1] = cv2.inpaint(np.ascontiguousarray(img[y0:y1, x0:x1]), np.ascontiguousarray(mask[y0:y1, x0:x1]), radius, flag)
        return out

    return run


def _xphoto(algorithm: str) -> Callable[[Image, Mask], Image]:
    def run(img: Image, mask: Mask) -> Image:
        xphoto = cv2.xphoto  # type: ignore[attr-defined]
        x0, y0, x1, y1 = _work_rect(mask, 48)
        crop = np.ascontiguousarray(img[y0:y1, x0:x1])
        # xphoto 的掩膜反过来：非零的是好像素，零是要补的
        valid = np.where(mask[y0:y1, x0:x1] > 0, 0, 255).astype(np.uint8)
        dst = np.zeros_like(crop)
        if algorithm == "shiftmap":
            # 移位图要求第一个通道是亮度，这里转成 Lab 再转回来
            lab = cv2.cvtColor(crop, cv2.COLOR_BGR2Lab)
            xphoto.inpaint(lab, valid, dst, xphoto.INPAINT_SHIFTMAP)
            dst = cv2.cvtColor(dst, cv2.COLOR_Lab2BGR)
        else:
            flag = xphoto.INPAINT_FSR_FAST if algorithm == "fsr-fast" else xphoto.INPAINT_FSR_BEST
            xphoto.inpaint(crop, valid, dst, flag)
        out = img.copy()
        out[y0:y1, x0:x1] = dst
        return out

    return run


@dataclass(frozen=True)
class Method:
    key: str
    family: str  # fill / opencv / xphoto / lama
    label: str  # 对照图上的标签
    run: Callable[[Image, Mask], Image] | None  # lama 的在运行时才建


METHODS: list[Method] = [
    Method("fill-flat", "fill", "底色填充（整行一色）", fill_flat),
    Method("fill-local", "fill", "底色填充（就近取色）", fill_local),
    Method("fill-local-grain", "fill", "底色填充（就近取色＋颗粒）", fill_local_grain),
    *[Method(f"telea-r{r}", "opencv", f"Telea 半径 {r}", _cv_inpaint(r, cv2.INPAINT_TELEA)) for r in (1, 3, 5, 9)],
    *[Method(f"ns-r{r}", "opencv", f"Navier-Stokes 半径 {r}", _cv_inpaint(r, cv2.INPAINT_NS)) for r in (1, 3, 5, 9)],
    Method("fsr-fast", "xphoto", "FSR（快）", _xphoto("fsr-fast")),
    Method("fsr-best", "xphoto", "FSR（慢）", _xphoto("fsr-best")),
    Method("shiftmap", "xphoto", "移位图", _xphoto("shiftmap")),
    Method("lama", "lama", "LaMa", None),
]
METHOD_BY_KEY = {m.key: m for m in METHODS}


def build_mask(out_dir: Path, sid: str, mask_key: str) -> Mask:
    """按名字取掩膜：来源（auto / gt / gray）加上往外扩的像素数，例如 auto-d3。"""
    source, _, d = mask_key.partition("-d")
    radius = int(d)
    if source == "gt":
        raw = imread(out_dir / "samples" / sid / "gt_mask.png", gray=True)
    else:
        name = {"auto": read_json(out_dir / "masks.json")["auto_method"], "gray": "gray-otsu"}[source]
        raw = imread(out_dir / "masks" / sid / f"{name}.png", gray=True)
    return dilate(np.where(raw > 0, 255, 0).astype(np.uint8), radius)


def run(
    out_dir: Path,
    mask_keys: list[str],
    methods: list[Method],
    lama_model: Path,
    lama_threads: int,
) -> dict[str, Any]:
    samples = read_json(out_dir / "samples" / "index.json")["samples"]
    report: dict[str, Any] = {"ran": [], "skipped": {}}

    runners: dict[str, Callable[[Image, Mask], Image]] = {}
    lama: Any = None
    for method in methods:
        if method.family == "xphoto" and not hasattr(cv2, "xphoto"):
            report["skipped"][method.key] = "当前环境的 OpenCV 没有 xphoto 模块（要 opencv-contrib-python-headless）"
        elif method.family == "lama":
            try:
                from lama import Lama

                lama = Lama(lama_model, lama_threads)
                runners[method.key] = lama.inpaint
                report["lama_init_seconds"] = round(lama.init_seconds, 2)
            except (ImportError, OSError) as err:
                report["skipped"][method.key] = f"LaMa 跑不起来：{err}"
        elif method.run is not None:
            runners[method.key] = method.run

    for mask_key in mask_keys:
        masks = {m["sid"]: build_mask(out_dir, m["sid"], mask_key) for m in samples}
        for sid, mask in masks.items():
            imwrite(out_dir / "maskset" / mask_key / f"{sid}.png", mask)
        for key, runner in runners.items():
            index: dict[str, Any] = {"mask": mask_key, "method": key, "samples": {}}
            for m in samples:
                sid = m["sid"]
                img = imread(out_dir / "samples" / sid / "input.png")
                mask = masks[sid]
                start = time.perf_counter()
                result = runner(img, mask)
                ms = (time.perf_counter() - start) * 1000.0
                # 掩膜外面的像素不许动
                result = np.where(mask[..., None] > 0, result, img)
                x0, y0, x1, y1 = bounding_rect(mask, pad=PATCH_PAD)
                imwrite(out_dir / "results" / mask_key / key / f"{sid}.png", result[y0:y1, x0:x1])
                entry: dict[str, Any] = {"rect": [x0, y0, x1, y1], "ms": round(ms, 2), "mask_pixels": int(np.count_nonzero(mask))}
                if key == "lama" and lama is not None:
                    entry["windows"] = lama.last_windows  # 这一行用了几次 512×512 的推理
                index["samples"][sid] = entry
            write_json(out_dir / "results" / mask_key / key / "index.json", index)
            times = [s["ms"] for s in index["samples"].values()]
            report["ran"].append({"mask": mask_key, "method": key, "median_ms": round(float(np.median(times)), 1), "max_ms": round(float(np.max(times)), 1)})
            print(f"{mask_key:8s} {key:17s} 每行中位 {np.median(times):8.1f} ms，最长 {np.max(times):8.1f} ms", flush=True)
    return report


def main() -> None:
    ap = argparse.ArgumentParser(description="用各种方法修补 50 个样本")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--masks", nargs="+", default=[f"auto-d{DEFAULT_DILATE}", f"gt-d{DEFAULT_DILATE}"])
    ap.add_argument("--methods", nargs="+", help="只跑这几种方法")
    ap.add_argument("--families", nargs="+", choices=["fill", "opencv", "xphoto", "lama"], help="只跑这几类方法")
    ap.add_argument("--lama-model", type=Path, default=DEFAULT_LAMA_MODEL)
    ap.add_argument("--lama-threads", type=int, default=0, help="LaMa 的线程数，0 是 ONNX Runtime 的默认值")
    ap.add_argument("--report", type=Path, help="把跑了哪些、跳过了哪些写到这个 JSON")
    ap.add_argument("--list", action="store_true", help="列出全部方法")
    args = ap.parse_args()
    if args.list:
        for m in METHODS:
            print(f"{m.key:17s} {m.family:7s} {m.label}")
        return
    methods = [m for m in METHODS if (not args.methods or m.key in args.methods) and (not args.families or m.family in args.families)]
    report = run(args.out, args.masks, methods, args.lama_model, args.lama_threads)
    for key, why in report["skipped"].items():
        print(f"跳过 {key}：{why}")
    if args.report:
        write_json(args.report, report)


if __name__ == "__main__":
    main()
