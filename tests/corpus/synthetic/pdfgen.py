"""手写 PDF 的最小写出器与二进制素材构造（只用 Python 3.12 标准库）。

设计要求（《工程规范》、V14 任务说明）：
- 输出必须确定：没有时间戳、没有随机数，文档 /ID 由样本名哈希得到；
- 交叉引用表 / 交叉引用流的偏移必须逐字节正确；
- 支持经典 xref 表、PDF 1.5 的 xref 流 + 对象流、增量更新，以及故意写坏的 xref（偏移错位、缺 xref）；
- 对象体由调用方按 PDF 语法手写（字符串按 latin-1 编码），本模块只负责编号、流长度、文件组装。

另外提供几种样本需要的二进制素材：最小 ICC 配置文件、极小 TrueType 字体、Code 39 条码、
Adobe-GB1 CID 换算、ISO 6346 箱号校验位。
"""

from __future__ import annotations

import hashlib
import struct
import zlib
from dataclasses import dataclass, field

HEADER_COMMENT = b"%\xe2\xe3\xcf\xd3\n"


def num(v: float | int) -> str:
    """PDF 数字：整数原样，浮点最多 4 位小数并去掉尾零（保证确定性与可读性）。"""
    if isinstance(v, bool):
        raise TypeError("bool is not a PDF number")
    if isinstance(v, int):
        return str(v)
    s = f"{v:.4f}".rstrip("0").rstrip(".")
    if s in ("", "-0"):
        return "0"
    return s


def nums(*vals: float | int) -> str:
    return " ".join(num(v) for v in vals)


def pdf_string(text: str) -> str:
    """ASCII/latin-1 文本转为 (…) 字面量，转义括号与反斜杠。"""
    out = []
    for ch in text:
        if ch in "()\\":
            out.append("\\" + ch)
        elif ord(ch) < 32 or ord(ch) > 126:
            out.append(f"\\{ord(ch):03o}")
        else:
            out.append(ch)
    return "(" + "".join(out) + ")"


def utf16_string(text: str) -> str:
    """任意 Unicode 文本转为带 BOM 的 UTF-16BE 十六进制串。"""
    return "<" + (b"\xfe\xff" + text.encode("utf-16-be")).hex().upper() + ">"


def hexstr(data: bytes) -> str:
    return "<" + data.hex().upper() + ">"


class Ref:
    """间接对象引用。"""

    __slots__ = ("num", "gen")

    def __init__(self, num: int, gen: int = 0) -> None:
        self.num = num
        self.gen = gen

    def __str__(self) -> str:
        return f"{self.num} {self.gen} R"

    def __repr__(self) -> str:
        return f"Ref({self.num}, {self.gen})"


@dataclass
class _Obj:
    body: bytes
    is_stream: bool
    gen: int = 0


@dataclass
class Pdf:
    """一个正在构造的 PDF 文档。

    用法：
        pdf = Pdf("1.4")
        font = pdf.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        content = pdf.stream("", b"0 0 1 rg 10 10 50 50 re f")
        ...
        pdf.root = pdf.add("<< /Type /Catalog /Pages ... >>")
        data = pdf.build()
    """

    version: str = "1.4"
    root: Ref | None = None
    info: Ref | None = None
    doc_id: bytes = b"\0" * 16
    _objs: dict[int, _Obj] = field(default_factory=dict)
    _count: int = 0
    _dirty: set[int] = field(default_factory=set)
    last_startxref: int | None = None

    # ---- 对象 -------------------------------------------------------------

    def reserve(self, gen: int = 0) -> Ref:
        self._count += 1
        return Ref(self._count, gen)

    def set(self, ref: Ref, body: str | bytes, *, is_stream: bool = False) -> Ref:
        data = body.encode("latin-1") if isinstance(body, str) else body
        self._objs[ref.num] = _Obj(data, is_stream, ref.gen)
        self._dirty.add(ref.num)
        return ref

    def add(self, body: str | bytes) -> Ref:
        return self.set(self.reserve(), body)

    def stream(
        self,
        entries: str,
        data: bytes,
        *,
        flate: bool = False,
        length: int | None = None,
        ref: Ref | None = None,
        extra_filter_entries: str = "",
        endstream: bool = True,
    ) -> Ref:
        """写一个流对象。`entries` 是字典里 /Length 之外的键值（不含 << >>）。

        flate=True 时用 zlib 压缩（级别 9，对同一 zlib 版本确定）。
        length 可以故意写错、endstream=False 可以省掉 endstream 关键字，用来构造损坏样本。
        """
        if flate:
            data = zlib.compress(data, 9)
            entries = f"{entries} /Filter /FlateDecode{extra_filter_entries}"
        n = len(data) if length is None else length
        head = f"<< {entries.strip()} /Length {n} >>\nstream\n".encode("latin-1")
        body = head + data + (b"\nendstream" if endstream else b"")
        return self.set(ref or self.reserve(), body, is_stream=True)

    def set_id_from(self, seed: str) -> None:
        self.doc_id = hashlib.md5(seed.encode("utf-8")).digest()

    # ---- 组装 -------------------------------------------------------------

    def _trailer_entries(self, size: int, prev: int | None = None) -> str:
        if self.root is None:
            raise ValueError("root not set")
        parts = [f"/Size {size}", f"/Root {self.root}"]
        if self.info is not None:
            parts.append(f"/Info {self.info}")
        parts.append(f"/ID [{hexstr(self.doc_id)} {hexstr(self.doc_id)}]")
        if prev is not None:
            parts.append(f"/Prev {prev}")
        return " ".join(parts)

    @staticmethod
    def _obj_bytes(n: int, o: _Obj) -> bytes:
        return f"{n} {o.gen} obj\n".encode("latin-1") + o.body + b"\nendobj\n"

    def build(
        self,
        *,
        xref_stream: bool = False,
        objstm: bool = False,
        flate: bool = False,
        predictor: bool = False,
        xref_offset_bias: int = 0,
        drop_xref: bool = False,
        trailer_extra: str = "",
    ) -> bytes:
        """组装完整文件。

        xref_stream: 用交叉引用流（PDF 1.5）代替 xref 表。
        objstm: 把所有非流对象放进一个对象流（要求 xref_stream=True）。
        flate / predictor: 对象流与 xref 流是否压缩、xref 流是否用 PNG Up 预测器。
        xref_offset_bias: 故意把 xref 表里的偏移加上这个值（构造损坏样本）。
        drop_xref: 不写 xref 表，startxref 指向 trailer（构造"缺 xref"样本）。
        """
        if objstm and not xref_stream:
            raise ValueError("object streams require an xref stream")
        out = bytearray()
        out += f"%PDF-{self.version}\n".encode("latin-1") + HEADER_COMMENT
        offsets: dict[int, int] = {}
        in_objstm: dict[int, tuple[int, int]] = {}  # num -> (objstm num, index)

        numbers = sorted(self._objs)
        packed: list[int] = []
        if objstm:
            packed = [n for n in numbers if not self._objs[n].is_stream and self._objs[n].gen == 0]
        direct = [n for n in numbers if n not in packed]

        objstm_ref: Ref | None = None
        if packed:
            objstm_ref = self.reserve()
            header_parts: list[str] = []
            bodies = bytearray()
            for i, n in enumerate(packed):
                header_parts.append(f"{n} {len(bodies)}")
                bodies += self._objs[n].body + b"\n"
                in_objstm[n] = (objstm_ref.num, i)
            header = (" ".join(header_parts) + "\n").encode("latin-1")
            self.stream(
                f"/Type /ObjStm /N {len(packed)} /First {len(header)}",
                header + bytes(bodies),
                flate=flate,
                ref=objstm_ref,
            )
            direct.append(objstm_ref.num)

        for n in direct:
            offsets[n] = len(out)
            out += self._obj_bytes(n, self._objs[n])

        if drop_xref:
            trailer_pos = len(out)
            out += b"trailer\n<< " + self._trailer_entries(self._count + 1).encode("latin-1")
            out += f" {trailer_extra} >>\nstartxref\n{trailer_pos}\n%%EOF\n".encode("latin-1")
            self.last_startxref = trailer_pos
            self._dirty.clear()
            return bytes(out)

        if xref_stream:
            xref_ref = self.reserve()
            size = self._count + 1
            rows = bytearray()
            for n in range(size):
                if n == 0:
                    rows += struct.pack(">BIH", 0, 0, 0xFFFF)
                elif n in offsets:
                    rows += struct.pack(">BIH", 1, offsets[n] + xref_offset_bias, 0)
                elif n in in_objstm:
                    rows += struct.pack(">BIH", 2, in_objstm[n][0], in_objstm[n][1])
                elif n == xref_ref.num:
                    rows += struct.pack(">BIH", 1, len(out), 0)
                else:
                    rows += struct.pack(">BIH", 0, 0, 0)
            data = bytes(rows)
            parms = ""
            if predictor:
                data = png_up_predict(data, 7)
                parms = " /DecodeParms << /Predictor 12 /Columns 7 >>"
            entries = f"/Type /XRef /W [1 4 2] /Index [0 {size}] {self._trailer_entries(size)} {trailer_extra}"
            self.stream(entries, data, flate=flate, ref=xref_ref, extra_filter_entries=parms)
            xref_pos = len(out)
            out += self._obj_bytes(xref_ref.num, self._objs[xref_ref.num])
            out += f"startxref\n{xref_pos}\n%%EOF\n".encode("latin-1")
        else:
            xref_pos = len(out)
            size = self._count + 1
            lines = [f"xref\n0 {size}\n", "0000000000 65535 f \n"]
            for n in range(1, size):
                if n in offsets:
                    lines.append(f"{offsets[n] + xref_offset_bias:010d} {self._objs[n].gen:05d} n \n")
                else:
                    lines.append("0000000000 65535 f \n")
            out += "".join(lines).encode("latin-1")
            out += f"trailer\n<< {self._trailer_entries(size)} {trailer_extra} >>\nstartxref\n{xref_pos}\n%%EOF\n".encode(
                "latin-1"
            )
        self.last_startxref = xref_pos
        self._dirty.clear()
        return bytes(out)

    def build_update(self, base: bytes) -> bytes:
        """在 `base`（上一次 build 的结果）后追加一个增量更新节：只写自上次 build 后改过/新增的对象。"""
        if self.last_startxref is None:
            raise ValueError("build() must run before build_update()")
        out = bytearray(base)
        if not out.endswith(b"\n"):
            out += b"\n"
        offsets: dict[int, int] = {}
        for n in sorted(self._dirty):
            offsets[n] = len(out)
            out += self._obj_bytes(n, self._objs[n])
        xref_pos = len(out)
        # 按连续区段写 xref 子节
        lines = ["xref\n"]
        nums_sorted = sorted(offsets)
        i = 0
        while i < len(nums_sorted):
            j = i
            while j + 1 < len(nums_sorted) and nums_sorted[j + 1] == nums_sorted[j] + 1:
                j += 1
            lines.append(f"{nums_sorted[i]} {j - i + 1}\n")
            for n in nums_sorted[i : j + 1]:
                lines.append(f"{offsets[n]:010d} {self._objs[n].gen:05d} n \n")
            i = j + 1
        out += "".join(lines).encode("latin-1")
        size = self._count + 1
        out += f"trailer\n<< {self._trailer_entries(size, prev=self.last_startxref)} >>\nstartxref\n{xref_pos}\n%%EOF\n".encode(
            "latin-1"
        )
        self.last_startxref = xref_pos
        self._dirty.clear()
        return bytes(out)


def png_up_predict(data: bytes, columns: int) -> bytes:
    """按 PNG "Up" 过滤器（类型 2）编码，行宽 columns 字节；供 /Predictor 12 使用。"""
    assert len(data) % columns == 0
    out = bytearray()
    prev = bytes(columns)
    for i in range(0, len(data), columns):
        row = data[i : i + columns]
        out.append(2)
        out += bytes((row[k] - prev[k]) & 0xFF for k in range(columns))
        prev = row
    return bytes(out)


# ---------------------------------------------------------------------------
# ICC 配置文件：最小的 v2 矩阵/TRC RGB 显示器配置文件。
# ---------------------------------------------------------------------------


def _s15f16(v: float) -> bytes:
    return struct.pack(">i", round(v * 65536))


def _icc_xyz(x: float, y: float, z: float) -> bytes:
    return b"XYZ " + b"\0" * 4 + _s15f16(x) + _s15f16(y) + _s15f16(z)


def _icc_curv_gamma(g: float) -> bytes:
    return b"curv" + b"\0" * 4 + struct.pack(">IH", 1, round(g * 256)) + b"\0\0"


def _icc_desc(text: str) -> bytes:
    s = text.encode("ascii") + b"\0"
    return b"desc" + b"\0" * 4 + struct.pack(">I", len(s)) + s + b"\0" * (4 + 4 + 2 + 1 + 67)


def _icc_text(text: str) -> bytes:
    return b"text" + b"\0" * 4 + text.encode("ascii") + b"\0"


def minimal_icc_rgb(*, swap_rg: bool = False, description: str = "MagPDF synthetic RGB") -> bytes:
    """生成一个 lcms 能解析的最小 RGB 配置文件（D50 适配的 sRGB 原色，gamma 2.2）。

    swap_rg=True 时把红、绿原色对调：按配置文件转换的渲染器会把 (1,0,0) 画成绿色，
    直接按 DeviceRGB 回退的渲染器画成红色——一眼就能看出 ICC 是否被真正使用。
    """
    r = (0.4361, 0.2225, 0.0139)
    g = (0.3851, 0.7169, 0.0971)
    b = (0.1431, 0.0606, 0.7141)
    if swap_rg:
        r, g = g, r
    tags: list[tuple[bytes, bytes]] = [
        (b"desc", _icc_desc(description)),
        (b"cprt", _icc_text("No copyright, synthetic test data")),
        (b"wtpt", _icc_xyz(0.9642, 1.0, 0.8249)),
        (b"rXYZ", _icc_xyz(*r)),
        (b"gXYZ", _icc_xyz(*g)),
        (b"bXYZ", _icc_xyz(*b)),
        (b"rTRC", _icc_curv_gamma(2.2)),
        (b"gTRC", _icc_curv_gamma(2.2)),
        (b"bTRC", _icc_curv_gamma(2.2)),
    ]
    table_len = 4 + 12 * len(tags)
    offset = 128 + table_len
    table = struct.pack(">I", len(tags))
    body = bytearray()
    for sig, data in tags:
        padded = data + b"\0" * ((4 - len(data) % 4) % 4)
        table += sig + struct.pack(">II", offset + len(body), len(data))
        body += padded
    size = 128 + table_len + len(body)
    header = bytearray(128)
    struct.pack_into(">I", header, 0, size)
    struct.pack_into(">I", header, 8, 0x02100000)  # version 2.1
    header[12:16] = b"mntr"
    header[16:20] = b"RGB "
    header[20:24] = b"XYZ "
    header[36:40] = b"acsp"
    header[68:80] = _s15f16(0.9642) + _s15f16(1.0) + _s15f16(0.8249)
    return bytes(header) + table + bytes(body)


# ---------------------------------------------------------------------------
# 极小 TrueType 字体：几个几何字形，用于"嵌入 TrueType 子集"样本。
# ---------------------------------------------------------------------------


def _ttf_checksum(data: bytes) -> int:
    data = data + b"\0" * ((4 - len(data) % 4) % 4)
    return sum(struct.unpack(f">{len(data) // 4}I", data)) & 0xFFFFFFFF


def _ttf_glyph(contours: list[list[tuple[int, int]]]) -> bytes:
    """简单字形：所有点都在曲线上（直线轮廓）。"""
    if not contours:
        return b""
    pts = [p for c in contours for p in c]
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    out = bytearray(struct.pack(">hhhhh", len(contours), min(xs), min(ys), max(xs), max(ys)))
    end = -1
    for c in contours:
        end += len(c)
        out += struct.pack(">H", end)
    out += struct.pack(">H", 0)  # 无指令
    out += bytes([0x01] * len(pts))  # 全部在曲线上，坐标用 int16
    px = py = 0
    for x, _ in pts:
        out += struct.pack(">h", x - px)
        px = x
    for _, y in pts:
        out += struct.pack(">h", y - py)
        py = y
    if len(out) % 4:
        out += b"\0" * (4 - len(out) % 4)
    return bytes(out)


def _ttf_cmap(mapping: dict[int, int]) -> bytes:
    """cmap：(3,1) format 4 + (1,0) format 0。mapping: 字符码 -> 字形号（码点 < 256）。"""
    codes = sorted(mapping)
    segments: list[tuple[int, int, int]] = []  # start, end, delta
    i = 0
    while i < len(codes):
        j = i
        while (
            j + 1 < len(codes)
            and codes[j + 1] == codes[j] + 1
            and mapping[codes[j + 1]] == mapping[codes[j]] + 1
        ):
            j += 1
        segments.append((codes[i], codes[j], (mapping[codes[i]] - codes[i]) & 0xFFFF))
        i = j + 1
    segments.append((0xFFFF, 0xFFFF, 1))
    seg_count = len(segments)
    entry_selector = seg_count.bit_length() - 1
    search_range = 2 * (1 << entry_selector)
    range_shift = 2 * seg_count - search_range
    f4 = bytearray(struct.pack(">HHHHHHH", 4, 16 + 8 * seg_count, 0, seg_count * 2, search_range, entry_selector, range_shift))
    f4 += b"".join(struct.pack(">H", s[1]) for s in segments)
    f4 += b"\0\0"
    f4 += b"".join(struct.pack(">H", s[0]) for s in segments)
    f4 += b"".join(struct.pack(">H", s[2]) for s in segments)
    f4 += b"\0\0" * seg_count
    f0 = bytearray(struct.pack(">HHH", 0, 262, 0))
    f0 += bytes(mapping.get(c, 0) for c in range(256))
    header = struct.pack(">HH", 0, 2)
    rec_len = 8 * 2
    off_f0 = 4 + rec_len
    off_f4 = off_f0 + len(f0)
    header += struct.pack(">HHI", 1, 0, off_f0) + struct.pack(">HHI", 3, 1, off_f4)
    return header + bytes(f0) + bytes(f4)


def _ttf_name(names: dict[int, str]) -> bytes:
    records = []
    strings = bytearray()
    for name_id in sorted(names):
        s_mac = names[name_id].encode("ascii")
        records.append((1, 0, 0, name_id, len(s_mac), len(strings)))
        strings += s_mac
        s_win = names[name_id].encode("utf-16-be")
        records.append((3, 1, 0x409, name_id, len(s_win), len(strings)))
        strings += s_win
    records.sort()
    out = bytearray(struct.pack(">HHH", 0, len(records), 6 + 12 * len(records)))
    for r in records:
        out += struct.pack(">HHHHHH", *r)
    return bytes(out) + bytes(strings)


def minimal_truetype(
    glyphs: list[tuple[str, int, list[list[tuple[int, int]]]]],
    family: str = "MagSynthGeo",
    units_per_em: int = 1000,
    ascent: int = 800,
    descent: int = -200,
) -> bytes:
    """构造一个只含直线字形的 TrueType 字体。

    glyphs: [(字符, 步进宽度, 轮廓列表)]，第 0 个字形自动作为 .notdef（空）。
    返回完整的 sfnt 字节，可直接作 /FontFile2。
    """
    glyph_data: list[bytes] = [b""]
    advances: list[int] = [glyphs[0][1] if glyphs else units_per_em // 2]
    lsbs: list[int] = [0]
    mapping: dict[int, int] = {}
    for gid, (ch, adv, contours) in enumerate(glyphs, start=1):
        glyph_data.append(_ttf_glyph(contours))
        advances.append(adv)
        lsbs.append(min((p[0] for c in contours for p in c), default=0))
        mapping[ord(ch)] = gid
    num_glyphs = len(glyph_data)
    all_pts = [p for g in glyphs for c in g[2] for p in c]
    xmin = min((p[0] for p in all_pts), default=0)
    ymin = min((p[1] for p in all_pts), default=0)
    xmax = max((p[0] for p in all_pts), default=0)
    ymax = max((p[1] for p in all_pts), default=0)
    max_pts = max((sum(len(c) for c in g[2]) for g in glyphs), default=0)
    max_contours = max((len(g[2]) for g in glyphs), default=0)

    loca = bytearray()
    glyf = bytearray()
    for g in glyph_data:
        loca += struct.pack(">I", len(glyf))
        glyf += g
    loca += struct.pack(">I", len(glyf))

    head = struct.pack(
        ">IIIIHHQQhhhhHHhhh",
        0x00010000,  # version
        0x00010000,  # fontRevision
        0,  # checkSumAdjustment（稍后填）
        0x5F0F3CF5,  # magic
        0x000B,  # flags
        units_per_em,
        0,  # created
        0,  # modified
        xmin,
        ymin,
        xmax,
        ymax,
        0,  # macStyle
        8,  # lowestRecPPEM
        2,  # fontDirectionHint
        1,  # indexToLocFormat: long
        0,  # glyphDataFormat
    )
    hhea = struct.pack(
        ">IhhhHhhhhhhhhhhhH",
        0x00010000,
        ascent,
        descent,
        0,  # lineGap
        max(advances),
        0,  # minLeftSideBearing
        0,  # minRightSideBearing
        xmax,  # xMaxExtent
        1,
        0,
        0,  # caret
        0,
        0,
        0,
        0,  # reserved
        0,  # metricDataFormat
        num_glyphs,
    )
    maxp = struct.pack(">IHHHHHHHHHHHHHH", 0x00010000, num_glyphs, max_pts, max_contours, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0)
    hmtx = b"".join(struct.pack(">Hh", adv, lsb) for adv, lsb in zip(advances, lsbs, strict=True))
    cmap = _ttf_cmap(mapping)
    name = _ttf_name({1: family, 2: "Regular", 4: family, 6: family})
    post = struct.pack(">IihhIIIII", 0x00030000, 0, -100, 50, 0, 0, 0, 0, 0)

    tables = {
        b"cmap": cmap,
        b"glyf": bytes(glyf),
        b"head": head,
        b"hhea": hhea,
        b"hmtx": hmtx,
        b"loca": bytes(loca),
        b"maxp": maxp,
        b"name": name,
        b"post": post,
    }
    tags = sorted(tables)
    n = len(tags)
    entry_selector = n.bit_length() - 1
    search_range = 16 * (1 << entry_selector)
    directory = bytearray(struct.pack(">IHHHH", 0x00010000, n, search_range, entry_selector, 16 * n - search_range))
    offset = 12 + 16 * n
    body = bytearray()
    for tag in tags:
        data = tables[tag]
        padded = data + b"\0" * ((4 - len(data) % 4) % 4)
        directory += tag + struct.pack(">III", _ttf_checksum(data), offset + len(body), len(data))
        body += padded
    font = bytearray(directory + body)
    adjustment = (0xB1B0AFBA - _ttf_checksum(bytes(font))) & 0xFFFFFFFF
    head_offset = 12 + 16 * tags.index(b"head")
    head_pos = struct.unpack(">I", font[head_offset + 8 : head_offset + 12])[0]
    struct.pack_into(">I", font, head_pos + 8, adjustment)
    return bytes(font)


# ---------------------------------------------------------------------------
# Code 39 条码（每个字符 5 条 4 空，n 窄 w 宽，按"条空条空…"排列）。
# ---------------------------------------------------------------------------

CODE39 = {
    "0": "nnnwwnwnn", "1": "wnnwnnnnw", "2": "nnwwnnnnw", "3": "wnwwnnnnn",
    "4": "nnnwwnnnw", "5": "wnnwwnnnn", "6": "nnwwwnnnn", "7": "nnnwnnwnw",
    "8": "wnnwnnwnn", "9": "nnwwnnwnn", "A": "wnnnnwnnw", "B": "nnwnnwnnw",
    "C": "wnwnnwnnn", "D": "nnnnwwnnw", "E": "wnnnwwnnn", "F": "nnwnwwnnn",
    "G": "nnnnnwwnw", "H": "wnnnnwwnn", "I": "nnwnnwwnn", "J": "nnnnwwwnn",
    "K": "wnnnnnnww", "L": "nnwnnnnww", "M": "wnwnnnnwn", "N": "nnnnwnnww",
    "O": "wnnnwnnwn", "P": "nnwnwnnwn", "Q": "nnnnnnwww", "R": "wnnnnnwwn",
    "S": "nnwnnnwwn", "T": "nnnnwnwwn", "U": "wwnnnnnnw", "V": "nwwnnnnnw",
    "W": "wwwnnnnnn", "X": "nwnnwnnnw", "Y": "wwnnwnnnn", "Z": "nwwnwnnnn",
    "-": "nwnnnnwnw", ".": "wwnnnnwnn", " ": "nwwnnnwnn", "*": "nwnnwnwnn",
}  # fmt: skip


def code39_bars(text: str, narrow: float, wide: float) -> list[tuple[float, float]]:
    """返回 [(x 起点, 宽度)] 的条列表，含起止符 *，字符间留一个窄空。"""
    x = 0.0
    bars: list[tuple[float, float]] = []
    for ch in f"*{text}*":
        pattern = CODE39[ch]
        for i, w in enumerate(pattern):
            width = wide if w == "w" else narrow
            if i % 2 == 0:
                bars.append((x, width))
            x += width
        x += narrow
    return bars


# ---------------------------------------------------------------------------
# 编号与 CID 换算
# ---------------------------------------------------------------------------

_ISO6346_LETTERS = {
    "A": 10, "B": 12, "C": 13, "D": 14, "E": 15, "F": 16, "G": 17, "H": 18, "I": 19, "J": 20,
    "K": 21, "L": 23, "M": 24, "N": 25, "O": 26, "P": 27, "Q": 28, "R": 29, "S": 30, "T": 31,
    "U": 32, "V": 34, "W": 35, "X": 36, "Y": 37, "Z": 38,
}  # fmt: skip


def iso6346_check_digit(prefix10: str) -> int:
    """ISO 6346 箱号校验位（《货代业务规则》1.1）。"""
    assert len(prefix10) == 10
    total = 0
    for i, ch in enumerate(prefix10):
        v = int(ch) if ch.isdigit() else _ISO6346_LETTERS[ch]
        total += v * (2**i)
    return total % 11 % 10


def adobe_gb1_cid(ch: str) -> int:
    """GB2312 字符对应的 Adobe-GB1 CID。

    Adobe-GB1 的排列：CID 1–95 为 ASCII 0x20–0x7E 的等比字形；GB2312 一级汉字（16–55 区）从
    CID 940（0xB0A1 "啊"）起按区位顺序连续编号，二级汉字（56–87 区）紧接其后。
    符号区（1–9 区）不在这里换算。
    """
    if len(ch) != 1:
        raise ValueError("one character at a time")
    if " " <= ch <= "~":
        return ord(ch) - 31
    b = ch.encode("gb2312")
    row, col = b[0] - 0xA0, b[1] - 0xA0
    if 16 <= row <= 55:
        return 940 + (row - 16) * 94 + (col - 1)
    if 56 <= row <= 87:
        return 940 + 3755 + (row - 56) * 94 + (col - 1)
    raise ValueError(f"{ch!r} is not a GB2312 hanzi")
