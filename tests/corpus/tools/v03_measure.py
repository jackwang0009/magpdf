"""V03 每修一项之后量一遍：往返、操作符比对、保真校验、保存结果的结构检查，并和基数比较有没有退步。

对自制样本（tests/corpus/synthetic）和 PDFium 公开测试集各跑一遍 `magpdf-cli roundtrip` 与 `ops_diff.py`，
对自制样本再跑 `magpdf-cli fidelity`，把结果汇总成一份 summary.json。给了 `--baseline` 时逐项对比：
- 公开测试集的无差异页数不能比基数少；
- 自制样本里基数通过的页不能变成不通过；
- 保存结果里 q/Q、BT/ET、标记内容不配对的页不能比基数多；
- 保真校验“能修改原有对象”的页数不能比基数少。
有退步时退出码为 1，逐条打印退步的页。

用法（在仓库根目录，先把 PDFIUM_PREBUILT_DIR 指到要测的 PDFium 构建并重新构建 magpdf-cli）：
  python -X dev tests/corpus/tools/v03_measure.py --label r3-item1 --out tests/out/v03r3/item1 \
      --baseline tests/out/v03r3/base/summary.json

只作测试工具（pikepdf：MPL-2.0，见 tests/requirements-test.txt），不进安装包。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

import pikepdf

import ops_diff

REPO = Path(__file__).resolve().parents[3]
DEFAULT_PUBLIC = Path(r"D:\tmp\v03-public\testing\resources")


def run(cmd: list[str], log: Path) -> None:
    """运行一条命令，输出写进 log；失败时把最后几行打出来再抛错。"""
    with log.open("w", encoding="utf-8") as fh:
        proc = subprocess.run(cmd, cwd=REPO, stdout=fh, stderr=subprocess.STDOUT, check=False)
    if proc.returncode != 0:
        tail = log.read_text(encoding="utf-8", errors="replace").splitlines()[-15:]
        raise RuntimeError(f"命令失败（{proc.returncode}）：{' '.join(cmd)}\n" + "\n".join(tail))


def pdfium_build_info() -> dict[str, str]:
    """PDFIUM_PREBUILT_DIR 里 BUILD-INFO.txt 的提交号，加上 pdfium.dll 的 SHA-256 前 16 位。"""
    info: dict[str, str] = {}
    prebuilt = os.environ.get("PDFIUM_PREBUILT_DIR", "")
    if not prebuilt:
        return info
    root = Path(prebuilt)
    build_info = root / "BUILD-INFO.txt"
    if build_info.is_file():
        for line in build_info.read_text(encoding="utf-8").splitlines():
            key, _, value = line.partition("=")
            if key in ("pdfium_commit", "pdfium_branch", "built_at", "version"):
                info[key] = value
    dll = root / "bin" / "pdfium.dll"
    if dll.is_file():
        info["dll_sha256_16"] = hashlib.sha256(dll.read_bytes()).hexdigest()[:16]
    return info


def balance_of(page: pikepdf.Page) -> dict[str, int]:
    """一页内容流里没有配对的 q/Q、BT/ET、BMC|BDC/EMC 各有多少。"""
    depth = 0
    extra_restore = 0
    in_text = False
    bad_text = 0
    marks = 0
    extra_emc = 0
    for item in pikepdf.parse_content_stream(page):
        if isinstance(item, pikepdf.ContentStreamInlineImage):
            continue
        op = str(item.operator)
        if op == "q":
            depth += 1
        elif op == "Q":
            if depth == 0:
                extra_restore += 1
            else:
                depth -= 1
        elif op == "BT":
            if in_text:
                bad_text += 1
            in_text = True
        elif op == "ET":
            if not in_text:
                bad_text += 1
            in_text = False
        elif op in ("BMC", "BDC"):
            marks += 1
        elif op == "EMC":
            if marks == 0:
                extra_emc += 1
            else:
                marks -= 1
    return {
        "q": depth + extra_restore,
        "BT": bad_text + (1 if in_text else 0),
        "marks": marks + extra_emc,
    }


def structure_check(roundtrip_json: Path, save_dir: Path) -> dict[str, Any]:
    """保存结果的结构检查：原件配对、保存后不配对的页，以及 pikepdf 打不开或解析不了的。"""
    data = json.loads(roundtrip_json.read_text(encoding="utf-8"))
    root = Path(data["options"]["root"])
    if not root.is_absolute():
        root = REPO / root
    unbalanced: list[str] = []
    errors: list[str] = []
    pages = 0
    for entry in data["files"]:
        rel = str(entry["path"])
        saved = save_dir / ops_diff.saved_name(str(entry["source"]), rel)
        if not saved.is_file():
            continue
        try:
            with pikepdf.open(root / rel) as original, pikepdf.open(saved) as result:
                for index in range(min(len(original.pages), len(result.pages))):
                    pages += 1
                    label = f"{rel} p{index + 1}"
                    try:
                        after = balance_of(result.pages[index])
                    except (pikepdf.PdfError, TypeError, ValueError) as e:
                        errors.append(f"{label}: 保存结果解析失败 {type(e).__name__}")
                        continue
                    if not any(after.values()):
                        continue
                    try:
                        before = balance_of(original.pages[index])
                    except (pikepdf.PdfError, TypeError, ValueError):
                        before = {"q": -1, "BT": -1, "marks": -1}
                    worse = [f"{k} {before[k]}→{after[k]}" for k in after if after[k] > max(before[k], 0)]
                    if worse:
                        unbalanced.append(f"{label}: {', '.join(worse)}")
        except pikepdf.PdfError as e:
            errors.append(f"{rel}: 打不开 {type(e).__name__}")
    return {"pages": pages, "unbalanced": sorted(unbalanced), "errors": sorted(errors)}


def summarize_roundtrip(roundtrip_json: Path) -> dict[str, Any]:
    data = json.loads(roundtrip_json.read_text(encoding="utf-8"))
    total = data["total"]
    passed: list[str] = []
    failed: dict[str, list[str]] = {}
    file_status: dict[str, str] = {}
    for entry in data["files"]:
        if entry.get("status") != "ok":
            file_status[str(entry["path"])] = str(entry.get("status"))
        for page in entry.get("pages") or []:
            label = f"{entry['path']} p{int(page['index']) + 1}"
            if page.get("status") == "pass":
                passed.append(label)
            elif page.get("status") == "diff":
                failed[label] = [str(h) for h in page.get("hints") or []]
    return {
        "files": total.get("files"),
        "pages_compared": total.get("pages_compared", len(passed) + len(failed)),
        "pass": len(passed),
        "diff": len(failed),
        "pass_pages": sorted(passed),
        "diff_pages": dict(sorted(failed.items())),
        "file_status": dict(sorted(file_status.items())),
        "total": total,
    }


def summarize_ops(ops_json: Path) -> dict[str, Any]:
    data = json.loads(ops_json.read_text(encoding="utf-8"))
    grand: dict[str, int] = {}
    for counts in data["totals"].values():
        for key, value in counts.items():
            grand[key] = grand.get(key, 0) + int(value)
    pages: dict[str, dict[str, Any]] = {}
    for record in data["files"]:
        for page in record.get("pages") or []:
            label = f"{record['path']} p{page['page']}"
            if "error" in page:
                pages[label] = {"error": page["error"]}
            elif any(page["counts"].values()):
                pages[label] = {"counts": page["counts"], "summary": page["summary"]}
    return {"totals": grand, "pages": dict(sorted(pages.items()))}


def summarize_fidelity(fidelity_json: Path) -> dict[str, Any]:
    data = json.loads(fidelity_json.read_text(encoding="utf-8"))
    editable: list[str] = []
    blocked: dict[str, str] = {}
    for entry in data["files"]:
        for page in entry.get("pages") or []:
            label = f"{entry['path']} p{int(page['index']) + 1}"
            if page.get("editable"):
                editable.append(label)
            else:
                blocked[label] = str(page.get("reason", ""))
    return {
        "pages": len(editable) + len(blocked),
        "editable": len(editable),
        "editable_pages": sorted(editable),
        "blocked_pages": dict(sorted(blocked.items())),
    }


def measure_set(name: str, source: Path, cli: Path, out: Path, jobs: int) -> dict[str, Any]:
    roundtrip_json = out / f"{name}.json"
    save_dir = out / f"{name}-saved"
    ops_json = out / f"{name}-ops.json"
    run(
        [
            str(cli),
            "roundtrip",
            str(source),
            "--isolate",
            "--jobs",
            str(jobs),
            "--quiet",
            "--json",
            str(roundtrip_json),
            "--save-dir",
            str(save_dir),
        ],
        out / f"{name}-roundtrip.log",
    )
    run(
        [
            sys.executable,
            "-X",
            "dev",
            str(Path(__file__).with_name("ops_diff.py")),
            "--roundtrip",
            str(roundtrip_json),
            "--save-dir",
            str(save_dir),
            "--json",
            str(ops_json),
            "--max-list",
            "400",
        ],
        out / f"{name}-ops.txt",
    )
    return {
        "roundtrip": summarize_roundtrip(roundtrip_json),
        "ops": summarize_ops(ops_json),
        "structure": structure_check(roundtrip_json, save_dir),
    }


def compare(base: dict[str, Any], cur: dict[str, Any]) -> list[str]:
    """和基数比较，返回退步清单（空表示没有退步）。"""
    problems: list[str] = []
    for name in ("synthetic", "public"):
        b, c = base[name], cur[name]
        if name == "public" and c["roundtrip"]["pass"] < b["roundtrip"]["pass"]:
            problems.append(f"{name}: 无差异页 {b['roundtrip']['pass']} → {c['roundtrip']['pass']}")
        now_pass = set(c["roundtrip"]["pass_pages"])
        for label in b["roundtrip"]["pass_pages"]:
            if label not in now_pass:
                hints = "/".join(c["roundtrip"]["diff_pages"].get(label, [])) or "不再比对"
                problems.append(f"{name}: {label} 原来无差异，现在不是（{hints}）")
        for label, status in c["roundtrip"]["file_status"].items():
            if b["roundtrip"]["file_status"].get(label) != status:
                problems.append(f"{name}: {label} 文件状态 {b['roundtrip']['file_status'].get(label, 'ok')} → {status}")
        for key, value in c["ops"]["totals"].items():
            if key in ("color", "text", "gs", "clip", "events") and value > b["ops"]["totals"].get(key, 0):
                problems.append(f"{name}: 操作符差异 {key} {b['ops']['totals'].get(key, 0)} → {value}")
        for label, page in c["ops"]["pages"].items():
            before = b["ops"]["pages"].get(label, {})
            if "error" in page and "error" not in before:
                problems.append(f"{name}: {label} 操作符比对出错 {page['error']}")
        base_unbalanced = set(b["structure"]["unbalanced"])
        for line in c["structure"]["unbalanced"]:
            if line not in base_unbalanced:
                problems.append(f"{name}: 保存结果不配对 {line}")
        base_errors = set(b["structure"]["errors"])
        for line in c["structure"]["errors"]:
            if line not in base_errors:
                problems.append(f"{name}: {line}")
    b_fid, c_fid = base["fidelity"], cur["fidelity"]
    now_editable = set(c_fid["editable_pages"])
    for label in b_fid["editable_pages"]:
        if label not in now_editable:
            problems.append(f"fidelity: {label} 原来能修改，现在不能（{c_fid['blocked_pages'].get(label, '')}）")
    return problems


def print_summary(summary: dict[str, Any], base: dict[str, Any] | None) -> None:
    def delta(cur: int, before: int | None) -> str:
        return f"{cur}" if before is None or before == cur else f"{cur}（基数 {before}）"

    print(f"标签 {summary['label']}  PDFium {summary['pdfium'].get('pdfium_commit', '?')[:10]}")
    print("| 样本 | 比对页 | 无差异页 | 颜色 | 文字状态 | ExtGState | 剪裁 | 事件缺失/多出 | 不配对的页 |")
    print("|---|---|---|---|---|---|---|---|---|")
    for name in ("synthetic", "public"):
        cur = summary[name]
        before = base[name] if base else None
        totals = cur["ops"]["totals"]
        cells = [
            name,
            str(cur["roundtrip"]["pages_compared"]),
            delta(cur["roundtrip"]["pass"], before["roundtrip"]["pass"] if before else None),
        ]
        for key in ("color", "text", "gs", "clip", "events"):
            cells.append(delta(int(totals.get(key, 0)), int(before["ops"]["totals"].get(key, 0)) if before else None))
        cells.append(
            delta(len(cur["structure"]["unbalanced"]), len(before["structure"]["unbalanced"]) if before else None)
        )
        print("| " + " | ".join(cells) + " |")
    fid = summary["fidelity"]
    print(
        "保真校验（自制样本）：能修改原有对象的 "
        + delta(fid["editable"], base["fidelity"]["editable"] if base else None)
        + f" / {fid['pages']} 页"
    )
    for name in ("synthetic", "public"):
        diff_pages = summary[name]["roundtrip"]["diff_pages"]
        if diff_pages:
            print(f"{name} 有像素差异的页（{len(diff_pages)}）：")
            for label, hints in diff_pages.items():
                ops = summary[name]["ops"]["pages"].get(label, {}).get("summary", "")
                print(f"  - {label} [{'/'.join(hints)}] {ops}")


def main() -> int:
    parser = argparse.ArgumentParser(description="V03 往返、操作符比对、保真校验一起量，并和基数比较")
    parser.add_argument("--label", required=True, help="这一次测量的名字，写进 summary.json")
    parser.add_argument("--out", required=True, help="输出目录（放在 tests/out/ 下，不入库）")
    parser.add_argument("--cli", default=str(REPO / "target" / "release" / "magpdf-cli.exe"))
    parser.add_argument("--synthetic", default=str(REPO / "tests" / "corpus" / "synthetic"))
    parser.add_argument("--public", default=str(DEFAULT_PUBLIC), help="PDFium 公开测试集目录")
    parser.add_argument("--baseline", help="基数的 summary.json；给了就比较有没有退步")
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    cli = Path(args.cli)
    if not cli.is_file():
        parser.error(f"找不到 {cli}，先 cargo build --release -p magpdf-cli")

    summary: dict[str, Any] = {"label": args.label, "pdfium": pdfium_build_info()}
    summary["synthetic"] = measure_set("syn", Path(args.synthetic), cli, out, args.jobs)
    summary["public"] = measure_set("pub", Path(args.public), cli, out, args.jobs)
    fidelity_json = out / "syn-fidelity.json"
    run(
        [str(cli), "fidelity", str(Path(args.synthetic)), "--json", str(fidelity_json), "--quiet"],
        out / "syn-fidelity.log",
    )
    summary["fidelity"] = summarize_fidelity(fidelity_json)

    base: dict[str, Any] | None = None
    problems: list[str] = []
    if args.baseline:
        base = json.loads(Path(args.baseline).read_text(encoding="utf-8"))
        problems = compare(base, summary)
        summary["baseline"] = {"label": base.get("label"), "regressions": problems}
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=1), encoding="utf-8")

    print_summary(summary, base)
    if problems:
        print(f"\n退步（{len(problems)}）：")
        for line in problems:
            print(f"  - {line}")
        return 1
    if base is not None:
        print("\n和基数相比没有退步。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
