use std::ffi::{CString, c_int, c_void};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr;

use crate::error::{Error, LoadError, Result};
use crate::library::Library;
use crate::page::Page;
use crate::raw;

/// 保存标志，对应 `fpdf_save.h`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveFlags(pub u32);

impl SaveFlags {
    /// 增量保存：原文件字节保留，修改追加在末尾（方案 9.1）。
    pub const INCREMENTAL: Self = Self(raw::FPDF_INCREMENTAL);
    /// 完整重写。
    pub const NO_INCREMENTAL: Self = Self(raw::FPDF_NO_INCREMENTAL);
    /// 去掉加密。
    pub const REMOVE_SECURITY: Self = Self(raw::FPDF_REMOVE_SECURITY);
}

/// 一个已打开的 PDF 文档。数据由 `Document` 持有（PDFium 要求内存文档的缓冲区在文档关闭前有效）。
pub struct Document<'lib> {
    handle: raw::FPDF_DOCUMENT,
    _data: Vec<u8>,
    _lib: PhantomData<&'lib Library>,
}

impl std::fmt::Debug for Document<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("bytes", &self._data.len())
            .finish_non_exhaustive()
    }
}

impl<'lib> Document<'lib> {
    /// 读入整个文件后从内存打开。P0 的样本都不大，这样最省事也绕开了路径编码问题；
    /// 大文件按需改用 `FPDF_LoadCustomDocument`。
    pub fn load_file(lib: &'lib Library, path: &Path, password: Option<&str>) -> Result<Self> {
        let data = std::fs::read(path)?;
        Self::load_bytes(lib, data, password)
    }

    /// 从内存打开。
    pub fn load_bytes(_lib: &'lib Library, data: Vec<u8>, password: Option<&str>) -> Result<Self> {
        let password_c = password.map(CString::new).transpose()?;
        let password_ptr = password_c.as_ref().map_or(ptr::null(), |p| p.as_ptr());
        // SAFETY: data 的缓冲区由返回的 Document 持有，寿命覆盖整个文档句柄；password 为有效 C 字符串或 NULL。
        let handle = unsafe {
            raw::FPDF_LoadMemDocument64(data.as_ptr().cast::<c_void>(), data.len(), password_ptr)
        };
        if handle.is_null() {
            return Err(Error::Load(LoadError::last()));
        }
        Ok(Self {
            handle,
            _data: data,
            _lib: PhantomData,
        })
    }

    /// 句柄（crate 内使用，给 `edit` 模块的新接口用）。
    pub(crate) fn raw_handle(&self) -> raw::FPDF_DOCUMENT {
        self.handle
    }

    pub fn page_count(&self) -> usize {
        // SAFETY: handle 有效。
        let n = unsafe { raw::FPDF_GetPageCount(self.handle) };
        usize::try_from(n).unwrap_or(0)
    }

    /// 文件头声明的 PDF 版本 ×10，例如 1.7 返回 17。
    pub fn file_version(&self) -> Option<i32> {
        let mut v: c_int = 0;
        // SAFETY: handle 有效，v 是有效的输出位置。
        let ok = unsafe { raw::FPDF_GetFileVersion(self.handle, &mut v) };
        (ok != 0).then_some(v)
    }

    /// 加载第 `index` 页（从 0 开始）。
    pub fn page(&self, index: usize) -> Result<Page<'_>> {
        let count = self.page_count();
        if index >= count {
            return Err(Error::PageIndex { index, count });
        }
        let idx = c_int::try_from(index).map_err(|_| Error::PageIndex { index, count })?;
        // SAFETY: handle 有效，index 已检查在范围内。
        let handle = unsafe { raw::FPDF_LoadPage(self.handle, idx) };
        if handle.is_null() {
            return Err(Error::LoadPage(LoadError::last()));
        }
        // SAFETY: handle 是刚由 FPDF_LoadPage 返回的有效页面句柄，归 Page 所有。
        Ok(unsafe { Page::from_raw(handle, self) })
    }

    /// 保存为新文件内容（`FPDF_SaveAsCopy`），返回字节。
    pub fn save_to_vec(&self, flags: SaveFlags) -> Result<Vec<u8>> {
        #[repr(C)]
        struct Writer {
            base: raw::FPDF_FILEWRITE,
            out: Vec<u8>,
        }

        unsafe extern "C" fn write_block(
            this: *mut raw::FPDF_FILEWRITE,
            data: *const c_void,
            size: std::ffi::c_ulong,
        ) -> c_int {
            // SAFETY: PDFium 回传的 this 正是我们传入的 Writer 的首字段地址（repr(C)，base 在最前），
            // data/size 描述一段由 PDFium 持有的有效内存。
            unsafe {
                let writer = this.cast::<Writer>();
                let len = usize::try_from(size).unwrap_or(0);
                let bytes = std::slice::from_raw_parts(data.cast::<u8>(), len);
                (*writer).out.extend_from_slice(bytes);
            }
            1
        }

        let mut writer = Writer {
            base: raw::FPDF_FILEWRITE {
                version: 1,
                WriteBlock: Some(write_block),
            },
            out: Vec::new(),
        };
        // SAFETY: handle 有效；writer 在调用期间保持有效且地址稳定。
        let ok = unsafe { raw::FPDF_SaveAsCopy(self.handle, &mut writer.base, flags.0) };
        if ok == 0 {
            return Err(Error::Save);
        }
        Ok(writer.out)
    }

    pub fn save_to_file(&self, path: &Path, flags: SaveFlags) -> Result<()> {
        let bytes = self.save_to_vec(flags)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

impl Drop for Document<'_> {
    fn drop(&mut self) {
        // SAFETY: handle 有效且只关闭一次；借用本文档的 Page 已全部析构。
        unsafe { raw::FPDF_CloseDocument(self.handle) };
    }
}
