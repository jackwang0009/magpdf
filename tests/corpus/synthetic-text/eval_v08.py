# V08「改一行字」评测：对每份合成样本分别做 tier1 / tier2 / tier3 三次独立改写。
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SAMPLES = Path(__file__).resolve().parent / "samples"
LARGE = Path(os.environ.get("MAGPDF_V08_LARGE_DIR", ROOT / "out" / "v08-samples-large"))
OUT = ROOT / "out" / "v08-eval"
CLI_CANDIDATES = [
    ROOT / "target" / "release" / "magpdf-cli.exe",
    ROOT / "target" / "debug" / "magpdf-cli.exe",
]
PLUS10 = "饕餮貔貅麒麟鸳鸯琥珀"  # 10 个原子集里没有的汉字


def find_cli() -> Path:
    for p in CLI_CANDIDATES:
        if p.exists():
            return p
    raise SystemExit("找不到 magpdf-cli.exe")


def lang_of(name: str) -> str:
    return "en" if name.startswith("en-") else "zh"


def original_and_tiers(name: str, line: str) -> dict[str, str]:
    lang = lang_of(name)
    if lang == "zh":
        t1 = (line[2:] + line[:2]) if len(line) >= 2 else line
        t2 = "饕餮貔貅" + (line[4:] if len(line) > 4 else "")
        t3 = "한글테스트행"
        return {"orig": line, "tier1": t1, "tier2": t2, "tier3": t3}
    t1 = (line[3:] + line[:3]) if len(line) >= 3 else line
    t2 = "Invoice No: xyz-987" if line.startswith("Invoice") else "xyzXYZ QKM"
    t3 = "货物验收编号"
    return {"orig": line, "tier1": t1, "tier2": t2, "tier3": t3}


def run_edit(cli: Path, src: Path, dest: Path, page: int, line: int, text: str) -> dict:
    dest.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.setdefault("PDFIUM_PREBUILT_DIR", r"D:\pdfium-src\pdfium\out\Release\dist")
    env.setdefault("PYTHONIOENCODING", "utf-8")
    cmd = [
        str(cli),
        "edit-line",
        str(src),
        "--page",
        str(page),
        "--line",
        str(line),
        "--text",
        text,
        "-o",
        str(dest),
        "--json",
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", env=env)
    rec = {
        "ok": proc.returncode == 0,
        "returncode": proc.returncode,
        "stderr": (proc.stderr or "").strip()[-800:],
        "stdout": (proc.stdout or "").strip()[-800:],
    }
    if proc.returncode == 0:
        try:
            rec.update(json.loads(proc.stdout))
        except json.JSONDecodeError as exc:
            rec["ok"] = False
            rec["stderr"] = f"JSON parse error: {exc}; stdout={proc.stdout[:400]}"
    else:
        rec["contains_new_text"] = False
        rec["tier"] = None
        rec["font_replaced"] = None
        rec["bytes_delta"] = None
    rec["bytes_before"] = rec.get("bytes_before")
    if rec.get("bytes_before") is None and src.exists():
        rec["bytes_before"] = src.stat().st_size
    if rec.get("bytes_after") is None and dest.exists():
        rec["bytes_after"] = dest.stat().st_size
    if rec.get("bytes_delta") is None and rec.get("bytes_before") is not None and rec.get("bytes_after") is not None:
        rec["bytes_delta"] = rec["bytes_after"] - rec["bytes_before"]
    return rec


def collect_pdfs() -> list[Path]:
    # 完整嵌入的大文件（数 MB）在 debug CLI 下单次改写可达数分钟甚至卡住，评测只跑 samples/。
    files = []
    if SAMPLES.exists():
        files.extend(sorted(SAMPLES.glob("*.pdf")))
    return files


def skipped_large() -> list[dict]:
    rows = []
    if not LARGE.exists():
        return rows
    for src in sorted(LARGE.glob("*.pdf")):
        rows.append({
            "sample": src.name,
            "src": str(src),
            "lang": lang_of(src.name),
            "skipped_reason": "timeout: full-embed PDF in out/v08-samples-large (debug CLI too slow / hung)",
            "bytes": src.stat().st_size,
            "success": False,
        })
    return rows


def main() -> int:
    cli = find_cli()
    OUT.mkdir(parents=True, exist_ok=True)
    rows = []
    pdfs = collect_pdfs()
    print(f"CLI={cli}")
    print(f"samples={len(pdfs)}")
    for src in pdfs:
        # 每份改第一行（布局行号 0）三次，均从原文件出发
        # 原文按文件名语言取 generate.py 的默认行；英文/中文样本的第一行固定。
        lang = lang_of(src.name)
        line0 = "Invoice No: ABC-123" if lang == "en" else "合同甲方乙方"
        planned = original_and_tiers(src.name, line0)
        for expected in ("tier1", "tier2", "tier3"):
            dest = OUT / "edited" / f"{src.stem}__{expected}.pdf"
            rec = run_edit(cli, src, dest, page=1, line=0, text=planned[expected])
            success = bool(rec.get("ok") and rec.get("contains_new_text"))
            row = {
                "sample": src.name,
                "src": str(src),
                "lang": lang,
                "expected_tier": expected,
                "new_text": planned[expected],
                "actual_tier": rec.get("tier"),
                "contains_new_text": rec.get("contains_new_text"),
                "font_replaced": rec.get("font_replaced"),
                "skipped": rec.get("skipped"),
                "bytes_before": rec.get("bytes_before"),
                "bytes_after": rec.get("bytes_after"),
                "bytes_delta": rec.get("bytes_delta"),
                "extracted": rec.get("extracted"),
                "ok": rec.get("ok"),
                "success": success,
                "tier_match": success and rec.get("tier") == expected,
                "error": None if rec.get("ok") else rec.get("stderr"),
                "output": str(dest),
            }
            rows.append(row)
            flag = "OK" if success else "FAIL"
            print(f"{flag} {src.name} {expected} -> {rec.get('tier')} contains={rec.get('contains_new_text')} delta={rec.get('bytes_delta')} font_replaced={rec.get('font_replaced')}")
            if not rec.get("ok"):
                err = (rec.get("stderr") or "")[:200]
                if err:
                    print("   ", err)

    # +10 汉字体积：优先用仿宋子集样本
    size_tests = []
    fang = SAMPLES / "zh-fang-tier1.pdf"
    size_src = fang if fang.exists() else (pdfs[0] if pdfs else None)
    if size_src is not None:
        lang = lang_of(size_src.name)
        base = "Invoice No: ABC-123" if lang == "en" else "合同甲方乙方"
        cases = [
            ("tier1_reorder", (base[2:] + base[:2]) if lang == "zh" else (base[3:] + base[:3]), "tier1"),
            ("tier2_plus10", (base + PLUS10) if lang == "zh" else (base + " xyzQKM@#"), "tier2"),
        ]
        if lang == "zh":
            cases.append(("tier3_hangul", "한글테스트행추가", "tier3"))
        for name, text, expected in cases:
            dest = OUT / "edited" / f"{size_src.stem}__{name}.pdf"
            rec = run_edit(cli, size_src, dest, 1, 0, text)
            added = [ch for ch in text if ch not in base]
            size_tests.append({
                "name": name,
                "sample": size_src.name,
                "expected_tier": expected,
                "actual_tier": rec.get("tier"),
                "new_text": text,
                "added_chars": "".join(dict.fromkeys(added)),
                "added_count": len("".join(dict.fromkeys(added))),
                "contains_new_text": rec.get("contains_new_text"),
                "font_replaced": rec.get("font_replaced"),
                "bytes_before": rec.get("bytes_before"),
                "bytes_after": rec.get("bytes_after"),
                "bytes_delta": rec.get("bytes_delta"),
                "limit_50kb": 50 * 1024,
                "within_50kb": rec.get("bytes_delta") is not None and rec["bytes_delta"] <= 50 * 1024,
                "success": bool(rec.get("ok") and rec.get("contains_new_text")),
                "error": None if rec.get("ok") else rec.get("stderr"),
            })
            print(f"SIZE {name} tier={rec.get('tier')} delta={rec.get('bytes_delta')} within_50kb={size_tests[-1]['within_50kb']}")

    summary = {
        "cli": str(cli),
        "samples": len(pdfs),
        "edits": len(rows),
        "successes": sum(1 for r in rows if r["success"]),
        "tier_success_by_actual": {
            t: sum(1 for r in rows if r["success"] and r["actual_tier"] == t)
            for t in ("tier1", "tier2", "tier3")
        },
        "tier_match_by_expected": {
            t: sum(1 for r in rows if r["tier_match"] and r["expected_tier"] == t)
            for t in ("tier1", "tier2", "tier3")
        },
        "failures": [
            {"sample": r["sample"], "expected_tier": r["expected_tier"], "actual_tier": r["actual_tier"], "error": r["error"], "extracted": r.get("extracted")}
            for r in rows if not r["success"]
        ],
    }
    skipped = skipped_large()
    summary["skipped_large"] = skipped
    summary["skipped_large_count"] = len(skipped)
    payload = {"summary": summary, "rows": rows, "size_tests": size_tests, "skipped_large": skipped}
    out_json = OUT / "results.json"
    out_json.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    print("wrote", out_json)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
