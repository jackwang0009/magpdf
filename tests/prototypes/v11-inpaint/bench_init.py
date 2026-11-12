#!/usr/bin/env python3
"""V11 背景修补原型：LaMa 建会话的时间花在哪。一条命令重量：python tests/prototypes/v11-inpaint/bench_init.py

bench.py 量出来建一次会话要十秒以上。这里看这段时间省不省得掉：同一个模型按三种办法建会话，每种 --repeat 次
（默认 5 次，三种轮流着来，免得整机负载的变化只落在一种上），结果写 <out>/bench-init.json：

  default          默认设置，ONNX Runtime 的图优化全开（bench.py 和流水线用的就是这种）
  no_optimization  关掉图优化
  preoptimized     先让 ONNX Runtime 把优化之后的模型存成一个文件，再读这个文件，不再做图优化

每建一次会话，预热一次，再推理 --runs 次（默认 3 次），所以每种办法有 repeat × runs 次推理；另外记下输出和默认设置的
输出差多少（最大绝对差，输出是 0–255 的像素值）。这里的推理时间只算 session.run，不含前后处理，是三种办法之间比着看的，
要一次推理的数看 bench.py。

存下来的优化模型放在 --tmp 指的目录里（约 200 MB），量完就删。ONNX Runtime 对这样存下来的模型有一句警告：
它可能带着和这台机器有关的优化，只该在生成它的环境里用（存的那一次把这句警告关掉了，免得刷屏）。

用法：python bench_init.py [--out tests/out/v11] [--json <out>/bench-init.json] [--repeat 5] [--runs 3] [--tmp <目录>]
"""
from __future__ import annotations

import argparse
import os
import tempfile
import time
from pathlib import Path
from typing import Any

import numpy as np
import onnxruntime as ort
import psutil

from bench import environment, synthetic_input
from common import DEFAULT_LAMA_MODEL, DEFAULT_OUT, write_json

CONFIGS = ["default", "no_optimization", "preoptimized"]


def spread(values: list[float], digits: int) -> dict[str, Any]:
    return {
        "median": round(float(np.median(values)), digits),
        "min": round(float(np.min(values)), digits),
        "max": round(float(np.max(values)), digits),
        "n": len(values),
    }


def make_session(
    path: Path, level: ort.GraphOptimizationLevel, save_to: Path | None = None
) -> tuple[ort.InferenceSession, float]:
    """建一个会话，返回会话和用了多少秒。save_to 不是 None 时让 ONNX Runtime 把优化之后的模型存到那里。"""
    options = ort.SessionOptions()
    options.graph_optimization_level = level
    if save_to is not None:
        options.optimized_model_filepath = str(save_to)
        options.log_severity_level = 3  # 只报错误：存优化模型时那句“和机器有关”的警告在文件开头的说明里写了
    start = time.perf_counter()
    session = ort.InferenceSession(str(path), options, providers=["CPUExecutionProvider"])
    return session, time.perf_counter() - start


def main() -> None:
    ap = argparse.ArgumentParser(description="量 LaMa 建会话的时间：默认、关掉图优化、读事先优化好的模型")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--json", type=Path, help="结果写到哪里，默认 <out>/bench-init.json")
    ap.add_argument("--lama-model", type=Path, default=DEFAULT_LAMA_MODEL)
    ap.add_argument("--repeat", type=int, default=5, help="每种办法建几次会话")
    ap.add_argument("--runs", type=int, default=3, help="每个会话预热之后推理几次")
    ap.add_argument("--tmp", type=Path, default=Path(tempfile.gettempdir()), help="优化之后的模型临时存在哪个目录")
    args = ap.parse_args()
    if args.repeat < 5:
        ap.error("--repeat 至少是 5（通用规则：每项至少量 5 次）")
    if not args.lama_model.exists():
        ap.error(f"没有模型文件 {args.lama_model}（先运行 fetch_lama.py）")
    json_path = args.json or args.out / "bench-init.json"

    img, mask = synthetic_input()
    feed = {
        "image": np.ascontiguousarray(img[..., ::-1].transpose(2, 0, 1)[None], dtype=np.float32) / 255.0,
        "mask": np.ascontiguousarray((mask > 0)[None, None], dtype=np.float32),
    }
    all_level, no_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL, ort.GraphOptimizationLevel.ORT_DISABLE_ALL
    optimized = args.tmp / f"lama-optimized-{os.getpid()}.onnx"
    result: dict[str, Any] = {
        "fmt": "magpdf-v11-bench-init/1",
        "environment": environment(),
        "model": args.lama_model.name,
        "repeat": args.repeat,
        "runs_per_session": args.runs,
        "cpu_before_pct": psutil.cpu_percent(interval=1.0),
    }
    try:
        # 先存一份优化之后的模型，顺便拿默认设置的输出当比较的基准
        session, seconds = make_session(args.lama_model, all_level, optimized)
        reference = session.run(None, feed)[0]
        del session
        result["save_optimized_s"] = round(seconds, 2)
        result["optimized_size_mb"] = round(optimized.stat().st_size / 1024 / 1024, 1)
        print(f"存优化之后的模型：{seconds:.1f} s，{result['optimized_size_mb']:.0f} MB", flush=True)

        plan = {
            "default": (args.lama_model, all_level),
            "no_optimization": (args.lama_model, no_level),
            "preoptimized": (optimized, no_level),
        }
        inits: dict[str, list[float]] = {name: [] for name in CONFIGS}
        infers: dict[str, list[float]] = {name: [] for name in CONFIGS}
        diffs: dict[str, float] = {name: 0.0 for name in CONFIGS}
        for round_no in range(args.repeat):
            for name in CONFIGS:
                path, level = plan[name]
                session, seconds = make_session(path, level)
                inits[name].append(seconds)
                out = session.run(None, feed)[0]  # 预热
                for _ in range(args.runs):
                    start = time.perf_counter()
                    out = session.run(None, feed)[0]
                    infers[name].append((time.perf_counter() - start) * 1000.0)
                diffs[name] = max(diffs[name], float(np.max(np.abs(out - reference))))
                del session
            print(
                f"第 {round_no + 1}/{args.repeat} 轮：建会话 "
                + "，".join(f"{name} {inits[name][-1]:.1f} s" for name in CONFIGS),
                flush=True,
            )
        result["configs"] = {
            name: {
                "init_s": spread(inits[name], 2),
                "infer_ms": spread(infers[name], 1),
                "max_abs_diff_vs_default": round(diffs[name], 4),
            }
            for name in CONFIGS
        }
        for name in CONFIGS:
            cfg = result["configs"][name]
            print(
                f"{name:16s} 建会话中位 {cfg['init_s']['median']:.2f} s（{cfg['init_s']['min']:.2f}–{cfg['init_s']['max']:.2f}），"
                f"推理中位 {cfg['infer_ms']['median']:.0f} ms（{cfg['infer_ms']['min']:.0f}–{cfg['infer_ms']['max']:.0f}，{cfg['infer_ms']['n']} 次），"
                f"输出和默认设置最多差 {cfg['max_abs_diff_vs_default']}",
                flush=True,
            )
    finally:
        optimized.unlink(missing_ok=True)
    result["cpu_after_pct"] = psutil.cpu_percent(interval=1.0)
    write_json(json_path, result)
    print(f"写了 {json_path}")


if __name__ == "__main__":
    main()
