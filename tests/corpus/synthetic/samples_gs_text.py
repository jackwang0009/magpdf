"""ExtGState 与文字样本（方案 2.1 第 3、4、5 条）。"""

from __future__ import annotations

from common import Content, Doc, sample
from pdfgen import adobe_gb1_cid, hexstr, iso6346_check_digit, minimal_truetype

REF_GS = ["方案 2.1 ExtGState", "P0 计划 V03 第 4 项"]
REF_TEXT = ["方案 2.1 文字 TJ/Tc/Tw", "P0 计划 V03 第 1 项"]
REF_TYPE3 = ["方案 2.1 Type3", "P0 计划 V03 第 5 项"]
REF_CLIP = ["方案 2.3 第 3 条 文字裁剪整体保留", "P0 计划 V03 生成规则"]

# ---------------------------------------------------------------------------
# ExtGState
# ---------------------------------------------------------------------------


@sample(
    "gs-alpha",
    "extgstate",
    "常量透明度 ca / CA（填充、描边、文字）",
    ["extgstate", "ca", "CA", "alpha"],
    "红块不透明，蓝块 50%、绿块 25% 叠在上面，底部黑线 50% 描边，文字 ALPHA 50%。生成器只合成 ca/CA/BM，这一项应能保持。",
    REF_GS,
)
def gs_alpha():
    d = Doc("gs-alpha")
    f1 = d.std_font("Helvetica-Bold")
    c = Content()
    c.path("1 0 0 rg 20 60 100 100 re f")
    c.path("q /GS50 gs 0 0 1 rg 70 40 100 100 re f Q")
    c.path("q /GS25 gs 0 0.6 0 rg 120 80 100 100 re f Q")
    c.path("q /GS50 gs 0 G 8 w 20 20 m 280 20 l S Q")
    c.text("q /GS50 gs BT /F1 24 Tf 0 g 190 40 Td (ALPHA) Tj ET Q")
    c.label("gs-alpha: ca/CA 0.5 and 0.25")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ExtGState << /GS50 << /ca 0.5 /CA 0.5 >> /GS25 << /ca 0.25 >> >>")
    return d.build(), [c.expect_page()]


@sample(
    "gs-blend-modes",
    "extgstate",
    "混合模式 BM（8 种）",
    ["extgstate", "blend-mode", "BM"],
    "三条彩色横带作底，8 个灰蓝方块分别用 Multiply、Screen、Difference、Darken、Lighten、Exclusion、HardLight、Hue 叠加。"
    "每个方块跨三条底色，效果应各不相同；BM 丢失时都变成普通覆盖。",
    REF_GS,
)
def gs_blend_modes():
    d = Doc("gs-blend-modes")
    f1 = d.std_font()
    modes = ["Multiply", "Screen", "Difference", "Darken", "Lighten", "Exclusion", "HardLight", "Hue"]
    gs_entries = " ".join(f"/B{i} << /BM /{m} >>" for i, m in enumerate(modes))
    c = Content()
    c.path("1 0.3 0.3 rg 10 130 280 40 re f")
    c.path("0.3 0.8 0.3 rg 10 90 280 40 re f")
    c.path("0.3 0.3 1 rg 10 50 280 40 re f")
    for i, _m in enumerate(modes):
        x = 15 + i * 34
        c.path(f"q /B{i} gs 0.5 0.5 0.7 rg {x} 40 28 140 re f Q")
    c.text("BT /F1 6 Tf 0 g 15 30 Td [(Mult) -900 (Scrn) -900 (Diff) -1100 (Dark) -950 (Light) -950 (Excl) -1000 (HardL) -900 (Hue)] TJ ET")
    c.label("gs-blend-modes: 8 blend modes over 3 bands")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ExtGState << {gs_entries} >>")
    return d.build(), [c.expect_page()]


@sample(
    "gs-smask-luminosity",
    "extgstate",
    "软蒙版 SMask /Luminosity（透明度组 XObject + 渐变）",
    ["extgstate", "smask", "luminosity", "transparency-group", "shading"],
    "蓝色大块经亮度蒙版从左到右淡出（蒙版是一个灰度透明度组，里面画黑到白的轴向渐变）；顶部红线也套同一蒙版；"
    "底部绿块用 /SMask /None 关闭蒙版后应是实心。生成器丢掉 SMask 时蓝块、红线变成实心。",
    REF_GS + ["方案 13.1 设计软件导出"],
)
def gs_smask_luminosity():
    d = Doc("gs-smask-luminosity")
    f1 = d.std_font()
    sh = d.add(
        "<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [20 0 280 0] "
        "/Function << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> /Extend [true true] >>"
    )
    mask_form = d.form_xobject(
        (0, 0, 300, 200),
        b"0 0 300 200 re W n /ShM sh",
        resources=f"/Shading << /ShM {sh} >>",
        extra="/Group << /S /Transparency /CS /DeviceGray >>",
    )
    gs = f"/GSm << /SMask << /S /Luminosity /G {mask_form} /BC [0] >> >> /GSnone << /SMask /None >>"
    c = Content()
    c.path("q /GSm gs 0 0 1 rg 20 60 260 100 re f Q")
    c.path("q /GSm gs 1 0 0 RG 10 w 20 175 m 280 175 l S Q")
    c.path("q /GSm gs /GSnone gs 0 0.6 0 rg 20 20 260 30 re f Q")
    c.label("gs-smask-luminosity: blue block and red line fade left->right; green block solid")
    d.page(c.data(), f"/Font << /F1 {f1} >> /ExtGState << {gs} >>")
    return d.build(), [c.expect_page()]


@sample(
    "gs-transparency-group",
    "extgstate",
    "透明度组 Form XObject（隔离、挖空）与嵌套混合模式",
    ["transparency-group", "knockout", "isolated", "form-xobject", "blend-mode", "ca"],
    "左：组内两个 50% 透明方块，组设 /K true 挖空，重叠处只见上面的方块；整组再以 ca 0.7 绘制。"
    "右：同样两个方块不放进组，重叠处颜色叠加。组内嵌套一个 Multiply 混合的子 Form。",
    REF_GS + ["方案 3.2 拖动预览第 4 条"],
)
def gs_transparency_group():
    d = Doc("gs-transparency-group")
    f1 = d.std_font()
    inner = d.form_xobject(
        (0, 0, 60, 60),
        b"q /BMm gs 1 1 0 rg 0 0 60 60 re f Q",
        resources="/ExtGState << /BMm << /BM /Multiply >> >>",
        extra="/Group << /S /Transparency /CS /DeviceRGB >>",
    )
    group = d.form_xobject(
        (0, 0, 130, 130),
        b"q /GS50 gs 1 0 0 rg 0 30 90 90 re f 0 0 1 rg 40 0 90 90 re f Q q 1 0 0 1 35 35 cm /Fin Do Q",
        resources=f"/ExtGState << /GS50 << /ca 0.5 >> >> /XObject << /Fin {inner} >>",
        extra="/Group << /S /Transparency /CS /DeviceRGB /I true /K true >>",
    )
    c = Content()
    c.form("q /GS70 gs 1 0 0 1 15 35 cm /Fg Do Q")
    c.path("q /GS50 gs 1 0 0 rg 160 65 90 90 re f Q")
    c.path("q /GS50 gs 0 0 1 rg 200 35 90 90 re f Q")
    c.label("gs-transparency-group: left knockout group (ca .7, nested Multiply form); right plain 50% overlap", size=6)
    d.page(
        c.data(),
        f"/Font << /F1 {f1} >> /ExtGState << /GS70 << /ca 0.7 >> /GS50 << /ca 0.5 >> >> /XObject << /Fg {group} >>",
    )
    return d.build(), [c.expect_page()]


@sample(
    "gs-overprint",
    "extgstate",
    "叠印 OP / op / OPM 1（CMYK）",
    ["extgstate", "overprint", "OP", "OPM", "devicecmyk"],
    "左对：青块以 OP/op true、OPM 1 叠在品红块上，支持叠印预览的渲染器（Acrobat 叠印预览）在重叠处显示蓝色；"
    "右对：同样的两块不叠印，重叠处为青色。PDFium 不模拟叠印，两对看起来一样；关键是往返后 ExtGState 里 OP/op/OPM 是否还在。",
    REF_GS + ["方案 13.2 第 3 项 第二渲染器"],
)
def gs_overprint():
    d = Doc("gs-overprint")
    f1 = d.std_font()
    c = Content()
    c.path("0 1 0 0 k 20 40 100 100 re f")
    c.path("q /GSop gs 1 0 0 0 k 60 80 100 100 re f Q")
    c.path("0 1 0 0 k 170 40 100 100 re f")
    c.path("q /GSnop gs 1 0 0 0 k 210 80 100 100 re f Q")
    c.label("gs-overprint: left pair OP/op true OPM 1, right pair OP/op false")
    d.page(
        c.data(),
        f"/Font << /F1 {f1} >> /ExtGState << /GSop << /OP true /op true /OPM 1 >> /GSnop << /OP false /op false /OPM 0 >> >>",
    )
    return d.build(), [c.expect_page()]


@sample(
    "gs-text-knockout",
    "extgstate",
    "文字挖空 TK 与重叠字形的透明度",
    ["extgstate", "text-knockout", "TK", "ca", "Tc"],
    "两行 50% 透明、字距为负因而互相重叠的 O：第一行 TK false（重叠处颜色叠加更深），第二行 TK true（重叠处不叠加）。"
    "PDFium 可能忽略 TK，两行相同；往返后 TK 是否保留要看 ExtGState。",
    REF_GS,
)
def gs_text_knockout():
    d = Doc("gs-text-knockout")
    f1 = d.std_font("Helvetica-Bold")
    c = Content()
    c.text("q /GStk0 gs BT /F1 40 Tf -14 Tc 0 0 0.8 rg 20 120 Td (OOOOOOOO) Tj ET Q")
    c.text("q /GStk1 gs BT /F1 40 Tf -14 Tc 0 0 0.8 rg 20 50 Td (OOOOOOOO) Tj ET Q")
    c.text("BT /F1 7 Tf 0 g 20 100 Td (TK false, ca 0.5, Tc -14) Tj ET")
    c.text("BT /F1 7 Tf 0 g 20 30 Td (TK true, ca 0.5, Tc -14) Tj ET")
    d.page(
        c.data(),
        f"/Font << /F1 {f1} >> /ExtGState << /GStk0 << /TK false /ca 0.5 >> /GStk1 << /TK true /ca 0.5 >> >>",
    )
    return d.build(), [c.expect_page()]


@sample(
    "gs-line-params",
    "extgstate",
    "线宽、线端、接头、斜接限制、虚线：ExtGState 与操作符两种写法",
    ["extgstate", "line-width", "line-cap", "line-join", "miter-limit", "dash", "LW", "D"],
    "上两条折线外观应完全相同：第一条用 ExtGState（LW/LC/LJ/ML/D），第二条用 w/J/j/M/d 操作符。"
    "第三条线宽 0（最细线），第四、五条比较斜接限制 10 与 1 的尖角。",
    REF_GS,
)
def gs_line_params():
    d = Doc("gs-line-params")
    f1 = d.std_font()
    zig = "m 60 {t} l 100 {b} l 140 {t} l 180 {b} l 220 {t} l 260 {b} l"
    c = Content()
    c.path("q /GSln gs 0 0 0.7 RG 20 175 " + zig.format(t=175, b=145) + " S Q")
    c.path("q 6 w 1 J 1 j 1 M [12 6] 0 d 0 0 0.7 RG 20 130 " + zig.format(t=130, b=100) + " S Q")
    c.path("q /GShair gs 0 G 20 85 m 280 85 l S Q")
    c.path("q 8 w 0 j 10 M 0.6 0 0 RG 20 20 m 60 70 l 100 20 l S Q")
    c.path("q 8 w 0 j 1 M 0.6 0 0 RG 160 20 m 200 70 l 240 20 l S Q")
    c.label("gs-line-params: row1 ExtGState LW/LC/LJ/ML/D == row2 operators; hairline; miter 10 vs 1")
    d.page(
        c.data(),
        f"/Font << /F1 {f1} >> /ExtGState << /GSln << /LW 6 /LC 1 /LJ 1 /ML 1 /D [[12 6] 0] >> /GShair << /LW 0 >> >>",
    )
    return d.build(), [c.expect_page()]


# ---------------------------------------------------------------------------
# 文字
# ---------------------------------------------------------------------------


@sample(
    "text-tj-kerning",
    "text",
    "TJ 字距数组（负值拉开、正值回退、首项调整、混合十六进制串）",
    ["text", "TJ", "kerning"],
    "第 1 行用 TJ 拉开字距，第 2 行是同样文字的 Tj 参照；第 3 行正值让字母回退重叠；第 4 行数组以数字开头；"
    "第 5 行混合 (…) 与 <…> 串。生成器丢掉字距后第 1、3、4、5 行会挤回正常间距，与原文件像素差异明显。",
    REF_TEXT,
)
def text_tj_kerning():
    d = Doc("text-tj-kerning")
    f1 = d.std_font()
    c = Content()
    c.text("BT /F1 14 Tf 20 165 Td [(Kern) -500 (ing) 250 (TJ) -1000 (array)] TJ ET")
    c.text("BT /F1 14 Tf 20 140 Td (KerningTJarray  <- same glyphs via Tj) Tj ET")
    c.text("BT /F1 14 Tf 20 110 Td [(A) -2000 (B) -2000 (C) 1500 (D) 1200 (E)] TJ ET")
    c.text("BT /F1 14 Tf 20 80 Td [-3000 (Lead) 0 (ing) 500 (adj) -800 (ust) 0 (ed)] TJ ET")
    c.text("BT /F1 14 Tf 20 50 Td [(Mixed) <20> -400 <686578> ( and ) 300 <6C6974> (eral)] TJ ET")
    c.label("text-tj-kerning: compare line 1 (TJ) with line 2 (Tj) after round-trip", y=20)
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]


@sample(
    "text-tc-tw",
    "text",
    "字符间距 Tc 与单词间距 Tw",
    ["text", "Tc", "Tw", "TJ"],
    "6 行：无间距参照、Tc 3、Tw 10、负 Tc 加 Tw、TJ 与 Tc/Tw 同时作用、Tw 20（Tw 只作用于单字节码 32）。"
    "Tc/Tw 只存于文字状态、生成器不写出，往返后 2–6 行会恢复成参照行的间距。"
    "最底下的说明文字故意不重置 Tw：文字状态跨 BT…ET 泄漏（方案 2.3 开头），它也应保持 20 的词距。",
    REF_TEXT,
)
def text_tc_tw():
    d = Doc("text-tc-tw")
    f1 = d.std_font()
    c = Content()
    c.text("BT /F1 12 Tf 20 170 Td (Reference line no spacing) Tj ET")
    c.text("BT /F1 12 Tf 3 Tc 20 145 Td (Tc = 3 char spacing) Tj ET")
    c.text("BT /F1 12 Tf 0 Tc 10 Tw 20 120 Td (Tw = 10 word spacing here) Tj ET")
    c.text("BT /F1 12 Tf -1 Tc 4 Tw 20 95 Td (Negative Tc with Tw 4) Tj ET")
    c.text("BT /F1 12 Tf 2 Tc 6 Tw 20 70 Td [(TJ) -300 (plus Tc) -300 (and Tw)] TJ ET")
    c.text("BT /F1 12 Tf 0 Tc 20 Tw 20 45 Td <547720323020616666656374732062797465203332206F6E6C79> Tj ET")
    c.label("text-tc-tw: Tc/Tw live only in text state", y=20)
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]


@sample(
    "text-positioning-ops",
    "text",
    "Tz 水平缩放、Ts 上下标、TL/T*、' 与 \" 换行操作符",
    ["text", "Tz", "Ts", "TL", "T*", "quote", "dblquote"],
    "前两行 Tz 50%/150%；第三行一个 BT 内 4 次 Tj 带不同 Ts；第四段用 TL、T*、' 和 \" 换行（\" 同时设置 Tw 2、Tc 1）。"
    "Tz、Ts 在解析时折算进文字矩阵，应能保留；\" 设的 Tw/Tc 会丢。",
    REF_TEXT,
)
def text_positioning_ops():
    d = Doc("text-positioning-ops")
    f1 = d.std_font()
    c = Content()
    c.text("BT /F1 12 Tf 20 175 Td 50 Tz (Tz 50 horizontal scaling narrow) Tj ET")
    c.text("BT /F1 12 Tf 20 155 Td 150 Tz (Tz 150 wide) Tj ET")
    c.text("BT /F1 12 Tf 100 Tz 20 130 Td (Base) Tj 5 Ts (super) Tj -5 Ts (sub) Tj 0 Ts ( normal) Tj ET", n=4)
    c.text(
        "BT /F1 12 Tf 14 TL 20 100 Td (Line 1 set with Td, TL 14) Tj T* (Line 2 after T*) Tj "
        "(Line 3 via quote operator) ' 2 1 (Line 4 via double-quote: Tw 2 Tc 1) \" ET",
        n=4,
    )
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]


@sample(
    "text-render-modes",
    "text",
    "文字渲染模式 Tr 0–3",
    ["text", "Tr", "stroke-text", "invisible-text"],
    "Tr 0 红色填充、Tr 1 蓝色描边、Tr 2 填充加描边、Tr 3 不可见（仍是文字对象，可被选中、可复制）。"
    "往返后 Tr 3 的文字不应显示出来，Tr 1 的描边宽度（1 pt）不应变。",
    REF_TEXT + ["方案 5.1 读取"],
)
def text_render_modes():
    d = Doc("text-render-modes")
    f1 = d.std_font("Helvetica-Bold")
    c = Content()
    c.text("BT /F1 26 Tf 1 0 0 rg 0 0 1 RG 1 w 0 Tr 20 160 Td (Tr0 fill) Tj ET")
    c.text("BT /F1 26 Tf 1 0 0 rg 0 0 1 RG 1 w 1 Tr 20 120 Td (Tr1 stroke) Tj ET")
    c.text("BT /F1 26 Tf 1 0 0 rg 0 0 1 RG 1 w 2 Tr 20 80 Td (Tr2 fill+stroke) Tj ET")
    c.text("BT /F1 26 Tf 1 0 0 rg 3 Tr 20 40 Td (Tr3 invisible) Tj ET")
    c.label("text-render-modes: Tr 0,1,2,3 (last line must be invisible)", y=20)
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]


@sample(
    "text-clip-modes",
    "text",
    "文字裁剪模式 Tr 4–7 与裁剪后的图形",
    ["text", "Tr", "text-clip", "shading", "image", "clip"],
    "CLIP4 填蓝再作裁剪、透出渐变；CLIP5 描边红再裁剪、透出绿块；CLIP6 填描边后裁剪、透出橙块；CLIP7 只裁剪、透出图像；"
    "底部一个 BT 内两个 Tj 的字形并集作裁剪、透出紫带。文字裁剪块必须整体保留（方案 2.3 第 3 条），"
    "拆开或丢掉裁剪后，渐变/色块/图像会整块露出来。",
    REF_CLIP + REF_TEXT,
)
def text_clip_modes():
    d = Doc("text-clip-modes")
    f1 = d.std_font("Helvetica-Bold")
    sh = d.add(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 140 0] "
        "/Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >>"
    )
    im = d.image_xobject(2, 2, "/DeviceRGB", 8, bytes([255, 0, 0, 0, 200, 0, 0, 0, 255, 255, 200, 0]))
    c = Content()
    c.text("q BT /F1 34 Tf 4 Tr 0 0 1 rg 20 150 Td (CLIP4) Tj ET")
    c.shading("/Sh1 sh Q")
    c.text("q BT /F1 34 Tf 5 Tr 1 0 0 RG 1.5 w 160 150 Td (CLIP5) Tj ET")
    c.path("0 0.7 0 rg 150 140 140 50 re f Q")
    c.text("q BT /F1 34 Tf 6 Tr 0.5 g 0 G 1 w 20 90 Td (CLIP6) Tj ET")
    c.path("1 0.5 0 rg 10 80 140 50 re f Q")
    c.text("q BT /F1 34 Tf 7 Tr 160 90 Td (CLIP7) Tj ET")
    c.image("q 140 0 0 60 155 75 cm /Im1 Do Q Q")
    c.text("q BT /F1 22 Tf 7 Tr 20 25 Td (Two) Tj 90 0 Td (Blocks) Tj ET", n=2)
    c.path("0.6 0 0.6 rg 0 15 300 40 re f Q")
    c.label("text-clip-modes: Tr 4-7; shapes must only show through glyphs")
    d.page(c.data(), f"/Font << /F1 {f1} >> /Shading << /Sh1 {sh} >> /XObject << /Im1 {im} >>")
    return d.build(), [c.expect_page()]


@sample(
    "text-standard14",
    "text",
    "标准 14 字体（非嵌入）与 Differences 编码",
    ["text", "standard-14", "type1", "encoding-differences", "symbol", "zapfdingbats"],
    "Helvetica、Helvetica-Bold、Times-Roman、Times-BoldItalic、Courier、Symbol、ZapfDingbats 各一行，"
    "最后一行 Helvetica 用 Differences 把 a/b/c 映射成 bullet/dagger/section。非嵌入字体由 PDFium 内置字体替代；往返后编码字典应保留。",
    REF_TEXT + ["方案 5.2 修改已有文字"],
)
def text_standard14():
    d = Doc("text-standard14")
    fonts = {
        "F1": d.std_font("Helvetica"),
        "F2": d.std_font("Helvetica-Bold"),
        "F3": d.std_font("Times-Roman"),
        "F4": d.std_font("Times-BoldItalic"),
        "F5": d.std_font("Courier"),
        "F6": d.std_font("Symbol", encoding=""),
        "F7": d.std_font("ZapfDingbats", encoding=""),
        "F8": d.add(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica "
            "/Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [97 /bullet /dagger /section] >> >>"
        ),
    }
    lines = [
        ("F1", "Helvetica: SAMPLE FREIGHT CO. B/L SMPL-TEST-0001"),
        ("F2", "Helvetica-Bold: PACKING LIST (SAMPLE)"),
        ("F3", "Times-Roman: Shipper: SAMPLE Trading Ltd, 1 Test Road"),
        ("F4", "Times-BoldItalic: Consignee: SAMPLE Import GmbH"),
        ("F5", f"Courier: CNTR SMPU123456{iso6346_check_digit('SMPU123456')}  SEAL SMP000001"),
        ("F6", "abgd ABGD \\245 \\362 \\345"),
        ("F7", "3456 nopq \\110\\111"),
        ("F8", "abc -> bullet dagger section via Differences"),
    ]
    c = Content()
    for i, (f, s) in enumerate(lines):
        c.text(f"BT /{f} 11 Tf 0 g 15 {180 - i * 22} Td ({s}) Tj ET")
    res = " ".join(f"/{k} {v}" for k, v in fonts.items())
    d.page(c.data(), f"/Font << {res} >>")
    return d.build(), [c.expect_page()]


# 极小 TrueType 字体的字形（1000 upm，直线轮廓；外轮廓顺时针、内孔逆时针）
_GEO_GLYPHS: list[tuple[str, int, list[list[tuple[int, int]]]]] = [
    (" ", 300, []),
    ("-", 600, [[(100, 300), (100, 400), (500, 400), (500, 300)]]),
    ("1", 600, [[(250, 0), (250, 600), (150, 500), (100, 580), (300, 700), (350, 700), (350, 0)]]),
    ("A", 700, [[(50, 0), (350, 700), (650, 0)], [(350, 500), (200, 100), (500, 100)]]),
    ("D", 700, [[(50, 0), (50, 700), (450, 700), (650, 500), (650, 200), (450, 0)],
                [(150, 100), (400, 100), (550, 250), (550, 450), (400, 600), (150, 600)]]),
    ("F", 650, [[(50, 0), (50, 700), (650, 700), (650, 600), (150, 600), (150, 400), (500, 400), (500, 300), (150, 300), (150, 0)]]),
    ("G", 700, [[(50, 0), (50, 700), (650, 700), (650, 500), (550, 500), (550, 600), (150, 600), (150, 100), (550, 100),
                 (550, 300), (350, 300), (350, 400), (650, 400), (650, 0)]]),
    ("M", 800, [[(50, 0), (50, 700), (200, 700), (400, 350), (600, 700), (750, 700), (750, 0), (650, 0), (650, 500),
                 (400, 100), (150, 500), (150, 0)]]),
    ("P", 700, [[(50, 0), (50, 700), (500, 700), (650, 550), (650, 400), (500, 250), (150, 250), (150, 0)],
                [(150, 350), (450, 350), (550, 500), (450, 600), (150, 600)]]),
]  # fmt: skip


def _geo_font_bytes() -> bytes:
    return minimal_truetype(_GEO_GLYPHS, family="MagSynthGeo")


def _geo_widths() -> dict[str, int]:
    return {ch: adv for ch, adv, _ in _GEO_GLYPHS}


@sample(
    "text-truetype-embedded",
    "text",
    "嵌入的极小 TrueType 子集（程序生成的几何字形）",
    ["text", "truetype", "embedded-font", "fontfile2", "subset"],
    "字体只有 9 个直线轮廓字形（空格 - 1 A D F G M P），以 /FontFile2 嵌入、WinAnsi 编码、带 /Widths。"
    "大字 MAG PDF、小字 GAP-1；字形轮廓是否正确、宽度是否按 /Widths 排布；往返后字体流必须原样保留。",
    REF_TEXT + ["方案 5.3 排版", "P0 计划 V08"],
)
def text_truetype_embedded():
    d = Doc("text-truetype-embedded")
    f_label = d.std_font()
    font_bytes = _geo_font_bytes()
    ff = d.stream(f"/Length1 {len(font_bytes)}", font_bytes, flate=True)
    fd = d.add(
        "<< /Type /FontDescriptor /FontName /MAGSYN+MagSynthGeo /Flags 32 /FontBBox [0 -200 800 800] /ItalicAngle 0 "
        f"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 {ff} >>"
    )
    widths = _geo_widths()
    first, last = 32, 80
    w = " ".join(str(widths.get(chr(code), 0)) for code in range(first, last + 1))
    tt = d.add(
        f"<< /Type /Font /Subtype /TrueType /BaseFont /MAGSYN+MagSynthGeo /FirstChar {first} /LastChar {last} "
        f"/Widths [{w}] /Encoding /WinAnsiEncoding /FontDescriptor {fd} >>"
    )
    c = Content()
    c.text("BT /FT 48 Tf 0 0 0.6 rg 20 110 Td (MAG PDF) Tj ET")
    c.text("BT /FT 20 Tf 0.6 0 0 rg 20 60 Td [(GAP-1) -800 (AD) 400 (FM)] TJ ET")
    c.label("text-truetype-embedded: 9-glyph synthetic TrueType via FontFile2", font="F1")
    d.page(c.data(), f"/Font << /F1 {f_label} /FT {tt} >>")
    return d.build(), [c.expect_page()]


def _type3_font(d: Doc) -> str:
    """返回 Type3 字体对象引用。字形：a=红色实心方（d0 着色）、b=三角（d1）、c=内联图像蒙版的空心方（d1）。"""
    square = d.stream("", b"1000 0 d0 1 0 0 rg 100 100 800 800 re f")
    tri = d.stream("", b"1000 0 0 0 1000 1000 d1 0 0 m 1000 0 l 500 1000 l h f")
    dot = d.stream(
        "",
        b"1000 0 0 0 1000 1000 d1 q 1000 0 0 1000 0 0 cm BI /W 4 /H 4 /IM true /BPC 1 /F /AHx ID\n00606000 >\nEI Q",
    )
    procs = d.add(f"<< /square {square} /tri {tri} /dot {dot} >>")
    return str(
        d.add(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] "
            f"/CharProcs {procs} /Encoding << /Type /Encoding /Differences [97 /square /tri /dot] >> "
            "/FirstChar 97 /LastChar 99 /Widths [1000 1000 1000] /Resources << >> >>"
        )
    )


@sample(
    "text-type3",
    "text",
    "Type3 字体文字，同一内容流里前后都有其他对象",
    ["text", "type3", "d0", "d1", "inline-image"],
    "蓝条 → Type3 文字 abcabc（红方、黑三角、空心方）→ Helvetica 文字 → 绿条、红斜线，全部在一条内容流里。"
    "原版生成器遇到 Type3 文字直接返回、留下未闭合的 q 和 BT，后面的 Helvetica 文字、绿条、红线全部落进文字块，整条流损坏。",
    REF_TYPE3,
)
def text_type3():
    d = Doc("text-type3")
    f1 = d.std_font()
    t3 = _type3_font(d)
    c = Content()
    c.path("0 0 1 rg 20 160 100 25 re f")
    c.text("BT /T3 28 Tf 0 g 20 110 Td (abcabc) Tj ET")
    c.text("BT /F1 12 Tf 0 g 20 80 Td (Helvetica text after Type3 in the same stream) Tj ET")
    c.path("0 0.6 0 rg 20 20 260 30 re f")
    c.path("1 0 0 RG 3 w 150 160 m 280 185 l S")
    d.page(c.data(), f"/Font << /F1 {f1} /T3 {t3} >>")
    return d.build(), [c.expect_page()]


@sample(
    "text-type3-multistream",
    "text",
    "Type3 文字在第一条内容流，其他对象在第二条",
    ["text", "type3", "multi-stream"],
    "与 text-type3 相同的内容，但 Type3 文字单独放在 /Contents 数组的第一条流里，Helvetica 文字和色条在第二条。"
    "用来观察生成器的损坏是否只限于 Type3 所在的流（只重写被改过的流时第二条流应完好）。",
    REF_TYPE3 + ["方案 2.3 第 5 条"],
)
def text_type3_multistream():
    d = Doc("text-type3-multistream")
    f1 = d.std_font()
    t3 = _type3_font(d)
    s1 = Content()
    s1.path("0 0 1 rg 20 160 100 25 re f")
    s1.text("BT /T3 28 Tf 0 g 20 110 Td (abcabc) Tj ET")
    s2 = Content()
    s2.text("BT /F1 12 Tf 0 g 20 80 Td (Stream 2: Helvetica text after Type3 stream) Tj ET")
    s2.path("0 0.6 0 rg 20 20 260 30 re f")
    s2.path("1 0 0 RG 3 w 150 160 m 280 185 l S")
    d.page([s1.data(), s2.data()], f"/Font << /F1 {f1} /T3 {t3} >>")
    total = Content()
    for k in total.counts:
        total.counts[k] = s1.counts[k] + s2.counts[k]
    return d.build(), [total.expect_page()]


@sample(
    "text-cid-cjk-nonembedded",
    "text",
    "Type0 非嵌入中文字体：UniGB-UCS2-H 与 Identity-H（Adobe-GB1 CID）",
    ["text", "type0", "cid", "cjk", "non-embedded", "UniGB-UCS2-H", "Identity-H", "lang-zh"],
    "STSong-Light 非嵌入，依赖 PDFium 内置 CMap 与系统替代字体：前两行同一句中文分别用 UCS2 编码和 Identity-H 的 CID 编码写出，"
    "应显示一致；第三行英文经 UCS2；第四行 CID 文字加 Tc 3。往返后双字节字串与字体字典必须原样。",
    REF_TEXT + ["方案 5.3 排版", "方案 5.4 字体来源"],
)
def text_cid_cjk_nonembedded():
    d = Doc("text-cid-cjk-nonembedded")
    f1 = d.std_font()
    f_ucs = d.cjk_type0_font("/UniGB-UCS2-H")
    f_idh = d.cjk_type0_font("/Identity-H")
    zh = "样本货运 测试文本"
    cids = b"".join(adobe_gb1_cid(ch).to_bytes(2, "big") for ch in zh)
    en = "SAMPLE FREIGHT 2026"
    en_cids = b"".join(adobe_gb1_cid(ch).to_bytes(2, "big") for ch in en)
    ucs2 = lambda s: hexstr(s.encode("utf-16-be"))  # noqa: E731  UCS-2 码（无 BOM）
    c = Content()
    c.text(f"BT /FU 16 Tf 0 g 20 160 Td {ucs2(zh)} Tj ET")
    c.text(f"BT /FI 16 Tf 0 0 0.6 rg 20 130 Td {hexstr(cids)} Tj ET")
    c.text(f"BT /FU 12 Tf 0 g 20 100 Td {ucs2(en)} Tj ET")
    c.text(f"q BT /FI 12 Tf 3 Tc 0.6 0 0 rg 20 70 Td {hexstr(en_cids)} Tj ET Q")
    c.label("text-cid-cjk-nonembedded: STSong-Light via UniGB-UCS2-H (black) and Identity-H CIDs (blue)", y=30)
    d.page(c.data(), f"/Font << /F1 {f1} /FU {f_ucs} /FI {f_idh} >>")
    return d.build(), [c.expect_page()]


@sample(
    "text-cid-truetype-identity",
    "text",
    "Type0/CIDFontType2 嵌入 TrueType，Identity-H + CIDToGIDMap + ToUnicode",
    ["text", "type0", "cidfonttype2", "Identity-H", "embedded-font", "tounicode", "W-array"],
    "这是 V08 '改一行字' 计划采用的嵌入方式：字形号直接作 CID，/W 数组给宽度，ToUnicode 让文字可复制。"
    "大字 MAG PDF 应与 text-truetype-embedded 一致，复制出的文字应是 'MAG PDF'。",
    REF_TEXT + ["方案 5.3 排版", "P0 计划 V08"],
)
def text_cid_truetype_identity():
    d = Doc("text-cid-truetype-identity")
    f1 = d.std_font()
    font_bytes = _geo_font_bytes()
    ff = d.stream(f"/Length1 {len(font_bytes)}", font_bytes, flate=True)
    fd = d.add(
        "<< /Type /FontDescriptor /FontName /MAGSYN+MagSynthGeo /Flags 4 /FontBBox [0 -200 800 800] /ItalicAngle 0 "
        f"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 {ff} >>"
    )
    gid = {ch: i + 1 for i, (ch, _, _) in enumerate(_GEO_GLYPHS)}
    w_entries = " ".join(str(adv) for _, adv, _ in _GEO_GLYPHS)
    cidf = d.add(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MAGSYN+MagSynthGeo "
        f"/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {fd} "
        f"/DW 1000 /W [1 [{w_entries}]] /CIDToGIDMap /Identity >>"
    )
    bf = "\n".join(f"<{g:04X}> <{ord(ch):04X}>" for ch, g in gid.items())
    tounicode = d.stream(
        "",
        (
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n"
            "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n"
            "/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n"
            "1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n"
            f"{len(gid)} beginbfchar\n{bf}\nendbfchar\n"
            "endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n"
        ).encode("latin-1"),
    )
    f0 = d.add(
        f"<< /Type /Font /Subtype /Type0 /BaseFont /MAGSYN+MagSynthGeo /Encoding /Identity-H "
        f"/DescendantFonts [{cidf}] /ToUnicode {tounicode} >>"
    )

    def enc(s: str) -> str:
        return hexstr(b"".join(gid[ch].to_bytes(2, "big") for ch in s))

    c = Content()
    c.text(f"BT /F0 48 Tf 0 0 0.6 rg 20 110 Td {enc('MAG PDF')} Tj ET")
    c.text(f"BT /F0 20 Tf 0.6 0 0 rg 20 60 Td [{enc('GAP-1')} -800 {enc('AD')} 400 {enc('FM')}] TJ ET")
    c.label("text-cid-truetype-identity: CIDFontType2 + Identity-H + ToUnicode (V08 embedding path)")
    d.page(c.data(), f"/Font << /F1 {f1} /F0 {f0} >>")
    return d.build(), [c.expect_page()]
