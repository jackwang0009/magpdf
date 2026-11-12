#!/usr/bin/env python3
"""V11 背景修补原型：造 50 个文字行的样本（《P0 技术验证计划》V11，方案 6.3、6.4）。

真实的扫描件没有，样本是画出来的；内容全部虚构（货代单证里常见的字段和句子）。
每个样本是一张 1280×560 的图，相当于 300 dpi 扫描页上 108×47 mm 的一块：中间一行是要抹掉的字（目标行），
上下是不动的邻行。三类背景：

  plain    纯色纸：白纸、发黄发灰的纸、彩色联单、底色不匀、装订侧的阴影、带表格线和下划线的   17 个
  texture  底纹：细线网纹（斜线、波浪线、方格）、点阵、渐变、浅色水印字                       17 个
  seal     压着红色圆章（外圈、五角星、环形文字、横排文字、编码），章和字半透明地叠在一起     16 个

每个样本写这些文件（tests/out/v11/samples/<编号>/，不入库）：

  input.png     带字的图（先模糊、加噪声、按 4:2:0 压成 JPEG 再解码，存成 PNG 免得各家解码器结果不同）
  ref.png       对照：同样的纸、底纹、章、邻行、模糊、噪声（同一份）、JPEG 质量，只是没有目标行
  gt_mask.png   字形像素的真值掩膜：目标行字形本来的形状（模糊之前覆盖率过半的像素）
  gt_alpha.png  模糊之后目标行让画面变暗了多少（8 位）：笔画边上发虚的一圈也在里面，≥26 的像素肉眼看得出
  nostruct.png  有章、底纹线条或表格线的样本才有：把这些去掉之后的对照图，用来量它们被抹掉了多少
  struct.png    同上：章、底纹线条、表格线所在的像素

另有 samples/index.json：每个样本的类别、文字、字体、字号、目标行的框（四个角点）和各项参数。

同一个种子、同一版 Pillow 和 OpenCV、同一批 Windows 字体，生成的文件逐字节相同。
字体用 Windows 自带的宋体、黑体、楷体、仿宋、微软雅黑、Arial、Times New Roman、Courier New、Calibri，
生成的图只在本机评估用，不入库、不分发。

用法：python gen_samples.py [--out tests/out/v11] [--seed 20261112]
"""
from __future__ import annotations

import argparse
import functools
import math
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

import cv2
import numpy as np
import numpy.typing as npt
from PIL import Image as PilImage
from PIL import ImageChops, ImageDraw, ImageFont

from common import DEFAULT_OUT, DPI, imwrite, write_json

W, H = 1280, 560
# 合成时四周多出来的一圈：旋转之后裁掉，免得角上露出空白
MARGIN = 48
WC, HC = W + 2 * MARGIN, H + 2 * MARGIN

Float2D = npt.NDArray[np.float32]
Float3D = npt.NDArray[np.float32]
Rgb = tuple[int, int, int]

FONT_DIR = Path(r"C:\Windows\Fonts")
FONTS: dict[str, tuple[str, int]] = {
    "simsun": ("simsun.ttc", 0),
    "simhei": ("simhei.ttf", 0),
    "kaiti": ("simkai.ttf", 0),
    "fangsong": ("simfang.ttf", 0),
    "msyh": ("msyh.ttc", 0),
    "msyhbd": ("msyhbd.ttc", 0),
    "arial": ("arial.ttf", 0),
    "arialbd": ("arialbd.ttf", 0),
    "times": ("times.ttf", 0),
    "cour": ("cour.ttf", 0),
    "calibri": ("calibri.ttf", 0),
}


def pt_to_px(size_pt: float) -> int:
    return int(round(size_pt / 72.0 * DPI))


def mm_to_px(mm: float) -> int:
    return int(round(mm / 25.4 * DPI))


@functools.lru_cache(maxsize=None)
def load_font(key: str, px: int) -> ImageFont.FreeTypeFont:
    name, index = FONTS[key]
    return ImageFont.truetype(str(FONT_DIR / name), px, index=index)


# ---------------------------------------------------------------------------
# 文字内容（全部虚构）
# ---------------------------------------------------------------------------

ZH_COMPANIES = [
    "宏达国际货运代理有限公司",
    "蓝港物流（上海）有限公司",
    "金桥进出口贸易有限公司",
    "长风纺织品制造有限公司",
    "远洋船务代理股份有限公司",
]
EN_COMPANIES = [
    "SUNRISE TRADING CO., LTD.",
    "BLUE HARBOR LOGISTICS GMBH",
    "NORTHWIND IMPORTS INC.",
    "GOLDEN BRIDGE SUPPLY LLC",
    "EVERGREEN TEXTILE PTE LTD",
]
PORTS_EN = ["SHANGHAI", "NINGBO", "SHENZHEN", "QINGDAO", "ROTTERDAM", "HAMBURG", "SINGAPORE", "BUSAN"]
PORTS_ZH = ["上海港", "宁波港", "深圳盐田港", "青岛港", "鹿特丹港", "汉堡港", "新加坡港", "釜山港"]
GOODS_EN = ["STAINLESS STEEL PIPES", "GARDEN FURNITURE", "LED LAMPS", "CERAMIC TILES", "AUTO PARTS"]
ZH_SENTENCES = [
    "本提单项下货物已按表面状况良好装船，承运人对内容不负责任。",
    "运费预付，到港后凭正本提单一份放货，其余各份自动失效。",
    "托运人保证所申报的货物名称、数量与重量准确无误。",
    "集装箱由托运人装箱并铅封，箱内货物以托运人申报为准。",
    "本合同一式两份，双方各执一份，自双方签字盖章之日起生效。",
    "如发生争议，双方应友好协商解决；协商不成的，提交仲裁。",
    "装运期：收到信用证后四十五天内装运，允许分批装运与转运。",
    "保险由买方自理；卖方应在装船后两个工作日内发出装船通知。",
    "品质以出口商检机构签发的检验证书为最终依据。",
]
EN_SENTENCES = [
    "SHIPPED ON BOARD IN APPARENT GOOD ORDER AND CONDITION UNLESS OTHERWISE STATED.",
    "Freight prepaid. One original bill of lading must be surrendered duly endorsed.",
    "Shipper's load, stow, count and seal. Said to contain the goods described below.",
    "This Agreement shall be governed by and construed in accordance with the laws of Singapore.",
    "Payment: 30% deposit by T/T, balance against copy of shipping documents.",
    "Partial shipment allowed. Transhipment allowed. Latest shipment date as per L/C.",
    "Demurrage and detention charges, if any, are for the account of the consignee.",
    "Quantity and quality certificates issued by the inspection bureau shall be final.",
]
TITLES = {
    "zh": ["海运出口委托书", "国际货物买卖合同", "订舱确认书", "装箱单"],
    "en": ["BILL OF LADING", "PACKING LIST", "COMMERCIAL INVOICE", "BOOKING CONFIRMATION"],
    "mixed": ["海运提单 BILL OF LADING", "装箱单 PACKING LIST", "商业发票 INVOICE"],
}
SEAL_NAMES = [
    "宏达国际货运代理有限公司",
    "蓝港物流上海有限公司",
    "金桥进出口贸易有限公司",
    "远洋船务代理股份有限公司",
    "东方环球供应链管理有限公司",
]
SEAL_BOTTOMS = ["合同专用章", "业务专用章", "提单专用章", "财务专用章", "报关专用章"]
MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"]


def pick(rng: np.random.Generator, items: list[str]) -> str:
    return items[int(rng.integers(len(items)))]


def rand_container(rng: np.random.Generator) -> str:
    letters = "".join(chr(ord("A") + int(v)) for v in rng.integers(0, 26, 3))
    return f"{letters}U{int(rng.integers(1000000, 9999999))}"


def rand_amount(rng: np.random.Generator) -> str:
    return f"{int(rng.integers(1, 999))},{int(rng.integers(0, 999)):03d}.{int(rng.integers(0, 99)):02d}"


def make_line(rng: np.random.Generator, lang: str, *, title: bool = False) -> str:
    """随机出一行字：一半是句子，一半是“字段：值”。"""
    if title:
        return pick(rng, TITLES[lang])
    y, m, d = int(rng.integers(2021, 2027)), int(rng.integers(1, 13)), int(rng.integers(1, 29))
    if lang == "zh":
        fields = [
            f"收货人：{pick(rng, ZH_COMPANIES)}",
            f"托运人：{pick(rng, ZH_COMPANIES)}",
            f"起运港：{pick(rng, PORTS_ZH)}，目的港：{pick(rng, PORTS_ZH)}",
            f"装船日期：{y}年{m}月{d}日",
            f"集装箱号：{rand_container(rng)}，封号：{int(rng.integers(100000, 999999))}",
            f"毛重：{int(rng.integers(100, 25000))}.{int(rng.integers(0, 99)):02d}千克",
            f"合计金额：人民币{rand_amount(rng)}元",
        ]
        return pick(rng, ZH_SENTENCES) if rng.random() < 0.5 else pick(rng, fields)
    if lang == "en":
        fields = [
            f"Consignee: {pick(rng, EN_COMPANIES)}",
            f"Shipper: {pick(rng, EN_COMPANIES)}",
            f"Port of Loading: {pick(rng, PORTS_EN)}, Port of Discharge: {pick(rng, PORTS_EN)}",
            f"Date of Issue: {d:02d} {MONTHS[m - 1]} {y}",
            f"Container No. {rand_container(rng)} / Seal No. {int(rng.integers(100000, 999999))}",
            f"Gross Weight: {rand_amount(rng)} KGS",
            f"Total Amount: USD {rand_amount(rng)}",
            f"Description of Goods: {pick(rng, GOODS_EN)}",
        ]
        return pick(rng, EN_SENTENCES) if rng.random() < 0.5 else pick(rng, fields)
    fields = [
        f"收货人 Consignee: {pick(rng, EN_COMPANIES)}",
        f"托运人 Shipper: {pick(rng, ZH_COMPANIES)}",
        f"目的港 Port of Discharge: {pick(rng, PORTS_EN)}",
        f"提单号 B/L No.: COSU{int(rng.integers(600000000, 699999999))}",
        f"签发日期 Date of Issue: {d:02d} {MONTHS[m - 1]} {y}",
        f"箱号 Container No.: {rand_container(rng)}",
        f"运费 Freight: PREPAID 预付，金额 USD {rand_amount(rng)}",
        f"货物描述 Description: {pick(rng, GOODS_EN)}",
    ]
    return pick(rng, fields)


def fit_text(text: str, font: ImageFont.FreeTypeFont, max_w: float) -> str:
    """太长就从后面截：有空格的按词截，没有的按字截。"""
    if font.getlength(text) <= max_w:
        return text
    if " " in text:
        words = text.split(" ")
        while len(words) > 2 and font.getlength(" ".join(words)) > max_w:
            words.pop()
        text = " ".join(words)
    while len(text) > 4 and font.getlength(text) > max_w:
        text = text[:-1]
    return text.rstrip(" ，、；：,;:")


# ---------------------------------------------------------------------------
# 样本的参数
# ---------------------------------------------------------------------------


@dataclass
class Spec:
    sid: str
    category: str  # plain / texture / seal
    tag: str  # 子类
    lang: str  # zh / en / mixed
    font: str
    size_pt: float
    leading: float  # 行距是字号的几倍
    text_rgb: Rgb
    paper_rgb: Rgb
    uneven: float = 0.0  # 底色不匀的幅度（相对值）
    shadow: tuple[str, float] | None = None  # (从哪一侧, 最深处暗多少)
    specks: int = 0  # 纸上的脏点个数
    gradient: dict[str, Any] | None = None
    texture: dict[str, Any] | None = None
    rules: str | None = None  # underline / box
    seal: dict[str, Any] | None = None
    skew_deg: float = 0.0
    blur_sigma: float = 0.7
    noise_sigma: float = 3.0
    jpeg_quality: int = 80
    extra: dict[str, Any] = field(default_factory=dict)


SIZES = [10.5, 9, 12, 10.5, 14, 10.5, 12, 9, 16, 10.5, 12, 18, 9, 14, 10.5, 22, 12]
LANGS = ["zh", "en", "mixed"]
ZH_FONTS = ["simsun", "simhei", "kaiti", "fangsong", "msyh"]
EN_FONTS = ["arial", "times", "cour", "calibri"]
MIXED_FONTS = ["simsun", "msyh", "simhei"]
LEADINGS = [1.7, 1.5, 2.0, 1.35, 1.8]
BLURS = [0.6, 0.8, 1.0, 0.7, 0.9, 1.1, 0.5]
NOISES = [2.0, 3.5, 5.0, 1.5, 4.0, 6.0, 2.5]
JPEGS = [85, 75, 90, 70, 80, 60]
SKEWS = [0.4, -0.7, 1.0, -0.3, 0.6, -1.1]
DARK_BLUE: Rgb = (34, 44, 112)

PLAIN: list[dict[str, Any]] = [
    {"tag": "white", "paper_rgb": (252, 252, 250)},
    {"tag": "white", "paper_rgb": (248, 248, 246), "text_rgb": (96, 96, 96)},
    {"tag": "white", "paper_rgb": (255, 255, 255)},
    {"tag": "offwhite", "paper_rgb": (244, 240, 228)},
    {"tag": "offwhite", "paper_rgb": (236, 232, 220), "specks": 40, "text_rgb": DARK_BLUE},
    {"tag": "gray", "paper_rgb": (226, 226, 222), "specks": 60},
    {"tag": "tinted", "paper_rgb": (250, 224, 228)},
    {"tag": "tinted", "paper_rgb": (252, 244, 196)},
    {"tag": "tinted", "paper_rgb": (218, 234, 248)},
    {"tag": "tinted", "paper_rgb": (222, 242, 226)},
    {"tag": "uneven", "paper_rgb": (246, 244, 238), "uneven": 0.05},
    {"tag": "uneven", "paper_rgb": (240, 236, 224), "uneven": 0.08, "specks": 30},
    {"tag": "uneven", "paper_rgb": (250, 250, 248), "uneven": 0.035, "text_rgb": DARK_BLUE},
    {"tag": "shadow", "paper_rgb": (248, 246, 240), "shadow": ("left", 0.24)},
    {"tag": "shadow", "paper_rgb": (244, 244, 244), "shadow": ("top", 0.30)},
    {"tag": "ruled", "paper_rgb": (250, 250, 248), "rules": "underline"},
    {"tag": "ruled", "paper_rgb": (246, 244, 236), "rules": "box"},
]

TEXTURE: list[dict[str, Any]] = [
    {"tag": "mesh", "texture": {"kind": "diag", "rgb": (150, 200, 170), "width": 1.5, "pitch": 12, "angle": 45}},
    {"tag": "mesh", "texture": {"kind": "diag", "rgb": (150, 185, 225), "width": 1.0, "pitch": 9, "angle": 30}},
    {"tag": "mesh", "texture": {"kind": "wave", "rgb": (232, 168, 172), "width": 1.5, "pitch": 11}},
    {"tag": "mesh", "texture": {"kind": "wave", "rgb": (186, 186, 186), "width": 2.0, "pitch": 15}},
    {"tag": "mesh", "texture": {"kind": "grid", "rgb": (160, 196, 232), "width": 1.0, "pitch": 14}, "text_rgb": DARK_BLUE},
    {"tag": "dots", "texture": {"kind": "dots", "rgb": (176, 176, 176), "radius": 2.0, "pitch": 9, "angle": 45}},
    {"tag": "dots", "texture": {"kind": "dots", "rgb": (150, 186, 228), "radius": 2.6, "pitch": 12, "angle": 0}},
    {"tag": "dots", "texture": {"kind": "dots", "rgb": (190, 190, 190), "radius": 1.5, "pitch": 6, "angle": 15}},
    {"tag": "dots", "texture": {"kind": "dots", "rgb": (156, 204, 168), "radius": 3.0, "pitch": 13, "angle": 45}},
    {"tag": "gradient", "gradient": {"kind": "linear", "c0": (252, 252, 252), "c1": (190, 214, 242), "angle": 0}},
    {"tag": "gradient", "gradient": {"kind": "linear", "c0": (255, 246, 216), "c1": (240, 196, 152), "angle": 90}, "text_rgb": (96, 96, 96)},
    {"tag": "gradient", "gradient": {"kind": "linear", "c0": (250, 250, 250), "c1": (150, 170, 200), "angle": 20}},
    {"tag": "gradient", "gradient": {"kind": "radial", "c0": (254, 254, 250), "c1": (188, 220, 196)}},
    {"tag": "watermark", "texture": {"kind": "watermark", "rgb": (214, 214, 214), "text": "副本", "px": 300, "font": "simhei"}},
    {"tag": "watermark", "texture": {"kind": "watermark", "rgb": (246, 204, 204), "text": "COPY", "px": 260, "font": "arialbd"}},
    {"tag": "watermark", "texture": {"kind": "watermark", "rgb": (198, 216, 240), "text": "样本", "px": 340, "font": "msyhbd"}},
    {"tag": "watermark", "texture": {"kind": "watermark", "rgb": (222, 222, 222), "text": "ORIGINAL", "px": 220, "font": "arialbd"}},
]

# dy：目标行的中线比章的圆心低多少，单位是章的半径（负数是在圆心上方）。
# frac：章的圆心落在目标行宽度的什么位置。
SEAL: list[dict[str, Any]] = [
    {"tag": "ring-text", "seal": {"mm": 40, "dy": -0.62, "frac": 0.70, "rgb": (220, 40, 45), "alpha": 0.80}},
    {"tag": "ring-text", "seal": {"mm": 42, "dy": -0.58, "frac": 0.35, "rgb": (205, 30, 60), "alpha": 0.70}},
    {"tag": "ring-text", "seal": {"mm": 38, "dy": -0.70, "frac": 0.55, "rgb": (228, 62, 50), "alpha": 0.90}},
    {"tag": "ring-text", "seal": {"mm": 45, "dy": -0.50, "frac": 0.80, "rgb": (214, 36, 40), "alpha": 0.62}},
    {"tag": "star", "seal": {"mm": 40, "dy": 0.0, "frac": 0.60, "rgb": (222, 44, 48), "alpha": 0.85}},
    {"tag": "star", "seal": {"mm": 42, "dy": 0.08, "frac": 0.30, "rgb": (200, 28, 52), "alpha": 0.75}},
    {"tag": "star", "seal": {"mm": 38, "dy": -0.10, "frac": 0.75, "rgb": (230, 70, 56), "alpha": 0.68}},
    {"tag": "star", "seal": {"mm": 45, "dy": 0.05, "frac": 0.50, "rgb": (180, 26, 38), "alpha": 0.92}},
    {"tag": "bottom-text", "seal": {"mm": 40, "dy": 0.52, "frac": 0.65, "rgb": (218, 38, 44), "alpha": 0.80}},
    {"tag": "bottom-text", "seal": {"mm": 42, "dy": 0.50, "frac": 0.40, "rgb": (226, 56, 46), "alpha": 0.72}},
    {"tag": "bottom-text", "seal": {"mm": 38, "dy": 0.55, "frac": 0.85, "rgb": (206, 32, 58), "alpha": 0.88}},
    {"tag": "ring-line", "seal": {"mm": 40, "dy": 0.84, "frac": 0.55, "rgb": (220, 42, 46), "alpha": 0.82}},
    {"tag": "ring-line", "seal": {"mm": 45, "dy": 0.88, "frac": 0.30, "rgb": (212, 34, 50), "alpha": 0.66}},
    {"tag": "ring-line", "seal": {"mm": 42, "dy": -0.95, "frac": 0.70, "rgb": (224, 50, 44), "alpha": 0.78}},
    {"tag": "ring-line", "seal": {"mm": 38, "dy": 0.96, "frac": 0.45, "rgb": (216, 40, 42), "alpha": 0.86}},
    {"tag": "ring-line", "seal": {"mm": 40, "dy": -0.93, "frac": 0.25, "rgb": (198, 30, 48), "alpha": 0.74}},
]
SEAL_PAPERS: list[Rgb] = [(252, 252, 250), (246, 243, 234), (250, 250, 250), (240, 238, 228)]


def build_specs() -> list[Spec]:
    """50 个样本的参数表。字号、语言、字体、模糊、噪声、JPEG 质量按固定的顺序轮着取，保证每一类里都有。"""
    specs: list[Spec] = []
    groups: list[tuple[str, str, list[dict[str, Any]]]] = [
        ("plain", "p", PLAIN),
        ("texture", "t", TEXTURE),
        ("seal", "s", SEAL),
    ]
    n = 0
    for gi, (category, prefix, rows) in enumerate(groups):
        for i, row in enumerate(rows):
            lang = LANGS[(i + gi) % 3]
            size = SIZES[(i + 5 * gi) % len(SIZES)]
            if size >= 18:
                font = {"zh": "simhei", "en": "arialbd", "mixed": "msyhbd"}[lang]
            elif lang == "zh":
                font = ZH_FONTS[(i + gi) % len(ZH_FONTS)]
            elif lang == "en":
                font = EN_FONTS[(i + gi) % len(EN_FONTS)]
            else:
                # 中英混排每三个样本轮到一次，按轮到的次数换字体
                font = MIXED_FONTS[((i + gi) // 3) % len(MIXED_FONTS)]
            gray = [28, 40, 52, 34, 46][n % 5]
            spec = Spec(
                sid=f"{prefix}{i + 1:02d}",
                category=category,
                tag=row["tag"],
                lang=lang,
                font=font,
                size_pt=size,
                leading=LEADINGS[n % len(LEADINGS)],
                text_rgb=row.get("text_rgb", (gray, gray, gray)),
                paper_rgb=row.get("paper_rgb", SEAL_PAPERS[i % len(SEAL_PAPERS)] if category == "seal" else (250, 250, 248)),
                uneven=row.get("uneven", 0.03 if category == "seal" and i % 4 == 1 else 0.0),
                shadow=row.get("shadow"),
                specks=row.get("specks", 0),
                gradient=row.get("gradient"),
                texture=row.get("texture"),
                rules=row.get("rules"),
                seal=row.get("seal"),
                skew_deg=SKEWS[(n // 4) % len(SKEWS)] if n % 4 == 2 else 0.0,
                blur_sigma=BLURS[n % len(BLURS)],
                noise_sigma=NOISES[n % len(NOISES)],
                jpeg_quality=JPEGS[n % len(JPEGS)],
            )
            specs.append(spec)
            n += 1
    return specs


# ---------------------------------------------------------------------------
# 纸、底纹、章
# ---------------------------------------------------------------------------


def low_freq(rng: np.random.Generator, grid: tuple[int, int] = (5, 9)) -> Float2D:
    """一张缓慢起伏的随机场，值在 -1..1。"""
    small = rng.normal(size=grid).astype(np.float32)
    big = cv2.resize(small, (WC, HC), interpolation=cv2.INTER_CUBIC)
    big -= float(big.mean())
    return np.asarray(big / max(float(np.abs(big).max()), 1e-6), dtype=np.float32)


def make_paper(rng: np.random.Generator, spec: Spec) -> Float3D:
    """纸的底色（RGB，0..255 的浮点数）：纯色或渐变，加上不匀、阴影、脏点。"""
    yy, xx = np.mgrid[0:HC, 0:WC].astype(np.float32)
    if spec.gradient is not None:
        g = spec.gradient
        c0 = np.asarray(g["c0"], dtype=np.float32)
        c1 = np.asarray(g["c1"], dtype=np.float32)
        if g["kind"] == "linear":
            a = math.radians(float(g["angle"]))
            t = (xx - WC / 2) * math.cos(a) / WC + (yy - HC / 2) * math.sin(a) / HC + 0.5
        else:
            t = np.hypot((xx - WC * 0.55) / (WC * 0.5), (yy - HC * 0.5) / (HC * 0.6))
        t = np.clip(t, 0.0, 1.0)[..., None]
        paper = c0 + (c1 - c0) * t
    else:
        paper = np.empty((HC, WC, 3), dtype=np.float32)
        paper[:] = np.asarray(spec.paper_rgb, dtype=np.float32)
    if spec.uneven > 0:
        paper = paper * (1.0 + spec.uneven * low_freq(rng)[..., None])
    if spec.shadow is not None:
        side, depth = spec.shadow
        # 阴影从一侧开始，越往里越淡，到图的六成处消失
        pos = xx / WC if side == "left" else yy / HC
        fall = np.clip(1.0 - pos / 0.6, 0.0, 1.0) ** 1.6
        paper = paper * (1.0 - depth * fall[..., None])
    if spec.specks > 0:
        dirt = np.zeros((HC, WC), dtype=np.float32)
        xs = rng.integers(0, WC, spec.specks)
        ys = rng.integers(0, HC, spec.specks)
        strength = rng.uniform(0.25, 0.7, spec.specks)
        for x, y, s in zip(xs, ys, strength):
            dirt[int(y), int(x)] = float(s)
        dirt = cv2.GaussianBlur(dirt, (0, 0), 1.1) * 8.0
        paper = paper * (1.0 - np.clip(dirt, 0.0, 0.8)[..., None])
    return np.asarray(np.clip(paper, 0.0, 255.0), dtype=np.float32)


SS = 3  # 底纹线条先放大三倍画，再缩回来，边缘才平滑


def _downsample(layer: PilImage.Image) -> Float2D:
    arr = np.asarray(layer, dtype=np.float32) / 255.0
    return np.asarray(cv2.resize(arr, (WC, HC), interpolation=cv2.INTER_AREA), dtype=np.float32)


def texture_alpha(rng: np.random.Generator, tex: dict[str, Any]) -> Float2D:
    """底纹的覆盖率（0..1）。"""
    kind = tex["kind"]
    if kind == "dots":
        yy, xx = np.mgrid[0:HC, 0:WC].astype(np.float32)
        a = math.radians(float(tex["angle"]))
        p = float(tex["pitch"])
        u = xx * math.cos(a) + yy * math.sin(a)
        v = -xx * math.sin(a) + yy * math.cos(a)
        du = np.mod(u, p) - p / 2
        dv = np.mod(v, p) - p / 2
        return np.asarray(np.clip(float(tex["radius"]) - np.hypot(du, dv) + 0.5, 0.0, 1.0), dtype=np.float32)

    layer = PilImage.new("L", (WC * SS, HC * SS), 0)
    draw = ImageDraw.Draw(layer)
    if kind in ("diag", "grid"):
        width = max(1, int(round(float(tex["width"]) * SS)))
        pitch = float(tex["pitch"]) * SS
        angles = [0.0, 90.0] if kind == "grid" else [float(tex["angle"]), -float(tex["angle"])]
        cx, cy = WC * SS / 2, HC * SS / 2
        reach = math.hypot(WC * SS, HC * SS)
        for ang in angles:
            a = math.radians(ang)
            dx, dy = math.cos(a), math.sin(a)
            nx, ny = -dy, dx
            for k in range(-int(reach / pitch) - 1, int(reach / pitch) + 2):
                px, py = cx + nx * k * pitch, cy + ny * k * pitch
                draw.line([(px - dx * reach, py - dy * reach), (px + dx * reach, py + dy * reach)], fill=255, width=width)
    elif kind == "wave":
        # 一条条波浪线，相邻两条相位相反，交织成网纹
        width = max(1, int(round(float(tex["width"]) * SS)))
        pitch = float(tex["pitch"]) * SS
        amp = pitch * 0.9
        wavelength = pitch * 5.0
        xs = np.arange(0, WC * SS + 6, 6, dtype=np.float32)
        for i in range(-2, int(HC * SS / pitch) + 3):
            phase = math.pi * (i % 2)
            ys = i * pitch + amp * np.sin(2 * math.pi * xs / wavelength + phase)
            draw.line(list(zip(xs.tolist(), ys.tolist())), fill=255, width=width, joint="curve")
    elif kind == "watermark":
        font = load_font(str(tex["font"]), int(tex["px"]) * SS // 2)
        text = str(tex["text"])
        tile_w = int(font.getlength(text) * 1.2)
        tile_h = int(int(tex["px"]) * SS // 2 * 1.25)
        # 先在一张更大的图上平铺，转 28 度之后取中间一块
        big_w, big_h = int(WC * SS * 0.5 * 1.8), int(HC * SS * 0.5 * 2.4)
        big = PilImage.new("L", (big_w, big_h), 0)
        bd = ImageDraw.Draw(big)
        off = int(rng.integers(0, tile_w))
        for row, y in enumerate(range(0, big_h + tile_h, tile_h)):
            for x in range(-tile_w, big_w + tile_w, tile_w):
                bd.text((x + off + (tile_w // 2) * (row % 2), y), text, font=font, fill=255, anchor="lm")
        big = big.rotate(28, resample=PilImage.Resampling.BICUBIC)
        left, top = (big_w - WC * SS // 2) // 2, (big_h - HC * SS // 2) // 2
        half = big.crop((left, top, left + WC * SS // 2, top + HC * SS // 2))
        return np.asarray(cv2.resize(np.asarray(half, dtype=np.float32) / 255.0, (WC, HC), interpolation=cv2.INTER_LINEAR), dtype=np.float32)
    else:
        raise ValueError(f"不认识的底纹：{kind}")
    return _downsample(layer)


def seal_shape(diameter_px: int, name: str, bottom: str, code: str, font_key: str) -> Float2D:
    """圆章的形状（覆盖率 0..1）：外圈、五角星、上半圈的环形文字、横排文字、下沿的编码。先放大两倍画。"""
    ss = 2
    r = diameter_px / 2 * ss
    size = int(diameter_px * ss) + 16 * ss
    c = size / 2
    img = PilImage.new("L", (size, size), 0)
    draw = ImageDraw.Draw(img)
    lw = max(2, int(round(0.055 * r)))
    draw.ellipse([c - r, c - r, c + r, c + r], outline=255, width=lw)

    rs = 0.30 * r
    star = []
    for i in range(10):
        ang = -math.pi / 2 + i * math.pi / 5
        rr = rs if i % 2 == 0 else rs * 0.382
        star.append((c + rr * math.cos(ang), c + rr * math.sin(ang)))
    draw.polygon(star, fill=255)

    def paste_glyph(ch: str, font: ImageFont.FreeTypeFont, box: int, radius: float, ang_deg: float, rot_deg: float) -> None:
        glyph = PilImage.new("L", (box, box), 0)
        ImageDraw.Draw(glyph).text((box / 2, box / 2), ch, font=font, fill=255, anchor="mm")
        glyph = glyph.rotate(rot_deg, resample=PilImage.Resampling.BICUBIC)
        x = int(round(c + radius * math.sin(math.radians(ang_deg)) - box / 2))
        y = int(round(c - radius * math.cos(math.radians(ang_deg)) - box / 2))
        region = img.crop((x, y, x + box, y + box))
        img.paste(ImageChops.lighter(region, glyph), (x, y))

    # 环形文字：从正上方起顺时针量角度，字头朝外
    ch_h = 0.27 * r
    font = load_font(font_key, int(round(ch_h)))
    rt = r - lw - 0.07 * r - ch_h / 2
    span = min(250.0, 22.0 * (len(name) - 1))
    for i, ch in enumerate(name):
        ang = -span / 2 + span * i / max(len(name) - 1, 1)
        paste_glyph(ch, font, int(ch_h * 1.7), rt, ang, -ang)

    draw = ImageDraw.Draw(img)
    draw.text((c, c + 0.53 * r), bottom, font=load_font(font_key, int(round(0.2 * r))), fill=255, anchor="mm")

    # 下沿的编码：字头朝圆心，从左往右读
    code_h = 0.105 * r
    code_font = load_font("arial", int(round(code_h)))
    rc = r - lw - 0.045 * r - code_h / 2
    span_c = 6.4 * (len(code) - 1)
    for i, ch in enumerate(code):
        ang = 180.0 + span_c / 2 - span_c * i / max(len(code) - 1, 1)
        paste_glyph(ch, code_font, int(code_h * 2.0), rc, ang, 180.0 - ang)

    small = img.resize((size // ss, size // ss), PilImage.Resampling.LANCZOS)
    return np.asarray(small, dtype=np.float32) / 255.0


def seal_alpha(rng: np.random.Generator, seal: dict[str, Any], cx: float, cy: float) -> Float2D:
    """把章盖到画布上 (cx, cy) 处：整个章转一个随机的角度，印泥有深有浅，零星掉墨。"""
    diameter = mm_to_px(float(seal["mm"]))
    code = "".join(str(int(v)) for v in rng.integers(0, 10, 13))
    font_key = "simsun" if rng.random() < 0.6 else "simhei"
    shape = seal_shape(diameter, pick(rng, SEAL_NAMES), pick(rng, SEAL_BOTTOMS), code, font_key)
    pil = PilImage.fromarray(np.asarray(np.round(shape * 255), dtype=np.uint8), mode="L")
    pil = pil.rotate(float(rng.uniform(-22, 22)), resample=PilImage.Resampling.BICUBIC)
    shape = np.asarray(pil, dtype=np.float32) / 255.0

    layer = np.zeros((HC, WC), dtype=np.float32)
    sh, sw = shape.shape
    x0, y0 = int(round(cx - sw / 2)), int(round(cy - sh / 2))
    xa, ya = max(x0, 0), max(y0, 0)
    xb, yb = min(x0 + sw, WC), min(y0 + sh, HC)
    if xb > xa and yb > ya:
        layer[ya:yb, xa:xb] = shape[ya - y0 : yb - y0, xa - x0 : xb - x0]
    layer = cv2.GaussianBlur(layer, (0, 0), 0.7)

    ink = 0.62 + 0.38 * (low_freq(rng, (4, 7)) * 0.5 + 0.5)
    grain = cv2.GaussianBlur(rng.uniform(0.0, 1.0, (HC, WC)).astype(np.float32), (0, 0), 1.0)
    grain = np.clip((grain - 0.5) * 6.0 + 0.5, 0.0, 1.0)  # 拉开对比
    holes = (cv2.GaussianBlur(rng.uniform(0.0, 1.0, (HC, WC)).astype(np.float32), (0, 0), 1.3) > 0.585).astype(np.float32)
    holes = cv2.GaussianBlur(holes, (0, 0), 0.8)
    ink = ink * (0.82 + 0.18 * grain) * (1.0 - 0.85 * holes)
    return np.asarray(np.clip(layer * ink * float(seal["alpha"]), 0.0, 1.0), dtype=np.float32)


# ---------------------------------------------------------------------------
# 排字、合成、做旧
# ---------------------------------------------------------------------------


def text_layer(items: list[tuple[str, ImageFont.FreeTypeFont, float, float]]) -> Float2D:
    """把若干行字画成一层覆盖率。每项是（文字, 字体, 左边 x, 基线 y），画布坐标。"""
    layer = PilImage.new("L", (WC, HC), 0)
    draw = ImageDraw.Draw(layer)
    for text, font, x, baseline in items:
        draw.text((x, baseline), text, font=font, fill=255, anchor="ls")
    return np.asarray(layer, dtype=np.float32) / 255.0


def blend(base: Float3D, alpha: Float2D, rgb: Rgb) -> Float3D:
    """普通的覆盖：base 上盖一层颜色为 rgb、覆盖率为 alpha 的东西。"""
    a = alpha[..., None]
    return np.asarray(base * (1.0 - a) + np.asarray(rgb, dtype=np.float32) * a, dtype=np.float32)


def multiply(base: Float3D, alpha: Float2D, rgb: Rgb) -> Float3D:
    """印泥盖在纸和字上：按颜色相乘，红墨压在黑字上还是黑的，压在白纸上是红的。"""
    tint = 1.0 - alpha[..., None] * (1.0 - np.asarray(rgb, dtype=np.float32) / 255.0)
    return np.asarray(base * tint, dtype=np.float32)


def warp(layer: npt.NDArray[np.float32], matrix: npt.NDArray[np.float64] | None) -> npt.NDArray[np.float32]:
    if matrix is None:
        return layer
    out = cv2.warpAffine(layer, matrix, (WC, HC), flags=cv2.INTER_CUBIC, borderMode=cv2.BORDER_REFLECT)
    return np.asarray(out, dtype=np.float32)


def crop(layer: npt.NDArray[np.float32]) -> npt.NDArray[np.float32]:
    return layer[MARGIN : MARGIN + H, MARGIN : MARGIN + W]


def degrade(rgb: Float3D, blur_sigma: float, noise: Float3D, jpeg_quality: int) -> npt.NDArray[np.uint8]:
    """做成扫描件的样子：模糊、加噪声、按 4:2:0 压成 JPEG 再解码。返回 BGR。"""
    img = cv2.GaussianBlur(rgb, (0, 0), blur_sigma) + noise
    bgr = cv2.cvtColor(np.clip(np.round(img), 0, 255).astype(np.uint8), cv2.COLOR_RGB2BGR)
    ok, buf = cv2.imencode(
        ".jpg",
        bgr,
        [cv2.IMWRITE_JPEG_QUALITY, jpeg_quality, cv2.IMWRITE_JPEG_SAMPLING_FACTOR, cv2.IMWRITE_JPEG_SAMPLING_FACTOR_420],
    )
    if not ok:
        raise OSError("JPEG 编码失败")
    out = cv2.imdecode(buf, cv2.IMREAD_COLOR)
    return np.asarray(out, dtype=np.uint8)


def generate(spec: Spec, seed: int, index: int, out_dir: Path) -> dict[str, Any]:
    rng = np.random.default_rng([seed, index])
    px = pt_to_px(spec.size_pt)
    font = load_font(spec.font, px)
    is_title = spec.size_pt >= 16
    ctx_font_key = spec.font if not is_title else {"simhei": "simsun", "arialbd": "arial", "msyhbd": "msyh"}.get(spec.font, spec.font)
    ctx_px = px if not is_title else pt_to_px(10.5)
    ctx_font = load_font(ctx_font_key, ctx_px)

    # 目标行
    x_left = float(rng.integers(56, 150))
    target_text = fit_text(make_line(rng, spec.lang, title=is_title), font, W - x_left - 70)
    text_w = font.getlength(target_text)
    if is_title:
        x_left = float((W - text_w) / 2 + rng.integers(-60, 60))

    # 目标行的中线放在哪：有章的样本要让章的大部分留在图里
    center_y = H / 2 + float(rng.integers(-24, 24))
    radius = 0.0
    if spec.seal is not None:
        radius = mm_to_px(float(spec.seal["mm"])) / 2
        center_y = float(np.clip(H / 2 + float(spec.seal["dy"]) * radius * 0.75, 100, H - 100))
    baseline = center_y + px * 0.36

    # 邻行：同一种语言的随机句子。大字（标题）上下配 10.5 pt 的正文。
    ctx_items: list[tuple[str, ImageFont.FreeTypeFont, float, float]] = []
    pitch = ctx_px * spec.leading
    ctx_left = x_left if not is_title else float(rng.integers(56, 150))
    up = baseline - (px * spec.leading if not is_title else px * 0.92 + ctx_px * 0.7)
    down = baseline + (px * spec.leading if not is_title else px * 0.3 + ctx_px * 1.6)
    while up - ctx_px * 0.9 > 8:
        ctx_items.append((fit_text(make_line(rng, spec.lang), ctx_font, W - ctx_left - 70), ctx_font, ctx_left + MARGIN, up + MARGIN))
        up -= pitch
    while down + ctx_px * 0.3 < H - 8:
        ctx_items.append((fit_text(make_line(rng, spec.lang), ctx_font, W - ctx_left - 70), ctx_font, ctx_left + MARGIN, down + MARGIN))
        down += pitch

    tgt_a = text_layer([(target_text, font, x_left + MARGIN, baseline + MARGIN)])
    ctx_a = text_layer(ctx_items)
    ink = float(rng.uniform(0.86, 1.0))  # 墨粉不一定盖得严实：合成时字的覆盖率再乘上这个数

    # 目标行的框：字形像素的外接矩形（转之前量，四个角点跟着转）
    ys, xs = np.nonzero(tgt_a > 0.2)
    bx0, by0, bx1, by1 = float(xs.min()), float(ys.min()), float(xs.max() + 1), float(ys.max() + 1)

    paper = make_paper(rng, spec)

    tex_a: Float2D | None = None
    tex_rgb: Rgb = (0, 0, 0)
    if spec.texture is not None:
        tex_a = texture_alpha(rng, spec.texture) * float(spec.texture.get("opacity", 1.0))
        tex_rgb = tuple(spec.texture["rgb"])

    rule_a: Float2D | None = None
    if spec.rules is not None:
        layer = PilImage.new("L", (WC, HC), 0)
        draw = ImageDraw.Draw(layer)
        lw = 3 if px >= 50 else 2
        if spec.rules == "underline":
            # 表单的填写线：每一行字下面一条，贴着字的下沿
            for _, _, lx, lb in [(target_text, font, x_left + MARGIN, baseline + MARGIN), *ctx_items]:
                y = int(round(lb + px * 0.16))
                draw.line([(lx - 24, y), (W + MARGIN - 40, y)], fill=255, width=lw)
        else:
            # 表格：行与行之间一条横线，左右各一条竖线，目标行的格子收得比较紧
            top = int(round(by0 - px * 0.30))
            bottom = int(round(by1 + px * 0.30))
            left = int(round(bx0 - px * 0.45))
            right = int(round(min(bx1 + px * 0.45, WC - MARGIN - 20)))
            for y in (top, bottom):
                draw.line([(left, y), (right, y)], fill=255, width=lw)
            step = int(round(px * spec.leading))
            y = top - step
            while y > 0:
                draw.line([(left, y), (right, y)], fill=255, width=lw)
                y -= step
            y = bottom + step
            while y < HC:
                draw.line([(left, y), (right, y)], fill=255, width=lw)
                y += step
            for x in (left, right):
                draw.line([(x, 0), (x, HC)], fill=255, width=lw)
        rule_a = np.asarray(layer, dtype=np.float32) / 255.0

    seal_a: Float2D | None = None
    seal_rgb: Rgb = (0, 0, 0)
    if spec.seal is not None:
        cx = x_left + float(spec.seal["frac"]) * text_w + MARGIN
        cy = center_y - float(spec.seal["dy"]) * radius + MARGIN
        seal_a = seal_alpha(rng, spec.seal, cx, cy)
        seal_rgb = tuple(spec.seal["rgb"])

    # 整页轻微歪斜：各层一起转
    matrix: npt.NDArray[np.float64] | None = None
    quad = np.array([[bx0, by0], [bx1, by0], [bx1, by1], [bx0, by1]], dtype=np.float64)
    if spec.skew_deg != 0.0:
        matrix = cv2.getRotationMatrix2D((WC / 2, HC / 2), spec.skew_deg, 1.0)
        quad = quad @ matrix[:, :2].T + matrix[:, 2]
        paper = warp(paper, matrix)
        tgt_a = np.clip(warp(tgt_a, matrix), 0.0, 1.0)
        ctx_a = np.clip(warp(ctx_a, matrix), 0.0, 1.0)
        if tex_a is not None:
            tex_a = np.clip(warp(tex_a, matrix), 0.0, 1.0)
        if rule_a is not None:
            rule_a = np.clip(warp(rule_a, matrix), 0.0, 1.0)
        if seal_a is not None:
            seal_a = np.clip(warp(seal_a, matrix), 0.0, 1.0)
    quad -= MARGIN

    rule_rgb: Rgb = (48, 48, 48)

    def compose(with_target: bool, with_struct: bool) -> Float3D:
        """按纸 → 底纹 → 表格线 → 字 → 章的顺序叠起来。"""
        page = paper
        if with_struct and tex_a is not None:
            page = blend(page, tex_a, tex_rgb)
        if with_struct and rule_a is not None:
            page = blend(page, rule_a, rule_rgb)
        page = blend(page, ctx_a * ink, spec.text_rgb)
        if with_target:
            page = blend(page, tgt_a * ink, spec.text_rgb)
        if with_struct and seal_a is not None:
            page = multiply(page, seal_a, seal_rgb)
        return np.asarray(crop(page), dtype=np.float32)

    sigma = spec.noise_sigma
    noise = (rng.normal(0, sigma * 0.85, (H, W, 1)) + rng.normal(0, sigma * 0.5, (H, W, 3))).astype(np.float32)

    sample_dir = out_dir / "samples" / spec.sid
    clean_ref = compose(False, True)
    imwrite(sample_dir / "input.png", degrade(compose(True, True), spec.blur_sigma, noise, spec.jpeg_quality))
    imwrite(sample_dir / "ref.png", degrade(clean_ref, spec.blur_sigma, noise, spec.jpeg_quality))

    # 真值：gt_mask 是字形本来的形状（模糊之前覆盖率过半的像素）；gt_alpha 是模糊之后这些字让画面变暗了多少，
    # 笔画边上发虚的一圈也在里面。
    footprint = np.asarray(crop(tgt_a), dtype=np.float32)
    gt_mask = np.where(footprint >= 0.5, 255, 0).astype(np.uint8)
    gt_alpha = cv2.GaussianBlur(footprint * ink, (0, 0), spec.blur_sigma)
    imwrite(sample_dir / "gt_alpha.png", np.clip(np.round(gt_alpha * 255), 0, 255).astype(np.uint8))
    imwrite(sample_dir / "gt_mask.png", gt_mask)

    has_struct = tex_a is not None or rule_a is not None or seal_a is not None
    struct_kind = "seal" if seal_a is not None else "rule" if rule_a is not None else "texture" if tex_a is not None else None
    struct_pixels = 0
    if has_struct:
        clean_nostruct = compose(False, False)
        imwrite(sample_dir / "nostruct.png", degrade(clean_nostruct, spec.blur_sigma, noise, spec.jpeg_quality))
        # 章、底纹线条、表格线所在的像素：去掉它们之后颜色变了 24 以上（三个通道的欧氏距离）的地方
        diff = cv2.GaussianBlur(clean_ref, (0, 0), spec.blur_sigma) - cv2.GaussianBlur(clean_nostruct, (0, 0), spec.blur_sigma)
        struct = np.where(np.linalg.norm(diff, axis=2) >= 24.0, 255, 0).astype(np.uint8)
        struct_pixels = int(np.count_nonzero(struct))
        imwrite(sample_dir / "struct.png", struct)

    meta: dict[str, Any] = asdict(spec)
    meta.update(
        {
            "text": target_text,
            "font_px": px,
            "quad": [[round(float(x), 2), round(float(y), 2)] for x, y in quad],
            "glyph_pixels": int(np.count_nonzero(gt_mask)),
            "struct_kind": struct_kind,
            "struct_pixels": struct_pixels,
            "size": [W, H],
        }
    )
    return meta


def main() -> None:
    ap = argparse.ArgumentParser(description="造 V11 的 50 个文字行样本")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--seed", type=int, default=20261112)
    ap.add_argument("--only", nargs="*", help="只生成这几个编号（调试用）；这时不改写 index.json")
    args = ap.parse_args()

    specs = build_specs()
    metas: list[dict[str, Any]] = []
    for index, spec in enumerate(specs):
        if args.only and spec.sid not in args.only:
            continue
        metas.append(generate(spec, args.seed, index, args.out))
    if not args.only:
        write_json(args.out / "samples" / "index.json", {"fmt": "magpdf-v11-samples/1", "seed": args.seed, "dpi": DPI, "samples": metas})
    counts: dict[str, int] = {}
    for m in metas:
        counts[m["category"]] = counts.get(m["category"], 0) + 1
    print(f"生成 {len(metas)} 个样本：{counts}，写到 {args.out / 'samples'}")


if __name__ == "__main__":
    main()
