//! 改字流程的错误类型。
//!
//! 流程里的"某一级回退不适用"不是错误，而是 [`crate::edit::TierSkipped`]；
//! 这里只放真正让流程停下的情况。

use thiserror::Error;

pub type Result<T> = std::result::Result<T, EditError>;

#[derive(Debug, Error)]
pub enum EditError {
    #[error("PDFium 调用失败：{0}")]
    Pdfium(#[from] pdfium_sys::Error),
    #[error("读不到页面文字页")]
    NoTextPage,
    #[error("第 {page} 页没有第 {line} 行（共 {lines} 行）")]
    NoSuchLine {
        page: usize,
        line: usize,
        lines: usize,
    },
    #[error("这一行含有不可编辑的文字（没有 Unicode 映射），方案 5.1 第 2 条")]
    NotEditable,
    #[error("这一行跨了多个文字对象或字体，P0 只处理单对象单字体的行（遗留问题，见 V08 报告）")]
    MixedLine,
    #[error("这一行含 Type3 字体的文字，方案 5.5 放到 P4")]
    Type3Font,
    #[error("字体文件无效：{0}")]
    FontData(String),
    #[error("子集化失败：{0}")]
    Subset(String),
    #[error("塑形失败：{0}")]
    Shape(String),
    #[error("三级回退都不适用：{0}")]
    NoTier(String),
    #[error("保存失败")]
    Save,
    #[error("读文件 {path} 失败：{source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

impl EditError {
    /// 失败分类名（报告里的统计用，不含文档内容）。
    pub fn class(&self) -> &'static str {
        match self {
            Self::Pdfium(_) => "pdfium",
            Self::NoTextPage => "no_text_page",
            Self::NoSuchLine { .. } => "no_such_line",
            Self::NotEditable => "not_editable",
            Self::MixedLine => "mixed_line",
            Self::Type3Font => "type3",
            Self::FontData(_) => "font_data",
            Self::Subset(_) => "subset",
            Self::Shape(_) => "shape",
            Self::NoTier(_) => "no_tier",
            Self::Save => "save",
            Self::Io { .. } => "io",
        }
    }
}
