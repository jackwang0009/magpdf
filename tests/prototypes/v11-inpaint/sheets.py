#!/usr/bin/env python3
"""V11 背景修补原型：给人打分用的对照图和打分表（主观分由人打，脚本不打）。

每个样本一行，从左到右：带字的原图｜掩膜（要抹掉的像素涂成蓝色）｜各方法修补的结果｜没有这一行字的对照图。
三类背景各拼一张大图。每一格是原图上同一个位置按 300 dpi 原尺寸裁出来的一块（默认 520 像素宽），不缩放：
有章、底纹的样本裁在字和它们叠得最多的那一段，其余的裁在这一行的中间。

输出（<out>/sheets/）：
  sheet-1-plain.png、sheet-2-texture.png、sheet-3-seal.png   对照图
  scores-template.csv   打分表模板：样本、方法、评分人1、评分人2、评分人3（UTF-8 带 BOM，Excel 能直接打开）

加 --blind 另出一套不写方法名的（<out>/sheets-blind/）：每个样本各方法的左右顺序打乱（固定种子），
列名是 A、B、C……，打分表按列名填，对应关系在 blind-key.csv 里，打完分再对回去。

--methods 里写“方法”或“方法@掩膜”：后一种是这一列换一套掩膜（比如 lama@auto-d7 是掩膜往外扩 7 像素之后的 LaMa），
掩膜那一格画的始终是 --mask 指的那一套。没有结果的列自动不放。

用法：python sheets.py [--out tests/out/v11] [--mask auto-d3] [--methods fill-local telea-r3 lama@auto-d7 ...] [--blind]
"""
from __future__ import annotations

import argparse
import csv
from pathlib import Path
from typing import Any

import cv2
import numpy as np
from PIL import Image as PilImage
from PIL import ImageDraw, ImageFont

from common import DEFAULT_OUT, Image, dilate, imread, imwrite, read_json
from masks import DEFAULT_DILATE

# 默认拿给人看的几列：每一类方法挑一两种（挑法见报告），太多了打不过来。“方法@掩膜”是这一列换一套掩膜
DEFAULT_METHODS = ["fill-local", "fill-local-grain", "telea-r3", "fsr-fast", "shiftmap", "lama", "lama@auto-d7"]
CATEGORIES = [("plain", "纯色背景"), ("texture", "底纹背景"), ("seal", "压着红章")]
TAG_NAMES = {
    "white": "白纸",
    "offwhite": "发黄的纸",
    "gray": "发灰的纸",
    "tinted": "彩色联单",
    "uneven": "底色不匀",
    "shadow": "装订阴影",
    "ruled": "表格线",
    "mesh": "网纹",
    "dots": "点阵",
    "gradient": "渐变",
    "watermark": "水印字",
    "ring-text": "章的环形文字",
    "star": "章的五角星",
    "bottom-text": "章的横排文字",
    "ring-line": "章的外圈",
}
CELL_W = 520
LABEL_W = 132
GAP = 10
ROW_GAP = 22
HEADER_H = 56
FONT_PATH = Path(r"C:\Windows\Fonts\msyh.ttc")


def label_strip(width: int, height: int, lines: list[str], px: int = 20) -> Image:
    """一块白底黑字的标签（BGR）。"""
    pil = PilImage.new("RGB", (width, height), (255, 255, 255))
    draw = ImageDraw.Draw(pil)
    font = ImageFont.truetype(str(FONT_PATH), px)
    total = len(lines) * (px + 6) - 6
    y = (height - total) / 2
    for line in lines:
        draw.text((width / 2, y + px / 2), line, font=font, fill=(0, 0, 0), anchor="mm")
        y += px + 6
    return np.ascontiguousarray(np.asarray(pil)[..., ::-1])


def window_for(meta: dict[str, Any], sample_dir: Path, cell_h: int) -> tuple[int, int]:
    """这个样本裁哪一块：返回左上角。"""
    width, height = meta["size"]
    quad = np.asarray(meta["quad"], dtype=np.float64)
    left, right = (quad[0] + quad[3]) / 2, (quad[1] + quad[2]) / 2
    x0 = int(round((left[0] + right[0]) / 2 - CELL_W / 2))
    if (sample_dir / "struct.png").exists():
        glyphs = dilate(imread(sample_dir / "gt_mask.png", gray=True), DEFAULT_DILATE) > 0
        overlap = np.count_nonzero(glyphs & (imread(sample_dir / "struct.png", gray=True) > 0), axis=0).astype(np.float64)
        if overlap.sum() > 0 and overlap.size > CELL_W:
            x0 = int(np.argmax(np.convolve(overlap, np.ones(CELL_W), mode="valid")))
    x0 = int(np.clip(x0, 0, width - CELL_W))
    t = float(np.clip((x0 + CELL_W / 2 - left[0]) / max(right[0] - left[0], 1.0), 0.0, 1.0))
    cy = left[1] + t * (right[1] - left[1])
    y0 = int(np.clip(round(cy - cell_h / 2), 0, height - cell_h))
    return x0, y0


def hstack_gap(cells: list[Image]) -> Image:
    gap = np.full((cells[0].shape[0], GAP, 3), 255, dtype=np.uint8)
    parts: list[Image] = []
    for i, cell in enumerate(cells):
        if i:
            parts.append(gap)
        parts.append(cell)
    return np.hstack(parts)


def split_column(entry: str, base_mask: str) -> tuple[str, str]:
    """“方法”或“方法@掩膜” → (方法, 掩膜)。"""
    method, _, mask = entry.partition("@")
    return method, mask or base_mask


def mask_note(mask_key: str) -> str:
    kind, _, radius = mask_key.rpartition("-d")
    name = {"auto": "掩膜", "gt": "真值掩膜", "gray": "不按颜色分的掩膜"}.get(kind, kind)
    return f"{name}往外扩 {radius} 像素"


def build(out_dir: Path, mask_key: str, methods: list[str], blind: bool, seed: int) -> None:
    from inpaint import METHOD_BY_KEY

    samples = read_json(out_dir / "samples" / "index.json")["samples"]
    available: list[str] = []
    for entry in methods:
        method, mask = split_column(entry, mask_key)
        if method in METHOD_BY_KEY and (out_dir / "results" / mask / method / "index.json").exists():
            available.append(entry)
        else:
            print(f"没有 {mask}/{method} 的结果，对照图里不放这一列")
    result_dirs = {entry: out_dir / "results" / split_column(entry, mask_key)[1] / split_column(entry, mask_key)[0] for entry in available}
    indexes = {entry: read_json(result_dirs[entry] / "index.json")["samples"] for entry in available}
    sheet_dir = out_dir / ("sheets-blind" if blind else "sheets")
    sheet_dir.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(seed)
    letters = [chr(ord("A") + i) for i in range(len(available))]
    score_rows: list[list[str]] = []
    key_rows: list[list[str]] = []

    def column_title(entry: str) -> list[str]:
        method, mask = split_column(entry, mask_key)
        return [METHOD_BY_KEY[method].label] + ([mask_note(mask)] if mask != mask_key else [])

    for page, (category, category_name) in enumerate(CATEGORIES, 1):
        first = [["带字的原图"], ["掩膜（蓝色是要抹掉的）", mask_note(mask_key)]]
        last = [["对照", "同样条件、本来就没有这行字"]]
        if blind:
            titles = [*first, *[[f"方法 {c}"] for c in letters], *last]
        else:
            titles = [*first, *[column_title(entry) for entry in available], *last]
        header = hstack_gap([label_strip(LABEL_W, HEADER_H, ["样本"], px=18), *[label_strip(CELL_W, HEADER_H, t, px=18) for t in titles]])
        rows: list[Image] = [header]
        for meta in [s for s in samples if s["category"] == category]:
            sid = meta["sid"]
            sample_dir = out_dir / "samples" / sid
            quad = np.asarray(meta["quad"], dtype=np.float64)
            line_h = float(np.linalg.norm(quad[3] - quad[0]))
            cell_h = int(np.clip(round(line_h + 96), 132, 196))  # 目标行上下各留四五十像素
            x0, y0 = window_for(meta, sample_dir, cell_h)
            img = imread(sample_dir / "input.png")
            mask = imread(out_dir / "maskset" / mask_key / f"{sid}.png", gray=True) > 0
            tinted = img.copy()
            tinted[mask] = (0.35 * tinted[mask] + 0.65 * np.array([255, 150, 0])).astype(np.uint8)

            order = list(range(len(available)))
            if blind:
                rng.shuffle(order)
            cells = [img, tinted]
            for column, mi in enumerate(order):
                m = available[mi]
                rx0, ry0, rx1, ry1 = indexes[m][sid]["rect"]
                full = img.copy()
                full[ry0:ry1, rx0:rx1] = imread(result_dirs[m] / f"{sid}.png")
                cells.append(full)
                if blind:
                    score_rows.append([sid, letters[column], "", "", ""])
                    key_rows.append([sid, letters[column], m])
                else:
                    score_rows.append([sid, m, "", "", ""])
            cells.append(imread(sample_dir / "ref.png"))
            label = label_strip(LABEL_W, cell_h, [sid, TAG_NAMES.get(meta["tag"], meta["tag"]), f"{meta['size_pt']:g} 磅"], px=18)
            rows.append(hstack_gap([label, *[np.ascontiguousarray(c[y0 : y0 + cell_h, x0 : x0 + CELL_W]) for c in cells]]))

        width = rows[0].shape[1]
        title = label_strip(
            width,
            48,
            [f"V11 背景修补对照图 {page}/3：{category_name}（每格是 300 dpi 原尺寸的 {CELL_W} 像素宽的一块，没有缩放；请放大到 100% 看）"],
            px=24,
        )
        # 行与行之间留白，中间一条灰线，免得上下两个样本的底纹连成一片
        gap = np.full((ROW_GAP, width, 3), 255, dtype=np.uint8)
        gap[ROW_GAP // 2 - 1 : ROW_GAP // 2 + 1] = 150
        parts: list[Image] = [title]
        for row in rows:
            parts += [gap, row]
        path = sheet_dir / f"sheet-{page}-{category}.png"
        imwrite(path, np.vstack(parts))
        print(f"写了 {path}（{width}×{sum(p.shape[0] for p in parts)}）")

    with (sheet_dir / "scores-template.csv").open("w", encoding="utf-8-sig", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["样本", "列" if blind else "方法", "评分人1", "评分人2", "评分人3"])
        writer.writerows(score_rows)
    if blind:
        with (sheet_dir / "blind-key.csv").open("w", encoding="utf-8-sig", newline="") as f:
            writer = csv.writer(f)
            writer.writerow(["样本", "列", "方法"])
            writer.writerows(key_rows)
    print(f"打分表模板 {sheet_dir / 'scores-template.csv'}：{len(score_rows)} 行（{len(samples)} 个样本 × {len(available)} 种方法）")


def main() -> None:
    ap = argparse.ArgumentParser(description="拼给人打分用的对照图，出打分表模板")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--mask", default=f"auto-d{DEFAULT_DILATE}")
    ap.add_argument("--methods", nargs="+", default=DEFAULT_METHODS)
    ap.add_argument("--blind", action="store_true", help="不写方法名，每个样本各方法的顺序打乱")
    ap.add_argument("--seed", type=int, default=20261112, help="--blind 打乱顺序用的种子")
    args = ap.parse_args()
    build(args.out, args.mask, args.methods, args.blind, args.seed)


if __name__ == "__main__":
    main()
