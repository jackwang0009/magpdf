#!/usr/bin/env python3
"""重新生成 tests/corpus/synthetic/ 下的全部自制样本与 manifest.json。

用法：
    python tests/corpus/synthetic/generate.py            # 生成全部样本并写 manifest.json
    python tests/corpus/synthetic/generate.py --check    # 生成后再运行 check.py（magpdf-cli 统计比对 + qpdf 结构检查）
    python tests/corpus/synthetic/generate.py --only text-type3 --only color-lab
    python tests/corpus/synthetic/generate.py --list

要求：Python 3.12，只用标准库；输出确定（无时间戳、无随机数），重复运行逐字节相同。
样本定义在 samples_*.py，每个样本声明：特性标签、要观察的现象、涉及的方案条目、以及 magpdf-cli `info --json` 应得到的页面与对象统计。
"""

from __future__ import annotations

import argparse
import hashlib
import importlib
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "tools"))

from common import REGISTRY, Sample, resolve_pages  # noqa: E402
from corpuslib import dump_json  # noqa: E402

SAMPLE_MODULES = ["samples_color", "samples_gs_text", "samples_struct", "samples_composite"]
MANIFEST_SCHEMA = 1
GROUP_TITLES = {
    "color": "颜色空间与图案（方案 2.1 第 1 条）",
    "shading": "渐变 sh（方案 2.1 第 2 条）",
    "extgstate": "扩展图形状态（方案 2.1 第 3 条）",
    "text": "文字：字距、渲染模式、字体类型（方案 2.1 第 4、5 条）",
    "structure": "内容流、页面与文件结构（方案 2.1 第 6 条、2.3、9.4/10.6）",
    "composite": "货代单证风格综合样本（方案 13.1）",
}


def load_samples() -> list[Sample]:
    for mod in SAMPLE_MODULES:
        importlib.import_module(mod)
    names = [s.name for s in REGISTRY]
    dupes = {n for n in names if names.count(n) > 1}
    if dupes:
        raise SystemExit(f"duplicate sample names: {sorted(dupes)}")
    return list(REGISTRY)


def manifest_entry(index: int, s: Sample, data: bytes) -> dict:
    pages = resolve_pages(s)
    if not pages:
        raise SystemExit(f"{s.name}: no page expectations")
    return {
        "id": f"S-{index:04d}",
        "category": "S",
        "file": s.file,
        "sha256": hashlib.sha256(data).hexdigest(),
        "size": len(data),
        "pages": len(pages),
        "origin": "self",
        "license": "project",
        "desensitized": "na",
        "desensitizedBy": None,
        "reviewedBy": None,
        "reviewedOn": None,
        "originalDeletedOn": None,
        "tags": s.tags,
        "expected": s.expected,
        "issues": [],
        # 以下为本轮新增字段（报告中提议写进规范第 6 节）
        "title": s.title,
        "group": s.group,
        "refs": s.refs,
        "pdfVersion": s.pdf_version,
        "generator": "tests/corpus/synthetic/generate.py",
        "check": s.check,
        "checkNote": s.check_note or None,
        "expect": {"pages": len(pages), "pageInfo": pages},
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="生成后运行 check.py")
    ap.add_argument("--only", action="append", default=[], help="只生成指定样本（可重复）")
    ap.add_argument("--list", action="store_true", help="列出样本后退出")
    ap.add_argument("--out", type=Path, default=HERE, help="输出目录（默认与脚本同目录）")
    ap.add_argument("check_args", nargs=argparse.REMAINDER, help="传给 check.py 的参数（放在 -- 之后）")
    args = ap.parse_args()

    samples = load_samples()
    if args.list:
        for s in samples:
            print(f"{s.group:10s} {s.name:36s} {s.check:11s} {s.title}")
        print(f"{len(samples)} samples")
        return 0

    selected = [s for s in samples if not args.only or s.name in args.only]
    missing = set(args.only) - {s.name for s in selected}
    if missing:
        raise SystemExit(f"unknown samples: {sorted(missing)}")

    out_dir: Path = args.out
    out_dir.mkdir(parents=True, exist_ok=True)
    entries = []
    total = 0
    changed = 0
    for index, s in enumerate(samples, start=1):
        if s not in selected:
            continue
        data = s.build()
        path = out_dir / s.file
        old = path.read_bytes() if path.is_file() else None
        if old != data:
            path.write_bytes(data)
            changed += 1
        total += len(data)
        entries.append(manifest_entry(index, s, data))

    if not args.only:
        manifest = {
            "schema": MANIFEST_SCHEMA,
            "directory": "synthetic",
            "category": "S",
            "description": "自制样本：针对 PDFium 内容流生成器缺陷与方案 2.3 生成规则逐项构造；由 generate.py 确定性生成。",
            "groups": GROUP_TITLES,
            "files": entries,
        }
        dump_json(manifest, out_dir / "manifest.json")
    print(f"generated {len(entries)} samples ({total} bytes, {changed} file(s) changed) -> {out_dir}")

    if args.check:
        extra = [a for a in args.check_args if a != "--"]
        return subprocess.call([sys.executable, str(HERE / "check.py"), *extra])
    return 0


if __name__ == "__main__":
    sys.exit(main())
