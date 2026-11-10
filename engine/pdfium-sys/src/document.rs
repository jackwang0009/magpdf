use std::ffi::{CString, c_int, c_void};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr;

use crate::error::{Error, LoadError, Result};
use crate::geom::Rect;
use crate::library::Library;
use crate::page::Page;
use crate::raw;

/// 页面的几何信息（尺寸、旋转、边界框），不需要把页面对象留在内存里。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeometry {
    pub index: usize,
    /// 按旋转后的显示尺寸（点）。
    pub width: f32,
    pub height: f32,
    /// 0、1、2、3 对应 0°、90°、180°、270°。
    pub rotation: i32,
    /// `FPDF_GetPageBoundingBox` 的结果（`CropBox` 与 `MediaBox` 的交集）。
    pub bounds: Option<Rect>,
}

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

    /// 新建一份空文档（还没有页面）。
    pub fn create(_lib: &'lib Library) -> Result<Self> {
        // SAFETY: 库已初始化；返回的文档句柄归 Document 所有。
        let handle = unsafe { raw::FPDF_CreateNewDocument() };
        if handle.is_null() {
            return Err(Error::Load(LoadError::last()));
        }
        Ok(Self {
            handle,
            _data: Vec::new(),
            _lib: PhantomData,
        })
    }

    /// 插入一张空白页，尺寸单位是点。`index` 可以等于当前页数，表示追加。
    pub fn add_blank_page(&self, index: usize, width: f32, height: f32) -> Result<Page<'_>> {
        if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
            return Err(Error::InvalidSize {
                width: 0,
                height: 0,
            });
        }
        let count = self.page_count();
        if index > count {
            return Err(Error::PageIndex { index, count });
        }
        let idx = c_int::try_from(index).map_err(|_| Error::PageIndex { index, count })?;
        // SAFETY: handle 有效；宽高已检查为正数。返回的页面句柄归 Page 所有。
        let handle =
            unsafe { raw::FPDFPage_New(self.handle, idx, f64::from(width), f64::from(height)) };
        if handle.is_null() {
            return Err(Error::LoadPage(LoadError::last()));
        }
        // SAFETY: handle 是刚由 FPDFPage_New 返回的有效页面句柄，归 Page 所有。
        Ok(unsafe { Page::from_raw(handle, self) })
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

    /// 页面尺寸与旋转。只加载页面读标量，读完立即关闭，不解析内容流对象。
    pub fn page_geometry(&self, page_index: usize) -> Result<PageGeometry> {
        let count = self.page_count();
        if page_index >= count {
            return Err(Error::PageIndex {
                index: page_index,
                count,
            });
        }
        let idx = c_int::try_from(page_index).map_err(|_| Error::PageIndex {
            index: page_index,
            count,
        })?;
        // SAFETY: handle 有效，idx 已检查；返回的页面句柄在读完标量后立即关闭。
        unsafe {
            let handle = raw::FPDF_LoadPage(self.handle, idx);
            if handle.is_null() {
                return Err(Error::LoadPage(LoadError::last()));
            }
            let mut box_ = raw::FS_RECTF {
                left: 0.0,
                bottom: 0.0,
                right: 0.0,
                top: 0.0,
            };
            let has_box = raw::FPDF_GetPageBoundingBox(handle, &mut box_) != 0;
            let geometry = PageGeometry {
                index: page_index,
                width: raw::FPDF_GetPageWidthF(handle),
                height: raw::FPDF_GetPageHeightF(handle),
                rotation: raw::FPDFPage_GetRotation(handle),
                bounds: has_box.then(|| Rect::from_fs(box_)),
            };
            raw::FPDF_ClosePage(handle);
            Ok(geometry)
        }
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
