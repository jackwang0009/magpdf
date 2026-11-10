//! 最小 PDF 对象层（V17 任务书 2 的选项 (b)）。
//!
//! 只做内容流读写需要的那部分：交叉引用表与交叉引用流、对象流、常用过滤器、
//! 间接引用解析、增量更新写出。不做加密、线性化、对象改写以外的结构编辑。
//!
//! 设计取舍见 `docs/p0/V17-自研无损内容流读写层（第一轮）.md` 第 2 节：内容流的原始字节
//! 必须按 PDFium 的容器规则取（`/Contents` 数组逐条拼接、**每条流之间插入一个空格**），
//! 交给别的库取就必须复刻它的拼接方式，而两边解析结果一旦不一致，操作符与对象的对应全错。
//! 自己写这一层可以保证"取出来的就是 PDFium 解释的那串字节"。

mod filter;
pub mod lexer;
pub mod object;
pub mod pages;
pub mod pairs;
pub mod provenance;
pub mod reader;
pub mod writer;

pub use filter::{FilterError, apply_filter_chain, encode_flate};
pub use object::{PdfDict, PdfObject, PdfStreamRef, decode_name, encode_name};
pub use pairs::{PagePair, pair_pages};
pub use reader::{PdfDocument, XrefError};
pub use writer::{IncrementalWriter, StreamPatch};

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
