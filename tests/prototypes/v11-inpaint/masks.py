#!/usr/bin/env python3
"""V11 背景修补原型：从行框里把字的像素分出来（方案 6.4“先按颜色区分文字笔画和印章”）。

产品里只有 OCR 给的行框，没有真值掩膜，所以这里只用图像和行框：

  1. 把行框沿自己的方向往外放宽一圈，转正成一个矩形（歪斜的行也当成水平的来处理）；
  2. 每个像素取三个通道里最大的那个值（下面叫 V）。黑字、深灰字、深蓝字的 V 低；白纸、彩色纸、红章、
     浅色底纹的 V 高，所以红章在 V 里几乎看不见，字压在章上也照样分得出来；
  3. 对 V 做一次闭运算（核比笔画宽、比行高小），得到“假如没有字，这里的底有多亮”；
  4. 行框里最暗的那一档当作字的亮度；一个像素从底到字的路上走过了一半就算字的像素；
  5. 去掉很长的横线和上下贯穿的竖线（表格线、填写线），去掉不碰行框本体的碎块（邻行伸进来的笔画）；
  6. 转回原图的坐标。往外扩几个像素（盖住笔画边上发虚的一圈）留给修补那一步做。

另外实现了两种对照的二值化，只为了和上面的比：
  gray-otsu  灰度图加大津法（红章在灰度图里是深色的，会被当成字）；
  v-otsu     V 加大津法（整行用一个阈值）。

单独运行：对 50 个样本算三种掩膜，和真值掩膜比查全、查准，写 <out>/masks/<编号>/*.png 和 <out>/masks.json。
用法：python masks.py [--out tests/out/v11]
"""
from __future__ import annotations

import argparse
from pathlib import Path
from typing import Any

import cv2
import numpy as np
import numpy.typing as npt

from common import DEFAULT_OUT, Image, Mask, dilate, expand_quad, imread, imwrite, quad_height, quad_to_array, read_json, write_json

METHODS = ("v-local", "v-otsu", "gray-otsu")
AUTO_METHOD = "v-local"  # 修补用的就是这一种
# 默认往外扩几个像素（300 dpi）。修补和评价都用这个数；evaluate.py 里另外试了几个别的值。
DEFAULT_DILATE = 3


def _odd(value: float) -> int:
    n = int(round(value))
    return n if n % 2 == 1 else n + 1


def _otsu(values: npt.NDArray[np.uint8]) -> float:
    if values.size == 0:
        return 0.0
    t, _ = cv2.threshold(values.reshape(-1, 1), 0, 255, cv2.THRESH_BINARY + cv2.THRESH_OTSU)
    return float(t)


def _binarize(rect: Image, core: tuple[slice, slice], line_h: float, method: str) -> Mask:
    """在转正之后的矩形里把字的像素分出来。core 是行框本体（没放宽的部分）在矩形里的位置。"""
    if method == "gray-otsu":
        gray = cv2.cvtColor(rect, cv2.COLOR_BGR2GRAY)
        return np.where(gray <= _otsu(gray), 255, 0).astype(np.uint8)
    v = rect.max(axis=2)
    if method == "v-otsu":
        return np.where(v <= _otsu(v), 255, 0).astype(np.uint8)
    if method != "v-local":
        raise ValueError(f"不认识的二值化方法：{method}")
    k = _odd(max(15.0, 0.7 * line_h))
    background = cv2.morphologyEx(v, cv2.MORPH_CLOSE, cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (k, k)))
    # 字有多暗：行框本体里最暗的 3% 像素的上沿。字占行框的面积一般在 5% 以上。
    text_level = float(np.percentile(v[core], 3))
    contrast = np.maximum(background.astype(np.float32) - text_level, 24.0)
    depth = (background.astype(np.float32) - v.astype(np.float32)) / contrast
    return np.where(depth >= 0.5, 255, 0).astype(np.uint8)


def _drop_rules(ink: Mask, line_h: float) -> Mask:
    """去掉表格线和填写线：比两个半行高还长的横线，从上到下贯穿整个矩形的竖线。"""
    h, w = ink.shape
    out = ink.copy()
    long_h = int(round(2.5 * line_h))
    if w > long_h:
        horiz = cv2.morphologyEx(ink, cv2.MORPH_OPEN, cv2.getStructuringElement(cv2.MORPH_RECT, (long_h, 1)))
        out[cv2.dilate(horiz, np.ones((3, 1), np.uint8)) > 0] = 0
    vert = cv2.morphologyEx(ink, cv2.MORPH_OPEN, cv2.getStructuringElement(cv2.MORPH_RECT, (1, max(h - 2, 3))))
    out[cv2.dilate(vert, np.ones((1, 3), np.uint8)) > 0] = 0
    return out


def _keep_touching_core(ink: Mask, core: tuple[slice, slice]) -> Mask:
    """只留碰到行框本体的连通块：邻行伸进放宽区的笔画、表格线去掉之后剩下的碎块都不要。"""
    count, labels = cv2.connectedComponents(ink, connectivity=8)
    if count <= 1:
        return ink
    keep = np.zeros(count, dtype=bool)
    keep[np.unique(labels[core])] = True
    keep[0] = False
    return np.where(keep[labels], 255, 0).astype(np.uint8)


def segment_text(img: Image, quads: list[npt.NDArray[np.float32]], method: str = AUTO_METHOD) -> Mask:
    """从若干个行框里分出字的像素。返回和原图一样大的掩膜，没有往外扩。"""
    h_img, w_img = img.shape[:2]
    mask = np.zeros((h_img, w_img), dtype=np.uint8)
    for quad in quads:
        line_h = quad_height(quad)
        pad_x = max(3, int(round(0.3 * line_h)))
        pad_y = max(3, int(round(0.25 * line_h)))
        q = expand_quad(quad, pad_x, pad_y)
        w = max(int(round(float(np.linalg.norm(q[1] - q[0])))), 2 * pad_x + 1)
        h = max(int(round(float(np.linalg.norm(q[3] - q[0])))), 2 * pad_y + 1)
        src = np.asarray([q[0], q[1], q[3]], dtype=np.float32)
        dst = np.asarray([[0, 0], [w, 0], [0, h]], dtype=np.float32)
        forward = cv2.getAffineTransform(src, dst)
        rect = cv2.warpAffine(img, forward, (w, h), flags=cv2.INTER_LINEAR, borderMode=cv2.BORDER_REPLICATE)
        core = (slice(pad_y, h - pad_y), slice(pad_x, w - pad_x))
        ink = _binarize(np.asarray(rect, dtype=np.uint8), core, line_h, method)
        ink = _keep_touching_core(_drop_rules(ink, line_h), core)
        back = cv2.warpAffine(ink, cv2.invertAffineTransform(forward), (w_img, h_img), flags=cv2.INTER_LINEAR, borderValue=0)
        mask[back >= 128] = 255
    return mask


def compare(mask: Mask, truth: Mask) -> dict[str, float]:
    """查准、查全、交并比。"""
    m, t = mask > 0, truth > 0
    inter = float(np.count_nonzero(m & t))
    return {
        "precision": round(inter / max(float(np.count_nonzero(m)), 1.0), 4),
        "recall": round(inter / max(float(np.count_nonzero(t)), 1.0), 4),
        "iou": round(inter / max(float(np.count_nonzero(m | t)), 1.0), 4),
    }


def run(out_dir: Path) -> dict[str, Any]:
    samples = read_json(out_dir / "samples" / "index.json")["samples"]
    boxes = read_json(out_dir / "ocr-input.json")["samples"]
    result: dict[str, Any] = {"fmt": "magpdf-v11-masks/1", "auto_method": AUTO_METHOD, "dilate": DEFAULT_DILATE, "samples": {}}
    for m in samples:
        sid = m["sid"]
        sample_dir = out_dir / "samples" / sid
        img = imread(sample_dir / "input.png")
        gt_core = imread(sample_dir / "gt_mask.png", gray=True)
        # 肉眼看得出的那一圈也算上：模糊之后变暗一成以上的像素
        gt_visible = np.where(imread(sample_dir / "gt_alpha.png", gray=True) >= 26, 255, 0).astype(np.uint8)
        gt_near = dilate(gt_core, DEFAULT_DILATE + 1)
        quads = [quad_to_array(q) for q in boxes[sid]["boxes"]]
        entry: dict[str, Any] = {"box_source": boxes[sid]["box_source"], "methods": {}}
        for method in METHODS:
            raw = segment_text(img, quads, method)
            final = dilate(raw, DEFAULT_DILATE)
            stats = compare(raw, gt_core)
            f, vis, near = final > 0, gt_visible > 0, gt_near > 0
            # 扩完之后：看得出的字形像素盖住了多少；掩膜里有多少落在字的近旁（其余的是误伤）
            stats["final_recall_visible"] = round(float(np.count_nonzero(f & vis)) / max(float(np.count_nonzero(vis)), 1.0), 4)
            stats["final_precision_near"] = round(float(np.count_nonzero(f & near)) / max(float(np.count_nonzero(f)), 1.0), 4)
            stats["final_pixels"] = int(np.count_nonzero(f))
            entry["methods"][method] = stats
            imwrite(out_dir / "masks" / sid / f"{method}.png", raw)
        result["samples"][sid] = entry
    write_json(out_dir / "masks.json", result)
    return result


def summarize(result: dict[str, Any], samples: list[dict[str, Any]]) -> list[str]:
    lines = []
    cats = {m["sid"]: m["category"] for m in samples}
    for method in METHODS:
        for cat in ("plain", "texture", "seal"):
            rows = [e["methods"][method] for sid, e in result["samples"].items() if cats[sid] == cat]
            lines.append(
                f"{method:10s} {cat:8s} 查准 {np.mean([r['precision'] for r in rows]):.3f}  查全 {np.mean([r['recall'] for r in rows]):.3f}"
                f"  IoU {np.mean([r['iou'] for r in rows]):.3f}  扩{DEFAULT_DILATE}px后：盖住看得见的字 {np.mean([r['final_recall_visible'] for r in rows]):.3f}"
                f"（最差 {np.min([r['final_recall_visible'] for r in rows]):.3f}）  落在字近旁 {np.mean([r['final_precision_near'] for r in rows]):.3f}"
                f"（最差 {np.min([r['final_precision_near'] for r in rows]):.3f}）"
            )
    return lines


def main() -> None:
    ap = argparse.ArgumentParser(description="从行框里分出字的像素，和真值掩膜比")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = ap.parse_args()
    result = run(args.out)
    for line in summarize(result, read_json(args.out / "samples" / "index.json")["samples"]):
        print(line)


if __name__ == "__main__":
    main()
