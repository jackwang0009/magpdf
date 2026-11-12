"""一条命令重跑 V10 的全部测量，结果写到 tests/out/v10/（JSON，不入库）。

    D:\\workspace\\magpdf-wt\\venvs\\v10\\Scripts\\python.exe tests/perf/gpu/run_all.py

做的事（已经有结果的步骤默认跳过，`--force` 全部重来）：

1. prepare      生成 OCR 合成样本，用固定的命令行工具跑一遍 OCR 得到文字行框，算出模型输入的缓存。
2. env          记下显卡、驱动、DirectML、ONNX Runtime 的版本；有 Windows ML 的虚拟环境时看一眼它的目录。
3. sizes        安装包大小：从 NuGet 取几个包，列出要随包带的 DLL 有多大（第一次要下载约 530 MB；`--quick` 不做）。
4. timing       每页推理耗时、初始化时间、内存：每种配置各起 5 个进程，配置之间轮流跑。
5. server       服务器版模型，同上（页数少一些）。
6. consistency  同一个输入，CPU 和 DirectML 的输出差多少。
7. nodes        DirectML 下各模型的节点落在哪个执行提供程序上。
8. shapes       识别模型换输入宽度的代价：头一次、连着跑、轮着跑。
9. widths       识别模型的宽度分档：产品现在的分法、按 320 一档补齐、每档一个固定形状的会话。
10. soak        连续处理 1000 页。
11. summarize   汇总成 summary.json 和 summary.md（报告里的表从这里抄）。

配置：
- `cpu-<版本>`：CPU 版 ONNX Runtime（另一个虚拟环境，和产品用的版本一致）。
- `cpu-<版本>-dmlpkg`：DirectML 包里自带的 CPU 执行提供程序（和 DirectML 是同一个构建）。
- `dml<序号>-<显卡名>`：DirectML 跑在某一块显卡上；本机 DXGI 里能看到的硬件显卡各量一遍，同名的只量第一块。
- `winml-<版本>-dml<序号>-<显卡名>`：Windows ML 那份 ONNX Runtime 自带的 DirectML 执行提供程序
  （第三个虚拟环境，没有就不量；只量每页耗时）。
- 标签后面带 `-prio` 的：同一个配置，测量进程的优先级调到“高于正常”。开发机上有别的任务在抢 CPU 时，
  正常优先级量出来的 CPU 推理时间会被拉长，这一份更接近机器空闲时的情况。timing、server 两步里它和
  正常优先级的轮流跑，两份都留；shapes、widths、soak 三步只量这一份。`--no-priority-pass` 全部按正常优先级量。
"""
from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, Iterable, Sequence

import sysinfo

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
BENCH = HERE / "bench.py"
STEPS = ("prepare", "env", "sizes", "timing", "server", "consistency", "nodes", "shapes", "widths", "soak", "summarize")
WIDTH_VARIANTS = ("product", "step320-pad6", "step320-pad6-static")
PRIO_SUFFIX = "-prio"


@dataclass(frozen=True)
class Target:
    """一种要量的配置：用哪个 Python（哪个 ONNX Runtime 包）、哪个执行提供程序、哪块显卡。"""

    label: str
    python: Path
    provider: str
    device_id: int | None = None
    adapter_name: str | None = None
    winml: bool = False  # Windows ML 那份 ONNX Runtime；只量每页耗时
    priority: str = "normal"

    def args(self) -> list[str]:
        out = ["--provider", self.provider, "--priority", self.priority]
        if self.device_id is not None:
            out += ["--device-id", str(self.device_id)]
        return out

    def raised(self) -> Target:
        """同一个配置，测量进程的优先级调到“高于正常”；标签后面加 `-prio`。"""
        return replace(self, label=self.label + PRIO_SUFFIX, priority="above-normal")


def _slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def _ort_version(python: Path) -> str | None:
    try:
        out = subprocess.run(
            [str(python), "-c", "import onnxruntime as o; print(o.__version__)"],
            capture_output=True,
            text=True,
            timeout=300,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    lines = out.stdout.strip().splitlines()
    return lines[-1] if out.returncode == 0 and lines else None


def discover_targets(
    python_dml: Path, python_cpu: Path | None, python_winml: Path | None, all_adapters: bool
) -> tuple[list[Target], list[str]]:
    """找出这台机器上能量的配置。返回 `(配置, 说明)`；说明里记下没法量的东西。"""
    notes: list[str] = []
    targets: list[Target] = []
    if python_cpu is not None and python_cpu.is_file():
        version = _ort_version(python_cpu)
        if version:
            targets.append(Target(f"cpu-{version}", python_cpu, "cpu"))
        else:
            notes.append(f"{python_cpu} 里没有可用的 onnxruntime，CPU 版这一项没有量")
    else:
        notes.append("没有给 CPU 版 ONNX Runtime 的虚拟环境（--python-cpu），只量了 DirectML 包自带的 CPU 执行提供程序")
    dml_version = _ort_version(python_dml)
    if dml_version is None:
        raise SystemExit(f"{python_dml} 里没有可用的 onnxruntime（要装 onnxruntime-directml）")
    targets.append(Target(f"cpu-{dml_version}-dmlpkg", python_dml, "cpu"))
    seen: set[tuple[int, int, str]] = set()
    for adapter in sysinfo.hardware_adapters():
        key = (adapter.vendor_id, adapter.device_id, adapter.name)
        if key in seen and not all_adapters:
            notes.append(f"DXGI 适配器 {adapter.index}（{adapter.name}）和前面的同名，没有再量")
            continue
        seen.add(key)
        targets.append(
            Target(f"dml{adapter.index}-{_slug(adapter.name)}", python_dml, "dml", adapter.index, adapter.name)
        )
    if python_winml is not None and python_winml.is_file():
        version = _ort_version(python_winml)
        if version:
            for t in [t for t in targets if t.provider == "dml"]:
                targets.append(Target(f"winml-{version}-{t.label}", python_winml, "dml", t.device_id, t.adapter_name, True))
        else:
            notes.append(f"{python_winml} 里没有可用的 onnxruntime，Windows ML 那份运行时没有量")
    else:
        notes.append("没有 Windows ML 的虚拟环境（--python-winml），Windows ML 那份运行时没有量")
    for controller in sysinfo.video_controllers():
        code = controller.get("config_manager_error_code")
        if code not in (0, None):
            notes.append(
                f"显卡 {controller.get('name')} 在设备管理器里是错误状态（代码 {code}），DXGI 里没有它，DirectML 量不了"
            )
    return targets, notes


class Runner:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.out: Path = args.out
        self.runs_dir = self.out / "runs"
        self.runs_dir.mkdir(parents=True, exist_ok=True)
        self.samples: Path = args.samples
        self.models_dir: Path = args.models_dir
        self.failures: list[str] = []

    # -- 小工具 ------------------------------------------------------------

    def log(self, message: str) -> None:
        print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)

    def cache(self, models: str) -> Path:
        return self.out / f"cache-{models}"

    def bench(self, name: str, target: Target, mode: str, extra: Sequence[str], timeout: float) -> dict[str, Any] | None:
        """起一个进程量一项；结果文件已经有且成功就不重跑。"""
        path = self.runs_dir / f"{name}.json"
        if path.is_file() and not self.args.force:
            data = json.loads(path.read_text(encoding="utf-8"))
            if data.get("ok"):
                return data
        cmd = [
            str(target.python),
            str(BENCH),
            mode,
            *target.args(),
            "--models-dir",
            str(self.models_dir),
            "--out",
            str(path),
            "--label",
            target.label,
            *extra,
        ]
        if self.args.dev:
            cmd[1:1] = ["-X", "dev"]
        started = time.perf_counter()
        env = dict(os.environ, PYTHONUTF8="1")
        try:
            done = subprocess.run(cmd, cwd=HERE, env=env, timeout=timeout, check=False, capture_output=True)
            code = done.returncode
            stderr = done.stderr.decode("utf-8", errors="replace").strip()
        except subprocess.TimeoutExpired:
            code, stderr = -1, f"超过 {timeout:.0f} 秒没有跑完"
        elapsed = time.perf_counter() - started
        if self.args.dev and stderr:
            self.log(f"  {name} 的标准错误输出：{stderr[-400:]}")
        if code != 0 or not path.is_file():
            self.failures.append(f"{name}：退出码 {code}；{stderr[-300:]}")
            self.log(f"  {name} 失败（退出码 {code}，{elapsed:.0f} 秒）")
            return json.loads(path.read_text(encoding="utf-8")) if path.is_file() else None
        self.log(f"  {name} 完成（{elapsed:.0f} 秒）")
        return json.loads(path.read_text(encoding="utf-8"))

    # -- 各步 --------------------------------------------------------------

    def prepare(self) -> None:
        images = self.samples / "images"
        if not images.is_dir() or not any(images.glob("*.png")):
            self.log("生成 OCR 合成样本")
            script = ROOT / "tests" / "corpus" / "synthetic-ocr" / "gen_synthetic.py"
            subprocess.run([sys.executable, str(script), "--out", str(self.samples)], check=True, capture_output=True)
        for models, flags in (("mobile", []), ("server", ["--server"])):
            cli_out = self.out / f"cli-{models}"
            n_images = len(list(images.glob("*.png")))
            if len(list(cli_out.glob("*.json"))) != n_images or self.args.force:
                self.log(f"命令行工具跑一遍 OCR（{models}）取文字行框")
                cli_out.mkdir(parents=True, exist_ok=True)
                subprocess.run(
                    [str(self.args.cli), "ocr", str(images), "--json", str(cli_out), "--models-dir", str(self.models_dir), *flags],
                    check=True,
                    capture_output=True,
                )
            cache = self.cache(models)
            if not (cache / "index.json").is_file() or self.args.force:
                self.log(f"算模型输入的缓存（{models}）")
                subprocess.run(
                    [sys.executable, str(HERE / "workload.py"), "--images", str(images), "--ocr-json", str(cli_out), "--out", str(cache)],
                    check=True,
                    capture_output=True,
                )

    def env(self, targets: Sequence[Target], notes: Sequence[str]) -> None:
        info = sysinfo.collect()
        info["targets"] = [
            {
                "label": t.label,
                "python": str(t.python),
                "provider": t.provider,
                "device_id": t.device_id,
                "adapter": t.adapter_name,
                "priority": t.priority,
            }
            for t in targets
        ]
        info["notes"] = list(notes)
        info["cli"] = str(self.args.cli)
        info["models_dir"] = str(self.models_dir)
        info["collected"] = time.strftime("%Y-%m-%d %H:%M:%S")
        (self.out / "env.json").write_text(json.dumps(info, ensure_ascii=False, indent=1), encoding="utf-8")
        winml = [t for t in targets if t.winml]
        if winml:
            # Windows ML 的目录对这台机器列出了哪些执行提供程序（只看，不下载）。
            subprocess.run(
                [str(winml[0].python), str(HERE / "winml_probe.py"), "--out", str(self.out / "winml.json")],
                cwd=HERE,
                env=dict(os.environ, PYTHONUTF8="1"),
                timeout=600,
                check=False,
                capture_output=True,
            )

    def sizes(self) -> None:
        """安装包大小。下载过的包留在 D 盘的缓存目录里，不重下。"""
        path = self.out / "pkg-sizes.json"
        if path.is_file() and not self.args.force:
            return
        self.log("安装包大小：从 NuGet 取几个包")
        done = subprocess.run(
            [sys.executable, str(HERE / "pkg_sizes.py"), "--out", str(path)],
            cwd=HERE,
            env=dict(os.environ, PYTHONUTF8="1"),
            timeout=3600,
            check=False,
            capture_output=True,
        )
        if done.returncode != 0:
            self.failures.append(f"sizes：退出码 {done.returncode}；{done.stderr.decode('utf-8', errors='replace')[-300:]}")

    def timing(self, targets: Sequence[Target], models: str) -> None:
        a = self.args
        extra = ["--models", models, "--cache", str(self.cache(models)), "--passes", str(a.passes if models == "mobile" else a.server_passes)]
        limit = a.page_limit if models == "mobile" else a.server_page_limit
        if limit:
            extra += ["--page-limit", str(limit)]
        for run in range(1, a.runs + 1):
            for target in targets:
                self.bench(f"timing-{models}-{target.label}-{run}", target, "timing", extra, timeout=a.timeout)

    def consistency(self, targets: Sequence[Target]) -> None:
        a = self.args
        for target in targets:
            for models, limit in (("mobile", a.page_limit), ("server", a.server_page_limit)):
                extra = ["--models", models, "--cache", str(self.cache(models))]
                if limit:
                    extra += ["--page-limit", str(limit)]
                self.bench(f"consistency-{models}-{target.label}", target, "consistency", extra, timeout=a.timeout)

    def nodes(self, targets: Sequence[Target]) -> None:
        for target in targets:
            for models in ("mobile", "server"):
                extra = ["--models", models, "--cache", str(self.cache(models))]
                self.bench(f"nodes-{models}-{target.label}", target, "nodes", extra, timeout=self.args.timeout)

    def shapes(self, targets: Sequence[Target]) -> None:
        extra = ["--models", "mobile", "--cache", str(self.cache("mobile"))]
        for run in range(1, self.args.runs + 1):
            for target in targets:
                self.bench(f"shapes-{target.label}-{run}", target, "shapes", extra, timeout=self.args.timeout)

    def widths(self, targets: Sequence[Target]) -> None:
        """只量识别模型。三种分法：产品现在的、按 320 一档并把批补满、同样分档但每档一个固定形状的会话。"""
        a = self.args
        flags: dict[str, list[str]] = {
            "product": ["--width-policy", "product"],
            "step320-pad6": ["--width-policy", "step320-pad6"],
            "step320-pad6-static": ["--width-policy", "step320-pad6", "--static-shapes"],
        }
        base = ["--models", "mobile", "--cache", str(self.cache("mobile")), "--stages", "rec", "--passes", "2"]
        if a.widths_page_limit:
            base += ["--page-limit", str(a.widths_page_limit)]
        for run in range(1, a.runs + 1):
            for target in targets:
                for name in WIDTH_VARIANTS:
                    self.bench(f"widths-{name}-{target.label}-{run}", target, "timing", base + flags[name], timeout=a.timeout)

    def soak(self, targets: Sequence[Target]) -> None:
        a = self.args
        extra = ["--models", "mobile", "--cache", str(self.cache("mobile")), "--pages", str(a.soak_pages)]
        if a.soak_page_limit:
            extra += ["--page-limit", str(a.soak_page_limit)]
        for target in targets:
            self.bench(f"soak-mobile-{target.label}", target, "soak", extra, timeout=a.soak_timeout)


# ---------------------------------------------------------------------------
# 汇总
# ---------------------------------------------------------------------------


def _mmm(values: Iterable[float | None]) -> dict[str, float | int | None]:
    """几次测量的中位数、最小、最大。"""
    vals = [float(v) for v in values if v is not None]
    if not vals:
        return {"n": 0, "median": None, "min": None, "max": None}
    return {"n": len(vals), "median": round(statistics.median(vals), 1), "min": round(min(vals), 1), "max": round(max(vals), 1)}


def _fmt(m: dict[str, Any] | None, digits: int = 0, unit: str = "") -> str:
    """`中位数（最小–最大）`；没有量就写“没有量”。"""
    if not m or m.get("median") is None:
        return "没有量"
    f = f"{{:.{digits}f}}"
    return f"{f.format(m['median'])}（{f.format(m['min'])}–{f.format(m['max'])}）{unit}"


def _num(value: Any, digits: int = 0, unit: str = "") -> str:
    if value is None:
        return "没有量"
    return f"{value:.{digits}f}{unit}"


def _label_order(label: str) -> tuple[bool, int, str]:
    """表里的行序：正常优先级的在前，调高优先级的在后；各自里面产品用的 CPU 版在最前（别的行拿它作对照），
    然后是 DirectML 包里的 CPU、DirectML、Windows ML。"""
    raised = label.endswith(PRIO_SUFFIX)
    base = label.removesuffix(PRIO_SUFFIX)
    if base.startswith("cpu-"):
        group = 1 if base.endswith("-dmlpkg") else 0
    else:
        group = 2 if base.startswith("dml") else 3
    return (raised, group, base)


def _width_variant(data: dict[str, Any]) -> str:
    return data.get("width_policy", "product") + ("-static" if data["config"].get("static_shapes") else "")


def _load(runs_dir: Path, prefix: str, variant: str | None = None) -> dict[str, list[dict[str, Any]]]:
    """读回 `<prefix>-<配置>-<序号>.json`，按配置分组；失败的不算。`variant` 只挑某一种宽度分法。"""
    grouped: dict[str, list[dict[str, Any]]] = {}
    for path in sorted(runs_dir.glob(f"{prefix}-*.json")):
        data = json.loads(path.read_text(encoding="utf-8"))
        if not data.get("ok"):
            continue
        if variant is not None and _width_variant(data) != variant:
            continue
        grouped.setdefault(data.get("label") or path.stem, []).append(data)
    return {label: grouped[label] for label in sorted(grouped, key=_label_order)}


def _gpu(run: dict[str, Any]) -> dict[str, Any]:
    return run.get("memory", {}).get("gpu", {})


def _gpu_peak(run: dict[str, Any], kind: str) -> float | None:
    peaks = _gpu(run).get("peak_mb_by_luid", {})
    if not peaks:
        return None
    return round(sum(v.get(kind, 0.0) for v in peaks.values()), 1)


def _gpu_own_util(run: dict[str, Any]) -> float | None:
    util = _gpu(run).get("own_utilization_percent_by_luid", {})
    return max((v["mean"] for v in util.values()), default=None)


def _gpu_others(run: dict[str, Any]) -> float | None:
    """量的时候同一块显卡上别的进程的占用（整段的平均）。CPU 配置不碰显卡，没有这个数。"""
    return max(_gpu(run).get("others_utilization_mean_percent_by_luid", {}).values(), default=None)


def _cpu_others(run: dict[str, Any]) -> float | None:
    return (run.get("cpu_during") or {}).get("others_percent")


def _cpu_own(run: dict[str, Any]) -> float | None:
    return (run.get("cpu_during") or {}).get("own_percent")


def summarize_timing(runs_dir: Path, prefix: str, variant: str | None = None) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for label, runs in _load(runs_dir, prefix, variant).items():
        s = [r["summary"] for r in runs]
        first_pass = [p for p in runs[0]["pages"] if p["pass"] == 0]
        result[label] = {
            "runs": len(runs),
            "provider": runs[0]["config"]["provider"],
            "priority": runs[0].get("priority", "normal"),
            "onnxruntime": runs[0]["onnxruntime"]["version"],
            "adapter": (runs[0]["config"].get("adapter") or {}).get("name"),
            "directml": (runs[0].get("directml_module") or {}).get("version"),
            "pages_per_pass": len(first_pass),
            "lines_per_page": round(statistics.fmean(p["lines"] for p in first_pass), 1),
            "steady_page_ms": _mmm(x["steady_page"]["median"] for x in s),
            "steady_page_mean_ms": _mmm(x["steady_page"]["mean"] for x in s),
            "steady_page_max_ms": _mmm(x["steady_page"]["max"] for x in s),
            "steady_det_ms": _mmm(x["steady_det"]["median"] for x in s),
            "steady_cls_ms": _mmm(x["steady_cls"]["median"] for x in s),
            "steady_rec_ms": _mmm(x["steady_rec"]["median"] for x in s),
            "steady_rec_ms_per_line": _mmm(x["steady_rec_ms_per_line"] for x in s),
            "pass0_page_ms": _mmm(x["pass0_page"]["median"] for x in s),
            "pass0_page_mean_ms": _mmm(x["pass0_page"]["mean"] for x in s),
            "import_ms": _mmm(x["import_ms"] for x in s),
            "sessions_ms": _mmm(x["sessions_ms"] for x in s),
            "static_session_ms": _mmm(x["static_session_ms"] for x in s),
            "static_sessions_created": s[0]["static_sessions_created"],
            "first_page_ms": _mmm(x["first_page_ms"] for x in s),
            "cold_start_ms": _mmm(x["cold_start_ms"] for x in s),
            "new_shape_extra_ms_median": _mmm(x["new_shape"]["extra_ms"]["median"] for x in s),
            "new_shape_extra_ms_total": _mmm(x["new_shape"]["extra_ms_total"] for x in s),
            "new_shape_ratio_median": _mmm(x["new_shape"]["ratio"]["median"] for x in s),
            "rec_shapes": s[0]["new_shape"]["shapes"],
            "working_set_after_sessions_mb": _mmm(r["memory"]["after_sessions"]["working_set_mb"] for r in runs),
            "peak_working_set_mb": _mmm(r["memory"]["at_end"]["peak_working_set_mb"] for r in runs),
            "gpu_dedicated_peak_mb": _mmm(_gpu_peak(r, "dedicated") for r in runs),
            "gpu_shared_peak_mb": _mmm(_gpu_peak(r, "shared") for r in runs),
            "gpu_own_utilization_mean_percent": _mmm(_gpu_own_util(r) for r in runs),
            "cpu_own_percent": _mmm(_cpu_own(r) for r in runs),
            "cpu_others_percent": _mmm(_cpu_others(r) for r in runs),
            "gpu_others_percent": _mmm(_gpu_others(r) for r in runs),
            "cpu_percent_before": _mmm(r["load_before"].get("cpu_percent") for r in runs),
        }
    return result


def summarize_shapes(runs_dir: Path) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for label, runs in _load(runs_dir, "shapes").items():
        s = [r["summary"] for r in runs]
        entry: dict[str, Any] = {"runs": len(runs), "repeats": s[0]["repeats"]}
        for name in ("a", "b"):
            entry[name] = {
                "width": s[0]["pair"][name]["width"],
                "same_ms": _mmm(x["pair"][name]["same_ms"]["median"] for x in s),
                "first_extra_ms": _mmm(x["pair"][name]["first_extra_ms"] for x in s),
                "alternate_extra_ms": _mmm(x["pair"][name]["alternate_extra_ms"] for x in s),
            }
        entry["cycle_widths"] = [c["width"] for c in s[0]["cycle"]]
        entry["cycle_same_ms"] = _mmm(x["cycle_same_ms"]["median"] for x in s)
        entry["cycle_first_extra_ms"] = _mmm(x["cycle_first_extra_ms"]["median"] for x in s)
        entry["cycle_cycled_extra_ms"] = _mmm(x["cycle_cycled_extra_ms"]["median"] for x in s)
        result[label] = entry
    return result


def summarize(out: Path, failures: Sequence[str]) -> dict[str, Any]:
    runs_dir = out / "runs"
    summary: dict[str, Any] = {
        "generated": time.strftime("%Y-%m-%d %H:%M:%S"),
        "timing_mobile": summarize_timing(runs_dir, "timing-mobile"),
        "timing_server": summarize_timing(runs_dir, "timing-server"),
        "widths": {variant: summarize_timing(runs_dir, "widths", variant) for variant in WIDTH_VARIANTS},
        "shapes": summarize_shapes(runs_dir),
        "consistency": {},
        "nodes": {},
        "soak": {},
        "failures": list(failures),
    }
    for path in sorted(runs_dir.glob("consistency-*.json")):
        data = json.loads(path.read_text(encoding="utf-8"))
        summary["consistency"][path.stem] = data["summary"] if data.get("ok") else {"error": data.get("error")}
    for path in sorted(runs_dir.glob("nodes-*.json")):
        if "-profile-" in path.name:  # ONNX Runtime 自己写的性能记录，中途出错时会留下
            continue
        data = json.loads(path.read_text(encoding="utf-8"))
        summary["nodes"][path.stem] = data["summary"] if data.get("ok") else {"error": data.get("error")}
    for path in sorted(runs_dir.glob("soak-*.json")):
        data = json.loads(path.read_text(encoding="utf-8"))
        partial = path.name.endswith(".partial.json")
        if partial and path.with_name(path.name.removesuffix(".partial.json") + ".json").is_file():
            continue  # 中途存的那一份；后来跑完了，以跑完的为准
        if not data.get("ok") and not partial:
            summary["soak"][path.stem] = {"error": data.get("error")}
            continue
        entry = dict(data["summary"])
        entry["partial"] = partial
        entry["cpu_others_percent"] = _cpu_others(data)
        entry["cpu_own_percent"] = _cpu_own(data)
        entry["gpu_others_percent"] = _gpu_others(data)
        entry["gpu_own_utilization_mean_percent"] = _gpu_own_util(data)
        entry["gpu_peak_mb_by_luid"] = _gpu(data).get("peak_mb_by_luid")
        entry["windows_median_ms"] = [w["median_ms"] for w in data["windows"]]
        entry["windows_mean_ms"] = [w.get("mean_ms") for w in data["windows"]]
        summary["soak"][path.stem] = entry
    for name, file in (("env", "env.json"), ("winml", "winml.json"), ("sizes", "pkg-sizes.json")):
        path = out / file
        if path.is_file():
            summary[name] = json.loads(path.read_text(encoding="utf-8"))
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=1), encoding="utf-8")
    (out / "summary.md").write_text(render_markdown(summary), encoding="utf-8")
    return summary


def _timing_table(title: str, data: dict[str, Any]) -> list[str]:
    if not data:
        return [f"### {title}", "", "没有量。", ""]
    first = next(iter(data.values()))
    lines = [
        f"### {title}",
        "",
        f"每遍 {first['pages_per_pass']} 页，平均每页 {first['lines_per_page']} 行文字。“每页”是稳定后（第 2 遍起）各页的中位数。",
        "",
        "| 配置 | 次数 | 每页 | 是第一行的几倍 | 每页（平均） | 检测 | 方向分类 | 识别 | 识别每行 | 别的进程占的 CPU | 同一块显卡上别的进程 |",
        "|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    base = first["steady_page_ms"]["median"]
    for label, t in data.items():
        median = t["steady_page_ms"]["median"]
        times = f"{median / base:.2f}" if base and median else "—"
        gpu_others = _fmt(t["gpu_others_percent"], unit=" %") if t["provider"] != "cpu" else "—"
        lines.append(
            f"| `{label}` | {t['runs']} | {_fmt(t['steady_page_ms'])} | {times} | {_fmt(t['steady_page_mean_ms'])} | "
            f"{_fmt(t['steady_det_ms'])} | {_fmt(t['steady_cls_ms'])} | {_fmt(t['steady_rec_ms'])} | "
            f"{_fmt(t['steady_rec_ms_per_line'], 1)} | {_fmt(t['cpu_others_percent'], unit=' %')} | {gpu_others} |"
        )
    lines += [
        "",
        "| 配置 | 装载运行时 | 建三个会话 | 第一页 | 三段合计 | 头一遍每页 | 建完会话时的工作集 | 峰值工作集 | 显存峰值（专用 / 共享） | 本进程占显卡 | 本进程占 CPU |",
        "|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for label, t in data.items():
        if t["provider"] == "cpu":
            gpu_mem, gpu_util = "—", "—"
        else:
            gpu_mem = f"{_fmt(t['gpu_dedicated_peak_mb'])} / {_fmt(t['gpu_shared_peak_mb'])} MB"
            gpu_util = _fmt(t["gpu_own_utilization_mean_percent"], unit=" %")
        lines.append(
            f"| `{label}` | {_fmt(t['import_ms'])} | {_fmt(t['sessions_ms'])} | {_fmt(t['first_page_ms'])} | {_fmt(t['cold_start_ms'])} | "
            f"{_fmt(t['pass0_page_ms'])} | {_fmt(t['working_set_after_sessions_mb'], unit=' MB')} | "
            f"{_fmt(t['peak_working_set_mb'], unit=' MB')} | {gpu_mem} | {gpu_util} | {_fmt(t['cpu_own_percent'], unit=' %')} |"
        )
    lines.append("")
    return lines


def _shapes_table(data: dict[str, Any]) -> list[str]:
    lines = ["### 识别模型换输入宽度的代价（一批 6 行，移动版）", ""]
    if not data:
        return lines + ["没有量。", ""]
    lines += [
        "| 配置 | 次数 | 宽度 | 连着跑一次 | 头一次多花 | 和另一种宽度轮着跑多花 |",
        "|---|---|---|---|---|---|",
    ]
    for label, t in data.items():
        for name in ("a", "b"):
            e = t[name]
            lines.append(
                f"| `{label}` | {t['runs']} | {e['width']} | {_fmt(e['same_ms'], 1)} | {_fmt(e['first_extra_ms'], 1)} | "
                f"{_fmt(e['alternate_extra_ms'], 1)} |"
            )
    lines += [
        "",
        "| 配置 | 八种宽度 | 连着跑一次（八种的中位数） | 头一次多花 | 八种轮着跑多花 |",
        "|---|---|---|---|---|",
    ]
    for label, t in data.items():
        widths = "、".join(str(w) for w in t["cycle_widths"])
        lines.append(
            f"| `{label}` | {widths} | {_fmt(t['cycle_same_ms'], 1)} | {_fmt(t['cycle_first_extra_ms'], 1)} | "
            f"{_fmt(t['cycle_cycled_extra_ms'], 1)} |"
        )
    lines.append("")
    return lines


def _soak_table(data: dict[str, Any]) -> list[str]:
    lines = ["### 连续处理（soak，移动版）", ""]
    if not data:
        return lines + ["没有量。", ""]
    lines += [
        "| 运行 | 页数 | 报错 | 重建会话 | 用时 | 每页中位数 | 第 2 个窗口 → 最后一个窗口（中位数） | 同上（平均） | 工作集每 100 页涨 | 共享显存每 100 页涨 | 峰值工作集 | 别的进程占的 CPU | 同一块显卡上别的进程 |",
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for name, s in data.items():
        if "error" in s:
            lines.append(f"| `{name}` | 失败：{s['error']} | | | | | | | | | | | |")
            continue
        drift = s.get("drift")
        if drift:
            by_median = f"{drift['early_median_ms']:.0f} → {drift['late_median_ms']:.0f}（×{drift['late_over_early']:.3f}）"
            by_mean = (
                f"{drift['early_mean_ms']:.0f} → {drift['late_mean_ms']:.0f}（×{drift['late_over_early_mean']:.3f}）"
                if "early_mean_ms" in drift
                else "没有量"
            )
        else:
            by_median = by_mean = "没有量（窗口不够 3 个）"
        growth = s.get("memory_growth") or {}

        def grow(key: str) -> str:
            g = growth.get(key)
            return f"{g['mb_per_100_pages']:+.2f} MB（{g['first']:.0f} → {g['last']:.0f}）" if g else "没有量"

        is_cpu = "-cpu-" in name
        shown = f"`{name.removesuffix('.partial')}`（没跑完，进程中途停了）" if s.get("partial") else f"`{name}`"
        lines.append(
            f"| {shown} | {s['pages_done']}/{s['pages_requested']} | {s['errors']} | {s['engine_rebuilds']} | {s['wall_s'] / 60:.0f} 分 | "
            f"{s['page']['median']:.0f} | {by_median} | {by_mean} | {grow('working_set_mb')} | "
            f"{'—' if is_cpu else grow('gpu_shared_mb')} | {s['peak_working_set_mb']:.0f} MB | "
            f"{_num(s.get('cpu_others_percent'), unit=' %')} | {'—' if is_cpu else _num(s.get('gpu_others_percent'), unit=' %')} |"
        )
    lines.append("")
    return lines


def _sizes_table(data: dict[str, Any] | None) -> list[str]:
    lines = ["### 安装包大小（NuGet 包里的 64 位 DLL；1 MB = 100 万字节）", ""]
    if not data:
        return lines + ["没有量。", ""]
    lines += [
        "| 做法 | 要带的文件（来自哪个包） | 合计（字节） | 合计 | 比现在多 | LZMA 压过之后 | 压过之后比现在多 |",
        "|---|---|---|---|---|---|---|",
    ]
    mb = 1_000_000
    for route in data["routes"]:
        files = "；".join(
            f"`{f['file']}` {f['bytes']:,}（{f['package']} {f['package_version']}，文件版本 {f['file_version']}）" for f in route["files"]
        )
        lines.append(
            f"| {route['name']} | {files} | {route['bytes']:,} | {route['bytes'] / mb:.1f} MB | "
            f"{route['extra_bytes'] / mb:+.1f} MB | {route['lzma_bytes'] / mb:.1f} MB | {route['extra_lzma_bytes'] / mb:+.1f} MB |"
        )
    lines.append("")
    return lines


def render_markdown(summary: dict[str, Any]) -> str:
    lines = [
        "# V10 测量汇总（run_all.py 生成）",
        "",
        f"生成时间：{summary['generated']}。时间单位 ms，写法是“中位数（最小–最大）”：每次运行是一个新进程，"
        "先在进程里取各页的中位数，再对各次运行取中位数、最小、最大。",
        "“别的进程占的 CPU”是量的那一段时间里，整机 CPU 时间有百分之几被别的进程用掉（100 % = 所有逻辑处理器全满）。",
        f"标签带 `{PRIO_SUFFIX}` 的是把测量进程的优先级调到“高于正常”量的：机器上有别的任务抢 CPU 时，这一份更接近机器空闲时的情况。",
        "",
    ]
    env = summary.get("env") or {}
    cpu = env.get("cpu") or {}
    if cpu.get("name"):
        lines.append(f"- 处理器：{cpu['name']}（{cpu.get('cores')} 核 {cpu.get('logical')} 线程），内存 {cpu.get('memory_gb')} GB，{env.get('os')}")
    for controller in env.get("video_controllers", []):
        lines.append(
            f"- 显卡设备：{controller.get('name')}，驱动 {controller.get('driver_version')}（{controller.get('driver_date')}），"
            f"状态 {controller.get('status')}，设备管理器代码 {controller.get('config_manager_error_code')}"
        )
    for note in env.get("notes", []):
        lines.append(f"- {note}")
    winml = summary.get("winml")
    if winml:
        if winml.get("ok"):
            names = [f"{p['name']}（{p['ready_state']}）" for p in winml["catalog_providers"]]
            ort = winml["onnxruntime"]
            lines.append(
                f"- Windows ML 目录对这台机器列出的执行提供程序：{'、'.join(names) if names else '一个也没有'}；"
                f"它自带的 ONNX Runtime {ort['version']} 里有 {'、'.join(ort['available_providers'])}"
            )
        else:
            lines.append(f"- Windows ML 的目录没有看成：{winml.get('error')}")
    lines.append("")
    for title, key in (("移动版模型：每页推理耗时、初始化、内存", "timing_mobile"), ("服务器版模型", "timing_server")):
        normal = {k: v for k, v in summary[key].items() if not k.endswith(PRIO_SUFFIX)}
        raised = {k: v for k, v in summary[key].items() if k.endswith(PRIO_SUFFIX)}
        lines += _timing_table(title, normal)
        if raised:
            lines += _timing_table(title + "（测量进程的优先级调到“高于正常”，和上表轮流量的）", raised)
    lines += ["### 新形状头一次多花的时间（移动版，产品现在的分法，整页流程里顺带量的）", ""]
    lines += ["| 配置 | 形状种数 | 每种形状头一次比之后多花（中位数） | 一遍下来合计多花 | 头一次 ÷ 之后 |", "|---|---|---|---|---|"]
    for label, t in summary["timing_mobile"].items():
        lines.append(
            f"| `{label}` | {t['rec_shapes']} | {_fmt(t['new_shape_extra_ms_median'], 1)} | {_fmt(t['new_shape_extra_ms_total'])} | "
            f"{_fmt(t['new_shape_ratio_median'], 2)} |"
        )
    lines.append("")
    lines += _shapes_table(summary["shapes"])
    lines += ["### 识别模型的宽度分档（只量识别这一段，移动版）", ""]
    lines += [
        "| 分法 | 配置 | 次数 | 形状种数 | 头一遍每页 | 稳定后每页 | 稳定后每页（平均） | 建固定形状的会话（个数，合计用时） | 峰值工作集 | 显存峰值（共享） |",
        "|---|---|---|---|---|---|---|---|---|---|",
    ]
    for variant, data in summary["widths"].items():
        for label, t in data.items():
            static = f"{t['static_sessions_created']} 个，{_fmt(t['static_session_ms'])}" if t["static_sessions_created"] else "—"
            shared = _fmt(t["gpu_shared_peak_mb"], unit=" MB") if t["provider"] != "cpu" else "—"
            lines.append(
                f"| {variant} | `{label}` | {t['runs']} | {t['rec_shapes']} | {_fmt(t['pass0_page_ms'])} | {_fmt(t['steady_page_ms'])} | "
                f"{_fmt(t['steady_page_mean_ms'])} | {static} | {_fmt(t['peak_working_set_mb'], unit=' MB')} | {shared} |"
            )
    lines.append("")
    lines += _soak_table(summary["soak"])
    lines += ["### 输出一致性（同一个构建里的 CPU 对 DirectML）", ""]
    lines += [
        "| 运行 | 页 | 检测概率图最大差 | 二值化后不同的像素 | 方向判定不同的行 | 识别输出最大差 | 解码路径不同的行 | 文字不同的行 | 和命令行工具的文字不同的行（CPU / DirectML） | DirectML 两次运行的最大差（检测 / 分类 / 识别） |",
        "|---|---|---|---|---|---|---|---|---|---|",
    ]
    for name, s in summary["consistency"].items():
        if "error" in s:
            lines.append(f"| `{name}` | 失败：{s['error']} | | | | | | | | |")
            continue
        r = s["dml_run_to_run_max_abs"]
        cli = s["texts_vs_cli"]
        lines.append(
            f"| `{name}` | {s['pages']} | {s['det']['max_abs']:.2e} | {s['det']['binarized_pixels_that_differ']} / {s['det']['pixels']} | "
            f"{s['cls']['decisions_differ']} / {s['cls']['lines']} | {s['rec']['max_abs']:.2e} | "
            f"{s['rec']['argmax_paths_differ']} / {s['rec']['lines']} | {s['rec']['texts_differ']} / {s['rec']['lines']} | "
            f"{cli['cpu_differ']} / {cli['dml_differ']} | {r['det']:.1e} / {r['cls']:.1e} / {r['rec']:.1e} |"
        )
    lines += ["", "### 节点落在哪个执行提供程序上（第二次运行）", ""]
    lines += ["| 运行 | 模型 | 输入形状 | 各执行提供程序上的节点数 | 没落在所要的执行提供程序上的算子 | 这一次运行用时 |", "|---|---|---|---|---|---|"]
    for name, s in summary["nodes"].items():
        if "error" in s:
            lines.append(f"| `{name}` | 失败：{s['error']} | | | | |")
            continue
        for stage, d in s.items():
            parts = "；".join(f"{k.replace('ExecutionProvider', '')} {v['nodes']} 个" for k, v in d["by_provider"].items())
            elsewhere = "、".join(f"{op}×{n}" for op, n in (d.get("ops_not_on_requested_provider") or {}).items()) or "—"
            lines.append(
                f"| `{name}` | {stage} | {'×'.join(str(x) for x in d['input_shape'])} | {parts} | {elsewhere} | {_num(d.get('model_run_ms'), 1)} |"
            )
    lines.append("")
    lines += _sizes_table(summary.get("sizes"))
    if summary["failures"]:
        lines += ["", "### 没有跑成的", ""] + [f"- {f}" for f in summary["failures"]]
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    prefix = Path(sys.prefix)
    ap.add_argument("--out", type=Path, default=ROOT / "tests" / "out" / "v10")
    ap.add_argument("--samples", type=Path, default=ROOT / "tests" / "corpus" / "synthetic-ocr" / "out")
    ap.add_argument("--models-dir", type=Path, default=Path(os.environ.get("MAGPDF_MODELS_DIR", r"D:\workspace\magpdf\models")))
    ap.add_argument("--cli", type=Path, default=Path(os.environ.get("MAGPDF_CLI", r"D:\workspace\magpdf-wt\bin\magpdf-cli.exe")))
    ap.add_argument(
        "--python-cpu",
        type=Path,
        default=prefix.with_name(prefix.name + "-cpu") / "Scripts" / "python.exe",
        help="装了 CPU 版 onnxruntime 的虚拟环境里的 python.exe",
    )
    ap.add_argument(
        "--python-winml",
        type=Path,
        default=prefix.with_name(prefix.name + "-winml") / "Scripts" / "python.exe",
        help="装了 Windows ML 那份 onnxruntime 的虚拟环境里的 python.exe（没有就不量）",
    )
    ap.add_argument("--runs", type=int, default=5, help="每种配置起几个进程")
    ap.add_argument("--passes", type=int, default=3, help="移动版：整套页面过几遍")
    ap.add_argument("--page-limit", type=int, default=None, help="移动版只用几页（默认全部）")
    ap.add_argument("--server-passes", type=int, default=2)
    ap.add_argument("--server-page-limit", type=int, default=6)
    ap.add_argument("--widths-page-limit", type=int, default=9)
    ap.add_argument("--soak-pages", type=int, default=1000)
    ap.add_argument("--soak-page-limit", type=int, default=None, help="连续处理时只轮着用几页（默认全部）")
    ap.add_argument("--timeout", type=float, default=3600.0, help="一个进程最多跑多少秒（soak 除外）")
    ap.add_argument("--soak-timeout", type=float, default=6 * 3600.0)
    ap.add_argument("--only", default="", help="只做这几步，逗号分隔：" + "、".join(STEPS))
    ap.add_argument("--skip", default="", help="跳过这几步")
    ap.add_argument("--targets", default="", help="只量标签里含这些字样的配置，逗号分隔（如 cpu-1.31,dml0）")
    ap.add_argument("--all-adapters", action="store_true", help="同名的 DXGI 适配器也各量一遍")
    ap.add_argument("--no-priority-pass", action="store_true", help="不量“测量进程优先级调高一档”的那一份")
    ap.add_argument("--force", action="store_true", help="已经有结果的也重跑")
    ap.add_argument("--quick", action="store_true", help="只为了看脚本跑不跑得通：1 次、2 页、soak 20 页，结果另放在 <out>-quick")
    ap.add_argument("--dev", action="store_true", help="子进程带 -X dev 运行并打印它们的标准错误输出（检查有没有警告，不用来量时间）")
    args = ap.parse_args()

    if args.quick:
        args.runs, args.passes, args.page_limit = 1, 2, 2
        args.server_page_limit, args.widths_page_limit = 2, 2
        args.soak_pages, args.soak_page_limit = 20, 2
        # 试跑的结果另放一处，免得被正式的那一轮当成“已经量过”。
        args.out = args.out.with_name(args.out.name + "-quick")
    only = [s for s in args.only.split(",") if s]
    skip = [s for s in args.skip.split(",") if s]
    unknown = [s for s in only + skip if s not in STEPS]
    if unknown:
        ap.error(f"不认识的步骤：{unknown}")
    steps = [s for s in STEPS if (not only or s in only) and s not in skip]

    runner = Runner(args)
    targets, notes = discover_targets(Path(sys.executable), args.python_cpu, args.python_winml, args.all_adapters)
    wanted = [w for w in args.targets.split(",") if w]
    if wanted:
        targets = [t for t in targets if any(w in t.label for w in wanted)]
    runner.log("配置：" + "、".join(t.label for t in targets))
    for note in notes:
        runner.log("注意：" + note)

    cpu_targets = [t for t in targets if t.provider == "cpu"]
    dml_targets = [t for t in targets if t.provider == "dml" and not t.winml]
    # 宽度、连续处理这几项只拿一个 CPU 配置作对照：有产品用的那个版本就用它。
    control = cpu_targets[:1]
    # 别的任务在抢 CPU 时，正常优先级量出来的 CPU 推理时间会被拉长。对照的 CPU 配置和各块显卡
    # 再各量一份优先级调高一档的，和正常优先级的轮流跑。
    raised = [] if args.no_priority_pass else [t.raised() for t in control + dml_targets]
    if raised:
        runner.log("优先级调高一档再量一份的：" + "、".join(t.label for t in raised))
    # 换宽度的代价、宽度分档、连续处理这三项看的是同一个配置自己前后比，别的任务一抢 CPU 就看不清，
    # 所以直接用优先级调高一档的那一份来量（不量那一份时才用正常优先级的）。
    detail = raised or (control + dml_targets)

    if "prepare" in steps:
        runner.prepare()
    if "env" in steps:
        runner.env(targets + raised, notes)
    if "sizes" in steps and (not args.quick or "sizes" in only):
        runner.sizes()
    if "timing" in steps:
        runner.log("每页推理耗时（移动版）")
        runner.timing(targets + raised, "mobile")
    if "server" in steps:
        runner.log("每页推理耗时（服务器版）")
        runner.timing(targets + raised, "server")
    if "consistency" in steps:
        runner.log("输出一致性")
        runner.consistency(dml_targets)
    if "nodes" in steps:
        runner.log("节点落在哪个执行提供程序上")
        # Windows ML 那份运行时也看一眼：它的 DirectML 和官方 DirectML 包不是同一个构建。
        runner.nodes(control + dml_targets + [t for t in targets if t.winml])
    if "shapes" in steps:
        runner.log("识别模型换输入宽度的代价")
        runner.shapes(detail)
    if "widths" in steps:
        runner.log("识别模型的宽度分档")
        runner.widths(detail)
    if "soak" in steps:
        runner.log(f"连续处理 {args.soak_pages} 页")
        runner.soak(detail)
    if "summarize" in steps:
        summarize(args.out, runner.failures)
        runner.log(f"汇总写到 {args.out / 'summary.md'}")
    for failure in runner.failures:
        runner.log("失败：" + failure)
    return 1 if runner.failures else 0


if __name__ == "__main__":
    sys.exit(main())
