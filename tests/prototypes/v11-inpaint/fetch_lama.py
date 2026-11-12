#!/usr/bin/env python3
"""下载 LaMa 的 ONNX 模型并校验（方案 6.5“背景修复”一行，P4 的可选组件；这里只为评估用）。

  来源    Carve/LaMa-ONNX 的 lama_fp32.onnx（big-lama 的 ONNX 导出，opset 17，输入固定 512×512）
  地址    https://hf-mirror.com/Carve/LaMa-ONNX （huggingface.co 在这台机器上连不上，走镜像）
  版本    仓库提交 c3c0c9e468934d62e79c329e35d82dd09ff8c444（2024-05-15）
  大小    208,044,816 字节
  SHA-256 1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6（和仓库里 LFS 记录的一致）
  许可证  Apache-2.0（模型卡标注；原模型 advimman/lama 也是 Apache-2.0）

模型放在仓库外面（默认 D:\\workspace\\magpdf-wt\\models-extra\\），不入库。断了可以接着下。
不走系统代理。

用法：python fetch_lama.py [--dest D:\\workspace\\magpdf-wt\\models-extra\\lama_fp32.onnx]
"""
from __future__ import annotations

import argparse
import hashlib
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

from common import DEFAULT_LAMA_MODEL, write_json

REPO_ID = "Carve/LaMa-ONNX"
REVISION = "c3c0c9e468934d62e79c329e35d82dd09ff8c444"
FILE_NAME = "lama_fp32.onnx"
URL = f"https://hf-mirror.com/{REPO_ID}/resolve/{REVISION}/{FILE_NAME}"
SIZE = 208_044_816
SHA256 = "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6"
LICENSE = "Apache-2.0"


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        while chunk := f.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def is_valid(path: Path) -> bool:
    return path.exists() and path.stat().st_size == SIZE and sha256_of(path) == SHA256


def download(dest: Path) -> None:
    part = dest.with_suffix(dest.suffix + ".part")
    dest.parent.mkdir(parents=True, exist_ok=True)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    have = part.stat().st_size if part.exists() else 0
    if have > SIZE:
        part.unlink()
        have = 0
    if have < SIZE:
        request = urllib.request.Request(URL, headers={"User-Agent": "magpdf-v11-prototype", "Range": f"bytes={have}-"})
        with opener.open(request, timeout=60) as response:
            if have and response.status != 206:
                have = 0  # 服务器不支持续传，从头来
            mode = "ab" if have else "wb"
            last_report = time.monotonic()
            with part.open(mode) as f:
                while chunk := response.read(1 << 20):
                    f.write(chunk)
                    have += len(chunk)
                    if time.monotonic() - last_report >= 15:
                        last_report = time.monotonic()
                        print(f"  已下载 {have / 1e6:.0f} / {SIZE / 1e6:.0f} MB", flush=True)
    if part.stat().st_size != SIZE:
        raise OSError(f"大小不对：{part.stat().st_size}，应为 {SIZE}（再运行一次会接着下）")
    actual = sha256_of(part)
    if actual != SHA256:
        part.unlink()
        raise OSError(f"SHA-256 不对：{actual}")
    part.replace(dest)


def main() -> None:
    ap = argparse.ArgumentParser(description="下载并校验 LaMa 的 ONNX 模型")
    ap.add_argument("--dest", type=Path, default=DEFAULT_LAMA_MODEL)
    args = ap.parse_args()
    if is_valid(args.dest):
        print(f"已经有了，校验通过：{args.dest}")
    else:
        print(f"下载 {URL}")
        start = time.monotonic()
        try:
            download(args.dest)
        except (urllib.error.URLError, OSError) as err:
            print(f"下载失败：{err}", file=sys.stderr)
            sys.exit(1)
        print(f"下载完成，用时 {time.monotonic() - start:.0f} 秒，校验通过：{args.dest}")
    # 旁边留一份说明，记下来源和校验值
    write_json(
        args.dest.with_suffix(".json"),
        {"file": FILE_NAME, "source": URL, "repo": REPO_ID, "revision": REVISION, "size": SIZE, "sha256": SHA256, "license": LICENSE},
    )


if __name__ == "__main__":
    main()
