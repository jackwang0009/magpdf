#!/usr/bin/env python3
"""核对识别字典：本地字典文件、ONNX 文件里带的字典、官方 `inference.yml` 里的 `character_dict` 三者逐项比较（V09 报告 2.1）。

官方的 `inference.yml` 从 PaddlePaddle 在 ModelScope 和 hf-mirror 上的模型仓库下载，存在 --cache 目录里，
下载过的不再下载。两个来源各自和本地字典文件比；哪个来源拿不到就照实写“没有拿到”，不算通过。

顺带把官方 `inference.yml` 里的预处理、后处理参数打出来，供和代码里的默认值对照（V09 报告 2.2）。

用法：
  python check_dicts.py [--models-dir models] [--cache out/official] [--offline] [--json 结果.json]
依赖 PyYAML（见同目录 requirements.txt）。
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

import yaml

SOURCES = {
    "modelscope": "https://www.modelscope.cn/models/PaddlePaddle/{model}/resolve/master/inference.yml",
    "hf-mirror": "https://hf-mirror.com/PaddlePaddle/{model}/resolve/main/inference.yml",
}
# 清单里的 id 和官方仓库名不一样的。
OFFICIAL_NAME: dict[str, str] = {}
# 清单之外也看一眼参数的模型（方案 6.2 第 1 步的文档方向模型，还没有 ONNX）。
EXTRA_MODELS = ["PP-LCNet_x1_0_doc_ori"]


def fetch(model: str, source: str, cache: Path, offline: bool) -> Path | None:
    """取官方 inference.yml，返回本地路径；拿不到返回 None。"""
    path = cache / f"{model}.{source}.inference.yml"
    if path.exists() and path.stat().st_size > 0:
        return path
    if offline:
        return None
    url = SOURCES[source].format(model=model)
    request = urllib.request.Request(url, headers={"User-Agent": "magpdf-check-dicts"})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))  # 不走系统代理
    try:
        with opener.open(request, timeout=60) as response:
            data = response.read()
    except (urllib.error.URLError, TimeoutError, OSError) as err:
        print(f"  {source}：没有拿到 {model} 的 inference.yml（{err}）")
        return None
    if not data.lstrip().startswith(b"Global:"):
        print(f"  {source}：{model} 返回的不是 inference.yml（{len(data)} 字节）")
        return None
    path.write_bytes(data)
    return path


def read_dict_file(path: Path) -> list[str]:
    """和 magpdf-scan 的 `parse_dict` 一样：按 \\n 分行，只去掉行尾的 \\r，最后一个空行不算。"""
    lines = path.read_text(encoding="utf-8").split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line.removesuffix("\r") for line in lines]


def read_varint(buf: bytes, pos: int) -> tuple[int, int]:
    value = shift = 0
    while True:
        byte = buf[pos]
        pos += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, pos
        shift += 7


def fields(buf: bytes) -> list[tuple[int, bytes]]:
    """一条 protobuf 消息里所有按长度存的字段：(字段号, 内容)。别的类型跳过。"""
    out: list[tuple[int, bytes]] = []
    pos = 0
    while pos < len(buf):
        tag, pos = read_varint(buf, pos)
        number, wire = tag >> 3, tag & 7
        if wire == 0:
            _, pos = read_varint(buf, pos)
        elif wire == 1:
            pos += 8
        elif wire == 5:
            pos += 4
        elif wire == 2:
            length, pos = read_varint(buf, pos)
            out.append((number, buf[pos : pos + length]))
            pos += length
        else:
            raise ValueError(f"不认识的 protobuf 字段类型 {wire}")
    return out


def onnx_metadata(path: Path) -> dict[str, str]:
    """读 ONNX 文件的 metadata_props（ModelProto 第 14 个字段，键值对），不依赖 onnx 包。"""
    meta: dict[str, str] = {}
    for number, body in fields(path.read_bytes()):
        if number != 14:
            continue
        entry = dict(fields(body))
        meta[entry.get(1, b"").decode("utf-8")] = entry.get(2, b"").decode("utf-8")
    return meta


def compare(a: list[str], b: list[str]) -> dict[str, object]:
    first = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), None)
    if first is None and len(a) != len(b):
        first = min(len(a), len(b))
    return {
        "same": a == b,
        "local_len": len(a),
        "other_len": len(b),
        "first_difference": first,
        "different_items": sum(1 for x, y in zip(a, b) if x != y) + abs(len(a) - len(b)),
    }


def describe(entry: dict[str, object]) -> str:
    if entry["same"]:
        return f"逐项一致（{entry['local_len']} 项）"
    return (f"不一致：本地 {entry['local_len']} 项，对方 {entry['other_len']} 项，"
            f"不同的 {entry['different_items']} 项，第一处在第 {entry['first_difference']} 项")


def op(config: dict[str, object], name: str) -> object:
    """PreProcess.transform_ops 里某一步的参数。"""
    pre = config.get("PreProcess") or {}
    for step in pre.get("transform_ops", []):  # type: ignore[union-attr]
        if isinstance(step, dict) and name in step:
            return step[name]
    return None


def params_of(config: dict[str, object]) -> dict[str, object]:
    """报告 2.2 要对照的参数。字典本身不放进来。"""
    post = dict(config.get("PostProcess") or {})  # type: ignore[call-overload]
    chars = post.pop("character_dict", None)
    if chars is not None:
        post["character_dict_len"] = len(chars)
    out: dict[str, object] = {"PostProcess": post}
    for name in ("DecodeImage", "DetResizeForTest", "RecResizeImg", "ResizeImage", "CropImage", "NormalizeImage"):
        value = op(config, name)
        if value is not None:
            out[name] = value
    return out


def main() -> int:
    here = Path(__file__).parent
    ap = argparse.ArgumentParser(description="核对识别字典和官方 inference.yml")
    ap.add_argument("--models-dir", type=Path,
                    default=Path(os.environ.get("MAGPDF_MODELS_DIR", here.parents[2] / "models")))
    ap.add_argument("--cache", type=Path, default=here / "out" / "official", help="官方 inference.yml 存在哪里")
    ap.add_argument("--offline", action="store_true", help="只用 --cache 里已有的文件，不下载")
    ap.add_argument("--json", type=Path, help="把结果写成 JSON")
    args = ap.parse_args()
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    args.cache.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((args.models_dir / "manifest.json").read_text(encoding="utf-8"))

    report: dict[str, object] = {"models_dir": str(args.models_dir), "dicts": [], "params": {}}
    failed = False
    for entry in manifest["models"]:
        if entry["kind"] != "rec" or "dict" not in entry:
            continue
        model = OFFICIAL_NAME.get(entry["id"], entry["id"])
        dict_path = args.models_dir / entry["dict"]
        local = read_dict_file(dict_path)
        row: dict[str, object] = {
            "model": entry["id"],
            "dict_file": entry["dict"],
            "dict_sha256": hashlib.sha256(dict_path.read_bytes()).hexdigest(),
            "items": len(local),
            "duplicates": len(local) - len(set(local)),
            "official": {},
        }
        print(f"{entry['id']}：字典 {entry['dict']}，{len(local)} 项，重复 {row['duplicates']} 项")
        onnx_path = args.models_dir / entry["file"]
        if onnx_path.exists():
            character = onnx_metadata(onnx_path).get("character")
            if character is None:
                row["onnx_metadata"] = None
                print("  ONNX 文件里没有 character 这一项")
            else:
                embedded = character.split("\n")
                if embedded and embedded[-1] == "":
                    embedded.pop()
                result = compare(local, embedded)
                row["onnx_metadata"] = result
                failed |= not result["same"]
                print(f"  和 ONNX 文件里的 character：{describe(result)}")
        else:
            row["onnx_metadata"] = "没有这个模型文件"
            print(f"  没有模型文件 {entry['file']}，ONNX 里的字典没有比")
        for source in SOURCES:
            path = fetch(model, source, args.cache, args.offline)
            if path is None:
                row["official"][source] = None  # type: ignore[index]
                continue
            config = yaml.safe_load(path.read_text(encoding="utf-8"))
            official = [str(c) for c in config["PostProcess"]["character_dict"]]
            result = compare(local, official)
            result["yml_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
            row["official"][source] = result  # type: ignore[index]
            failed |= not result["same"]
            print(f"  和 {source} 上官方 inference.yml 的 character_dict：{describe(result)}")
        if not any(row["official"].values()):  # type: ignore[union-attr]
            failed = True
            print("  两个来源都没有拿到官方 inference.yml：没有核实")
        report["dicts"].append(row)  # type: ignore[union-attr]

    print("\n官方 inference.yml 里的预处理、后处理参数：")
    models = [OFFICIAL_NAME.get(e["id"], e["id"]) for e in manifest["models"] if e["kind"] in ("det", "rec", "cls")]
    for model in models + EXTRA_MODELS:
        got: dict[str, object] = {}
        for source in SOURCES:
            path = fetch(model, source, args.cache, args.offline)
            if path is not None:
                got[source] = params_of(yaml.safe_load(path.read_text(encoding="utf-8")))
        if not got:
            print(f"  {model}：没有拿到")
            report["params"][model] = None  # type: ignore[index]
            continue
        values = list(got.values())
        agree = all(v == values[0] for v in values)
        report["params"][model] = {"sources": sorted(got), "sources_agree": agree, "values": values[0]}  # type: ignore[index]
        note = "" if agree else "（两个来源不一样！）"
        print(f"  {model}（{'、'.join(sorted(got))}）{note}：{json.dumps(values[0], ensure_ascii=False)}")

    if args.json:
        args.json.write_text(json.dumps(report, ensure_ascii=False, indent=1), encoding="utf-8")
    print("\n结论：" + ("有不一致或没有核实的项" if failed else "字典三方逐项一致"))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
