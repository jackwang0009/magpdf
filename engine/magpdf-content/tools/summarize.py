"""把一轮实验（run-experiments.ps1 写出的那些 JSON）汇总成报告里用的表格。

用法：
    python -X dev engine/magpdf-content/tools/summarize.py tests/out/v17/r2
    python -X dev engine/magpdf-content/tools/summarize.py tests/out/v17/r2 --reasons --hashes

只读 JSON 里的 `total`（合计数由 magpdf-cli 从页记录算出）和页记录，不重新计算比例以外的东西。
输出里只有文件名、页序号和计数，没有文档内容。
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

EXPERIMENTS = ["roundtrip", "modify", "modify-random", "modify-regenerated"]
CORPORA = ["synthetic", "public"]
BUILDS = ["stock", "patched"]
REWRITE_ONLY = "改写层写不了这次修改"


def load(path: Path) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    with path.open(encoding="utf-8") as handle:
        data: dict[str, Any] = json.load(handle)
    return data


def pct(a: int, b: int) -> str:
    return "—" if b == 0 else f"{a / b * 100:.2f}%"


def tally(t: dict[str, int] | None) -> str:
    if t is None:
        return "—"
    compared = t["pass"] + t["diff"]
    return f"{t['pass']} / {compared}（{pct(t['pass'], compared)}）"


def summary_row(name: str, total: dict[str, Any]) -> str:
    counted = total["pages"] - total["pages_error"]
    unmatched = total["pages_fallback"] - total["pages_rewrite_unsupported"]
    return (
        f"| {name} | {total['files']} | {total['load_failed']} | "
        f"{total['crashed'] + total['timed_out']} | {total['object_layer_failed']} | "
        f"{counted} | {total['pages_matched']} | {total['pages_fallback']} | "
        f"{unmatched}（{pct(unmatched, counted)}） | {total['pages_rewrite_unsupported']} | "
        f"{total['streams_identical']} / {total['streams']}（{pct(total['streams_identical'], total['streams'])}） | "
        f"{tally(total['verify'])} | {tally(total['verify_matched'])} | {tally(total['verify_fallback'])} | "
        f"{tally(total['verify_edited'])} | {tally(total.get('control'))} | {tally(total.get('control_edited'))} |"
    )


HEADER = (
    "| 实验 | 文件 | 打不开 | 崩溃/超时 | 对象层打不开 | 参加统计的页 | 对得上 | 回退 | 对不上的页 | 其中只是修改写不了 | "
    "逐字节相同的内容流 | 全部页无差异 | 改写层写的页 | 回退的页 | 做了修改的页 | 对照组全部页 | 对照组做了修改的页 |\n"
    "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
)


def print_reasons(name: str, data: dict[str, Any]) -> None:
    reasons: dict[str, int] = data["total"]["fallback_reasons"]
    if not reasons:
        return
    print(f"\n{name} 的回退原因（页数）：")
    for reason, count in sorted(reasons.items(), key=lambda kv: (-kv[1], kv[0])):
        print(f"- {reason}：{count}")


def print_pages(name: str, data: dict[str, Any], limit: int) -> None:
    """列出对不上的页（原因和细节），以及像素比对不过的页。"""
    shown = 0
    for file in data["files"]:
        for page in file.get("page_records", []):
            reasons = [r for r in page.get("reasons", [])]
            verify = page.get("verify") or {}
            bad_pixels = verify.get("status") == "diff"
            unmatched = page["route"] == "fallback" and reasons != [REWRITE_ONLY]
            if not (unmatched or bad_pixels or page["route"] == "fallback"):
                continue
            if shown >= limit:
                print(f"…（{name} 还有更多，已截断）")
                return
            shown += 1
            notes = "；".join(page.get("notes", []))
            print(
                f"{name} | {file['path']} p{page['index'] + 1} | {page['route']} | "
                f"{'、'.join(reasons) or '-'} | 对象 {page['objects']} | 像素 {verify.get('status', '-')} | {notes}"
            )


def hashes(data: dict[str, Any]) -> dict[tuple[str, int], tuple[str | None, str | None, str]]:
    out: dict[tuple[str, int], tuple[str | None, str | None, str]] = {}
    for file in data["files"]:
        for page in file.get("page_records", []):
            verify = page.get("verify")
            if verify:
                out[(file["path"], page["index"])] = (
                    verify.get("expected_hash"),
                    verify.get("actual_hash"),
                    verify["status"],
                )
    return out


def print_hash_comparison(root: Path, corpus: str, experiment: str) -> None:
    """同一个实验换一份 PDFium：哪些页的结论变了，变的是预期画面还是保存结果的画面。"""
    stock = load(root / f"{corpus}-stock-{experiment}.json")
    patched = load(root / f"{corpus}-patched-{experiment}.json")
    if stock is None or patched is None:
        return
    a, b = hashes(stock), hashes(patched)
    changed = [key for key in sorted(a) if key in b and a[key][2] != b[key][2]]
    exp_changed = sum(1 for key in a if key in b and a[key][0] != b[key][0])
    act_changed = sum(1 for key in a if key in b and a[key][1] != b[key][1])
    print(
        f"\n{corpus} {experiment}：共 {len(a)} 页；预期画面的指纹在两份 PDFium 下不同的 {exp_changed} 页，"
        f"保存结果的画面不同的 {act_changed} 页；结论不同的 {len(changed)} 页："
    )
    for key in changed:
        sa, sb = a[key], b[key]
        print(
            f"- {key[0]} p{key[1] + 1}：原版 {sa[2]}，修补版 {sb[2]}；"
            f"预期画面{'相同' if sa[0] == sb[0] else '不同'}，保存结果的画面{'相同' if sa[1] == sb[1] else '不同'}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="一轮实验的输出目录")
    parser.add_argument("--reasons", action="store_true", help="列出回退原因")
    parser.add_argument("--pages", type=int, default=0, help="列出回退和有差异的页（最多这么多条）")
    parser.add_argument("--hashes", action="store_true", help="比较两份 PDFium 下的画面指纹")
    args = parser.parse_args()

    for corpus in CORPORA:
        for build in BUILDS:
            rows = []
            for experiment in EXPERIMENTS:
                data = load(args.root / f"{corpus}-{build}-{experiment}.json")
                if data is not None:
                    rows.append((experiment, data))
            if not rows:
                continue
            print(f"\n### {corpus} / {build}\n")
            print(HEADER)
            for experiment, data in rows:
                print(summary_row(experiment, data["total"]))
            if args.reasons:
                for experiment, data in rows:
                    print_reasons(f"{corpus}-{build}-{experiment}", data)
            if args.pages:
                for experiment, data in rows:
                    print_pages(f"{corpus}-{build}-{experiment}", data, args.pages)
    if args.hashes:
        for corpus in CORPORA:
            for experiment in EXPERIMENTS:
                print_hash_comparison(args.root, corpus, experiment)
    return 0


if __name__ == "__main__":
    sys.exit(main())
