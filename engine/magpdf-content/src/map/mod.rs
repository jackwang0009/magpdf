//! 对应层：照 PDFium 解释内容流的规则，得出“哪个操作符产生哪个页面对象、对象由哪一段字节构成”。
//!
//! 第二轮起这一层是 `core/fpdfapi/page/cpdf_streamcontentparser.cpp` 里**决定建不建对象**的那部分的
//! 移植（BSD-3-Clause，版权声明见本 crate 的 `PDFIUM-NOTICE.md`），见 [`interp`]。第一轮按规范自己
//! 归纳规则，在 PDFium 和规范不一样的地方（操作数个数不对时忽略操作符、路径的快速解析、只有一个点的
//! 路径、字体找不到时换备用字体、`TJ` 里全是空串、资源缺失的 `Do`/`sh`……）对不上。
//!
//! 这一层只做**字节层面的解释**：不解析字体度量，不计算包围盒（几何以 PDFium 为准）。

mod interp;
pub mod resources;

use std::ops::Range;

use crate::content::Content;
use crate::pdf::pages::PageInfo;
use crate::pdf::reader::PdfDocument;

pub use resources::{DocResources, NoResources, Resources};

/// 变换矩阵 `[a b c d e f]`，行向量在左：`(x, y) → (a·x + c·y + e, b·x + d·y + f)`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// 先做 `self`，再做 `next`。
    pub fn then(self, next: Self) -> Self {
        Self {
            a: self.a * next.a + self.b * next.c,
            b: self.a * next.b + self.b * next.d,
            c: self.c * next.a + self.d * next.c,
            d: self.c * next.b + self.d * next.d,
            e: self.e * next.a + self.f * next.c + next.e,
            f: self.e * next.b + self.f * next.d + next.f,
        }
    }

    /// 把页面空间里的位移 `(dx, dy)` 换算到这个矩阵之前的空间：求 `(vx, vy)` 使
    /// `(vx, vy)` 经过矩阵的线性部分之后等于 `(dx, dy)`。矩阵不可逆（或接近不可逆）时没有结果。
    pub fn pull_back(self, dx: f64, dy: f64) -> Option<(f64, f64)> {
        let det = self.a * self.d - self.b * self.c;
        let scale = (self.a.abs() + self.b.abs() + self.c.abs() + self.d.abs()).max(1e-30);
        if !det.is_finite() || det.abs() < 1e-9 * scale * scale {
            return None;
        }
        let vx = (dx * self.d - dy * self.c) / det;
        let vy = (dy * self.a - dx * self.b) / det;
        (vx.is_finite() && vy.is_finite()).then_some((vx, vy))
    }
}

/// 轴对齐的矩形（页面空间），`left <= right`、`bottom <= top`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

impl Rect {
    /// `other` 是不是整个落在这个矩形里。四条边各放宽 `slack`。
    pub fn contains(&self, other: &Self, slack: f64) -> bool {
        other.left >= self.left - slack
            && other.right <= self.right + slack
            && other.bottom >= self.bottom - slack
            && other.top <= self.top + slack
    }

    /// 平移之后的矩形。
    pub fn translated(&self, dx: f64, dy: f64) -> Self {
        Self {
            left: self.left + dx,
            bottom: self.bottom + dy,
            right: self.right + dx,
            top: self.top + dy,
        }
    }
}

/// 判断“矩形包不包得住”时放宽的量（点）。PDFium 用单精度数直接比较，这边的坐标是另算出来的，
/// 差在末位上；放宽之后只会把“也许包得住”多算进来，见 [`MappedObject::droppable_clip`]。
pub const CLIP_SLACK: f64 = 0.01;

/// 对象类别，与 `FPDF_PAGEOBJ_*` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjKind {
    Text,
    Path,
    Image,
    Shading,
    Form,
}

impl ObjKind {
    /// 报告里的中文名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "文字",
            Self::Path => "路径",
            Self::Image => "图像",
            Self::Shading => "渐变",
            Self::Form => "表单",
        }
    }
}

/// 填充规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    None,
    NonZero,
    EvenOdd,
}

/// 文字显示操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowKind {
    /// `Tj`
    Show,
    /// `TJ`
    ShowArray,
    /// `'`
    NextLineShow,
    /// `"`
    NextLineShowSpaced,
}

/// 对象出现时的填充色是怎么设出来的：改色之后要把它设回去。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FillHistory {
    /// 依次重放这些操作符（各自的操作数起点到操作符末尾）就回到当时的填充色。
    pub ops: Vec<Range<usize>>,
    /// 第一个操作符不是“一步到位”的设色（`g`/`rg`/`k`/有效的 `cs`）：重放之前先回到默认的黑色。
    pub from_default: bool,
    /// 记录的操作符太多，放弃了：这个对象不能改色。
    pub overflow: bool,
}

/// 路径对象。
#[derive(Debug, Clone, PartialEq)]
pub struct PathDetail {
    /// 绘制之前挂着的 `W`/`W*`（这条路径同时是剪切路径）。
    pub clip: Option<Range<usize>>,
    /// 从第一个构造操作符到绘制操作符之间只有路径构造、至多一个 `W`/`W*` 和绘制操作符：
    /// 这一段字节可以整体搬动、复制。
    pub simple: bool,
    /// 路径的点数（PDFium 的 `FPDFPath_CountSegments`）。
    pub points: usize,
    pub stroke: bool,
    pub fill: FillRule,
}

/// 文字对象：一次 `Tj`/`TJ`/`'`/`"`。
#[derive(Debug, Clone, PartialEq)]
pub struct TextDetail {
    pub show: ShowKind,
    /// 操作数的写法规整（`Tj`/`'` 一个字符串，`TJ` 一个数组，`"` 两个数加一个字符串）时，
    /// 各操作数的字节区间；不规整时为空，只能整体删除或改色。
    pub operands: Vec<Range<usize>>,
    /// 显示之前笔的位置就在行首（上一个定位操作符之后还没有显示过文字）。
    pub at_line_start: bool,
    /// 后面还有文字接着这一次显示的笔位置往下写（中间没有重新定位）。
    pub continued: bool,
    /// 在 `BT … ET` 之内。PDFium 不管这个（文字操作符写在外面也照样执行），这里只是如实记下。
    pub in_text_block: bool,
    /// 当时的文字矩阵（`Tm` 设的，不含 `Td` 的位移）。
    pub tm: Matrix,
    /// 当时的渲染模式（`Tr`）。Type3 字体不看它。
    pub render_mode: i32,
    pub type3: bool,
    pub font_size: f32,
}

/// 图像对象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDetail {
    /// 内联图像 `BI … EI`；否则是 `Do`。
    pub inline: bool,
    /// 内联图像字典里写的宽高（`Do` 的图像这里是 0）。
    pub width: i32,
    pub height: i32,
}

/// 渐变对象所在的 `q … Q` 组：组里只有它一个对象、它的剪切全部是组里建立的。
/// 移动时在组的开头插一个 `cm`，剪切和渐变一起动（PDFium 移动渐变对象时也是连剪切一起动）。
#[derive(Debug, Clone, PartialEq)]
pub struct ShadingGroup {
    /// `q` 操作符之后的位置。
    pub after_q: usize,
    /// `q` 时的变换矩阵。
    pub ctm: Matrix,
}

/// 各类对象特有的信息。
#[derive(Debug, Clone, PartialEq)]
pub enum Detail {
    Path(PathDetail),
    Text(TextDetail),
    Image(ImageDetail),
    Shading(Option<ShadingGroup>),
    Form,
}

/// 一个页面对象与它的字节来源。
#[derive(Debug, Clone, PartialEq)]
pub struct MappedObject {
    pub kind: ObjKind,
    /// 这个对象自己的字节：第一个操作数到操作符末尾；路径是第一个构造操作符的操作数到绘制操作符末尾。
    pub span: Range<usize>,
    /// 产生对象的操作符（绘制、显示、`Do`、`sh`；内联图像是整个 `BI … EI`）。
    pub op: Range<usize>,
    /// 这个操作符的操作数起点（没有操作数时等于 `op.start`）。
    pub op_operands: usize,
    /// 当时的变换矩阵。
    pub ctm: Matrix,
    pub fill: FillHistory,
    /// PDFium 解析完一页之后会逐个检查对象的剪切（`CPDF_ContentParser::CheckClip`）：剪切只有一条
    /// 路径、那条路径是矩形、又包得住对象的外接矩形时，把这个对象的剪切去掉。之后在内存里移动这个
    /// 对象，它就不受那条剪切管了；而写在内容流里的对象怎么移动都还在剪切里面。
    ///
    /// 这里是“可能因此被去掉”的那条剪切的范围（页面空间）。`None` 表示肯定不会被去掉：没有剪切、
    /// 剪切不止一条路径、路径带曲线，或者对象是渐变（PDFium 不检查渐变）。“是不是矩形”“是不是只剩
    /// 一条”都往宽里算，不会漏；到底去没去掉，还要拿 PDFium 报的外接矩形来比。
    pub droppable_clip: Option<Rect>,
    pub detail: Detail,
}

/// 一页的对应结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PageMap {
    /// 页面的内容流（拼接后），写回层据此取字节。
    pub content: Content,
    /// 按 PDFium 建立对象的顺序排列；下标即 PDFium 的页面对象索引。
    pub objects: Vec<MappedObject>,
    /// 执行过的操作符个数。
    pub operators: usize,
    /// 解释不下去的原因。有它时后面的对象不可信，这一页归为“对不上”。
    pub blocked: Option<Unmatched>,
}

/// 不可对应的原因分类（报告按它统计）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Unmatched {
    /// 内联图像用了这里定不了界的过滤器（`DCTDecode`、`CCITTFaxDecode`）。
    InlineImageFilter(String),
    /// 对象数与 PDFium 不一致。
    ObjectCountMismatch,
    /// 对象数一致，但某个对象的类别或特征（点数、字号、图像宽高）与 PDFium 不一致。
    ObjectMismatch,
}

impl Unmatched {
    pub fn label(&self) -> String {
        match self {
            Self::InlineImageFilter(filter) => {
                format!("内联图像用了定不了界的过滤器（{filter}）")
            }
            Self::ObjectCountMismatch => "对象数与 PDFium 不一致".to_owned(),
            Self::ObjectMismatch => "对象的类别或特征与 PDFium 不一致".to_owned(),
        }
    }
}

/// 解释一页的内容流。
pub fn map_page(doc: &PdfDocument, page: &PageInfo) -> PageMap {
    let content = Content::read(doc, &page.contents);
    let resources = DocResources::new(doc, page.resources.clone());
    map_content(content, &resources)
}

/// 解释已经取出来的内容流。
pub fn map_content(content: Content, resources: &dyn Resources) -> PageMap {
    let outcome = interp::interpret(&content.data, resources);
    PageMap {
        content,
        objects: outcome.objects,
        operators: outcome.operators,
        blocked: outcome.blocked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_composition_and_pull_back() {
        let scale = Matrix {
            a: 2.0,
            d: 4.0,
            ..Matrix::IDENTITY
        };
        let shift = Matrix {
            e: 10.0,
            f: 20.0,
            ..Matrix::IDENTITY
        };
        // 先缩放再平移。
        let m = scale.then(shift);
        assert_eq!((m.a, m.d, m.e, m.f), (2.0, 4.0, 10.0, 20.0));
        // 先平移再缩放：平移量也被缩放。
        let m = shift.then(scale);
        assert_eq!((m.e, m.f), (20.0, 80.0));
        assert_eq!(scale.pull_back(10.0, 10.0), Some((5.0, 2.5)));
        let singular = Matrix {
            a: 0.0,
            d: 0.0,
            ..Matrix::IDENTITY
        };
        assert_eq!(singular.pull_back(1.0, 1.0), None);
        // 旋转 90°：(x, y) → (-y, x)。
        let rot = Matrix {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        };
        let (vx, vy) = rot.pull_back(0.0, 1.0).unwrap();
        assert!((vx - 1.0).abs() < 1e-12 && vy.abs() < 1e-12);
    }
}
