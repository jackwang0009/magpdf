"""文件结构与页面结构样本（方案 2.1 第 6 条、2.3 生成规则、9.4/10.6 不打印图层、文件级结构）。"""

from __future__ import annotations

import zlib

from common import Content, Doc, page_expect, sample
from pdfgen import nums

REF_STREAMS = ["方案 2.1 层级与内容流", "方案 2.3 第 5 条", "P0 计划 V03 第 6 项"]
REF_CLIPGROUP = ["方案 2.3 第 2 条 剪切路径共用", "方案 4.3 对象很多的页面"]
REF_MC = ["方案 2.3 第 4 条 标记内容嵌套", "方案 4.1 对象", "方案 11.4 组件存储"]
REF_FILE = ["方案 13.2 第 4 项 结构校验", "方案 9.1 保存方式", "P0 计划 V17 第 1 条"]
REF_OCG = ["方案 9.4 打印", "方案 10.6 套打", "P0 计划 V16", "方案 2.3 像素比对打开所有图层"]
REF_PAGE = ["方案 4.1 对象", "P0 计划 V02 渲染比对"]

# ---------------------------------------------------------------------------
# 多内容流
# ---------------------------------------------------------------------------


@sample(
    "struct-multi-stream-q",
    "structure",
    "多内容流：q/剪切在第一条流，Q 在后面的流；操作数跨流拆开",
    ["multi-stream", "contents-array", "q-Q-split", "clip", "operand-split"],
    "/Contents 是 3 条流：流 1 画红块并留下未闭合的 q + 剪切；流 2 画被该剪切限制的蓝块、Q、并以 '220 120 50' 三个操作数结尾；"
    "流 3 以 '60 re f' 开头接上绿块。渲染：红块、剪切后的蓝带、绿块都完整。往返后 3 条流被合并或逐条重写时都不能丢对象、不能错剪切。",
    REF_STREAMS,
)
def struct_multi_stream_q():
    d = Doc("struct-multi-stream-q")
    f1 = d.std_font()
    s1 = b"q 1 0 0 rg 20 120 80 60 re f q 20 20 260 90 re W n"
    s2 = b"0 0 1 rg 0 0 300 200 re f Q 0 1 0 rg 220 120 50"
    s3 = b"60 re f Q BT /F1 7 Tf 0 g 10 188 Td (struct-multi-stream-q: 3 streams, q/clip in #1, Q in #2, operands split #2/#3) Tj ET"
    d.page([s1, s2, s3], f"/Font << /F1 {f1} >>")
    return d.build(), [page_expect(path=3, text=1)]


@sample(
    "struct-multi-stream-bt",
    "structure",
    "多内容流：文字块 BT…ET 跨三条流",
    ["multi-stream", "contents-array", "BT-ET-split", "TJ"],
    "BT 和第一行在流 1，第二行与一个 TJ 在流 2，第三行和 ET 在流 3，之后是一条黑带。4 个文字对象、1 个路径。"
    "跨流的文字块在重写时必须保持为一个 BT…ET 组，否则第二、三行位置（相对 Td）错位。",
    REF_STREAMS,
)
def struct_multi_stream_bt():
    d = Doc("struct-multi-stream-bt")
    f1 = d.std_font()
    s1 = b"BT /F1 16 Tf 1 0 0 rg 20 150 Td (Text starts in stream 1) Tj"
    s2 = b"0 -30 Td 0 0 1 rg (continues in stream 2) Tj [(and TJ in) -500 (stream 2)] TJ"
    s3 = b"0 -30 Td 0 g (ends in stream 3) Tj ET 0 g 20 20 260 20 re f"
    d.page([s1, s2, s3], f"/Font << /F1 {f1} >>")
    return d.build(), [page_expect(text=4, path=1)]


# ---------------------------------------------------------------------------
# Form XObject
# ---------------------------------------------------------------------------


@sample(
    "struct-form-xobject-nested",
    "structure",
    "两层嵌套的 Form XObject，带 Matrix 与 BBox 剪切",
    ["form-xobject", "nested", "bbox-clip", "matrix"],
    "外层 Form（蓝底）内部缩放绘制内层 Form（红底黄心，且故意画出 BBox 之外的部分应被裁掉）；外层以两种 cm 画两次。"
    "页面对象是 2 个 Form，内部对象不计入页面统计；往返后 Form 内容流应原样。",
    REF_PAGE + ["方案 2.1 向 Form XObject 插入对象"],
)
def struct_form_xobject_nested():
    d = Doc("struct-form-xobject-nested")
    f1 = d.std_font()
    inner = d.form_xobject(
        (0, 0, 100, 100),
        b"1 0 0 rg -30 -30 160 160 re f 1 1 0 rg 25 25 50 50 re f",
    )
    outer = d.form_xobject(
        (0, 0, 100, 100),
        b"0 0 1 rg 0 0 100 100 re f q 0.5 0 0 0.5 25 25 cm /Fx2 Do Q",
        resources=f"/XObject << /Fx2 {inner} >>",
    )
    c = Content()
    c.form("q 1 0 0 1 20 50 cm /Fx1 Do Q")
    c.form("q 1.2 0 0 1.2 150 40 cm /Fx1 Do Q")
    c.label("struct-form-xobject-nested: outer form x2, inner form scaled 0.5, inner overflow clipped by BBox")
    d.page(c.data(), f"/Font << /F1 {f1} >> /XObject << /Fx1 {outer} >>")
    return d.build(), [c.expect_page()]


@sample(
    "struct-form-xobject-reused",
    "structure",
    "同一个 Form XObject 以不同矩阵绘制三次",
    ["form-xobject", "reuse", "matrix", "rotate"],
    "一个'印章'Form（圆形路径 + 文字）平移、旋转 30°、缩放 0.6 各画一次。编辑其中一个实例时不能影响其他两个——"
    "对象模型里是 3 个 Form 对象共用一个流。",
    REF_PAGE + ["方案 4.2 命令与撤销"],
)
def struct_form_xobject_reused():
    d = Doc("struct-form-xobject-reused")
    f1 = d.std_font("Helvetica-Bold")
    k = 0.5523 * 40
    circle = (
        f"80 40 m 80 {40 + k:.2f} {40 + k:.2f} 80 40 80 c {40 - k:.2f} 80 0 {40 + k:.2f} 0 40 c "
        f"0 {40 - k:.2f} {40 - k:.2f} 0 40 0 c {40 + k:.2f} 0 80 {40 - k:.2f} 80 40 c h"
    )
    stamp = d.form_xobject(
        (0, 0, 80, 80),
        f"0.8 0 0 RG 3 w {circle} S BT /F1 12 Tf 0.8 0 0 rg 16 34 Td (SAMPLE) Tj ET".encode("latin-1"),
        resources=f"/Font << /F1 {f1} >>",
    )
    c = Content()
    c.form("q 1 0 0 1 20 100 cm /St Do Q")
    c.form("q 0.866 0.5 -0.5 0.866 150 90 cm /St Do Q")
    c.form("q 0.6 0 0 0.6 220 20 cm /St Do Q")
    c.label("struct-form-xobject-reused: same form, 3 matrices", font="F1")
    d.page(c.data(), f"/Font << /F1 {f1} >> /XObject << /St {stamp} >>")
    return d.build(), [c.expect_page()]


# ---------------------------------------------------------------------------
# 图像
# ---------------------------------------------------------------------------


@sample(
    "struct-inline-image",
    "structure",
    "内联图像 BI/ID/EI：灰度 AHx、原始 RGB（数据含 'EI '）、图像蒙版、命名颜色空间、多重过滤器、Interpolate",
    ["inline-image", "BI-ID-EI", "AHx", "Fl", "image-mask", "indexed", "filter-chain"],
    "6 个 60×40 的内联图像：第 2 个的像素数据恰好含字节 'EI '，解析器必须按尺寸读而不是搜索 EI；"
    "第 3 个是用填充色（深绿）画的 1 位蒙版；第 4 个用页面资源里的命名 Indexed 颜色空间；第 5 个过滤器链 AHx→Fl。"
    "往返后内联图像应原样写回（数据、缩写键名、过滤器）。",
    REF_PAGE + ["方案 2.1 公共 API 缺少", "P0 计划 V17 内联图像对应一个对象"],
)
def struct_inline_image():
    d = Doc("struct-inline-image")
    f1 = d.std_font()
    cs_idx = d.add("[/Indexed /DeviceRGB 1 <FF8800 0044AA>]")
    gray = bytes([0, 64, 128, 255, 64, 128, 255, 0, 128, 255, 0, 64, 255, 0, 64, 128])
    rgb_raw = bytes([255, 0, 0, 0, 255, 0, 0x45, 0x49, 0x20, 0, 0, 255])  # 第 3 个像素是 "EI "
    mask8 = bytes([0b11111111, 0b10000001, 0b10111101, 0b10100101, 0b10100101, 0b10111101, 0b10000001, 0b11111111])
    idx_rows = bytes([0b10100000, 0b01010000, 0b10100000, 0b01010000])  # 4×4、1 bpc 的索引（棋盘）
    rgb4 = bytes(v for i in range(16) for v in ((i * 16) & 255, 255 - (i * 16) & 255, 128))
    chain = zlib.compress(rgb4, 9).hex().upper()
    c = Content()
    c.image(f"q 60 0 0 40 20 140 cm BI /W 4 /H 4 /CS /G /BPC 8 /F /AHx ID\n{gray.hex().upper()} >\nEI Q")
    c.image(b"q 60 0 0 40 100 140 cm BI /W 2 /H 2 /CS /RGB /BPC 8 ID\n".decode("latin-1") + rgb_raw.decode("latin-1") + "\nEI Q")
    c.image(f"q 0 0.4 0 rg 60 0 0 40 180 140 cm BI /W 8 /H 8 /IM true /BPC 1 /D [0 1] /F /AHx ID\n{mask8.hex().upper()} >\nEI Q")
    c.image(f"q 60 0 0 40 20 60 cm BI /W 4 /H 4 /CS /CSidx /BPC 1 /F /AHx ID\n{idx_rows.hex().upper()} >\nEI Q")
    c.image(f"q 60 0 0 40 100 60 cm BI /W 4 /H 4 /CS /RGB /BPC 8 /F [/AHx /Fl] ID\n{chain} >\nEI Q")
    c.image(f"q 60 0 0 40 180 60 cm BI /W 4 /H 4 /CS /G /BPC 8 /I true /F /AHx ID\n{gray.hex().upper()} >\nEI Q")
    c.label("struct-inline-image: gray AHx | raw RGB containing 'EI ' | 1-bit mask | named Indexed CS | AHx+Fl | Interpolate", y=40)
    d.page(c.data(), f"/Font << /F1 {f1} >> /ColorSpace << /CSidx {cs_idx} >>")
    return d.build(), [c.expect_page()]


@sample(
    "struct-image-xobject-masks",
    "structure",
    "图像 XObject：SMask、颜色键 Mask、模板蒙版、16 位、Decode 反相",
    ["image", "smask", "color-key-mask", "stencil-mask", "16-bit", "decode"],
    "斜向灰带作底；5 个图像：带 SMask 的 RGB（左侧透明右侧不透明）、颜色键蒙版（纯蓝像素透明）、1 位模板蒙版（用蓝色填充色）、"
    "16 位灰度、/Decode 反相的 RGB。往返后 SMask/Mask 引用、Decode 数组都应保留。",
    REF_PAGE,
    pdf_version="1.5",
)
def struct_image_xobject_masks():
    d = Doc("struct-image-xobject-masks", version="1.5")
    f1 = d.std_font()
    rgb = bytes(v for y in range(4) for x in range(4) for v in (255 if (x + y) % 2 else 0, 0, 255 if (x + y) % 2 == 0 else 0))
    smask = d.image_xobject(4, 4, "/DeviceGray", 8, bytes(v for _y in range(4) for v in (0, 85, 170, 255)))
    im1 = d.image_xobject(4, 4, "/DeviceRGB", 8, rgb, extra=f"/SMask {smask}")
    im2 = d.image_xobject(4, 4, "/DeviceRGB", 8, rgb, extra="/Mask [0 0 0 0 250 255]")
    im3 = d.image_xobject(
        8, 8, "", 1, bytes([0b11111111, 0b10000001, 0b10111101, 0b10100101, 0b10100101, 0b10111101, 0b10000001, 0b11111111]),
        extra="/ImageMask true /Decode [0 1]",
    )
    g16 = b"".join(v.to_bytes(2, "big") for v in (0, 21845, 43690, 65535))
    im4 = d.image_xobject(2, 2, "/DeviceGray", 16, g16)
    im5 = d.image_xobject(4, 4, "/DeviceRGB", 8, rgb, extra="/Decode [1 0 1 0 1 0]")
    c = Content()
    c.path("0.85 g 0 60 m 300 110 l 300 140 l 0 90 l h f")
    c.image("q 50 0 0 50 15 120 cm /Im1 Do Q")
    c.image("q 50 0 0 50 75 120 cm /Im2 Do Q")
    c.image("q 0 0 0.8 rg 50 0 0 50 135 120 cm /Im3 Do Q")
    c.image("q 50 0 0 50 195 120 cm /Im4 Do Q")
    c.image("q 50 0 0 50 15 40 cm /Im5 Do Q")
    c.label("struct-image-xobject-masks: SMask | colour-key /Mask | stencil (blue) | 16-bit gray | /Decode inverted", y=25)
    d.page(
        c.data(),
        f"/Font << /F1 {f1} >> /XObject << /Im1 {im1} /Im2 {im2} /Im3 {im3} /Im4 {im4} /Im5 {im5} >>",
    )
    return d.build(), [c.expect_page()]


# ---------------------------------------------------------------------------
# 大量对象、标记内容
# ---------------------------------------------------------------------------

MANY_COLS, MANY_ROWS = 50, 30


@sample(
    "struct-many-objects-clip",
    "structure",
    "同一剪切路径下 1500 个小矩形",
    ["many-objects", "clip", "shared-clip", "performance"],
    f"一个 q…剪切…Q 组里 {MANY_COLS * MANY_ROWS} 个 4×4 的小方块（每行换一次颜色），内容流用 Flate 压缩。"
    "按方案 2.3 第 2 条应合为一组、剪切只写一次；若生成器给每个对象重复写剪切，文件会膨胀几十倍，可用文件大小直接看出。",
    REF_CLIPGROUP,
)
def struct_many_objects_clip():
    d = Doc("struct-many-objects-clip")
    f1 = d.std_font()
    c = Content()
    c.raw("q 20 20 260 160 re W n")
    for r in range(MANY_ROWS):
        shade = r / (MANY_ROWS - 1)
        c.raw(f"{nums(0.2 + 0.6 * shade, 0.3, 0.8 - 0.6 * shade)} rg")
        for col in range(MANY_COLS):
            x = 20 + col * 5.2
            y = 20 + r * 5.3
            c.path(f"{nums(x, y)} 4 4 re f")
    c.raw("Q")
    c.label(f"struct-many-objects-clip: {MANY_COLS * MANY_ROWS} rects under one clip")
    d.page(c.data(), f"/Font << /F1 {f1} >>", flate=True)
    return d.build(), [c.expect_page()]


@sample(
    "struct-marked-content",
    "structure",
    "标记内容 BMC/BDC/EMC 嵌套、MCID、资源属性、私有标签",
    ["marked-content", "BDC", "MCID", "tagged", "properties", "private-dict"],
    "Artifact 背景 → /P MCID 0 文字 → /Div MCID 1 内嵌套 /Span MCID 2 文字、红块、以 /Properties 资源名引用的 /Figure 蓝块 → "
    "带内联私有字典的 /MagPDF 标签文字。渲染无特殊效果；关键是往返后嵌套层次与 MCID 不乱（FPDFPageObj_GetMarkedContentID 可读到 0/2/1/3）。",
    REF_MC,
)
def struct_marked_content():
    d = Doc("struct-marked-content")
    f1 = d.std_font()
    props = d.add("<< /Type /MCR /MCID 3 /Alt (Blue box) >>")
    c = Content()
    c.path("/Artifact BMC 0.92 g 0 0 300 200 re f EMC")
    c.text("/P << /MCID 0 >> BDC BT /F1 12 Tf 0 g 20 170 Td (Paragraph MCID 0) Tj ET EMC")
    c.raw("/Div << /MCID 1 >> BDC")
    c.text("/Span << /MCID 2 /Lang (en-US) >> BDC BT /F1 12 Tf 20 150 Td (Nested Span MCID 2 inside Div MCID 1) Tj ET EMC")
    c.path("1 0 0 rg 20 100 100 30 re f")
    c.path("/Figure /Prop1 BDC 0 0 1 rg 150 100 100 30 re f EMC")
    c.raw("EMC")
    c.text("/MagPDF << /Comp /Text /Ver 1 /Lock true >> BDC BT /F1 10 Tf 0 g 20 60 Td (Private tag with inline dict) Tj ET EMC")
    c.label("struct-marked-content: nesting Artifact / P / Div(Span, path, Figure via resource) / MagPDF", y=30)
    d.page(c.data(), f"/Font << /F1 {f1} >> /Properties << /Prop1 {props} >>")
    return d.build(catalog_extra="/MarkInfo << /Marked true >>"), [c.expect_page()]


# ---------------------------------------------------------------------------
# 页面几何
# ---------------------------------------------------------------------------


def _arrow_page(d: Doc, f1, rotate: int, note: str) -> dict:
    c = Content()
    c.path("0 0 0.7 rg 150 190 m 170 160 l 160 160 l 160 120 l 140 120 l 140 160 l 130 160 l h f")
    c.text(f"BT /F1 12 Tf 0 g 20 20 Td (/Rotate {rotate}: {note}) Tj ET")
    d.page(c.data(), f"/Font << /F1 {f1} >>", extra=f"/Rotate {rotate}")
    w, h = (200, 300) if rotate in (90, 270) else (300, 200)
    return c.expect_page(w, h, rotate // 90)


@sample(
    "struct-page-rotate",
    "structure",
    "页面 /Rotate 90 / 180 / 270",
    ["rotate", "multi-page", "page-geometry"],
    "3 页，每页内容相同（箭头指向未旋转页面的上方 + 文字），/Rotate 依次 90、180、270。"
    "info 的宽高在 90/270 时应互换为 200×300，rotation 为 1/2/3；渲染后箭头分别指右、下、左。",
    REF_PAGE + ["接口协议 9.3 渲染"],
)
def struct_page_rotate():
    d = Doc("struct-page-rotate")
    f1 = d.std_font()
    pages = [
        _arrow_page(d, f1, 90, "arrow should point RIGHT"),
        _arrow_page(d, f1, 180, "arrow should point DOWN"),
        _arrow_page(d, f1, 270, "arrow should point LEFT"),
    ]
    return d.build(), pages


@sample(
    "struct-mediabox-cropbox",
    "structure",
    "MediaBox 原点偏移 / CropBox 小于 MediaBox / 反向角点 / 负坐标",
    ["mediabox", "cropbox", "page-geometry", "multi-page"],
    "4 页：MediaBox [100 100 400 300]；MediaBox 300×200 加 CropBox [50 50 250 150]（页面尺寸 200×100）；"
    "MediaBox 写成 [300 200 0 0]；MediaBox [-150 -100 150 100]。每页画边框与坐标文字，边框应贴合可见区域。"
    "对象坐标不在 0..宽高范围时，命中测试与保存都容易出错。",
    REF_PAGE + ["方案 10.6 裁剪到单据边缘"],
)
def struct_mediabox_cropbox():
    d = Doc("struct-mediabox-cropbox")
    f1 = d.std_font()

    def page(mb, label, extra="", vis=None):
        x0, y0, x1, y1 = vis or mb
        c = Content()
        c.path(f"0.8 0 0 RG 2 w {nums(x0 + 5, y0 + 5, x1 - x0 - 10, y1 - y0 - 10)} re S")
        c.text(f"BT /F1 9 Tf 0 g {nums(x0 + 12, y0 + 12)} Td ({label}) Tj ET")
        d.page(c.data(), f"/Font << /F1 {f1} >>", mediabox=mb, extra=extra)
        return c.expect_page(abs(x1 - x0), abs(y1 - y0))

    pages = [
        page((100, 100, 400, 300), "MediaBox [100 100 400 300]"),
        page((0, 0, 300, 200), "CropBox [50 50 250 150]", extra="/CropBox [50 50 250 150]", vis=(50, 50, 250, 150)),
        page((300, 200, 0, 0), "MediaBox [300 200 0 0] reversed corners", vis=(0, 0, 300, 200)),
        page((-150, -100, 150, 100), "MediaBox [-150 -100 150 100]"),
    ]
    return d.build(), pages


@sample(
    "struct-page-tree-inherited",
    "structure",
    "页面树中间节点，MediaBox / Resources / Rotate 由父节点继承",
    ["page-tree", "inherited-attributes", "rotate", "multi-page"],
    "根 Pages → 中间 Pages（声明 MediaBox、Resources、Rotate 90）→ 2 个不带这些键的页面。"
    "info 两页都应是 200×300、rotation 1，文字用继承的字体正常显示。保存时若把继承属性写丢，页面会变成默认尺寸。",
    REF_PAGE + ["方案 9.1 保存方式"],
)
def struct_page_tree_inherited():
    d = Doc("struct-page-tree-inherited")
    f1 = d.std_font()
    mid = d.reserve()
    kids = []
    pages = []
    for i in range(2):
        c = Content()
        c.path(f"{nums(0.2 * (i + 1), 0.4, 0.8)} rg 20 20 260 60 re f")
        c.text(f"BT /F1 12 Tf 0 g 20 120 Td (Inherited MediaBox/Resources/Rotate, page {i + 1}) Tj ET")
        kids.append(d.page(c.data(), resources=None, mediabox=None, parent=mid))
        pages.append(c.expect_page(200, 300, 1))
    d.set(
        mid,
        f"<< /Type /Pages /Parent {d.pages_ref} /Kids [{' '.join(str(k) for k in kids)}] /Count 2 "
        f"/MediaBox [0 0 300 200] /Rotate 90 /Resources << /Font << /F1 {f1} >> >> >>",
    )
    d.kids.append(mid)
    d.page_count = 2
    return d.build(), pages


@sample(
    "struct-empty-pages",
    "structure",
    "无内容页：没有 /Contents、空流、只有注释的流、空数组",
    ["empty-page", "no-contents", "multi-page", "edge-case"],
    "4 页都应渲染为空白、对象数 0。生成器对没有内容流的页面新建流、对空数组的处理，往返后不应报错也不应产生多余对象。",
    REF_STREAMS,
)
def struct_empty_pages():
    d = Doc("struct-empty-pages")
    d.page(None)
    d.page(b"")
    d.page(b"% only a comment and whitespace\n   \n")
    d.page([])
    return d.build(), [page_expect() for _ in range(4)]


# ---------------------------------------------------------------------------
# 文件级结构
# ---------------------------------------------------------------------------


def _simple_content(d: Doc, note: str, color: str = "0 0 1") -> Content:
    c = Content()
    c.path(f"{color} rg 20 60 120 100 re f")
    c.path("0 g 160 60 120 100 re S")
    c.text(f"BT /F1 10 Tf 0 g 20 30 Td ({note}) Tj ET")
    return c


@sample(
    "struct-xref-stream-objstm",
    "structure",
    "PDF 1.5：交叉引用流 + 对象流（Flate，PNG 预测器）",
    ["xref-stream", "object-stream", "pdf-1.5", "flate", "predictor"],
    "所有非流对象打包进一个对象流，交叉引用流带 /Predictor 12。内容是简单的蓝块、黑框和文字。"
    "保存（尤其是增量保存）时要能在对象流之上正确追加；往返后 PDFium 以何种形式写出 xref。",
    REF_FILE,
    pdf_version="1.5",
)
def struct_xref_stream_objstm():
    d = Doc("struct-xref-stream-objstm", version="1.5")
    f1 = d.std_font()
    c = _simple_content(d, "xref stream + object stream + predictor")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(xref_stream=True, objstm=True, flate=True, predictor=True), [c.expect_page()]


@sample(
    "struct-xref-stream-plain",
    "structure",
    "PDF 1.5：未压缩的交叉引用流，无对象流",
    ["xref-stream", "pdf-1.5", "uncompressed"],
    "与 struct-xref-stream-objstm 内容相同，但交叉引用流不压缩、对象不打包，便于用文本编辑器查看结构。",
    REF_FILE,
    pdf_version="1.5",
)
def struct_xref_stream_plain():
    d = Doc("struct-xref-stream-plain", version="1.5")
    f1 = d.std_font()
    c = _simple_content(d, "uncompressed xref stream, no object stream", color="0 0.5 0")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(xref_stream=True), [c.expect_page()]


@sample(
    "struct-incremental-update",
    "structure",
    "增量更新：第二节替换了内容流和 Info",
    ["incremental-update", "prev", "two-xref-sections"],
    "第一节写出蓝块 + 'VERSION 1'，第二节（/Prev 指向第一节）用同编号对象替换成红块 + 'VERSION 2 (UPDATED)' 并改 /Title。"
    "打开时必须显示红色第二版；qpdf 应无警告。往返保存时第三节追加还是重写整个文件（方案 9.1）。",
    REF_FILE,
)
def struct_incremental_update():
    d = Doc("struct-incremental-update")
    f1 = d.std_font()
    c1 = _simple_content(d, "VERSION 1 - must NOT be visible")
    content = d.stream("", c1.data())
    d.page(content, f"/Font << /F1 {f1} >>")
    base = d.build()
    c2 = _simple_content(d, "VERSION 2 (UPDATED) - incremental section", color="0.8 0 0")
    d.stream("", c2.data(), ref=content)
    assert d.pdf.info is not None
    d.set(d.pdf.info, "<< /Title (struct-incremental-update v2) /Producer (MagPDF synthetic corpus generator) >>")
    return d.pdf.build_update(base), [c2.expect_page()]


@sample(
    "struct-xref-bad-offsets",
    "structure",
    "损坏但可修复：xref 表每个偏移错 3 字节",
    ["damaged", "xref-offsets-wrong", "repairable"],
    "交叉引用表存在但所有偏移量加了 3，PDFium 校验失败后重建 xref 仍能打开并正确渲染。qpdf 会报 'xref not found / recovered'。",
    REF_FILE + ["方案 2.2 损坏的 PDF"],
    check="expect-warn",
    check_note="xref 偏移故意错位，qpdf 报警并修复",
)
def struct_xref_bad_offsets():
    d = Doc("struct-xref-bad-offsets")
    f1 = d.std_font()
    c = _simple_content(d, "xref offsets are off by +3 bytes", color="0.5 0 0.5")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(xref_offset_bias=3), [c.expect_page()]


@sample(
    "struct-xref-missing",
    "structure",
    "损坏但可修复：没有 xref 表，startxref 指向 trailer",
    ["damaged", "xref-missing", "repairable"],
    "文件没有交叉引用表，startxref 指向 trailer 关键字。解析器必须扫描全文件重建对象表。",
    REF_FILE + ["方案 2.2 损坏的 PDF"],
    check="expect-warn",
    check_note="故意不写 xref，qpdf 报警并重建",
)
def struct_xref_missing():
    d = Doc("struct-xref-missing")
    f1 = d.std_font()
    c = _simple_content(d, "no xref table at all", color="0.9 0.5 0")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(drop_xref=True), [c.expect_page()]


@sample(
    "struct-stream-length-wrong",
    "structure",
    "损坏但可修复：内容流 /Length 比实际短 10 字节",
    ["damaged", "stream-length-wrong", "repairable"],
    "内容流 /Length 少写 10，解析器在 Length 处找不到 endstream 时要向后搜索。渲染应完整（文字最后几个字符不能丢）。",
    REF_FILE,
    check="expect-warn",
    check_note="/Length 故意写短，qpdf 报警并恢复",
)
def struct_stream_length_wrong():
    d = Doc("struct-stream-length-wrong")
    f1 = d.std_font()
    c = _simple_content(d, "stream /Length is 10 bytes too short", color="0 0.5 0.5")
    data = c.data()
    d.page(data, f"/Font << /F1 {f1} >>", length=len(data) - 10)
    return d.build(), [c.expect_page()]


# ---------------------------------------------------------------------------
# 可选内容、批注、变换、缺失资源
# ---------------------------------------------------------------------------


@sample(
    "struct-ocg-layers",
    "structure",
    "两个可选内容组：一个打印关闭（/PrintState /OFF），一个屏幕关闭（/ViewState /OFF）",
    ["ocg", "optional-content", "print-off", "view-off", "layers", "no-print-layer"],
    "图层 Background：浅黄底 + 文字 'BACKGROUND (screen only)'，/Usage /Print /PrintState /OFF 并在 /D /AS 登记 Print 事件；"
    "图层 PrintOnly：红框 + 文字 'PRINT ONLY' + 一个带 /OC 的图像，/View /ViewState /OFF 并登记 View 事件；中间的黑字始终可见。"
    "屏幕渲染：见背景、不见红框；FPDF_PRINTING 渲染：不见背景、见红框（V16 要验证的前提）。往返保存后 BDC /OC 嵌套与 /OC 键必须保留。",
    REF_OCG,
    pdf_version="1.5",
)
def struct_ocg_layers():
    d = Doc("struct-ocg-layers", version="1.5")
    f1 = d.std_font("Helvetica-Bold")
    ocg_bg = d.add("<< /Type /OCG /Name (Background - no print) /Usage << /Print << /PrintState /OFF >> /View << /ViewState /ON >> >> >>")
    ocg_pr = d.add("<< /Type /OCG /Name (Print only) /Usage << /View << /ViewState /OFF >> /Print << /PrintState /ON >> >> >>")
    im = d.image_xobject(2, 2, "/DeviceRGB", 8, bytes([255, 0, 0, 255, 255, 255, 255, 255, 255, 255, 0, 0]), extra=f"/OC {ocg_pr}")
    c = Content()
    c.raw("/OC /ocBg BDC")
    c.path("1 0.97 0.8 rg 0 0 300 200 re f")
    c.text("BT /F1 14 Tf 0.6 0.5 0 rg 20 160 Td (BACKGROUND \\(screen only\\)) Tj ET")
    c.raw("EMC")
    c.text("BT /F1 12 Tf 0 g 20 100 Td (Always visible content) Tj ET")
    c.raw("/OC /ocPr BDC")
    c.path("1 0 0 RG 3 w 15 20 270 50 re S")
    c.text("BT /F1 14 Tf 1 0 0 rg 25 38 Td (PRINT ONLY \\(ViewState OFF\\)) Tj ET")
    c.image("q 40 0 0 40 240 25 cm /Im1 Do Q")
    c.raw("EMC")
    d.page(c.data(), f"/Font << /F1 {f1} >> /Properties << /ocBg {ocg_bg} /ocPr {ocg_pr} >> /XObject << /Im1 {im} >>")
    occ = (
        f"/OCProperties << /OCGs [{ocg_bg} {ocg_pr}] /D << /Order [{ocg_bg} {ocg_pr}] /ON [{ocg_bg} {ocg_pr}] "
        f"/AS [ << /Event /Print /OCGs [{ocg_bg} {ocg_pr}] /Category [/Print] >> "
        f"<< /Event /View /OCGs [{ocg_bg} {ocg_pr}] /Category [/View] >> ] >> >>"
    )
    return d.build(catalog_extra=occ), [c.expect_page()]


@sample(
    "struct-annots-appearance",
    "structure",
    "页面 /Annots：带外观流的 Square 与 Stamp、Hidden、NoView、Link、Text+Popup",
    ["annots", "appearance-stream", "hidden-flag", "noview-flag", "link", "popup"],
    "页面内容只有一行字和一条线；批注：带 /AP 的黄底红框 Square、带 /AP 的 DRAFT 印章、F=Hidden 的 Square（不显示）、"
    "F=NoView 的 Square（屏幕不显示、打印显示）、无 AP 的 Link、带 Popup 的 Text。不带 --annots 渲染时只有页面内容；"
    "带 --annots 时出现 Square 与 Stamp。批注不是页面对象，往返重写内容流不应动它们。",
    ["方案 9.4 批注和表单", "方案 11.6 印章", "接口协议 9.3"],
)
def struct_annots_appearance():
    d = Doc("struct-annots-appearance")
    f1 = d.std_font("Helvetica-Bold")
    ap_square = d.form_xobject((0, 0, 100, 60), b"1 1 0 rg 1 0 0 RG 2 w 1 1 98 58 re B")
    ap_stamp = d.form_xobject(
        (0, 0, 120, 40),
        b"q 0.8 0 0 RG 2 w 2 2 116 36 re S BT /F1 18 Tf 0.8 0 0 rg 20 13 Td (DRAFT) Tj ET Q",
        resources=f"/Font << /F1 {f1} >>",
    )
    popup = d.reserve()
    text_annot = d.add(
        f"<< /Type /Annot /Subtype /Text /Rect [250 150 270 170] /F 4 /Name /Comment /T (SAMPLE reviewer) "
        f"/Contents (Sample note contents) /Popup {popup} /C [1 1 0] >>"
    )
    d.set(popup, f"<< /Type /Annot /Subtype /Popup /Rect [180 100 290 145] /Parent {text_annot} /Open true >>")
    annots = [
        d.add(f"<< /Type /Annot /Subtype /Square /Rect [20 100 120 160] /F 4 /C [1 0 0] /IC [1 1 0] /AP << /N {ap_square} >> >>"),
        d.add(f"<< /Type /Annot /Subtype /Stamp /Name /Draft /Rect [140 120 260 160] /F 4 /AP << /N {ap_stamp} >> >>"),
        d.add(f"<< /Type /Annot /Subtype /Square /Rect [20 30 120 90] /F 6 /C [0 0 1] /AP << /N {ap_square} >> >>"),
        d.add(f"<< /Type /Annot /Subtype /Square /Rect [140 30 240 90] /F 36 /C [0 1 0] /AP << /N {ap_square} >> >>"),
        d.add("<< /Type /Annot /Subtype /Link /Rect [20 10 150 25] /Border [0 0 1] /C [0 0 1] /A << /S /URI /URI (https://example.invalid/sample) >> >>"),
        text_annot,
        popup,
    ]
    c = Content()
    c.text("BT /F1 9 Tf 0 g 20 12 Td (page content: link annot sits on this line) Tj ET")
    c.path("0 g 1 w 20 95 m 280 95 l S")
    c.label("struct-annots-appearance: render with and without annotations", font="F1")
    d.page(c.data(), f"/Font << /F1 {f1} >>", extra=f"/Annots [{' '.join(str(a) for a in annots)}]")
    return d.build(), [c.expect_page()]


@sample(
    "struct-cm-nested-transforms",
    "structure",
    "cm 变换：缩放 0.001 的大坐标、旋转、三层嵌套 q/cm、斜切、镜像、Tm 旋转文字",
    ["cm", "transform", "nested-q", "rotate", "skew", "mirror", "Tm", "number-formatting"],
    "红块用 0.001 缩放加五位数坐标画出；蓝块旋转 45°；绿块经三层 q/cm 嵌套（2×0.5 = 1）；黑框斜切；紫块水平镜像；右侧一行竖排文字。"
    "生成器把 cm 折算进对象矩阵后，数字精度与写法（科学计数法、过多小数）会影响保真；往返后各块位置不能偏移。",
    REF_PAGE + ["P0 计划 V17 自研序列化"],
)
def struct_cm_nested_transforms():
    d = Doc("struct-cm-nested-transforms")
    f1 = d.std_font()
    c = Content()
    c.path("q 0.001 0 0 0.001 0 0 cm 1 0 0 rg 20000 120000 80000 60000 re f Q")
    c.path("q 0.7071 0.7071 -0.7071 0.7071 150 90 cm 0 0 1 rg 0 0 50 50 re f Q")
    c.path("q 1 0 0 1 200 100 cm q 2 0 0 2 0 0 cm q 0.5 0 0 0.5 0 0 cm 0 0.6 0 rg 0 0 50 50 re f Q Q Q")
    c.path("q 1 0 0.5 1 20 20 cm 0 G 2 w 0 0 60 40 re S Q")
    c.path("q -1 0 0 1 280 20 cm 0.6 0 0.6 rg 0 0 40 40 re f Q")
    c.text("BT 0 1 -1 0 292 20 Tm /F1 10 Tf 0 g (Rotated text via Tm) Tj ET")
    c.label("struct-cm-nested-transforms: scale 0.001 / rotate / nested q cm / skew / mirror / Tm")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]


@sample(
    "struct-missing-resources",
    "structure",
    "内容流引用不存在的字体、XObject、ExtGState、颜色空间",
    ["missing-resources", "edge-case", "fallback-font"],
    "/F9 不在资源里（PDFium 用默认 Helvetica 显示）、/X9 Do 被忽略（不产生对象）、/GS9 gs 被忽略后方块照画、/CS9 cs 被忽略。"
    "预期 2 个文字、2 个路径、0 个图像/Form。生成器写回时不应为缺失的资源生成空引用或崩溃。",
    REF_PAGE + ["方案 2.1 对不规范文件的兼容性"],
)
def struct_missing_resources():
    d = Doc("struct-missing-resources")
    f1 = d.std_font()
    c = Content()
    c.text("BT /F9 14 Tf 0 g 20 150 Td (Font /F9 is missing from /Resources) Tj ET")
    c.raw("q 100 0 0 50 20 80 cm /X9 Do Q")
    c.path("q /GS9 gs 0 0 1 rg 150 80 100 50 re f Q")
    c.path("q /CS9 cs 0.5 sc 0.9 0.4 0 rg 20 20 100 40 re f Q")
    c.label("struct-missing-resources: F9 / X9 / GS9 / CS9 all undefined", font="F1")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]
