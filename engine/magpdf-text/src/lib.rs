//! 文字服务（方案 3.1、第 5 节）：文字块识别（5.1）、修改已有文字的三级回退（5.2）、
//! 排版（5.3）、字体来源与子集化（5.4）。P0 阶段对应验证项 V07（聚类原型）和 V08（"改一行字"端到端）。
//!
//! 输入来自 [`pdfium_sys::TextPage`] 的字符级信息；本 crate 不直接调用 PDFium 的 FFI。
//!
//! 文字块识别（V07）的用法：
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use magpdf_text::{LayoutParams, PageText, analyze};
//! let lib = pdfium_sys::Library::init()?;
//! let doc = pdfium_sys::Document::load_file(&lib, std::path::Path::new("a.pdf"), None)?;
//! let page = doc.page(0)?;
//! let params = LayoutParams::default();
//! let text = PageText::from_page(&page, params.ruling_max_thickness)?;
//! let layout = analyze(&text, &params);
//! println!("{} 行，{} 块", layout.lines.len(), layout.blocks.len());
//! # Ok(()) }
//! ```
#![forbid(unsafe_code)]

pub mod edit;
pub mod error;
pub mod eval;
pub mod fontfile;
pub mod geom;
pub mod glyph;
pub mod layout;
pub mod shaping;
pub mod subset;

pub use error::{EditError, Result as EditResult};
pub use fontfile::FontFace;
pub use geom::{Point, Quadrant, Rect};
pub use glyph::{FontInfo, Glyph, PageText, collect_rulings};
pub use layout::{Alignment, Block, Layout, LayoutParams, Line, LinePitch, analyze};

#[cfg(test)]
pub(crate) mod testutil;
