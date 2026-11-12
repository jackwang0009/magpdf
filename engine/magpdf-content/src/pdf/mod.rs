//! 最小 PDF 对象层（V17 任务书 2 的选项 (b)）。
//!
//! 只做内容流读写需要的那部分：交叉引用表与交叉引用流、对象流、常用过滤器、
//! 间接引用解析、页面树、增量更新写出。不做加密、线性化、对象改写以外的结构编辑。
//!
//! 第二轮起这一层**照 PDFium 的读法移植**（`CPDF_Parser`、`CPDF_SyntaxParser`、`CPDF_Document`
//! 的页树部分、`CPDF_Page` 的继承属性与尺寸；BSD-3-Clause，版权声明见本 crate 的
//! `PDFIUM-NOTICE.md`）：同一份文件，这里认到的对象、页数、每页的内容流字节要和 PDFium 认到的一样，
//! 包括文件损坏时 PDFium 的容错结果（交叉引用重建、长度写错的流、解到一半的压缩数据）。
//! 第一轮按规范自己写的读法在损坏文件上和 PDFium 对不上，是当时“不可对应”的主要来源之一。

mod filter;
pub mod lexer;
pub mod object;
pub mod pages;
pub mod pairs;
pub mod reader;
pub mod writer;

pub use filter::{
    FilterError, apply_filter_chain, decode_like_pdfium, decode_like_pdfium_checked, encode_flate,
};
pub use object::{PdfDict, PdfObject, PdfStreamRef, encode_name};
pub use reader::{EntryLocation, PdfDocument, XrefError};
pub use writer::{IncrementalWriter, WriteError, format_real, write_object};

/// 本模块的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("不是 PDF 文件（缺少 %PDF 头）")]
    NotPdf,
    #[error("交叉引用损坏：{0}")]
    Xref(#[from] XrefError),
    #[error("对象解析失败：{0}")]
    Syntax(String),
    #[error("对象 {0} 不存在")]
    MissingObject(u32),
    #[error("过滤器：{0}")]
    Filter(#[from] FilterError),
    #[error("IO：{0}")]
    Io(#[from] std::io::Error),
}

/// 便于函数签名的结果类型。
pub type Result<T> = std::result::Result<T, Error>;
