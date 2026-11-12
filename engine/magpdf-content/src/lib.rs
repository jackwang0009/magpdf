//! 无损内容流读写层（P0 计划 V17，"方案 2.1 备选做法"）。
//!
//! PDFium 仍是唯一的几何来源（对象类型、包围盒、矩阵、颜色）；本 crate 只负责字节层面：
//!
//! - [`pdf`]：最小 PDF 对象层，照 PDFium 的读法移植。取 `/Contents` 的原始字节、写增量更新。
//! - [`content`]：内容流的拼接与分词（`CPDF_StreamParser` 的移植），支持多条流拼接与内联图像。
//! - [`map`]：照 PDFium `CPDF_StreamContentParser` 的规则解释内容流，得出“哪个操作符产生哪个页面对象、
//!   对象由哪一段字节构成”。
//! - [`rewrite`]：只重写改过的对象，其余字节原样保留；写不了的修改明确拒绝，交给 PDFium 的生成器。
//!
//! 实验（往返、随机修改、回退、像素比对）在 `magpdf-cli` 的 `content` 模块里，这里只有读写本身。
//! 设计取舍、对应规则和实验结果见 `docs/p0/V17-自研无损内容流读写层原型.md`。
#![forbid(unsafe_code)]

pub mod content;
pub mod map;
pub mod pdf;
pub mod rewrite;

pub use pdf::{Error, PdfDocument, Result};
