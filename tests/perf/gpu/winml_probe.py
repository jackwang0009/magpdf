"""Windows ML 在这台机器上给出哪些执行提供程序（V10）。

Windows ML 是一份微软分发的 ONNX Runtime，自带 CPU 和 DirectML 两个执行提供程序；
各家硬件专用的执行提供程序放在一个目录里，按这台机器的硬件筛选，用到时从微软商店下载。
这个脚本只看不下载：

- 目录对这台机器列出了哪些执行提供程序，各是什么状态（没装、装了没注册、已就绪）；
- Windows ML 自带的 ONNX Runtime 是哪个版本，已有哪些执行提供程序，各看到哪些设备。

要另一个虚拟环境（requirements-winml.txt）和系统里已经装着的 Windows App Runtime 2.x。
系统里没有对得上的运行时会直接报错，不会弹出安装窗口。

    python winml_probe.py [--out winml.json]
    python winml_probe.py --ensure-ready    # 会从微软商店下载执行提供程序并装到系统里，默认不做
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

import sysinfo

# 确认用的是哪一份运行时：这几个 DLL 装载之后记下路径和版本。
_MODULES = (
    "Microsoft.Windows.AI.MachineLearning.dll",
    "Microsoft.WindowsAppRuntime.dll",
    "onnxruntime.dll",
    "DirectML.dll",
)


def _enum_name(value: Any) -> str:
    return str(getattr(value, "name", value))


def _provider(p: Any, ensure_ready: bool) -> dict[str, Any]:
    item: dict[str, Any] = {
        "name": p.name,
        "ready_state": _enum_name(p.ready_state),
        "certification": _enum_name(p.certification),
    }
    if ensure_ready:
        result = p.ensure_ready_async().get()
        item["ensure_ready"] = {"status": _enum_name(result.status), "diagnostic": result.diagnostic_text}
        item["ready_state"] = _enum_name(p.ready_state)
    # 没装的执行提供程序没有路径和包，读这两个属性会报错。
    try:
        item["library_path"] = p.library_path or None
    except OSError:
        item["library_path"] = None
    try:
        package = p.package_id
        version = package.version
        item["package"] = f"{package.name} {version.major}.{version.minor}.{version.build}.{version.revision}"
    except (OSError, AttributeError):
        item["package"] = None
    return item


def _ep_devices(ort: Any) -> list[dict[str, Any]]:
    """ONNX Runtime 自己看到的“执行提供程序 × 设备”（1.22 起有这个接口）。"""
    if not hasattr(ort, "get_ep_devices"):
        return []
    devices = []
    for d in ort.get_ep_devices():
        hw = d.device
        devices.append(
            {
                "ep": d.ep_name,
                "ep_vendor": d.ep_vendor,
                "type": _enum_name(hw.type),
                "vendor_id": f"0x{hw.vendor_id:04X}",
                "device_id": f"0x{hw.device_id:04X}",
                "metadata": {k: str(v).strip() for k, v in dict(hw.metadata).items()},
            }
        )
    return devices


def probe(ensure_ready: bool = False) -> dict[str, Any]:
    from winui3.microsoft.windows.applicationmodel.dynamicdependency import bootstrap

    result: dict[str, Any] = {}
    # 选项给 0：不要 ON_NO_MATCH_SHOW_UI，系统里没有运行时就报错，不弹窗。
    with bootstrap.initialize(options=bootstrap.InitializeOptions(0)):
        import winui3.microsoft.windows.ai.machinelearning as winml

        catalog = winml.ExecutionProviderCatalog.get_default()
        result["catalog_providers"] = [_provider(p, ensure_ready) for p in catalog.find_all_providers()]

        import onnxruntime as ort

        result["onnxruntime"] = {
            "version": ort.__version__,
            "build_info": ort.get_build_info(),
            "available_providers": ort.get_available_providers(),
            "ep_devices": _ep_devices(ort),
        }
        modules = {}
        for name in _MODULES:
            path = sysinfo.loaded_module_path(name)
            if path is not None:
                modules[name] = {"path": str(path), "version": sysinfo.file_version(path), "size": path.stat().st_size}
        result["loaded_modules"] = modules
    result["video_controllers"] = sysinfo.video_controllers()
    result["os_build"] = sys.getwindowsversion().build
    return result


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=None, help="结果写到这个文件；不给就打印出来")
    ap.add_argument("--ensure-ready", action="store_true", help="把目录里的执行提供程序都下载、装好（改动系统，默认不做）")
    args = ap.parse_args()
    try:
        result: dict[str, Any] = {"ok": True} | probe(args.ensure_ready)
    except (ImportError, OSError) as exc:
        result = {"ok": False, "error": {"type": type(exc).__name__, "message": str(exc)[:500]}}
    text = json.dumps(result, ensure_ascii=False, indent=1)
    if args.out is not None:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text, encoding="utf-8")
    else:
        print(text)
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
