#!/usr/bin/env python3
"""语料目录的指纹：目录下全部 PDF 的相对路径和内容算出的一个 SHA-256。

用来确认两次往返测试跑的是同一份文件。PDFium 的测试 PDF 在 `core.autocrlf=true` 的检出里会被改成 CRLF
（`third_party/pdfium/README.md` 第 2 节），文件名、文件数都不变，只有内容变了，往返结果因此不可比——
指纹能把这种情况查出来。基线里也记着生成它时的指纹。

算法：递归收集扩展名是 .pdf 的文件（不分大小写，和 `magpdf-cli roundtrip` 收集文件的规则一样）；
相对路径用 `/` 分隔、按字符串排序；每个文件一行 `<内容的 SHA-256>  <相对路径>\n`，UTF-8；整段再取 SHA-256。

用法：
    python tests/corpus/tools/corpus_fingerprint.py <目录> [--expect <sha256>] [--expect-files N]
    --expect 对不上时退出码 1。

只用标准库。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any, Iterable


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def corpus_fingerprint(root: Path) -> dict[str, Any]:
    """返回 `{"files": 文件数, "bytes": 总字节数, "sha256": 指纹}`。"""
    entries: list[tuple[str, Path]] = []
    for path in root.rglob("*"):
        if path.is_file() and path.suffix.lower() == ".pdf":
            entries.append((path.relative_to(root).as_posix(), path))
    entries.sort(key=lambda item: item[0])
    digest = hashlib.sha256()
    total = 0
    for rel, path in entries:
        total += path.stat().st_size
        digest.update(f"{sha256_of(path)}  {rel}\n".encode("utf-8"))
    return {"files": len(entries), "bytes": total, "sha256": digest.hexdigest()}


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="语料目录的指纹（全部 PDF 的相对路径和内容）")
    parser.add_argument("root", type=Path, help="语料目录")
    parser.add_argument("--expect", help="期望的指纹（SHA-256），对不上退出码 1")
    parser.add_argument("--expect-files", type=int, help="期望的文件数，对不上退出码 1")
    args = parser.parse_args(list(argv) if argv is not None else None)
    if not args.root.is_dir():
        print(f"找不到目录：{args.root}", file=sys.stderr)
        return 1
    result = corpus_fingerprint(args.root)
    print(json.dumps(result, ensure_ascii=False))
    ok = True
    if args.expect_files is not None and result["files"] != args.expect_files:
        print(f"文件数不对：期望 {args.expect_files}，实际 {result['files']}", file=sys.stderr)
        ok = False
    if args.expect and result["sha256"].lower() != args.expect.lower():
        print(
            f"指纹不对：期望 {args.expect.lower()}，实际 {result['sha256']}。"
            "如果是 PDFium 的测试文件，先查检出时 core.autocrlf 是不是 false。",
            file=sys.stderr,
        )
        ok = False
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
