"""生成 V08 的 40 份改字样本（中文 20、英文 20）。

用法（仓库根目录）::

    python tests/corpus/synthetic-text/generate.py

``common-line``（基线）与 ``common``（各级替换后的文本）两份样本由 ``generate.py`` 生成，
其余中文样本（``zh-*-full``、``zh-notembedded`` 等）和英文样本（``en-*-full`` 等）需要
完整嵌入字体（单份 5–10 MB），脚本只把它们写到仓库外 ``MAGPDF_V08_LARGE_DIR``
（默认 ``tests/out/v08-samples-large/``），不入库；``manifest.json`` 记录全部样本参数与文件位置。

样本类型（manifest 里的 ``kind`` / ``embedded``）：

* ``cidfonttype2`` + ``subset`` / ``full``：TrueType 轮廓，子集或完整嵌入；
* ``cidfonttype0`` + ``subset`` / ``full``：FontFile3（CFF 或 OpenType），入口为 CIDFontType0；
* ``cidfonttype2`` + ``none``：不嵌入，只引用字体名；
* ``simple``：简单字体 + WinAnsiEncoding + 单对象。

``case`` 字段说明这次改动应该落在哪一级（评测时与程序判定对比），
``variant`` 说明字体改动范围（``none`` / ``glyphs`` / ``font``）。
"""

from __future__ import annotations

import io
import json
import os
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pdfgen as G  # noqa: E402

ROOT = Path(__file__).resolve().parent
SAMPLES = ROOT / "samples"
LARGE = Path(os.environ.get("MAGPDF_V08_LARGE_DIR", ROOT.parents[2] / "out" / "v08-samples-large"))
MAX_BYTES = 1024 * 1024
PAGE_W, PAGE_H = 595, 842

# 行文：两行，第 1 行短、第 2 行长。字符集覆盖常用数字与标点。
ZH_LINE1 = "合同甲方乙方"
ZH_LINE2 = "合同甲方乙方应按时付款"
EN_LINE1 = "Invoice No: ABC-123"
EN_LINE2 = "Total amount payable"
ZH_EXTRA = "和号货物验收价款式"
EN_EXTRA = "xyzXYZQKMS8/"

# 字体与写法：中文
ZH_FONTS = {
    "fang": ("simfang.ttf", 0, "FangSong"),
    "hei": ("simhei.ttf", 0, "SimHei"),
    "sun": ("simsun.ttc", 0, "SimSun"),
    "sun1": ("simsun.ttc", 1, "NSimSun"),
    "kai": ("simkai.ttf", 0, "KaiTi"),
    "yahei": ("msyh.ttc", 0, "MicrosoftYaHei"),
    "yahei_bd": ("msyhbd.ttc", 0, "MicrosoftYaHei-Bold"),
    "dengxian": ("Deng.ttf", 0, "DengXian"),
    "dengxian_bd": ("Dengb.ttf", 0, "DengXian-Bold"),
    "stzhongs": ("STZHONGS.TTF", 0, "STZhongsong"),
    "stkaiti": ("STKAITI.TTF", 0, "STKaiti"),
    "stfangsong": ("STFANGSO.TTF", 0, "STFangsong"),
    "stxihei": ("STXIHEI.TTF", 0, "STXihei"),
    "simyou": ("SIMYOU.TTF", 0, "SimYou"),
    "li": ("simli.ttf", 0, "LiSu"),
    "yahei_lt": ("msyhl.ttc", 0, "MicrosoftYaHei-Light"),
    "dengxian_lt": ("Dengl.ttf", 0, "DengXian-Light"),
    "stxingkai": ("STXINGKA.TTF", 0, "STXingkai"),
    "stxinwei": ("STXINWEI.TTF", 0, "STXinwei"),
    "stsong": ("STSONG.TTF", 0, "STSong"),
}
EN_FONTS = {
    "times": ("times.ttf", 0, "TimesNewRoman"),
    "timesbd": ("timesbd.ttf", 0, "TimesNewRoman-Bold"),
    "arial": ("arial.ttf", 0, "Arial"),
    "arialbd": ("arialbd.ttf", 0, "Arial-Bold"),
    "calibri": ("calibri.ttf", 0, "Calibri"),
    "calibrib": ("calibrib.ttf", 0, "Calibri-Bold"),
    "cour": ("cour.ttf", 0, "CourierNew"),
    "courbd": ("courbd.ttf", 0, "CourierNew-Bold"),
    "georgia": ("georgia.ttf", 0, "Georgia"),
    "segoe": ("segoeui.ttf", 0, "SegoeUI"),
    "seguibl": ("seguibl.ttf", 0, "SegoeUI-Black"),
    "cambria": ("cambriab.ttf", 0, "Cambria-Bold"),
    "constan": ("constan.ttf", 0, "Constantia"),
    "trebuc": ("trebuc.ttf", 0, "TrebuchetMS"),
    "verdana": ("verdana.ttf", 0, "Verdana"),
    "tahoma": ("tahoma.ttf", 0, "Tahoma"),
    "comic": ("comic.ttf", 0, "ComicSansMS"),
    "pala": ("pala.ttf", 0, "PalatinoLinotype"),
    "candara": ("candara.ttf", 0, "Candara"),
    "consola": ("consola.ttf", 0, "Consolas"),
}


@dataclass
class Case:
    name: str
    lang: str
    font: str
    kind: str  # cidfonttype2 / cidfonttype0 / simple
    embedded: str  # subset / full / none
    line1: str
    line2: str
    case: str  # tier1 / tier2_embed / tier2_system / tier3
    variant: str  # none / glyphs / font
    new1: str = ""
    new2: str = ""
    base_font: str = ""  # 覆盖 PDF 里写的字体名
    extra_pages: int = 0
    pages: str = "1"  # 改哪一页（给两页样本）
    note: str = ""

    @property
    def expected_tier(self) -> int:
        return {"tier1": 1, "tier2_embed": 2, "tier2_system": 2, "tier3": 3}[self.case]

    @property
    def expected_changed(self) -> bool:
        return self.variant != "none"


def zh_cases() -> list[Case]:
    c: list[Case] = []
    fonts = list(ZH_FONTS)
    kinds = ["cidfonttype2", "cidfonttype0", "simple", "cidfonttype2", "cidfonttype0"]
    embeds = ["subset", "subset", "subset", "full", "none"]
    cases = ["tier1", "tier2_embed", "tier3", "tier2_embed", "tier2_system"]
    for i, f in enumerate(fonts):
        kind = kinds[i % len(kinds)]
        embed = embeds[i % len(embeds)]
        case = cases[i % len(cases)] if kind != "simple" else ["tier1", "tier2_embed", "tier2_system", "tier3"][i % 4]
        c.append(
            Case(
                name=f"zh-{f}-{case.replace('_', '-')}",
                lang="zh",
                font=f,
                kind=kind,
                embedded=embed,
                line1=ZH_LINE1,
                line2=ZH_LINE2,
                case=case,
                variant={"tier1": "none", "tier2_embed": "glyphs", "tier2_system": "font", "tier3": "font"}[case],
                extra_pages=1 if i % 7 == 3 else 0,
                pages="1",
            )
        )
    return c


def en_cases() -> list[Case]:
    c: list[Case] = []
    fonts = list(EN_FONTS)
    kinds = ["cidfonttype2", "cidfonttype0", "simple", "cidfonttype2", "cidfonttype0"]
    embeds = ["subset", "subset", "subset", "full", "none"]
    cases = ["tier2_system", "tier1", "tier2_system", "tier2_embed", "tier2_system"]
    for i, f in enumerate(fonts):
        kind = kinds[i % len(kinds)]
        embed = embeds[i % len(embeds)]
        case = cases[i % len(cases)]
        c.append(
            Case(
                name=f"en-{f}-{case.replace('_', '-')}",
                lang="en",
                font=f,
                kind=kind,
                embedded=embed,
                line1=EN_LINE1,
                line2=EN_LINE2,
                case=case,
                variant={"tier1": "none", "tier2_embed": "glyphs", "tier2_system": "font", "tier3": "font"}[case],
                extra_pages=1 if i % 6 == 2 else 0,
            )
        )
    return c


def plan_texts(case: Case) -> tuple[str, str, int]:
    """按 case 生成 (line1, line2, 期望级别)。"""
    if case.case == "tier1":
        # 同一行内调换字的顺序：所有字符都在原字体里，文件不应变化
        l1 = case.line1[2:] + case.line1[:2]
        l2 = case.line2[2:] + case.line2[:2]
    elif case.case == "tier2_embed":
        l1 = case.line1[:2] + "货物" + case.line1[4:]
        l2 = case.line2[:2] + "验收" + case.line2[4:]
    elif case.case == "tier2_system":
        l1 = case.line1[:2] + "价款" + case.line1[4:]
        l2 = case.line2[:2] + "款式" + case.line2[4:]
    else:  # tier3
        l1 = "货物验收" + case.line1[4:]
        l2 = "和号验收" + case.line2[4:]
    if case.lang == "en":
        if case.case == "tier1":
            l1 = case.line1[3:] + case.line1[:3]
            l2 = case.line2[3:] + case.line2[:3]
        elif case.case == "tier2_embed":
            l1 = "Invoice No: xyz-987"
            l2 = "Total XYZ payable"
        elif case.case == "tier2_system":
            l1 = "Ref: xyz-987"
            l2 = "Shipment XYZ"
        else:
            l1 = "Ref: Qx-8"
            l2 = "Shipment QK"
    return l1, l2, case.expected_tier


def build(case: Case) -> tuple[bytes, dict]:
    if case.lang == "zh":
        font_file, index, ps_name = ZH_FONTS[case.font]
        # 子集必须覆盖页面原文的每个字（「甲方」里的「方」曾因西文字体缺字触发 KeyError）。
        # 不把 ZH_EXTRA（货物验收等）编进子集，否则第 2 级替换字已在子集内，会全部落成 tier1。
        new_text = "".join(dict.fromkeys(case.line1 + case.line2))
    else:
        font_file, index, ps_name = EN_FONTS[case.font]
        new_text = "".join(dict.fromkeys(case.line1 + case.line2))
    path = G.FONT_DIR / font_file
    if not path.exists():
        reason = f"missing font {font_file}"
        print(f"SKIP {case.name}: {reason}")
        return None, {"name": case.name, "skipped": True, "reason": reason}
    try:
        probe_map = G.glyph_map(G.font_file(path, index))
    except Exception as exc:
        reason = f"cannot open {font_file}: {exc}"
        print(f"SKIP {case.name}: {reason}")
        return None, {"name": case.name, "skipped": True, "reason": reason}
    missing = [ch for ch in new_text if ch not in probe_map]
    if missing:
        reason = f"{font_file} missing glyphs: {''.join(missing)}"
        print(f"SKIP {case.name}: {reason}")
        return None, {"name": case.name, "skipped": True, "reason": reason}
    base = case.base_font or ps_name
    if case.embedded == "subset":
        base = "ABCDEF+" + base
    b = G.PdfBuilder()
    new1, new2, _ = plan_texts(case)
    warnings: list[str] = []

    if case.kind == "simple":
        codes: dict[str, int] = {}
        for ch_ in new_text:
            if ord(ch_) >= 32:
                codes.setdefault(ch_, 0)
        for i, ch_ in enumerate(sorted(codes)):
            codes[ch_] = 32 + i
        data, gmap = G.ttf_subset(path, new_text, index)
        adv = G.advances(data)
        font = _simple_font(b, base, data, codes, gmap, adv)
        encode = lambda t: bytes(codes[c] for c in t)  # noqa: E731
    else:
        full_data, full_map = G.ttf_full(path, index)
        if case.embedded == "full":
            data, gmap = full_data, full_map
        elif case.embedded == "none":
            data, gmap = None, full_map
        else:
            data, gmap = G.ttf_subset(path, new_text, index)
        adv = G.advances(data if data is not None else full_data)
        tu = G.invert_to_unicode(gmap)
        cid_gid = {gid: gid for gid in gmap.values()}
        font = _type0_font(b, base, data, cid_gid, adv, tu, cff=case.kind == "cidfonttype0")
        encode = lambda t: b"".join(gmap[c].to_bytes(2, "big") for c in t)  # noqa: E731

    pages_ref = b.placeholder()
    page_refs = []
    for _page in range(1 + case.extra_pages):
        ops = [b"BT"]
        for y, size, text in ((750, 14, case.line1), (720, 10.5, case.line2)):
            ops.append(f"/F1 {size} Tf 1 0 0 1 72 {y} Tm".encode())
            ops.append(b"<" + encode(text).hex().encode() + b"> Tj")
        ops.append(b"ET")
        ops.append(b"0.5 w 72 714 m 523 714 l S")
        content = b.add_stream(b"\n".join(ops))
        page_refs.append(
            b.add(
                f"<< /Type /Page /Parent {pages_ref} 0 R /MediaBox [0 0 {PAGE_W} {PAGE_H}] "
                f"/Resources << /Font << /F1 {font} 0 R >> >> /Contents {content} 0 R >>".encode()
            )
        )
    kids = " ".join(f"{p} 0 R" for p in page_refs)
    b.set(pages_ref, f"<< /Type /Pages /Kids [{kids}] /Count {len(page_refs)} >>".encode())
    catalog = b.add(f"<< /Type /Catalog /Pages {pages_ref} 0 R >>".encode())
    data_out = b.build(catalog)

    page = int(case.pages)
    meta = {
        "name": case.name,
        "lang": case.lang,
        "kind": case.kind,
        "embedded": case.embedded,
        "font_file": font_file,
        "font_index": index,
        "base_font": base,
        "pages": 1 + case.extra_pages,
        "lines": [
            {"index": 1, "page": page, "text": case.line1, "size": 14},
            {"index": 2, "page": page, "text": case.line2, "size": 10.5},
        ],
        "edits": [
            {
                "page": page,
                "line": 1,
                "new_text": new1,
                "case": case.case,
                "expected_tier": case.expected_tier,
                "expected_changed": case.expected_changed,
                "variant": case.variant,
            },
            {
                "page": page,
                "line": 2,
                "new_text": new2,
                "case": case.case,
                "expected_tier": case.expected_tier,
                "expected_changed": case.expected_changed,
                "variant": case.variant,
            },
        ],
        "note": case.note,
        "warnings": warnings,
    }
    return data_out, meta


def _type0_font(b, base: str, data, cid_gid, adv, tu, cff: bool) -> int:
    parts = [f"/Type /Font /Subtype /CIDFontType{'0' if cff else '2'}", f"/BaseFont /{base}"]
    parts.append("/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>")
    if data is not None:
        if cff:
            ff = b.add_stream(data, "/Subtype /OpenType")
            parts.append(f"/FontDescriptor {_descriptor(b, base, 4, ff, 'FontFile3')} 0 R")
        else:
            ff = b.add_stream(data, "/Length1 %d" % len(data))
            parts.append(f"/FontDescriptor {_descriptor(b, base, 4, ff, 'FontFile2')} 0 R")
    else:
        parts.append(f"/FontDescriptor {_descriptor(b, base, 4, None, '')} 0 R")
    widths = {int(cid): adv.get(gid, 500) for cid, gid in cid_gid.items()}
    parts.append(f"/DW 1000 /W {b.add(G.widths_array(widths))} 0 R")
    desc = b.add(("<< " + " ".join(parts) + " >>").encode())
    cmap = b.add_stream(G.to_unicode_cmap(tu))
    return b.add(
        f"<< /Type /Font /Subtype /Type0 /BaseFont /{base} /Encoding /Identity-H "
        f"/DescendantFonts [{desc} 0 R] /ToUnicode {cmap} 0 R >>".encode()
    )


def _descriptor(b, base: str, flags: int, ff, key: str) -> int:
    parts = [
        f"/Type /FontDescriptor /FontName /{base} /Flags {flags}",
        "/FontBBox [0 -200 1000 900] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 700 /StemV 80",
    ]
    if ff is not None:
        parts.append(f"/{key} {ff} 0 R")
    return b.add(("<< " + " ".join(parts) + " >>").encode())


def _simple_font(b, base: str, data: bytes, codes, gmap, adv) -> int:
    ff = b.add_stream(data, "/Length1 %d" % len(data))
    fd = _descriptor(b, base, 32, ff, "FontFile2")
    first, last = min(codes.values()), max(codes.values())
    widths = [adv.get(gmap.get(chr(c), 0), 500) for c in range(first, last + 1)]
    w = b.add(b"[" + b" ".join(str(x).encode() for x in widths) + b"]")
    order = sorted(codes, key=lambda x: codes[x])
    diffs = " ".join(f"{codes[ch_]} /g{gmap.get(ch_, 0)}" for ch_ in order)
    tu = {codes[ch_]: ch_ for ch_ in codes}
    cmap = b.add_stream(G.to_unicode_cmap(tu))
    return b.add(
        f"<< /Type /Font /Subtype /TrueType /BaseFont /{base} /FirstChar {first} /LastChar {last} "
        f"/Widths {w} 0 R /Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding "
        f"/Differences [{diffs}] >> /FontDescriptor {fd} 0 R /ToUnicode {cmap} 0 R >>".encode()
    )


def main() -> int:
    SAMPLES.mkdir(parents=True, exist_ok=True)
    LARGE.mkdir(parents=True, exist_ok=True)
    manifest = []
    for case in zh_cases() + en_cases():
        data, meta = build(case)
        if data is None:
            manifest.append(meta)
            continue
        committed = len(data) <= MAX_BYTES
        target = (SAMPLES if committed else LARGE) / f"{case.name}.pdf"
        target.write_bytes(data)
        try:
            rel = str(target.relative_to(ROOT))
        except ValueError:
            rel = str(target)
        meta.update({"bytes": len(data), "committed": committed, "path": rel})
        manifest.append(meta)
        print(f"{target.name}: {len(data)} bytes{'（不入库）' if not committed else ''}")
    (SAMPLES / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=1), encoding="utf-8")
    total = len(manifest)
    written = [m for m in manifest if not m.get("skipped")]
    ok = sum(1 for m in written if m.get("committed"))
    skipped = sum(1 for m in manifest if m.get("skipped"))
    print(f"合计设计 {total} 份，写出 {len(written)} 份，入库 {ok} 份，跳过 {skipped} 份")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
