"""测试文件库工具的公共部分：定位 magpdf-cli、调用 `info --json`、哈希、目录到分类的映射。

只用 Python 3.12 标准库。被 tools/make_manifest.py 与 synthetic/check.py 共用。
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
CORPUS_ROOT = REPO_ROOT / "tests" / "corpus"

# 目录名 → 规范第 2 节的分类字母。S 是本轮新增：针对生成器缺陷逐项构造的自制样本（规范里没有对应分类，报告中提出补充）。
CATEGORY_BY_DIR = {
    "office-word": "A",
    "office-pptx-xlsx": "B",
    "design": "C",
    "browser-report": "D",
    "scan": "E",
    "freight": "F",
    "public": "G",
    "special": "H",
    "synthetic": "S",
}

ORIGIN_BY_CATEGORY = {"G": "public", "S": "self"}


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def find_cli(explicit: str | None = None) -> list[str] | None:
    """返回可执行 magpdf-cli 的命令前缀；找不到返回 None。

    查找顺序：--cli 参数 → 环境变量 MAGPDF_CLI → 仓库 target/{debug,release} → PATH。
    不自动调用 cargo run，避免在别人的 worktree 里触发编译。
    """
    candidates: list[str] = []
    if explicit:
        candidates.append(explicit)
    if os.environ.get("MAGPDF_CLI"):
        candidates.append(os.environ["MAGPDF_CLI"])
    exe = "magpdf-cli.exe" if sys.platform == "win32" else "magpdf-cli"
    for profile in ("debug", "release"):
        candidates.append(str(REPO_ROOT / "target" / profile / exe))
    for cand in candidates:
        if Path(cand).is_file():
            return [cand]
    found = shutil.which("magpdf-cli")
    return [found] if found else None


def cli_info(cli: list[str], pdf: Path, timeout: float = 60) -> dict | None:
    """运行 `magpdf-cli info <pdf> --json`，失败返回 None（stderr 放在返回 dict 的 `_error`）。"""
    try:
        proc = subprocess.run(
            [*cli, "info", str(pdf), "--json"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {"_error": f"{type(exc).__name__}: {exc}"}
    if proc.returncode != 0:
        return {"_error": (proc.stderr or proc.stdout).strip()[-800:]}
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        return {"_error": f"bad json: {exc}"}


def cli_render(cli: list[str], pdf: Path, out_png: Path, page: int = 1, dpi: float = 72, annots: bool = False, timeout: float = 120) -> str | None:
    """渲染一页为 PNG，成功返回 None，失败返回错误文本。"""
    args = [*cli, "render", str(pdf), "--page", str(page), "--dpi", str(dpi), "--output", str(out_png)]
    if annots:
        args.append("--annots")
    try:
        proc = subprocess.run(args, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return f"{type(exc).__name__}: {exc}"
    if proc.returncode != 0:
        return (proc.stderr or proc.stdout).strip()[-800:]
    return None


def dump_json(obj: object, path: Path) -> None:
    """确定性 JSON：UTF-8、不转义中文、2 空格缩进、末尾换行、LF。"""
    text = json.dumps(obj, ensure_ascii=False, indent=2)
    path.write_bytes((text + "\n").encode("utf-8"))


def load_json(path: Path) -> dict | None:
    if not path.is_file():
        return None
    return json.loads(path.read_text(encoding="utf-8"))
