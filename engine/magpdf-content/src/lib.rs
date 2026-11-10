//! 无损内容流读写层（P0 计划 V17，"方案 2.1 备选做法"）。
//!
//! PDFium 仍是唯一的几何来源（对象类型、包围盒、矩阵、颜色）；本 crate 只负责字节层面：
//!
//! - [`pdf`]：最小 PDF 对象层。取 `/Contents` 的原始字节、写增量更新。
//! - [`content`]：内容流分词器。每个记号带字节区间，支持多条流拼接与内联图像。
//! - [`map`]：按 PDFium `CPDF_StreamContentParser` 的规则，把操作符与 PDFium 的页面对象一一对应。
//! - [`rewrite`]：只重写改过的对象，按方案 2.3 的生成规则处理状态泄漏、剪切共用、文字块。
//!
//! 设计取舍与对应规则见 `docs/p0/V17-自研无损内容流读写层（第一轮）.md`。
#![forbid(unsafe_code)]

pub mod content;
pub mod experiment;
pub mod map;
pub mod pdf;
pub mod rewrite;

pub use pdf::{Error, PdfDocument, Result};
