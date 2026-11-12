"""本机信息：DXGI 适配器、显卡驱动、DirectML 版本、CPU 占用、进程内存和显存（V10）。

只用标准库（ctypes 调 dxgi.dll、pdh.dll、psapi.dll、version.dll），不引入别的包。

- DirectML 执行提供程序的 `device_id` 就是 `IDXGIFactory1::EnumAdapters1` 的序号，
  所以这里按同样的顺序列适配器，测量脚本按名字找到序号。
- 显存用性能计数器 `GPU Process Memory`（任务管理器“专用 GPU 内存”“共享 GPU 内存”那两列的来源），
  集成显卡没有独立显存，主要看共享的那一列。

单独运行：python sysinfo.py  → 打印一份 JSON。
"""
from __future__ import annotations

import ctypes
import json
import os
import platform
import subprocess
import sys
import threading
import time
from ctypes import wintypes
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# DXGI 适配器
# ---------------------------------------------------------------------------


class _GUID(ctypes.Structure):
    _fields_ = [
        ("Data1", wintypes.DWORD),
        ("Data2", wintypes.WORD),
        ("Data3", wintypes.WORD),
        ("Data4", ctypes.c_ubyte * 8),
    ]


class _LUID(ctypes.Structure):
    _fields_ = [("LowPart", wintypes.DWORD), ("HighPart", wintypes.LONG)]


class _DXGI_ADAPTER_DESC1(ctypes.Structure):
    _fields_ = [
        ("Description", wintypes.WCHAR * 128),
        ("VendorId", wintypes.UINT),
        ("DeviceId", wintypes.UINT),
        ("SubSysId", wintypes.UINT),
        ("Revision", wintypes.UINT),
        ("DedicatedVideoMemory", ctypes.c_size_t),
        ("DedicatedSystemMemory", ctypes.c_size_t),
        ("SharedSystemMemory", ctypes.c_size_t),
        ("AdapterLuid", _LUID),
        ("Flags", wintypes.UINT),
    ]


_IID_IDXGIFactory1 = _GUID(
    0x770AAE78, 0xF26F, 0x4DBA, (ctypes.c_ubyte * 8)(0xA8, 0x29, 0x25, 0x3C, 0x83, 0xD1, 0xB3, 0x87)
)
_DXGI_ERROR_NOT_FOUND = 0x887A0002
_DXGI_ADAPTER_FLAG_SOFTWARE = 2


@dataclass(frozen=True)
class Adapter:
    """一块 DXGI 适配器。`index` 就是 DirectML 的 `device_id`。"""

    index: int
    name: str
    vendor_id: int
    device_id: int
    dedicated_video_mb: float
    shared_system_mb: float
    luid: str
    software: bool

    @property
    def vendor(self) -> str:
        return {0x10DE: "NVIDIA", 0x1002: "AMD", 0x8086: "Intel", 0x1414: "Microsoft"}.get(
            self.vendor_id, f"0x{self.vendor_id:04X}"
        )


def _vtbl_call(obj: ctypes.c_void_p, slot: int, restype: Any, argtypes: list[Any], *args: Any) -> Any:
    """调用 COM 对象虚表里第 slot 个方法。"""
    vtbl = ctypes.cast(obj, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
    proto = ctypes.WINFUNCTYPE(restype, ctypes.c_void_p, *argtypes)
    return proto(vtbl[slot])(obj, *args)


def enum_adapters() -> list[Adapter]:
    """按 `EnumAdapters1` 的顺序列出适配器（含软件适配器 Microsoft Basic Render Driver）。"""
    dxgi = ctypes.WinDLL("dxgi")
    create = dxgi.CreateDXGIFactory1
    create.restype = ctypes.HRESULT
    create.argtypes = [ctypes.POINTER(_GUID), ctypes.POINTER(ctypes.c_void_p)]
    factory = ctypes.c_void_p()
    create(ctypes.byref(_IID_IDXGIFactory1), ctypes.byref(factory))
    adapters: list[Adapter] = []
    try:
        index = 0
        while True:
            adapter = ctypes.c_void_p()
            # IDXGIFactory1::EnumAdapters1 在虚表第 12 项。
            hr = _vtbl_call(
                factory, 12, ctypes.c_long, [wintypes.UINT, ctypes.POINTER(ctypes.c_void_p)], index, ctypes.byref(adapter)
            )
            if (hr & 0xFFFFFFFF) == _DXGI_ERROR_NOT_FOUND:
                break
            if hr < 0:
                raise OSError(f"EnumAdapters1({index}) 失败：0x{hr & 0xFFFFFFFF:08X}")
            desc = _DXGI_ADAPTER_DESC1()
            # IDXGIAdapter1::GetDesc1 在虚表第 10 项。
            hr = _vtbl_call(adapter, 10, ctypes.c_long, [ctypes.POINTER(_DXGI_ADAPTER_DESC1)], ctypes.byref(desc))
            _vtbl_call(adapter, 2, wintypes.ULONG, [])  # Release
            if hr < 0:
                raise OSError(f"GetDesc1({index}) 失败：0x{hr & 0xFFFFFFFF:08X}")
            adapters.append(
                Adapter(
                    index=index,
                    name=desc.Description,
                    vendor_id=desc.VendorId,
                    device_id=desc.DeviceId,
                    dedicated_video_mb=round(desc.DedicatedVideoMemory / 2**20, 1),
                    shared_system_mb=round(desc.SharedSystemMemory / 2**20, 1),
                    luid=f"0x{desc.AdapterLuid.HighPart & 0xFFFFFFFF:08X}_0x{desc.AdapterLuid.LowPart:08X}",
                    software=bool(desc.Flags & _DXGI_ADAPTER_FLAG_SOFTWARE),
                )
            )
            index += 1
    finally:
        _vtbl_call(factory, 2, wintypes.ULONG, [])  # Release
    return adapters


def hardware_adapters() -> list[Adapter]:
    """能给 DirectML 用的适配器：去掉软件适配器。"""
    return [a for a in enum_adapters() if not a.software]


# ---------------------------------------------------------------------------
# 驱动、设备状态、文件版本
# ---------------------------------------------------------------------------


def _powershell_json(command: str, timeout: float = 60.0) -> Any:
    """跑一条 PowerShell 命令，把输出当 JSON 读回来；失败返回 None。"""
    exe = "pwsh" if _which("pwsh") else "powershell"
    try:
        out = subprocess.run(
            [exe, "-NoProfile", "-NonInteractive", "-Command", command],
            capture_output=True,
            timeout=timeout,
            check=False,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    text = out.stdout.decode("utf-8", errors="replace").strip()
    if not text:
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None


def _which(name: str) -> bool:
    from shutil import which

    return which(name) is not None


def video_controllers() -> list[dict[str, Any]]:
    """显卡设备（含出了问题的）：名称、驱动版本和日期、设备管理器里的状态码。

    `ConfigManagerErrorCode` 不是 0 的设备没有启动，DXGI 里看不到它，DirectML 也就用不了。
    """
    data = _powershell_json(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; "
        "Get-CimInstance Win32_VideoController | ForEach-Object { [pscustomobject]@{ "
        "name=$_.Name; driver_version=$_.DriverVersion; "
        "driver_date= if ($_.DriverDate) { $_.DriverDate.ToString('yyyy-MM-dd') } else { $null }; "
        "status=$_.Status; config_manager_error_code=$_.ConfigManagerErrorCode; pnp_device_id=$_.PNPDeviceID } } "
        "| ConvertTo-Json -Compress -AsArray"
        if _which("pwsh")
        else "[Console]::OutputEncoding=[Text.Encoding]::UTF8; "
        "@(Get-CimInstance Win32_VideoController | ForEach-Object { [pscustomobject]@{ "
        "name=$_.Name; driver_version=$_.DriverVersion; "
        "driver_date= if ($_.DriverDate) { $_.DriverDate.ToString('yyyy-MM-dd') } else { $null }; "
        "status=$_.Status; config_manager_error_code=$_.ConfigManagerErrorCode; pnp_device_id=$_.PNPDeviceID } }) "
        "| ConvertTo-Json -Compress"
    )
    if data is None:
        return []
    return data if isinstance(data, list) else [data]


def cpu_and_memory() -> dict[str, Any]:
    """CPU 型号、物理核数和逻辑核数、内存总量（GB）。查不到的项是 None。"""
    data = _powershell_json(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; "
        "$p = Get-CimInstance Win32_Processor | Select-Object -First 1; "
        "$m = Get-CimInstance Win32_ComputerSystem; "
        "[pscustomobject]@{ name=$p.Name.Trim(); cores=$p.NumberOfCores; logical=$p.NumberOfLogicalProcessors; "
        "memory_gb=[math]::Round($m.TotalPhysicalMemory / 1GB, 1) } | ConvertTo-Json -Compress"
    )
    if not isinstance(data, dict):
        return {"name": None, "cores": None, "logical": os.cpu_count(), "memory_gb": None}
    return data


def file_version(path: Path) -> str | None:
    """PE 文件的产品版本字符串（`DirectML.dll`、`onnxruntime.dll`）。"""
    version = ctypes.WinDLL("version")
    version.GetFileVersionInfoSizeW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.DWORD)]
    version.GetFileVersionInfoSizeW.restype = wintypes.DWORD
    version.GetFileVersionInfoW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]
    version.GetFileVersionInfoW.restype = wintypes.BOOL
    version.VerQueryValueW.argtypes = [
        ctypes.c_void_p,
        wintypes.LPCWSTR,
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(wintypes.UINT),
    ]
    version.VerQueryValueW.restype = wintypes.BOOL
    handle = wintypes.DWORD()
    size = version.GetFileVersionInfoSizeW(str(path), ctypes.byref(handle))
    if size == 0:
        return None
    buf = ctypes.create_string_buffer(size)
    if not version.GetFileVersionInfoW(str(path), 0, size, buf):
        return None
    ptr = ctypes.c_void_p()
    length = wintypes.UINT()
    if not version.VerQueryValueW(buf, "\\VarFileInfo\\Translation", ctypes.byref(ptr), ctypes.byref(length)):
        return None
    if length.value < 4 or not ptr.value:
        return None
    lang, codepage = ctypes.cast(ptr, ctypes.POINTER(wintypes.WORD * 2)).contents
    for key in ("ProductVersion", "FileVersion"):
        sub = f"\\StringFileInfo\\{lang:04x}{codepage:04x}\\{key}"
        if version.VerQueryValueW(buf, sub, ctypes.byref(ptr), ctypes.byref(length)) and ptr.value:
            return ctypes.wstring_at(ptr.value, max(length.value - 1, 0))
    return None


def loaded_module_path(name: str) -> Path | None:
    """当前进程里已经装载的某个 DLL 的完整路径（确认用的是哪一份 `DirectML.dll`）。"""
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetModuleHandleW.argtypes = [wintypes.LPCWSTR]
    kernel32.GetModuleHandleW.restype = wintypes.HMODULE
    kernel32.GetModuleFileNameW.argtypes = [wintypes.HMODULE, wintypes.LPWSTR, wintypes.DWORD]
    kernel32.GetModuleFileNameW.restype = wintypes.DWORD
    handle = kernel32.GetModuleHandleW(name)
    if not handle:
        return None
    buf = ctypes.create_unicode_buffer(1024)
    if kernel32.GetModuleFileNameW(handle, buf, 1024) == 0:
        return None
    return Path(buf.value)


# ---------------------------------------------------------------------------
# 进程内存
# ---------------------------------------------------------------------------


class _PROCESS_MEMORY_COUNTERS_EX(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("PageFaultCount", wintypes.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


@dataclass(frozen=True)
class ProcessMemory:
    working_set_mb: float
    peak_working_set_mb: float
    private_mb: float


def process_memory() -> ProcessMemory:
    """当前进程的工作集、峰值工作集、私有提交（MB）。"""
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    psapi.GetProcessMemoryInfo.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(_PROCESS_MEMORY_COUNTERS_EX),
        wintypes.DWORD,
    ]
    psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
    counters = _PROCESS_MEMORY_COUNTERS_EX()
    counters.cb = ctypes.sizeof(counters)
    if not psapi.GetProcessMemoryInfo(kernel32.GetCurrentProcess(), ctypes.byref(counters), counters.cb):
        raise ctypes.WinError(ctypes.get_last_error())
    mb = 2**20
    return ProcessMemory(
        working_set_mb=round(counters.WorkingSetSize / mb, 1),
        peak_working_set_mb=round(counters.PeakWorkingSetSize / mb, 1),
        private_mb=round(counters.PrivateUsage / mb, 1),
    )


def set_above_normal_priority() -> None:
    """把当前进程的优先级调到“高于正常”。

    开发机上同时有别的任务在抢 CPU 时，正常优先级量出来的时间会被拉长；调高一档之后，
    推理线程要用 CPU 时能排到别的任务前面，量出来的更接近机器空闲时的情况（不完全等于）。
    """
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    kernel32.SetPriorityClass.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel32.SetPriorityClass.restype = wintypes.BOOL
    above_normal_priority_class = 0x00008000
    if not kernel32.SetPriorityClass(kernel32.GetCurrentProcess(), above_normal_priority_class):
        raise ctypes.WinError(ctypes.get_last_error())


# ---------------------------------------------------------------------------
# 一段时间里 CPU 被谁用掉了：本进程用了多少，别的进程用了多少
# ---------------------------------------------------------------------------


class _FILETIME(ctypes.Structure):
    _fields_ = [("dwLowDateTime", wintypes.DWORD), ("dwHighDateTime", wintypes.DWORD)]

    def seconds(self) -> float:
        return ((self.dwHighDateTime << 32) | self.dwLowDateTime) / 1e7


@dataclass(frozen=True)
class CpuTimes:
    """开机以来的累计值（秒，各逻辑处理器加在一起）。两次快照相减才有意义。"""

    total_s: float  # 忙 + 闲
    busy_s: float
    own_s: float  # 本进程用掉的


def cpu_times() -> CpuTimes:
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    kernel32.GetSystemTimes.argtypes = [ctypes.POINTER(_FILETIME)] * 3
    kernel32.GetSystemTimes.restype = wintypes.BOOL
    kernel32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(_FILETIME)] * 4
    kernel32.GetProcessTimes.restype = wintypes.BOOL
    idle, kernel, user = _FILETIME(), _FILETIME(), _FILETIME()
    if not kernel32.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kernel), ctypes.byref(user)):
        raise ctypes.WinError(ctypes.get_last_error())
    created, exited, own_kernel, own_user = _FILETIME(), _FILETIME(), _FILETIME(), _FILETIME()
    if not kernel32.GetProcessTimes(
        kernel32.GetCurrentProcess(),
        ctypes.byref(created),
        ctypes.byref(exited),
        ctypes.byref(own_kernel),
        ctypes.byref(own_user),
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    total = kernel.seconds() + user.seconds()  # 系统的内核时间里已经包含空闲时间
    return CpuTimes(total_s=total, busy_s=total - idle.seconds(), own_s=own_kernel.seconds() + own_user.seconds())


def cpu_share(before: CpuTimes, after: CpuTimes) -> dict[str, float | None]:
    """两次快照之间，整机 CPU 时间里本进程占了百分之几、别的进程占了百分之几（100 % = 所有逻辑处理器都满）。"""
    total = after.total_s - before.total_s
    if total <= 0:
        return {"own_percent": None, "others_percent": None}
    own = after.own_s - before.own_s
    busy = after.busy_s - before.busy_s
    return {
        "own_percent": round(100.0 * own / total, 1),
        "others_percent": round(100.0 * max(busy - own, 0.0) / total, 1),
    }


# ---------------------------------------------------------------------------
# 性能计数器（PDH）：CPU 占用、显存、显卡占用
# ---------------------------------------------------------------------------

_PDH_FMT_DOUBLE = 0x00000200
_PDH_FMT_NOCAP100 = 0x00008000
_PDH_MORE_DATA = 0x800007D2
_PDH_CSTATUS_VALID_DATA = 0x00000000
_PDH_CSTATUS_NEW_DATA = 0x00000001


class _PDH_FMT_COUNTERVALUE(ctypes.Structure):
    _fields_ = [("CStatus", wintypes.DWORD), ("doubleValue", ctypes.c_double)]


class _PDH_FMT_COUNTERVALUE_ITEM_W(ctypes.Structure):
    _fields_ = [("szName", wintypes.LPWSTR), ("FmtValue", _PDH_FMT_COUNTERVALUE)]


class PdhQuery:
    """一个 PDH 查询，里面可以放几条带通配符的计数器路径。

    `collect()` 采一次样，`values(key)` 取这条路径下各个实例当前的值。
    带 `*` 的路径每次采样都会重新展开，所以进程用上显卡之后才出现的实例也读得到。
    """

    def __init__(self) -> None:
        self._pdh = ctypes.WinDLL("pdh")
        self._pdh.PdhOpenQueryW.argtypes = [wintypes.LPCWSTR, ctypes.c_size_t, ctypes.POINTER(wintypes.HANDLE)]
        self._pdh.PdhOpenQueryW.restype = wintypes.DWORD
        self._pdh.PdhAddEnglishCounterW.argtypes = [
            wintypes.HANDLE,
            wintypes.LPCWSTR,
            ctypes.c_size_t,
            ctypes.POINTER(wintypes.HANDLE),
        ]
        self._pdh.PdhAddEnglishCounterW.restype = wintypes.DWORD
        self._pdh.PdhCollectQueryData.argtypes = [wintypes.HANDLE]
        self._pdh.PdhCollectQueryData.restype = wintypes.DWORD
        self._pdh.PdhGetFormattedCounterArrayW.argtypes = [
            wintypes.HANDLE,
            wintypes.DWORD,
            ctypes.POINTER(wintypes.DWORD),
            ctypes.POINTER(wintypes.DWORD),
            ctypes.c_void_p,
        ]
        self._pdh.PdhGetFormattedCounterArrayW.restype = wintypes.DWORD
        self._pdh.PdhCloseQuery.argtypes = [wintypes.HANDLE]
        self._pdh.PdhCloseQuery.restype = wintypes.DWORD
        self._query = wintypes.HANDLE()
        status = self._pdh.PdhOpenQueryW(None, 0, ctypes.byref(self._query))
        if status != 0:
            raise OSError(f"PdhOpenQuery 失败：0x{status:08X}")
        self._counters: dict[str, wintypes.HANDLE] = {}

    def add(self, key: str, path: str) -> bool:
        """加一条计数器路径（英文名）。这台机器上没有这个计数器时返回 False。"""
        counter = wintypes.HANDLE()
        status = self._pdh.PdhAddEnglishCounterW(self._query, path, 0, ctypes.byref(counter))
        if status != 0:
            return False
        self._counters[key] = counter
        return True

    def collect(self) -> None:
        self._pdh.PdhCollectQueryData(self._query)

    def values(self, key: str) -> dict[str, float]:
        """这条路径下各实例的值；还没有实例、或者数据无效时返回空表。"""
        counter = self._counters.get(key)
        if counter is None:
            return {}
        size = wintypes.DWORD(0)
        count = wintypes.DWORD(0)
        fmt = _PDH_FMT_DOUBLE | _PDH_FMT_NOCAP100
        status = self._pdh.PdhGetFormattedCounterArrayW(counter, fmt, ctypes.byref(size), ctypes.byref(count), None)
        if status != _PDH_MORE_DATA or size.value == 0:
            return {}
        buf = ctypes.create_string_buffer(size.value)
        status = self._pdh.PdhGetFormattedCounterArrayW(counter, fmt, ctypes.byref(size), ctypes.byref(count), buf)
        if status != 0:
            return {}
        items = ctypes.cast(buf, ctypes.POINTER(_PDH_FMT_COUNTERVALUE_ITEM_W))
        result: dict[str, float] = {}
        for i in range(count.value):
            item = items[i]
            if item.FmtValue.CStatus in (_PDH_CSTATUS_VALID_DATA, _PDH_CSTATUS_NEW_DATA):
                name = item.szName or ""
                # 同名实例（少见）把值加起来。
                result[name] = result.get(name, 0.0) + item.FmtValue.doubleValue
        return result

    def close(self) -> None:
        if self._query:
            self._pdh.PdhCloseQuery(self._query)
            self._query = wintypes.HANDLE()

    def __enter__(self) -> "PdhQuery":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


def cpu_percent(interval: float = 1.0) -> float | None:
    """整机 CPU 占用（`\\Processor(_Total)\\% Processor Time`），采样间隔 `interval` 秒。"""
    with PdhQuery() as query:
        if not query.add("cpu", r"\Processor(_Total)\% Processor Time"):
            return None
        query.collect()
        time.sleep(interval)
        query.collect()
        values = query.values("cpu")
        if not values:
            return None
        return round(next(iter(values.values())), 1)


def gpu_load_snapshot(interval: float = 1.0) -> dict[str, Any]:
    """量之前看一眼：整机 CPU 占用，各显卡上别的进程的占用（按适配器 LUID 汇总各引擎利用率的最大值）。

    显卡的数是 `GPU Engine(*)\\Utilization Percentage`：一块显卡有多个引擎（3D、Compute、Copy、视频编解码…），
    这里对每块显卡取“各引擎上所有进程利用率之和”的最大值，和任务管理器显卡那一栏的算法一致。
    """
    own_prefix = f"pid_{os.getpid()}_"
    with PdhQuery() as query:
        has_cpu = query.add("cpu", r"\Processor(_Total)\% Processor Time")
        has_gpu = query.add("engine", r"\GPU Engine(*)\Utilization Percentage")
        query.collect()
        time.sleep(interval)
        query.collect()
        cpu = None
        if has_cpu:
            values = query.values("cpu")
            cpu = round(next(iter(values.values())), 1) if values else None
        per_adapter: dict[str, dict[str, float]] = {}
        if has_gpu:
            for name, value in query.values("engine").items():
                # 实例名形如 pid_1234_luid_0x00000000_0x0000C0DE_phys_0_eng_0_engtype_3D
                if name.startswith(own_prefix) or "_luid_" not in name:
                    continue
                luid = name.split("_luid_", 1)[1].split("_phys_", 1)[0]
                engine = name.split("_eng_", 1)[1] if "_eng_" in name else "?"
                per_adapter.setdefault(luid.upper().replace("X", "x"), {}).setdefault(engine, 0.0)
                per_adapter[luid.upper().replace("X", "x")][engine] += value
    gpu = {luid: round(max(engines.values()), 1) for luid, engines in per_adapter.items()}
    return {"cpu_percent": cpu, "gpu_busiest_engine_percent_by_luid": gpu}


class GpuMemorySampler:
    """后台线程按固定间隔读本进程的显存占用（专用、共享）和显卡占用，记下峰值和最后一次的值。

    计数器是 `GPU Process Memory(pid_<本进程>*)\\Dedicated Usage`、`\\Shared Usage`（字节）
    和 `GPU Engine(pid_<本进程>*)\\Utilization Percentage`。
    一个进程在每块用到的显卡上各有一个实例，这里按适配器 LUID 分开记。
    显卡占用取本进程在这块显卡各引擎上的最大值，用来确认推理确实是显卡在算。
    """

    def __init__(self, interval: float = 0.25) -> None:
        self._interval = interval
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._lock = threading.Lock()
        self._peak: dict[str, dict[str, float]] = {}
        self._last: dict[str, dict[str, float]] = {}
        self._util_sum: dict[str, float] = {}
        self._util_max: dict[str, float] = {}
        self._others_sum: dict[str, float] = {}
        self._samples = 0
        self.available = False

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name="gpu-mem-sampler", daemon=True)
        self._thread.start()

    @staticmethod
    def _luid(instance: str) -> str:
        return instance.split("_luid_", 1)[1].split("_phys_", 1)[0] if "_luid_" in instance else instance

    def _run(self) -> None:
        pid = os.getpid()
        try:
            query = PdhQuery()
        except OSError:
            return
        with query:
            ok_d = query.add("dedicated", rf"\GPU Process Memory(pid_{pid}_*)\Dedicated Usage")
            ok_s = query.add("shared", rf"\GPU Process Memory(pid_{pid}_*)\Shared Usage")
            query.add("engine", rf"\GPU Engine(pid_{pid}_*)\Utilization Percentage")
            query.add("engine_all", r"\GPU Engine(*)\Utilization Percentage")
            self.available = ok_d and ok_s
            if not self.available:
                return
            own_prefix = f"pid_{pid}_"
            while not self._stop.is_set():
                query.collect()
                snapshot: dict[str, dict[str, float]] = {}
                for kind in ("dedicated", "shared"):
                    for name, value in query.values(kind).items():
                        snapshot.setdefault(self._luid(name), {})[kind] = value / 2**20
                busiest: dict[str, float] = {}
                for name, value in query.values("engine").items():
                    luid = self._luid(name)
                    busiest[luid] = max(busiest.get(luid, 0.0), value)
                # 同一块显卡上别的进程：每个引擎上各进程的占用加起来，取最忙的那个引擎。
                others: dict[str, dict[str, float]] = {}
                for name, value in query.values("engine_all").items():
                    if name.startswith(own_prefix) or "_luid_" not in name:
                        continue
                    engine = name.split("_eng_", 1)[1] if "_eng_" in name else "?"
                    slot = others.setdefault(self._luid(name), {})
                    slot[engine] = slot.get(engine, 0.0) + value
                with self._lock:
                    self._samples += 1
                    self._last = snapshot
                    for luid, kinds in snapshot.items():
                        peak = self._peak.setdefault(luid, {})
                        for kind, value in kinds.items():
                            peak[kind] = max(peak.get(kind, 0.0), value)
                    for luid, value in busiest.items():
                        self._util_sum[luid] = self._util_sum.get(luid, 0.0) + value
                        self._util_max[luid] = max(self._util_max.get(luid, 0.0), value)
                    for luid, engines in others.items():
                        self._others_sum[luid] = self._others_sum.get(luid, 0.0) + max(engines.values())
                self._stop.wait(self._interval)

    def snapshot(self) -> dict[str, Any]:
        """到现在为止的峰值和最近一次的值（MB）、本进程的显卡占用（%），按适配器 LUID。"""
        with self._lock:
            n = max(self._samples, 1)
            return {
                "available": self.available,
                "samples": self._samples,
                "peak_mb_by_luid": {k: {kk: round(vv, 1) for kk, vv in v.items()} for k, v in self._peak.items()},
                "last_mb_by_luid": {k: {kk: round(vv, 1) for kk, vv in v.items()} for k, v in self._last.items()},
                "own_utilization_percent_by_luid": {
                    k: {"mean": round(self._util_sum[k] / n, 1), "max": round(self._util_max[k], 1)}
                    for k in self._util_sum
                },
                # 只留本进程用到的那几块显卡。
                "others_utilization_mean_percent_by_luid": {
                    k: round(v / n, 1) for k, v in self._others_sum.items() if k in self._peak
                },
            }

    def stop(self) -> dict[str, Any]:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=5)
        return self.snapshot()


# ---------------------------------------------------------------------------
# 汇总
# ---------------------------------------------------------------------------


def collect() -> dict[str, Any]:
    """这台机器和运行环境的一份快照，写进每个结果 JSON 的开头。"""
    info: dict[str, Any] = {
        "os": platform.platform(),
        "os_build": sys.getwindowsversion().build,
        "processor": platform.processor(),
        "logical_cpus": os.cpu_count(),
        "cpu": cpu_and_memory(),
        "python": sys.version.split()[0],
        "dxgi_adapters": [asdict(a) | {"vendor": a.vendor} for a in enum_adapters()],
        "video_controllers": video_controllers(),
    }
    system_dml = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "DirectML.dll"
    if system_dml.is_file():
        info["system_directml"] = {
            "path": str(system_dml),
            "version": file_version(system_dml),
            "size": system_dml.stat().st_size,
        }
    try:
        import onnxruntime as ort

        capi = Path(ort.__file__).parent / "capi"
        info["onnxruntime"] = {
            "package_version": ort.__version__,
            "build_info": ort.get_build_info(),
            "available_providers": ort.get_available_providers(),
            "files": {
                p.name: {"size": p.stat().st_size, "version": file_version(p)}
                for p in sorted(capi.glob("*.dll")) + sorted(capi.glob("*.pyd"))
            },
        }
    except ImportError:
        info["onnxruntime"] = None
    return info


if __name__ == "__main__":
    json.dump(collect() | gpu_load_snapshot(), sys.stdout, ensure_ascii=False, indent=1)
    print()
