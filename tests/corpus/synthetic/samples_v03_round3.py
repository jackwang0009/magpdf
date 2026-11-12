"""V03 第三轮（修内容流生成器）补的样本：原有样本覆盖不到的写回情况。

放在单独的模块里、排在最后登记，前面 58 份样本的编号（S-0001…S-0058）不变。
"""

from __future__ import annotations

from common import Content, Doc, sample

REF_GS = ["方案 2.1 ExtGState", "P0 计划 V03 第 4 项", "V03 第三轮 第 3 项"]

# ---------------------------------------------------------------------------
# 扩展图形状态
# ---------------------------------------------------------------------------


@sample(
    "gs-smask-ctm",
    "extgstate",
    "软蒙版在变换过的坐标系里设置（cm 之后 gs）：亮度蒙版、Alpha 蒙版",
    ["extgstate", "smask", "luminosity", "alpha-mask", "cm", "transparency-group"],
    "软蒙版按 gs 生效那一刻的 CTM 摆放。左上：缩放平移之后设亮度蒙版，蓝块只在蒙版的方框里可见，方框中间一块更淡；"
    "右上：平移之后设蒙版、再旋转 30° 画红色方块，方块转了，蒙版的方框不转；左下：Alpha 蒙版（三角形）里的绿块；"
    "右下：压扁的蒙版下的紫色文字 MASK，两头被蒙版截掉。重写时要是在单位矩阵下写 gs，四处的可见范围都会挪到页面"
    "左下角 100×100 的区域里。",
    REF_GS,
)
def gs_smask_ctm():
    d = Doc("gs-smask-ctm")
    f1 = d.std_font()
    f2 = d.std_font("Helvetica-Bold")
    # 亮度蒙版：外框白（全显）、中间 0.4 灰（显 40%），框外是组的黑底（不显）
    lum_form = d.form_xobject(
        (0, 0, 100, 100),
        b"1 g 10 10 80 80 re f 0.4 g 30 30 40 40 re f",
        extra="/Group << /S /Transparency /CS /DeviceGray >>",
    )
    # Alpha 蒙版：只看组里画过的地方（三角形）
    alpha_form = d.form_xobject(
        (0, 0, 100, 100),
        b"0 g 0 0 m 100 0 l 50 100 l h f",
        extra="/Group << /S /Transparency /CS /DeviceGray >>",
    )
    gs = (
        f"/GSlum << /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G {lum_form} >> >> "
        f"/GSalpha << /Type /ExtGState /SMask << /Type /Mask /S /Alpha /G {alpha_form} >> >>"
    )
    c = Content()
    c.path("0.9 g 10 15 280 170 re f")
    c.path("q 1.2 0 0 0.6 20 115 cm /GSlum gs 0 0 1 rg 0 0 100 100 re f Q")
    c.path("q 1 0 0 1 170 85 cm /GSlum gs 0.866 0.5 -0.5 0.866 40 5 cm 1 0 0 rg 0 0 70 70 re f Q")
    c.path("q 0.8 0 0 0.8 20 25 cm /GSalpha gs 0 0.6 0 rg 0 0 100 100 re f Q")
    c.text("q 1 0 0 0.5 150 30 cm /GSlum gs BT /F2 34 Tf 0.5 0 0.5 rg 4 34 Td (MASK) Tj ET Q")
    c.label("gs-smask-ctm: soft masks set under scaled / translated CTMs; mask stays put when the object rotates", size=6)
    d.page(c.data(), f"/Font << /F1 {f1} /F2 {f2} >> /ExtGState << {gs} >>")
    return d.build(), [c.expect_page()]


@sample(
    "gs-stacked-other-keys",
    "extgstate",
    "多个 ExtGState 叠加；SA、AIS、SM、FL、RI、TR2、BG2、UCR2、HT 等条目；ri、i 操作符",
    ["extgstate", "stacked-gs", "SA", "AIS", "SM", "FL", "RI", "TR2", "BG2", "UCR2", "HT", "ri", "flatness", "LW"],
    "第一行：左块先后套两个字典（后一个只盖掉 ca，另加 Multiply），压在蓝带上应是 80% 的正片叠底；右块的第二个字典把第一个的"
    "条目全部盖掉，应是不透明的黄。第二行：TR2 是反相函数，红块显示成青色；再套 /TR2 /Default 的右块仍是红；右边的深灰粗线"
    "经反相变浅。第三行：字典给 LW 8、圆线端，上面一条被 w 改成 2，下面一条保持 8。右下两块用了 ri、i 操作符"
    "（PDFium 渲染不看这两个参数）。重写之后字典里的每个条目都应该还在；只合成 ca/CA/BM 的话，青块变红、粗细和线端都不对。",
    REF_GS,
)
def gs_stacked_other_keys():
    d = Doc("gs-stacked-other-keys")
    f1 = d.std_font()
    invert = d.add("<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>")
    gs = (
        "/GSa << /Type /ExtGState /ca 0.5 /SA true /OP true /RI /Perceptual /FL 0.5 /SM 0.02 >> "
        "/GSb << /ca 0.8 /BM /Multiply /AIS false >> "
        "/GSa2 << /Type /ExtGState /ca 1 /SA false /OP false /RI /RelativeColorimetric /FL 1 /SM 0 >> "
        f"/GStr << /TR2 {invert} /BG2 /Default /UCR2 /Default /HT /Default >> "
        "/GStr0 << /TR2 /Default >> "
        "/GSlw << /LW 8 /LC 1 /ML 3 >>"
    )
    c = Content()
    c.path("0.2 0.6 1 rg 10 150 280 20 re f")
    c.path("q /GSa gs /GSb gs 1 0.8 0 rg 20 135 120 45 re f Q")
    c.path("q /GSa gs /GSa2 gs 1 0.8 0 rg 160 135 120 45 re f Q")
    c.path("q /GStr gs 1 0 0 rg 20 85 80 40 re f Q")
    c.path("q /GStr gs /GStr0 gs 1 0 0 rg 110 85 80 40 re f Q")
    c.path("q /GStr gs 0.2 0.2 0.2 RG 6 w 205 90 m 280 120 l S Q")
    c.path("q /GSlw gs 2 w 0 0 0.7 RG 20 65 m 140 65 l S Q")
    c.path("q /GSlw gs 0 0 0.7 RG 20 30 m 80 50 l 140 30 l S Q")
    c.path("q /Saturation ri 0.5 i 0 0.5 0 rg 160 40 m 200 80 250 80 280 40 c f Q")
    c.path("q /GSa gs /AbsoluteColorimetric ri 2 i 0.5 0 0.5 rg 160 22 120 10 re f Q")
    c.label("gs-stacked-other-keys: stacked dictionaries, TR2 (inverting), LW vs w, ri / i operators", size=6)
    d.page(c.data(), f"/Font << /F1 {f1} >> /ExtGState << {gs} >>")
    return d.build(), [c.expect_page()]
