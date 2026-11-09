"""样本定义的公共部分：文档骨架、内容流构造器（顺带记录对象数预期）、样本登记。"""

from __future__ import annotations

from collections.abc import Callable, Sequence
from dataclasses import dataclass, field

from pdfgen import Pdf, Ref, num, nums, pdf_string

PAGE_W, PAGE_H = 300, 200
PRODUCER = "MagPDF synthetic corpus generator (tests/corpus/synthetic/generate.py)"


# ---------------------------------------------------------------------------
# 内容流构造器：每次调用都声明"这段内容会产生几个页面对象"，用来生成 manifest 的 expect。
# 计数规则按 PDFium 的页面对象模型：一次路径绘制 / 一次 Tj|TJ|'|" / 一个 BI…EI / 一次 Do / 一次 sh 各计一个；
# `W n`、纯状态操作符不计。
# ---------------------------------------------------------------------------


class Content:
    def __init__(self) -> None:
        self.parts: list[str] = []
        self.counts = {"text": 0, "path": 0, "image": 0, "shading": 0, "form": 0}

    def raw(self, ops: str) -> Content:
        self.parts.append(ops)
        return self

    def path(self, ops: str, n: int = 1) -> Content:
        self.parts.append(ops)
        self.counts["path"] += n
        return self

    def text(self, ops: str, n: int = 1) -> Content:
        self.parts.append(ops)
        self.counts["text"] += n
        return self

    def image(self, ops: str, n: int = 1) -> Content:
        self.parts.append(ops)
        self.counts["image"] += n
        return self

    def shading(self, ops: str, n: int = 1) -> Content:
        self.parts.append(ops)
        self.counts["shading"] += n
        return self

    def form(self, ops: str, n: int = 1) -> Content:
        self.parts.append(ops)
        self.counts["form"] += n
        return self

    def label(self, s: str, x: float = 10, y: float = PAGE_H - 12, font: str = "F1", size: float = 7) -> Content:
        """页面角落的说明文字（Helvetica，用 /F1）。"""
        return self.text(f"BT /{font} {num(size)} Tf 0 g {nums(x, y)} Td {pdf_string(s)} Tj ET")

    def data(self) -> bytes:
        return "\n".join(self.parts).encode("latin-1")

    def expect_page(self, width: float = PAGE_W, height: float = PAGE_H, rotation: int = 0) -> dict:
        return page_expect(width, height, rotation, **self.counts)


def page_expect(
    width: float = PAGE_W,
    height: float = PAGE_H,
    rotation: int = 0,
    *,
    text: int = 0,
    path: int = 0,
    image: int = 0,
    shading: int = 0,
    form: int = 0,
) -> dict:
    return {
        "width": width,
        "height": height,
        "rotation": rotation,
        "objects": text + path + image + shading + form,
        "text": text,
        "path": path,
        "image": image,
        "shading": shading,
        "form": form,
    }


# ---------------------------------------------------------------------------
# 文档骨架
# ---------------------------------------------------------------------------


class Doc:
    """单文档构造：页面树、标准字体、Info，最后 build。"""

    def __init__(self, name: str, version: str = "1.4", title: str | None = None, subject: str = "") -> None:
        self.name = name
        self.pdf = Pdf(version)
        self.pdf.set_id_from(f"magpdf-synthetic:{name}")
        self.pages_ref = self.pdf.reserve()
        self.kids: list[Ref] = []
        self.title = title or name
        self.subject = subject
        self.page_count: int | None = None  # 页面树有中间节点时由样本自己给出总页数
        self._std_fonts: dict[tuple[str, str], Ref] = {}

    # 对象
    def add(self, body: str | bytes) -> Ref:
        return self.pdf.add(body)

    def reserve(self) -> Ref:
        return self.pdf.reserve()

    def set(self, ref: Ref, body: str | bytes) -> Ref:
        return self.pdf.set(ref, body)

    def stream(self, entries: str, data: bytes, **kw) -> Ref:
        return self.pdf.stream(entries, data, **kw)

    def std_font(self, base: str = "Helvetica", encoding: str = "/WinAnsiEncoding") -> Ref:
        key = (base, encoding)
        if key not in self._std_fonts:
            enc = f" /Encoding {encoding}" if encoding else ""
            self._std_fonts[key] = self.add(f"<< /Type /Font /Subtype /Type1 /BaseFont /{base}{enc} >>")
        return self._std_fonts[key]

    def cjk_type0_font(self, encoding: str = "/UniGB-UCS2-H", base: str = "STSong-Light") -> Ref:
        """非嵌入的 Adobe-GB1 中文字体（依赖 PDFium 内置 CMap 与系统替代字体）。"""
        key = (base, f"type0{encoding}")
        if key not in self._std_fonts:
            fd_key = (base, "type0-descriptor")
            if fd_key not in self._std_fonts:
                fd = self.add(
                    f"<< /Type /FontDescriptor /FontName /{base} /Flags 4 /FontBBox [-25 -254 1000 880] /ItalicAngle 0 "
                    "/Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 >>"
                )
                self._std_fonts[fd_key] = fd
                self._std_fonts[(base, "type0-cidfont")] = self.add(
                    f"<< /Type /Font /Subtype /CIDFontType0 /BaseFont /{base} "
                    f"/CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> /FontDescriptor {fd} /DW 1000 /W [1 95 500] >>"
                )
            cid_font = self._std_fonts[(base, "type0-cidfont")]
            self._std_fonts[key] = self.add(
                f"<< /Type /Font /Subtype /Type0 /BaseFont /{base} /Encoding {encoding} /DescendantFonts [{cid_font}] >>"
            )
        return self._std_fonts[key]

    def form_xobject(self, bbox: Sequence[float], content: bytes, resources: str = "", extra: str = "", **kw) -> Ref:
        res = f" /Resources << {resources} >>" if resources else ""
        return self.stream(
            f"/Type /XObject /Subtype /Form /BBox [{nums(*bbox)}]{res} {extra}".strip(),
            content,
            **kw,
        )

    def image_xobject(
        self,
        width: int,
        height: int,
        colorspace: str,
        bpc: int,
        data: bytes,
        extra: str = "",
        **kw,
    ) -> Ref:
        cs = f" /ColorSpace {colorspace}" if colorspace else ""
        return self.stream(
            f"/Type /XObject /Subtype /Image /Width {width} /Height {height}{cs} /BitsPerComponent {bpc} {extra}".strip(),
            data,
            **kw,
        )

    # 页面
    def page(
        self,
        content: bytes | Ref | Sequence[Ref] | Sequence[bytes] | None,
        resources: str | None = "",
        mediabox: Sequence[float] | None = (0, 0, PAGE_W, PAGE_H),
        extra: str = "",
        flate: bool = False,
        parent: Ref | None = None,
        **stream_kw,
    ) -> Ref:
        """加一页。content 可以是字节（单流）、字节列表（多流 /Contents 数组）、已有流引用或 None（无 /Contents）。"""
        if content is None:
            contents = ""
        elif isinstance(content, Ref):
            contents = f" /Contents {content}"
        elif isinstance(content, (bytes, bytearray)):
            contents = f" /Contents {self.stream('', bytes(content), flate=flate, **stream_kw)}"
        else:
            refs = [c if isinstance(c, Ref) else self.stream("", c, flate=flate) for c in content]
            contents = " /Contents [" + " ".join(str(r) for r in refs) + "]"
        res = "" if resources is None else f" /Resources << {resources} >>"
        mb = "" if mediabox is None else f" /MediaBox [{nums(*mediabox)}]"
        ref = self.add(f"<< /Type /Page /Parent {parent or self.pages_ref}{mb}{res}{contents} {extra}".rstrip() + " >>")
        if parent is None:
            self.kids.append(ref)
        return ref

    def build(self, catalog_extra: str = "", info_extra: str = "", **build_kw) -> bytes:
        self.pdf.set(
            self.pages_ref,
            f"<< /Type /Pages /Kids [{' '.join(str(k) for k in self.kids)}] /Count {self._count_pages()} >>",
        )
        self.pdf.root = self.add(f"<< /Type /Catalog /Pages {self.pages_ref} {catalog_extra}".rstrip() + " >>")
        self.pdf.info = self.add(
            f"<< /Title {pdf_string(self.title)} /Subject {pdf_string(self.subject or 'MagPDF synthetic test sample')} "
            f"/Producer {pdf_string(PRODUCER)} {info_extra}".rstrip() + " >>"
        )
        return self.pdf.build(**build_kw)

    def _count_pages(self) -> int:
        # 默认直接挂在根节点下的子节点都是页面；有中间 Pages 节点的样本设置 page_count（见 struct-page-tree-inherited）。
        return self.page_count if self.page_count is not None else len(self.kids)


# ---------------------------------------------------------------------------
# 样本登记
# ---------------------------------------------------------------------------


@dataclass
class Sample:
    name: str
    group: str
    title: str
    tags: list[str]
    expected: str  # 要观察的现象 / 预期结果（规范第 6 节的 expected 字段）
    refs: list[str]  # 涉及的方案条目
    build: Callable[[], bytes]
    pages: list[dict]  # 每页的 expect（见 page_expect）
    check: str = "pass"  # pass | expect-warn（故意损坏：qpdf 必须报警且仍能打开）
    check_note: str = ""
    pdf_version: str = "1.4"
    extra_tags: list[str] = field(default_factory=list)

    @property
    def file(self) -> str:
        return f"{self.name}.pdf"


REGISTRY: list[Sample] = []


def sample(
    name: str,
    group: str,
    title: str,
    tags: Sequence[str],
    expected: str,
    refs: Sequence[str],
    pages: Sequence[dict] | dict | None = None,
    check: str = "pass",
    check_note: str = "",
    pdf_version: str = "1.4",
):
    """装饰器：登记一个样本。被装饰函数返回 (bytes, pages) 或 bytes（此时用 pages 参数）。"""

    def deco(fn: Callable[[], bytes | tuple[bytes, list[dict]]]):
        holder: dict = {}

        def build() -> bytes:
            result = fn()
            if isinstance(result, tuple):
                data, pg = result
                holder["pages"] = list(pg)
                return data
            return result

        s = Sample(
            name=name,
            group=group,
            title=title,
            tags=list(tags),
            expected=expected,
            refs=list(refs),
            build=build,
            pages=[pages] if isinstance(pages, dict) else list(pages or []),
            check=check,
            check_note=check_note,
            pdf_version=pdf_version,
        )
        s._holder = holder  # type: ignore[attr-defined]
        REGISTRY.append(s)
        return fn

    return deco


def resolve_pages(s: Sample) -> list[dict]:
    holder = getattr(s, "_holder", {})
    return holder.get("pages") or s.pages
