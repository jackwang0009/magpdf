"""保真校验用的样本：在仓库默认的原版 PDFium 下也能修改原有对象的页（方案 2.3）。

放在单独的模块里、排在最后登记，前面 60 份样本的编号（S-0001…S-0060）不变。
"""

from __future__ import annotations

from common import PAGE_H, Content, Doc, sample

REF_FIDELITY = ["方案 2.3 保真校验", "P0 计划 V06", "V06 报告 12.2"]


@sample(
    "color-devicergb",
    "color",
    "DeviceRGB 填充与描边（rg / RG），布局同 color-devicegray",
    ["devicergb", "fill", "stroke", "fidelity-editable"],
    "三个色块、三个描边框和一行文字，全部用 rg / RG，没有灰度、CMYK 和专色。原版生成器把颜色写成 RGB，这一页重写前后"
    "对象、画面、颜色空间都不变，保真校验应当放行；编辑命令、分层渲染的端到端测试用它当“能改”的页。"
    "布局和 color-devicegray 一样，那一份在原版 PDFium 下因为灰度被写成 RGB 而只能叠加内容。",
    REF_FIDELITY,
)
def color_devicergb():
    d = Doc("color-devicergb")
    f1 = d.std_font()
    c = Content()
    c.path("0.8 0.2 0.2 rg 20 120 60 60 re f")
    c.path("0.2 0.6 0.3 rg 100 120 60 60 re f")
    c.path("0.2 0.3 0.8 rg 180 120 60 60 re f")
    c.path("0 0 0 RG 4 w 20 40 60 60 re S")
    c.path("0.5 0.1 0.6 RG 100 40 60 60 re S")
    c.path("0.9 0.8 0.3 rg 0.3 0.3 0.3 RG 180 40 60 60 re B")
    c.text("BT /F1 10 Tf 0.3 0.3 0.3 rg 20 20 Td (DeviceRGB fill rg / stroke RG) Tj ET")
    # Content.label 用 0 g 写字，这里要全是 RGB，自己写。
    c.text(f"BT /F1 7 Tf 0 0 0 rg 10 {PAGE_H - 12:g} Td (color-devicergb: rg/RG only; editable under stock PDFium) Tj ET")
    d.page(c.data(), f"/Font << /F1 {f1} >>")
    return d.build(), [c.expect_page()]
