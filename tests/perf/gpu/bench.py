"""一个进程量一种配置：CPU 或者 DirectML 的某一块显卡（V10）。

只量模型推理这一段（`InferenceSession.run`，含把输入送进去、把输出取回来），前后处理不算。
输入是 workload.py 预先算好的张量，和产品里 Rust 的预处理一致。每种配置各起一个新进程，
这样会话初始化、第一次推理、峰值内存都是干净的。一般由 run_all.py 调用。

    python bench.py timing      --provider cpu|dml [--device-id N] --cache <目录> --out <结果.json>
    python bench.py soak        --provider ...  --pages 1000 ...
    python bench.py consistency --device-id N ...          # 同一个输入，CPU 和 DirectML 的输出差多少
    python bench.py nodes       --provider dml --device-id N ...   # 各模型的节点落在哪个执行提供程序上
    python bench.py shapes      --provider ...             # 识别模型换输入宽度的代价

会话的设置和 `engine/magpdf-scan/src/ocr/runtime.rs` 一样：图优化开到最高一级，线程数交给 ONNX Runtime。
DirectML 另按它的要求关掉内存模式优化、用顺序执行。
"""
from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Sequence

_T_START = time.perf_counter()

import numpy as np  # noqa: E402
from numpy.typing import NDArray  # noqa: E402

import sysinfo  # noqa: E402
import workload as wl  # noqa: E402

F32 = NDArray[np.float32]

MODEL_SETS: dict[str, dict[str, str]] = {
    "mobile": {
        "det": "PP-OCRv5_mobile_det.onnx",
        "cls": "PP-LCNet_x0_25_textline_ori.onnx",
        "rec": "PP-OCRv5_mobile_rec.onnx",
        "dict": "ppocrv5_dict.txt",
    },
    "server": {
        "det": "PP-OCRv5_server_det.onnx",
        "cls": "PP-LCNet_x0_25_textline_ori.onnx",
        "rec": "PP-OCRv5_server_rec.onnx",
        "dict": "ppocrv5_dict.txt",
    },
}

# 模型输入里可变维度的名字（paddle2onnx 起的）。固定形状的会话按名字把它们定死。
DIM_BATCH = "DynamicDimension.0"
DET_DIM_H, DET_DIM_W = "DynamicDimension.1", "DynamicDimension.2"
REC_DIM_W = "DynamicDimension.1"

WIDTH_POLICIES: dict[str, wl.WidthPolicy] = {
    "product": wl.PRODUCT_POLICY,
    "step32-pad6": wl.WidthPolicy("step32-pad6", step=32, pad_batch=True),
    "step320-pad6": wl.WidthPolicy("step320-pad6", step=320, pad_batch=True),
}


@dataclass(frozen=True)
class Config:
    provider: str  # "cpu" 或 "dml"
    device_id: int | None
    models: str
    models_dir: Path
    threads: int = 0
    static_shapes: bool = False
    stages: tuple[str, ...] = ("det", "cls", "rec")
    provider_options: tuple[tuple[str, str], ...] = ()
    session_config: tuple[tuple[str, str], ...] = ()


class ProviderError(RuntimeError):
    """要的执行提供程序没有起来（Python 接口会悄悄回退到 CPU，这里当成错误）。"""


def _import_ort() -> Any:
    import onnxruntime as ort

    return ort


def make_session(cfg: Config, model: Path, dims: dict[str, int] | None = None, profile: Path | None = None) -> Any:
    ort = _import_ort()
    so = ort.SessionOptions()
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    so.log_severity_level = 3
    if cfg.threads > 0:
        so.intra_op_num_threads = cfg.threads
    for name, value in (dims or {}).items():
        so.add_free_dimension_override_by_name(name, value)
    for key, value in cfg.session_config:
        so.add_session_config_entry(key, value)
    if profile is not None:
        so.enable_profiling = True
        so.profile_file_prefix = str(profile)
    if cfg.provider == "dml":
        so.enable_mem_pattern = False
        so.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
        options = {"device_id": str(cfg.device_id if cfg.device_id is not None else 0)}
        options.update(dict(cfg.provider_options))
        providers: list[Any] = [("DmlExecutionProvider", options)]
        expect = "DmlExecutionProvider"
    elif cfg.provider == "cpu":
        providers = ["CPUExecutionProvider"]
        expect = "CPUExecutionProvider"
    else:
        raise ValueError(f"不认识的执行提供程序 {cfg.provider!r}")
    session = ort.InferenceSession(str(model), so, providers=providers)
    actual = session.get_providers()
    if not actual or actual[0] != expect:
        raise ProviderError(f"要的是 {expect}，实际是 {actual}")
    return session


@dataclass
class BatchTiming:
    n: int
    w: int
    ms: float
    first_seen: bool


@dataclass
class PageTiming:
    name: str
    lines: int
    det_ms: float = 0.0
    cls_ms: float = 0.0
    rec_ms: float = 0.0
    rec_batches: list[BatchTiming] = field(default_factory=list)

    @property
    def total_ms(self) -> float:
        return self.det_ms + self.cls_ms + self.rec_ms


@dataclass
class PageOutputs:
    det: F32 | None = None
    cls: list[F32] = field(default_factory=list)
    rec: list[F32] = field(default_factory=list)


class Engine:
    """三个模型的会话。`static_shapes` 时每种输入形状各建一个把维度定死的会话（用到时才建，时间算进那一次）。"""

    def __init__(self, cfg: Config) -> None:
        self.cfg = cfg
        files = MODEL_SETS[cfg.models]
        self._paths = {k: cfg.models_dir / "ocr" / files[k] for k in ("det", "cls", "rec")}
        self.session_ms: dict[str, float] = {}
        self._dynamic: dict[str, Any] = {}
        self._static: dict[tuple[str, tuple[int, ...]], Any] = {}
        self.static_sessions_created = 0
        self.static_session_ms = 0.0
        self._seen_shapes: set[tuple[str, tuple[int, ...]]] = set()
        if not cfg.static_shapes:
            for stage in ("det", "cls", "rec"):
                if stage in cfg.stages:
                    t = time.perf_counter()
                    self._dynamic[stage] = make_session(cfg, self._paths[stage])
                    self.session_ms[stage] = _ms(t)
        self.input_name = "x"

    def rec_classes(self) -> int:
        """识别模型输出的类别数（最后一维）。"""
        return int(self._dynamic["rec"].get_outputs()[0].shape[-1])

    def _session_for(self, stage: str, shape: tuple[int, ...]) -> Any:
        if not self.cfg.static_shapes:
            return self._dynamic[stage]
        key = (stage, shape)
        session = self._static.get(key)
        if session is None:
            if stage == "det":
                dims = {DIM_BATCH: shape[0], DET_DIM_H: shape[2], DET_DIM_W: shape[3]}
            elif stage == "cls":
                dims = {DIM_BATCH: shape[0]}
            else:
                dims = {DIM_BATCH: shape[0], REC_DIM_W: shape[3]}
            t = time.perf_counter()
            session = make_session(self.cfg, self._paths[stage], dims)
            self.static_session_ms += _ms(t)
            self.static_sessions_created += 1
            self._static[key] = session
        return session

    def run_one(self, stage: str, x: F32) -> tuple[F32, float, bool]:
        """跑一次。固定形状的会话是第一次用到时才建的，建会话的时间也算在这一次里。"""
        shape = tuple(int(d) for d in x.shape)
        first = (stage, shape) not in self._seen_shapes
        self._seen_shapes.add((stage, shape))
        t = time.perf_counter()
        session = self._session_for(stage, shape)
        out = session.run(None, {self.input_name: x})[0]
        return out, _ms(t), first

    def run_page(self, page: wl.Page, rec_inputs: Sequence[F32] | None = None, keep: bool = False) -> tuple[PageTiming, PageOutputs]:
        timing = PageTiming(page.name, page.lines)
        outputs = PageOutputs()
        if "det" in self.cfg.stages:
            out, ms, _ = self.run_one("det", page.det)
            timing.det_ms = ms
            if keep:
                outputs.det = out
        if "cls" in self.cfg.stages:
            for batch in page.cls:
                n = batch.shape[0]
                if self.cfg.static_shapes and n < wl.CLS_BATCH:
                    # 固定形状的会话批大小恒为 6，不足的用零补上。
                    batch = np.concatenate([batch, np.zeros((wl.CLS_BATCH - n, *batch.shape[1:]), dtype=np.float32)])
                out, ms, _ = self.run_one("cls", batch)
                timing.cls_ms += ms
                if keep:
                    outputs.cls.append(out[:n])
        if "rec" in self.cfg.stages:
            for batch in rec_inputs if rec_inputs is not None else page.rec:
                out, ms, first = self.run_one("rec", batch)
                timing.rec_ms += ms
                timing.rec_batches.append(BatchTiming(int(batch.shape[0]), int(batch.shape[3]), round(ms, 3), first))
                if keep:
                    outputs.rec.append(out)
        return timing, outputs


def _ms(start: float) -> float:
    return (time.perf_counter() - start) * 1000.0


def _write_json(path: Path, data: dict[str, Any]) -> None:
    """先写到旁边的临时文件再改名，读的人不会看到写了一半的文件。"""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(data, ensure_ascii=False, indent=1), encoding="utf-8")
    tmp.replace(path)


def _median(values: Sequence[float]) -> float | None:
    return round(statistics.median(values), 2) if values else None


def _stats(values: Sequence[float]) -> dict[str, float | int | None]:
    if not values:
        return {"n": 0, "median": None, "min": None, "max": None, "mean": None, "p95": None}
    ordered = sorted(values)
    p95 = ordered[min(len(ordered) - 1, int(round(0.95 * (len(ordered) - 1))))]
    return {
        "n": len(values),
        "median": round(statistics.median(values), 2),
        "min": round(ordered[0], 2),
        "max": round(ordered[-1], 2),
        "mean": round(statistics.fmean(values), 2),
        "p95": round(p95, 2),
    }


def _page_files(cache: Path, limit: int | None) -> list[Path]:
    files = sorted(cache.glob("*.npz"))
    if not files:
        raise FileNotFoundError(f"{cache} 里没有输入缓存，先跑 workload.py")
    if limit:
        # 取样时在各语言、各变体之间均匀挑，别只拿到排在前面的那一种。
        step = len(files) / limit
        files = [files[int(i * step)] for i in range(min(limit, len(files)))]
    return files


def _adapter_info(cfg: Config) -> dict[str, Any] | None:
    if cfg.provider != "dml":
        return None
    for adapter in sysinfo.enum_adapters():
        if adapter.index == (cfg.device_id or 0):
            return asdict(adapter) | {"vendor": adapter.vendor}
    return None


def _config_json(cfg: Config) -> dict[str, Any]:
    data = asdict(cfg)
    data["models_dir"] = str(cfg.models_dir)
    data["provider_options"] = dict(cfg.provider_options)
    data["session_config"] = dict(cfg.session_config)
    data["adapter"] = _adapter_info(cfg)
    return data


def _dml_module() -> dict[str, Any] | None:
    """这个进程实际装载的是哪一份 DirectML.dll。"""
    path = sysinfo.loaded_module_path("DirectML.dll")
    if path is None:
        return None
    return {"path": str(path), "version": sysinfo.file_version(path), "size": path.stat().st_size}


def _rec_inputs(page: wl.Page, policy: wl.WidthPolicy) -> list[F32] | None:
    if policy is wl.PRODUCT_POLICY:
        return None
    tensors, _ = wl.rec_batches(page.crops, policy)
    return tensors


# ---------------------------------------------------------------------------
# timing：初始化、第一页、稳定后的每页耗时、内存
# ---------------------------------------------------------------------------


def run_timing(cfg: Config, args: argparse.Namespace) -> dict[str, Any]:
    policy = WIDTH_POLICIES[args.width_policy]
    need_crops = policy is not wl.PRODUCT_POLICY
    files = _page_files(args.cache, args.page_limit)
    load_before = sysinfo.gpu_load_snapshot(1.0)
    sampler = sysinfo.GpuMemorySampler(0.5)
    sampler.start()
    cpu_before = sysinfo.cpu_times()

    t = time.perf_counter()
    engine = Engine(cfg)
    sessions_ms = _ms(t)
    mem_sessions = sysinfo.process_memory()

    pages: list[dict[str, Any]] = []
    for p in range(args.passes):
        for path in files:
            page = wl.load_page(path, with_crops=need_crops)
            timing, _ = engine.run_page(page, _rec_inputs(page, policy))
            pages.append(
                {
                    "pass": p,
                    "name": timing.name,
                    "lines": timing.lines,
                    "det_ms": round(timing.det_ms, 2),
                    "cls_ms": round(timing.cls_ms, 2),
                    "rec_ms": round(timing.rec_ms, 2),
                    "total_ms": round(timing.total_ms, 2),
                    "rec_batches": [asdict(b) for b in timing.rec_batches],
                }
            )
    cpu_during = sysinfo.cpu_share(cpu_before, sysinfo.cpu_times())
    mem_end = sysinfo.process_memory()
    gpu_mem = sampler.stop()
    load_after = sysinfo.gpu_load_snapshot(1.0)

    steady = [p for p in pages if p["pass"] >= 1] or pages
    first = pages[0]
    lines = sum(p["lines"] for p in steady)
    summary: dict[str, Any] = {
        # 冷启动：装载 ONNX Runtime、建三个会话、第一页的推理，三段加起来。
        "import_ms": round(args.import_ms, 1),
        "sessions_ms": round(sessions_ms, 1),
        "session_ms_by_model": {k: round(v, 1) for k, v in engine.session_ms.items()},
        "static_sessions_created": engine.static_sessions_created,
        "static_session_ms": round(engine.static_session_ms, 1),
        "first_page_ms": first["total_ms"],
        "first_page": {k: first[k] for k in ("name", "det_ms", "cls_ms", "rec_ms", "lines")},
        "cold_start_ms": round(args.import_ms + sessions_ms + first["total_ms"], 1),
        "pass0_page": _stats([p["total_ms"] for p in pages if p["pass"] == 0]),
        "steady_page": _stats([p["total_ms"] for p in steady]),
        "steady_det": _stats([p["det_ms"] for p in steady]),
        "steady_cls": _stats([p["cls_ms"] for p in steady]),
        "steady_rec": _stats([p["rec_ms"] for p in steady]),
        "steady_rec_ms_per_line": round(sum(p["rec_ms"] for p in steady) / lines, 2) if lines else None,
        "steady_passes": sorted({p["pass"] for p in steady}),
        "new_shape": _new_shape_penalty(pages),
    }
    return {
        "load_before": load_before,
        "load_after": load_after,
        "cpu_during": cpu_during,
        "summary": summary,
        "memory": {
            "before_import": asdict(args.mem_before_import),
            "after_sessions": asdict(mem_sessions),
            "at_end": asdict(mem_end),
            "gpu": gpu_mem,
        },
        "directml_module": _dml_module(),
        "width_policy": args.width_policy,
        "pages": pages,
    }


def _new_shape_penalty(pages: Sequence[dict[str, Any]]) -> dict[str, Any]:
    """识别批次里，一种形状第一次出现那一次比之后各次慢多少（只看出现过两次以上的形状）。"""
    by_shape: dict[tuple[int, int], dict[str, list[float]]] = {}
    for page in pages:
        for b in page["rec_batches"]:
            slot = by_shape.setdefault((b["n"], b["w"]), {"first": [], "later": []})
            slot["first" if b["first_seen"] else "later"].append(b["ms"])
    extra: list[float] = []
    ratio: list[float] = []
    for slot in by_shape.values():
        if slot["first"] and slot["later"]:
            later = statistics.median(slot["later"])
            extra.append(slot["first"][0] - later)
            if later > 0:
                ratio.append(slot["first"][0] / later)
    return {
        "shapes": len(by_shape),
        "shapes_compared": len(extra),
        "extra_ms": _stats(extra),
        "ratio": _stats(ratio),
        "extra_ms_total": round(sum(extra), 1) if extra else None,
    }


# ---------------------------------------------------------------------------
# soak：连续很多页，看报错、内存、耗时漂不漂
# ---------------------------------------------------------------------------


def run_soak(cfg: Config, args: argparse.Namespace) -> dict[str, Any]:
    files = _page_files(args.cache, args.page_limit)
    load_before = sysinfo.gpu_load_snapshot(1.0)
    sampler = sysinfo.GpuMemorySampler(1.0)
    sampler.start()
    cpu_before = sysinfo.cpu_times()
    engine = Engine(cfg)
    totals: list[float] = []
    samples: list[dict[str, Any]] = []
    errors: list[dict[str, Any]] = []
    rebuilds = 0
    started = time.perf_counter()

    def result(final: bool) -> dict[str, Any]:
        """到现在为止的结果。`final` 为假时是中途存的一份，不停采样。"""
        wall_s = time.perf_counter() - started
        cpu_during = sysinfo.cpu_share(cpu_before, sysinfo.cpu_times())
        mem_end = sysinfo.process_memory()
        gpu_mem = sampler.stop() if final else sampler.snapshot()
        load_after = sysinfo.gpu_load_snapshot(1.0) if final else None
        good = [(i, v) for i, v in enumerate(totals) if v == v]
        # 页面是轮着用的，各页行数不一样；窗口取整数轮（约 100 页），每个窗口里的页面才是同一套。
        cycles = max(1, min(round(100 / len(files)), len(good) // (3 * len(files))))
        window = len(files) * cycles
        windows = []
        for start in range(0, len(good) - window + 1, window):
            values = [v for _, v in good[start : start + window]]
            windows.append(
                {"from": start, "to": start + window, "median_ms": _median(values), "mean_ms": round(statistics.fmean(values), 2)}
            )
        # 头一个窗口里各种形状都是第一次出现，漂移从第二个窗口起比：第二个对最后一个。
        drift = None
        if len(windows) >= 3:
            early, late = windows[1], windows[-1]
            drift = {
                "early_median_ms": early["median_ms"],
                "late_median_ms": late["median_ms"],
                "late_over_early": round(late["median_ms"] / early["median_ms"], 3),
                "early_mean_ms": early["mean_ms"],
                "late_mean_ms": late["mean_ms"],
                "late_over_early_mean": round(late["mean_ms"] / early["mean_ms"], 3),
            }
        return {
            "load_before": load_before,
            "load_after": load_after,
            "cpu_during": cpu_during,
            "summary": {
                "pages_requested": args.pages,
                "pages_done": len(good),
                "distinct_pages": len(files),
                "errors": len(errors),
                "engine_rebuilds": rebuilds,
                "wall_s": round(wall_s, 1),
                "page": _stats([v for _, v in good]),
                "window_pages": window,
                "drift": drift,
                "memory_growth": _growth(samples),
                "peak_working_set_mb": mem_end.peak_working_set_mb,
            },
            "errors": errors,
            "windows": windows,
            "memory": {"at_end": asdict(mem_end), "gpu": gpu_mem, "samples": samples},
            "directml_module": _dml_module(),
            "page_total_ms": [round(v, 1) if v == v else None for v in totals],
        }

    # 这一项要跑很久。每隔一段把到目前为止的结果另存一份，进程中途被结束时不至于什么都没留下；跑完就删掉。
    partial = args.out.with_suffix(".partial.json")
    for i in range(args.pages):
        path = files[i % len(files)]
        page = wl.load_page(path)
        try:
            timing, _ = engine.run_page(page)
        except Exception as exc:  # noqa: BLE001  报错要记下来再接着跑，不能让一页的错把整轮带走
            errors.append({"page_index": i, "type": type(exc).__name__, "message": str(exc)[:300]})
            totals.append(float("nan"))
            if rebuilds >= 3:
                break
            rebuilds += 1
            engine = Engine(cfg)
            continue
        totals.append(timing.total_ms)
        if i % args.sample_every == 0 or i == args.pages - 1:
            mem = sysinfo.process_memory()
            gpu = sampler.snapshot()["last_mb_by_luid"]
            samples.append(
                {
                    "page_index": i,
                    "elapsed_s": round(time.perf_counter() - started, 1),
                    "working_set_mb": mem.working_set_mb,
                    "private_mb": mem.private_mb,
                    "gpu_dedicated_mb": round(sum(v.get("dedicated", 0.0) for v in gpu.values()), 1),
                    "gpu_shared_mb": round(sum(v.get("shared", 0.0) for v in gpu.values()), 1),
                }
            )
        if args.checkpoint_every and (i + 1) % args.checkpoint_every == 0 and i + 1 < args.pages:
            _write_json(partial, {**args.header, **result(False), "ok": False, "partial": True})
    out = result(True)
    partial.unlink(missing_ok=True)
    return out


def _growth(samples: Sequence[dict[str, Any]]) -> dict[str, Any]:
    """内存涨没涨：去掉头 10% 的页（预热）之后，对页序号做一次直线拟合，给出每 100 页涨多少 MB。"""
    result: dict[str, Any] = {}
    if len(samples) < 8:
        return result
    tail = samples[len(samples) // 10 :]
    x = np.asarray([s["page_index"] for s in tail], dtype=np.float64)
    for key in ("working_set_mb", "private_mb", "gpu_dedicated_mb", "gpu_shared_mb"):
        y = np.asarray([s[key] for s in tail], dtype=np.float64)
        slope = float(np.polyfit(x, y, 1)[0]) if np.ptp(x) > 0 else 0.0
        result[key] = {
            "first": float(y[0]),
            "last": float(y[-1]),
            "min": float(y.min()),
            "max": float(y.max()),
            "mb_per_100_pages": round(slope * 100.0, 3),
        }
    return result


# ---------------------------------------------------------------------------
# consistency：同一个输入，CPU 和 DirectML 的输出差多少
# ---------------------------------------------------------------------------


def _diff(a: F32, b: F32) -> dict[str, float]:
    d = np.abs(a.astype(np.float64) - b.astype(np.float64))
    return {"max_abs": float(d.max()), "mean_abs": float(d.mean())}


def run_consistency(cfg: Config, args: argparse.Namespace) -> dict[str, Any]:
    """同一个进程里各建一套 CPU 会话和 DirectML 会话，逐页比输出。"""
    files = _page_files(args.cache, args.page_limit)
    cpu_cfg = Config("cpu", None, cfg.models, cfg.models_dir, cfg.threads)
    cpu = Engine(cpu_cfg)
    dml = Engine(cfg)
    table = wl.load_dict(cfg.models_dir / "ocr" / MODEL_SETS[cfg.models]["dict"], cpu.rec_classes())

    det_max: list[float] = []
    det_mean: list[float] = []
    det_flips = 0
    det_pixels = 0
    det_mask_pixels = 0
    cls_max: list[float] = []
    cls_lines = 0
    cls_decision_diff = 0
    rec_max: list[float] = []
    rec_mean: list[float] = []
    rec_lines = 0
    rec_path_diff = 0
    rec_text_diff = 0
    conf_diff: list[float] = []
    cpu_vs_cli_diff = 0
    dml_vs_cli_diff = 0
    repeat_max: dict[str, float] = {"det": 0.0, "cls": 0.0, "rec": 0.0}
    per_page: list[dict[str, Any]] = []

    for path in files:
        page = wl.load_page(path)
        _, out_cpu = cpu.run_page(page, keep=True)
        _, out_dml = dml.run_page(page, keep=True)
        _, out_dml2 = dml.run_page(page, keep=True)
        assert out_cpu.det is not None and out_dml.det is not None and out_dml2.det is not None

        d = _diff(out_cpu.det, out_dml.det)
        det_max.append(d["max_abs"])
        det_mean.append(d["mean_abs"])
        mask_cpu = out_cpu.det > 0.3
        flips = int((mask_cpu != (out_dml.det > 0.3)).sum())
        det_flips += flips
        det_pixels += int(mask_cpu.size)
        det_mask_pixels += int(mask_cpu.sum())
        repeat_max["det"] = max(repeat_max["det"], _diff(out_dml.det, out_dml2.det)["max_abs"])

        page_cls_diff = 0
        for a, b, b2 in zip(out_cpu.cls, out_dml.cls, out_dml2.cls):
            cls_max.append(_diff(a, b)["max_abs"])
            page_cls_diff += int((wl.cls_flipped(a) != wl.cls_flipped(b)).sum())
            cls_lines += a.shape[0]
            repeat_max["cls"] = max(repeat_max["cls"], _diff(b, b2)["max_abs"])
        cls_decision_diff += page_cls_diff

        page_text_diff = 0
        for a, b, b2, members in zip(out_cpu.rec, out_dml.rec, out_dml2.rec, page.rec_members):
            d = _diff(a, b)
            rec_max.append(d["max_abs"])
            rec_mean.append(d["mean_abs"])
            repeat_max["rec"] = max(repeat_max["rec"], _diff(b, b2)["max_abs"])
            for slot, line in enumerate(members):
                rec_lines += 1
                if not np.array_equal(a[slot].argmax(axis=1), b[slot].argmax(axis=1)):
                    rec_path_diff += 1
                text_cpu, conf_cpu = wl.ctc_decode(a[slot], table)
                text_dml, conf_dml = wl.ctc_decode(b[slot], table)
                conf_diff.append(abs(conf_cpu - conf_dml))
                if text_cpu != text_dml:
                    rec_text_diff += 1
                    page_text_diff += 1
                cpu_vs_cli_diff += int(text_cpu != page.texts[line])
                dml_vs_cli_diff += int(text_dml != page.texts[line])
        per_page.append(
            {
                "name": page.name,
                "lines": page.lines,
                "det_max_abs": det_max[-1],
                "det_mask_flips": flips,
                "cls_decisions_differ": page_cls_diff,
                "texts_differ": page_text_diff,
            }
        )

    return {
        "summary": {
            "pages": len(files),
            "det": {
                "max_abs": max(det_max),
                "mean_abs": float(np.mean(det_mean)),
                "pixels": det_pixels,
                "text_pixels_cpu": det_mask_pixels,
                "binarized_pixels_that_differ": det_flips,
            },
            "cls": {"max_abs": max(cls_max), "lines": cls_lines, "decisions_differ": cls_decision_diff},
            "rec": {
                "max_abs": max(rec_max),
                "mean_abs": float(np.mean(rec_mean)),
                "lines": rec_lines,
                "argmax_paths_differ": rec_path_diff,
                "texts_differ": rec_text_diff,
                "confidence_max_abs_diff": max(conf_diff),
            },
            "texts_vs_cli": {"lines": rec_lines, "cpu_differ": cpu_vs_cli_diff, "dml_differ": dml_vs_cli_diff},
            "dml_run_to_run_max_abs": repeat_max,
        },
        "directml_module": _dml_module(),
        "pages": per_page,
    }


# ---------------------------------------------------------------------------
# nodes：节点落在哪个执行提供程序上
# ---------------------------------------------------------------------------


def run_nodes(cfg: Config, args: argparse.Namespace) -> dict[str, Any]:
    """开着 ONNX Runtime 的性能记录各跑两次，数一数第二次里每个执行提供程序上跑了多少个节点、占多少时间。"""
    page = wl.load_page(_page_files(args.cache, 1)[0])
    inputs = {"det": page.det, "cls": page.cls[0], "rec": page.rec[len(page.rec) // 2]}
    files = MODEL_SETS[cfg.models]
    result: dict[str, Any] = {}
    for stage, x in inputs.items():
        prefix = args.out.with_suffix("").with_name(f"{args.out.stem}-profile-{stage}")
        session = make_session(cfg, cfg.models_dir / "ocr" / files[stage], profile=prefix)
        session.run(None, {"x": x})
        session.run(None, {"x": x})
        profile_path = Path(session.end_profiling())
        events = json.loads(profile_path.read_text(encoding="utf-8"))
        profile_path.unlink()
        runs = [e for e in events if e.get("cat") == "Session" and e.get("name") == "model_run"]
        second_start = runs[-1]["ts"] if len(runs) >= 2 else 0
        expect = "DmlExecutionProvider" if cfg.provider == "dml" else "CPUExecutionProvider"
        by_provider: dict[str, dict[str, float]] = {}
        elsewhere: dict[str, int] = {}
        for e in events:
            if e.get("cat") != "Node" or not str(e.get("name", "")).endswith("_kernel_time"):
                continue
            if e["ts"] < second_start:
                continue
            node_args = e.get("args", {})
            provider = node_args.get("provider", "?")
            slot = by_provider.setdefault(provider, {"nodes": 0, "ms": 0.0})
            slot["nodes"] += 1
            slot["ms"] += e["dur"] / 1000.0
            if provider != expect:
                op = str(node_args.get("op_name", "?"))
                elsewhere[op] = elsewhere.get(op, 0) + 1
        result[stage] = {
            "input_shape": list(x.shape),
            "model_run_ms": round(runs[-1]["dur"] / 1000.0, 2) if runs else None,
            # DirectML 的节点时间只是把活交给显卡的那一下，不是显卡算完的时间；CPU 上的才是算完的时间。
            "by_provider": {k: {"nodes": int(v["nodes"]), "ms": round(v["ms"], 2)} for k, v in by_provider.items()},
            "ops_not_on_requested_provider": elsewhere,
        }
    return {"summary": result}


# ---------------------------------------------------------------------------
# shapes：识别模型换输入宽度的代价
# ---------------------------------------------------------------------------


def run_shapes(cfg: Config, args: argparse.Namespace) -> dict[str, Any]:
    """识别模型的输入宽度是变的。量三件事（都是满批 6 行、各一个真实批次）：

    - 一种宽度头一次跑，比之后连着跑多花多少；
    - 两种宽度轮着跑，比各自连着跑多花多少（看是不是每换一次都要重新准备）；
    - 八种宽度轮着跑，比各自连着跑多花多少（看记住的形状多了以后还管不管用）。
    """
    rec_cfg = Config(
        cfg.provider,
        cfg.device_id,
        cfg.models,
        cfg.models_dir,
        cfg.threads,
        False,
        ("rec",),
        cfg.provider_options,
        cfg.session_config,
    )
    by_width: dict[int, F32] = {}
    for path in _page_files(args.cache, args.page_limit):
        for batch in wl.load_page(path).rec:
            if batch.shape[0] == wl.REC_BATCH:
                by_width.setdefault(int(batch.shape[3]), batch)
    widths = sorted(by_width)
    if len(widths) < 4:
        raise RuntimeError(f"满批的宽度只有 {len(widths)} 种，不够比")
    # 预热用最宽的；A 取最窄的，B 取中间的；八种轮着跑的在其余宽度里均匀挑。
    warm_w, a_w, b_w = widths[-1], widths[0], widths[len(widths) // 2]
    rest = [w for w in widths if w not in (warm_w, a_w, b_w)]
    step = max(len(rest) / 8, 1.0)
    cycle_ws = [rest[int(i * step)] for i in range(min(8, len(rest)))]
    repeats = args.repeats

    engine = Engine(rec_cfg)

    def once(width: int) -> float:
        _, ms, _ = engine.run_one("rec", by_width[width])
        return ms

    for _ in range(3):
        once(warm_w)
    pair: dict[str, dict[str, Any]] = {}
    for name, width in (("a", a_w), ("b", b_w)):
        first = once(width)
        same = [once(width) for _ in range(repeats)]
        pair[name] = {"width": width, "first_ms": round(first, 2), "same_ms": _stats(same)}
    alternate: dict[str, list[float]] = {"a": [], "b": []}
    for _ in range(repeats):
        alternate["a"].append(once(a_w))
        alternate["b"].append(once(b_w))
    for name in ("a", "b"):
        entry = pair[name]
        entry["alternate_ms"] = _stats(alternate[name])
        entry["first_extra_ms"] = round(entry["first_ms"] - entry["same_ms"]["median"], 2)
        entry["alternate_extra_ms"] = round(entry["alternate_ms"]["median"] - entry["same_ms"]["median"], 2)

    first_seen = {w: once(w) for w in cycle_ws}
    cycled: dict[int, list[float]] = {w: [] for w in cycle_ws}
    for _ in range(repeats):
        for w in cycle_ws:
            cycled[w].append(once(w))
    same = {w: [once(w) for _ in range(repeats)] for w in cycle_ws}
    cycle = []
    for w in cycle_ws:
        same_median = statistics.median(same[w])
        cycle.append(
            {
                "width": w,
                "first_ms": round(first_seen[w], 2),
                "same_ms": round(same_median, 2),
                "cycled_ms": round(statistics.median(cycled[w]), 2),
                "first_extra_ms": round(first_seen[w] - same_median, 2),
                "cycled_extra_ms": round(statistics.median(cycled[w]) - same_median, 2),
            }
        )
    return {
        "summary": {
            "repeats": repeats,
            "batch": wl.REC_BATCH,
            "pair": pair,
            "cycle": cycle,
            "cycle_first_extra_ms": _stats([c["first_extra_ms"] for c in cycle]),
            "cycle_cycled_extra_ms": _stats([c["cycled_extra_ms"] for c in cycle]),
            "cycle_same_ms": _stats([c["same_ms"] for c in cycle]),
        },
        "directml_module": _dml_module(),
    }


# ---------------------------------------------------------------------------


def _kv(items: Sequence[str]) -> tuple[tuple[str, str], ...]:
    pairs = []
    for item in items:
        key, _, value = item.partition("=")
        pairs.append((key, value))
    return tuple(pairs)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("mode", choices=["timing", "soak", "consistency", "nodes", "shapes"])
    ap.add_argument("--provider", choices=["cpu", "dml"], default="cpu")
    ap.add_argument("--device-id", type=int, default=None, help="DirectML 的适配器序号（EnumAdapters1 的顺序）")
    ap.add_argument("--models", choices=sorted(MODEL_SETS), default="mobile")
    ap.add_argument("--models-dir", type=Path, default=Path(os.environ.get("MAGPDF_MODELS_DIR", "models")))
    ap.add_argument("--cache", type=Path, required=True, help="workload.py 生成的输入缓存目录")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--label", default="")
    ap.add_argument("--threads", type=int, default=0, help="算子内线程数，0 交给 ONNX Runtime")
    ap.add_argument("--passes", type=int, default=3, help="timing：整套页面过几遍；第 1 遍之后的算稳定值")
    ap.add_argument("--page-limit", type=int, default=None, help="只用缓存里的前几页（均匀取样）")
    ap.add_argument("--pages", type=int, default=1000, help="soak：一共处理多少页")
    ap.add_argument("--sample-every", type=int, default=10, help="soak：每多少页记一次内存")
    ap.add_argument("--checkpoint-every", type=int, default=50, help="soak：每多少页把到目前为止的结果另存一份（0 不存）")
    ap.add_argument("--repeats", type=int, default=10, help="shapes：每种跑法重复几次")
    ap.add_argument("--stages", default="det,cls,rec")
    ap.add_argument("--width-policy", choices=sorted(WIDTH_POLICIES), default="product")
    ap.add_argument("--static-shapes", action="store_true", help="每种输入形状各建一个把维度定死的会话")
    ap.add_argument("--provider-option", action="append", default=[], metavar="K=V")
    ap.add_argument("--session-config", action="append", default=[], metavar="K=V")
    ap.add_argument(
        "--priority",
        choices=["normal", "above-normal"],
        default="normal",
        help="进程优先级。机器上有别的任务抢 CPU 时，调到“高于正常”量出来的更接近机器空闲时的情况",
    )
    args = ap.parse_args()
    if args.priority == "above-normal":
        sysinfo.set_above_normal_priority()

    if args.provider == "dml" and args.device_id is None:
        args.device_id = 0
    cfg = Config(
        provider=args.provider,
        device_id=args.device_id,
        models=args.models,
        models_dir=args.models_dir,
        threads=args.threads,
        static_shapes=args.static_shapes,
        stages=tuple(s for s in args.stages.split(",") if s),
        provider_options=_kv(args.provider_option),
        session_config=_kv(args.session_config),
    )
    result: dict[str, Any] = {
        "kind": args.mode,
        "label": args.label,
        "started": time.strftime("%Y-%m-%d %H:%M:%S"),
        "config": _config_json(cfg),
        "priority": args.priority,
        "cache": str(args.cache),
    }
    args.mem_before_import = sysinfo.process_memory()
    t = time.perf_counter()
    ort = _import_ort()
    args.import_ms = _ms(t)
    result["onnxruntime"] = {"version": ort.__version__, "build_info": ort.get_build_info()}
    args.header = dict(result)  # soak 中途存结果时带上这些
    try:
        runner = {
            "timing": run_timing,
            "soak": run_soak,
            "consistency": run_consistency,
            "nodes": run_nodes,
            "shapes": run_shapes,
        }[args.mode]
        result.update(runner(cfg, args))
        result["ok"] = True
    except Exception as exc:  # noqa: BLE001  失败也要留下一份结果文件，汇总时才知道哪一项没量成
        result["ok"] = False
        result["error"] = {"type": type(exc).__name__, "message": str(exc)[:500]}
    result["wall_s"] = round(time.perf_counter() - _T_START, 1)
    _write_json(args.out, result)
    if not result["ok"]:
        print(f"{args.mode} 失败：{result['error']['type']}: {result['error']['message']}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
