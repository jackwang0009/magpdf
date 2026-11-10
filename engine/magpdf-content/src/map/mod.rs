//! 对应层：按 PDFium `CPDF_StreamContentParser` 的规则，把内容流里的操作符与页面对象一一对应。
//!
//! 规则逐条对照 `core/fpdfapi/page/cpdf_streamcontentparser.cpp`（BSD-3-Clause，见 `PDFIUM-NOTICE.md`），
//! 关键行为在 [`collect`] 的注释里写明；完整规格见报告第 3 节。
//!
//! 这一层只做**字节层面的解释**：不解析字体度量，不计算包围盒（几何以 PDFium 为准），
//! 只决定"哪个操作符产生哪个对象"，以及"对象由哪一段字节构成"（写回层的输入）。

mod clip;
mod collect;
mod ops;

pub use clip::{ClipEvent, ClipKind, Track};
pub use ops::PdfiumOp;

use crate::content::{Content, StreamData, Token, Tokenized};
use crate::pdf::pages::Contents;

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

/// 一次文字显示（`Tj`/`TJ`/`'`/`"`）产生的对象；渲染模式 4–7 时 PDFium 不建对象。
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// 显示操作符在记号表里的下标。
    pub op_token: usize,
}

/// 一个页面对象与它的字节来源。
#[derive(Debug, Clone, PartialEq)]
pub struct MappedObject {
    pub kind: ObjKind,
    /// 起始记号（`Path` 为第一次路径构造/绘制，`Text` 为 `BT`，其余为各自的操作符）。
    pub first_token: usize,
    /// 结束记号（含）。`Path` 为绘制操作符，`Text` 为 `ET`（未闭合时为流尾），其余同 `first_token`。
    pub last_token: usize,
    /// 文字对象的一次或多次显示。同一个 `BT … ET` 里的连续显示合并成一个文字对象（与 PDFium 相同）。
    pub text_runs: Vec<TextRun>,
    /// `Path` 的绘制操作符下标。
    pub paint_token: Option<usize>,
    /// `Path` 的绘制方式是否是填充（`f`/`F`/`f*`/`B`/`B*`/`b`/`b*`），填充色有效；否则描边色有效。
    pub fills: bool,
    /// `Path` 的开始就在一个已有剪切路径之内（`W` 之后的 `n`，或绘制后保留的老剪切）。
    pub starts_clipped: bool,
    /// 图像的 `Do` 引用的资源名（`Image`/`Form`）。
    pub resource: Option<Vec<u8>>,
    /// 是否是内联图像（`BI … EI`）。
    pub inline_image: bool,
    /// 透明组（Form 的 `/Group`）或软蒙版：写回时状态隔离的取舍不同（只记录，PDFium 读不到）。
    pub transactional: bool,
}

impl MappedObject {
    fn new(kind: ObjKind, token: usize) -> Self {
        Self {
            kind,
            first_token: token,
            last_token: token,
            text_runs: Vec::new(),
            paint_token: None,
            fills: true,
            starts_clipped: false,
            resource: None,
            inline_image: false,
            transactional: false,
        }
    }
}

/// 一页的对应结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PageMap {
    /// 页面的内容流（拼接后），写回层据此取字节。
    pub content: Content,
    pub tokens: Vec<Token>,
    /// 按 PDFium 建立对象的顺序排列；下标即 PDFium 的页面对象索引。
    pub objects: Vec<MappedObject>,
    /// 记号层发现的异常（内联图像未终止等）。
    pub notes: Vec<crate::content::Note>,
    /// 无法对应的原因（有内容时整页标为"不可对应"，退回 PDFium 生成器）。
    pub unmatched: Vec<Unmatched>,
    /// 解析器丢弃的字形（`Tj`/`'`/`"` 的空串、`TJ` 里的空串），不计入对象。
    pub empty_shows: usize,
    /// `W` 之后的 `n` 没有产出对象（剪切只改状态）。
    pub clip_only: usize,
    /// 没有任何绘制内容的路径（`m l m` 之类）被 PDFium 保留成空路径对象：这里不逐条判断几何，
    /// 只在 `Path` 对象数为 0 且对象数对不上时提示。
    pub diagnostics: Vec<String>,
}

impl PageMap {
    pub fn is_matched(&self) -> bool {
        self.unmatched.is_empty()
    }
}

/// 不可对应的原因分类（报告第 4 节按它统计）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Unmatched {
    /// 内容流取不到或解不开。
    UnreadableContents,
    /// 记号层发现流损坏（内联图像无法定界等）。
    DamagedStream,
    /// 其他读方（PDFium）不支持或这里来不及实现的操作符（如 `d0`/`d1` 出现在页面层）。
    UnknownOperator,
    /// 括号不配对（`q`/`Q`、`BT`/`ET`、`BMC`/`BDC`/`EMC`）。
    Nesting,
    /// 对象数与 PDFium 不一致（原因待查）。
    ObjectCountMismatch,
}

impl Unmatched {
    pub fn label(self) -> &'static str {
        match self {
            Self::UnreadableContents => "内容流不可读",
            Self::DamagedStream => "流损坏",
            Self::UnknownOperator => "未知或不支持的操作符",
            Self::Nesting => "嵌套错误",
            Self::ObjectCountMismatch => "对象数不一致（PDFium 丢弃或保留的方式不同）",
        }
    }
}

/// 从页面的 `/Contents` 取流并拼接。直接给已经解好码的流时用 [`map_streams`]。
pub fn map_page(
    doc: &crate::pdf::PdfDocument,
    page: &crate::pdf::pages::PageInfo,
) -> Result<PageMap, Unmatched> {
    let streams = read_streams(doc, page.contents.as_ref())?;
    Ok(map_streams(&streams))
}

/// 取一页的内容流（解码后）。返回空表表示页面没有内容流。
pub fn read_streams(
    doc: &crate::pdf::PdfDocument,
    contents: Option<&Contents>,
) -> Result<Vec<StreamData>, Unmatched> {
    let numbers: Vec<u32> = match contents {
        None => return Ok(Vec::new()),
        Some(Contents::One(n)) => vec![*n],
        Some(Contents::Many(v)) => v.clone(),
    };
    let mut out = Vec::with_capacity(numbers.len());
    for (i, n) in numbers.iter().enumerate() {
        // PDFium 跳过不是流的项（`CPDF_ContentParser::PrepareContent`）。
        let Some(crate::pdf::PdfObject::Stream(s)) = doc.object(*n) else {
            continue;
        };
        let data = s.decoded(doc).map_err(|_| Unmatched::UnreadableContents)?;
        out.push(StreamData {
            index: i,
            object_number: *n,
            data,
        });
    }
    Ok(out)
}

/// 对已经解好码的流做分词与对应。
pub fn map_streams(streams: &[StreamData]) -> PageMap {
    let content = Content::concat(streams);
    let Tokenized { tokens, notes, .. } = content.tokenize();
    let damaged = notes.iter().any(|n| {
        matches!(
            n.note,
            crate::content::NoteKind::InlineImageUnterminated
                | crate::content::NoteKind::Unterminated
        )
    });
    let mut st = collect::State::default();
    for (i, tok) in tokens.iter().enumerate() {
        collect::feed(&mut st, &tokens, i, tok);
    }
    let ctx = collect::FinishCtx { tokens: &tokens };
    let fin = collect::finish(st, &ctx);
    let mut unmatched = fin.unmatched;
    if damaged {
        unmatched.push(Unmatched::DamagedStream);
    }
    unmatched.sort_unstable();
    unmatched.dedup();
    PageMap {
        content,
        tokens,
        objects: fin.objects,
        notes,
        unmatched,
        empty_shows: fin.empty_shows,
        clip_only: fin.clip_only,
        diagnostics: fin.diagnostics,
    }
}
