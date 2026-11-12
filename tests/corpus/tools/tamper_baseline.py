#!/usr/bin/env python3
"""故意改一份往返测试的基线，用来验证退步检测（V14）。只在验证时用，改出来的文件不要提交。

做法是改基线而不是改代码：把基线里几页“有差异”改成“无差异”，这次的结果和它一比，这几页就成了“新变坏”；
反过来改，就成了“新变好”。改了哪些页打印出来，方便和报告页面上列出的对一遍。

用法：
    python tests/corpus/tools/tamper_baseline.py tests/baselines/roundtrip/synthetic.json out/synthetic.json \
        --regress 3 [--improve 2] [--uncrash 1] [--ops-regress 2] [--drop-files 1] [--add-file ghost.pdf] [--source NAME]

    --regress N      基线里前 N 页有差异的页改成无差异（这次看起来新变坏了 N 页）
    --improve N      基线里前 N 页有对象的无差异页改成有差异（这次看起来新变好了 N 页）
    --uncrash N      基线里前 N 个崩溃或超时的文件改成正常、带一页无差异的页（这次看起来新崩溃了 N 个）
    --ops-regress N  基线里前 N 页“像素无差异但操作符有差异”的页改成操作符也没有差异（这次看起来操作符级新变坏 N 页）
    --drop-files N   从基线里删掉前 N 个文件（这次看起来多了 N 个新文件）
    --add-file PATH  往基线里加一个不存在的文件（这次看起来少跑了一个文件），可以重复
    --source NAME    只动这个来源里的页和文件

只用标准库。
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Iterable

sys.path.insert(0, str(Path(__file__).resolve().parent))

from roundtrip_model import FileResult, Page, dumps_baseline, loads_baseline  # noqa: E402


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="故意改一份往返测试的基线，用来验证退步检测")
    parser.add_argument("baseline", type=Path, help="原来的基线")
    parser.add_argument("output", type=Path, help="改过的基线写到哪里")
    parser.add_argument("--regress", type=int, default=0)
    parser.add_argument("--improve", type=int, default=0)
    parser.add_argument("--uncrash", type=int, default=0)
    parser.add_argument("--ops-regress", type=int, default=0)
    parser.add_argument("--drop-files", type=int, default=0)
    parser.add_argument("--add-file", action="append", default=[])
    parser.add_argument("--source")
    args = parser.parse_args(list(argv) if argv is not None else None)

    snapshot = loads_baseline(args.baseline.read_text(encoding="utf-8-sig"))
    files = [f for f in snapshot.files if args.source is None or f.source == args.source]
    if not files:
        print("没有符合条件的文件", file=sys.stderr)
        return 1
    done: list[str] = []

    left = args.drop_files
    for file in list(files):
        if left <= 0:
            break
        snapshot.files.remove(file)
        files.remove(file)
        done.append(f"删掉文件 {file.source} / {file.path}")
        left -= 1

    left = args.uncrash
    for file in files:
        if left <= 0:
            break
        if file.status in ("crashed", "timed_out"):
            done.append(f"{file.source} / {file.path}：{file.status} 改成 ok，加一页无差异")
            file.status = "ok"
            file.pages = [Page(number=1, pixel="pass", objects=1, ops={} if snapshot.has_ops else None)]
            left -= 1

    counters = {"regress": args.regress, "improve": args.improve, "ops": args.ops_regress}
    for file in files:
        for page in file.pages:
            if counters["regress"] > 0 and page.pixel == "diff":
                done.append(f"{file.source} / {file.path} 第 {page.number} 页：有差异 改成 无差异")
                page.pixel, page.diff_pixels, page.largest_region = "pass", 0, 0
                if page.ops is not None:
                    page.ops = {}
                counters["regress"] -= 1
            elif counters["ops"] > 0 and page.pixel == "pass" and page.ops:
                done.append(f"{file.source} / {file.path} 第 {page.number} 页：操作符有差异 改成 没有差异")
                page.ops = {}
                counters["ops"] -= 1
            elif counters["improve"] > 0 and page.pixel == "pass" and page.objects > 0:
                done.append(f"{file.source} / {file.path} 第 {page.number} 页：无差异 改成 有差异")
                page.pixel, page.diff_pixels, page.largest_region = "diff", 1234, 321
                counters["improve"] -= 1

    source = args.source or files[0].source
    for path in args.add_file:
        snapshot.files.append(FileResult(path=path, source=source, status="ok", pages=[Page(number=1, pixel="pass", objects=1)]))
        done.append(f"加上不存在的文件 {source} / {path}")

    meta = dict(snapshot.meta)
    meta["tampered"] = "tamper_baseline.py 故意改过，只用来验证退步检测，不要提交"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(dumps_baseline(snapshot, meta), encoding="utf-8", newline="\n")
    for line in done:
        print(line)
    short = {name: left for name, left in counters.items() if left > 0}
    if short:
        print(f"没改够：{short}", file=sys.stderr)
    print(f"改了 {len(done)} 处，写到 {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
