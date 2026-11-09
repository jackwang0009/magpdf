#!/usr/bin/env python3
"""扫描一个来源目录，生成或更新其 manifest.json（《测试样本收集与脱敏规范》第 6 节字段）。

用法：
    python tests/corpus/tools/make_manifest.py tests/corpus/public
    python tests/corpus/tools/make_manifest.py tests/corpus/office-word --cli target/debug/magpdf-cli.exe
    python tests/corpus/tools/make_manifest.py <dir> --category F --origin partner

行为：
- 递归收集目录下的 *.pdf（忽略 manifest.json 与非 PDF），按相对路径排序。
- 自动字段：file、sha256、size、pages（调用 magpdf-cli `info --json`；找不到可执行文件或打开失败时为 null，并在 note 里说明）。
- 分类字母按目录名映射（office-word→A … public→G，synthetic→S），可用 --category 覆盖；origin 按分类推断（public→public、synthetic→self），
  其余留空由测试组填写，可用 --origin 覆盖。
- 人工字段（license、desensitized、desensitizedBy、reviewedBy、reviewedOn、originalDeletedOn、tags、expected、issues、id）
  在已有 manifest.json 里存在时原样保留；新文件给出占位值。目录里已不存在的文件从清单中删除并打印提示。
- id 为"分类字母-四位序号"，已有条目保留原 id，新条目接在最大序号之后。
- 输出确定（按文件路径排序、UTF-8、LF）。synthetic/ 目录由 generate.py 自己写清单，不要对它用本工具。
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from corpuslib import CATEGORY_BY_DIR, ORIGIN_BY_CATEGORY, cli_info, dump_json, find_cli, load_json, sha256_of  # noqa: E402

MANIFEST_SCHEMA = 1
MANUAL_FIELDS = (
    "id",
    "origin",
    "license",
    "desensitized",
    "desensitizedBy",
    "reviewedBy",
    "reviewedOn",
    "originalDeletedOn",
    "tags",
    "expected",
    "issues",
    "title",
    "note",
)


def placeholder(category: str, origin: str | None) -> dict:
    needs_desensitize = category in ("D", "E", "F")  # 真实来源的分类默认标为未脱敏，入库前必须改
    return {
        "origin": origin or "",
        "license": "",
        "desensitized": "no" if needs_desensitize else "na",
        "desensitizedBy": None,
        "reviewedBy": None,
        "reviewedOn": None,
        "originalDeletedOn": None,
        "tags": [],
        "expected": "",
        "issues": [],
        "title": "",
        "note": "",
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("directory", type=Path)
    ap.add_argument("--cli", help="magpdf-cli 路径（默认自动查找）")
    ap.add_argument("--category", help="分类字母 A–H/S，默认按目录名")
    ap.add_argument("--origin", help="self / public / partner / interview，默认按分类推断")
    ap.add_argument("--no-pages", action="store_true", help="不调用 magpdf-cli 统计页数")
    args = ap.parse_args()

    directory: Path = args.directory.resolve()
    if not directory.is_dir():
        print(f"not a directory: {directory}")
        return 1
    category = args.category or CATEGORY_BY_DIR.get(directory.name, "")
    if not category:
        print(f"cannot infer category from directory name {directory.name!r}; pass --category")
        return 1
    origin = args.origin or ORIGIN_BY_CATEGORY.get(category)

    manifest_path = directory / "manifest.json"
    old = load_json(manifest_path) or {}
    old_entries = {e["file"]: e for e in old.get("files", [])}

    cli = None if args.no_pages else find_cli(args.cli)
    if cli is None and not args.no_pages:
        print("WARN magpdf-cli not found; pages will be null (set MAGPDF_CLI or pass --cli)")

    pdfs = sorted(p for p in directory.rglob("*.pdf") if p.is_file())
    used_ids = {e.get("id") for e in old_entries.values() if e.get("id")}
    next_no = 1 + max((int(i.split("-")[1]) for i in used_ids if i and i.startswith(f"{category}-") and i.split("-")[1].isdigit()), default=0)

    entries = []
    for pdf in pdfs:
        rel = pdf.relative_to(directory).as_posix()
        prev = old_entries.pop(rel, None)
        entry = {"file": rel, "category": category}
        base = placeholder(category, origin)
        for key in MANUAL_FIELDS:
            if prev is not None and key in prev:
                entry[key] = prev[key]
            elif key in base:
                entry[key] = base[key]
        if not entry.get("id"):
            entry["id"] = f"{category}-{next_no:04d}"
            next_no += 1
        entry["sha256"] = sha256_of(pdf)
        entry["size"] = pdf.stat().st_size
        pages = None
        note = entry.get("note") or ""
        if cli:
            info = cli_info(cli, pdf)
            if info and "_error" not in info:
                pages = info.get("pages")
                entry["pdfVersion"] = (lambda v: f"{v // 10}.{v % 10}" if isinstance(v, int) else None)(info.get("pdf_version"))
                entry["pageInfo"] = [
                    {k: p.get(k) for k in ("width", "height", "rotation", "objects", "text", "path", "image", "shading", "form")}
                    for p in info.get("page_info", [])
                ]
            else:
                note = (note + " | " if note else "") + f"magpdf-cli info failed: {(info or {}).get('_error', 'no output')}"
        elif prev is not None:
            pages = prev.get("pages")
        entry["pages"] = pages
        entry["note"] = note
        # 固定键顺序，便于 diff
        ordered = {
            k: entry.get(k)
            for k in (
                "id", "category", "file", "sha256", "size", "pages", "pdfVersion", "origin", "license", "desensitized",
                "desensitizedBy", "reviewedBy", "reviewedOn", "originalDeletedOn", "tags", "expected", "issues",
                "title", "note", "pageInfo",
            )
            if k in entry
        }  # fmt: skip
        entries.append(ordered)

    for gone in sorted(old_entries):
        print(f"removed from manifest (file no longer present): {gone}")

    manifest = {
        "schema": MANIFEST_SCHEMA,
        "directory": directory.name,
        "category": category,
        "description": old.get("description", ""),
        "files": entries,
    }
    dump_json(manifest, manifest_path)
    missing_pages = sum(1 for e in entries if e["pages"] is None)
    print(f"{manifest_path}: {len(entries)} file(s), {missing_pages} without page count")
    return 0


if __name__ == "__main__":
    sys.exit(main())
