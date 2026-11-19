#!/usr/bin/env python3
"""保真校验的结论和基线比（方案 2.3；每晚的测试第 5 步）。

`magpdf-cli fidelity --isolate --json` 给出每一页能不能修改原有对象、不能的原因。这里拿它和
`tests/baselines/fidelity/<语料集>.json` 比：

| 情况 | 结果 | 退出码 |
|---|---|---|
| 原来能改的页现在只能叠加内容；文件从正常变成出错、崩溃、超时；基线里的文件或页这次没有 | 有退步 | 3 |
| 原来只能叠加的页现在能改；不能改的原因变了；文件出错的说法变了；新文件、新页 | 只提示 | 0 |
| PDFium、机器和基线的不一样 | 只提示 | 0 |
| 没有基线 | 不判定 | 0（加 `--require-baseline` 时是 1） |

`--fail-on alarm` / `never` 时 3 改成 0（和往返测试一样，保真校验没有“报警线”这一档）。

用法：
  python tests/corpus/tools/fidelity_report.py --set synthetic --result out/fidelity.json \
      --baseline tests/baselines/fidelity/synthetic.json --candidate out/baseline-candidate/fidelity-synthetic.json \
      --markdown out/fidelity-synthetic.md [--meta-json meta.json] [--update-baseline]

只用标准库。基线和摘要里只有相对路径、页码和原因（引擎给的固定说法），没有文档内容。
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_WORSE = 3
META_COMPARED = ("pdfium_version", "pdfium_dll_sha256", "machine")


def page_verdicts(file_entry: dict[str, Any]) -> dict[int, str | None]:
    """页码（从 1 开始）→ 不能修改的原因，能改是 `None`。"""
    out: dict[int, str | None] = {}
    for p in file_entry.get("pages") or []:
        number = int(p["page"]) if "page" in p else int(p["index"]) + 1
        out[number] = None if p.get("editable") else str(p.get("reason") or "")
    return out


def to_baseline(result: dict[str, Any], set_name: str, meta: dict[str, Any]) -> dict[str, Any]:
    """命令行工具的 JSON 换成基线的写法：按路径排序，页码从 1 开始。"""
    files = []
    for f in sorted(result.get("files") or [], key=lambda f: f["path"]):
        entry: dict[str, Any] = {"path": f["path"], "status": f.get("status", "ok")}
        if f.get("error"):
            entry["error"] = f["error"]
        entry["pages"] = [
            {"page": n, "editable": True} if r is None else {"page": n, "editable": False, "reason": r}
            for n, r in sorted(page_verdicts(f).items())
        ]
        files.append(entry)
    full_meta = {"pdfium_version": result.get("pdfium_version", ""), **meta}
    return {"kind": "magpdf-fidelity-baseline", "format": 1, "set": set_name, "meta": full_meta, "files": files}


def dump_baseline(baseline: dict[str, Any]) -> str:
    """一个文件一行开头、一页一行，`git diff` 直接看得出哪一页变了。"""
    head = {k: v for k, v in baseline.items() if k != "files"}
    lines = [json.dumps(head, ensure_ascii=False, indent=1, sort_keys=True)[:-2] + ',\n "files": [']
    entries = []
    for f in baseline["files"]:
        meta = {k: v for k, v in f.items() if k != "pages"}
        body = json.dumps(meta, ensure_ascii=False)[:-1] + ', "pages": ['
        pages = ",\n".join("   " + json.dumps(p, ensure_ascii=False) for p in f["pages"])
        entries.append("  " + body + ("\n" + pages + "\n  ]}" if pages else "]}"))
    lines.append(",\n".join(entries))
    lines.append(" ]\n}\n")
    return "\n".join(lines)


@dataclass
class Comparison:
    newly_overlay: list[tuple[str, int, str]] = field(default_factory=list)
    newly_editable: list[tuple[str, int, str]] = field(default_factory=list)
    reason_changed: list[tuple[str, int, str, str]] = field(default_factory=list)
    file_worse: list[tuple[str, str, str]] = field(default_factory=list)
    file_better: list[tuple[str, str, str]] = field(default_factory=list)
    removed_files: list[str] = field(default_factory=list)
    added_files: list[str] = field(default_factory=list)
    removed_pages: list[tuple[str, int]] = field(default_factory=list)
    added_pages: list[tuple[str, int]] = field(default_factory=list)
    meta_changes: list[tuple[str, Any, Any]] = field(default_factory=list)

    @property
    def worse(self) -> bool:
        return bool(self.newly_overlay or self.file_worse or self.removed_files or self.removed_pages)


def compare(baseline: dict[str, Any], current: dict[str, Any]) -> Comparison:
    cmp = Comparison()
    old = {f["path"]: f for f in baseline.get("files") or []}
    new = {f["path"]: f for f in current.get("files") or []}
    cmp.removed_files = sorted(set(old) - set(new))
    cmp.added_files = sorted(set(new) - set(old))
    for path in sorted(set(old) & set(new)):
        a, b = old[path], new[path]
        sa, sb = a.get("status", "ok"), b.get("status", "ok")
        if sa == "ok" and sb != "ok":
            cmp.file_worse.append((path, sa, sb))
        elif sa != "ok" and sb == "ok":
            cmp.file_better.append((path, sa, sb))
        pa, pb = page_verdicts(a), page_verdicts(b)
        for n in sorted(pa):
            if n not in pb:
                # 文件出错的时候后面的页本来就没有，已经算在 file_worse 里了
                if sb == "ok":
                    cmp.removed_pages.append((path, n))
                continue
            ra, rb = pa[n], pb[n]
            if ra is None and rb is not None:
                cmp.newly_overlay.append((path, n, rb))
            elif ra is not None and rb is None:
                cmp.newly_editable.append((path, n, ra))
            elif ra is not None and rb is not None and ra != rb:
                cmp.reason_changed.append((path, n, ra, rb))
        cmp.added_pages.extend((path, n) for n in sorted(set(pb) - set(pa)))
    for key in META_COMPARED:
        va, vb = baseline.get("meta", {}).get(key), current.get("meta", {}).get(key)
        if va != vb:
            cmp.meta_changes.append((key, va, vb))
    return cmp


def counts(baseline: dict[str, Any]) -> tuple[int, int]:
    pages = [r for f in baseline["files"] for r in page_verdicts(f).values()]
    return len(pages), sum(1 for r in pages if r is None)


def markdown(set_name: str, current: dict[str, Any], cmp: Comparison | None, max_list: int = 20) -> str:
    total, editable = counts(current)
    lines = [f"### 保真校验：{set_name}", ""]
    ratio = f"{editable * 100 / total:.1f}%" if total else "—"
    bad = sum(1 for f in current["files"] if f.get("status", "ok") != "ok")
    lines.append(f"{len(current['files'])} 个文件、{total} 页；能修改原有对象的 {editable} 页（{ratio}）；出错、崩溃或超时的文件 {bad} 个。")
    lines.append("")
    if cmp is None:
        lines.append("没有基线，不判定。")
        return "\n".join(lines) + "\n"
    if not cmp.worse:
        lines.append("和基线比：**没有退步**。")
    else:
        lines.append("和基线比：**有退步**。")
    lines.append("")

    def section(title: str, rows: list[str]) -> None:
        if not rows:
            return
        lines.append(f"- {title}（{len(rows)}）")
        for row in rows[:max_list]:
            lines.append(f"  - {row}")
        if len(rows) > max_list:
            lines.append(f"  - ……另有 {len(rows) - max_list} 条")

    section("原来能改、现在只能叠加内容的页", [f"`{p}` 第 {n} 页：{r}" for p, n, r in cmp.newly_overlay])
    section("变差的文件", [f"`{p}`：{a} → {b}" for p, a, b in cmp.file_worse])
    section("基线里有、这次没有的文件", [f"`{p}`" for p in cmp.removed_files])
    section("基线里有、这次没有的页", [f"`{p}` 第 {n} 页" for p, n in cmp.removed_pages])
    section("原来只能叠加、现在能改的页", [f"`{p}` 第 {n} 页（原来：{r}）" for p, n, r in cmp.newly_editable])
    section("不能改的原因变了", [f"`{p}` 第 {n} 页：{a} → {b}" for p, n, a, b in cmp.reason_changed])
    section("变好的文件", [f"`{p}`：{a} → {b}" for p, a, b in cmp.file_better])
    section("新文件", [f"`{p}`" for p in cmp.added_files])
    section("新页", [f"`{p}` 第 {n} 页" for p, n in cmp.added_pages])
    section("和基线不是同一种环境", [f"{k}：{a} → {b}" for k, a, b in cmp.meta_changes])
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--set", required=True, help="语料集的名字")
    ap.add_argument("--result", required=True, type=Path, help="magpdf-cli fidelity --json 的输出")
    ap.add_argument("--baseline", required=True, type=Path)
    ap.add_argument("--candidate", type=Path, help="把这一次的结果按基线的写法写到这里")
    ap.add_argument("--markdown", type=Path, help="Markdown 摘要")
    ap.add_argument("--meta-json", type=Path, help="写进基线 meta 的附加字段（JSON 对象）")
    ap.add_argument("--fail-on", choices=("any", "alarm", "never"), default="any")
    ap.add_argument("--require-baseline", action="store_true")
    ap.add_argument("--update-baseline", action="store_true", help="比完之后用这一次的结果覆盖基线，退出码 0")
    args = ap.parse_args(argv)

    try:
        result = json.loads(args.result.read_text(encoding="utf-8"))
        meta = json.loads(args.meta_json.read_text(encoding="utf-8-sig")) if args.meta_json else {}
    except (OSError, ValueError) as e:
        print(f"读不了输入：{e}", file=sys.stderr)
        return EXIT_ERROR
    current = to_baseline(result, args.set, meta)
    baseline = None
    if args.baseline.is_file():
        baseline = json.loads(args.baseline.read_text(encoding="utf-8"))
    cmp = compare(baseline, current) if baseline else None
    text = markdown(args.set, current, cmp)
    print(text, end="")
    if args.markdown:
        args.markdown.parent.mkdir(parents=True, exist_ok=True)
        args.markdown.write_text(text, encoding="utf-8")
    if args.candidate:
        args.candidate.parent.mkdir(parents=True, exist_ok=True)
        args.candidate.write_text(dump_baseline(current), encoding="utf-8")
    if args.update_baseline:
        args.baseline.parent.mkdir(parents=True, exist_ok=True)
        args.baseline.write_text(dump_baseline(current), encoding="utf-8")
        print(f"基线已更新：{args.baseline}")
        return EXIT_OK
    if cmp is None:
        return EXIT_ERROR if args.require_baseline else EXIT_OK
    if cmp.worse and args.fail_on == "any":
        return EXIT_WORSE
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
