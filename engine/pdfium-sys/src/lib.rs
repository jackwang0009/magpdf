//! PDFium 的 Rust 绑定（方案 12 的 `engine/pdfium-sys/`）。
//!
//! 分两层：
//! - [`raw`]：bindgen 生成的原始 FFI，一比一对应 PDFium 公共头文件；
//! - 顶层类型：最小的安全封装，把句柄寿命、线程约束和错误码翻译成 Rust 类型。
//!   工作区内只有本 crate 允许出现 `unsafe`（《工程规范》4），其他 crate 一律通过这一层调用。
//!
//! 线程模型：PDFium 不是线程安全的，[`Library`] 不 `Send`/`Sync`，由它派生的所有句柄都借用它，
//! 因此所有 PDFium 调用必然在同一线程（方案 3.3 的"PDFium 专用线程"）。
//!
//! 当前链接的是 pdfium-binaries 发布的原版 PDFium 预编译库（`scripts/fetch-pdfium-prebuilt.ps1`），
//! 用于 P0 的往返基线（V02）。切换到自家分支后，`MAG_*` 扩展通过同一套机制暴露。
#![deny(unsafe_op_in_unsafe_fn)]

pub mod raw;

mod bitmap;
mod document;
/// 编辑相关（P0 V08）：文字对象读写、新建字体与文字对象、字形度量与路径对象。
pub mod edit;
mod error;
mod font;
mod geom;
mod library;
mod object;
mod page;
mod text;
mod tile;

pub use bitmap::Bitmap;
pub use document::{Document, PageGeometry, SaveFlags};
pub use edit::LoadedFont;
pub use error::{Error, LoadError, Result};
pub use font::Font;
pub use geom::{Matrix, Rect, Rgba};
pub use library::Library;
pub use object::{ObjectKind, PageObject, TextRenderMode};
pub use page::{Page, RenderFlags, RenderOptions};
pub use text::{CharInfo, TextPage};
pub use tile::{PageSurface, PixelRect, TILE_SIZE, render_rect};
