//! 内容流分词器（V17 任务书第 3 条）。
//!
//! 语法按 PDFium 的 `core/fpdfapi/page/cpdf_streamparser.cpp`（BSD-3-Clause，见 `PDFIUM-NOTICE.md`）
//! 移植，行为对齐的地方在注释里写明：
//!
//! - 每个记号记录**字节区间**（流序号 + 偏移），写回时据此原样保留未修改的部分；
//! - 数字、字符串（转义、嵌套括号）、十六进制串、名字（含 `#xx`）、数组、字典、注释、操作符；
//! - 内联图像 `BI … ID … EI` 整体作为一个记号，二进制数据按 `/L`（`/Length`）、过滤器、宽高、位深
//!   确定长度，读不出来时退回查找 `EI` 边界（与 PDFium 相同）；
//! - 多条内容流按 PDFium `CPDF_ContentParser::PrepareContent` 的方式拼接：**每条流之间插入一个空格**，
//!   记号可以跨流（`q` 与 `Q`、`BT` 与 `ET`、操作数都可能分在两条流里）。
//!
//! 损坏输入不 panic：所有读取都有边界检查，返回 `None` 或提前结束。

mod syntax;
#[cfg(test)]
mod tests;
mod token;

pub use token::{InlineImage, Token, TokenKind, TokenRange};

use crate::pdf::object::{PdfDict, PdfObject};

/// 一条内容流：第几条（页的 `/Contents` 数组里的下标）与解码后的字节。
#[derive(Debug, Clone, PartialEq)]
pub struct StreamData {
    pub index: usize,
    /// 流对象号（重写时用来定位）。
    pub object_number: u32,
    pub data: Vec<u8>,
}

/// 拼接后的内容流缓冲。
#[derive(Debug, Clone, PartialEq)]
pub struct Content {
    /// 拼接后的字节（每条流之后补一个空格，与 PDFium 一致）。
    pub data: Vec<u8>,
    /// 每条流在 `data` 里的起点。
    pub segment_starts: Vec<usize>,
    /// 每条流的长度（不含补的空格）。
    pub segment_lengths: Vec<usize>,
    /// 对应的对象号。
    pub object_numbers: Vec<u32>,
}

impl Content {
    /// 按 PDFium 的规则拼接多条流。
    pub fn concat(streams: &[StreamData]) -> Self {
        let mut data = Vec::with_capacity(streams.iter().map(|s| s.data.len() + 1).sum());
        let mut segment_starts = Vec::with_capacity(streams.len());
        let mut segment_lengths = Vec::with_capacity(streams.len());
        let mut object_numbers = Vec::with_capacity(streams.len());
        for s in streams {
            segment_starts.push(data.len());
            segment_lengths.push(s.data.len());
            object_numbers.push(s.object_number);
            data.extend_from_slice(&s.data);
            data.push(b' ');
        }
        Self {
            data,
            segment_starts,
            segment_lengths,
            object_numbers,
        }
    }

    /// 偏移所在的流序号。与 PDFium 的 `GetCurrentStreamIndex` 相同：
    /// `upper_bound(segment_starts, pos) - 1`；偏移恰好在某条流之后补的空格上时归前一条。
    pub fn segment_of(&self, pos: usize) -> usize {
        self.segment_starts
            .partition_point(|&start| start <= pos)
            .saturating_sub(1)
    }

    pub fn segment_count(&self) -> usize {
        self.segment_starts.len()
    }

    /// 第 `i` 条流的字节区间（不含补的空格）。
    pub fn segment(&self, i: usize) -> std::ops::Range<usize> {
        let start = self.segment_starts.get(i).copied().unwrap_or(0);
        let len = self.segment_lengths.get(i).copied().unwrap_or(0);
        start..start + len
    }
}

/// 分词结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Tokenized {
    pub tokens: Vec<Token>,
    /// 解析过程中遇到的、不属于任何记号的信息（内联图像读不出边界等），供对应层归类。
    pub notes: Vec<Note>,
}

/// 分词过程中的异常记录（不中断解析，对应层用来判断"这一页不可对应"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub offset: usize,
    pub note: NoteKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// 内联图像的字节数据无法确定长度，用 `EI` 边界兜底。
    InlineImageEidFallback,
    /// 内联图像连 `EI` 都找不到，当作读到流尾。
    InlineImageUnterminated,
    /// 字符串、数组、字典在流尾仍未闭合。
    Unterminated,
    /// 名字解码后为空（`/` 后面直接是分隔符）。
    EmptyName,
}

impl Content {
    /// 分词。
    pub fn tokenize(&self) -> Tokenized {
        syntax::tokenize(self)
    }
}

/// 判断一个字典是不是内联图像的字（`/F`、`/DP` 等，多数是**缩写**，先按全文名再按缩写查）。
pub fn inline_dict_get<'a>(dict: &'a PdfDict, full: &[u8], abbrev: &[u8]) -> Option<&'a PdfObject> {
    dict.get(full).or_else(|| dict.get(abbrev))
}

#[expect(
    dead_code,
    reason = "调试开关：分词异常时打开 MAGPDF_DEBUG 打印中间状态"
)]
pub(crate) fn debug_enabled() -> bool {
    #[cfg(debug_assertions)]
    {
        use std::sync::OnceLock;
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("MAGPDF_DEBUG").is_some())
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}
