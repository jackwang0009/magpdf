#!/usr/bin/env python3
"""往返前后的结构校验（《方案》13.2 第 4 项、《测试计划》第 2 节）：原件和保存结果各查一遍，只统计。

每个文件做的事和 `qpdf --check` 差不多：打开（读交叉引用表和文件尾）、把每条流解一遍、把每页的内容流过一遍，
收集 qpdf 报的警告。用的是 pikepdf 的 `Pdf.check_pdf_syntax()`（pikepdf 的轮子里带着 qpdf），不是 `qpdf --check`
这个命令本身；JBIG2 的流这里查不到（轮子里没有 JBIG2 解码器），碰到时记一笔 `undecoded`。

一侧（原件或保存结果）的结果分四种：
  ok          打得开，没有查出问题
  problems    打得开，查出了问题（qpdf 能绕过去的损坏：交叉引用表要重建、流的长度不对、页树有毛病……）
  unreadable  打不开
  error       打得开，查到一半出错（内容流解不了之类）

一个文件按“原件 / 保存结果各自有没有问题”分成四类：都没问题、原件有问题保存结果没问题、都有问题、
原件没问题保存结果有问题。最后一类是往返把好文件写坏了，要逐个看。

输出里不放 qpdf 的原话（里面有路径、对象号、偏移，也可能带出文件里的名字），只把每条归到几类里计数。
要看原话加 --messages，那是给本机排查用的，不要放进流水线的产物。

用法：
  python tests/corpus/tools/struct_check.py --roundtrip tests/out/x.json --save-dir tests/out/x-saved [--json out.json]
  python tests/corpus/tools/struct_check.py original.pdf saved.pdf
  加 --qpdf <qpdf 可执行文件>：改用 `qpdf --check` 这个命令本身来查（每个文件起一个进程，带超时），拿来复核上面那一套查法。
  流水线上不用它（机器上没有装 qpdf）。

只作测试工具（pikepdf：MPL-2.0，见 tests/requirements-test.txt；qpdf：Apache-2.0），不进安装包。默认的查法没有隔离
也没有超时：一个文件把 qpdf 弄崩了，这一步就没做成，入口脚本照常出报告（和 ops_diff.py 一样）。
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import warnings
from collections import Counter
from pathlib import Path
from typing import Any, Iterable

import pikepdf

sys.path.insert(0, str(Path(__file__).resolve().parent))

from ops_diff import saved_name  # noqa: E402

RESULT_KIND = "magpdf-struct-check"
#: 结果的格式版本。状态、分类的含义变了就加一：不同版本数出来的数不能放在一起比。
VERSION = 1

STATUSES = ("ok", "problems", "unreadable", "error")
KINDS = ("xref", "stream", "object", "pages", "content", "password", "other")
CLASSES = ("ok_ok", "bad_ok", "bad_bad", "ok_bad")

#: 按顺序试，第一个对上的算数。内容流、页树、交叉引用表的话里也会出现 stream、object 这些词，所以它们排在前面。
_KIND_RULES: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("password", ("password",)),
    ("content", ("content stream",)),
    (
        "pages",
        ("kid ", "pages tree", "page tree", "/kids", "/pages", "/count", "/type key should be", "mediabox",
         "resources is missing", "catalog", "appears more than once", "array of streams"),
    ),
    (
        "xref",
        ("xref", "cross-reference", "cross reference", "damaged", "startxref", "trailer", "/root", "number of objects",
         "/size key", "linearization", "hint table", "not a pdf file", "pdf header"),
    ),
    (
        "stream",
        ("stream length", "/length", "endstream", "decoding", "decode", "filter", "inflate", "lzw", "ascii85", "asciihex",
         "runlength", "predictor", "jbig2", "dct", "stream data", "stream dictionary"),
    ),
    (
        "object",
        ("endobj", " obj", "unknown token", "dictionary key", "duplicated key", "treating as string", "indirect reference",
         "parse error", "unexpected", "invalid character", "hexstring", "expected", "premature eof", "too many errors",
         "name with", "object"),
    ),
)


def kind_of(message: str, path: Path | None = None) -> str:
    """把 qpdf 的一句话归到 KINDS 里的一类。

    qpdf 的话是“文件名 (object 5 0, offset 123): 说明”这样的，也有“文件名, object 5 0 at offset 123: 说明”
    和不带文件名的“page object 5 0: 说明”。只拿第一个“: ”后面的说明来归类：文件名里有 xref、pages 这些词，
    位置里又总有 object，带着它们会归错。
    """
    text = message.removeprefix("WARNING: ")
    if path is not None:
        for needle in (str(path), path.as_posix(), path.name):
            text = text.replace(needle, "")
    _location, sep, rest = text.partition(": ")
    text = (rest if sep else text).lower()
    for kind, needles in _KIND_RULES:
        if any(needle in text for needle in needles):
            return kind
    return "other"


def parse_qpdf_check(returncode: int, output: str) -> tuple[str, list[str]]:
    """把 `qpdf --check` 的退出码和输出变成一侧的状态和 qpdf 的话。

    退出码：0 没有问题；3 有警告、没有错误；2 有错误。有错误时，输出里已经有“PDF Version:”说明文件打开了、
    是查到后面出的错（error），没有就是打不开（unreadable）。别的退出码（崩溃）记成 error。
    """
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    found = [line for line in lines if line.startswith("WARNING: ")]
    errors = [
        line.split(": ", 1)[1]
        for line in lines
        if line.startswith(("qpdf: ", "ERROR: ")) and "operation succeeded with warnings" not in line
    ]
    if returncode == 0:
        return "ok", []
    if returncode == 3:
        return "problems", found or ["qpdf exit 3"]
    opened = any(line.startswith("PDF Version:") for line in lines)
    if returncode == 2:
        return ("error" if opened else "unreadable"), (errors + found) or ["qpdf exit 2"]
    return "error", [f"qpdf exit {returncode}", *errors, *found]


def check_file_with_qpdf(path: Path, qpdf: Path, timeout: float) -> tuple[str, list[str]]:
    """用 `qpdf --check` 查一个文件。超时记成 error。"""
    try:
        done = subprocess.run(  # noqa: S603  自己给的可执行文件和路径，不经过 shell
            [str(qpdf), "--check", str(path)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return "error", [f"qpdf timed out after {timeout:g} s"]
    return parse_qpdf_check(done.returncode, done.stdout.decode("utf-8", errors="replace"))


def check_file(path: Path, *, with_messages: bool = False, qpdf: Path | None = None, timeout: float = 120.0) -> dict[str, Any]:
    """查一个文件，返回一侧的结果：status、problems、kinds，查不到的流记 undecoded。

    给了 `qpdf` 就用 `qpdf --check` 这个命令查，否则用 pikepdf。
    """
    status = "ok"
    messages: list[str] = []
    undecoded = False
    if qpdf is not None:
        status, messages = check_file_with_qpdf(path, qpdf, timeout)
    else:
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            try:
                pdf = pikepdf.open(path)
            except pikepdf.PasswordError:
                status, messages = "unreadable", ["password required"]
            except (pikepdf.PdfError, pikepdf.PikepdfError, OSError) as e:
                status, messages = "unreadable", [str(e)]
            else:
                with pdf:
                    try:
                        messages = list(pdf.check_pdf_syntax())
                        status = "problems" if messages else "ok"
                    except Exception as e:  # noqa: BLE001  查到一半出什么错都记成 error，不让一个文件拖垮整批
                        status = "error"
                        messages = [str(e), *pdf.get_warnings()]
            undecoded = any("decoders" in str(w.message) for w in caught)
    kinds = Counter(kind_of(message, path) for message in messages)
    side: dict[str, Any] = {
        "status": status,
        "problems": len(messages),
        "kinds": {kind: kinds[kind] for kind in KINDS if kinds.get(kind)},
    }
    if undecoded:
        side["undecoded"] = True
    if with_messages:
        side["messages"] = messages
    return side


def class_of(original: dict[str, Any], saved: dict[str, Any]) -> str:
    return ("ok" if original["status"] == "ok" else "bad") + "_" + ("ok" if saved["status"] == "ok" else "bad")


def check_pair(original: Path, saved: Path, **options: Any) -> dict[str, Any]:
    """原件和保存结果各查一遍。`options` 原样交给 check_file（with_messages、qpdf、timeout）。"""
    return {"original": check_file(original, **options), "saved": check_file(saved, **options)}


def run_roundtrip(roundtrip: Path, save_dir: Path, **options: Any) -> dict[str, Any]:
    """对一次 `magpdf-cli roundtrip --json --save-dir` 的每个有保存结果的文件，原件和保存结果各查一遍。"""
    data = json.loads(roundtrip.read_text(encoding="utf-8-sig"))
    root = Path(data["options"]["root"])
    files: list[dict[str, Any]] = []
    for entry in data["files"]:
        rel = str(entry["path"])
        record: dict[str, Any] = {"path": rel, "source": entry["source"]}
        saved = save_dir / saved_name(str(entry["source"]), rel)
        if not saved.is_file():
            record["skipped"] = "no saved file"
        else:
            record.update(check_pair(root / rel, saved, **options))
        files.append(record)
    return {"roundtrip": str(roundtrip), "save_dir": str(save_dir), "files": files}


def aggregate(result: dict[str, Any]) -> dict[str, dict[str, int]]:
    by_source: dict[str, Counter[str]] = {}
    for record in result["files"]:
        counter = by_source.setdefault(str(record["source"]), Counter())
        counter["files"] += 1
        if "saved" not in record:
            counter["not_checked"] += 1
            continue
        counter["checked"] += 1
        counter[class_of(record["original"], record["saved"])] += 1
        if record["original"].get("undecoded") or record["saved"].get("undecoded"):
            counter["undecoded"] += 1
    return {k: dict(v) for k, v in sorted(by_source.items())}


def _row(source: str, c: Counter[str] | dict[str, int]) -> str:
    def g(key: str) -> int:
        return int(c.get(key, 0))

    return (
        f"| {source} | {g('files')} | {g('checked')} | {g('ok_ok')} | {g('bad_ok')} | {g('bad_bad')} | {g('ok_bad')} "
        f"| {g('not_checked')} | {g('undecoded')} |"
    )


def print_report(result: dict[str, Any], max_list: int) -> None:
    totals = aggregate(result)
    print(
        "| 来源 | 文件 | 查了的 | 原件和保存结果都没问题 | 原件有问题，保存结果没问题 | 都有问题 | 原件没问题，保存结果有问题 "
        "| 没查（没有保存结果） | 有解不了的流 |"
    )
    print("|---|---|---|---|---|---|---|---|---|")
    grand: Counter[str] = Counter()
    for source, c in totals.items():
        grand.update(c)
        print(_row(source, c))
    if len(totals) > 1:
        print(_row("（合计）", grand))
    broken = [r for r in result["files"] if "saved" in r and class_of(r["original"], r["saved"]) == "ok_bad"]
    if broken:
        print(f"\n原件没问题、保存结果有问题的文件（{len(broken)}）：")
        for record in broken[:max_list]:
            saved = record["saved"]
            kinds = "、".join(f"{kind} ×{count}" for kind, count in saved["kinds"].items())
            print(f"- {record['path']}：保存结果 {saved['status']}，{saved['problems']} 条（{kinds or '—'}）")
        if len(broken) > max_list:
            print(f"- …… 另有 {len(broken) - max_list} 个，见 --json")


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument("original", nargs="?", type=Path, help="原件（与 saved 成对使用）")
    parser.add_argument("saved", nargs="?", type=Path, help="往返保存结果")
    parser.add_argument("--roundtrip", type=Path, help="magpdf-cli roundtrip --json 的输出")
    parser.add_argument("--save-dir", type=Path, help="magpdf-cli roundtrip --save-dir 的目录")
    parser.add_argument("--json", type=Path, help="写出逐个文件的结果")
    parser.add_argument("--messages", action="store_true", help="结果里带上 qpdf 的原话（只在本机排查用，里面有路径和对象号）")
    parser.add_argument("--max-list", type=int, default=40, help="标准输出最多列出多少个被写坏的文件")
    parser.add_argument("--qpdf", type=Path, help="qpdf 可执行文件：改用 `qpdf --check` 这个命令来查（复核用，流水线上不用）")
    parser.add_argument("--timeout", type=float, default=120.0, help="用 --qpdf 时单个文件的超时（秒）")
    args = parser.parse_args(list(argv) if argv is not None else None)

    tool: dict[str, Any] = {"pikepdf": pikepdf.__version__, "libqpdf": pikepdf.__libqpdf_version__}
    if args.qpdf is not None:
        if not args.qpdf.is_file():
            parser.error(f"找不到 qpdf：{args.qpdf}")
        version = subprocess.run([str(args.qpdf), "--version"], capture_output=True, check=False)  # noqa: S603
        first = version.stdout.decode("utf-8", errors="replace").splitlines()[:1]
        tool = {"qpdf": first[0].strip() if first else ""}
    options: dict[str, Any] = {"with_messages": args.messages, "qpdf": args.qpdf, "timeout": args.timeout}

    started = time.perf_counter()
    if args.roundtrip is not None and args.save_dir is not None:
        result = run_roundtrip(args.roundtrip, args.save_dir, **options)
    elif args.original is not None and args.saved is not None:
        record: dict[str, Any] = {"path": args.original.name, "source": "-"}
        record.update(check_pair(args.original, args.saved, **options))
        result = {"files": [record]}
    else:
        parser.error("需要 --roundtrip 与 --save-dir，或 original saved 两个文件")
    result = {
        "kind": RESULT_KIND,
        "version": VERSION,
        "tool": tool,
        "seconds": round(time.perf_counter() - started, 2),
        **result,
        "totals": aggregate(result),
    }
    if args.json is not None:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(result, ensure_ascii=False, indent=1) + "\n", encoding="utf-8", newline="\n")
    print_report(result, args.max_list)
    return 0


if __name__ == "__main__":
    sys.exit(main())
