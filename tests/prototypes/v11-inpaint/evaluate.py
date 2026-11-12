#!/usr/bin/env python3
"""V11 背景修补原型：客观评价。

对 <out>/results/<掩膜>/<方法>/ 下每一组修补结果，逐个样本和“没有这一行字的对照图”比，按三类背景汇总：

1. 和对照图差多少。比的范围是“改过的像素加上原来有字的像素”：修补用的掩膜，并上真值里肉眼看得出的字形像素
   （掩膜漏掉的字也算进来）。四个数：
     mae   平均绝对差（三个通道一起平均，0..255）
     bad   超过阈值的像素比例：三个通道里差得最多的那个超过 24 的像素占多少
     psnr  峰值信噪比（dB）
     ssim  结构相似度（灰度图上算，高斯窗 11×11、σ=1.5，只在上面的范围里取平均）
2. 抹完之后原来的字还认不认得出来：把补丁贴回原图，用固定的命令行工具跑 OCR，看目标行的位置上
   还能不能检出文字（ocr_lines）、读出来的和原文有多像（ocr_similarity，1 − 编辑距离 ÷ 较长一段的长度）。
3. 红章、底纹线条、表格线有没有被抹掉：掩膜里本来有这些东西的像素（samples/<编号>/struct.png），修补之后
   还剩多少。一个像素“还在”的判定：把修补结果减去“没有章和底纹的对照图”，在“章和底纹本来带来的颜色变化”
   这个方向上至少还有一半（struct_kept 是还在的像素比例，struct_strength 是平均还剩几成）。

另外给两行参照：
  none   不修补（带字的原图）
  floor  噪声下限：同一张对照图去掉噪声之后和对照图比。修补的方法不可能知道纸面噪声的具体取值，
         平均绝对差做到这个数就是到头了

输出 <out>/evaluation.json（每个样本的数、按类别和子类的汇总）和 <out>/evaluation.md（按类别的表）。

用法：python evaluate.py [--out tests/out/v11] [--masks auto-d3 ...] [--methods ...] [--no-ocr] [--force-ocr]
"""
from __future__ import annotations

import argparse
import math
import os
import shutil
from pathlib import Path
from typing import Any

import cv2
import numpy as np
import numpy.typing as npt

from common import DEFAULT_CLI, DEFAULT_MODELS_DIR, DEFAULT_OUT, Image, Mask, bounding_rect, imread, imwrite, quad_to_array, read_json, write_json
from ocr import read_target, run_cli

BAD_THRESHOLD = 24
VISIBLE_ALPHA = 26  # gt_alpha 里变暗一成以上的像素算“看得出的字形像素”
CATEGORIES = ("plain", "texture", "seal")
CATEGORY_NAMES = {"plain": "纯色", "texture": "底纹", "seal": "压红章", "all": "全部"}
BASELINES = ("none", "floor")
BoolMap = npt.NDArray[np.bool_]


def ssim_map(a: npt.NDArray[np.uint8], b: npt.NDArray[np.uint8]) -> npt.NDArray[np.float32]:
    """两张灰度图逐像素的 SSIM（Wang 等 2004 的标准参数）。"""
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    x, y = a.astype(np.float32), b.astype(np.float32)

    def blur(v: npt.NDArray[np.float32]) -> npt.NDArray[np.float32]:
        return np.asarray(cv2.GaussianBlur(v, (11, 11), 1.5), dtype=np.float32)

    mx, my = blur(x), blur(y)
    vx, vy, cov = blur(x * x) - mx * mx, blur(y * y) - my * my, blur(x * y) - mx * my
    return np.asarray(((2 * mx * my + c1) * (2 * cov + c2)) / ((mx * mx + my * my + c1) * (vx + vy + c2)), dtype=np.float32)


def region_metrics(result: Image, ref: Image, region: BoolMap) -> dict[str, Any]:
    """在 region 里比 result 和 ref。"""
    x0, y0, x1, y1 = bounding_rect(region.astype(np.uint8), pad=16)
    r, t, m = result[y0:y1, x0:x1], ref[y0:y1, x0:x1], region[y0:y1, x0:x1]
    diff = np.abs(r.astype(np.float32) - t.astype(np.float32))[m]
    mse = float(np.mean(diff * diff))
    ssim = ssim_map(cv2.cvtColor(r, cv2.COLOR_BGR2GRAY), cv2.cvtColor(t, cv2.COLOR_BGR2GRAY))[m]
    return {
        "mae": round(float(diff.mean()), 3),
        "bad": round(float(np.mean(diff.max(axis=1) > BAD_THRESHOLD)), 4),
        "psnr": round(10.0 * math.log10(255.0**2 / max(mse, 1e-6)), 2),
        "ssim": round(float(ssim.mean()), 4),
        "region_pixels": int(np.count_nonzero(m)),
    }


def structure_metrics(result: Image, ref: Image, nostruct: Image, struct: Mask, mask: Mask) -> dict[str, Any]:
    """掩膜里本来有章、底纹线条或表格线的像素，修补之后还剩多少。"""
    sel = (struct > 0) & (mask > 0)
    n = int(np.count_nonzero(sel))
    if n == 0:
        return {"struct_n": 0}
    base = nostruct[sel].astype(np.float32)
    want = ref[sel].astype(np.float32) - base
    got = result[sel].astype(np.float32) - base
    strength = np.sum(got * want, axis=1) / np.maximum(np.sum(want * want, axis=1), 1.0)
    kept = strength >= 0.5
    return {
        "struct_n": n,
        "struct_kept": round(float(np.mean(kept)), 4),
        "struct_strength": round(float(np.mean(np.clip(strength, 0.0, 1.0))), 4),
        # 整个章（或整片底纹）的像素里丢了多少：掩膜外面的没动过
        "struct_lost_overall": round(float(np.count_nonzero(~kept)) / max(float(np.count_nonzero(struct)), 1.0), 4),
    }


def aggregate(rows: list[dict[str, Any]]) -> dict[str, Any]:
    """一组样本的汇总：各项取平均，另记最差的和几个计数。"""
    out: dict[str, Any] = {"n": len(rows)}
    for key in ("mae", "bad", "psnr", "ssim", "ocr_similarity", "struct_kept", "struct_strength", "struct_lost_overall", "windows"):
        vals = [float(r[key]) for r in rows if r.get(key) is not None]
        if vals:
            out[key] = round(float(np.mean(vals)), 4)
    for key, pick in (("mae", max), ("bad", max), ("struct_kept", min)):
        vals = [float(r[key]) for r in rows if r.get(key) is not None]
        if vals:
            out[f"{key}_worst"] = round(pick(vals), 4)
    ms = [float(r["ms"]) for r in rows if r.get("ms") is not None]
    if ms:
        out["ms_median"] = round(float(np.median(ms)), 1)
        out["ms_max"] = round(float(np.max(ms)), 1)
    if any("ocr_lines" in r for r in rows):
        # 目标行的位置上还检出文字的样本数。压着章的行，章上的字也会被检出来，所以对照图在这一项上也不是 0
        out["ocr_detected"] = sum(1 for r in rows if r.get("ocr_lines", 0) > 0)
        out["ocr_trace"] = sum(1 for r in rows if r.get("ocr_similarity", 0.0) > 0.0)  # 还读得出一点原文的样本数
        out["ocr_readable"] = sum(1 for r in rows if r.get("ocr_similarity", 0.0) >= 0.5)  # 还读得出一半以上原文的样本数
    out["struct_samples"] = sum(1 for r in rows if r.get("struct_n", 0) > 0)
    return out


def _stamp(index_path: Path) -> dict[str, int]:
    st = index_path.stat()
    return {"mtime_ns": st.st_mtime_ns, "size": st.st_size}


def evaluate(
    out_dir: Path,
    mask_keys: list[str] | None,
    method_filter: list[str] | None,
    cli: Path,
    models_dir: Path,
    do_ocr: bool,
    force_ocr: bool,
) -> dict[str, Any]:
    samples = read_json(out_dir / "samples" / "index.json")["samples"]
    ocr_input = read_json(out_dir / "ocr-input.json")["samples"]
    results_root = out_dir / "results"
    found = sorted(p.name for p in results_root.iterdir() if p.is_dir())
    result: dict[str, Any] = {"fmt": "magpdf-v11-eval/1", "bad_threshold": BAD_THRESHOLD, "sets": {}}

    for mask_key in mask_keys or found:
        methods = sorted(p.name for p in (results_root / mask_key).iterdir() if (p / "index.json").exists())
        if method_filter:
            methods = [m for m in methods if m in method_filter]
        indexes = {m: read_json(results_root / mask_key / m / "index.json")["samples"] for m in methods}

        # 哪些方法要重新跑 OCR：结果的 index.json 变了，或者还没跑过
        ocr_dirs = {m: out_dir / "ocr" / "results" / mask_key / m for m in methods}
        need_ocr: dict[str, bool] = {}
        for m in methods:
            stamp_path = ocr_dirs[m] / "_stamp.json"
            fresh = stamp_path.exists() and read_json(stamp_path) == _stamp(results_root / mask_key / m / "index.json")
            need_ocr[m] = do_ocr and (force_ocr or not fresh)
        # 给 OCR 用的整图临时放这里；目录名带进程号，两个进程同时评价也不会互相删
        image_root = out_dir / "ocr" / f"_images-{os.getpid()}" / mask_key

        per_sample: dict[str, dict[str, dict[str, Any]]] = {m: {} for m in (*BASELINES, *methods)}
        for meta in samples:
            sid = meta["sid"]
            sample_dir = out_dir / "samples" / sid
            img = imread(sample_dir / "input.png")
            ref = imread(sample_dir / "ref.png")
            mask = imread(out_dir / "maskset" / mask_key / f"{sid}.png", gray=True)
            visible = imread(sample_dir / "gt_alpha.png", gray=True) >= VISIBLE_ALPHA
            region = (mask > 0) | visible
            has_struct = (sample_dir / "struct.png").exists()
            struct = imread(sample_dir / "struct.png", gray=True) if has_struct else None
            nostruct = imread(sample_dir / "nostruct.png") if has_struct else None

            base = region_metrics(img, ref, region)
            base.update({"ocr_similarity": ocr_input[sid]["input"]["similarity"], "ocr_lines": ocr_input[sid]["input"]["lines"]})
            per_sample["none"][sid] = base
            floor = region_metrics(imread(sample_dir / "ref_nonoise.png"), ref, region)
            floor.update({"ocr_similarity": ocr_input[sid]["ref"]["similarity"], "ocr_lines": ocr_input[sid]["ref"]["lines"]})
            per_sample["floor"][sid] = floor

            for m in methods:
                info = indexes[m][sid]
                x0, y0, x1, y1 = info["rect"]
                full = img.copy()
                full[y0:y1, x0:x1] = imread(results_root / mask_key / m / f"{sid}.png")
                entry = region_metrics(full, ref, region)
                entry["ms"] = info["ms"]
                if "windows" in info:
                    entry["windows"] = info["windows"]
                if struct is not None and nostruct is not None:
                    entry.update(structure_metrics(full, ref, nostruct, struct, mask))
                per_sample[m][sid] = entry
                if need_ocr[m]:
                    imwrite(image_root / m / f"{sid}.png", full)

        for m in methods:
            if need_ocr[m]:
                if ocr_dirs[m].exists():
                    shutil.rmtree(ocr_dirs[m])
                run_cli(image_root / m, ocr_dirs[m], cli, models_dir, out_dir / "ocr" / "results" / mask_key / f"{m}.log")
                write_json(ocr_dirs[m] / "_stamp.json", _stamp(results_root / mask_key / m / "index.json"))
                shutil.rmtree(image_root / m)
                print(f"OCR：{mask_key} {m}", flush=True)
            if (ocr_dirs[m] / "_stamp.json").exists():
                for meta in samples:
                    sid = meta["sid"]
                    read = read_target(ocr_dirs[m] / f"{sid}.json", quad_to_array(meta["quad"]), meta["text"])
                    per_sample[m][sid].update({"ocr_similarity": read["similarity"], "ocr_lines": read["lines"], "ocr_chars": read["chars"]})
        if image_root.parent.exists():
            shutil.rmtree(image_root.parent)

        set_result: dict[str, Any] = {}
        for m, rows in per_sample.items():
            by_category = {c: aggregate([rows[s["sid"]] for s in samples if s["category"] == c]) for c in CATEGORIES}
            by_category["all"] = aggregate(list(rows.values()))
            tags = sorted({s["tag"] for s in samples})
            by_tag = {t: aggregate([rows[s["sid"]] for s in samples if s["tag"] == t]) for t in tags}
            set_result[m] = {"by_category": by_category, "by_tag": by_tag, "samples": rows}
        result["sets"][mask_key] = set_result
    return result


def _fmt(value: Any, spec: str, scale: float = 1.0) -> str:
    return "—" if value is None else format(float(value) * scale, spec)


def method_order(names: list[str]) -> list[str]:
    """表里的顺序：两行参照在前，然后按 inpaint.py 里的顺序。"""
    from inpaint import METHODS

    known = [m.key for m in METHODS]
    rest = [n for n in names if n not in BASELINES]
    rest.sort(key=lambda n: (known.index(n) if n in known else len(known), n))
    return [n for n in BASELINES if n in names] + rest


def method_label(name: str) -> str:
    from inpaint import METHOD_BY_KEY

    if name == "none":
        return "不修补（参照）"
    if name == "floor":
        return "噪声下限（参照）"
    return f"{METHOD_BY_KEY[name].label}（`{name}`）" if name in METHOD_BY_KEY else f"`{name}`"


def tables_markdown(result: dict[str, Any]) -> str:
    lines: list[str] = []
    for mask_key, set_result in result["sets"].items():
        for cat in (*CATEGORIES, "all"):
            n = set_result["none"]["by_category"][cat]["n"]
            lines.append(f"### 掩膜 `{mask_key}`，{CATEGORY_NAMES[cat]}（{n} 行）\n")
            lines.append("| 方法 | 平均绝对差 | 超过阈值的像素 | 最差一行 | PSNR（dB） | SSIM | OCR 还读出原文的行：一半以上 / 一点 | 读出的和原文的相似度 | 章、底纹的像素还在的比例 | 每行耗时（中位，ms） |")
            lines.append("|---|---|---|---|---|---|---|---|---|---|")
            for m in method_order(list(set_result)):
                a = set_result[m]["by_category"][cat]
                detected = f"{a['ocr_readable']} / {a['ocr_trace']}" if "ocr_readable" in a else "—"
                kept = "—"
                if m not in BASELINES and a.get("struct_kept") is not None:
                    kept = f"{a['struct_kept'] * 100:.0f}%（{a['struct_samples']} 行，最差 {a['struct_kept_worst'] * 100:.0f}%）"
                lines.append(
                    f"| {method_label(m)} | {_fmt(a.get('mae'), '.2f')} | {_fmt(a.get('bad'), '.1f', 100)}% | {_fmt(a.get('bad_worst'), '.1f', 100)}%"
                    f" | {_fmt(a.get('psnr'), '.1f')} | {_fmt(a.get('ssim'), '.3f')} | {detected} | {_fmt(a.get('ocr_similarity'), '.3f')}"
                    f" | {kept} | {_fmt(a.get('ms_median'), '.0f')} |"
                )
            lines.append("")
    return "\n".join(lines)


def print_summary(result: dict[str, Any]) -> None:
    for mask_key, set_result in result["sets"].items():
        print(f"== 掩膜 {mask_key}：每类三个数是 平均绝对差 / 超过阈值的像素% / 章和底纹还在% ；后面是 OCR 还读出原文的行数（一半以上/一点）")
        for m in method_order(list(set_result)):
            cells = []
            for cat in CATEGORIES:
                a = set_result[m]["by_category"][cat]
                kept = _fmt(a.get("struct_kept") if m not in BASELINES else None, ".0f", 100)
                cells.append(f"{CATEGORY_NAMES[cat]} {_fmt(a.get('mae'), '5.2f')} /{_fmt(a.get('bad'), '5.1f', 100)} /{kept:>4s}")
            a = set_result[m]["by_category"]["all"]
            ocr = f"{a['ocr_readable']:2d}/{a['ocr_trace']:2d}" if "ocr_readable" in a else "  —"
            print(f"  {m:17s} " + "   ".join(cells) + f"   OCR {ocr}")


def main() -> None:
    ap = argparse.ArgumentParser(description="按三类背景评价修补结果")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--masks", nargs="+", help="只评这几套掩膜（默认是 results/ 下有的全部）")
    ap.add_argument("--methods", nargs="+", help="只评这几种方法")
    ap.add_argument("--cli", type=Path, default=DEFAULT_CLI)
    ap.add_argument("--models-dir", type=Path, default=DEFAULT_MODELS_DIR)
    ap.add_argument("--no-ocr", action="store_true", help="不跑 OCR（已经跑过的结果照样用）")
    ap.add_argument("--force-ocr", action="store_true", help="OCR 全部重跑")
    args = ap.parse_args()
    result = evaluate(args.out, args.masks, args.methods, args.cli, args.models_dir, not args.no_ocr, args.force_ocr)
    write_json(args.out / "evaluation.json", result)
    (args.out / "evaluation.md").write_text(tables_markdown(result), encoding="utf-8", newline="\n")
    print_summary(result)


if __name__ == "__main__":
    main()
