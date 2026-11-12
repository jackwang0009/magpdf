#!/usr/bin/env python3
"""生成 OCR 自测用的“打印体单证”合成图（V09）。

内容全部虚构（货代单证、合同、发票、通知里常见的字段和句子），不含任何真实文件。图片不入库
（out/ 在 .gitignore 里）。

每个“底页”先按 600 dpi 画一张母版（A4，4960×7016），再由母版出各种变体。同一个底页的各个变体文字、
版面完全相同，变体之间的差别只来自分辨率和退化方式：

  扫描（300 dpi、200 dpi 各一套，母版按 2、3 倍取平均缩小，相当于同一张纸用两种分辨率扫）
    clean     不做别的处理
    noise     轻微模糊（高斯 0.6 像素）、对比度降低、灰度噪声
    rot       整页歪 ±1.0° 或 ±1.5°（在母版上旋转）
    noiserot  两者都有
  方向（300 dpi，clean）
    up180     整页倒置
    cw90      整页顺时针转 90°
    ccw90     整页逆时针转 90°
  手机拍照的近似（只是近似，不是真照片）
    photo_12mp  A4 页透视变形后放进 3024×4032 的画面（深色桌面），光照不匀、偏暖、轻微失焦、
                传感器噪声，JPEG 质量 75
    photo_3mp   上一张缩到 1512×2016，JPEG 质量 60

底页的种类（计划 V09 要的六种文档各有一个对应的模板，另加中英对照的单证）：
  zh     contract（合同）、invoice（发票）、notice（公文）轮流
  en     bl（提单）、packing（装箱单）、invoice（发票）轮流
  mixed  bl（中英对照提单）、invoice（中英对照费用清单）轮流

输出：
  out/images/<语言>_<序号>_<变体>_<分辨率>.png|jpg
  out/Label.txt   PPOCRLabel 格式（《测试样本收集与脱敏规范》7.1），每张图一行
  out/langs.json  图像主干名 → 页面语言
  out/meta.json   生成参数；每个底页的模板、字体、行距，每条标注的行类别、字体、字号、是否被印章压住；
                  每张图属于哪个底页、哪个变体
  out/speed/      量速度用的三页（中文合同、英文提单、30 行货物的英文装箱单），noise、300 dpi，
                  各有一张 PNG 和一份 PDF（整页一张灰度 JPEG，当作扫描仪出的文件）；另有 Label.txt

噪声用给定种子的伪随机数生成，同一版 Pillow、同一套字体下每次生成的图逐字节相同。

用法：python gen_synthetic.py [--out out] [--pages-per-lang 30] [--seed 7] [--sets scan300 scan200 ...]
依赖：Pillow（tests/requirements-test.txt）。字体用 Windows 自带的。
"""
from __future__ import annotations

import argparse
import json
import math
import os
import random
from collections.abc import Callable
from concurrent.futures import ProcessPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont
from PIL import __version__ as PILLOW_VERSION

MASTER_DPI = 600
MASTER_W, MASTER_H = 4960, 7016  # A4，600 dpi
U = MASTER_DPI / 72.0  # 母版上 1 pt 合多少像素
MARGIN_PT = 48.0
TOP_PT = 52.8
LABEL_PAD_PT = 1.44  # 标注框比文字外沿多留的边
SEGMENT_GAP_PT = 43.2  # 同一行里两个字段之间的空当（约 1.5 cm）
PHOTO_W, PHOTO_H = 3024, 4032  # 1200 万像素，3:4

FONT_DIR = Path(os.environ.get("WINDIR", r"C:\Windows")) / "Fonts"
FONTS = {
    "msyh": "msyh.ttc",  # 微软雅黑
    "simsun": "simsun.ttc",  # 宋体
    "simhei": "simhei.ttf",  # 黑体
    "simfang": "simfang.ttf",  # 仿宋
    "simkai": "simkai.ttf",  # 楷体
    "arial": "arial.ttf",
    "times": "times.ttf",
    "cour": "cour.ttf",
    "calibri": "calibri.ttf",
}
ZH_FONTS = ["msyh", "simsun", "simhei", "simfang", "simkai"]
EN_FONTS = ["arial", "times", "cour", "calibri"]
LEADINGS = [1.35, 1.5, 1.7]
DOCS = {"zh": ["contract", "invoice", "notice"], "en": ["bl", "packing", "invoice"], "mixed": ["bl", "invoice"]}
SCAN_VARIANTS = ["clean", "noise", "rot", "noiserot"]
ALL_SETS = ["scan300", "scan200", "orient", "photo", "speed"]

BLACK = (0, 0, 0)
RED = (200, 30, 30)
Quad = list[list[float]]

# ---------------------------------------------------------------------------------------------
# 内容：全部虚构
# ---------------------------------------------------------------------------------------------

PORTS = ["SHANGHAI", "NINGBO", "SHENZHEN", "QINGDAO", "ROTTERDAM", "HAMBURG", "LOS ANGELES", "SINGAPORE", "FELIXSTOWE", "BUSAN"]
PORTS_ZH = ["上海", "宁波", "深圳", "青岛", "鹿特丹", "汉堡", "洛杉矶", "新加坡", "釜山", "天津"]
GOODS = ["STAINLESS STEEL PIPES", "GARDEN FURNITURE", "LED LAMPS", "CERAMIC TILES", "AUTO PARTS", "COTTON T-SHIRTS", "PLASTIC TOYS", "SOLAR PANELS"]
GOODS_ZH = ["不锈钢管", "园艺家具", "LED 灯具", "瓷砖", "汽车配件", "纯棉T恤", "塑料玩具", "太阳能板"]
COMPANIES_EN = [
    "SUNRISE TRADING CO., LTD.",
    "BLUE HARBOR LOGISTICS GMBH",
    "NORTHWIND IMPORTS INC.",
    "GOLDEN BRIDGE SUPPLY LLC",
    "EVERGREEN TEXTILE PTE LTD",
    "PACIFIC QUAY FORWARDING B.V.",
    "RED MAPLE HARDWARE S.A.",
]
COMPANIES_ZH = [
    "宏达国际货运代理有限公司",
    "蓝港物流（上海）有限公司",
    "金桥进出口贸易有限公司",
    "长风纺织品制造有限公司",
    "远洋船务代理股份有限公司",
    "瑞丰五金制品（宁波）有限公司",
    "滨海新区恒通供应链管理有限公司",
]
ADDRS_ZH = [
    "上海市浦东新区世纪大道1568号12层",
    "宁波市北仑区明州西路455号3幢",
    "深圳市福田区深南中路2018号兴华大厦B座906室",
    "青岛市市南区香港中路76号",
    "天津市滨海新区第二大街29号",
]
ADDRS_EN = [
    "No. 1568 Century Avenue, Pudong, Shanghai 200122, China",
    "Unit 906, Block B, 2018 Shennan Road, Futian, Shenzhen 518031",
    "Waalhaven Z.z. 19, 3089 JH Rotterdam, The Netherlands",
    "4400 Harbor Gateway Blvd, Suite 210, Long Beach, CA 90802",
    "22 Tuas Avenue 6, #03-15, Singapore 639309",
]
BANKS_ZH = ["工商银行上海外高桥支行", "建设银行宁波北仑支行", "招商银行深圳蛇口支行", "中国银行青岛市南支行"]
BANKS_EN = ["BANK OF EASTERN CHINA, SHANGHAI BRANCH", "HARBOUR COMMERCIAL BANK PLC", "FIRST MERIDIAN BANK N.A."]
SURNAMES = "王李张刘陈杨黄赵吴周徐孙马朱胡郭何林罗高"
GIVEN = ["建国", "秀英", "伟", "敏", "海燕", "志强", "丽华", "晓东", "思远", "雨桐", "国庆", "婷婷"]
PEOPLE_EN = ["David Miller", "Sophie van Dijk", "Kenji Watanabe", "Maria Gonzalez", "Oliver Schmidt", "Li Na Tan"]
CHARGES_ZH = ["海运费", "码头操作费", "文件费", "报关费", "拖车费", "仓储费", "保险费", "舱单申报费", "电放费", "铅封费"]
CHARGES_EN = ["OCEAN FREIGHT", "TERMINAL HANDLING CHARGE", "DOCUMENTATION FEE", "CUSTOMS CLEARANCE", "TRUCKING", "WAREHOUSE STORAGE", "CARGO INSURANCE", "AMS FILING FEE", "TELEX RELEASE FEE", "SEAL FEE"]
ZH_CONTRACT = [
    "甲方委托乙方办理本合同项下货物的出口订舱、报关、报检及国际运输事宜。",
    "乙方应在收到甲方书面委托后两个工作日内确认舱位，并将订舱结果通知甲方。",
    "货物的品名、规格、数量、单价及总金额以本合同附件一所列为准。",
    "买方应于提单日后三十日内，以电汇方式将全部货款支付至卖方指定账户。",
    "如因不可抗力导致任何一方不能履行合同，该方应在十五日内提供有关机构出具的证明文件。",
    "任何一方违反本合同约定，应向守约方支付合同总金额百分之五的违约金。",
    "本合同适用中华人民共和国法律；因本合同引起的争议，提交上海仲裁委员会仲裁。",
    "本合同一式两份，双方各执一份，自双方签字盖章之日起生效，具有同等法律效力。",
    "运费及附加费按双方确认的报价单结算，汇率以开票当日银行公布的外汇牌价为准。",
    "乙方对因甲方申报不实、包装不良或单证不全造成的损失不承担责任。",
    "货物在目的港超过免费堆存期所产生的滞箱费、滞港费由收货人承担。",
    "保险由买方自理；卖方应在装船后两个工作日内以电子邮件发出装船通知。",
    "品质与数量的异议，买方应在货到目的港后三十日内凭检验报告向卖方提出。",
    "未经对方书面同意，任何一方不得将本合同项下的权利义务转让给第三方。",
    "付款方式：见票后三十天付款的即期信用证，不可撤销。",
    "装运期：收到信用证后四十五天内装运，允许分批装运与转运。",
]
ZH_SMALL = [
    "本提单项下货物已按表面状况良好装船，承运人对内容不负责任。",
    "运费预付，到港后凭正本提单一份放货，其余各份自动失效。",
    "托运人保证所申报的货物名称、数量与重量准确无误。",
    "集装箱由托运人装箱并铅封，箱内货物以托运人申报为准。",
    "品质以出口商检局签发的检验证书为最终依据。",
    "本单据经涂改无效；复印件须加盖业务专用章方为有效。",
]
ZH_NOTICE_TITLES = [
    "关于调整出口集装箱码头操作费的通知",
    "关于规范进出口环节收费行为的通知",
    "关于做好汛期港口安全生产工作的通知",
    "关于开展国际货运代理企业备案的通告",
]
ZH_NOTICE = [
    "为进一步规范港口收费行为，优化口岸营商环境，现将有关事项通知如下。",
    "冷藏箱、危险品箱及超限箱的附加费标准另行公布，原有文件与本通知不一致的，以本通知为准。",
    "各企业应当在营业场所和网站显著位置公示收费项目、收费标准和计费办法。",
    "对违反规定乱收费、变相涨价的行为，将依照《中华人民共和国价格法》予以查处。",
    "严格执行收费目录清单制度，清单之外一律不得收费。",
    "不得以绿色通道费、加急费等名义强制服务并收取费用。",
    "各单位要落实值班制度，遇有重大情况须在两小时内书面报告。",
    "请各单位认真贯彻执行，执行中遇到的问题，请及时向港口管理部门反映。",
]
EN_SENTENCES = [
    "SHIPPED ON BOARD IN APPARENT GOOD ORDER AND CONDITION UNLESS OTHERWISE STATED.",
    "FREIGHT PREPAID. ONE ORIGINAL BILL OF LADING MUST BE SURRENDERED DULY ENDORSED.",
    "Shipper's load, stow, count and seal. Said to contain the goods described below.",
    "This Agreement shall be governed by and construed in accordance with the laws of Singapore.",
    "Payment: 30% deposit by T/T, balance against copy of shipping documents.",
    "Partial shipment allowed. Transhipment allowed. Latest shipment date as per L/C.",
    "Insurance to be covered by the buyer. Shipping advice to be sent within two working days.",
    "Quantity and quality certificates issued by the inspection bureau shall be final.",
    "Demurrage and detention charges, if any, are for the account of the consignee.",
    "The carrier's liability is limited as per the Hague-Visby Rules where applicable.",
    "Cargo shall not be delivered unless freight and all charges are paid in full.",
    "All bank charges outside China are for the account of the beneficiary.",
    "We hereby certify that the above goods are of Chinese origin and the particulars are true and correct.",
    "Goods remain the property of the seller until payment has been received in full.",
    "Please quote our invoice number on all remittances and correspondence.",
    "Any discrepancy must be reported in writing within 7 days after receipt of goods.",
    "Wooden packing materials have been fumigated and marked in accordance with ISPM 15.",
    "Subject to the terms and conditions overleaf, including the law and jurisdiction clause.",
]
EN_SMALL = [
    "Received by the Carrier from the Shipper in apparent good order and condition unless otherwise indicated herein.",
    "Weight, measure, marks, numbers, quality, contents and value unknown to the Carrier.",
    "The Merchant's attention is called to the fact that liability is limited under Clauses 18 and 23 hereof.",
    "In witness whereof the number of original Bills of Lading stated above have been signed, one of which being accomplished, the others to stand void.",
    "E. & O.E. This document is computer generated and is valid without signature.",
    "Interest at 1.5% per month will be charged on overdue accounts.",
]
MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"]
CN_DIGITS = "零壹贰叁肆伍陆柒捌玖"
CN_ORDINALS = "一二三四五六七八九十"


def rand_container(rng: random.Random) -> str:
    prefix = "".join(rng.choice("ABCDEFGHIJKLMNOPQRSTUVWXYZ") for _ in range(3)) + "U"
    return f"{prefix}{rng.randint(1000000, 9999999)}"


def rand_bl(rng: random.Random) -> str:
    return f"{rng.choice(['COSU', 'MAEU', 'OOLU', 'HLCU', 'EGLV'])}{rng.randint(600000000, 699999999)}"


def rand_date(rng: random.Random, zh: bool) -> str:
    y, m, d = rng.randint(2021, 2026), rng.randint(1, 12), rng.randint(1, 28)
    return f"{y}年{m}月{d}日" if zh else f"{d:02d} {MONTHS[m - 1]} {y}"


def rand_fen(rng: random.Random, lo: int = 10_000, hi: int = 99_999_999) -> int:
    """金额，单位是分。"""
    return rng.randint(lo, hi)


def money(fen: int) -> str:
    return f"{fen // 100:,}.{fen % 100:02d}"


def rand_amount(rng: random.Random) -> str:
    return f"USD {money(rand_fen(rng))}"


def rand_digits(rng: random.Random, n: int) -> str:
    return "".join(rng.choice("0123456789") for _ in range(n))


def rand_usci(rng: random.Random) -> str:
    """样子像统一社会信用代码的 18 位编号（数字加不含 I、O、Z、S、V 的大写字母），不保证校验位。"""
    return "91" + rand_digits(rng, 6) + "".join(rng.choice("0123456789ABCDEFGHJKLMNPQRTUWXY") for _ in range(10))


def rand_phone_zh(rng: random.Random) -> str:
    return f"{rng.choice(['021', '0574', '0755', '0532', '022'])}-{rng.randint(20000000, 89999999)}"


def rand_phone_en(rng: random.Random) -> str:
    return f"+{rng.choice(['86 21', '31 10', '49 40', '1 562', '65'])} {rng.randint(2000, 8999)} {rng.randint(1000, 9999)}"


def rand_person(rng: random.Random) -> str:
    return rng.choice(SURNAMES) + rng.choice(GIVEN)


def rand_email(rng: random.Random) -> str:
    user = rng.choice(["ops", "docs", "accounts", "booking", "sales01", "li.na"])
    host = rng.choice(["sunrise-trading", "blueharbor", "northwind", "goldenbridge"])
    return f"{user}@{host}.example"


def rand_hs(rng: random.Random) -> str:
    return f"{rng.randint(1000, 9999)}.{rng.randint(10, 99)}.{rng.randint(0, 99):02d}"


def rand_vessel(rng: random.Random) -> str:
    name = rng.choice(["EVER BRIGHT", "COSCO HARMONY", "MSC AURORA", "OOCL JASMINE", "HAMBURG EXPRESS"])
    return f"{name} {rng.randint(10, 999)}{rng.choice('EWNS')}"


def cn_upper(fen: int) -> str:
    """人民币大写金额（1 亿元以内）。"""
    yuan, rest = divmod(fen, 100)
    jiao, cent = divmod(rest, 10)

    def group(n: int) -> str:
        out, zero = "", False
        for unit, name in ((1000, "仟"), (100, "佰"), (10, "拾"), (1, "")):
            d = n // unit % 10
            if d == 0:
                zero = bool(out)
            else:
                out += ("零" if zero else "") + CN_DIGITS[d] + name
                zero = False
        return out

    wan, low = divmod(yuan, 10000)
    out = group(wan) + "万" if wan else ""
    if low:
        out += ("零" if wan and low < 1000 else "") + group(low)
    out = (out or "零") + "元"
    if jiao == 0 and cent == 0:
        return out + "整"
    if jiao:
        out += CN_DIGITS[jiao] + "角"
    elif yuan:
        out += "零"
    if cent:
        out += CN_DIGITS[cent] + "分"
    return out


def en_words(n: int) -> str:
    """整数的英文读法（100 万以内），大写。"""
    ones = ["ZERO", "ONE", "TWO", "THREE", "FOUR", "FIVE", "SIX", "SEVEN", "EIGHT", "NINE", "TEN", "ELEVEN", "TWELVE",
            "THIRTEEN", "FOURTEEN", "FIFTEEN", "SIXTEEN", "SEVENTEEN", "EIGHTEEN", "NINETEEN"]
    tens = ["", "", "TWENTY", "THIRTY", "FORTY", "FIFTY", "SIXTY", "SEVENTY", "EIGHTY", "NINETY"]

    def below_1000(v: int) -> str:
        parts = []
        if v >= 100:
            parts.append(f"{ones[v // 100]} HUNDRED")
            v %= 100
            if v:
                parts.append("AND")
        if v >= 20:
            parts.append(tens[v // 10] + (f"-{ones[v % 10]}" if v % 10 else ""))
        elif v or not parts:
            parts.append(ones[v])
        return " ".join(parts)

    if n < 1000:
        return below_1000(n)
    high, low = divmod(n, 1000)
    return f"{below_1000(high)} THOUSAND" + (f" {below_1000(low)}" if low else "")


# ---------------------------------------------------------------------------------------------
# 版面元素
# ---------------------------------------------------------------------------------------------


@dataclass
class Flow:
    """一段文字：超过版心宽度就折行，每个折出来的行各标一条。文字里连着四个空格的地方断成两个字段，
    中间留约 1.5 cm 的空当，各标一条（表单里相邻的两个字段）。"""

    text: str
    font: str
    size: float
    kind: str
    leading: float | None = None  # 行距倍数；不给就用这一页的
    align: str = "left"  # left | center | right
    color: tuple[int, int, int] = BLACK


@dataclass
class Table:
    """表格：每个单元格单独标一条（标注员也是按单元格标的）。`ruled` 时画表格线。"""

    rows: list[list[str]]
    widths: list[float]  # 各列的相对宽度
    font: str
    size: float
    ruled: bool


@dataclass
class Gap:
    pt: float


@dataclass
class Rule:
    """一条横线（公文的红色分隔线）。"""

    color: tuple[int, int, int]
    width_pt: float


Block = Flow | Table | Gap | Rule


@dataclass
class TextLine:
    text: str
    box: tuple[float, float, float, float]  # 母版像素 x0, y0, x1, y1，已经加上留边
    kind: str
    font: str
    size: float
    under_seal: bool = False


@dataclass
class PageSpec:
    """一个底页：内容和版面参数，跟分辨率、退化方式无关。"""

    lang: str
    index: int
    doc: str
    font: str
    leading: float
    ruled: bool
    seal: str | None  # None 没有章；"below" 章在最后一行下面；"over" 章压在落款上
    seal_text: str
    angle: float  # rot、noiserot 用的角度
    blocks: list[Block] = field(default_factory=list)

    @property
    def name(self) -> str:
        return f"{self.lang}_{self.index:03d}"


def yen(font_key: str) -> str:
    """人民币符号：字体里有半角的 ¥（U+00A5）就用它，没有（黑体、仿宋、楷体）就用全角的 ￥（U+FFE5）。"""
    return "¥" if has_glyph(font_key, "¥") else "￥"


def zh_contract(rng: random.Random, f: str) -> list[Block]:
    fen = rand_fen(rng, 1_000_000, 99_999_999_99)
    blocks: list[Block] = [
        Flow(rng.choice(["国际货物买卖合同", "货运代理协议", "仓储服务合同", "设备采购合同"]), rng.choice([f, "simhei"]), 18, "title", align="center"),
        Gap(8),
        Flow(f"合同编号：HT-{rng.randint(2021, 2026)}-{rng.randint(1, 99999):05d}    签订日期：{rand_date(rng, True)}", f, 10.5, "field"),
        Flow(f"甲方（委托方）：{rng.choice(COMPANIES_ZH)}", f, 10.5, "field"),
        Flow(f"地址：{rng.choice(ADDRS_ZH)}", f, 10.5, "field"),
        Flow(f"电话：{rand_phone_zh(rng)}    传真：{rand_phone_zh(rng)}", f, 10.5, "field"),
        Flow(f"乙方（受托方）：{rng.choice(COMPANIES_ZH)}", f, 10.5, "field"),
        Flow(f"地址：{rng.choice(ADDRS_ZH)}", f, 10.5, "field"),
        Flow(f"电话：{rand_phone_zh(rng)}    电子邮箱：{rand_email(rng)}", f, 10.5, "field"),
        Gap(6),
    ]
    for i, s in enumerate(rng.sample(ZH_CONTRACT, 8)):
        blocks.append(Flow(f"第{CN_ORDINALS[i]}条 {s}", f, 10.5, "sentence"))
    blocks.append(Gap(6))
    blocks.append(Flow(f"合同总金额：人民币{cn_upper(fen)}（{yen(f)}{money(fen)}）", f, 10.5, "field"))
    for s in rng.sample(ZH_SMALL, 2):
        blocks.append(Flow(s, f, 7.5, "small", leading=1.25))
    blocks += [
        Gap(10),
        Flow("甲方（盖章）：    乙方（盖章）：", f, 10.5, "field"),
        Flow(f"授权代表：{rand_person(rng)}    授权代表：{rand_person(rng)}", f, 10.5, "field"),
        Flow(f"日期：{rand_date(rng, True)}    日期：{rand_date(rng, True)}", f, 10.5, "field"),
    ]
    return blocks


def zh_invoice(rng: random.Random, f: str, ruled: bool) -> list[Block]:
    rows = [["项目名称", "单位", "数量", "单价", "金额", "税率", "税额"]]
    total = tax_total = 0
    for item in rng.sample(CHARGES_ZH, rng.randint(5, 8)):
        qty = rng.randint(1, 40)
        price = rand_fen(rng, 5_000, 999_999)
        rate = rng.choice([0, 6, 9, 13])
        amount = qty * price
        tax = amount * rate // 100
        total += amount
        tax_total += tax
        rows.append([item, rng.choice(["票", "箱", "吨", "次"]), str(qty), money(price), money(amount), f"{rate}%" if rate else "免税", money(tax)])
    return [
        Flow(rng.choice(["增值税电子普通发票", "费用结算单", "形式发票", "国际货运代理业专用发票"]), rng.choice([f, "simhei"]), 18, "title", align="center"),
        Gap(8),
        Flow(f"发票代码：{rand_digits(rng, 12)}    发票号码：{rand_digits(rng, 8)}", f, 10.5, "field"),
        Flow(f"开票日期：{rand_date(rng, True)}    校验码：{' '.join(rand_digits(rng, 5) for _ in range(4))}", f, 10.5, "field"),
        Flow(f"购买方名称：{rng.choice(COMPANIES_ZH)}", f, 10.5, "field"),
        Flow(f"纳税人识别号：{rand_usci(rng)}", f, 10.5, "field"),
        Flow(f"地址、电话：{rng.choice(ADDRS_ZH)} {rand_phone_zh(rng)}", f, 10.5, "field"),
        Flow(f"开户行及账号：{rng.choice(BANKS_ZH)} {rand_digits(rng, 19)}", f, 10.5, "field"),
        Gap(4),
        Table(rows, [3.0, 1.0, 1.0, 1.8, 2.2, 1.1, 1.9], f, 9, ruled),
        Gap(4),
        Flow(f"合计金额：{yen(f)}{money(total)}    合计税额：{yen(f)}{money(tax_total)}", f, 10.5, "field"),
        Flow(f"价税合计（大写）：{cn_upper(total + tax_total)}    （小写）{yen(f)}{money(total + tax_total)}", f, 10.5, "field"),
        Flow(f"销售方名称：{rng.choice(COMPANIES_ZH)}", f, 10.5, "field"),
        Flow(f"纳税人识别号：{rand_usci(rng)}", f, 10.5, "field"),
        Flow(f"备注：提单号 {rand_bl(rng)}，箱号 {rand_container(rng)}，{rng.choice(PORTS_ZH)}至{rng.choice(PORTS_ZH)}", f, 10.5, "field"),
        *[Flow(s, f, 7.5, "small", leading=1.25) for s in rng.sample(ZH_SMALL, 2)],
        Gap(10),
        Flow(f"收款人：{rand_person(rng)}    复核：{rand_person(rng)}    开票人：{rand_person(rng)}", f, 10.5, "field"),
        Flow("销售方（章）：", f, 10.5, "field"),
    ]


def zh_notice(rng: random.Random, f: str) -> list[Block]:
    org = rng.choice(["滨海市港航管理局", "临江市口岸管理委员会", "东湾市交通运输局"])
    year = rng.randint(2021, 2026)
    body = rng.sample(ZH_NOTICE[1:], 5)
    fee = rng.randint(300, 990)
    blocks: list[Block] = [
        Flow(f"{org}文件", rng.choice(["simsun", "simhei"]), 24, "title", align="center", color=RED),
        Gap(10),
        Flow(f"{org[:2]}{rng.choice(['港航', '口岸', '交运'])}〔{year}〕{rng.randint(1, 180)}号", f, 14, "field", align="center"),
        Gap(4),
        Rule(RED, 1.5),
        Gap(14),
        Flow(rng.choice(ZH_NOTICE_TITLES), "simhei", 18, "title", align="center"),
        Gap(10),
        Flow("各船公司、货运代理企业、报关企业：", f, 14, "sentence"),
        Flow(ZH_NOTICE[0], f, 14, "sentence"),
        Flow(f"一、自{year}年{rng.randint(1, 12)}月1日起，出口重箱码头操作费按每标准箱人民币{fee}元计收。", f, 14, "sentence"),
    ]
    for i, s in enumerate(body[:3]):
        blocks.append(Flow(f"{CN_ORDINALS[i + 1]}、{s}", f, 14, "sentence"))
    for i, s in enumerate(body[3:]):
        blocks.append(Flow(f"（{CN_ORDINALS[i]}）{s}", f, 14, "sentence"))
    blocks += [
        Flow("特此通知。", f, 14, "sentence"),
        Gap(16),
        Flow(org, f, 14, "field", align="right"),
        Flow(rand_date(rng, True), f, 14, "field", align="right"),
        Gap(12),
        Flow(f"（联系人：{rand_person(rng)}    联系电话：{rand_phone_zh(rng)}）", f, 12, "field"),
        Flow("抄送：市交通委员会，市发展和改革委员会，海关。", f, 12, "field"),
        Flow(f"{org}办公室    {rand_date(rng, True)}印发", f, 12, "field"),
    ]
    return blocks


def en_container_rows(rng: random.Random, n: int) -> list[list[str]]:
    return [
        [
            f"{rand_container(rng)} / {rng.randint(100000, 999999)}",
            f"{rng.randint(1, 1200)} CTNS",
            f"{rng.randint(100, 24000)}.{rng.randint(0, 99):02d} KGS",
            f"{rng.randint(1, 67)}.{rng.randint(0, 99):02d} CBM",
        ]
        for _ in range(n)
    ]


def en_bl(rng: random.Random, f: str, ruled: bool) -> list[Block]:
    rows = [["Container No. / Seal No.", "Packages", "Gross Weight", "Measurement"], *en_container_rows(rng, rng.randint(3, 5))]
    return [
        Flow(rng.choice(["BILL OF LADING", "SEA WAYBILL", "COMBINED TRANSPORT BILL OF LADING"]), f, 18, "title", align="center"),
        Gap(8),
        Flow(f"B/L No. {rand_bl(rng)}    Booking No. {rng.randint(10000000, 99999999)}", f, 10.5, "field"),
        Flow(f"Shipper: {rng.choice(COMPANIES_EN)}", f, 10.5, "field"),
        Flow(rng.choice(ADDRS_EN), f, 10.5, "field"),
        Flow(f"Consignee: {rng.choice(['TO ORDER', 'TO ORDER OF ' + rng.choice(BANKS_EN), rng.choice(COMPANIES_EN)])}", f, 10.5, "field"),
        Flow(f"Notify Party: {rng.choice(COMPANIES_EN)}, TEL: {rand_phone_en(rng)}", f, 10.5, "field"),
        Flow(f"Vessel/Voyage: {rand_vessel(rng)}    Port of Loading: {rng.choice(PORTS)}", f, 10.5, "field"),
        Flow(f"Port of Discharge: {rng.choice(PORTS)}    Place of Delivery: {rng.choice(PORTS)}", f, 10.5, "field"),
        Gap(4),
        Table(rows, [2.6, 1.4, 1.8, 1.5], "cour", 9, ruled),
        Gap(4),
        Flow(f"Description of Goods: {rng.choice(GOODS)}, HS CODE {rand_hs(rng)}", f, 10.5, "field"),
        Flow(f"Marks & Nos.: {rng.choice(['N/M', 'AS PER INVOICE', 'PO#' + rand_digits(rng, 7) + ' C/NO. 1-UP'])}", f, 10.5, "field"),
        Flow(f"Freight: {rng.choice(['PREPAID', 'COLLECT'])}    Number of Original B/Ls: {rng.choice(['THREE (3)', 'ZERO (0)', 'ONE (1)'])}", f, 10.5, "field"),
        Flow(f"Place and Date of Issue: {rng.choice(PORTS)}, {rand_date(rng, False)}", f, 10.5, "field"),
        Flow(f"Total Amount: {rand_amount(rng)}    Terms: {rng.choice(['FOB SHANGHAI', 'CIF ROTTERDAM', 'EXW NINGBO'])}", f, 10.5, "field"),
        *[Flow(s, f, 9, "sentence") for s in rng.sample(EN_SENTENCES, 5)],
        *[Flow(s, f, 7, "small", leading=1.25) for s in rng.sample(EN_SMALL, 3)],
        Gap(10),
        Flow("Signed for the Carrier:", f, 10.5, "field"),
    ]


def en_packing(rng: random.Random, f: str, ruled: bool, item_rows: int | None = None, size: float = 9) -> list[Block]:
    rows = [["No.", "Description", "Quantity", "N.W. (KGS)", "G.W. (KGS)", "Meas. (CBM)"]]
    n = item_rows if item_rows is not None else rng.randint(6, 10)
    net = gross = 0
    for i in range(n):
        nw = rng.randint(5000, 2_400_000)
        gw = nw + rng.randint(500, 90_000)
        net += nw
        gross += gw
        model = f"{rng.choice('ABCDEFGHKLMNPRSTXZ')}{rng.choice('ABCDEFGHKLMNPRSTXZ')}-{rng.randint(100, 9999)}"
        rows.append([str(i + 1), f"{rng.choice(GOODS)} {model}", f"{rng.randint(10, 9999):,} {rng.choice(['PCS', 'SETS', 'CTNS'])}",
                     money(nw), money(gw), f"{rng.randint(0, 67)}.{rng.randint(0, 999):03d}"])
    return [
        Flow("PACKING LIST", f, 18, "title", align="center"),
        Gap(8),
        Flow(f"Invoice No. INV-{rng.randint(10000, 99999)}    Date: {rand_date(rng, False)}", f, 10.5, "field"),
        Flow(f"Seller: {rng.choice(COMPANIES_EN)}", f, 10.5, "field"),
        Flow(f"Buyer: {rng.choice(COMPANIES_EN)}", f, 10.5, "field"),
        Flow(f"From: {rng.choice(PORTS)}    To: {rng.choice(PORTS)}    By: {rand_vessel(rng)}", f, 10.5, "field"),
        Gap(4),
        Table(rows, [0.6, 4.1, 1.5, 1.4, 1.4, 1.5], f, size, ruled),
        Gap(4),
        Flow(f"TOTAL: {rng.randint(n, 400 * n):,} CARTONS    N.W.: {money(net)} KGS    G.W.: {money(gross)} KGS", f, 10.5, "field"),
        Flow(f"Marks & Nos.: {rng.choice(['N/M', 'AS PER INVOICE', 'PO#' + rand_digits(rng, 7) + ' C/NO. 1-UP'])}", f, 10.5, "field"),
        Flow(f"Country of Origin: CHINA    HS Code: {rand_hs(rng)}", f, 10.5, "field"),
        *[Flow(s, f, 9, "sentence") for s in rng.sample(EN_SENTENCES, 3)],
        *[Flow(s, f, 7, "small", leading=1.25) for s in rng.sample(EN_SMALL, 2)],
        Gap(10),
        Flow("Authorized Signature:", f, 10.5, "field"),
    ]


def en_invoice(rng: random.Random, f: str, ruled: bool) -> list[Block]:
    rows = [["Description", "Qty", "Unit Price", "Amount (USD)"]]
    total = 0
    for item in rng.sample(CHARGES_EN, rng.randint(5, 8)):
        qty = rng.randint(1, 40)
        price = rand_fen(rng, 2_500, 480_000)
        total += qty * price
        rows.append([item, str(qty), money(price), money(qty * price)])
    return [
        Flow(rng.choice(["COMMERCIAL INVOICE", "PROFORMA INVOICE", "FREIGHT INVOICE"]), f, 18, "title", align="center"),
        Gap(8),
        Flow(f"Invoice No. INV-{rng.randint(10000, 99999)}    Date: {rand_date(rng, False)}", f, 10.5, "field"),
        Flow(f"Bill To: {rng.choice(COMPANIES_EN)}", f, 10.5, "field"),
        Flow(rng.choice(ADDRS_EN), f, 10.5, "field"),
        Flow(f"Attn: {rng.choice(PEOPLE_EN)}    Email: {rand_email(rng)}", f, 10.5, "field"),
        Flow(f"Payment Terms: {rng.choice(['NET 30 DAYS', 'T/T IN ADVANCE', 'L/C AT SIGHT'])}    Incoterms: {rng.choice(['FOB SHANGHAI', 'CIF ROTTERDAM', 'EXW NINGBO'])}", f, 10.5, "field"),
        Gap(4),
        Table(rows, [3.6, 0.8, 1.6, 1.9], f, 9, ruled),
        Gap(4),
        Flow(f"Subtotal: USD {money(total)}    VAT (0%): USD 0.00", f, 10.5, "field"),
        Flow(f"TOTAL: USD {money(total)}", f, 10.5, "field"),
        Flow(f"SAY US DOLLARS {en_words(total // 100)} AND CENTS {en_words(total % 100)} ONLY", f, 10.5, "field"),
        Flow(f"Bank: {rng.choice(BANKS_EN)}", f, 10.5, "field"),
        Flow(f"SWIFT: {''.join(rng.choice('ABCDEFGHIJKLMNOPQRSTUVWXYZ') for _ in range(6))}{rng.choice(['2A', '3X', 'BJ'])}    Account No.: {' '.join(rand_digits(rng, 4) for _ in range(4))}", f, 10.5, "field"),
        *[Flow(s, f, 9, "sentence") for s in rng.sample(EN_SENTENCES, 4)],
        *[Flow(s, f, 7, "small", leading=1.25) for s in rng.sample(EN_SMALL, 2)],
        Gap(10),
        Flow("Authorized Signature:", f, 10.5, "field"),
    ]


def mixed_bl(rng: random.Random, zf: str, ef: str, ruled: bool) -> list[Block]:
    rows = [["箱号 Container", "封号 Seal", "件数 Pkgs", "毛重 G.W.", "体积 Meas."]]
    for _ in range(rng.randint(2, 4)):
        rows.append([rand_container(rng), f"SL{rng.randint(100000, 999999)}", f"{rng.randint(1, 1200)} 件",
                     f"{rng.randint(100, 24000)} KGS", f"{rng.randint(1, 67)}.{rng.randint(0, 9)} CBM"])
    goods = rng.randrange(len(GOODS))
    return [
        Flow("海运提单 BILL OF LADING", zf, 18, "title", align="center"),
        Gap(8),
        Flow(f"提单号 B/L No.: {rand_bl(rng)}    签发日期 Date of Issue: {rand_date(rng, False)}", zf, 10.5, "field"),
        Flow(f"托运人 Shipper: {rng.choice(COMPANIES_EN)}", zf, 10.5, "field"),
        Flow(f"收货人 Consignee: {rng.choice(COMPANIES_ZH)}", zf, 10.5, "field"),
        Flow(f"通知方 Notify Party: {rng.choice(['SAME AS CONSIGNEE', rng.choice(COMPANIES_EN)])}", zf, 10.5, "field"),
        Flow(f"起运港 Port of Loading: {rng.choice(PORTS)}    目的港 Port of Discharge: {rng.choice(PORTS)}", zf, 10.5, "field"),
        Flow(f"船名航次 Vessel/Voy.: {rand_vessel(rng)}", zf, 10.5, "field"),
        Gap(4),
        Table(rows, [2.2, 1.6, 1.4, 1.6, 1.5], zf, 9, ruled),
        Gap(4),
        Flow(f"货物描述 Description: {GOODS[goods]}（{GOODS_ZH[goods]}）", zf, 10.5, "field"),
        Flow(f"运费 Freight: {rng.choice(['PREPAID 预付', 'COLLECT 到付'])}    金额 Amount: {rand_amount(rng)}", zf, 10.5, "field"),
        Flow(f"运输条款 Terms: {rng.choice(['CY-CY', 'CY-DOOR', 'CFS-CFS'])}    正本份数 Originals: {rng.choice(['三份 THREE', '一份 ONE'])}", zf, 10.5, "field"),
        *[Flow(s, zf, 9, "sentence") for s in rng.sample(ZH_CONTRACT + ZH_SMALL, 3)],
        *[Flow(s, ef, 9, "sentence") for s in rng.sample(EN_SENTENCES, 3)],
        Flow(rng.choice(ZH_SMALL), zf, 7.5, "small", leading=1.25),
        Flow(rng.choice(EN_SMALL), ef, 7, "small", leading=1.25),
        Gap(10),
        Flow("承运人签章 For the Carrier:", zf, 10.5, "field"),
    ]


def mixed_invoice(rng: random.Random, zf: str, ef: str, ruled: bool) -> list[Block]:
    rows = [["费用项目 Charge", "数量 Qty", "单价 Unit Price", "金额 Amount"]]
    total = 0
    for i in rng.sample(range(len(CHARGES_ZH)), rng.randint(4, 7)):
        qty = rng.randint(1, 40)
        price = rand_fen(rng, 2_500, 480_000)
        total += qty * price
        rows.append([f"{CHARGES_ZH[i]} {CHARGES_EN[i]}", str(qty), money(price), money(qty * price)])
    return [
        Flow("费用清单 DEBIT NOTE", zf, 18, "title", align="center"),
        Gap(8),
        Flow(f"编号 No.: DN-{rng.randint(10000, 99999)}    日期 Date: {rand_date(rng, False)}", zf, 10.5, "field"),
        Flow(f"致 To: {rng.choice(COMPANIES_ZH)}", zf, 10.5, "field"),
        Flow(f"地址 Address: {rng.choice(ADDRS_ZH)}", zf, 10.5, "field"),
        Flow(f"联系人 Attn: {rand_person(rng)}    电话 Tel: {rand_phone_zh(rng)}", zf, 10.5, "field"),
        Flow(f"提单号 B/L No.: {rand_bl(rng)}    箱号 Container No.: {rand_container(rng)}", zf, 10.5, "field"),
        Gap(4),
        Table(rows, [4.2, 1.2, 1.9, 1.9], zf, 9, ruled),
        Gap(4),
        Flow(f"合计 TOTAL: USD {money(total)}    汇率 Rate: {rng.randint(680, 730) / 100:.4f}", zf, 10.5, "field"),
        Flow(f"折合人民币 CNY: {yen(zf)}{money(total * 7)}（{cn_upper(total * 7)}）", zf, 10.5, "field"),
        Flow(f"开户行 Bank: {rng.choice(BANKS_ZH)}", zf, 10.5, "field"),
        Flow(f"账号 A/C No.: {rand_digits(rng, 19)}    付款期限 Due: {rand_date(rng, False)}", zf, 10.5, "field"),
        *[Flow(s, zf, 9, "sentence") for s in rng.sample(ZH_CONTRACT + ZH_SMALL, 3)],
        *[Flow(s, ef, 9, "sentence") for s in rng.sample(EN_SENTENCES, 3)],
        Flow(rng.choice(ZH_SMALL), zf, 7.5, "small", leading=1.25),
        Flow(rng.choice(EN_SMALL), ef, 7, "small", leading=1.25),
        Gap(10),
        Flow("制单 Prepared by:    复核 Checked by:", zf, 10.5, "field"),
    ]


def build_page(seed: int, lang: str, index: int, doc: str | None = None, item_rows: int | None = None) -> PageSpec:
    """由种子、语言、序号定出一个底页的全部内容。"""
    rng = random.Random(f"{seed}:{lang}:{index}:{doc or ''}")
    doc = doc or DOCS[lang][(index - 1) % len(DOCS[lang])]
    zf, ef = rng.choice(ZH_FONTS), rng.choice(EN_FONTS)
    leading = rng.choice(LEADINGS)
    ruled = rng.random() < 0.5
    seal = rng.choice([None, "below", "below", "over", "over"])
    seal_text = rng.choice(["合同专用章", "业务专用章", "FOR THE CARRIER"])
    angle = rng.choice([-1.5, -1.0, 1.0, 1.5])
    if lang == "zh":
        blocks = {"contract": lambda: zh_contract(rng, zf), "invoice": lambda: zh_invoice(rng, zf, ruled), "notice": lambda: zh_notice(rng, zf)}[doc]()
        font = zf
    elif lang == "en":
        blocks = {
            "bl": lambda: en_bl(rng, ef, ruled),
            "packing": lambda: en_packing(rng, ef, ruled, item_rows, 8 if item_rows else 9),
            "invoice": lambda: en_invoice(rng, ef, ruled),
        }[doc]()
        font = ef
    else:
        blocks = {"bl": lambda: mixed_bl(rng, zf, ef, ruled), "invoice": lambda: mixed_invoice(rng, zf, ef, ruled)}[doc]()
        font = zf
    return PageSpec(lang, index, doc, font, leading, ruled, seal, seal_text, angle, blocks)


# ---------------------------------------------------------------------------------------------
# 排版：把底页画成 600 dpi 的母版
# ---------------------------------------------------------------------------------------------

_FONT_CACHE: dict[tuple[str, float], ImageFont.FreeTypeFont] = {}
CJK_CLOSERS = "，。；：、！？）》」』〕】％"


def font_at(key: str, size_pt: float) -> ImageFont.FreeTypeFont:
    cached = _FONT_CACHE.get((key, size_pt))
    if cached is None:
        cached = ImageFont.truetype(str(FONT_DIR / FONTS[key]), int(round(size_pt * U)))
        _FONT_CACHE[(key, size_pt)] = cached
    return cached


_GLYPH_CACHE: dict[tuple[str, str], bool] = {}


def has_glyph(key: str, ch: str) -> bool:
    """字体里有没有这个字：画出来和“缺字”的方框一样就是没有。"""
    cached = _GLYPH_CACHE.get((key, ch))
    if cached is None:
        font = ImageFont.truetype(str(FONT_DIR / FONTS[key]), 48)
        missing, got = font.getmask("\U0010ffff"), font.getmask(ch)
        cached = ch == " " or (got.size, bytes(got)) != (missing.size, bytes(missing))
        _GLYPH_CACHE[(key, ch)] = cached
    return cached


def is_cjk(ch: str) -> bool:
    return "\u2e80" <= ch <= "\u9fff" or "\uff00" <= ch <= "\uffef" or "\u3000" <= ch <= "\u303f"


def tokenize(text: str) -> list[tuple[str, bool]]:
    """切成排版单元：汉字和全角标点一个一个切，西文按空格切成词。返回 (单元, 前面有没有空格)。"""
    out: list[tuple[str, bool]] = []
    cur, space = "", False
    for ch in text:
        if ch == " ":
            if cur:
                out.append((cur, space))
                cur = ""
            space = True
        elif is_cjk(ch):
            if cur:
                out.append((cur, space))
                cur, space = "", False
            out.append((ch, space))
            space = False
        else:
            cur += ch
    if cur:
        out.append((cur, space))
    return out


def wrap(text: str, font: ImageFont.FreeTypeFont, max_w: float, draw: ImageDraw.ImageDraw) -> list[str]:
    """按像素宽度折行。西文不在词中间断开；全角的句读不放在行首。"""
    if draw.textlength(text, font=font) <= max_w:
        return [text]
    out: list[str] = []
    cur = ""
    for tok, space in tokenize(text):
        cand = tok if not cur else cur + (" " if space else "") + tok
        if not cur or draw.textlength(cand, font=font) <= max_w or tok in CJK_CLOSERS:
            cur = cand
        else:
            out.append(cur)
            cur = tok
    if cur:
        out.append(cur)
    return out


class Layout:
    """在母版上从上往下排版，同时记下每条标注。"""

    def __init__(self, spec: PageSpec) -> None:
        self.spec = spec
        self.img = Image.new("RGB", (MASTER_W, MASTER_H), (255, 255, 255))
        self.draw = ImageDraw.Draw(self.img)
        self.y = TOP_PT * U
        self.left = MARGIN_PT * U
        self.width = MASTER_W - 2 * MARGIN_PT * U
        self.lines: list[TextLine] = []
        self.seal_box: tuple[float, float, float, float] | None = None

    def put(self, x: float, y: float, text: str, font_key: str, size: float, kind: str, color: tuple[int, int, int]) -> None:
        absent = [ch for ch in set(text) if not has_glyph(font_key, ch)]
        if absent:
            raise ValueError(f"{self.spec.name}：字体 {font_key} 里没有 {absent}，标注和画面会对不上")
        font = font_at(font_key, size)
        self.draw.text((x, y), text, font=font, fill=color)
        l, t, r, b = self.draw.textbbox((x, y), text, font=font)
        pad = LABEL_PAD_PT * U
        self.lines.append(TextLine(text, (l - pad, t - pad, r + pad, b + pad), kind, font_key, size))

    def flow(self, b: Flow) -> None:
        font = font_at(b.font, b.size)
        leading = b.leading or self.spec.leading
        gap = SEGMENT_GAP_PT * U
        # 带字段空当的行不折行（放不下就报错，让写模板的人去调）；其余的按版心宽度折行。
        pieces = [b.text] if "    " in b.text else wrap(b.text, font, self.width, self.draw)
        for piece in pieces:
            segments = piece.split("    ")
            total = sum(self.draw.textlength(s, font=font) for s in segments) + gap * (len(segments) - 1)
            # 行尾的句读可以探出版心一个字宽。
            if total > self.width + b.size * U:
                raise ValueError(f"{self.spec.name}：一行放不下（{total / U:.0f} pt）")
            if b.align == "center":
                cursor = self.left + (self.width - total) / 2
            elif b.align == "right":
                cursor = self.left + self.width - total
            else:
                cursor = self.left
            for seg in segments:
                if seg:
                    self.put(cursor, self.y, seg, b.font, b.size, b.kind, b.color)
                cursor += self.draw.textlength(seg, font=font) + gap
            self.y += b.size * leading * U
        if b.kind == "title":
            self.y += 4 * U

    def table(self, b: Table) -> None:
        font = font_at(b.font, b.size)
        row_h = b.size * (1.9 if b.ruled else 1.6) * U
        pad_x = (4.0 if b.ruled else 0.0) * U
        pad_y = b.size * (0.3 if b.ruled else 0.0) * U
        unit = self.width / sum(b.widths)
        xs = [self.left]
        for w in b.widths:
            xs.append(xs[-1] + w * unit)
        top = self.y
        for row in b.rows:
            for ci, cell in enumerate(row):
                cell = cell.strip()
                if not cell:
                    continue
                if self.draw.textlength(cell, font=font) > xs[ci + 1] - xs[ci] - 2 * pad_x - (0 if b.ruled else 6 * U):
                    raise ValueError(f"{self.spec.name}：单元格放不下第 {ci + 1} 列")
                self.put(xs[ci] + pad_x, self.y + pad_y, cell, b.font, b.size, "cell", BLACK)
            self.y += row_h
        if b.ruled:
            lw = max(1, int(round(0.75 * U)))
            for ri in range(len(b.rows) + 1):
                yy = top + ri * row_h
                self.draw.line([(xs[0], yy), (xs[-1], yy)], fill=BLACK, width=lw)
            for x in xs:
                self.draw.line([(x, top), (x, top + len(b.rows) * row_h)], fill=BLACK, width=lw)
        self.y += 2 * U

    def run(self) -> None:
        for b in self.spec.blocks:
            if isinstance(b, Flow):
                self.flow(b)
            elif isinstance(b, Table):
                self.table(b)
            elif isinstance(b, Gap):
                self.y += b.pt * U
            else:
                lw = max(1, int(round(b.width_pt * U)))
                self.draw.line([(self.left, self.y), (self.left + self.width, self.y)], fill=b.color, width=lw)
                self.y += lw
        if self.y > MASTER_H - MARGIN_PT * U:
            raise ValueError(f"{self.spec.name}：内容超出页面（到了 {self.y / U:.0f} pt）")
        if self.spec.seal:
            self.seal()

    def seal(self) -> None:
        """伪印章：红圈加一行字，按“印泥盖在纸上”叠上去（正片叠底），压住的黑字还看得见。"""
        r = 40.8 * U
        last = self.lines[-1].box
        cx = MASTER_W - 124.8 * U
        if self.spec.seal == "over":
            # 压在落款那一行上：圆心放在最后一行的末尾偏左
            cx = min(max(last[2] - 0.4 * r, self.left + r), MASTER_W - MARGIN_PT * U - r)
            cy = (last[1] + last[3]) / 2 - 0.25 * r
        else:
            cy = self.y + 36 * U
        cy = min(cy, MASTER_H - MARGIN_PT * U - r)
        x0, y0 = int(cx - r - 4), int(cy - r - 4)
        size = int(2 * r + 8)
        layer = Image.new("RGB", (size, size), (255, 255, 255))
        d = ImageDraw.Draw(layer)
        d.ellipse((4, 4, size - 4, size - 4), outline=RED, width=int(round(1.92 * U)))
        font = font_at("simsun", 12)
        tw = d.textlength(self.spec.seal_text, font=font)
        d.text((size / 2 - tw / 2, size / 2 - 7.2 * U), self.spec.seal_text, font=font, fill=RED)
        region = self.img.crop((x0, y0, x0 + size, y0 + size))
        self.img.paste(ImageChops.multiply(region, layer), (x0, y0))
        self.seal_box = (cx - r, cy - r, cx + r, cy + r)
        for line in self.lines:
            lx0, ly0, lx1, ly1 = line.box
            # 圆和矩形有没有重叠：圆心到矩形最近点的距离
            nx, ny = min(max(cx, lx0), lx1), min(max(cy, ly0), ly1)
            line.under_seal = math.hypot(nx - cx, ny - cy) < r


def box_quad(box: tuple[float, float, float, float]) -> Quad:
    x0, y0, x1, y1 = box
    return [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]


# ---------------------------------------------------------------------------------------------
# 变体
# ---------------------------------------------------------------------------------------------


def seeded_noise(size: tuple[int, int], sigma: float, rng: random.Random) -> Image.Image:
    """近似高斯的噪声图（模式 L，中心 128）：四张均匀噪声取平均，再按 sigma 缩放。
    `Image.effect_noise` 不能给种子，这里用给定的随机数发生器，生成结果可以重现。"""
    n = size[0] * size[1]
    layers = [Image.frombytes("L", size, rng.randbytes(n)) for _ in range(4)]
    avg = ImageChops.add(ImageChops.add(layers[0], layers[1], scale=2.0), ImageChops.add(layers[2], layers[3], scale=2.0), scale=2.0)
    # 四个 0–255 均匀数的平均：均值 127，标准差约 36.95
    k = sigma / 36.95
    return avg.point([max(0, min(255, round(128 + (v - 127.0) * k))) for v in range(256)])


def degrade_scan(img: Image.Image, rng: random.Random) -> Image.Image:
    """扫描的退化：轻微模糊、对比度降低（白 255→232，黑 0→23）、灰度噪声（标准差约 3.2）。输出灰度图。"""
    gray = img.filter(ImageFilter.GaussianBlur(0.6)).convert("L")
    return Image.blend(gray, seeded_noise(gray.size, 18.0, rng), 0.18)


def rotate_point(x: float, y: float, cx: float, cy: float, deg: float) -> list[float]:
    """PIL 的 rotate(angle) 是逆时针；像素坐标 y 向下，所以用对应的旋转矩阵。"""
    t = math.radians(deg)
    dx, dy = x - cx, y - cy
    return [cx + dx * math.cos(t) + dy * math.sin(t), cy - dx * math.sin(t) + dy * math.cos(t)]


def solve_homography(src: Quad, dst: Quad) -> list[float]:
    """把 src 四点映到 dst 四点的单应矩阵的 8 个系数 (a..h)：
    x' = (a x + b y + c) / (g x + h y + 1)，y' = (d x + e y + f) / (g x + h y + 1)。"""
    m: list[list[float]] = []
    for (x, y), (u, v) in zip(src, dst):
        m.append([x, y, 1, 0, 0, 0, -u * x, -u * y, u])
        m.append([0, 0, 0, x, y, 1, -v * x, -v * y, v])
    for col in range(8):
        pivot = max(range(col, 8), key=lambda r: abs(m[r][col]))
        m[col], m[pivot] = m[pivot], m[col]
        for r in range(8):
            if r != col:
                factor = m[r][col] / m[col][col]
                if factor:
                    m[r] = [a - factor * b for a, b in zip(m[r], m[col])]
    return [m[i][8] / m[i][i] for i in range(8)]


def apply_homography(h: list[float], x: float, y: float) -> list[float]:
    den = h[6] * x + h[7] * y + 1
    return [(h[0] * x + h[1] * y + h[2]) / den, (h[3] * x + h[4] * y + h[5]) / den]


def photo_variant(page: Image.Image, rng: random.Random) -> tuple[Image.Image, list[float]]:
    """手机拍照的近似。输入是 300 dpi 的干净页；返回 3024×4032 的画面和把页面坐标映到画面坐标的单应系数。"""
    pw, ph = page.size
    # 页面在画面里的位置：高占画面的 80–86%，四个角各自再挪一点（透视）；远的一边窄一些。
    height = PHOTO_H * rng.uniform(0.80, 0.86)
    width = height * pw / ph
    cx = PHOTO_W / 2 + rng.uniform(-60, 60)
    cy = PHOTO_H / 2 + rng.uniform(-60, 60)
    narrow = rng.uniform(0.88, 0.96)  # 上边宽度是下边的多少
    tilt = math.radians(rng.uniform(-3.0, 3.0))
    corners = [(-width * narrow / 2, -height / 2), (width * narrow / 2, -height / 2), (width / 2, height / 2), (-width / 2, height / 2)]
    dst: Quad = []
    for x, y in corners:
        x, y = x * math.cos(tilt) - y * math.sin(tilt), x * math.sin(tilt) + y * math.cos(tilt)
        dst.append([cx + x + rng.uniform(-25, 25), cy + y + rng.uniform(-25, 25)])
    src: Quad = [[0, 0], [pw, 0], [pw, ph], [0, ph]]
    forward = solve_homography(src, dst)
    inverse = solve_homography(dst, src)
    desk = (rng.randint(70, 110), rng.randint(60, 90), rng.randint(45, 75))
    shot = page.transform((PHOTO_W, PHOTO_H), Image.Transform.PERSPECTIVE, inverse, resample=Image.Resampling.BICUBIC, fillcolor=desk)

    # 光照：一个方向上的渐变（最暗处 58–75%），加一块边缘很软的暗影；整体偏暖。
    gw, gh = 48, 64
    direction = rng.uniform(0, 2 * math.pi)
    floor = rng.uniform(0.58, 0.75)
    sx, sy, sr = rng.uniform(0.1, 0.9), rng.uniform(0.1, 0.9), rng.uniform(0.25, 0.45)
    depth = rng.uniform(0.12, 0.25)
    values = bytearray()
    for j in range(gh):
        for i in range(gw):
            u, v = i / (gw - 1), j / (gh - 1)
            t = 0.5 + ((u - 0.5) * math.cos(direction) + (v - 0.5) * math.sin(direction)) / 1.42
            level = floor + (1 - floor) * t
            dist = math.hypot(u - sx, (v - sy) * gh / gw)
            if dist < sr:
                level *= 1 - depth * (0.5 + 0.5 * math.cos(math.pi * dist / sr))
            values.append(max(0, min(255, round(level * 255))))
    light = Image.frombytes("L", (gw, gh), bytes(values)).resize((PHOTO_W, PHOTO_H), Image.Resampling.BICUBIC)
    gains = (1.0, rng.uniform(0.94, 0.98), rng.uniform(0.84, 0.92))
    light_rgb = Image.merge("RGB", [light.point([round(v * g) for v in range(256)]) for g in gains])
    shot = ImageChops.multiply(shot, light_rgb)

    # 轻微失焦，传感器噪声（三个通道用同一张噪声，标准差 4）。
    shot = shot.filter(ImageFilter.GaussianBlur(rng.uniform(0.8, 1.2)))
    noise = seeded_noise((PHOTO_W, PHOTO_H), 4.0, rng)
    shot = ImageChops.add(shot, Image.merge("RGB", [noise, noise, noise]), scale=1.0, offset=-128)
    return shot, forward


@dataclass
class Emitted:
    """一张输出图的记录。"""

    file: str  # 相对 out/ 的路径
    label: str  # Label.txt 里的一行
    stem: str
    info: dict[str, object]


Mapper = Callable[[float, float], list[float]]


def scaled(k: float) -> Mapper:
    """母版坐标缩小 k 倍。"""
    return lambda x, y: [x / k, y / k]


def rotated_then_scaled(angle: float, k: float) -> Mapper:
    """母版绕中心转 angle 度之后再缩小 k 倍。"""
    return lambda x, y: [v / k for v in rotate_point(x, y, MASTER_W / 2, MASTER_H / 2, angle)]


def annotations(lines: list[TextLine], seal_box: tuple[float, float, float, float] | None, mapper: Mapper) -> list[dict[str, object]]:
    """母版上的标注经 mapper（母版坐标 → 输出图坐标）变成 PPOCRLabel 的条目。"""
    out: list[dict[str, object]] = []
    for line in lines:
        out.append({"transcription": line.text, "points": [[round(v, 1) for v in mapper(x, y)] for x, y in box_quad(line.box)]})
    if seal_box is not None:
        out.append({"transcription": "", "points": [[round(v, 1) for v in mapper(x, y)] for x, y in box_quad(seal_box)], "key_cls": "seal"})
    return out


def emit(out: Path, sub: str, stem: str, ext: str, img: Image.Image, anns: list[dict[str, object]], info: dict[str, object], **save: object) -> Emitted:
    rel = f"{sub}/{stem}.{ext}"
    img.save(out / rel, **save)
    info = {**info, "width": img.width, "height": img.height}
    return Emitted(rel, f"{rel}\t{json.dumps(anns, ensure_ascii=False)}", stem, info)


def render_page(args: tuple[str, int, str, int, str | None, int | None, list[str]]) -> tuple[dict[str, object], list[Emitted]]:
    """画一个底页的全部变体（在工作进程里执行）。返回底页的说明和各张输出图。"""
    out_dir, seed, lang, index, doc, item_rows, sets = args
    out = Path(out_dir)
    spec = build_page(seed, lang, index, doc, item_rows)
    layout = Layout(spec)
    layout.run()
    master, lines, seal_box = layout.img, layout.lines, layout.seal_box
    base = {"page": spec.name, "lang": lang}
    emitted: list[Emitted] = []

    def variant_rng(tag: str) -> random.Random:
        return random.Random(f"{seed}:{spec.name}:{tag}")

    def add(sub: str, stem: str, ext: str, img: Image.Image, mapper: Mapper, variant: str, res: str, **save: object) -> None:
        info = {**base, "variant": variant, "res": res}
        emitted.append(emit(out, sub, stem, ext, img, annotations(lines, seal_box, mapper), info, **save))

    # 扫描：母版（或转过的母版）按 2、3 倍取平均缩小到 300、200 dpi。
    rotated: Image.Image | None = None
    for dpi in (300, 200):
        if f"scan{dpi}" not in sets:
            continue
        k = MASTER_DPI // dpi
        for variant in SCAN_VARIANTS:
            if "rot" in variant:
                if rotated is None:
                    rotated = master.rotate(spec.angle, resample=Image.Resampling.BICUBIC, fillcolor=(255, 255, 255))
                img, mapper = rotated.reduce(k), rotated_then_scaled(spec.angle, k)
            else:
                img, mapper = master.reduce(k), scaled(k)
            if "noise" in variant:
                img = degrade_scan(img, variant_rng(f"{variant}:{dpi}"))
            add("images", f"{spec.name}_{variant}_{dpi}", "png", img, mapper, variant, str(dpi), compress_level=3)

    clean300 = master.reduce(2) if any(s in sets for s in ("orient", "photo", "speed")) else None
    if clean300 is not None and "orient" in sets:
        w, h = clean300.size
        turns: dict[str, tuple[Image.Transpose, Mapper]] = {
            "up180": (Image.Transpose.ROTATE_180, lambda x, y: [w - x / 2, h - y / 2]),
            "cw90": (Image.Transpose.ROTATE_270, lambda x, y: [h - y / 2, x / 2]),
            "ccw90": (Image.Transpose.ROTATE_90, lambda x, y: [y / 2, w - x / 2]),
        }
        for variant, (op, mapper) in turns.items():
            add("images", f"{spec.name}_{variant}_300", "png", clean300.transpose(op), mapper, variant, "300", compress_level=3)
    if clean300 is not None and "photo" in sets:
        shot, forward = photo_variant(clean300, variant_rng("photo"))
        add("images", f"{spec.name}_photo_12mp", "jpg", shot, lambda x, y: apply_homography(forward, x / 2, y / 2), "photo", "12mp", quality=75)
        small = shot.resize((PHOTO_W // 2, PHOTO_H // 2), Image.Resampling.LANCZOS)
        add("images", f"{spec.name}_photo_3mp", "jpg", small, lambda x, y: [v / 2 for v in apply_homography(forward, x / 2, y / 2)], "photo", "3mp", quality=60)

    if clean300 is not None and "speed" in sets:
        # 量速度用的页：noise、300 dpi，一张 PNG，一份“扫描仪出的 PDF”（整页一张灰度 JPEG）。
        img = degrade_scan(clean300, variant_rng("noise:300"))
        stem = f"speed_{lang}_{spec.doc}{'_dense' if item_rows else ''}_noise_300"
        add("speed", stem, "png", img, scaled(2), "noise", "300", compress_level=3)
        img.save(out / "speed" / f"{stem}.pdf", "PDF", resolution=300.0, quality=85)

    page_info: dict[str, object] = {
        "lang": lang,
        "doc": spec.doc,
        "font": spec.font,
        "leading": spec.leading,
        "ruled": spec.ruled,
        "seal": spec.seal,
        "angle": spec.angle,
        "anns": [{"kind": ln.kind, "font": ln.font, "size": ln.size, "under_seal": ln.under_seal} for ln in lines]
        + ([{"kind": "seal"}] if seal_box is not None else []),
    }
    return page_info, emitted


def main() -> None:
    ap = argparse.ArgumentParser(description="生成 OCR 自测用的合成图")
    ap.add_argument("--out", default=str(Path(__file__).parent / "out"))
    ap.add_argument("--pages-per-lang", type=int, default=30)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--langs", nargs="+", default=["zh", "en", "mixed"], choices=["zh", "en", "mixed"])
    ap.add_argument("--sets", nargs="+", default=ALL_SETS, choices=ALL_SETS, help="出哪几套变体")
    ap.add_argument("--jobs", type=int, default=min(6, os.cpu_count() or 1), help="并行的进程数")
    args = ap.parse_args()
    out = Path(args.out)
    (out / "images").mkdir(parents=True, exist_ok=True)
    (out / "speed").mkdir(parents=True, exist_ok=True)

    page_sets = [s for s in args.sets if s != "speed"]
    tasks: list[tuple[str, int, str, int, str | None, int | None, list[str]]] = []
    if page_sets:
        for lang in args.langs:
            for i in range(1, args.pages_per_lang + 1):
                tasks.append((str(out), args.seed, lang, i, None, None, page_sets))
    if "speed" in args.sets:
        tasks.append((str(out), args.seed, "zh", 901, "contract", None, ["speed"]))
        tasks.append((str(out), args.seed, "en", 902, "bl", None, ["speed"]))
        tasks.append((str(out), args.seed, "en", 903, "packing", 30, ["speed"]))

    if args.jobs > 1:
        with ProcessPoolExecutor(max_workers=args.jobs) as pool:
            results = list(pool.map(render_page, tasks))
    else:
        results = [render_page(t) for t in tasks]

    labels: dict[str, list[str]] = {"images": [], "speed": []}
    langs: dict[str, str] = {}
    meta_pages: dict[str, object] = {}
    meta_images: dict[str, object] = {}
    for task, (page_info, emitted) in zip(tasks, results):
        key = emitted[0].info["page"] if emitted else None
        for e in emitted:
            sub = e.file.split("/")[0]
            labels[sub].append(e.label)
            if sub == "images":
                langs[e.stem] = str(e.info["lang"])
                meta_images[e.stem] = {k: v for k, v in e.info.items() if k != "lang"}
            else:
                meta_images[e.stem] = {**{k: v for k, v in e.info.items() if k != "lang"}, "page": e.stem}
                meta_pages[e.stem] = page_info
        if key is not None and task[4] is None:
            meta_pages[str(key)] = page_info
    for sub, lines in labels.items():
        if lines:
            (out / ("Label.txt" if sub == "images" else "speed/Label.txt")).write_text("\n".join(sorted(lines)) + "\n", encoding="utf-8")
    if langs:
        (out / "langs.json").write_text(json.dumps(dict(sorted(langs.items())), ensure_ascii=False, indent=1), encoding="utf-8")
    meta = {
        "generator": {
            "script": "tests/corpus/synthetic-ocr/gen_synthetic.py",
            "seed": args.seed,
            "pages_per_lang": args.pages_per_lang,
            "langs": args.langs,
            "sets": args.sets,
            "pillow": PILLOW_VERSION,
            "master_dpi": MASTER_DPI,
        },
        "pages": meta_pages,
        "images": dict(sorted(meta_images.items())),
    }
    (out / "meta.json").write_text(json.dumps(meta, ensure_ascii=False, indent=1), encoding="utf-8")
    n_text = sum(1 for p in meta_pages.values() for a in p["anns"] if a["kind"] != "seal")  # type: ignore[index]
    print(f"底页 {len(meta_pages)} 个（文字行共 {n_text} 条），图 {len(meta_images)} 张，写入 {out}")


if __name__ == "__main__":
    main()
