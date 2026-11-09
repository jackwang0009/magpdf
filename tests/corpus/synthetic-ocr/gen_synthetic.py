#!/usr/bin/env python3
"""生成 OCR 自测用的"打印体单证"合成图（V09）。

- 内容全部虚构（货代单证/合同常见字段与句子），不含任何真实文件；
- 300 dpi A4（2480×3508），微软雅黑 / 宋体 / Arial / Times / Courier；
- 变体：clean（干净）、noise（高斯噪声+轻微模糊）、rot（±1.5° 旋转）、noiserot（两者）；
- 输出 out/images/*.png 与 out/Label.txt（PPOCRLabel 格式，脱敏规范 §7），out/langs.json；
- 图片不入库（out/ 已在 .gitignore）。

用法：python gen_synthetic.py [--out out] [--pages-per-lang 6] [--seed 7]
"""
from __future__ import annotations

import argparse
import json
import math
import random
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

DPI = 300
PAGE_W, PAGE_H = 2480, 3508
FONT_DIR = Path(r"C:\Windows\Fonts")
FONTS = {
    "msyh": FONT_DIR / "msyh.ttc",
    "simsun": FONT_DIR / "simsun.ttc",
    "arial": FONT_DIR / "arial.ttf",
    "times": FONT_DIR / "times.ttf",
    "cour": FONT_DIR / "cour.ttf",
}


def pt(size_pt: float) -> int:
    return int(round(size_pt / 72 * DPI))


PORTS = ["SHANGHAI", "NINGBO", "SHENZHEN", "QINGDAO", "ROTTERDAM", "HAMBURG", "LOS ANGELES", "SINGAPORE", "FELIXSTOWE", "BUSAN"]
GOODS = ["STAINLESS STEEL PIPES", "GARDEN FURNITURE", "LED LAMPS", "CERAMIC TILES", "AUTO PARTS", "COTTON T-SHIRTS", "PLASTIC TOYS", "SOLAR PANELS"]
COMPANIES_EN = ["SUNRISE TRADING CO., LTD.", "BLUE HARBOR LOGISTICS GMBH", "NORTHWIND IMPORTS INC.", "GOLDEN BRIDGE SUPPLY LLC", "EVERGREEN TEXTILE PTE LTD"]
COMPANIES_ZH = ["宏达国际货运代理有限公司", "蓝港物流（上海）有限公司", "金桥进出口贸易有限公司", "长风纺织品制造有限公司", "远洋船务代理股份有限公司"]
ZH_FIELDS = ["托运人", "收货人", "通知方", "起运港", "卸货港", "目的港", "船名航次", "集装箱号", "封号", "件数", "包装", "毛重", "体积", "运费", "装船日期", "签发地点", "正本份数"]
ZH_SENTENCES = [
    "本提单项下货物已按表面状况良好装船，承运人对内容不负责任。",
    "运费预付，到港后凭正本提单一份放货，其余各份自动失效。",
    "托运人保证所申报的货物名称、数量与重量准确无误。",
    "集装箱由托运人装箱并铅封，箱内货物以托运人申报为准。",
    "本合同一式两份，双方各执一份，自双方签字盖章之日起生效。",
    "如发生争议，双方应友好协商解决；协商不成，提交上海仲裁委员会仲裁。",
    "付款方式：见票后三十天付款的即期信用证，不可撤销。",
    "装运期：收到信用证后四十五天内装运，允许分批装运与转运。",
    "保险由买方自理；卖方应在装船后两个工作日内传真装船通知。",
    "品质以出口商检局签发的检验证书为最终依据。",
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
]


def rand_container(rng: random.Random) -> str:
    prefix = "".join(rng.choice("ABCDEFGHIJKLMNOPQRSTUVWXYZ") for _ in range(3)) + "U"
    return f"{prefix}{rng.randint(1000000, 9999999)}"


def rand_bl(rng: random.Random) -> str:
    return f"{rng.choice(['COSU', 'MAEU', 'OOLU', 'HLCU', 'EGLV'])}{rng.randint(600000000, 699999999)}"


def rand_date(rng: random.Random, zh: bool) -> str:
    y, m, d = rng.randint(2021, 2026), rng.randint(1, 12), rng.randint(1, 28)
    return f"{y}年{m}月{d}日" if zh else f"{d:02d} {['JAN','FEB','MAR','APR','MAY','JUN','JUL','AUG','SEP','OCT','NOV','DEC'][m-1]} {y}"


def rand_amount(rng: random.Random) -> str:
    return f"USD {rng.randint(1, 999):,}," + f"{rng.randint(0, 999):03d}.{rng.randint(0, 99):02d}"


def build_lines(rng: random.Random, lang: str) -> list[tuple[str, str, float]]:
    """返回 (文字, 字体键, 字号 pt) 列表，按版面顺序。"""
    lines: list[tuple[str, str, float]] = []
    zh_font = rng.choice(["msyh", "simsun"])
    en_font = rng.choice(["arial", "times", "cour"])
    if lang == "zh":
        lines.append((rng.choice(["海运出口委托书", "国际货物买卖合同", "装箱单", "海运提单（副本）"]), zh_font, 18))
        lines.append((f"编号：{rand_bl(rng)}    日期：{rand_date(rng, True)}", zh_font, 10.5))
        for f in rng.sample(ZH_FIELDS, 9):
            val = {
                "托运人": rng.choice(COMPANIES_ZH), "收货人": rng.choice(COMPANIES_ZH), "通知方": rng.choice(COMPANIES_ZH),
                "起运港": "上海港", "卸货港": "鹿特丹港", "目的港": "汉堡港",
                "船名航次": f"长风{rng.randint(1, 99)}号 / {rng.randint(100, 999)}E",
                "集装箱号": rand_container(rng), "封号": f"SL{rng.randint(100000, 999999)}",
                "件数": f"{rng.randint(10, 2000)}箱", "包装": "纸箱", "毛重": f"{rng.randint(100, 25000)}.{rng.randint(0, 99):02d}千克",
                "体积": f"{rng.randint(1, 68)}.{rng.randint(0, 99):02d}立方米", "运费": rng.choice(["预付", "到付"]),
                "装船日期": rand_date(rng, True), "签发地点": "上海", "正本份数": "三份",
            }[f]
            lines.append((f"{f}：{val}", zh_font, 10.5))
        for s in rng.sample(ZH_SENTENCES, 6):
            lines.append((s, zh_font, 9))
        lines.append((f"合计金额：{rand_amount(rng)}（大写：{rng.choice(['壹拾贰万叁仟', '伍万陆仟捌佰', '玖拾万零伍佰'])}元整）", zh_font, 10.5))
        lines.append(("承运人签章：", zh_font, 10.5))
    elif lang == "en":
        lines.append((rng.choice(["COMMERCIAL INVOICE", "PACKING LIST", "BILL OF LADING", "BOOKING CONFIRMATION"]), en_font, 18))
        lines.append((f"Invoice No. INV-{rng.randint(10000, 99999)}    Date: {rand_date(rng, False)}", en_font, 10.5))
        lines.append((f"Shipper: {rng.choice(COMPANIES_EN)}", en_font, 10.5))
        lines.append((f"Consignee: {rng.choice(COMPANIES_EN)}", en_font, 10.5))
        lines.append((f"Port of Loading: {rng.choice(PORTS)}    Port of Discharge: {rng.choice(PORTS)}", en_font, 10.5))
        lines.append((f"Vessel/Voyage: EVER BRIGHT {rng.randint(100, 999)}W    B/L No. {rand_bl(rng)}", en_font, 10.5))
        lines.append(("Container / Seal | Packages | Gross Weight | Measurement", "cour", 9))
        for _ in range(3):
            lines.append((f"{rand_container(rng)} / {rng.randint(100000, 999999)} | {rng.randint(1, 1200)} CTNS | {rng.randint(100, 24000)}.{rng.randint(0, 99):02d} KGS | {rng.randint(1, 67)}.{rng.randint(0, 99):02d} CBM", "cour", 9))
        lines.append((f"Description of Goods: {rng.choice(GOODS)}", en_font, 10.5))
        lines.append((f"Total Amount: {rand_amount(rng)}    Terms: {rng.choice(['FOB SHANGHAI', 'CIF ROTTERDAM', 'EXW NINGBO'])}", en_font, 10.5))
        for s in rng.sample(EN_SENTENCES, 7):
            lines.append((s, en_font, 9))
        lines.append(("Authorized Signature:", en_font, 10.5))
    else:  # mixed
        lines.append(("海运提单 BILL OF LADING", zh_font, 18))
        lines.append((f"提单号 B/L No.: {rand_bl(rng)}    签发日期 Date of Issue: {rand_date(rng, False)}", zh_font, 10.5))
        lines.append((f"托运人 Shipper: {rng.choice(COMPANIES_EN)}", zh_font, 10.5))
        lines.append((f"收货人 Consignee: {rng.choice(COMPANIES_ZH)}", zh_font, 10.5))
        lines.append((f"起运港 Port of Loading: {rng.choice(PORTS)}    目的港 Port of Discharge: {rng.choice(PORTS)}", zh_font, 10.5))
        lines.append((f"船名航次 Vessel/Voy.: COSCO HARMONY {rng.randint(10, 99)}E", zh_font, 10.5))
        lines.append(("箱号 Container | 封号 Seal | 件数 Pkgs | 毛重 G.W. | 体积 Meas.", zh_font, 9))
        for _ in range(2):
            lines.append((f"{rand_container(rng)} | SL{rng.randint(100000, 999999)} | {rng.randint(1, 1200)} 件 | {rng.randint(100, 24000)} KGS | {rng.randint(1, 67)}.{rng.randint(0, 9)} CBM", zh_font, 9))
        lines.append((f"货物描述 Description: {rng.choice(GOODS)}（{rng.choice(['不锈钢管', '园艺家具', 'LED 灯具', '瓷砖', '汽车配件'])}）", zh_font, 10.5))
        lines.append((f"运费 Freight: {rng.choice(['PREPAID 预付', 'COLLECT 到付'])}    金额 Amount: {rand_amount(rng)}", zh_font, 10.5))
        for s in rng.sample(ZH_SENTENCES, 3):
            lines.append((s, zh_font, 9))
        for s in rng.sample(EN_SENTENCES, 3):
            lines.append((s, en_font, 9))
        lines.append(("承运人签章 For the Carrier:", zh_font, 10.5))
    return lines


def wrap(text: str, font: ImageFont.FreeTypeFont, max_w: int, draw: ImageDraw.ImageDraw) -> list[str]:
    """按像素宽度折行：中文按字切，英文按词切。"""
    if draw.textlength(text, font=font) <= max_w:
        return [text]
    out, cur = [], ""
    tokens = text.split(" ") if " " in text and not any("\u4e00" <= c <= "\u9fff" for c in text) else list(text)
    sep = " " if tokens and len(tokens[0]) > 1 else ""
    for tok in tokens:
        cand = tok if not cur else cur + sep + tok
        if draw.textlength(cand, font=font) <= max_w:
            cur = cand
        else:
            if cur:
                out.append(cur)
            cur = tok
    if cur:
        out.append(cur)
    return out


def rotate_point(x: float, y: float, cx: float, cy: float, deg: float) -> list[float]:
    """PIL 的 rotate(angle) 为逆时针；像素坐标 y 向下，所以用对应的旋转矩阵。"""
    t = math.radians(deg)
    dx, dy = x - cx, y - cy
    return [cx + dx * math.cos(t) + dy * math.sin(t), cy - dx * math.sin(t) + dy * math.cos(t)]


def render_page(rng: random.Random, lang: str, variant: str, fonts: dict) -> tuple[Image.Image, list[dict]]:
    img = Image.new("RGB", (PAGE_W, PAGE_H), (255, 255, 255))
    draw = ImageDraw.Draw(img)
    margin_x, y = 200, 220
    max_w = PAGE_W - 2 * margin_x
    annotations: list[dict] = []
    for text, font_key, size in build_lines(rng, lang):
        font = fonts[(font_key, size)]
        pad = 6
        if "|" in text:
            # 表格行：以 | 分隔的单元格放到等距列位上，每个单元格单独标注（与标注员按单元格标注的习惯一致）。
            cells = [c.strip() for c in text.split("|")]
            col_w = max_w // len(cells)
            for ci, cell in enumerate(cells):
                if not cell:
                    continue
                cx = margin_x + ci * col_w
                bbox = draw.textbbox((cx, y), cell, font=font)
                draw.text((cx, y), cell, font=font, fill=(0, 0, 0))
                x0, y0, x1, y1 = bbox[0] - pad, bbox[1] - pad, bbox[2] + pad, bbox[3] + pad
                annotations.append({"transcription": cell, "points": [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]})
            y += int(size / 72 * DPI * 1.7)
            continue
        for piece in wrap(text, font, max_w, draw):
            x = margin_x if size < 16 else (PAGE_W - draw.textlength(piece, font=font)) // 2
            # 以 4 个空格隔开的字段：留出约 1.5 cm 的空当并各自标注，模拟表单里相邻的两个字段。
            segments = piece.split("    ")
            cursor = x
            for si, seg in enumerate(segments):
                if seg:
                    bbox = draw.textbbox((cursor, y), seg, font=font)
                    draw.text((cursor, y), seg, font=font, fill=(0, 0, 0))
                    x0, y0, x1, y1 = bbox[0] - pad, bbox[1] - pad, bbox[2] + pad, bbox[3] + pad
                    annotations.append({"transcription": seg, "points": [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]})
                cursor += draw.textlength(seg, font=font)
                if si + 1 < len(segments):
                    cursor += 180
            y += int(size / 72 * DPI * 1.7)
        if size >= 16:
            y += 40
    # 伪印章：红色圆圈 + 文字，区域标为 seal（不转写）。
    if rng.random() < 0.6:
        cx, cy, r = PAGE_W - 520, y + 150, 170
        draw.ellipse((cx - r, cy - r, cx + r, cy + r), outline=(200, 30, 30), width=8)
        seal_font = fonts[("simsun", 12)]
        seal_text = rng.choice(["合同专用章", "业务专用章", "FOR THE CARRIER"])
        tw = draw.textlength(seal_text, font=seal_font)
        draw.text((cx - tw / 2, cy - 30), seal_text, font=seal_font, fill=(200, 30, 30))
        annotations.append({"transcription": "", "points": [[cx - r, cy - r], [cx + r, cy - r], [cx + r, cy + r], [cx - r, cy + r]], "key_cls": "seal"})

    angle = 0.0
    if "rot" in variant:
        angle = rng.choice([-1.5, -1.0, 1.0, 1.5])
        img = img.rotate(angle, resample=Image.BICUBIC, fillcolor=(255, 255, 255))
        cx, cy = PAGE_W / 2, PAGE_H / 2
        for a in annotations:
            a["points"] = [[round(v, 1) for v in rotate_point(px, py, cx, cy, angle)] for px, py in a["points"]]
    if "noise" in variant:
        img = img.filter(ImageFilter.GaussianBlur(0.6))
        noise = Image.effect_noise((PAGE_W, PAGE_H), 18).convert("L")
        gray = img.convert("L")
        gray = Image.blend(gray, noise, 0.18)
        img = gray.convert("RGB")
    return img, annotations


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(Path(__file__).parent / "out"))
    ap.add_argument("--pages-per-lang", type=int, default=6)
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()
    rng = random.Random(args.seed)
    out = Path(args.out)
    (out / "images").mkdir(parents=True, exist_ok=True)

    fonts: dict = {}
    for key, path in FONTS.items():
        for size in (9, 10.5, 12, 18):
            fonts[(key, size)] = ImageFont.truetype(str(path), pt(size))

    variants = ["clean", "noise", "rot", "noiserot"]
    label_lines, langs = [], {}
    for lang in ("zh", "en", "mixed"):
        for i in range(args.pages_per_lang):
            variant = variants[i % len(variants)]
            img, ann = render_page(rng, lang, variant, fonts)
            name = f"{lang}_{i + 1:03d}_{variant}.png"
            img.save(out / "images" / name, compress_level=3)
            label_lines.append(f"images/{name}\t{json.dumps(ann, ensure_ascii=False)}")
            langs[Path(name).stem] = lang
            print(f"{name}: {sum(1 for a in ann if 'key_cls' not in a)} 行")
    (out / "Label.txt").write_text("\n".join(label_lines) + "\n", encoding="utf-8")
    (out / "langs.json").write_text(json.dumps(langs, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"写入 {out}")


if __name__ == "__main__":
    main()
