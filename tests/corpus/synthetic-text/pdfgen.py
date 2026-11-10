"""V08 改字样本的生成工具：手工构造 PDF 结构，覆盖各类字体编码。

只依赖 Python 标准库（``zlib``）；裁剪字体用 ``fontTools``（只在生成样本时用，不在产品依赖里）。
样本 PDF 写成不压缩的简单结构，便于用 pikepdf / pdfjs 检查；字体数据流用 FlateDecode。

覆盖的字体写法见 ``samples.py``。
"""

from __future__ import annotations

import io
import struct
import zlib
from pathlib import Path

FONT_DIR = Path(r"C:\Windows\Fonts")


class PdfBuilder:
    """最小 PDF 写出器：对象编号从 1 开始，交叉引用表按写出顺序生成。"""

    def __init__(self) -> None:
        self.objects: list[bytes] = []

    def add(self, body: bytes) -> int:
        self.objects.append(body)
        return len(self.objects)

    def placeholder(self) -> int:
        """先占一个对象编号，稍后用 :meth:`set` 回填（页树要引用页对象，页对象要引用页树）。"""
        return self.add(b"<< >>")

    def set(self, num: int, body: bytes) -> None:
        self.objects[num - 1] = body

    def add_stream(self, data: bytes, extra: str = "", compress: bool = True) -> int:
        if compress:
            data = zlib.compress(data)
            extra = f"/Filter /FlateDecode {extra}"
        body = (
            f"<< /Length {len(data)} {extra}>>\nstream\n".encode("ascii")
            + data
            + b"\nendstream"
        )
        return self.add(body)

    def build(self, root: int) -> bytes:
        info = self.add(b"<< /Producer (MagPDF V08 synthetic) >>")
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets: list[int] = []
        for num, body in enumerate(self.objects, start=1):
            offsets.append(len(out))
            out += b"%d 0 obj\n" % num + body + b"\nendobj\n"
        xref = len(out)
        out += b"xref\n0 %d\n" % (len(self.objects) + 1)
        out += b"0000000000 65535 f \n"
        for off in offsets:
            out += b"%010d 00000 n \n" % off
        out += b"trailer\n<< /Size %d /Root %d 0 R /Info %d 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
            len(self.objects) + 1,
            root,
            info,
            xref,
        )
        return bytes(out)


def _unique_map(mapping: dict[str, int]) -> dict[str, int]:
    """去掉 GID 重复的映射（多个码点共用一个字形，如全角空格与空格）。"""
    seen: set[int] = set()
    out: dict[str, int] = {}
    for ch, gid in mapping.items():
        if gid in seen:
            continue
        seen.add(gid)
        out[ch] = gid
    return out


def glyph_map(font) -> dict[str, int]:
    """字符 → GID，取自字体**每一张** Unicode cmap 子表。

    不用 ``getBestCmap``：它只挑一张子表，本机的 Dengb.ttf 里 0x65B9 在子表里映射到 ``uni2F45``
    （字形名误导，但 GID 正确），按字形名反推会漏字。

    不要对共享轮廓去重：等线里「方」(U+65B9) 与康熙部首「⽅」(U+2F45) 共用 GID，
    去重会丢掉正文用的码点，生成时 ``gmap['方']`` 就会 KeyError。ToUnicode 只需
    GID → 一个码点，由调用方在反转映射时自然去重。
    """
    order = font.getGlyphOrder()
    index = {name: i for i, name in enumerate(order)}
    out: dict[str, int] = {}
    for table in font["cmap"].tables:
        if table.isUnicode():
            for code, name in table.cmap.items():
                ch = chr(code)
                if ch in out:
                    continue
                gid = index.get(name)
                if gid is not None:
                    out[ch] = gid
    return out



def unicode_rank(ch: str):
    cp = ord(ch)
    if cp < 0x20 or 0x7F <= cp < 0xA0:
        return (5, cp)
    if 0x20 <= cp <= 0x7E:
        return (0, cp)
    if 0x4E00 <= cp <= 0x9FFF:
        return (1, cp)
    if 0x3400 <= cp <= 0x4DBF or 0x20000 <= cp <= 0x2A6DF:
        return (2, cp)
    if 0x2F00 <= cp <= 0x2FDF or 0xF900 <= cp <= 0xFAFF:
        return (4, cp)
    return (3, cp)


def prefer_unicode(a: str, b: str) -> str:
    return b if unicode_rank(b) < unicode_rank(a) else a


def invert_to_unicode(gmap: dict) -> dict:
    """GID -> one codepoint.

    Prefer ASCII (so space stays U+0020, not NBSP) and CJK unified ideographs
    (so U+65B9 wins over Kangxi radical U+2F45).
    """
    tu = {}
    for ch, gid in gmap.items():
        if gid not in tu:
            tu[gid] = ch
        else:
            tu[gid] = prefer_unicode(tu[gid], ch)
    return tu


def font_file(path: Path, index: int = 0):
    """打开字体文件（TTC 按面索引）。"""
    from fontTools.ttLib import TTCollection, TTFont

    raw = path.read_bytes()
    if raw[:4] == b"ttcf":
        return TTCollection(io.BytesIO(raw)).fonts[index]
    return TTFont(io.BytesIO(raw), fontNumber=index)


def ttf_subset(path: Path, chars: str, index: int = 0) -> tuple[bytes, dict[str, int]]:
    """按字符子集化 TrueType（glyf 轮廓），返回 (字体字节, 字符→GID)。"""
    from fontTools import subset

    font = font_file(path, index)
    opts = subset.Options()
    opts.notdef_outline = True
    opts.recalc_bounds = True
    opts.drop_tables += ["GSUB", "GPOS", "GDEF", "BASE", "DSIG"]
    sub = subset.Subsetter(options=opts)
    sub.populate(text=chars)
    sub.subset(font)
    out = io.BytesIO()
    font.save(out)
    return out.getvalue(), glyph_map(font)


def ttf_full(path: Path, index: int = 0) -> tuple[bytes, dict[str, int]]:
    """完整 TrueType 字体与字符→GID（不改写字形，只读 cmap）。"""
    font = font_file(path, index)
    out = io.BytesIO()
    font.save(out)
    return out.getvalue(), glyph_map(font)


def cff_table(path: Path, index: int = 0) -> tuple[bytes, dict[str, int]]:
    """OpenType/CFF 字体里的 `CFF ` 表原始字节（作 FontFile3 /OpenType），以及字符→GID。"""
    font = font_file(path, index)
    table = font["CFF "]
    out = io.BytesIO()
    table.compile(out)
    cmap = font.getBestCmap()
    return out.getvalue(), cmap


def advances(ttf_bytes: bytes) -> dict[int, int]:
    """每个 GID 的进距，单位 1000 分之一 em。"""
    from fontTools.ttLib import TTFont

    font = TTFont(io.BytesIO(ttf_bytes))
    upem = font["head"].unitsPerEm
    hmtx = font["hmtx"]
    order = font.getGlyphOrder()
    return {gid: round(hmtx[glyph][0] * 1000 / upem) for gid, glyph in enumerate(order)}


def cff_advances(cff: bytes) -> dict[int, int]:
    """CFF 表里的字符宽度（em 单位）换算成 1000 分之一 em。"""
    from fontTools.cffLib import CFFFontSet

    cff_font = CFFFontSet()
    cff_font.decompile(io.BytesIO(cff), None)
    top = cff_font[cff_font.fontNames[0]]
    charstrings = top.CharStrings
    out: dict[int, int] = {}
    for name in charstrings.keys():
        gid = charstrings.charStringsIndex.getGlyphNameIndex()[name] if hasattr(charstrings.charStringsIndex, "getGlyphNameIndex") else None
        if gid is None:
            continue
        width = top.Private and top.Private.nominalWidthX or 0
        try:
            cs = charstrings[name]
            cs.decompile()
            w = cs.width if cs.width is not None else top.Private.defaultWidthX
            out[gid] = round((w - top.Private.defaultWidthX if False else w) * 1000)
        except Exception:
            out[gid] = 0
    return out


def cmap_table(mapping: dict[int, int]) -> bytes:
    """format 4 的 cmap 表：`version=0, numTables=1, (3,1) → 子表`。"""
    codes = sorted(mapping)
    segs: list[tuple[int, int]] = []
    start = prev = codes[0]
    for c in codes[1:]:
        if c == prev + 1:
            prev = c
            continue
        segs.append((start, prev))
        start = prev = c
    segs.append((start, prev))
    segs.append((0xFFFF, 0xFFFF))
    n = len(segs)
    seg_count_x2 = n * 2
    search_range = 2 * (1 << (n.bit_length() - 1))
    entry_selector = n.bit_length() - 1
    range_shift = seg_count_x2 - search_range
    end_codes = b"".join(struct.pack(">H", e) for _, e in segs)
    start_codes = b"".join(struct.pack(">H", s) for s, _ in segs)
    deltas = []
    for s, _ in segs:
        deltas.append(1 if s == 0xFFFF else (mapping[s] - s) & 0xFFFF)
    id_delta = b"".join(struct.pack(">H", d) for d in deltas)
    id_range_offset = b"\x00\x00" * n
    sub = (
        struct.pack(">HHHHHHH", 4, 16 + 8 * n, 0, seg_count_x2, search_range, entry_selector, range_shift)
        + end_codes
        + b"\x00\x00"
        + start_codes
        + id_delta
        + id_range_offset
    )
    header = struct.pack(">HH", 0, 1) + struct.pack(">HHI", 3, 1, 12)
    return header + sub


def to_unicode_cmap(pairs: dict[int, str]) -> bytes:
    items = sorted(pairs.items())
    lines = [
        "/CIDInit /ProcSet findresource begin",
        "12 dict begin",
        "begincmap",
        "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def",
        "/CMapName /Adobe-Identity-UCS def",
        "/CMapType 2 def",
        "1 begincodespacerange",
        "<0000> <FFFF>",
        "endcodespacerange",
    ]
    for i in range(0, len(items), 100):
        chunk = items[i : i + 100]
        lines.append(f"{len(chunk)} beginbfchar")
        for code, text in chunk:
            utf16 = text.encode("utf-16-be").hex().upper()
            lines.append(f"<{code:04X}> <{utf16}>")
        lines.append("endbfchar")
    lines += ["endcmap", "CMapName currentdict /CMap defineresource pop", "end", "end"]
    return ("\n".join(lines) + "\n").encode()


def widths_array(cids: dict[int, int]) -> bytes:
    """W 数组：按连续 CID 分组的 `c [w...]` 形式。"""
    groups: list[list[int]] = []
    for c in sorted(cids):
        if groups and c == groups[-1][-1] + 1:
            groups[-1].append(c)
        else:
            groups.append([c])
    body = " ".join(f"{g[0]} [" + " ".join(str(cids[c]) for c in g) + "]" for g in groups)
    return f"[{body}]".encode()
