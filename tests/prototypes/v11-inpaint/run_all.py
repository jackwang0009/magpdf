#!/usr/bin/env python3
"""V11 背景修补原型：一条命令重跑整个流程（生成样本 → 修补 → 评价 → 拼图）。

  python tests/prototypes/v11-inpaint/run_all.py

各步按下面的顺序跑；--stages 可以只跑其中几步，--quick 只跑不带 * 的：

  samples     造 50 个样本                                        gen_samples.py
  ocr-input   对带字原图、无字对照图跑 OCR，取行框                  ocr.py
  masks       从行框里分出字的像素，和真值掩膜比                    masks.py
  inpaint     底色填充、cv2.inpaint；掩膜 auto-d3 和 gt-d3          inpaint.py --families fill opencv
  xphoto *    xphoto 的移位图、FSR 快档（要另一个虚拟环境，见 README） <contrib python> inpaint.py --families xphoto
  lama *      LaMa（要先用 fetch_lama.py 下载模型）                 inpaint.py --families lama
  sweep *     掩膜往外扩 1、5、7 像素（就近取色、Telea、FSR 快档、LaMa）；
              不按颜色分的掩膜（就近取色、Telea）                    inpaint.py --masks auto-d1 auto-d5 auto-d7 / gray-d3
  evaluate    客观评价，含对每一组结果跑 OCR                        evaluate.py
  sheets      给人打分用的对照图和打分表（另出一套不写方法名的）      sheets.py、sheets.py --blind

缺了 xphoto 的环境或 LaMa 的模型，对应的步骤跳过，别的照常跑，跳过了什么记在 run.json 里。
全部跑完很慢（时间大半花在对每一组结果跑 OCR、xphoto 和 LaMa 上），--quick 少得多；实际用了多久看 run.json。

输出都在 <out>（默认 tests/out/v11/，不入库）：run.json 记每一步的命令、耗时和环境（用 --stages 只跑几步时记在
run-stages.json 里，不盖掉整趟的记录）；评价结果在 evaluation.json、evaluation.md；对照图在 sheets/。量时间另有 bench.py。

用法：python run_all.py [--out tests/out/v11] [--stages ...] [--quick] [--clean] [--contrib-python ...] [--lama-model ...]
"""
from __future__ import annotations

import argparse
import datetime
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import psutil

from common import DEFAULT_CLI, DEFAULT_LAMA_MODEL, DEFAULT_MODELS_DIR, DEFAULT_OUT, read_json, write_json

HERE = Path(__file__).resolve().parent
STAGES = ["samples", "ocr-input", "masks", "inpaint", "xphoto", "lama", "sweep", "evaluate", "sheets"]
QUICK_STAGES = ["samples", "ocr-input", "masks", "inpaint", "evaluate", "sheets"]
DEFAULT_CONTRIB_PYTHON = Path(os.environ.get("MAGPDF_V11_CONTRIB_PYTHON", r"D:\workspace\magpdf-wt\venvs\v11-contrib\Scripts\python.exe"))
MAIN_MASKS = ["auto-d3", "gt-d3"]
SWEEP_MASKS = ["auto-d1", "auto-d5", "auto-d7"]
SWEEP_METHODS = ["fill-local", "telea-r3", "lama"]
SWEEP_XPHOTO = ["fsr-fast"]


def python_flags() -> list[str]:
    """子进程沿用 -X dev。"""
    return ["-X", "dev"] if sys.flags.dev_mode else []


def has_xphoto(python: Path) -> bool:
    if not python.exists():
        return False
    proc = subprocess.run([str(python), "-c", "import cv2, sys; sys.exit(0 if hasattr(cv2, 'xphoto') else 1)"], check=False, capture_output=True)
    return proc.returncode == 0


def acquire_lock(out_dir: Path) -> Path:
    """同一个输出目录不能两个流程一起跑（中间文件会互相覆盖）。"""
    out_dir.mkdir(parents=True, exist_ok=True)
    lock = out_dir / ".run_all.lock"
    if lock.exists():
        try:
            pid = int(lock.read_text(encoding="utf-8").strip())
        except ValueError:
            pid = -1
        if pid > 0 and pid != os.getpid() and psutil.pid_exists(pid):
            sys.exit(f"另一个 run_all.py（进程 {pid}）正在用 {out_dir}，等它跑完，或者确认它已经不在了之后删掉 {lock}")
    lock.write_text(str(os.getpid()), encoding="utf-8")
    return lock


def main() -> None:
    ap = argparse.ArgumentParser(description="一条命令重跑 V11 背景修补原型的整个流程")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--stages", nargs="+", choices=STAGES, help="只跑这几步")
    ap.add_argument("--quick", action="store_true", help="不跑 xphoto、LaMa 和掩膜外扩的对比")
    ap.add_argument("--clean", action="store_true", help="先把输出目录清空")
    ap.add_argument("--seed", type=int, default=20261112)
    ap.add_argument("--cli", type=Path, default=DEFAULT_CLI)
    ap.add_argument("--models-dir", type=Path, default=DEFAULT_MODELS_DIR)
    ap.add_argument("--lama-model", type=Path, default=DEFAULT_LAMA_MODEL)
    ap.add_argument("--contrib-python", type=Path, default=DEFAULT_CONTRIB_PYTHON, help="装了 opencv-contrib-python-headless 的那个 Python")
    args = ap.parse_args()

    out = args.out.resolve()
    lock = acquire_lock(out)
    if args.clean:
        for child in out.iterdir():
            if child.name == lock.name:
                continue
            if child.is_dir():
                shutil.rmtree(child)
            else:
                child.unlink()
    wanted = args.stages or (QUICK_STAGES if args.quick else STAGES)
    # 只跑几步时另记一个文件，免得把整趟的记录盖掉
    record_path = out / ("run-stages.json" if args.stages else "run.json")
    py = [sys.executable, *python_flags()]
    out_arg = ["--out", str(out)]
    ocr_args = ["--cli", str(args.cli), "--models-dir", str(args.models_dir)]
    have_lama = args.lama_model.exists()
    xphoto_here = has_xphoto(Path(sys.executable))
    have_xphoto = xphoto_here or has_xphoto(args.contrib_python)
    xphoto_py = py if xphoto_here else [str(args.contrib_python), *python_flags()]

    def short(cmd: list[str]) -> str:
        """打印用：只留脚本名和参数；用另一个 Python 跑的标出来。"""
        first = next(i for i, part in enumerate(cmd) if part.endswith(".py"))
        return ("" if cmd[0] == sys.executable else "[contrib] ") + " ".join(cmd[first:])

    plan: dict[str, list[list[str]] | str] = {
        "samples": [[*py, "gen_samples.py", *out_arg, "--seed", str(args.seed)]],
        "ocr-input": [[*py, "ocr.py", *out_arg, *ocr_args]],
        "masks": [[*py, "masks.py", *out_arg]],
        "inpaint": [[*py, "inpaint.py", *out_arg, "--families", "fill", "opencv", "--masks", *MAIN_MASKS]],
        "xphoto": [[*xphoto_py, "inpaint.py", *out_arg, "--families", "xphoto", "--masks", *MAIN_MASKS]]
        if have_xphoto
        else f"没有带 xphoto 的 OpenCV（{args.contrib_python} 不存在或没装 opencv-contrib-python-headless）",
        "lama": [[*py, "inpaint.py", *out_arg, "--families", "lama", "--masks", *MAIN_MASKS, "--lama-model", str(args.lama_model)]]
        if have_lama
        else f"没有 LaMa 模型文件 {args.lama_model}（先运行 fetch_lama.py）",
        "sweep": [
            [*py, "inpaint.py", *out_arg, "--masks", *SWEEP_MASKS, "--methods", *[m for m in SWEEP_METHODS if m != "lama" or have_lama], "--lama-model", str(args.lama_model)],
            *([[*xphoto_py, "inpaint.py", *out_arg, "--masks", *SWEEP_MASKS, "--methods", *SWEEP_XPHOTO]] if have_xphoto else []),
            [*py, "inpaint.py", *out_arg, "--masks", "gray-d3", "--methods", "fill-local", "telea-r3"],
        ],
        "evaluate": [[*py, "evaluate.py", *out_arg, *ocr_args]],
        "sheets": [[*py, "sheets.py", *out_arg], [*py, "sheets.py", *out_arg, "--blind"]],
    }

    record: dict[str, Any] = {
        "fmt": "magpdf-v11-run/1",
        "started": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "seed": args.seed,
        "cpu_before_pct": psutil.cpu_percent(interval=1.0),
        "stages": [],
    }
    failed = False
    try:
        for stage in STAGES:
            if stage not in wanted:
                continue
            commands = plan[stage]
            if isinstance(commands, str):
                print(f"== {stage}：跳过，{commands}", flush=True)
                record["stages"].append({"name": stage, "skipped": commands})
                continue
            start = time.perf_counter()
            for cmd in commands:
                print(f"== {stage}：{short(cmd)}", flush=True)
                code = subprocess.run(cmd, cwd=str(HERE), check=False).returncode
                if code != 0:
                    failed = True
                    record["stages"].append({"name": stage, "failed": code, "command": cmd})
                    break
            if failed:
                break
            seconds = round(time.perf_counter() - start, 1)
            record["stages"].append({"name": stage, "seconds": seconds, "commands": commands})
            print(f"== {stage}：{seconds:.0f} 秒", flush=True)
    finally:
        record["finished"] = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
        record["cpu_after_pct"] = psutil.cpu_percent(interval=1.0)
        try:
            from bench import environment

            record["environment"] = environment()
        except ImportError as err:
            record["environment"] = {"error": str(err)}
        if (out / "samples" / "index.json").exists():
            record["samples"] = len(read_json(out / "samples" / "index.json")["samples"])
        record["outputs"] = {
            name: path
            for name, path in {
                "评价": "evaluation.json",
                "评价的表": "evaluation.md",
                "掩膜和真值的比较": "masks.json",
                "带字原图的 OCR": "ocr-input.json",
                "对照图": "sheets/",
                "不写方法名的对照图": "sheets-blind/",
            }.items()
            if (out / path).exists()
        }
        write_json(record_path, record)
        lock.unlink(missing_ok=True)
    if failed:
        sys.exit(f"有一步失败了，见上面的输出；{record_path} 里记了是哪一步")
    print(f"跑完了，记录在 {record_path}")


if __name__ == "__main__":
    main()
