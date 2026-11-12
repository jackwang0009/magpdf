#!/usr/bin/env python3
"""V11 背景修补原型：量时间。一条命令重量：python tests/prototypes/v11-inpaint/bench.py

量两样，结果写 <out>/bench.json：

1. LaMa（ONNX，CPU 版 onnxruntime）处理一个 512×512 区域要多久。每种线程设置起 --processes 个新进程（默认 5 个，
   几种设置轮流着起；峰值内存是按进程记的，放在一个进程里量会串），每个进程里建一次会话、预热一次、
   再连着推理 --runs 次（默认 7 次）：
     init_s        建会话用了多久（含读 208 MB 的模型文件和 ONNX Runtime 的图优化）：每个进程一个数
     first_ms      第一次推理（预热）：每个进程一个数
     infer_ms      预热之后的推理：所有进程的放在一起（默认 5×7 = 35 次）取中位数、最小、最大
     peak_wset_mb  进程的峰值工作集；peak_commit_mb 是峰值提交内存；rss_before_mb 是加载模型之前
                   （Python、NumPy、OpenCV、onnxruntime 本身），rss_loaded_mb 是建完会话之后
   默认量四种线程设置：0（交给 ONNX Runtime，默认是物理核数）、4、2、1；任务书要的是前两种，
   后两种是为了看核少的机器上大概会怎样。输入是一张固定的合成图。
   时间含前后处理（BGR 和浮点数来回转）。

2. 传统方法处理一行字要多久（要先跑过流水线，没有样本就跳过）：从 50 个样本里每隔 5 个取一个，共 10 行，
   每种方法每行跑 --repeat 次（默认 5 次）取中位数；表里给这 10 行的中位数、最小、最大。
   掩膜用流水线算好的 auto-d3，时间不含算掩膜（算掩膜的时间单独记一项 mask_ms）。
   xphoto 的两种方法要带 contrib 的 OpenCV：当前环境里没有时，用 --contrib-python 指的那个 Python 另起一个进程量，
   结果并进来；那个环境也没有就跳过，并在 JSON 里写明。fsr-best 太慢，不量。

量之前、量之后各记一次整机的 CPU 占用（1 秒的平均）。这台机器不是参考机 R1，量的时候可能有别的任务在跑。
加 --priority high 把量的进程调成高优先级：整机被别的任务占满时，这样量出来的更接近机器空着时的数（共用缓存、
内存带宽和超线程的影响还在）；JSON 里会记下用的是哪种优先级。

用法：python bench.py [--out tests/out/v11] [--json <out>/bench.json] [--threads 0 4 2 1] [--processes 5] [--runs 7] [--repeat 5]
                      [--families fill opencv xphoto] [--skip-traditional] [--skip-lama] [--contrib-python ...] [--priority high]
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import platform
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import cv2
import numpy as np
import psutil

from common import DEFAULT_LAMA_MODEL, DEFAULT_OUT, Image, Mask, imread, quad_to_array, read_json, write_json

MB = 1024 * 1024
DEFAULT_CONTRIB_PYTHON = Path(os.environ.get("MAGPDF_V11_CONTRIB_PYTHON", r"D:\workspace\magpdf-wt\venvs\v11-contrib\Scripts\python.exe"))


def set_priority(level: str) -> None:
    """--priority high：把本进程调成高优先级（只有 Windows 上有），机器上有别的任务在跑时少受一点影响。"""
    if level == "high" and hasattr(psutil, "HIGH_PRIORITY_CLASS"):
        psutil.Process().nice(psutil.HIGH_PRIORITY_CLASS)


def synthetic_input() -> tuple[Image, Mask]:
    """一张固定的 512×512 测试图：带噪声的纸面上四行深色的条，掩膜是其中一行。"""
    rng = np.random.default_rng(11)
    img = np.clip(rng.normal(238, 5, (512, 512, 3)), 0, 255).astype(np.uint8)
    mask = np.zeros((512, 512), dtype=np.uint8)
    for row, y in enumerate(range(120, 420, 80)):
        for x in range(40, 470, 34):
            cv2.rectangle(img, (x, y), (x + 22, y + 36), (40, 40, 40), -1)
            if row == 1:
                cv2.rectangle(mask, (x - 3, y - 3), (x + 25, y + 39), 255, -1)
    return img, mask


def lama_worker(model: Path, threads: int, runs: int) -> None:
    """在单独的进程里量一种线程设置，结果作为一行 JSON 打到标准输出。"""
    from lama import Lama

    proc = psutil.Process()
    rss_before = proc.memory_info().rss
    lama = Lama(model, threads)
    rss_loaded = proc.memory_info().rss
    img, mask = synthetic_input()
    start = time.perf_counter()
    lama.infer(img, mask)
    first = time.perf_counter() - start
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        lama.infer(img, mask)
        times.append((time.perf_counter() - start) * 1000.0)
    mem = proc.memory_info()
    result = {
        "threads": threads,
        "init_s": round(lama.init_seconds, 3),
        "first_ms": round(first * 1000.0, 1),
        "runs": runs,
        "median_ms": round(float(np.median(times)), 1),
        "min_ms": round(float(np.min(times)), 1),
        "max_ms": round(float(np.max(times)), 1),
        "times_ms": [round(t, 1) for t in times],
        "rss_before_mb": round(rss_before / MB, 1),
        "rss_loaded_mb": round(rss_loaded / MB, 1),
        "rss_after_mb": round(mem.rss / MB, 1),
        # 这两项只有 Windows 上有
        "peak_wset_mb": round(getattr(mem, "peak_wset", 0) / MB, 1),
        "peak_commit_mb": round(getattr(mem, "peak_pagefile", 0) / MB, 1),
    }
    print(json.dumps(result))


def _spread(values: list[float], digits: int = 1) -> dict[str, Any]:
    return {
        "median": round(float(np.median(values)), digits),
        "min": round(float(np.min(values)), digits),
        "max": round(float(np.max(values)), digits),
        "n": len(values),
    }


def bench_lama(model: Path, thread_settings: list[int], runs: int, processes: int, priority: str) -> list[dict[str, Any]]:
    """每种线程设置起 processes 个进程，轮流着起（这一轮默认线程、4 线程，下一轮再来），免得整机负载的变化只落在一种设置上。"""
    per_setting: dict[int, list[dict[str, Any]]] = {threads: [] for threads in thread_settings}
    errors: dict[int, str] = {}
    for round_no in range(processes):
        for threads in thread_settings:
            if threads in errors:
                continue
            cpu_before = psutil.cpu_percent(interval=1.0)
            proc = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).resolve()),
                    "--lama-worker",
                    "--threads",
                    str(threads),
                    "--runs",
                    str(runs),
                    "--lama-model",
                    str(model),
                    "--priority",
                    priority,
                ],
                capture_output=True,
                text=True,
                check=False,
                cwd=str(Path(__file__).resolve().parent),
            )
            cpu_after = psutil.cpu_percent(interval=1.0)
            if proc.returncode != 0:
                errors[threads] = proc.stderr.strip().splitlines()[-1] if proc.stderr.strip() else f"退出码 {proc.returncode}"
                continue
            entry = json.loads(proc.stdout.strip().splitlines()[-1])
            entry.update({"cpu_before_pct": cpu_before, "cpu_after_pct": cpu_after})
            per_setting[threads].append(entry)
            name = "默认线程" if threads == 0 else f"{threads} 线程"
            print(
                f"LaMa {name} 第 {round_no + 1}/{processes} 个进程：建会话 {entry['init_s']:.2f} s，第一次 {entry['first_ms']:.0f} ms，"
                f"之后 {runs} 次中位 {entry['median_ms']:.0f} ms，峰值工作集 {entry['peak_wset_mb']:.0f} MB；量之前整机 CPU {cpu_before:.0f}%",
                flush=True,
            )
    results = []
    for threads in thread_settings:
        entries = per_setting[threads]
        if not entries:
            results.append({"threads": threads, "error": errors.get(threads, "没有量到")})
            continue
        summary: dict[str, Any] = {
            "threads": threads,
            "processes": len(entries),
            "runs_per_process": runs,
            # 每个进程建一次会话、预热一次：这两项各有 processes 个数
            "init_s": _spread([e["init_s"] for e in entries], 2),
            "first_ms": _spread([e["first_ms"] for e in entries]),
            # 预热之后的推理：所有进程的放在一起
            "infer_ms": _spread([t for e in entries for t in e["times_ms"]]),
            "peak_wset_mb": _spread([e["peak_wset_mb"] for e in entries]),
            "peak_commit_mb": _spread([e["peak_commit_mb"] for e in entries]),
            "rss_before_mb": _spread([e["rss_before_mb"] for e in entries]),
            "rss_loaded_mb": _spread([e["rss_loaded_mb"] for e in entries]),
            "cpu_before_pct": _spread([e["cpu_before_pct"] for e in entries]),
            "cpu_after_pct": _spread([e["cpu_after_pct"] for e in entries]),
            "per_process": entries,
        }
        if threads in errors:
            summary["error"] = errors[threads]
        results.append(summary)
        name = "默认线程" if threads == 0 else f"{threads} 线程"
        print(
            f"LaMa {name}：建会话中位 {summary['init_s']['median']:.2f} s（{summary['init_s']['min']:.2f}–{summary['init_s']['max']:.2f}），"
            f"一次推理中位 {summary['infer_ms']['median']:.0f} ms（{summary['infer_ms']['min']:.0f}–{summary['infer_ms']['max']:.0f}，{summary['infer_ms']['n']} 次），"
            f"峰值工作集中位 {summary['peak_wset_mb']['median']:.0f} MB（最大 {summary['peak_wset_mb']['max']:.0f}）",
            flush=True,
        )
    return results


def _timing(per_line: list[float]) -> dict[str, Any]:
    return {
        "median_ms": round(float(np.median(per_line)), 1),
        "min_ms": round(float(np.min(per_line)), 1),
        "max_ms": round(float(np.max(per_line)), 1),
        "per_line_ms": [round(t, 1) for t in per_line],
    }


def bench_traditional(out_dir: Path, repeat: int, mask_key: str, families: list[str]) -> dict[str, Any]:
    from inpaint import METHODS
    from masks import AUTO_METHOD, segment_text

    index_path = out_dir / "samples" / "index.json"
    if not index_path.exists() or not (out_dir / "maskset" / mask_key).exists():
        return {"skipped": f"没有样本或 {mask_key} 掩膜（先跑 run_all.py）"}
    samples = read_json(index_path)["samples"][::5]
    data = [
        (m["sid"], imread(out_dir / "samples" / m["sid"] / "input.png"), imread(out_dir / "maskset" / mask_key / f"{m['sid']}.png", gray=True))
        for m in samples
    ]
    result: dict[str, Any] = {
        "mask": mask_key,
        "samples": [sid for sid, _, _ in data],
        "mask_pixels_median": int(np.median([np.count_nonzero(mask) for _, _, mask in data])),
        "repeat": repeat,
        "opencv": cv2.__version__,
        "cpu_before_pct": psutil.cpu_percent(interval=1.0),
        "methods": {},
    }

    # 从行框里分出字的像素要多久（不含 OCR 本身）
    boxes_path = out_dir / "ocr-input.json"
    if boxes_path.exists() and "fill" in families:
        boxes = read_json(boxes_path)["samples"]
        per_line = []
        for sid, img, _ in data:
            quads = [quad_to_array(q) for q in boxes[sid]["boxes"]]
            times = []
            for _ in range(repeat):
                start = time.perf_counter()
                segment_text(img, quads, AUTO_METHOD)
                times.append((time.perf_counter() - start) * 1000.0)
            per_line.append(float(np.median(times)))
        result["mask_ms"] = _timing(per_line)
        print(f"{'算掩膜（' + AUTO_METHOD + '）':15s} 每行中位 {np.median(per_line):7.1f} ms（{np.min(per_line):.1f}–{np.max(per_line):.1f}）", flush=True)

    for method in METHODS:
        if method.run is None or not method.default or method.family not in families:
            continue
        if method.family == "xphoto" and not hasattr(cv2, "xphoto"):
            continue
        per_line = []
        for _, img, mask in data:
            times = []
            for _ in range(repeat):
                start = time.perf_counter()
                method.run(img, mask)
                times.append((time.perf_counter() - start) * 1000.0)
            per_line.append(float(np.median(times)))
        result["methods"][method.key] = _timing(per_line)
        print(f"{method.key:17s} 每行中位 {np.median(per_line):7.1f} ms（{np.min(per_line):.1f}–{np.max(per_line):.1f}）", flush=True)
    result["cpu_after_pct"] = psutil.cpu_percent(interval=1.0)
    return result


def bench_xphoto_elsewhere(python: Path, out_dir: Path, repeat: int, mask_key: str, json_path: Path, priority: str) -> dict[str, Any]:
    """用装了 contrib 的那个 Python 另起一个进程量 xphoto 的方法，结果写到 json_path 再读回来。"""
    cmd = [
        str(python),
        *(["-X", "dev"] if sys.flags.dev_mode else []),
        str(Path(__file__).resolve()),
        "--out",
        str(out_dir),
        "--json",
        str(json_path),
        "--repeat",
        str(repeat),
        "--mask",
        mask_key,
        "--families",
        "xphoto",
        "--skip-lama",
        "--no-contrib",
        "--priority",
        priority,
    ]
    code = subprocess.run(cmd, check=False, cwd=str(Path(__file__).resolve().parent)).returncode
    if code != 0 or not json_path.exists():
        return {"skipped": f"用 {python} 量 xphoto 失败（退出码 {code}）"}
    part = read_json(json_path)
    json_path.unlink()
    return part["traditional"]


def cpu_name() -> str:
    try:
        import winreg

        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0") as key:
            return str(winreg.QueryValueEx(key, "ProcessorNameString")[0]).strip()
    except (ImportError, OSError):
        return platform.processor()


def environment() -> dict[str, Any]:
    env: dict[str, Any] = {
        "when": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "cpu": cpu_name(),
        "cores_physical": psutil.cpu_count(logical=False),
        "cores_logical": os.cpu_count(),
        "memory_gb": round(psutil.virtual_memory().total / 1024**3, 1),
        "os": platform.platform(),
        "python": platform.python_version(),
        "opencv": cv2.__version__,
        "opencv_xphoto": hasattr(cv2, "xphoto"),
        "numpy": np.__version__,
    }
    try:
        import onnxruntime

        env["onnxruntime"] = onnxruntime.__version__
    except ImportError:
        env["onnxruntime"] = None
    return env


def main() -> None:
    ap = argparse.ArgumentParser(description="量 LaMa 一次推理和传统方法每行的耗时")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--json", type=Path, help="结果写到哪里，默认 <out>/bench.json")
    ap.add_argument("--lama-model", type=Path, default=DEFAULT_LAMA_MODEL)
    ap.add_argument("--threads", type=int, nargs="+", default=[0, 4, 2, 1], help="LaMa 的线程设置，0 是 ONNX Runtime 的默认值")
    ap.add_argument("--runs", type=int, default=7, help="LaMa 每个进程里预热之后推理几次")
    ap.add_argument("--processes", type=int, default=5, help="LaMa 每种线程设置起几个进程（建会话的时间、峰值内存每个进程一个数）")
    ap.add_argument("--repeat", type=int, default=5, help="传统方法每行跑几次")
    ap.add_argument("--mask", default="auto-d3")
    ap.add_argument("--families", nargs="+", default=["fill", "opencv", "xphoto"], choices=["fill", "opencv", "xphoto"], help="量哪几类传统方法")
    ap.add_argument("--skip-lama", action="store_true")
    ap.add_argument("--skip-traditional", action="store_true")
    ap.add_argument("--contrib-python", type=Path, default=DEFAULT_CONTRIB_PYTHON, help="装了 opencv-contrib-python-headless 的那个 Python")
    ap.add_argument("--no-contrib", action="store_true", help="当前环境里没有 xphoto 时不去另一个环境量")
    ap.add_argument("--priority", choices=["normal", "high"], default="normal", help="量的进程用什么优先级；high 是为了机器上有别的任务时少受影响，结果里会记下来")
    ap.add_argument("--lama-worker", action="store_true", help=argparse.SUPPRESS)
    args = ap.parse_args()
    set_priority(args.priority)
    if args.lama_worker:
        lama_worker(args.lama_model, args.threads[0], args.runs)
        return
    if args.runs < 5 or args.repeat < 5 or args.processes < 5:
        ap.error("--runs、--repeat、--processes 至少是 5（通用规则：每项至少量 5 次）")
    json_path = args.json or args.out / "bench.json"

    result: dict[str, Any] = {"fmt": "magpdf-v11-bench/1", "environment": environment(), "priority": args.priority}
    if args.skip_lama:
        result["lama"] = {"skipped": "--skip-lama"}
    elif not args.lama_model.exists():
        result["lama"] = {"skipped": f"没有模型文件 {args.lama_model}（先运行 fetch_lama.py）"}
        print(result["lama"]["skipped"])
    else:
        result["lama"] = {
            "model": args.lama_model.name,
            "input": "512×512",
            "configs": bench_lama(args.lama_model, args.threads, args.runs, args.processes, args.priority),
        }
        lama_index = args.out / "results" / args.mask / "lama" / "index.json"
        if lama_index.exists():
            windows = [s["windows"] for s in read_json(lama_index)["samples"].values() if "windows" in s]
            if windows:
                # 一行字要推理几次：流水线里记下的
                result["lama"]["windows_per_line"] = {"median": float(np.median(windows)), "mean": round(float(np.mean(windows)), 2), "max": int(np.max(windows))}
    if args.skip_traditional:
        result["traditional"] = {"skipped": "--skip-traditional"}
    else:
        traditional = bench_traditional(args.out, args.repeat, args.mask, args.families)
        result["traditional"] = traditional
        if "xphoto" in args.families and not hasattr(cv2, "xphoto") and "skipped" not in traditional:
            if args.no_contrib or not args.contrib_python.exists():
                traditional["xphoto"] = {"skipped": f"当前环境的 OpenCV 没有 xphoto，{args.contrib_python} 也用不了"}
                print(traditional["xphoto"]["skipped"])
            else:
                part = bench_xphoto_elsewhere(
                    args.contrib_python, args.out, args.repeat, args.mask, json_path.with_name(json_path.stem + "-xphoto.json"), args.priority
                )
                if "skipped" in part:
                    traditional["xphoto"] = part
                else:
                    traditional["methods"].update(part["methods"])
                    traditional["xphoto"] = {k: part[k] for k in ("opencv", "cpu_before_pct", "cpu_after_pct")}
    write_json(json_path, result)
    print(f"写了 {json_path}")


if __name__ == "__main__":
    main()
