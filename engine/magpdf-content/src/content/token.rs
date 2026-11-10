//! 记号的数据类型。

use crate::pdf::object::{PdfDict, PdfObject};

/// 记号种类。字符串与数组保留**解码前**的字节区间（写回时原样输出），
/// 解码结果只在数组需要时按需算（`TJ` 数组）。
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// 数字（整数或实数）。
    Number(f64),
    /// 名字，已解码 `#xx`（`/` 前缀已去掉）。
    Name(Vec<u8>),
    /// 字面量字符串 `( … )`，含转义。
    LiteralString,
    /// 十六进制串 `< … >`。
    HexString,
    /// 数组 `[ … ]`（含方括号）。
    Array,
    /// 字典 `<< … >>`。
    Dict,
    Bool(bool),
    Null,
    /// 操作符（关键字），如 `Tj`、`f`、`BT`。
    Operator,
    /// 内联图像 `BI … ID … EI`。
    InlineImage(InlineImage),
    /// 紧挨着上一记号、没有空白分隔的记号（PDFium 记为非法，这里保留供诊断）。
    StrayDelimiter(u8),
}

/// 内联图像。`data` 区间是 `ID` 之后一个空白之后到 `EI` 之前的原始字节（可能含补的空白）。
#[derive(Debug, Clone, PartialEq)]
pub struct InlineImage {
    pub dict: PdfDict,
    /// 二进制数据的字节区间 `[start, end)`（文件里的偏移）。
    pub data: (usize, usize),
    /// 像素数据的解码后长度（PDFium 写进 `/Length` 的那个数）。`None` 表示无法确定。
    pub decoded_len: Option<usize>,
    /// 是否用了 `EI` 边界兜底。
    pub ei_fallback: bool,
}

/// 一个记号的字节区间。`end` 不含。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenRange {
    pub start: usize,
    pub end: usize,
}

impl TokenRange {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// 一个记号。
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// 记号本身的字节区间（数字/名字/操作符是词，字符串含括号，内的图像是整个 `BI … EI`）。
    pub range: TokenRange,
    /// 记号**之前**的空白与注释的起点。写回时把"参数区"从它开始算。
    pub leading_start: usize,
    /// 记号所在的内容流序号。
    pub stream: usize,
    /// 操作符的原文（`TokenKind::Operator` 时有）。
    pub op: Vec<u8>,
    /// 数组的解码结果（`TokenKind::Array` 时按需由 `array_value` 得到）。
    pub array: Option<Vec<PdfObject>>,
}

impl Token {
    pub fn is_op(&self, name: &[u8]) -> bool {
        self.kind == TokenKind::Operator && self.op == name
    }

    pub fn as_number(&self) -> Option<f64> {
        match self.kind {
            TokenKind::Number(v) => Some(v),
            _ => None,
        }
    }
}
