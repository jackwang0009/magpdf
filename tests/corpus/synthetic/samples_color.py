"""颜色空间与渐变样本（方案 2.1 第 1、2 条：颜色只写 RGB；`sh` 被跳过）。"""

from __future__ import annotations

import struct

from common import Content, Doc, sample
from pdfgen import minimal_icc_rgb

REF_COLOR = ["方案 2.1 颜色空间", "P0 计划 V02/V03 第 2 项"]
REF_SHADING = ["方案 2.1 渐变 sh", "P0 计划 V03 第 3 项"]


def _label(c: Content, s: str) -> None:
    c.label(s)


# ---------------------------------------------------------------------------
# 颜色空间
# ---------------------------------------------------------------------------


@sample(
    "color-devicegray",
    "color",
    "DeviceGray 填充与描边（g / G）",
    ["devicegray", "fill", "stroke"],
    "三个灰阶方块、三个灰色描边框和一行灰色文字。生成器把 g/G 改写成 rg/RG：像素应一致，但内容流的颜色操作符变了；"
    "比较往返前后内容流即可看出。",
    REF_COLOR,
)
def color_devicegray():
    d = Doc("color-devicegray")
    f1 = d.std_font()
    c = Content()
    c.path("0.2 g 20 120 60 60 re f")
    c.path("0.5 g 100 120 60 60 re f")
    c.path("0.8 g 180 120 60 60 re f")
    c.path("0 G 4 w 20 40 60 60 re S")
    c.path("0.5 G 100 40 60 60 re S")
    c.path("0.75 g 0.25 G 180 40 60 60 re B")
    c.text("BT /F1 10 Tf 0.3 g 20 20 Td (DeviceGray fill g / stroke G) Tj ET")
    _label(c, "color-devicegray: g/G operators; expect same pixels, different operators after round-trip")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]


@sample(
    "color-devicecmyk",
    "color",
    "DeviceCMYK 填充与描边（k / K）",
    ["devicecmyk", "fill", "stroke", "rich-black"],
    "C/M/Y/K 四个原色块、四色黑与单黑对比、蓝色（C+M）描边。生成器把 k/K 换成 rg/RG 后颜色空间丢失；"
    "四色黑和单黑在转成 RGB 后可能变成同一个值，印刷时无法区分。",
    REF_COLOR,
)
def color_devicecmyk():
    d = Doc("color-devicecmyk")
    f1 = d.std_font()
    c = Content()
    c.path("1 0 0 0 k 20 120 50 50 re f")
    c.path("0 1 0 0 k 80 120 50 50 re f")
    c.path("0 0 1 0 k 140 120 50 50 re f")
    c.path("0 0 0 1 k 200 120 50 50 re f")
    c.path("0.6 0.4 0.4 1 k 20 40 50 50 re f")
    c.path("0 0 0 1 k 80 40 50 50 re f")
    c.path("1 1 0 0 K 6 w 140 40 50 50 re S")
    c.path("0.5 0 0 0 k 0 0 0 1 K 2 w 200 40 50 50 re B")
    c.text("BT /F1 9 Tf 0 0 0 1 k 20 20 Td (CMYK text: 0 0 0 1 k   rich black vs 100K above) Tj ET")
    _label(c, "color-devicecmyk: k/K operators; round-trip rewrites as RGB")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]


@sample(
    "color-separation",
    "color",
    "Separation 专色（含 /All 与 /None）",
    ["separation", "spot", "tint-transform", "sep-all", "sep-none"],
    "虚构专色 SampleOrange 的 100%/60%/30% 色块与描边、/All 色块、/None 色块（按规范不应留下任何墨迹）、专色文字。"
    "生成器不写专色操作符时色块变黑；/None 色块若被画出来说明转换成了 RGB。",
    REF_COLOR,
)
def color_separation():
    d = Doc("color-separation")
    f1 = d.std_font()
    fn = "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.55 1 0] /N 1 >>"
    fn_all = "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 1 1 1] /N 1 >>"
    cs_orange = d.add(f"[/Separation /SampleOrange /DeviceCMYK {fn}]")
    cs_all = d.add(f"[/Separation /All /DeviceCMYK {fn_all}]")
    cs_none = d.add(f"[/Separation /None /DeviceCMYK {fn}]")
    c = Content()
    c.path("/CSo cs 1 sc 20 110 60 70 re f")
    c.path("/CSo cs 0.6 scn 90 110 60 70 re f")
    c.path("0.3 scn 160 110 60 70 re f")
    c.path("/CSo CS 1 SCN 5 w 235 110 45 70 re S")
    c.path("/CSa cs 1 sc 20 30 60 60 re f")
    c.path("/CSn cs 1 sc 90 30 60 60 re f")
    c.text("BT /F1 9 Tf /CSo cs 1 sc 160 70 Td (Spot text SampleOrange) Tj ET")
    c.text("BT /F1 7 Tf 0 g 160 40 Td (left: /All   middle: /None = must be blank) Tj ET")
    _label(c, "color-separation: Separation cs/scn; blocks turn black if generator drops the colour space")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ColorSpace << /CSo {cs_orange} /CSa {cs_all} /CSn {cs_none} >>")
    return d.build(), [c.expect_page()]


@sample(
    "color-devicen",
    "color",
    "DeviceN 双通道专色（PostScript 型 4 函数）",
    ["devicen", "spot", "function-type4"],
    "两个虚构着色剂 SampleOrange / SampleBlue 的四种组合色块与一个描边。生成器丢失颜色空间时全部变黑；"
    "PostScript 计算函数的解析与 DeviceN 分量数（2）要在往返后保持。",
    REF_COLOR,
)
def color_devicen():
    d = Doc("color-devicen")
    f1 = d.std_font()
    # 输入 o b → 输出 c m y k = b, 0.5o, o, 0
    fn = d.stream("/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1]", b"{ exch dup 0.5 mul exch 0 }")
    cs = d.add(f"[/DeviceN [/SampleOrange /SampleBlue] /DeviceCMYK {fn}]")
    c = Content()
    c.path("/CSdn cs 1 0 scn 20 110 60 70 re f")
    c.path("/CSdn cs 0 1 scn 90 110 60 70 re f")
    c.path("/CSdn cs 1 1 scn 160 110 60 70 re f")
    c.path("/CSdn cs 0.5 0.5 scn 20 30 60 60 re f")
    c.path("/CSdn CS 1 0 SCN 6 w 100 30 120 60 re S")
    _label(c, "color-devicen: 2-colorant DeviceN with type-4 tint transform")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ColorSpace << /CSdn {cs} >>")
    return d.build(), [c.expect_page()]


@sample(
    "color-iccbased-embedded",
    "color",
    "ICCBased N=3，嵌入红绿互换的小 ICC 配置",
    ["iccbased", "icc-embedded", "n3"],
    "嵌入的 ICC 把红、绿原色对调：按配置转换的渲染器把 (1 0 0) 画成绿色，第二块画成红色；忽略 ICC 的渲染器画成红、绿。"
    "右侧 DeviceRGB 参照红块。往返后若颜色空间被丢成 DeviceRGB，前两块与参照块的关系反转。",
    REF_COLOR,
)
def color_iccbased_embedded():
    d = Doc("color-iccbased-embedded")
    f1 = d.std_font()
    icc = d.stream("/N 3 /Alternate /DeviceRGB", minimal_icc_rgb(swap_rg=True), flate=True)
    cs = d.add(f"[/ICCBased {icc}]")
    c = Content()
    c.path("/CSicc cs 1 0 0 sc 20 100 70 80 re f")
    c.path("/CSicc cs 0 1 0 sc 110 100 70 80 re f")
    c.path("1 0 0 rg 200 100 70 80 re f")
    c.path("/CSicc CS 0 0 1 SC 6 w 20 30 250 50 re S")
    c.text("BT /F1 7 Tf 0 g 20 20 Td (ICC 1 0 0 = GREEN if profile honoured | ICC 0 1 0 | DeviceRGB 1 0 0 reference) Tj ET")
    _label(c, "color-iccbased-embedded: swapped R/G matrix profile")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ColorSpace << /CSicc {cs} >>")
    return d.build(), [c.expect_page()]


@sample(
    "color-iccbased-fallback",
    "color",
    "ICCBased 配置无效，回退到 /Alternate",
    ["iccbased", "icc-invalid", "alternate", "n4", "n1"],
    "N=4 的 ICC 流内容不是合法配置，带 /Alternate /DeviceCMYK；N=1 的无效配置不带 Alternate（应按 DeviceGray）。"
    "青、黑、灰三块应正常显示；往返后颜色空间对象是否保留、还是被替换成设备空间。",
    REF_COLOR,
)
def color_iccbased_fallback():
    d = Doc("color-iccbased-fallback")
    f1 = d.std_font()
    icc4 = d.stream("/N 4 /Alternate /DeviceCMYK", b"this is not an ICC profile, renderer must fall back to /Alternate")
    icc1 = d.stream("/N 1", b"also not an ICC profile; no /Alternate so DeviceGray applies")
    cs4 = d.add(f"[/ICCBased {icc4}]")
    cs1 = d.add(f"[/ICCBased {icc1}]")
    c = Content()
    c.path("/CS4 cs 1 0 0 0 sc 20 100 70 80 re f")
    c.path("/CS4 cs 0 0 0 1 sc 110 100 70 80 re f")
    c.path("/CS1 cs 0.5 sc 200 100 70 80 re f")
    _label(c, "color-iccbased-fallback: invalid ICC streams -> cyan / black / 50% gray")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ColorSpace << /CS4 {cs4} /CS1 {cs1} >>")
    return d.build(), [c.expect_page()]


@sample(
    "color-indexed",
    "color",
    "Indexed 颜色空间：填充与 2 位图像",
    ["indexed", "palette", "image-indexed"],
    "4 色调色板（红绿蓝黄）：索引 0–3 的四个色块，以及用同一调色板的 2 bpc 4×4 图像。索引色填充写成 RGB 时像素不变，"
    "但 hival 边界（索引 3）和调色板引用会丢失。",
    REF_COLOR,
)
def color_indexed():
    d = Doc("color-indexed")
    f1 = d.std_font()
    cs = d.add("[/Indexed /DeviceRGB 3 <FF000000FF000000FFFFFF00>]")
    # 4×4、2 bpc：每行 1 字节。行像素索引：0123 / 1230 / 2301 / 3012
    img_data = bytes([0b00011011, 0b01101100, 0b10110001, 0b11000110])
    im = d.image_xobject(4, 4, "/CSidx", 2, img_data)
    c = Content()
    c.path("/CSidx cs 0 sc 20 110 50 70 re f")
    c.path("/CSidx cs 1 sc 80 110 50 70 re f")
    c.path("2 sc 140 110 50 70 re f")
    c.path("3 sc 200 110 50 70 re f")
    c.image("q 80 0 0 80 20 20 cm /Im1 Do Q")
    c.text("BT /F1 8 Tf 0 g 120 50 Td (left: 2 bpc Indexed image, same palette) Tj ET")
    _label(c, "color-indexed: 4-entry palette, fills and image")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ColorSpace << /CSidx {cs} >> /XObject << /Im1 {im} >>")
    return d.build(), [c.expect_page()]


@sample(
    "color-lab",
    "color",
    "Lab 颜色空间填充",
    ["lab", "cie"],
    "四个 Lab 色块：品红、绿、蓝、近白。Lab 分量范围按 /Range [-100 100 -100 100]；写成 RGB 后颜色相近但不再是设备无关色。",
    REF_COLOR,
)
def color_lab():
    d = Doc("color-lab")
    f1 = d.std_font()
    cs = d.add("[/Lab << /WhitePoint [0.9505 1 1.089] /Range [-100 100 -100 100] >>]")
    c = Content()
    c.path("/CSlab cs 50 80 0 sc 20 110 60 70 re f")
    c.path("/CSlab cs 80 -80 80 sc 90 110 60 70 re f")
    c.path("/CSlab cs 30 60 -100 sc 160 110 60 70 re f")
    c.path("/CSlab cs 95 0 0 sc 230 110 50 70 re f")
    c.path("/CSlab CS 50 80 0 SC 4 w 20 30 260 60 re S")
    _label(c, "color-lab: Lab fills (magenta, green, blue, near-white) and stroke")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ColorSpace << /CSlab {cs} >>")
    return d.build(), [c.expect_page()]


@sample(
    "color-calrgb-calgray",
    "color",
    "CalRGB 与 CalGray",
    ["calrgb", "calgray", "cie"],
    "CalRGB（带 Gamma 与 Matrix）三块、CalGray 两块。PDFium 把 CalRGB 按 DeviceRGB 处理（忽略 Gamma/Matrix），"
    "往返后颜色空间名是否保留。",
    REF_COLOR,
)
def color_calrgb_calgray():
    d = Doc("color-calrgb-calgray")
    f1 = d.std_font()
    calrgb = d.add(
        "[/CalRGB << /WhitePoint [0.9505 1 1.089] /Gamma [2.2 2.2 2.2] "
        "/Matrix [0.4124 0.2126 0.0193 0.3576 0.7152 0.1192 0.1805 0.0722 0.9505] >>]"
    )
    calgray = d.add("[/CalGray << /WhitePoint [0.9505 1 1.089] /Gamma 2.2 >>]")
    c = Content()
    c.path("/CSrgb cs 0.9 0.2 0.2 sc 20 110 60 70 re f")
    c.path("/CSrgb cs 0.2 0.7 0.2 sc 90 110 60 70 re f")
    c.path("/CSrgb cs 0.2 0.2 0.9 sc 160 110 60 70 re f")
    c.path("/CSg cs 0.3 sc 20 30 60 60 re f")
    c.path("/CSg cs 0.7 sc 90 30 60 60 re f")
    _label(c, "color-calrgb-calgray: CalRGB x3, CalGray x2")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ColorSpace << /CSrgb {calrgb} /CSg {calgray} >>")
    return d.build(), [c.expect_page()]


@sample(
    "pattern-tiling",
    "color",
    "平铺图案填充：着色 / 非着色 / 带 Matrix / 描边",
    ["pattern", "tiling", "paint-type-1", "paint-type-2", "pattern-matrix"],
    "左：着色平铺图案（红蓝格）；右：非着色图案以 /Pattern+DeviceRGB 指定蓝色、图案矩阵旋转 45°；底部：图案描边的粗线。"
    "生成器不写 Pattern 时三者都变成黑色实心。",
    REF_COLOR + ["方案 4.1 对象"],
)
def pattern_tiling():
    d = Doc("pattern-tiling")
    f1 = d.std_font()
    p1 = d.stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f 0 0 1 rg 10 10 10 10 re f 0 g 0 10 m 20 10 l 10 0 m 10 20 l S",
    )
    p2 = d.stream(
        "/Type /Pattern /PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 12 12] /XStep 12 /YStep 12 "
        "/Resources << >> /Matrix [0.7071 0.7071 -0.7071 0.7071 0 0]",
        b"0 0 6 6 re f 6 6 6 6 re f",
    )
    cs_pat = d.add("[/Pattern /DeviceRGB]")
    c = Content()
    c.path("/Pattern cs /P1 scn 20 50 120 120 re f")
    c.path("/CSpat cs 0 0 1 /P2 scn 160 50 120 120 re f")
    c.path("/Pattern CS /P1 SCN 10 w 20 30 m 280 30 l S")
    _label(c, "pattern-tiling: coloured tiling, uncoloured tiling (blue, rotated matrix), pattern stroke")
    d.page(
        c.data(),
        f"/Font << /F1 {f1} >> /Pattern << /P1 {p1} /P2 {p2} >> /ColorSpace << /CSpat {cs_pat} >>",
    )
    return d.build(), [c.expect_page()]


@sample(
    "pattern-shading-fill",
    "color",
    "渐变图案作为路径与文字的填充、描边",
    ["pattern", "shading-pattern", "axial", "radial", "text-pattern-fill"],
    "左块轴向渐变图案、右块径向渐变图案、底部大字用轴向渐变填充、顶部粗线用径向渐变描边。"
    "这些是路径/文字对象而不是 sh 对象；生成器不写 Pattern 时全部变黑。",
    REF_COLOR + REF_SHADING,
)
def pattern_shading_fill():
    d = Doc("pattern-shading-fill")
    f1 = d.std_font("Helvetica-Bold")
    axial = (
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 280 0] "
        "/Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >>"
    )
    radial = (
        "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [220 110 0 220 110 60] "
        "/Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 0] /C1 [0 0.5 0] /N 1 >> /Extend [false true] >>"
    )
    psh = d.add(f"<< /Type /Pattern /PatternType 2 /Shading {axial} >>")
    prad = d.add(f"<< /Type /Pattern /PatternType 2 /Shading {radial} >>")
    c = Content()
    c.path("/Pattern cs /Psh scn 20 60 120 110 re f")
    c.path("/Pattern cs /Prad scn 160 60 120 110 re f")
    c.text("BT /F1 26 Tf /Pattern cs /Psh scn 20 20 Td (GRADIENT TEXT) Tj ET")
    c.path("/Pattern CS /Prad SCN 8 w 20 182 m 280 182 l S")
    d.page(c.data(), f"/Font << /F1 {f1} >> /Pattern << /Psh {psh} /Prad {prad} >>")
    return d.build(), [c.expect_page()]


# ---------------------------------------------------------------------------
# 渐变 sh
# ---------------------------------------------------------------------------


@sample(
    "shading-axial",
    "shading",
    "轴向渐变 sh（Extend 开 / 关）",
    ["shading", "axial", "sh", "extend", "clip"],
    "左：Extend [true true]，剪切区被整块填满；右：Extend [false false] 且 Coords 窄于剪切区，两侧留白。"
    "生成器跳过 sh 时两块都消失。",
    REF_SHADING,
)
def shading_axial():
    d = Doc("shading-axial")
    f1 = d.std_font()
    sh1 = d.add(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [40 0 120 0] "
        "/Function << /FunctionType 2 /Domain [0 1] /C0 [1 0.5 0] /C1 [0.1 0.1 0.6] /N 1 >> /Extend [true true] >>"
    )
    sh2 = d.add(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [190 40 250 180] "
        "/Function << /FunctionType 2 /Domain [0 1] /C0 [0 0.6 0.2] /C1 [0.9 0.9 0] /N 1 >> /Extend [false false] >>"
    )
    c = Content()
    c.shading("q 20 40 120 140 re W n /Sh1 sh Q")
    c.shading("q 160 40 120 140 re W n /Sh2 sh Q")
    _label(c, "shading-axial: left Extend on, right Extend off (blank margins expected)")
    d.page(c.data(), f"/Font << /F1 {f1} >> /Shading << /Sh1 {sh1} /Sh2 {sh2} >>")
    return d.build(), [c.expect_page()]


@sample(
    "shading-radial",
    "shading",
    "径向渐变 sh（同心 / 偏心，Extend）",
    ["shading", "radial", "sh", "extend", "clip"],
    "左：同心圆，Extend [false true] 外圈颜色延伸填满剪切区；右：偏心小圆到大圆、不延伸，圆外留白。",
    REF_SHADING,
)
def shading_radial():
    d = Doc("shading-radial")
    f1 = d.std_font()
    sh1 = d.add(
        "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [80 110 0 80 110 50] "
        "/Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [0.8 0 0] /N 1 >> /Extend [false true] >>"
    )
    sh2 = d.add(
        "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [200 130 5 225 110 60] "
        "/Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 0.6] /C1 [0 0.3 0.7] /N 1 >> /Extend [false false] >>"
    )
    c = Content()
    c.shading("q 20 40 120 140 re W n /Sh1 sh Q")
    c.shading("q 160 40 120 140 re W n /Sh2 sh Q")
    _label(c, "shading-radial: concentric (outer extend) and eccentric (no extend)")
    d.page(c.data(), f"/Font << /F1 {f1} >> /Shading << /Sh1 {sh1} /Sh2 {sh2} >>")
    return d.build(), [c.expect_page()]


@sample(
    "shading-stitching-background",
    "shading",
    "拼接函数多色渐变，带 /Background 与 /BBox",
    ["shading", "axial", "function-type3", "background", "bbox", "shading-pattern"],
    "左块：同一渐变作为图案填充，Coords 不覆盖整块、Extend 关，渐变外的区域应显示 /Background 黄色；"
    "右块：同一渐变直接 sh，按规范忽略 Background，渐变外留白；/BBox 把渐变限制在中间一条。",
    REF_SHADING,
)
def shading_stitching_background():
    d = Doc("shading-stitching-background")
    f1 = d.std_font()
    fn = (
        "<< /FunctionType 3 /Domain [0 1] /Functions ["
        "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 1 0] /N 1 >> "
        "<< /FunctionType 2 /Domain [0 1] /C0 [0 1 0] /C1 [0 0 1] /N 1 >> "
        "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 1] /C1 [0 0 0] /N 1 >>] "
        "/Bounds [0.3 0.7] /Encode [0 1 0 1 0 1] >>"
    )
    sh_pat = d.add(
        f"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [50 0 110 0] /Function {fn} /Extend [false false] "
        "/Background [1 0.9 0.2] /BBox [20 70 140 150] >>"
    )
    sh_direct = d.add(
        f"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [190 0 250 0] /Function {fn} /Extend [false false] "
        "/Background [1 0.9 0.2] /BBox [160 70 280 150] >>"
    )
    pat = d.add(f"<< /Type /Pattern /PatternType 2 /Shading {sh_pat} >>")
    c = Content()
    c.path("/Pattern cs /Pbg scn 20 40 120 140 re f")
    c.shading("q 160 40 120 140 re W n /Shd sh Q")
    _label(c, "shading-stitching-background: left pattern shows /Background yellow, right sh ignores it")
    d.page(c.data(), f"/Font << /F1 {f1} >> /Pattern << /Pbg {pat} >> /Shading << /Shd {sh_direct} >>")
    return d.build(), [c.expect_page()]


@sample(
    "shading-functions",
    "shading",
    "采样函数、PostScript 函数、函数数组的渐变",
    ["shading", "axial", "function-type0", "function-type4", "function-array"],
    "三条横向渐变：上为 4 个采样点的型 0 函数（台阶状），中为型 4 PostScript 函数（正弦条纹），下为三个单输出函数组成的数组。"
    "任一函数类型解析或写回失败都会让对应条带消失或变色。",
    REF_SHADING,
)
def shading_functions():
    d = Doc("shading-functions")
    f1 = d.std_font()
    sampled = d.stream(
        "/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [4] /BitsPerSample 8",
        bytes([255, 0, 0, 255, 255, 0, 0, 160, 0, 0, 0, 200]),
    )
    ps = d.stream(
        "/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1]",
        b"{ 1080 mul sin 2 div 0.5 add dup 1 exch sub 0 }",
    )
    f_r = "<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >>"
    f_g = "<< /FunctionType 2 /Domain [0 1] /C0 [0.5] /C1 [0] /N 2 >>"
    f_b = "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 0.5 >>"
    sh0 = d.add(f"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 280 0] /Function {sampled} /Extend [true true] >>")
    sh4 = d.add(f"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 280 0] /Function {ps} /Extend [true true] >>")
    sha = d.add(
        f"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 280 0] /Function [{f_r} {f_g} {f_b}] /Extend [true true] >>"
    )
    c = Content()
    c.shading("q 20 130 260 45 re W n /Sh0 sh Q")
    c.shading("q 20 75 260 45 re W n /Sh4 sh Q")
    c.shading("q 20 20 260 45 re W n /ShA sh Q")
    _label(c, "shading-functions: type 0 sampled / type 4 PostScript / array of 3 type-2 functions")
    d.page(c.data(), f"/Font << /F1 {f1} >> /Shading << /Sh0 {sh0} /Sh4 {sh4} /ShA {sha} >>")
    return d.build(), [c.expect_page()]


def _u16(v: float, lo: float, hi: float) -> int:
    return max(0, min(65535, round((v - lo) / (hi - lo) * 65535)))


@sample(
    "shading-mesh",
    "shading",
    "网格渐变：型 4 自由三角形、型 6 Coons 曲面片（二进制数据）",
    ["shading", "mesh", "type4", "type6", "coons", "binary-stream"],
    "左：两个三角形组成的四边形，顶点红绿蓝黄；右：一个边缘弯曲的 Coons 曲面片。"
    "二进制顶点数据在往返时必须原样保留；任何重编码都会破坏网格。",
    REF_SHADING,
)
def shading_mesh():
    d = Doc("shading-mesh")
    f1 = d.std_font()

    def vert(flag: int, x: float, y: float, rgb: tuple[int, int, int]) -> bytes:
        return struct.pack(">BHH3B", flag, _u16(x, 0, 300), _u16(y, 0, 200), *rgb)

    tri = b"".join(
        [
            vert(0, 20, 40, (255, 0, 0)),
            vert(0, 140, 40, (0, 255, 0)),
            vert(0, 140, 180, (0, 0, 255)),
            vert(0, 20, 40, (255, 0, 0)),
            vert(0, 140, 180, (0, 0, 255)),
            vert(0, 20, 180, (255, 255, 0)),
        ]
    )
    sh4 = d.stream(
        "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 /BitsPerComponent 8 /BitsPerFlag 8 "
        "/Decode [0 300 0 200 0 1 0 1 0 1]",
        tri,
    )
    pts = [
        (160, 40), (200, 20), (240, 60), (280, 40),  # 底边
        (260, 90), (300, 130), (280, 180),  # 右边
        (240, 160), (200, 200), (160, 180),  # 顶边
        (180, 130), (140, 90),  # 左边
    ]  # fmt: skip
    coons = struct.pack(">B", 0)
    for x, y in pts:
        coons += struct.pack(">HH", _u16(x, 0, 300), _u16(y, 0, 200))
    for rgb in [(255, 0, 128), (0, 128, 255), (255, 200, 0), (0, 160, 80)]:
        coons += bytes(rgb)
    sh6 = d.stream(
        "/ShadingType 6 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 /BitsPerComponent 8 /BitsPerFlag 8 "
        "/Decode [0 300 0 200 0 1 0 1 0 1]",
        coons,
    )
    c = Content()
    c.shading("q 0 0 150 200 re W n /Sh4 sh Q")
    c.shading("q 150 0 150 200 re W n /Sh6 sh Q")
    _label(c, "shading-mesh: type 4 triangles (left), type 6 Coons patch (right)")
    d.page(c.data(), f"/Font << /F1 {f1} >> /Shading << /Sh4 {sh4} /Sh6 {sh6} >>")
    return d.build(), [c.expect_page()]
