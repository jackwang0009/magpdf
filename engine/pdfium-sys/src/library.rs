use std::ffi::{CString, c_char};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};
use crate::raw;

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// PDFium 库的进程级句柄。
///
/// PDFium 不是线程安全的（方案 3.3），所以 `Library` 既不 `Send` 也不 `Sync`，
/// 由它派生出的 [`Document`](crate::Document)、[`Page`](crate::Page) 等都借用它，
/// 编译期就保证所有 PDFium 调用留在同一个线程上。
///
/// 同一进程同一时刻只能有一个 `Library`；`Drop` 时释放全局资源。
#[derive(Debug)]
pub struct Library {
    _not_send_sync: PhantomData<*const ()>,
}

impl Library {
    /// 用默认配置初始化（系统默认字体目录）。
    pub fn init() -> Result<Self> {
        Self::init_with_font_dirs(&[])
    }

    /// 初始化并指定额外的字体扫描目录（为空时使用 PDFium 的默认目录）。
    pub fn init_with_font_dirs(font_dirs: &[&Path]) -> Result<Self> {
        if INITIALIZED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::AlreadyInitialized);
        }

        let owned: Vec<CString> = font_dirs
            .iter()
            .map(|p| CString::new(p.to_string_lossy().as_bytes()))
            .collect::<std::result::Result<_, _>>()
            .inspect_err(|_| INITIALIZED.store(false, Ordering::Release))?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|s| s.as_ptr()).collect();
        ptrs.push(ptr::null());

        let config = raw::FPDF_LIBRARY_CONFIG {
            version: 2,
            m_pUserFontPaths: if owned.is_empty() {
                ptr::null_mut()
            } else {
                ptrs.as_mut_ptr()
            },
            m_pIsolate: ptr::null_mut(),
            m_v8EmbedderSlot: 0,
            m_pPlatform: ptr::null_mut(),
            m_RendererType: raw::FPDF_RENDERERTYPE_AGG,
            m_FontLibraryType: raw::FPDF_FONTBACKENDTYPE_FREETYPE,
            m_BrotliEnabled: 0,
            m_IsolatePerDocument: 0,
        };
        // SAFETY: config 的 version 为 2，只有前三个字段会被读取；m_pUserFontPaths 指向以 NULL 结尾的
        // 有效 C 字符串数组，PDFium 在初始化期间复制这些路径，调用返回后不再引用 `owned`。
        unsafe { raw::FPDF_InitLibraryWithConfig(&config) };

        Ok(Self {
            _not_send_sync: PhantomData,
        })
    }

    /// 编译时链接的 PDFium 预编译库版本，例如 `159.0.8138.0`。
    pub fn linked_version() -> &'static str {
        env!("PDFIUM_PREBUILT_VERSION")
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: 与 FPDF_InitLibraryWithConfig 配对；此时所有借用 Library 的文档、页面都已析构。
        unsafe { raw::FPDF_DestroyLibrary() };
        INITIALIZED.store(false, Ordering::Release);
    }
}
