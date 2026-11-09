#!/usr/bin/env python3
"""校验 tests/corpus/synthetic/ 下的样本与 manifest.json。

三项检查：
1. 完整性：文件存在、SHA-256 与大小和 manifest 一致（重新生成后应逐字节相同）。
2. magpdf-cli `info --json`：能打开；页数、每页尺寸/旋转、对象类型统计与 manifest 的 `expect` 一致。
3. 结构：pikepdf（qpdf）打开并 check_pdf_syntax。`check: pass` 的样本不得有任何警告；
   `check: expect-warn` 的样本（故意损坏）必须至少有一条警告、且仍能打开——否则说明损坏没有做出来。

用法：
    python tests/corpus/synthetic/check.py [--cli PATH] [--render DIR] [--dpi 72] [--only NAME ...]
    --render DIR 时把每个样本第 1 页渲染成 PNG（带批注的样本再渲染一份 *-annots.png）供肉眼检查；DIR 建议用 tests/out/。

magpdf-cli 找不到时跳过第 2 项并提示（先 cargo build -p magpdf-cli）；pikepdf 没装时跳过第 3 项并提示
（pip install pikepdf；MPL-2.0，内置 qpdf Apache-2.0，仅作测试工具）。任一项失败退出码为 1。
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "tools"))

from corpuslib import cli_info, cli_render, find_cli, load_json, sha256_of  # noqa: E402

INFO_FIELDS = ("width", "height", "rotation", "objects", "text", "path", "image", "shading", "form")


def compare_info(expect: dict, info: dict) -> list[str]:
    problems: list[str] = []
    if info.get("pages") != expect["pages"]:
        problems.append(f"pages {info.get('pages')} != {expect['pages']}")
    got_pages = info.get("page_info") or []
    for i, (exp, got) in enumerate(zip(expect["pageInfo"], got_pages), start=1):
        for key in INFO_FIELDS:
            e, g = exp[key], got.get(key)
            if isinstance(e, float) or isinstance(g, float):
                ok = g is not None and abs(float(e) - float(g)) < 0.01
            else:
                ok = e == g
            if not ok:
                problems.append(f"p{i}.{key} {g} != {e}")
    return problems


def qpdf_check(path: Path) -> tuple[list[str], str | None]:
    """返回 (警告/问题列表, 打不开时的错误)。"""
    import pikepdf  # noqa: PLC0415

    try:
        with pikepdf.open(path) as pdf:
            problems = [str(w) for w in pdf.get_warnings()]
            try:
                problems += [str(p) for p in pdf.check_pdf_syntax()]
            except Exception as exc:  # noqa: BLE001
                problems.append(f"check_pdf_syntax: {exc}")
            _ = len(pdf.pages)
            return problems, None
    except Exception as exc:  # noqa: BLE001
        return [], f"{type(exc).__name__}: {exc}"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cli", help="magpdf-cli 可执行文件路径")
    ap.add_argument("--manifest", type=Path, default=HERE / "manifest.json")
    ap.add_argument("--render", type=Path, help="渲染第 1 页 PNG 到该目录（肉眼检查用，不入库）")
    ap.add_argument("--dpi", type=float, default=72.0)
    ap.add_argument("--only", action="append", default=[])
    ap.add_argument("--no-qpdf", action="store_true", help="跳过 pikepdf/qpdf 结构检查")
    args = ap.parse_args()

    manifest = load_json(args.manifest)
    if not manifest:
        print(f"manifest not found: {args.manifest}")
        return 1
    files = [f for f in manifest["files"] if not args.only or Path(f["file"]).stem in args.only]
    base = args.manifest.parent

    cli = find_cli(args.cli)
    if cli is None:
        print("WARN magpdf-cli not found (build with `cargo build -p magpdf-cli` or set MAGPDF_CLI); skipping info/render checks")
    pikepdf_ok = False
    if not args.no_qpdf:
        try:
            import pikepdf  # noqa: F401,PLC0415

            pikepdf_ok = True
        except ImportError:
            print("WARN pikepdf not installed (pip install pikepdf); skipping qpdf structure checks")
    if args.render:
        args.render.mkdir(parents=True, exist_ok=True)

    failures = 0
    rows: list[tuple[str, str, str, str, str]] = []
    for entry in files:
        name = entry["file"]
        path = base / name
        notes: list[str] = []
        status_integrity = status_info = status_qpdf = "-"

        # 1. 完整性
        if not path.is_file():
            status_integrity = "MISSING"
            notes.append("file missing")
        else:
            sha = sha256_of(path)
            size = path.stat().st_size
            if sha != entry["sha256"] or size != entry["size"]:
                status_integrity = "FAIL"
                notes.append("sha256/size differ from manifest (regenerate or update manifest)")
            else:
                status_integrity = "ok"

        # 2. magpdf-cli info
        if cli and path.is_file():
            info = cli_info(cli, path)
            if info is None or "_error" in info:
                status_info = "FAIL"
                notes.append(f"info: {info.get('_error') if info else 'no output'}")
            else:
                problems = compare_info(entry["expect"], info)
                if problems:
                    status_info = "FAIL"
                    notes.append("info: " + "; ".join(problems))
                else:
                    status_info = "ok"
            if args.render:
                err = cli_render(cli, path, args.render / f"{Path(name).stem}.png", dpi=args.dpi)
                if err:
                    status_info = "FAIL"
                    notes.append(f"render: {err}")
                elif "annots" in entry.get("tags", []):
                    cli_render(cli, path, args.render / f"{Path(name).stem}-annots.png", dpi=args.dpi, annots=True)

        # 3. qpdf
        if pikepdf_ok and path.is_file():
            problems, fatal = qpdf_check(path)
            expect_warn = entry.get("check") == "expect-warn"
            if fatal:
                status_qpdf = "FAIL"
                notes.append(f"qpdf cannot open: {fatal}")
            elif expect_warn:
                if problems:
                    status_qpdf = "warn(expected)"
                    notes.append(f"qpdf: {problems[0][:120]}")
                else:
                    status_qpdf = "FAIL"
                    notes.append("qpdf reported nothing, but sample is supposed to be damaged")
            elif problems:
                status_qpdf = "FAIL"
                notes.append("qpdf: " + " | ".join(p[:120] for p in problems[:3]))
            else:
                status_qpdf = "ok"

        failed = any(s in ("FAIL", "MISSING") for s in (status_integrity, status_info, status_qpdf))
        failures += failed
        rows.append((name, status_integrity, status_info, status_qpdf, "; ".join(notes)))

    width = max(len(r[0]) for r in rows) if rows else 20
    print(f"{'file':{width}s} {'sha':8s} {'info':8s} {'qpdf':14s} notes")
    for r in rows:
        print(f"{r[0]:{width}s} {r[1]:8s} {r[2]:8s} {r[3]:14s} {r[4]}")
    print(f"\n{len(rows)} files checked, {failures} failed"
          + ("" if cli else " (magpdf-cli checks skipped)")
          + ("" if pikepdf_ok or args.no_qpdf else " (qpdf checks skipped)"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
