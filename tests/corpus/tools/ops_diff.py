"""往返前后内容流的操作符级比对（V03）。

像素比对看不出的丢失——CMYK/灰度被换算成等价 RGB、专色变成 RGB、叠印参数丢失——在这里按"绘制事件"比较：
每个路径绘制、文字输出、`Do`、`sh`、内联图像是一个事件，记录它生效时的
- 颜色：填充/描边的颜色空间（g/G、rg/RG、k/K 推出的设备空间；cs/CS 引用的 ICCBased、Separation、DeviceN、
  Indexed、Lab、Cal*、Pattern 及其底色空间、图案类型）和分量值；
- 文字状态：Tc、Tw、Tz、TL、Ts、Tr，以及字号 × 水平缩放 × Tm × CTM 的线性部分（用来判断 Tz 是否被等价折算进 Tm）；
- 图形状态参数（表里“ExtGState”一栏）：ca、CA、BM、OP、op、OPM、TK；SMask（类型、蒙版组的内容、`gs` 生效时的
  CTM——软蒙版按那一刻的坐标系摆放）；ExtGState 字典里其余的条目（SA、AIS、SM、TR/TR2、BG/BG2、UCR/UCR2、HT 和
  不认识的键，按值比较）；平滑度（`i` 或 FL）、渲染意图（`ri` 或 RI）；描边的事件和 Form 另外比较线宽、线端、
  接头、斜接限制、虚线（`w`/`J`/`j`/`M`/`d` 或 LW/LC/LJ/ML/D）。
原件与保存结果的事件序列按种类（文字按字节串）对齐后逐项比较，输出每页的差异摘要。

用法：
  python tests/corpus/tools/ops_diff.py --roundtrip tests/out/x.json --save-dir tests/out/x-saved [--json out.json]
  python tests/corpus/tools/ops_diff.py original.pdf saved.pdf

只作测试工具（pikepdf：MPL-2.0，见 tests/requirements-test.txt），不进安装包。
"""

from __future__ import annotations

import argparse
import copy
import difflib
import hashlib
import json
import sys
from collections import Counter
from dataclasses import dataclass, field
from decimal import Decimal
from pathlib import Path
from typing import Any, Iterable

import pikepdf

Matrix = tuple[float, float, float, float, float, float]
IDENTITY: Matrix = (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)

DEVICE_SPACES = {"DeviceGray": 1, "DeviceRGB": 3, "DeviceCMYK": 4}
INLINE_CS_ABBREVS = {"G": "DeviceGray", "RGB": "DeviceRGB", "CMYK": "DeviceCMYK", "I": "Indexed"}
PATH_PAINT = {
    "S": "stroke",
    "s": "stroke",
    "f": "fill",
    "F": "fill",
    "f*": "fill",
    "B": "fillstroke",
    "B*": "fillstroke",
    "b": "fillstroke",
    "b*": "fillstroke",
}
FILL_TEXT_MODES = {0, 2, 4, 6}
STROKE_TEXT_MODES = {1, 2, 5, 6}
CLIP_TEXT_MODES = {4, 5, 6, 7}
VALUE_TOLERANCE = 2e-3
TEXT_TOLERANCE = 1e-3
# ExtGState 字典里单独跟踪的键；其余的键（除了 Type）按值记进 State.gs_other
GS_OWN_KEYS = frozenset(
    "/" + key
    for key in "Type ca CA BM SMask OP op OPM TK FL RI LW LC LJ ML D Font".split()
)
# 比对项目变了就加一：不同版本量出来的差异数不能放在一起比（v03_measure.py 会检查）
#   1  颜色、文字状态、ca/CA/BM/SMask 的类型/OP/op/OPM、剪裁
#   2  加上 TK、软蒙版的内容和 CTM、ExtGState 的其余条目、平滑度、渲染意图、线型参数
VERSION = 2


def mul(m1: Matrix, m2: Matrix) -> Matrix:
    """m1 × m2（行向量约定，与 PDF 规范相同）。"""
    a1, b1, c1, d1, e1, f1 = m1
    a2, b2, c2, d2, e2, f2 = m2
    return (
        a1 * a2 + b1 * c2,
        a1 * b2 + b1 * d2,
        c1 * a2 + d1 * c2,
        c1 * b2 + d1 * d2,
        e1 * a2 + f1 * c2 + e2,
        e1 * b2 + f1 * d2 + f2,
    )


def num(obj: Any) -> float:
    if isinstance(obj, (int, float, Decimal)):
        return float(obj)
    try:
        return float(obj)
    except (TypeError, ValueError):
        return 0.0


def name_of(obj: Any) -> str:
    text = str(obj)
    return text[1:] if text.startswith("/") else text


def fmt(value: float) -> str:
    return f"{value:.4g}"


def short_hash(data: bytes) -> str:
    return hashlib.sha1(data).hexdigest()[:8]


@dataclass
class Color:
    space: str = "DeviceGray"
    values: tuple[float, ...] = (0.0,)
    pattern: str = ""
    op: str = "init"

    def describe(self) -> str:
        if self.space.startswith("Pattern"):
            vals = ",".join(fmt(v) for v in self.values)
            return f"{self.space}[{self.pattern}]" + (f"({vals})" if vals else "")
        return f"{self.space}({','.join(fmt(v) for v in self.values)})"


@dataclass
class State:
    ctm: Matrix = IDENTITY
    fill: Color = field(default_factory=Color)
    stroke: Color = field(default_factory=Color)
    tc: float = 0.0
    tw: float = 0.0
    tz: float = 100.0
    tl: float = 0.0
    ts: float = 0.0
    tr: int = 0
    font: str = ""
    font_size: float = 0.0
    ca: float = 1.0
    CA: float = 1.0
    bm: str = "Normal"
    smask: str = "None"
    # `gs` 设置软蒙版那一刻的 CTM；没有软蒙版时是 None
    smask_ctm: Matrix | None = None
    OP: bool = False
    op: bool = False
    OPM: int = 0
    tk: bool = True
    flatness: float = 1.0
    intent: str = "RelativeColorimetric"
    # ExtGState 字典里上面没有单列的条目：键 → 值的描述
    gs_other: dict[str, str] = field(default_factory=dict)
    line_width: float = 1.0
    line_cap: int = 0
    line_join: int = 0
    miter_limit: float = 10.0
    dash: tuple[tuple[float, ...], float] = ((), 0.0)
    # Current clip: path entries (rule, path ops) and text-clip groups.
    clip_path: tuple = ()
    clip_texts: tuple = ()


@dataclass
class Event:
    kind: str
    key: str
    fill: Color | None
    stroke: Color | None
    text: dict[str, Any] | None
    gs: dict[str, Any]
    clip: dict[str, Any] = field(default_factory=dict)
    # 线型参数；只有描边的事件和 Form（里面的内容继承这些参数）才记
    line: dict[str, Any] | None = None


class Resources:
    """页面资源（沿 /Parent 继承），按类型取条目。"""

    def __init__(self, page: pikepdf.Dictionary) -> None:
        node: Any = page
        res: Any = None
        for _ in range(64):
            if node is None:
                break
            if "/Resources" in node:
                res = node.get("/Resources")
                break
            node = node.get("/Parent")
        self.res = res if isinstance(res, pikepdf.Dictionary) else pikepdf.Dictionary()

    def get(self, kind: str, name: str) -> Any:
        sub = self.res.get("/" + kind)
        if not isinstance(sub, pikepdf.Dictionary):
            return None
        return sub.get("/" + name)


def describe_cs(obj: Any, res: Resources, depth: int = 0) -> str:
    if depth > 8 or obj is None:
        return "Unknown"
    if isinstance(obj, pikepdf.Name):
        name = name_of(obj)
        name = INLINE_CS_ABBREVS.get(name, name)
        if name in DEVICE_SPACES or name == "Pattern":
            return name
        if name in ("CalRGB", "CalGray", "Lab"):
            return name
        found = res.get("ColorSpace", name)
        return describe_cs(found, res, depth + 1) if found is not None else f"Missing({name})"
    if isinstance(obj, pikepdf.Array) and len(obj) > 0:
        family = name_of(obj[0])
        family = INLINE_CS_ABBREVS.get(family, family)
        if family in DEVICE_SPACES:
            return family
        if family == "ICCBased" and len(obj) > 1:
            stream = obj[1]
            n = int(num(stream.get("/N"))) if isinstance(stream, pikepdf.Stream) else 0
            try:
                digest = short_hash(stream.read_bytes())
            except (pikepdf.PdfError, AttributeError):
                digest = "?"
            return f"ICCBased(N={n},{digest})"
        if family == "Separation" and len(obj) > 2:
            return f"Separation({name_of(obj[1])},alt={describe_cs(obj[2], res, depth + 1)})"
        if family == "DeviceN" and len(obj) > 2:
            names = "+".join(name_of(n) for n in obj[1]) if isinstance(obj[1], pikepdf.Array) else "?"
            return f"DeviceN({names},alt={describe_cs(obj[2], res, depth + 1)})"
        if family == "Indexed" and len(obj) > 2:
            return f"Indexed({describe_cs(obj[1], res, depth + 1)},hival={int(num(obj[2]))})"
        if family == "Pattern":
            if len(obj) > 1:
                return f"Pattern({describe_cs(obj[1], res, depth + 1)})"
            return "Pattern"
        if family in ("CalRGB", "CalGray", "Lab"):
            return family
        return f"Other({family})"
    return "Unknown"


def describe_pattern(name: str, res: Resources) -> str:
    pat = res.get("Pattern", name)
    if pat is None:
        return f"Missing({name})"
    ptype = int(num(pat.get("/PatternType")))
    if ptype == 1:
        return f"tiling/PaintType={int(num(pat.get('/PaintType')))}"
    if ptype == 2:
        shading = pat.get("/Shading")
        stype = int(num(shading.get("/ShadingType"))) if shading is not None else 0
        return f"shading/ShadingType={stype}"
    return f"PatternType={ptype}"


def describe_value(obj: Any, depth: int = 0) -> str:
    """一个 PDF 对象的值的描述，不含对象号（保存之后对象号会变）。

    流按解码后的内容取摘要，字符串也只取摘要（描述会写进报告，不能带文档内容）。
    """
    if depth > 6:
        return "…"
    if isinstance(obj, pikepdf.Stream):
        try:
            digest = short_hash(obj.read_bytes())
        except pikepdf.PdfError:
            digest = "?"
        keys = {k: v for k, v in obj.stream_dict.items() if k not in ("/Length", "/Filter", "/DecodeParms")}
        inner = ",".join(f"{name_of(k)}={describe_value(v, depth + 1)}" for k, v in sorted(keys.items()))
        return f"stream({inner};{digest})"
    if isinstance(obj, pikepdf.Dictionary):
        inner = ",".join(f"{name_of(k)}={describe_value(v, depth + 1)}" for k, v in sorted(obj.items()))
        return f"<<{inner}>>"
    if isinstance(obj, pikepdf.Array):
        return "[" + ",".join(describe_value(v, depth + 1) for v in obj) + "]"
    if isinstance(obj, pikepdf.Name):
        return "/" + name_of(obj)
    if isinstance(obj, bool):
        return "true" if obj else "false"
    if isinstance(obj, (int, float, Decimal)):
        return fmt(float(obj))
    if isinstance(obj, pikepdf.String):
        return "(" + short_hash(bytes(obj)) + ")"
    if obj is None:
        return "null"
    return type(obj).__name__


def parse_dash(array: Any, phase: Any) -> tuple[tuple[float, ...], float]:
    """虚线：`[数组] 相位`。数组为空就是实线，相位没有意义，记成 0。"""
    if not isinstance(array, pikepdf.Array):
        return ((), 0.0)
    values = tuple(num(v) for v in array)
    return (values, num(phase)) if values else ((), 0.0)


def describe_smask(obj: Any) -> str:
    """软蒙版：类型、蒙版组的内容，以及有没有底色和传递函数。"""
    if obj is None or isinstance(obj, pikepdf.Name):
        return "None"
    if isinstance(obj, pikepdf.Dictionary):
        parts = [name_of(obj.get("/S", "?"))]
        group = obj.get("/G")
        if isinstance(group, pikepdf.Stream):
            try:
                parts.append(short_hash(group.read_bytes()))
            except pikepdf.PdfError:
                parts.append("?")
        else:
            parts.append("no-group")
        if "/BC" in obj:
            parts.append("BC=" + describe_value(obj.get("/BC")))
        if "/TR" in obj:
            parts.append("TR=" + short_hash(describe_value(obj.get("/TR")).encode()))
        return ":".join(parts)
    return "Unknown"


def describe_font(obj: Any) -> str:
    if not isinstance(obj, pikepdf.Dictionary):
        return "Missing"
    return f"{name_of(obj.get('/Subtype', '?'))}:{name_of(obj.get('/BaseFont', '-'))}"


def set_color_space(color: Color, space: str) -> None:
    color.space = space
    color.pattern = ""
    color.op = "cs"
    if space in DEVICE_SPACES:
        color.values = (0.0,) * DEVICE_SPACES[space]
    elif space.startswith("Separation") or space.startswith("Indexed"):
        color.values = (1.0,) if space.startswith("Separation") else (0.0,)
    else:
        color.values = ()


def text_bytes(op: str, operands: list[Any]) -> bytes:
    if op == "TJ" and operands and isinstance(operands[0], pikepdf.Array):
        return b"".join(bytes(item) for item in operands[0] if isinstance(item, pikepdf.String))
    if op in ("Tj", "'") and operands and isinstance(operands[0], pikepdf.String):
        return bytes(operands[0])
    if op == '"' and len(operands) == 3 and isinstance(operands[2], pikepdf.String):
        return bytes(operands[2])
    return b""


def gs_snapshot(st: State) -> dict[str, Any]:
    return {
        "ca": round(st.ca, 4),
        "CA": round(st.CA, 4),
        "BM": st.bm,
        "SMask": st.smask,
        "SMask matrix": st.smask_ctm,
        "OP": st.OP,
        "op": st.op,
        "OPM": st.OPM,
        "TK": st.tk,
        "FL": st.flatness,
        "RI": st.intent,
        "other": dict(st.gs_other),
    }


def line_snapshot(st: State) -> dict[str, Any]:
    return {
        "LW": st.line_width,
        "LC": st.line_cap,
        "LJ": st.line_join,
        "ML": st.miter_limit,
        "D": [list(st.dash[0]), st.dash[1]],
    }


def _to_lists(value: Any) -> Any:
    if isinstance(value, (list, tuple)):
        return [_to_lists(v) for v in value]
    return value


def clip_snapshot(st: State) -> dict[str, Any]:
    # The clip in force when the event is drawn: path clips (rule + path
    # ops) and text-clip groups (font, size, Tm, bytes, Tr per text).
    return {
        "path": _to_lists(st.clip_path),
        "text": _to_lists(st.clip_texts),
    }


def _clip_values_equal(a: Any, b: Any) -> bool:
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return abs(float(a) - float(b)) <= VALUE_TOLERANCE
    if isinstance(a, str) and isinstance(b, str):
        return a == b
    if isinstance(a, (list, tuple)) and isinstance(b, (list, tuple)):
        return len(a) == len(b) and all(
            _clip_values_equal(x, y) for x, y in zip(a, b, strict=True)
        )
    return a == b


class Interpreter:
    """按顺序执行一页的内容流，只跟踪比对需要的状态，产出绘制事件。"""

    def __init__(self, page: pikepdf.Page) -> None:
        self.page = page
        self.res = Resources(page.obj)
        self.st = State()
        self.stack: list[State] = []
        self.tm: Matrix = IDENTITY
        self.events: list[Event] = []
        self.path: list[Any] = []
        self.bt_texts: list[Any] = []

    def run(self) -> list[Event]:
        for item in pikepdf.parse_content_stream(self.page):
            try:
                if isinstance(item, pikepdf.ContentStreamInlineImage):
                    self.inline_image(item.iimage)
                    continue
                op = str(item.operator)
                operands = list(item.operands)
                if not _apply(op, operands, self.st, self.res):
                    self.step(op, operands)
            except (pikepdf.PdfError, TypeError, ValueError, IndexError, AttributeError, KeyError):
                continue
        return self.events

    def add(self, kind: str, key: str, uses_fill: bool, uses_stroke: bool, text: dict[str, Any] | None) -> None:
        st = self.st
        self.events.append(
            Event(
                kind=kind,
                key=key,
                fill=copy.copy(st.fill) if uses_fill else None,
                stroke=copy.copy(st.stroke) if uses_stroke else None,
                text=text,
                gs=gs_snapshot(st),
                clip=clip_snapshot(st),
                line=line_snapshot(st) if uses_stroke else None,
            )
        )

    def inline_image(self, image: Any) -> None:
        mask = bool(getattr(image, "image_mask", False))
        space = "-" if mask else describe_cs(image.obj.get("/CS", image.obj.get("/ColorSpace")), self.res)
        self.add("inline-image", f"inline-image:{mask}", mask, False, {"cs": space})

    def step(self, op: str, operands: list[Any]) -> None:
        st = self.st
        if op == "q":
            self.stack.append(copy.deepcopy(st))
        elif op == "Q":
            if self.stack:
                self.st = self.stack.pop()
        elif op == "BT":
            self.tm = IDENTITY
            self.bt_texts = []
        elif op == "ET":
            if self.st.tr in CLIP_TEXT_MODES and self.bt_texts:
                self.st.clip_texts = self.st.clip_texts + (tuple(self.bt_texts),)
            self.bt_texts = []
        elif op == "Td" and len(operands) == 2:
            self.tm = mul((1.0, 0.0, 0.0, 1.0, num(operands[0]), num(operands[1])), self.tm)
        elif op == "TD" and len(operands) == 2:
            self.tm = mul((1.0, 0.0, 0.0, 1.0, num(operands[0]), num(operands[1])), self.tm)
        elif op == "T*":
            self.tm = mul((1.0, 0.0, 0.0, 1.0, 0.0, -self.st.tl), self.tm)
        elif op == "Tm" and len(operands) == 6:
            a, b, c, d, e, f = (num(v) for v in operands)
            self.tm = (a, b, c, d, e, f)
        elif op == "m" and len(operands) == 2:
            self.path = [("m", round(num(operands[0]), 4), round(num(operands[1]), 4))]
        elif op == "l" and len(operands) == 2:
            self.path.append(("l", round(num(operands[0]), 4), round(num(operands[1]), 4)))
        elif op == "c" and len(operands) == 6:
            self.path.append(("c",) + tuple(round(num(v), 4) for v in operands))
        elif op == "v" and len(operands) == 4:
            self.path.append(("v",) + tuple(round(num(v), 4) for v in operands))
        elif op == "y" and len(operands) == 4:
            self.path.append(("y",) + tuple(round(num(v), 4) for v in operands))
        elif op == "h":
            self.path.append(("h",))
        elif op == "re" and len(operands) == 4:
            self.path.append(("re",) + tuple(round(num(v), 4) for v in operands))
        elif op in ("W", "W*"):
            rule = "nonzero" if op == "W" else "evenodd"
            self.st.clip_path = self.st.clip_path + ((rule, tuple(self.path)),)
        elif op == "n":
            self.path = []
        elif op in PATH_PAINT:
            kind = PATH_PAINT[op]
            self.add(kind, kind, kind != "stroke", kind != "fill", None)
            self.path = []
        elif op in ("Tj", "TJ", "'", '"'):
            self.show_text(op, operands)
        elif op == "Do" and operands:
            xobj = self.res.get("XObject", name_of(operands[0]))
            subtype = name_of(xobj.get("/Subtype")) if isinstance(xobj, pikepdf.Stream) else "Missing"
            if subtype == "Image":
                mask = bool(xobj.get("/ImageMask", False))
                space = "-" if mask else describe_cs(xobj.get("/ColorSpace"), self.res)
                self.add("image", f"image:{mask}", mask, False, {"cs": space})
            elif subtype == "Form":
                self.add("form", "form", True, True, None)
            elif subtype != "Missing":
                self.add("xobject", f"xobject:{subtype}", False, False, None)
        elif op == "sh" and operands:
            shading = self.res.get("Shading", name_of(operands[0]))
            stype = int(num(shading.get("/ShadingType"))) if shading is not None else 0
            space = describe_cs(shading.get("/ColorSpace"), self.res) if shading is not None else "Missing"
            self.add("shading", f"shading:{stype}", False, False, {"cs": space})

    def show_text(self, op: str, operands: list[Any]) -> None:
        st = self.st
        if op == '"' and len(operands) == 3:
            st.tw = num(operands[0])
            st.tc = num(operands[1])
        data = text_bytes(op, operands)
        if not data:
            return
        size, tm, ctm = st.font_size, self.tm, st.ctm
        lin = mul((size * st.tz / 100.0, 0.0, 0.0, size, 0.0, 0.0), (tm[0], tm[1], tm[2], tm[3], 0.0, 0.0))
        lin = mul(lin, (ctm[0], ctm[1], ctm[2], ctm[3], 0.0, 0.0))
        text = {
            "Tc": st.tc,
            "Tw": st.tw,
            "Tz": st.tz,
            "TL": st.tl,
            "Ts": st.ts,
            "Tr": st.tr,
            "font": st.font,
            "lin": lin[:4],
        }
        if st.tr in CLIP_TEXT_MODES:
            self.bt_texts.append(
                (st.font, round(st.font_size, 4), tuple(round(v, 4) for v in self.tm), data.hex(), st.tr)
            )
        self.add("text", "text:" + data.hex(), st.tr in FILL_TEXT_MODES, st.tr in STROKE_TEXT_MODES, text)


def collect_events(page: pikepdf.Page) -> list[Event]:
    return Interpreter(page).run()


def _apply(op: str, operands: list[Any], st: State, res: Resources) -> bool:
    """更新图形状态；返回 True 表示已处理（不是绘制事件、也不是 q/Q/BT/Tm）。"""
    if op == "cm" and len(operands) == 6:
        st.ctm = mul(tuple(num(v) for v in operands), st.ctm)  # type: ignore[arg-type]
        return True
    if op in ("g", "G", "rg", "RG", "k", "K"):
        color = st.fill if op.islower() else st.stroke
        color.space = {"g": "DeviceGray", "rg": "DeviceRGB", "k": "DeviceCMYK"}[op.lower()]
        color.values = tuple(min(max(num(v), 0.0), 1.0) for v in operands)
        color.pattern = ""
        color.op = op
        return True
    if op in ("cs", "CS") and operands:
        color = st.fill if op == "cs" else st.stroke
        set_color_space(color, describe_cs(operands[0], res))
        color.op = op
        return True
    if op in ("sc", "SC", "scn", "SCN"):
        color = st.fill if op.islower() else st.stroke
        values = tuple(num(v) for v in operands if not isinstance(v, pikepdf.Name))
        names = [v for v in operands if isinstance(v, pikepdf.Name)]
        if color.space in DEVICE_SPACES:
            values = tuple(min(max(v, 0.0), 1.0) for v in values)
        color.values = values
        color.pattern = describe_pattern(name_of(names[-1]), res) if names else ""
        if names and not color.space.startswith("Pattern"):
            # PDFium 对没有先 `/Pattern cs` 的 `/P scn` 也按图案填充处理
            color.space = "Pattern"
        color.op = op
        return True
    if op == "gs" and operands:
        gsd = res.get("ExtGState", name_of(operands[0]))
        if isinstance(gsd, pikepdf.Dictionary):
            if "/ca" in gsd:
                st.ca = num(gsd.get("/ca"))
            if "/CA" in gsd:
                st.CA = num(gsd.get("/CA"))
            if "/BM" in gsd:
                bm = gsd.get("/BM")
                st.bm = name_of(bm[0] if isinstance(bm, pikepdf.Array) and len(bm) else bm)
            if "/SMask" in gsd:
                st.smask = describe_smask(gsd.get("/SMask"))
                st.smask_ctm = st.ctm if st.smask != "None" else None
            if "/OP" in gsd:
                st.OP = bool(gsd.get("/OP"))
                if "/op" not in gsd:
                    st.op = st.OP
            if "/op" in gsd:
                st.op = bool(gsd.get("/op"))
            if "/OPM" in gsd:
                st.OPM = int(num(gsd.get("/OPM")))
            if "/TK" in gsd:
                st.tk = bool(gsd.get("/TK"))
            if "/FL" in gsd:
                st.flatness = num(gsd.get("/FL"))
            if "/RI" in gsd:
                st.intent = name_of(gsd.get("/RI"))
            if "/LW" in gsd:
                st.line_width = num(gsd.get("/LW"))
            if "/LC" in gsd:
                st.line_cap = int(num(gsd.get("/LC")))
            if "/LJ" in gsd:
                st.line_join = int(num(gsd.get("/LJ")))
            if "/ML" in gsd:
                st.miter_limit = num(gsd.get("/ML"))
            if "/D" in gsd:
                dash = gsd.get("/D")
                if isinstance(dash, pikepdf.Array) and len(dash) == 2:
                    st.dash = parse_dash(dash[0], dash[1])
            if "/Font" in gsd:
                font = gsd.get("/Font")
                if isinstance(font, pikepdf.Array) and len(font) == 2:
                    st.font = describe_font(font[0])
                    st.font_size = num(font[1])
            for key, value in gsd.items():
                if key not in GS_OWN_KEYS:
                    st.gs_other[name_of(key)] = describe_value(value)
        return True
    if op == "w" and operands:
        st.line_width = num(operands[0])
        return True
    if op == "J" and operands:
        st.line_cap = int(num(operands[0]))
        return True
    if op == "j" and operands:
        st.line_join = int(num(operands[0]))
        return True
    if op == "M" and operands:
        st.miter_limit = num(operands[0])
        return True
    if op == "d" and len(operands) == 2:
        st.dash = parse_dash(operands[0], operands[1])
        return True
    if op == "i" and operands:
        st.flatness = num(operands[0])
        return True
    if op == "ri" and operands:
        st.intent = name_of(operands[0])
        return True
    if op == "Tc" and operands:
        st.tc = num(operands[0])
        return True
    if op == "Tw" and operands:
        st.tw = num(operands[0])
        return True
    if op == "Tz" and operands:
        st.tz = num(operands[0])
        return True
    if op == "TL" and operands:
        st.tl = num(operands[0])
        return True
    if op == "TD" and len(operands) == 2:
        st.tl = -num(operands[1])
        return False
    if op == "Ts" and operands:
        st.ts = num(operands[0])
        return True
    if op == "Tr" and operands:
        st.tr = int(num(operands[0]))
        return True
    if op == "Tf" and operands:
        # 与 PDFium 一样从末尾取操作数：缺字体名时字号照样生效
        font = res.get("Font", name_of(operands[-2])) if len(operands) >= 2 else None
        st.font = describe_font(font)
        st.font_size = num(operands[-1])
        return True
    return False


def colors_differ(a: Color, b: Color) -> bool:
    if a.space != b.space or a.pattern != b.pattern:
        return True
    if len(a.values) != len(b.values):
        return True
    return any(abs(x - y) > VALUE_TOLERANCE for x, y in zip(a.values, b.values, strict=True))


def close(a: float, b: float, tol: float = TEXT_TOLERANCE) -> bool:
    return abs(a - b) <= tol * max(1.0, abs(a), abs(b))


@dataclass
class PageDiff:
    events_a: int = 0
    events_b: int = 0
    missing: Counter[str] = field(default_factory=Counter)
    extra: Counter[str] = field(default_factory=Counter)
    color: Counter[str] = field(default_factory=Counter)
    text: Counter[str] = field(default_factory=Counter)
    gs: Counter[str] = field(default_factory=Counter)
    clip: Counter[str] = field(default_factory=Counter)
    folded: Counter[str] = field(default_factory=Counter)

    def counts(self) -> dict[str, int]:
        return {
            "color": sum(self.color.values()),
            "text": sum(self.text.values()),
            "gs": sum(self.gs.values()),
            "clip": sum(self.clip.values()),
            "events": sum(self.missing.values()) + sum(self.extra.values()),
        }

    def total(self) -> int:
        return sum(self.counts().values())

    def to_json(self) -> dict[str, Any]:
        return {
            "events_a": self.events_a,
            "events_b": self.events_b,
            "counts": self.counts(),
            "missing": dict(self.missing),
            "extra": dict(self.extra),
            "color": dict(self.color),
            "text": dict(self.text),
            "gs": dict(self.gs),
            "clip": dict(self.clip),
            "folded": dict(self.folded),
        }

    def summary(self) -> str:
        parts: list[str] = []
        for label, counter in (
            ("missing", self.missing),
            ("extra", self.extra),
            ("color", self.color),
            ("text", self.text),
            ("gs", self.gs),
            ("clip", self.clip),
        ):
            if counter:
                top = ", ".join(f"{k}×{v}" for k, v in counter.most_common(3))
                more = f" (+{len(counter) - 3})" if len(counter) > 3 else ""
                parts.append(f"{label}: {top}{more}")
        return "; ".join(parts)


def compare_events(a: list[Event], b: list[Event]) -> PageDiff:
    diff = PageDiff(events_a=len(a), events_b=len(b))
    matcher = difflib.SequenceMatcher(None, [e.key for e in a], [e.key for e in b], autojunk=False)
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag == "equal":
            for ea, eb in zip(a[i1:i2], b[j1:j2], strict=True):
                compare_pair(ea, eb, diff)
            continue
        for ea in a[i1:i2]:
            diff.missing[ea.kind] += 1
        for eb in b[j1:j2]:
            diff.extra[eb.kind] += 1
    return diff


def _brief(text: str | None, limit: int = 32) -> str:
    if text is None:
        return "-"
    return text if len(text) <= limit else text[: limit - 1] + "…"


def _brief_dash(dash: list[Any]) -> str:
    return "[" + " ".join(fmt(v) for v in dash[0]) + "] " + fmt(dash[1])


def compare_pair(ea: Event, eb: Event, diff: PageDiff) -> None:
    for channel, ca, cb in (("fill", ea.fill, eb.fill), ("stroke", ea.stroke, eb.stroke)):
        if ca is None or cb is None:
            continue
        if colors_differ(ca, cb):
            if ca.space != cb.space or ca.pattern != cb.pattern:
                key = f"{channel} {ca.space}{'[' + ca.pattern + ']' if ca.pattern else ''}"
                key += f"→{cb.space}{'[' + cb.pattern + ']' if cb.pattern else ''}"
            else:
                key = f"{channel} {ca.space} value"
            diff.color[key] += 1
    if ea.kind in ("image", "shading", "inline-image") and ea.text and eb.text:
        if ea.text.get("cs") != eb.text.get("cs"):
            diff.color[f"{ea.kind} {ea.text.get('cs')}→{eb.text.get('cs')}"] += 1
    if ea.kind == "text" and ea.text and eb.text:
        ta, tb = ea.text, eb.text
        for param in ("Tc", "Tw"):
            if not close(ta[param], tb[param]):
                diff.text[f"{param} {fmt(ta[param])}→{fmt(tb[param])}"] += 1
        if ta["Tr"] != tb["Tr"]:
            diff.text[f"Tr {ta['Tr']}→{tb['Tr']}"] += 1
        if ta["font"] != tb["font"]:
            if ta["font"] == "Missing":
                diff.folded["font-fallback"] += 1
            else:
                diff.text[f"font {ta['font']}→{tb['font']}"] += 1
        same_lin = all(close(x, y) for x, y in zip(ta["lin"], tb["lin"], strict=True))
        if not same_lin:
            diff.text["matrix"] += 1
        for param in ("Tz", "TL", "Ts"):
            if not close(ta[param], tb[param]):
                # Tz 折算进 Tm（线性部分相同）、Ts/TL 折算进位置：画面等价，单列为"折算"
                if param == "Tz" and not same_lin:
                    continue
                diff.folded[param] += 1
    for key, va in ea.gs.items():
        vb = eb.gs[key]
        if key == "other":
            for name in sorted(set(va) | set(vb)):
                if va.get(name) != vb.get(name):
                    diff.gs[f"{name} {_brief(va.get(name))}→{_brief(vb.get(name))}"] += 1
        elif key == "SMask matrix":
            # 软蒙版本身不一样时上面的 SMask 一项已经记了，这里只看两边都有蒙版时摆放的坐标系
            if va is not None and vb is not None and not all(close(x, y) for x, y in zip(va, vb, strict=True)):
                diff.gs["SMask-matrix"] += 1
        elif key == "FL":
            if not close(va, vb):
                diff.gs[f"FL {fmt(va)}→{fmt(vb)}"] += 1
        elif va != vb:
            diff.gs[f"{key} {va}→{vb}"] += 1
    if ea.line is not None and eb.line is not None:
        la, lb = ea.line, eb.line
        for key in ("LW", "ML"):
            if not close(la[key], lb[key]):
                diff.gs[f"{key} {fmt(la[key])}→{fmt(lb[key])}"] += 1
        for key in ("LC", "LJ"):
            if la[key] != lb[key]:
                diff.gs[f"{key} {la[key]}→{lb[key]}"] += 1
        if not _clip_values_equal(la["D"], lb["D"]):
            diff.gs[f"D {_brief_dash(la['D'])}→{_brief_dash(lb['D'])}"] += 1
    if not _clip_values_equal(ea.clip.get("path", []), eb.clip.get("path", [])):
        diff.clip["path"] += 1
    if not _clip_values_equal(ea.clip.get("text", []), eb.clip.get("text", [])):
        diff.clip["text"] += 1


def page_events(pdf: pikepdf.Pdf, index: int) -> list[Event]:
    return collect_events(pdf.pages[index])


def compare_files(original: Path, saved: Path) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    with pikepdf.open(original) as pa, pikepdf.open(saved) as pb:
        for index in range(min(len(pa.pages), len(pb.pages))):
            entry: dict[str, Any] = {"page": index + 1}
            try:
                diff = compare_events(page_events(pa, index), page_events(pb, index))
            except (pikepdf.PdfError, ValueError, TypeError, KeyError) as e:
                entry["error"] = type(e).__name__
                results.append(entry)
                continue
            entry.update(diff.to_json())
            entry["summary"] = diff.summary()
            results.append(entry)
        if len(pa.pages) != len(pb.pages):
            results.append({"page": 0, "error": f"page count {len(pa.pages)}→{len(pb.pages)}"})
    return results


def sanitize(text: str) -> str:
    return "".join(c if c.isalnum() or c in "-._" else "_" for c in text)


def saved_name(source: str, rel: str) -> str:
    stem = rel[:-4] if rel.endswith(".pdf") else rel
    return f"{sanitize(source)}__{sanitize(stem)}.pdf"


def pixel_status(file_entry: dict[str, Any]) -> dict[int, tuple[str, list[str]]]:
    return {
        int(p["index"]) + 1: (str(p.get("status")), [str(h) for h in p.get("hints") or []])
        for p in file_entry.get("pages") or []
    }


def run_roundtrip(args: argparse.Namespace) -> dict[str, Any]:
    data = json.loads(Path(args.roundtrip).read_text(encoding="utf-8"))
    root = Path(data["options"]["root"])
    save_dir = Path(args.save_dir)
    files: list[dict[str, Any]] = []
    for entry in data["files"]:
        rel = str(entry["path"])
        saved = save_dir / saved_name(str(entry["source"]), rel)
        record: dict[str, Any] = {"path": rel, "source": entry["source"]}
        if not saved.is_file():
            record["skipped"] = "no saved file"
            files.append(record)
            continue
        try:
            pages = compare_files(root / rel, saved)
        except pikepdf.PdfError as e:
            record["error"] = f"open: {type(e).__name__}"
            files.append(record)
            continue
        status = pixel_status(entry)
        for page in pages:
            page["pixel"], page["hints"] = status.get(int(page["page"]), ("?", []))
        record["pages"] = pages
        files.append(record)
    return {"roundtrip": str(args.roundtrip), "save_dir": str(save_dir), "files": files}


def run_pair(args: argparse.Namespace) -> dict[str, Any]:
    original, saved = Path(args.original), Path(args.saved)
    record: dict[str, Any] = {"path": original.name, "source": "-", "pages": compare_files(original, saved)}
    return {"files": [record]}


def aggregate(result: dict[str, Any]) -> dict[str, dict[str, int]]:
    by_source: dict[str, Counter[str]] = {}
    for record in result["files"]:
        counter = by_source.setdefault(str(record["source"]), Counter())
        counter["files"] += 1
        if "pages" not in record:
            counter["skipped"] += 1
            continue
        for page in record["pages"]:
            if "error" in page:
                counter["page_errors"] += 1
                continue
            counter["pages"] += 1
            if page.get("pixel") == "diff":
                counter["pixel_diff"] += 1
            counts = page["counts"]
            if any(counts.values()):
                counter["pages_with_diff"] += 1
                if page.get("pixel") == "pass":
                    counter["pixel_pass_ops_diff"] += 1
            for key, value in counts.items():
                counter[key] += value
                if value:
                    counter[f"pages_{key}"] += 1
    return {k: dict(v) for k, v in sorted(by_source.items())}


def print_report(result: dict[str, Any], max_list: int) -> None:
    totals = aggregate(result)
    print(
        "| 来源 | 文件 | 比对页 | 像素差异页 | 有操作符差异的页 | 其中像素无差异 "
        "| 颜色 | 文字状态 | ExtGState | 剪裁 | 事件缺失/多出 |"
    )
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    grand: Counter[str] = Counter()
    for source, c in totals.items():
        grand.update(c)
        print(_row(source, c))
    if len(totals) > 1:
        print(_row("（合计）", grand))
    lines: list[str] = []
    for record in result["files"]:
        for page in record.get("pages", []):
            label = f"- {record['path']} p{page['page']}"
            if "error" in page:
                lines.append(f"{label}: error {page['error']}")
            elif any(page["counts"].values()) or page.get("pixel") == "diff":
                pixel = page.get("pixel", "-")
                if pixel == "diff" and page.get("hints"):
                    pixel += " " + "/".join(page["hints"])
                lines.append(f"{label} [pixel {pixel}]: {page['summary'] or '操作符无差异'}")
    if lines:
        print(f"\n有差异的页（{len(lines)}）：")
        for line in lines[:max_list]:
            print(line)
        if len(lines) > max_list:
            print(f"- …… 另有 {len(lines) - max_list} 页，见 --json")


def _row(source: str, c: Counter[str] | dict[str, int]) -> str:
    def g(key: str) -> int:
        return int(c.get(key, 0))

    return (
        f"| {source} | {g('files')} | {g('pages')} | {g('pixel_diff')} | {g('pages_with_diff')} | "
        f"{g('pixel_pass_ops_diff')} | "
        f"{g('color')}（{g('pages_color')} 页） | {g('text')}（{g('pages_text')} 页） | "
        f"{g('gs')}（{g('pages_gs')} 页） | {g('clip')}（{g('pages_clip')} 页） | {g('events')}（{g('pages_events')} 页） |"
    )


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument("original", nargs="?", help="原件（与 saved 成对使用）")
    parser.add_argument("saved", nargs="?", help="往返保存结果")
    parser.add_argument("--roundtrip", help="magpdf-cli roundtrip --json 的输出")
    parser.add_argument("--save-dir", help="magpdf-cli roundtrip --save-dir 的目录")
    parser.add_argument("--json", help="写出逐页明细")
    parser.add_argument("--max-list", type=int, default=40, help="标准输出最多列出多少个有差异的页")
    args = parser.parse_args(list(argv) if argv is not None else None)
    if args.roundtrip and args.save_dir:
        result = run_roundtrip(args)
    elif args.original and args.saved:
        result = run_pair(args)
    else:
        parser.error("需要 --roundtrip 与 --save-dir，或 original saved 两个文件")
    result["version"] = VERSION
    result["totals"] = aggregate(result)
    if args.json:
        out = Path(args.json)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(result, ensure_ascii=False, indent=1), encoding="utf-8")
    print_report(result, args.max_list)
    return 0


if __name__ == "__main__":
    sys.exit(main())
