use std::ffi::{CString, c_char};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
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

    /// 这个进程实际加载的 PDFium 库文件（Windows 上是 `pdfium.dll`）的路径；取不到时是 `None`。
    ///
    /// [`linked_version`](Self::linked_version) 是编译时那一份预编译库的版本号。修补过的构建和原版的版本号
    /// 一样，运行时加载的也不一定是编译时那一份（看可执行文件旁边放的是哪个），所以报告里要分清用的是
    /// 哪一份 PDFium，得看实际加载的文件（V14 每晚往返报告第 10 节第 1 条）。不需要先初始化库。
    pub fn loaded_path() -> Option<PathBuf> {
        loaded::path()
    }
}

#[cfg(windows)]
mod loaded {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
        fn GetModuleFileNameW(module: *mut c_void, filename: *mut u16, size: u32) -> u32;
    }

    /// Windows 的路径最长约 32767 个字符（加了 `\\?\` 前缀时）。
    const MAX_PATH_CHARS: usize = 32 * 1024;

    pub(super) fn path() -> Option<PathBuf> {
        let name: Vec<u16> = "pdfium.dll".encode_utf16().chain([0]).collect();
        // SAFETY: name 是以 0 结尾的 UTF-16 字符串，调用期间一直有效；函数只读它，不保存指针。
        let module = unsafe { GetModuleHandleW(name.as_ptr()) };
        if module.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 512];
        loop {
            let size = u32::try_from(buf.len()).ok()?;
            // SAFETY: buf 有 size 个 u16 的空间可写；module 是上面取到的模块句柄。pdfium.dll 是随可执行文件
            // 静态导入的，进程存活期间不会卸载，句柄一直有效。
            let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), size) };
            let len = usize::try_from(len).ok()?;
            if len == 0 {
                return None;
            }
            // 返回值等于缓冲区大小说明路径被截断了：加大再取。
            if len < buf.len() {
                buf.truncate(len);
                return Some(PathBuf::from(OsString::from_wide(&buf)));
            }
            if buf.len() >= MAX_PATH_CHARS {
                return None;
            }
            buf.resize(buf.len() * 2, 0);
        }
    }
}

#[cfg(not(windows))]
mod loaded {
    use std::path::PathBuf;

    /// 只在 Windows 上实现（产品只有 Windows 版）；别的系统上报不出来。
    pub(super) fn path() -> Option<PathBuf> {
        None
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: 与 FPDF_InitLibraryWithConfig 配对；此时所有借用 Library 的文档、页面都已析构。
        unsafe { raw::FPDF_DestroyLibrary() };
        INITIALIZED.store(false, Ordering::Release);
    }
}
