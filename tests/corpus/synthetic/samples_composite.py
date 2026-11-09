"""货代单证风格的综合样本：把多种特性放进一页，接近真实单证的结构（方案 13.1 货代单证、11.5 条码、11.6 印章）。

全部内容虚构：公司名以 SAMPLE 开头，箱号前缀 SMPU，提单号 SMPL-…；编号校验位按《货代业务规则》第 1 节算法生成。
"""

from __future__ import annotations

from common import Content, Doc, sample
from pdfgen import code39_bars, hexstr, iso6346_check_digit, nums, pdf_string

REF_FREIGHT = ["方案 13.1 货代单证", "方案 11.5 条码", "方案 11.6 印章与签名", "货代业务规则 1.1/1.4"]


def gs1_check_digit(digits: str) -> int:
    total = sum(int(ch) * (3 if i % 2 == 0 else 1) for i, ch in enumerate(reversed(digits)))
    return (10 - total % 10) % 10


CONTAINER = "SMPU123456" + str(iso6346_check_digit("SMPU123456"))
SSCC = "09999999000000001" + str(gs1_check_digit("09999999000000001"))
BL_NO = "SMPL-TEST-0001"


def _ucs2(s: str) -> str:
    return hexstr(s.encode("utf-16-be"))


def _barcode(c: Content, text: str, x: float, y: float, height: float, narrow: float, color: str = "0 g") -> float:
    """Code 39 条码画成一个路径对象；返回条码总宽。"""
    bars = code39_bars(text, narrow, narrow * 2.5)
    ops = " ".join(f"{nums(x + bx, y, bw, height)} re" for bx, bw in bars)
    c.path(f"{color} {ops} f")
    last_x, last_w = bars[-1]
    return last_x + last_w


def _circle(cx: float, cy: float, r: float) -> str:
    k = 0.5523 * r
    return (
        f"{nums(cx + r, cy)} m {nums(cx + r, cy + k, cx + k, cy + r, cx, cy + r)} c "
        f"{nums(cx - k, cy + r, cx - r, cy + k, cx - r, cy)} c {nums(cx - r, cy - k, cx - k, cy - r, cx, cy - r)} c "
        f"{nums(cx + k, cy - r, cx + r, cy - k, cx + r, cy)} c h"
    )


@sample(
    "freight-label-composite",
    "composite",
    "4×6 英寸货运标签：Code 39 条码、箱号校验位、SSCC、旋转文字、专色印章、中文",
    ["freight", "label", "barcode", "code39", "container-number", "sscc", "rotate-text", "separation", "ca", "cjk", "lang-zh", "lang-en", "TJ"],
    f"288×432 pt 标签页。箱号 {CONTAINER}（ISO 6346 校验位正确）与 SSCC {SSCC}（GS1 模 10 校验位正确）各配一个 Code 39 条码（路径对象）；"
    "右侧竖排 SIDE A；左下专色红圈 SAMPLE 印章带 60% 透明；底部一行中文。往返后条码条宽不能变（否则扫描枪读不出），专色与透明度不能丢。",
    REF_FREIGHT + ["方案 11.7 标签", "货代业务规则 2.2 进仓标签"],
)
def freight_label_composite():
    d = Doc("freight-label-composite", subject="Synthetic shipping label, fictional data")
    f_b = d.std_font("Helvetica-Bold")
    f_r = d.std_font("Helvetica")
    f_c = d.std_font("Courier-Bold")
    f_zh = d.cjk_type0_font()
    cs_red = d.add("[/Separation /SampleStampRed /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 1 0] /N 1 >>]")
    W, H = 288, 432
    c = Content()
    c.path(f"0 G 1.5 w 8 8 {W - 16} {H - 16} re S")
    c.text(f"BT /FB 16 Tf 0 g 16 {H - 34} Td (SAMPLE LOGISTICS) Tj ET")
    c.text(f"BT /FR 7 Tf 0.3 g 16 {H - 46} Td [(TEST LABEL) -600 (NOT A REAL SHIPMENT) -600 (DATA IS FICTIONAL)] TJ ET")
    c.path(f"0 G 0.75 w 16 {H - 130} 256 72 re S 16 {H - 94} m 272 {H - 94} l S", n=2)
    c.text(f"BT /FR 6 Tf 0 g 20 {H - 66} Td (FROM) Tj ET")
    c.text(f"BT /FR 8 Tf 0 g 50 {H - 66} Td (SAMPLE Trading Co., 1 Test Road, Sample City) Tj ET")
    c.text(f"BT /FR 6 Tf 0 g 20 {H - 84} Td (TO) Tj ET")
    c.text(f"BT /FB 9 Tf 0 g 50 {H - 84} Td (SAMPLE Import GmbH, Musterweg 2, Sample Town) Tj ET")
    c.text(f"BT /FR 6 Tf 0 g 20 {H - 108} Td (B/L) Tj ET")
    c.text(f"BT /FC 10 Tf 0 g 50 {H - 108} Td ({BL_NO}) Tj ET")
    c.text(f"BT /FR 6 Tf 0 g 160 {H - 108} Td (CTN) Tj ET")
    c.text(f"BT /FC 10 Tf 0 g 185 {H - 108} Td [(1) -300 (/) -300 (12)] TJ ET")
    c.text(f"BT /FR 6 Tf 0 g 20 {H - 124} Td (CONTAINER) Tj ET")
    c.text(f"BT /FC 12 Tf 0 g 70 {H - 125} Td ({CONTAINER[:4]} {CONTAINER[4:10]} {CONTAINER[10]}) Tj ET")
    w1 = _barcode(c, CONTAINER, 24, 230, 50, 1.2)
    c.text(f"BT /FC 9 Tf 0 g {nums(24 + w1 / 2 - 30)} 218 Td (*{CONTAINER}*) Tj ET")
    c.text(f"BT /FR 6 Tf 0 g 20 196 Td (SSCC \\(00\\)) Tj ET")
    w2 = _barcode(c, SSCC, 24, 130, 50, 0.75)
    c.text(f"BT /FC 8 Tf 0 g {nums(24 + w2 / 2 - 45)} 118 Td ({SSCC}) Tj ET")
    c.text(f"BT 0 1 -1 0 {W - 18} 150 Tm /FB 14 Tf 0 g (SIDE A - KEEP DRY) Tj ET")
    c.path(f"1 0 0 0 k 16 60 60 40 re f")
    c.text(f"BT /FB 8 Tf 1 1 1 rg 22 76 Td (LOGO CMYK) Tj ET")
    c.path(f"q /GSa gs /CSred CS 1 SCN 2.5 w {_circle(220, 70, 32)} S Q")
    c.text(f"q /GSa gs BT 0.9659 0.2588 -0.2588 0.9659 196 66 Tm /FB 13 Tf /CSred cs 1 scn (SAMPLE) Tj ET Q")
    c.text(f"BT /FZ 11 Tf 0 g 90 70 Td {_ucs2('样本物流 测试标签')} Tj ET")
    c.text(f"BT /FR 6 Tf 0.4 g 16 20 Td (freight-label-composite: fictional shipper/consignee, valid check digits, Code 39 bars are path objects) Tj ET")
    d.page(
        c.data(),
        f"/Font << /FB {f_b} /FR {f_r} /FC {f_c} /FZ {f_zh} >> /ColorSpace << /CSred {cs_red} >> /ExtGState << /GSa << /ca 0.6 /CA 0.6 >> >>",
        mediabox=(0, 0, W, H),
    )
    return d.build(), [c.expect_page(W, H)]


@sample(
    "freight-bl-composite",
    "composite",
    "提单样式页：表格线、TJ 字段、CMYK 标志、专色透明印章、标记内容、Form 页脚、中文、虚线",
    ["freight", "bill-of-lading", "table", "TJ", "devicecmyk", "separation", "ca", "marked-content", "form-xobject", "cjk", "lang-zh", "lang-en", "dash", "barcode"],
    "300×440 pt。一条路径画出整张表格线；字段标签 6 pt、字段值 9 pt（部分用 TJ 调字距）；右上 CMYK 标志块；"
    "'SAMPLE' 专色圆章 50% 透明压在签名栏；箱号字段包在 /MagPDF 标记内容里（模拟组件存储）；页脚是 Form XObject；底部 Code 39 条码。"
    "这是最接近真实单证的综合样本，往返后任一特性丢失都会在像素比对里出现。",
    REF_FREIGHT + ["方案 11.4 在文件中的存储", "货代业务规则 1.3 提单号"],
)
def freight_bl_composite():
    d = Doc("freight-bl-composite", subject="Synthetic bill of lading layout, fictional data")
    f_b = d.std_font("Helvetica-Bold")
    f_r = d.std_font("Helvetica")
    f_c = d.std_font("Courier")
    f_zh = d.cjk_type0_font()
    cs_red = d.add("[/Separation /SampleStampRed /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 1 0] /N 1 >>]")
    footer = d.form_xobject(
        (0, 0, 300, 24),
        b"0.9 g 0 0 300 24 re f BT /FR 6 Tf 0.2 g 12 9 Td (SAMPLE SHIPPING LINE - synthetic test document - page footer drawn as Form XObject) Tj ET",
        resources=f"/Font << /FR {f_r} >>",
    )
    W, H = 300, 440
    c = Content()
    # 表格线：一个路径对象
    grid = [
        f"12 {H - 60} m 288 {H - 60} l", f"12 {H - 110} m 288 {H - 110} l", f"12 {H - 160} m 288 {H - 160} l",
        f"12 {H - 200} m 288 {H - 200} l", f"12 {H - 290} m 288 {H - 290} l", f"12 {H - 340} m 288 {H - 340} l",
        f"150 {H - 60} m 150 {H - 160} l", f"100 {H - 200} m 100 {H - 290} l", f"190 {H - 200} m 190 {H - 290} l",
        f"12 {H - 60} m 12 {H - 340} l", f"288 {H - 60} m 288 {H - 340} l", f"12 {H - 340} m 288 {H - 340} l",
    ]  # fmt: skip
    c.path("0 G 0.6 w " + " ".join(grid) + " S")
    c.text(f"BT /FB 14 Tf 0 g 12 {H - 30} Td [(BILL) -250 (OF) -250 (LADING)] TJ ET")
    c.text(f"BT /FR 7 Tf 0.3 g 12 {H - 42} Td (SAMPLE SHIPPING LINE   -   NOT NEGOTIABLE   -   TEST) Tj ET")
    c.path(f"1 0.6 0 0 k 236 {H - 48} 52 36 re f 0 0 0 1 k 236 {H - 48} 52 6 re f", n=2)
    c.text(f"BT /FB 7 Tf 1 1 1 rg 244 {H - 30} Td (CMYK) Tj ET")

    def field(x: float, y: float, label: str, value: str, font: str = "FR", size: float = 9, tj: str | None = None) -> None:
        c.text(f"BT /FR 5.5 Tf 0.35 g {nums(x, y)} Td ({label}) Tj ET")
        if tj:
            c.text(f"BT /{font} {size} Tf 0 g {nums(x, y - 12)} Td {tj} TJ ET")
        else:
            c.text(f"BT /{font} {size} Tf 0 g {nums(x, y - 12)} Td {pdf_string(value)} Tj ET")

    field(16, H - 70, "SHIPPER", "SAMPLE Trading Co.")
    c.text(f"BT /FR 8 Tf 0 g 16 {H - 96} Td (1 Test Road, Sample City) Tj ET")
    field(154, H - 70, "B/L NO.", BL_NO, font="FC", size=10)
    field(154, H - 95, "BOOKING", "", tj="[(BK) -400 (SMP) -400 (000) 200 (123)]")
    field(16, H - 120, "CONSIGNEE", "SAMPLE Import GmbH")
    c.text(f"BT /FR 8 Tf 0 g 16 {H - 146} Td (Musterweg 2, Sample Town) Tj ET")
    field(154, H - 120, "VESSEL / VOYAGE", "", tj="[(SAMPLE) -300 (STAR) -600 (V.) -200 (001E)]")
    field(16, H - 170, "PORT OF LOADING", "SAMPLE PORT A")
    field(154, H - 170, "PORT OF DISCHARGE", "SAMPLE PORT B")
    c.text(f"BT /FR 5.5 Tf 0.35 g 16 {H - 210} Td (MARKS & NOS / CONTAINER) Tj ET")
    c.text(f"BT /FR 5.5 Tf 0.35 g 104 {H - 210} Td (DESCRIPTION OF GOODS) Tj ET")
    c.text(f"BT /FR 5.5 Tf 0.35 g 194 {H - 210} Td (GROSS WEIGHT / MEASUREMENT) Tj ET")
    c.text(
        f"/MagPDF << /Comp /Text /Ver 1 /Field (container) >> BDC BT /FC 9 Tf 0 g 16 {H - 226} Td "
        f"({CONTAINER[:4]} {CONTAINER[4:10]} {CONTAINER[10]}) Tj ET EMC"
    )
    c.text(f"BT /FC 8 Tf 0 g 16 {H - 240} Td (SEAL SMP000001) Tj ET")
    c.text(f"BT /FR 8 Tf 0 g 16 {H - 254} Td (40HQ / 45G1) Tj ET")
    c.text(f"BT /FR 8 Tf 0 g 104 {H - 226} Td (12 CARTONS) Tj ET")
    c.text(f"BT /FR 8 Tf 0 g 104 {H - 240} Td [(SAMPLE) -400 (GOODS) -400 (S.T.C.)] TJ ET")
    c.text(f"BT /FZ 9 Tf 0 g 104 {H - 256} Td {_ucs2('样本货物 测试用途')} Tj ET")
    c.text(f"BT /FR 8 Tf 0 g 194 {H - 226} Td (1,234.50 KGS) Tj ET")
    c.text(f"BT /FR 8 Tf 0 g 194 {H - 240} Td (12.345 CBM) Tj ET")
    field(16, H - 300, "FREIGHT & CHARGES", "FREIGHT PREPAID")
    field(154, H - 300, "PLACE AND DATE OF ISSUE", "SAMPLE CITY")
    c.text(f"BT /FR 5.5 Tf 0.35 g 16 {H - 350} Td (SIGNED FOR THE CARRIER) Tj ET")
    c.path(f"q [3 2] 0 d 0.3 G 0.5 w 16 {H - 372} m 150 {H - 372} l S Q")
    c.path(f"q /GSa gs /CSred CS 1 SCN 2 w {_circle(230, H - 362, 22)} S Q")
    c.text(f"q /GSa gs BT 0.9659 0.2588 -0.2588 0.9659 212 {H - 366} Tm /FB 10 Tf /CSred cs 1 scn (SAMPLE) Tj ET Q")
    w = _barcode(c, BL_NO.replace("-", " "), 16, 36, 24, 0.8)
    c.text(f"BT /FC 6 Tf 0 g {nums(16 + w / 2 - 30)} 28 Td ({BL_NO}) Tj ET")
    c.form("q 1 0 0 1 0 0 cm /Ft Do Q")
    d.page(
        c.data(),
        f"/Font << /FB {f_b} /FR {f_r} /FC {f_c} /FZ {f_zh} >> /ColorSpace << /CSred {cs_red} >> "
        f"/ExtGState << /GSa << /ca 0.5 /CA 0.5 >> >> /XObject << /Ft {footer} >>",
        mediabox=(0, 0, W, H),
    )
    return d.build(), [c.expect_page(W, H)]
