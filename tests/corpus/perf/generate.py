#!/usr/bin/env python3
"""生成性能测量用的页面（《P0 技术验证计划》V04、V05；V06 报告第 12 节、V04 报告里的数字就是在这些文件上量的）。

全部是画出来的，不含任何真实文件。文件写到 tests/out/perf/（不入库）：

  a4-5000-objects.pdf    A4 一页，5000 个路径对象：4999 个小色块，最上面一个 140×70 pt 的深色块
                         （界面端到端测试拿最上面的对象来拖）
  300-pages.pdf          300 页，每页 500 个小色块
  scan-300dpi.pdf        A4 一页，一张 2480×3508 的灰度 JPEG 铺满整页，当作 300 dpi 的整页扫描件
  scan-300dpi-stamp.pdf  同上，上面再放一个深红色的方块（当作后来盖上去的章），它是最上面的对象
  scan-100mb.pdf         50 页，每页一张压不动的灰度图，文件约 100 MB（量“大文件开副本要多久”）

这些是最容易渲染的一类页面：色块没有文字、剪切、透明；“扫描件”是纸的底色加噪声和一行行深色的条。
真实的图纸和扫描件会慢，数字只能当下限看。

a4-5000-objects、300-pages、scan-100mb 每次生成的字节完全相同。两份扫描件里纸面的噪声来自 Pillow 的
`effect_noise`，它不能指定种子：在同一台机器、同一版 Pillow 上每次生成的结果相同（量数字用的就是这样生成的），
换了平台或 Pillow 版本噪声可能不一样，大小和分布是一样的。

用法：python tests/corpus/perf/generate.py [--out tests/out/perf] [名字 ...]
不给名字就全部生成；名字是上面的文件名去掉 .pdf。
"""
from __future__ import annotations

import argparse
import functools
import io
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


SAMPLES = {
    "a4-5000-objects": a4_objects,
    "300-pages": many_pages,
    "scan-300dpi": lambda target: scan_page(target, stamp=False),
    "scan-300dpi-stamp": lambda target: scan_page(target, stamp=True),
    "scan-100mb": big_scan,
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
