"""样本定义：每类字体写法一页，行文与要改的句子。

句子设计（三级回退各自对样本的预期）：

* ``line1`` 是单据标题/正文一行，改它就是 V08 的主体；
* ``chars`` 是这一行里**原字体子集一定包含**的字符集合（用来构造子集），
  ``tier1`` 用这行里已有的字符组合，``tier2`` 用同一款字体完整版里才有的字，
  ``tier3`` 换成系统里另一种字体才有的字（例如宋体 → 仿宋）。

``CJK_SENTENCE`` 等常量只出现在样本里，测试与报告用行号定位，不打印文本内容。
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class SampleSpec:
    name: str
    lang: str  # zh / en
    # 原字体：系统字体文件名与面索引（未嵌入的样本也用它渲染，注释里说明）
    font_file: str
    font_index: int = 0
    kind: str = "cidfonttype2"  # cidfonttype2 / cidfonttype0 / simple
    embedded: str = "subset"  # subset / full / none
    # 原字体在 PDF 里的 BaseFont 名（可以是真实字体名，也可以是标准替代名）
    base_font: str = ""
    # 页面上的行：每行 (y, 字号, 文本)
    lines: list[tuple[float, float, str]] = field(default_factory=list)
    # 子集要包含的字符
    subset_chars: str = ""
    # 第 1/2/3 级各自要改成的文本（在同一行里替换）
    tier1_text: str = ""
    tier2_text: str = ""
    tier3_text: str = ""
    # 这一级期望落在哪一级（评测时对比）
    expected: tuple[str, str, str] = ("tier1", "tier2", "tier3")
    # 简单字体用：/Differences 的起始字符码与字形名（None 时用 WinAnsi 默认）
    simple_encoding: str = "WinAnsiEncoding"
    # 额外页数（第二页放同样的行，测试多页定位）
    extra_pages: int = 0
    note: str = ""


ZH_FONT = "simfang.ttf"  # 仿宋：合同正文的常见字体
ZH_FONT2 = "simsun.ttc"  # 宋体（TTC 第 0 面）
ZH_HEI = "simhei.ttf"
EN_SERIF = "times.ttf"
EN_SANS = "arial.ttf"

CJK_T1 = "合同甲合同乙"
CJK_T2 = "合同价款"
CJK_T3 = "货物验收"


def zh_samples() -> list[SampleSpec]:
    return [
        SampleSpec(
            "zh-tt-subset",
            "zh",
            ZH_FONT,
            kind="cidfonttype2",
            embedded="subset",
            base_font="ABCDEE+SimFang",
            lines=[(750, 14, "合同甲方乙方"), (720, 10.5, "合同甲方乙方应按时付款")],
            subset_chars="合同甲方乙方应按时付款号和：",
            tier1_text="合同乙方甲方",
            tier2_text="合同价款",
            tier3_text="货物验收",
            note="子集嵌入的 TrueType（CIDFontType2），第 2/3 级需要完整字体",
        ),
        SampleSpec(
            "zh-tt-full",
            "zh",
            ZH_HEI,
            kind="cidfonttype2",
            embedded="full",
            base_font="SimHei",
            lines=[(750, 14, "合同甲方乙方"), (720, 10.5, "合同甲方乙方应按时付款")],
            subset_chars="",
            tier1_text="合同乙方甲方",
            tier2_text="验收货物",
            tier3_text="货物验收",
            note="完整嵌入的 TrueType：第 2 级从嵌入数据补字形",
        ),
        SampleSpec(
            "zh-cff-subset",
            "zh",
            ZH_FONT2,
            kind="cidfonttype0",
            embedded="subset",
            base_font="FGHIJK+SimSun",
            lines=[(750, 14, "合同甲方乙方"), (720, 10.5, "合同甲方乙方和号")],
            subset_chars="合同甲方乙方和号应按时付款",
            tier1_text="合同乙方甲方",
            tier2_text="合同价款",
            tier3_text="货物验收",
            note="把宋体轮廓以 CFF 形式嵌入（CIDFontType0），字形 ID 与 TrueType 版一致",
        ),
        SampleSpec(
            "zh-notembedded",
            "zh",
            ZH_FONT2,
            kind="cidfonttype0",
            embedded="none",
            base_font="STSong-Light",
            lines=[(750, 14, "合同甲方乙方"), (720, 10.5, "合同甲方乙方和号")],
            tier1_text="合同乙方甲方",
            tier2_text="合同价款",
            tier3_text="货物验收",
            note="引用标准字体名 STSong-Light、不嵌入；第 2 级靠系统里的同名字体（宋体）",
        ),
        SampleSpec(
            "zh-simple",
            "zh",
            ZH_FONT,
            kind="simple",
            embedded="subset",
            base_font="ABCDEE+SimFang",
            simple_encoding="WinAnsiEncoding",
            lines=[(750, 14, "CONTRACT A"), (720, 10.5, "CONTRACT AB NO: 123")],
            subset_chars="CONTRACT BNO:123 ",
            tier1_text="CONTRACT B",
            tier2_text="CONTRACT C",
            tier3_text="CONTRACT D",
            note="简单字体（TrueType + WinAnsi）—— 单字节编码装不下汉字，第 3 级换字体",
        ),
        SampleSpec(
            "zh-twopages",
            "zh",
            ZH_FONT,
            kind="cidfonttype2",
            embedded="subset",
            base_font="ABCDEE+SimFang",
            lines=[(750, 14, "合同甲方乙方"), (700, 9, "付款和号 123")],
            subset_chars="合同甲方乙方应按时付款和号：0123456789",
            tier1_text="合同乙方甲方",
            tier2_text="合同价款",
            tier3_text="货物验收",
            extra_pages=1,
            note="两页，第 2 页有同样的行，验证页码/行号定位",
        ),
    ]


def en_samples() -> list[SampleSpec]:
    return [
        SampleSpec(
            "en-tt-subset",
            "en",
            EN_SERIF,
            kind="cidfonttype2",
            embedded="subset",
            base_font="LMNOPQ+TimesNewRoman",
            lines=[(750, 14, "Invoice No: ABC-123"), (720, 10, "Total amount payable")],
            subset_chars="Invoice No: ABC-123Total amountpble",
            tier1_text="Invoice No: BCA-321",
            tier2_text="Invoice No: XYZ-777",
            tier3_text="Shipment advice",
            note="子集 Times：第 2 级从系统 Times 补字形，第 3 级仍 Times（字形都能补上）",
        ),
        SampleSpec(
            "en-tt-full",
            "en",
            EN_SANS,
            kind="cidfonttype2",
            embedded="full",
            base_font="Arial",
            lines=[(750, 14, "Invoice No: ABC-123"), (720, 10, "Total amount payable")],
            tier1_text="Invoice No: BCA-321",
            tier2_text="Invoice No: QZ-9",
            tier3_text="Shipment advice",
            note="完整 Arial：第 2 级从嵌入数据补字形",
        ),
        SampleSpec(
            "en-notembedded",
            "en",
            EN_SERIF,
            kind="cidfonttype2",
            embedded="none",
            base_font="TimesNewRoman",
            lines=[(750, 14, "Invoice No: ABC-123"), (720, 10, "Total amount payable")],
            tier1_text="Invoice No: BCA-321",
            tier2_text="Invoice No: QZ-9",
            tier3_text="Shipment advice",
            note="未嵌入、引用 TimesNewRoman；系统里同名的 times.ttf 不叫这个名，第 2/3 级都靠匹配",
        ),
        SampleSpec(
            "en-simple",
            "en",
            EN_SERIF,
            kind="simple",
            embedded="subset",
            base_font="LMNOPQ+TimesNewRoman",
            lines=[(750, 14, "Invoice No: ABC-123"), (720, 10, "Total amount payable")],
            subset_chars="Invoice No: ABC-123Total amountpble",
            tier1_text="Invoice No: BCA-321",
            tier2_text="Invoice No: KZ-8",
            tier3_text="Shipment advice",
            note="简单字体（Times 子集 + WinAnsi）",
        ),
    ]
