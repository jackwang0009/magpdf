use crate::raw;

/// `pdfium-sys` 安全封装层的错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("PDFium 已初始化：同一进程同一时刻只能存在一个 Library")]
    AlreadyInitialized,
    #[error("打开文档失败：{0:?}")]
    Load(LoadError),
    #[error("页码越界：请求第 {index} 页，文档共 {count} 页")]
    PageIndex { index: usize, count: usize },
    #[error("加载页面失败：{0:?}")]
    LoadPage(LoadError),
    #[error("创建 {width}x{height} 位图失败")]
    Bitmap { width: u32, height: u32 },
    #[error("渲染尺寸无效：{width}x{height}")]
    InvalidSize { width: u32, height: u32 },
    #[error("重新生成页面内容流失败")]
    GenerateContent,
    #[error("保存文档失败")]
    Save,
    #[error("页面对象操作失败")]
    PageObject,
    #[error("加载文字页失败")]
    LoadTextPage,
    #[error("字符串含 NUL 字符")]
    Nul(#[from] std::ffi::NulError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `FPDF_GetLastError()` 的取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    Unknown,
    File,
    Format,
    Password,
    Security,
    Page,
    Other(u32),
}

impl LoadError {
    pub(crate) fn last() -> Self {
        // SAFETY: 无参数的纯查询函数，只读线程局部的错误码。
        // Windows 上 c_ulong 即 u32，与 bindgen 生成的 FPDF_ERR_* 常量同型。
        let code = unsafe { raw::FPDF_GetLastError() };
        match code {
            raw::FPDF_ERR_UNKNOWN => Self::Unknown,
            raw::FPDF_ERR_FILE => Self::File,
            raw::FPDF_ERR_FORMAT => Self::Format,
            raw::FPDF_ERR_PASSWORD => Self::Password,
            raw::FPDF_ERR_SECURITY => Self::Security,
            raw::FPDF_ERR_PAGE => Self::Page,
            other => Self::Other(other),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
