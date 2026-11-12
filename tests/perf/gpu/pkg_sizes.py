"""安装包大小：换成带 DirectML 的 ONNX Runtime 要多带多少（V10）。

从 NuGet 下载下面几个包（下载到 D 盘的缓存目录，不解到别处），列出 64 位 Windows 的原生 DLL：
文件大小、版本、用 LZMA 压过之后的大小（安装包一般会压，按这个估进包之后占多少），
以及包里声明的许可证和带的许可证文件名。

    python pkg_sizes.py [--out pkg-sizes.json] [--cache-dir D:\\workspace\\magpdf-wt\\tmp\\v10\\nuget]

只用标准库。
"""
from __future__ import annotations

import argparse
import hashlib
import json
import lzma
import re
import sys
import time
import urllib.request
import zipfile
from pathlib import Path
from typing import Any

import sysinfo

# （包名，版本，说明）
PACKAGES: tuple[tuple[str, str, str], ...] = (
    ("Microsoft.ML.OnnxRuntime", "1.31.1", "产品现在用的 CPU 版"),
    ("Microsoft.ML.OnnxRuntime", "1.24.4", "和 DirectML 版同一个版本的 CPU 版"),
    ("Microsoft.ML.OnnxRuntime.DirectML", "1.24.4", "带 DirectML 执行提供程序的 ONNX Runtime（官方最后一个版本）"),
    ("Microsoft.AI.DirectML", "1.15.4", "DirectML.dll 可再分发包"),
    ("Microsoft.Windows.AI.MachineLearning", "2.5.96", "Windows ML（自带 onnxruntime.dll 和 DirectML.dll）"),
)

# 三条路各要随包带哪些 DLL：（说明，((包名, 版本, 文件名), …)）。第一条是产品现在的做法，后面的和它比。
ROUTES: tuple[tuple[str, tuple[tuple[str, str, str], ...]], ...] = (
    ("现在：CPU 版 ONNX Runtime", (("Microsoft.ML.OnnxRuntime", "1.31.1", "onnxruntime.dll"),)),
    (
        "DirectML：带 DirectML 的 ONNX Runtime + DirectML.dll",
        (
            ("Microsoft.ML.OnnxRuntime.DirectML", "1.24.4", "onnxruntime.dll"),
            ("Microsoft.AI.DirectML", "1.15.4", "DirectML.dll"),
        ),
    ),
    (
        "Windows ML，运行时随包带",
        (
            ("Microsoft.Windows.AI.MachineLearning", "2.5.96", "Microsoft.Windows.AI.MachineLearning.dll"),
            ("Microsoft.Windows.AI.MachineLearning", "2.5.96", "onnxruntime.dll"),
            ("Microsoft.Windows.AI.MachineLearning", "2.5.96", "DirectML.dll"),
        ),
    ),
)

# 各包里 64 位 Windows 原生文件所在的目录。
_X64_DIRS = ("runtimes/win-x64/native/", "bin/x64-win/", "runtimes-framework/win-x64/native/")
_LICENSE_NAME = re.compile(r"(license|notice|thirdparty|privacy)", re.IGNORECASE)


def download(package: str, version: str, cache_dir: Path) -> Path:
    name = f"{package.lower()}.{version}.nupkg"
    path = cache_dir / name
    if path.is_file():
        return path
    cache_dir.mkdir(parents=True, exist_ok=True)
    url = f"https://api.nuget.org/v3-flatcontainer/{package.lower()}/{version}/{name}"
    partial = path.with_suffix(".part")
    for attempt in range(1, 6):
        try:
            with urllib.request.urlopen(url, timeout=120) as response, partial.open("wb") as out:
                while chunk := response.read(1 << 20):
                    out.write(chunk)
            break
        except OSError:  # 网络一时不通（URLError 也是 OSError），隔一会儿再试
            if attempt == 5:
                raise
            time.sleep(10 * attempt)
    partial.replace(path)
    return path


def _nuspec_license(archive: zipfile.ZipFile) -> dict[str, str | None]:
    for entry in archive.namelist():
        if entry.endswith(".nuspec") and "/" not in entry:
            text = archive.read(entry).decode("utf-8", errors="replace")
            license_tag = re.search(r"<license[^>]*type=\"([^\"]+)\"[^>]*>([^<]*)</license>", text)
            url = re.search(r"<licenseUrl>([^<]*)</licenseUrl>", text)
            project = re.search(r"<projectUrl>([^<]*)</projectUrl>", text)
            return {
                "license_type": license_tag.group(1) if license_tag else None,
                "license": license_tag.group(2).strip() if license_tag else None,
                "license_url": url.group(1).strip() if url else None,
                "project_url": project.group(1).strip() if project else None,
            }
    return {"license_type": None, "license": None, "license_url": None, "project_url": None}


def inspect(package: str, version: str, note: str, cache_dir: Path) -> dict[str, Any]:
    nupkg = download(package, version, cache_dir)
    extract_dir = cache_dir / "x64" / f"{package}.{version}"
    files: list[dict[str, Any]] = []
    with zipfile.ZipFile(nupkg) as archive:
        info: dict[str, Any] = {
            "package": package,
            "version": version,
            "note": note,
            "nupkg_bytes": nupkg.stat().st_size,
            **_nuspec_license(archive),
            "license_files": sorted(e for e in archive.namelist() if "/" not in e and _LICENSE_NAME.search(e)),
        }
        for entry in archive.infolist():
            lower = entry.filename.lower()
            if not lower.endswith(".dll") or not any(d in lower for d in _X64_DIRS):
                continue
            data = archive.read(entry)
            target = extract_dir / Path(entry.filename).name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)  # 读版本号要一份落在磁盘上的文件
            files.append(
                {
                    "path": entry.filename,
                    "bytes": entry.file_size,
                    "lzma_bytes": len(lzma.compress(data, preset=6)),
                    "version": sysinfo.file_version(target),
                    "sha256": hashlib.sha256(data).hexdigest(),
                }
            )
    info["x64_dlls"] = sorted(files, key=lambda f: f["path"])
    return info


def routes(packages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """每条路要带的 DLL 加起来多大，比现在的做法多多少（原始大小和 LZMA 压过之后的大小）。"""
    result: list[dict[str, Any]] = []
    for name, wanted in ROUTES:
        files = []
        for package, version, dll in wanted:
            found = next(
                f
                for p in packages
                if p["package"] == package and p["version"] == version
                for f in p["x64_dlls"]
                if f["path"].rsplit("/", 1)[-1] == dll
            )
            files.append(
                {
                    "package": package,
                    "package_version": version,
                    "file": dll,
                    "bytes": found["bytes"],
                    "lzma_bytes": found["lzma_bytes"],
                    "file_version": found["version"],
                }
            )
        total = sum(f["bytes"] for f in files)
        total_lzma = sum(f["lzma_bytes"] for f in files)
        base = result[0] if result else None
        result.append(
            {
                "name": name,
                "files": files,
                "bytes": total,
                "lzma_bytes": total_lzma,
                "extra_bytes": total - base["bytes"] if base else 0,
                "extra_lzma_bytes": total_lzma - base["lzma_bytes"] if base else 0,
            }
        )
    return result


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=None, help="结果写到这个文件；不给就打印出来")
    ap.add_argument("--cache-dir", type=Path, default=Path(r"D:\workspace\magpdf-wt\tmp\v10\nuget"))
    args = ap.parse_args()
    packages = [inspect(p, v, note, args.cache_dir) for p, v, note in PACKAGES]
    result = {"packages": packages, "routes": routes(packages)}
    text = json.dumps(result, ensure_ascii=False, indent=1)
    if args.out is not None:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text, encoding="utf-8")
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
