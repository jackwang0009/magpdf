#!/usr/bin/env python3
"""生成性能测量用的页面（《P0 技术验证计划》V04、V05；V06 报告第 12 节、V04 报告里的数字就是在这些文件上量的）。

全部是画出来的，不含任何真实文件。文件写到 tests/out/perf/（不入库）：

  a4-5000-objects.pdf    A4 一页，5000 个路径对象：4999 个小色块，最上面一个 140×70 pt 的深色块
                         （界面端到端测试拿最上面的对象来拖）
  300-pages.pdf          300 页，每页 500 个小色块
  scan-300dpi.pdf        A4 一页，一张 2480×3508 的灰度 JPEG 铺满整页，当作 300 dpi 的整页扫描件
  scan-300dpi-stamp.pdf  同上，上面再放一个深红色的方块（当作后来盖上去的章），它是最上面的对象
  scan-100mb.pdf         50 页，每页一张压不动的灰度图，文件约 100 MB（量“大文件开副本要多久”）
  report-300-pages.pdf   300 页的报表（V05“300 页文档出首屏”）：每页一张 40 行 6 列的表，320 个对象，
                         每个单元格一个文字对象，表格线一条一个对象，隔行底色，页眉一张标志图（各页共用）。
                         文字用嵌入的宋体、黑体子集（Identity-H，带 ToUnicode），报表系统导出的文件大多这样
  drawing-5000-objects.pdf  A4 横放的图纸，5000 个对象（V05“单对象修改到重绘完成”）：粗细、颜色、线型各不相同的
                         直线、折线、圆和圆弧，浅色填充的区域，螺栓孔，尺寸文字（Helvetica，四分之一竖排），
                         图框和标题栏；最上面一个 120×50 pt 的色块（界面端到端测试拿它来拖）

前五份是最容易渲染的一类页面：色块没有文字、剪切、透明；“扫描件”是纸的底色加噪声和一行行深色的条。
真实的图纸和扫描件会慢，数字只能当下限看。报表和图纸两份接近真实文件，但仍然是画出来的。

报表样本要用 Windows 自带的宋体（simsun.ttc）和黑体（simhei.ttf），字体子集由 fontTools 做
（`tests/requirements-test.txt`）。生成的文件只在本机量数字用，不入库、不分发。

除两份扫描件外，每次生成的字节完全相同（报表样本还要求字体文件和 fontTools 的版本相同）。两份扫描件里纸面的
噪声来自 Pillow 的 `effect_noise`，它不能指定种子：在同一台机器、同一版 Pillow 上每次生成的结果相同（量数字用的
就是这样生成的），换了平台或 Pillow 版本噪声可能不一样，大小和分布是一样的。

用法：python tests/corpus/perf/generate.py [--out tests/out/perf] [名字 ...]
不给名字就全部生成；名字是上面的文件名去掉 .pdf。
"""
from __future__ import annotations

import argparse
import functools
import io
import logging
import math
import os
import random
import zlib
from pathlib import Path

A4_W, A4_H = 595.276, 841.89
SCAN_W, SCAN_H = 2480, 3508  # A4，300 dpi


def write_pdf(target: Path, objects: list[bytes]) -> int:
    """对象依次编号 1、2、3……，1 号是目录。返回文件大小。"""
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % number + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n" % (len(objects) + 1)
    out += b"0000000000 65535 f \n"
    for offset in offsets:
        out += b"%010d 00000 n \n" % offset
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
    target.write_bytes(out)
    return len(out)


def stream(dictionary: bytes, data: bytes) -> bytes:
    return b"<< " + dictionary + b" /Length %d >>\nstream\n" % len(data) + data + b"\nendstream"


def a4_objects(target: Path, count: int = 5000) -> int:
    columns = 50
    rows = (count + columns - 1) // columns
    margin = 20.0
    cell_w = (A4_W - 2 * margin) / columns
    cell_h = (A4_H - 2 * margin) / rows
    parts = []
    for i in range(count - 1):
        col, row = i % columns, i // columns
        x = margin + col * cell_w
        y = A4_H - margin - (row + 1) * cell_h
        r = (col * 5 % 256) / 255
        g = (row * 2 % 256) / 255
        b = ((col + row) * 3 % 256) / 255
        parts.append(f"{r:.3f} {g:.3f} {b:.3f} rg {x:.2f} {y:.2f} {cell_w * 0.8:.2f} {cell_h * 0.8:.2f} re f")
    # 最后一个（最上面的）对象大一些，好拿来拖
    parts.append(f"0.100 0.100 0.100 rg {A4_W / 2 - 70:.2f} {A4_H / 2 - 35:.2f} 140.00 70.00 re f")
    content = zlib.compress("\n".join(parts).encode("ascii"))
    return write_pdf(
        target,
        [
            b"<< /Type /Catalog /Pages 2 0 R >>",
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {A4_W} {A4_H}] /Contents 4 0 R /Resources << >> >>".encode(),
            b"<< /Length %d /Filter /FlateDecode >>\nstream\n" % len(content) + content + b"\nendstream",
        ],
    )


def many_pages(target: Path, pages: int = 300, per_page: int = 500) -> int:
    columns = 25
    rows = (per_page + columns - 1) // columns
    cell_w, cell_h = (A4_W - 40) / columns, (A4_H - 40) / rows
    # 对象号：1 目录，2 页树，之后每页两个（页、内容流）
    objects: list[bytes] = [b"<< /Type /Catalog /Pages 2 0 R >>", b""]
    kids = []
    for page in range(pages):
        parts = []
        for i in range(per_page):
            col, row = i % columns, i // columns
            x, y = 20 + col * cell_w, A4_H - 20 - (row + 1) * cell_h
            shade = ((i + page) * 7 % 200) / 255
            parts.append(f"{shade:.3f} {shade / 2:.3f} 0.400 rg {x:.2f} {y:.2f} {cell_w * 0.8:.2f} {cell_h * 0.8:.2f} re f")
        content = zlib.compress("\n".join(parts).encode("ascii"))
        page_number = len(objects) + 1
        kids.append(b"%d 0 R" % page_number)
        objects.append(
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %.3f %.3f] /Contents %d 0 R /Resources << >> >>"
            % (A4_W, A4_H, page_number + 1)
        )
        objects.append(stream(b"/Filter /FlateDecode", content))
    objects[1] = b"<< /Type /Pages /Kids [" + b" ".join(kids) + b"] /Count %d >>" % pages
    return write_pdf(target, objects)


@functools.cache
def scan_jpeg() -> bytes:
    """两份扫描件用同一张图：只生成一次（`effect_noise` 每调一次给的噪声都不同）。"""
    from PIL import Image, ImageDraw

    rng = random.Random(20261111)
    paper = Image.new("L", (SCAN_W, SCAN_H), 236)
    # 纸面的噪声：扫描件压不到很小，解码也不轻松
    noise = Image.effect_noise((SCAN_W, SCAN_H), 18)
    page = Image.blend(paper, noise, 0.12)
    draw = ImageDraw.Draw(page)
    y = 260
    while y < SCAN_H - 300:
        x = 220
        line_end = SCAN_W - 220 - rng.randint(0, 700)
        while x < line_end:
            word = rng.randint(40, 190)
            draw.rectangle([x, y, min(x + word, line_end), y + 26], fill=rng.randint(25, 70))
            x += word + 26
        y += 58 if rng.random() > 0.08 else 130
    draw.rectangle([150, 150, SCAN_W - 150, SCAN_H - 150], outline=60, width=4)
    out = io.BytesIO()
    page.save(out, format="JPEG", quality=78)
    return out.getvalue()


def scan_page(target: Path, stamp: bool) -> int:
    jpeg = scan_jpeg()
    content = f"q {A4_W} 0 0 {A4_H} 0 0 cm /Im0 Do Q"
    if stamp:
        content += "\n0.75 0.1 0.1 rg 380 120 140 70 re f"
    content_bytes = content.encode("ascii")
    return write_pdf(
        target,
        [
            b"<< /Type /Catalog /Pages 2 0 R >>",
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            (
                f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {A4_W} {A4_H}] /Contents 4 0 R "
                f"/Resources << /XObject << /Im0 5 0 R >> >> >>"
            ).encode(),
            b"<< /Length %d >>\nstream\n" % len(content_bytes) + content_bytes + b"\nendstream",
            (
                f"<< /Type /XObject /Subtype /Image /Width {SCAN_W} /Height {SCAN_H} /ColorSpace /DeviceGray "
                f"/BitsPerComponent 8 /Filter /DCTDecode /Length {len(jpeg)} >>\nstream\n"
            ).encode()
            + jpeg
            + b"\nendstream",
        ],
    )


def big_scan(target: Path, pages: int = 50, image_bytes: int = 2_000_000) -> int:
    rng = random.Random(20261111)
    side = int(image_bytes**0.5)
    objects: list[bytes] = [b"<< /Type /Catalog /Pages 2 0 R >>", b""]
    kids = []
    for _ in range(pages):
        page_number = len(objects) + 1
        kids.append(b"%d 0 R" % page_number)
        objects.append(
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents %d 0 R "
            b"/Resources << /XObject << /Im0 %d 0 R >> >> >>" % (page_number + 1, page_number + 2)
        )
        objects.append(stream(b"", b"q 555 0 0 802 20 20 cm /Im0 Do Q"))
        # 随机数据压不动，文件里就是这么大
        objects.append(
            stream(
                b"/Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray /BitsPerComponent 8"
                % (side, side),
                rng.randbytes(side * side),
            )
        )
    objects[1] = b"<< /Type /Pages /Kids [" + b" ".join(kids) + b"] /Count %d >>" % pages
    return write_pdf(target, objects)


FONTS = Path(os.environ.get("WINDIR", r"C:\Windows")) / "Fonts"


class EmbeddedFont:
    """系统字体里用到的那些字做成的子集，按 Identity-H 嵌入：字形号就是 CID，字符串里每个字两个字节。"""

    def __init__(self, path: Path, base_name: str, tag: str, text: str) -> None:
        from fontTools.subset import Options, Subsetter
        from fontTools.ttLib import TTFont

        if not path.exists():
            raise SystemExit(f"找不到字体 {path}：报表样本要用 Windows 自带的宋体和黑体")
        # 子集里用不上的表（字形替换、元数据）它会逐个报一句“丢掉了”
        logging.getLogger("fontTools.subset").setLevel(logging.ERROR)
        font = TTFont(path, fontNumber=0)
        options = Options()
        # 只要轮廓：小字号用的点阵字形和数字签名 PDF 里用不上
        options.drop_tables += ["EBDT", "EBLC", "EBSC", "DSIG"]
        subsetter = Subsetter(options)
        subsetter.populate(text=text)
        subsetter.subset(font)
        out = io.BytesIO()
        font.save(out)
        self.data = out.getvalue()
        # 字形号和宽度从存下来的子集里读，和嵌进去的字体一致
        font = TTFont(io.BytesIO(self.data))
        scale = 1000 / font["head"].unitsPerEm
        cmap = font.getBestCmap()
        missing = sorted({ch for ch in text if ord(ch) not in cmap})
        if missing:
            raise SystemExit(f"{path.name} 里没有这些字：{''.join(missing)}")
        self.glyphs = {ch: font.getGlyphID(cmap[ord(ch)]) for ch in set(text)}
        order = font.getGlyphOrder()
        self.widths = {
            gid: round(font["hmtx"][order[gid]][0] * scale) for gid in self.glyphs.values()
        }
        head = font["head"]
        self.bbox = [round(v * scale) for v in (head.xMin, head.yMin, head.xMax, head.yMax)]
        self.ascent = round(font["hhea"].ascent * scale)
        self.descent = round(font["hhea"].descent * scale)
        self.name = f"{tag}+{base_name}"

    def hex(self, text: str) -> str:
        return "".join(f"{self.glyphs[ch]:04X}" for ch in text)

    def width(self, text: str, size: float) -> float:
        return sum(self.widths[self.glyphs[ch]] for ch in text) * size / 1000

    def objects(self, first: int) -> list[bytes]:
        """字体的五个对象，编号从 `first` 起：Type0 字体、CID 字体、字体描述、字体文件、ToUnicode。"""
        cid, descriptor, program, to_unicode = first + 1, first + 2, first + 3, first + 4
        widths = " ".join(f"{gid} [{w}]" for gid, w in sorted(self.widths.items()))
        unicode_of = sorted((gid, ch) for ch, gid in self.glyphs.items())
        blocks = []
        for start in range(0, len(unicode_of), 100):
            chunk = unicode_of[start : start + 100]
            lines = "\n".join(f"<{gid:04X}> <{ord(ch):04X}>" for gid, ch in chunk)
            blocks.append(f"{len(chunk)} beginbfchar\n{lines}\nendbfchar")
        cmap = (
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n"
            "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n"
            "/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n"
            "1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n"
            + "\n".join(blocks)
            + "\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend"
        ).encode("ascii")
        packed = zlib.compress(self.data)
        return [
            (
                f"<< /Type /Font /Subtype /Type0 /BaseFont /{self.name} /Encoding /Identity-H "
                f"/DescendantFonts [{cid} 0 R] /ToUnicode {to_unicode} 0 R >>"
            ).encode(),
            (
                f"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{self.name} "
                f"/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> "
                f"/FontDescriptor {descriptor} 0 R /DW 1000 /W [{widths}] /CIDToGIDMap /Identity >>"
            ).encode(),
            (
                f"<< /Type /FontDescriptor /FontName /{self.name} /Flags 4 "
                f"/FontBBox [{' '.join(map(str, self.bbox))}] /ItalicAngle 0 /Ascent {self.ascent} "
                f"/Descent {self.descent} /CapHeight {self.ascent} /StemV 80 /FontFile2 {program} 0 R >>"
            ).encode(),
            b"<< /Length %d /Length1 %d /Filter /FlateDecode >>\nstream\n" % (len(packed), len(self.data))
            + packed
            + b"\nendstream",
            stream(b"/Filter /FlateDecode", zlib.compress(cmap)),
        ]


def logo_jpeg() -> bytes:
    """页眉的标志：240×72 的彩色图。"""
    from PIL import Image, ImageDraw

    image = Image.new("RGB", (240, 72), (255, 255, 255))
    draw = ImageDraw.Draw(image)
    for x in range(240):
        draw.line([(x, 0), (x, 71)], fill=(20 + x // 3, 70 + x // 4, 160))
    draw.ellipse([10, 8, 66, 64], fill=(240, 180, 30))
    draw.rectangle([84, 22, 228, 50], fill=(255, 255, 255))
    out = io.BytesIO()
    image.save(out, format="JPEG", quality=85)
    return out.getvalue()


REPORT_COLUMNS = (
    ("序号", 40.0),
    ("提单号", 110.0),
    ("客户", 130.0),
    ("目的港", 90.0),
    ("件数", 60.0),
    ("金额（元）", 93.276),
)
CUSTOMERS = (
    "华东国际物流有限公司",
    "远洋货运代理",
    "恒通贸易有限公司",
    "联运供应链管理",
    "瑞丰进出口",
    "海天国际货代",
    "宏达电子科技",
    "嘉禾纺织品进出口",
)
PORTS = ("洛杉矶", "长滩", "鹿特丹", "汉堡", "安特卫普", "新加坡", "釜山", "迪拜", "悉尼", "温哥华", "费利克斯托", "巴生港")


def report(target: Path, pages: int = 300, rows: int = 40) -> int:
    rng = random.Random(20261112)
    title = "海运出口货物明细表"
    printed = "打印日期：2026-11-12"
    table = [
        [
            [
                str(page * rows + row + 1),
                f"COSU{rng.randrange(10**9, 10**10)}",
                rng.choice(CUSTOMERS),
                rng.choice(PORTS),
                str(rng.randint(1, 980)),
                f"{rng.randint(80_000, 99_000_000) / 100:,.2f}",
            ]
            for row in range(rows)
        ]
        for page in range(pages)
    ]
    footers = [f"第 {page + 1} 页  共 {pages} 页" for page in range(pages)]
    body_text = printed + "".join(footers) + "".join(c for page in table for row in page for c in row)
    heading_text = title + "".join(name for name, _ in REPORT_COLUMNS)
    body = EmbeddedFont(FONTS / "simsun.ttc", "SimSun", "MAGPAA", body_text)
    heading = EmbeddedFont(FONTS / "simhei.ttf", "SimHei", "MAGPAB", heading_text)

    # 对象号：1 目录，2 页树，3–7 正文字体，8–12 标题字体，13 标志图，14 各页共用的资源，之后每页两个（页、内容流）
    objects: list[bytes] = [b"<< /Type /Catalog /Pages 2 0 R >>", b""]
    objects += body.objects(3)
    objects += heading.objects(8)
    logo = logo_jpeg()
    objects.append(
        b"<< /Type /XObject /Subtype /Image /Width 240 /Height 72 /ColorSpace /DeviceRGB "
        b"/BitsPerComponent 8 /Filter /DCTDecode /Length %d >>\nstream\n" % len(logo)
        + logo
        + b"\nendstream"
    )
    objects.append(b"<< /Font << /F1 3 0 R /F2 8 0 R >> /XObject << /Im1 13 0 R >> >>")
    left = 36.0
    table_width = sum(width for _, width in REPORT_COLUMNS)
    header_h, row_h = 18.0, 16.0
    top = A4_H - 92
    bottom = top - header_h - rows * row_h
    kids = []
    for page, page_rows in enumerate(table):
        parts = [f"q 96 0 0 28.8 {left:.2f} {A4_H - 66:.2f} cm /Im1 Do Q"]
        x = (A4_W - heading.width(title, 16)) / 2
        parts.append(f"0 0 0 rg BT /F2 16 Tf 1 0 0 1 {x:.2f} {A4_H - 58:.2f} Tm <{heading.hex(title)}> Tj ET")
        x = left + table_width - body.width(printed, 9)
        parts.append(f"BT /F1 9 Tf 1 0 0 1 {x:.2f} {top + 8:.2f} Tm <{body.hex(printed)}> Tj ET")
        # 表头和隔行的底色
        parts.append(f"0.85 0.89 0.95 rg {left:.2f} {top - header_h:.2f} {table_width:.2f} {header_h:.2f} re f")
        for row in range(1, rows, 2):
            y = top - header_h - (row + 1) * row_h
            parts.append(f"0.95 0.96 0.98 rg {left:.2f} {y:.2f} {table_width:.2f} {row_h:.2f} re f")
        # 表格线：一条一个对象
        parts.append("0.5 w 0.3 0.3 0.3 RG")
        for y in [top, top - header_h] + [top - header_h - (row + 1) * row_h for row in range(rows)]:
            parts.append(f"{left:.2f} {y:.2f} m {left + table_width:.2f} {y:.2f} l S")
        x = left
        for _, width in REPORT_COLUMNS + (("", 0.0),):
            parts.append(f"{x:.2f} {top:.2f} m {x:.2f} {bottom:.2f} l S")
            x += width
        # 表头居中；单元格里数字靠右，别的靠左。每格一个文字对象
        parts.append("0 0 0 rg")
        x = left
        for name, width in REPORT_COLUMNS:
            tx = x + (width - heading.width(name, 9)) / 2
            parts.append(f"BT /F2 9 Tf 1 0 0 1 {tx:.2f} {top - 13:.2f} Tm <{heading.hex(name)}> Tj ET")
            x += width
        for row, cells in enumerate(page_rows):
            y = top - header_h - (row + 1) * row_h + 4.5
            x = left
            for column, (cell, (_, width)) in enumerate(zip(cells, REPORT_COLUMNS)):
                tx = x + width - 4 - body.width(cell, 9) if column in (0, 4, 5) else x + 4
                parts.append(f"BT /F1 9 Tf 1 0 0 1 {tx:.2f} {y:.2f} Tm <{body.hex(cell)}> Tj ET")
                x += width
        footer = footers[page]
        x = (A4_W - body.width(footer, 9)) / 2
        parts.append(f"BT /F1 9 Tf 1 0 0 1 {x:.2f} 30 Tm <{body.hex(footer)}> Tj ET")
        content = zlib.compress("\n".join(parts).encode("ascii"))
        number = len(objects) + 1
        kids.append(b"%d 0 R" % number)
        objects.append(
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %.3f %.3f] /Resources 14 0 R /Contents %d 0 R >>"
            % (A4_W, A4_H, number + 1)
        )
        objects.append(stream(b"/Filter /FlateDecode", content))
    objects[1] = b"<< /Type /Pages /Kids [" + b" ".join(kids) + b"] /Count %d >>" % pages
    return write_pdf(target, objects)


PEN_WIDTHS = ("0.18", "0.25", "0.35", "0.5", "0.7")
PEN_COLORS = ("0 0 0", "0 0 0", "0 0 0", "0 0 0.75", "0.8 0 0", "0 0.5 0", "0.45 0.45 0.45")
# 实线多，虚线（隐藏线）和点划线（中心线）少
PEN_DASHES = ("[] 0",) * 6 + ("[3 1.5] 0", "[6 2 1.5 2] 0")
FILLS = ("0.85 0.92 1", "1 0.93 0.85", "0.9 0.97 0.9", "0.93 0.93 0.93")


def arc(cx: float, cy: float, r: float, start: float, quarters: int) -> str:
    """从 `start`（弧度）起逆时针画 `quarters` 段四分之一圆弧，每段一条三次贝塞尔曲线。"""
    k = 0.5523 * r
    x0, y0 = cx + r * math.cos(start), cy + r * math.sin(start)
    out = [f"{x0:.2f} {y0:.2f} m"]
    t0 = start
    for _ in range(quarters):
        t1 = t0 + math.pi / 2
        x1, y1 = cx + r * math.cos(t1), cy + r * math.sin(t1)
        c1 = (x0 - k * math.sin(t0), y0 + k * math.cos(t0))
        c2 = (x1 + k * math.sin(t1), y1 - k * math.cos(t1))
        out.append(f"{c1[0]:.2f} {c1[1]:.2f} {c2[0]:.2f} {c2[1]:.2f} {x1:.2f} {y1:.2f} c")
        x0, y0, t0 = x1, y1, t1
    return " ".join(out)


def drawing(target: Path, count: int = 5000) -> int:
    width, height = A4_H, A4_W  # 横放
    margin = 28.0
    rng = random.Random(20261113)
    parts: list[str] = []
    state: dict[str, str] = {}
    made = 0

    def put(op: str, value: str) -> None:
        """图形状态变了才写，导出程序都这样。"""
        if state.get(op) != value:
            state[op] = value
            parts.append(f"{value} {op}")

    def paint(path: str) -> None:
        nonlocal made
        parts.append(path)
        made += 1

    def anywhere(pad: float = 10.0) -> tuple[float, float]:
        return (
            rng.uniform(margin + pad, width - margin - pad),
            rng.uniform(margin + pad, height - margin - pad),
        )

    def pen() -> tuple[str, str, str]:
        return rng.choice(PEN_WIDTHS), rng.choice(PEN_COLORS), rng.choice(PEN_DASHES)

    def stroke_all(paths: list[tuple[tuple[str, str, str], str]]) -> None:
        # 按笔分组画（相当于按图层），状态切换少
        for (line_width, color, dash), path in sorted(paths):
            put("w", line_width)
            put("RG", color)
            put("d", dash)
            paint(f"{path} S")

    # 浅色填充的区域
    for _ in range(250):
        cx, cy = anywhere(40)
        size = rng.uniform(12, 50)
        angles = sorted(rng.uniform(0, 2 * math.pi) for _ in range(rng.choice((3, 4, 5))))
        points = [(cx + size * math.cos(a), cy + size * math.sin(a)) for a in angles]
        put("rg", rng.choice(FILLS))
        path = [f"{points[0][0]:.2f} {points[0][1]:.2f} m"] + [f"{x:.2f} {y:.2f} l" for x, y in points[1:]]
        paint(" ".join(path) + " h f")
    # 直线：图框、标题栏、填充、圆、弧、折线、螺栓孔、尺寸文字和最上面的色块之外，剩下的都是直线
    lines = []
    for _ in range(count - 1867):
        x, y = anywhere()
        length = rng.uniform(8, 140)
        angle = math.radians(rng.choice(range(0, 180, 15)))
        x2 = min(max(x + length * math.cos(angle), margin), width - margin)
        y2 = min(max(y + length * math.sin(angle), margin), height - margin)
        lines.append((pen(), f"{x:.2f} {y:.2f} m {x2:.2f} {y2:.2f} l"))
    stroke_all(lines)
    curves = []
    for _ in range(450):
        cx, cy = anywhere(32)
        curves.append((pen(), arc(cx, cy, rng.uniform(2, 30), 0, 4)))
    for _ in range(150):
        cx, cy = anywhere(32)
        curves.append((pen(), arc(cx, cy, rng.uniform(5, 30), rng.randrange(8) * math.pi / 4, 2)))
    stroke_all(curves)
    outlines = []
    for _ in range(400):
        x, y = anywhere(60)
        path = [f"{x:.2f} {y:.2f} m"]
        for step in range(rng.randint(4, 10)):
            delta = rng.uniform(5, 40) * rng.choice((-1, 1))
            if step % 2 == 0:
                x = min(max(x + delta, margin), width - margin)
            else:
                y = min(max(y + delta, margin), height - margin)
            path.append(f"{x:.2f} {y:.2f} l")
        outlines.append((pen(), " ".join(path)))
    stroke_all(outlines)
    # 螺栓孔
    put("rg", "0 0 0")
    for _ in range(200):
        cx, cy = anywhere(10)
        paint(arc(cx, cy, rng.uniform(1.2, 3), 0, 4) + " f")
    # 尺寸文字，四分之一竖排
    for i in range(400):
        x, y = anywhere(20)
        label = rng.choice(
            (
                f"{rng.uniform(4, 900):.1f}",
                f"R{rng.uniform(1, 60):.1f}",
                f"{rng.randint(2, 12)}x{rng.uniform(3, 20):.1f}",
            )
        )
        matrix = "0 1 -1 0" if i % 4 == 0 else "1 0 0 1"
        paint(f"BT /F1 {rng.choice((5, 6, 7))} Tf {matrix} {x:.2f} {y:.2f} Tm ({label}) Tj ET")
    # 图框和标题栏
    put("w", "1")
    put("RG", "0 0 0")
    put("d", "[] 0")
    paint(f"{margin:.2f} {margin:.2f} {width - 2 * margin:.2f} {height - 2 * margin:.2f} re S")
    inner = margin + 6
    paint(f"{inner:.2f} {inner:.2f} {width - 2 * inner:.2f} {height - 2 * inner:.2f} re S")
    bw, bh = 250.0, 60.0
    bx, by = width - inner - bw, inner
    put("w", "0.5")
    for y in (by + 20, by + 40, by + bh):
        paint(f"{bx:.2f} {y:.2f} m {bx + bw:.2f} {y:.2f} l S")
    for x in (bx, bx + 90, bx + 170):
        paint(f"{x:.2f} {by:.2f} m {x:.2f} {by + bh:.2f} l S")
    titles = (
        (bx + 4, by + 46, 8, "MAGPDF PERF DRAWING"),
        (bx + 94, by + 46, 7, "SCALE 1:20"),
        (bx + 174, by + 46, 7, "SHEET 1/1"),
        (bx + 4, by + 26, 7, "DRAWN"),
        (bx + 94, by + 26, 7, "CHECKED"),
        (bx + 174, by + 26, 7, "REV A"),
        (bx + 4, by + 6, 7, "PART NO. MP-0005"),
        (bx + 94, by + 6, 7, "DATE 2026-11-12"),
    )
    for x, y, size, text in titles:
        paint(f"BT /F1 {size} Tf 1 0 0 1 {x:.2f} {y:.2f} Tm ({text}) Tj ET")
    # 最上面一个色块：界面端到端测试拿它来拖
    put("w", "1.5")
    put("RG", "0.8 0 0")
    put("rg", "0.98 0.86 0.25")
    paint(f"{width / 2 - 60:.2f} {height / 2 - 25:.2f} 120 50 re B")
    assert made == count, f"画了 {made} 个对象，应当是 {count} 个"
    content = zlib.compress("\n".join(parts).encode("ascii"))
    return write_pdf(
        target,
        [
            b"<< /Type /Catalog /Pages 2 0 R >>",
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            (
                f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Contents 4 0 R "
                f"/Resources << /Font << /F1 5 0 R >> >> >>"
            ).encode(),
            b"<< /Length %d /Filter /FlateDecode >>\nstream\n" % len(content) + content + b"\nendstream",
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        ],
    )


SAMPLES = {
    "a4-5000-objects": a4_objects,
    "300-pages": many_pages,
    "scan-300dpi": lambda target: scan_page(target, stamp=False),
    "scan-300dpi-stamp": lambda target: scan_page(target, stamp=True),
    "scan-100mb": big_scan,
    "report-300-pages": report,
    "drawing-5000-objects": drawing,
}


def main() -> None:
    repo = Path(__file__).resolve().parents[3]
    parser = argparse.ArgumentParser(description="生成性能测量用的页面")
    parser.add_argument("names", nargs="*", metavar="名字", help="、".join(SAMPLES))
    parser.add_argument("--out", type=Path, default=repo / "tests" / "out" / "perf")
    args = parser.parse_args()
    unknown = [name for name in args.names if name not in SAMPLES]
    if unknown:
        parser.error(f"没有这个样本：{'、'.join(unknown)}")
    args.out.mkdir(parents=True, exist_ok=True)
    for name in args.names or SAMPLES:
        target = args.out / f"{name}.pdf"
        size = SAMPLES[name](target)
        print(f"{target}: {size / 1_000_000:.2f} MB")


if __name__ == "__main__":
    main()
