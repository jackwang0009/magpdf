use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_int, c_void};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr;
use std::sync::Arc;

use crate::error::{Error, LoadError, Result};
use crate::geom::{Matrix, Rect};
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

impl PageGeometry {
    /// PDF 用户空间到显示坐标的矩阵。
    ///
    /// 显示坐标是页面显示出来的样子：左上角为原点，x 向右、y 向下，单位 pt，范围 `0..width × 0..height`。
    /// 页面的 `/Rotate` 和可见区域不从原点开始这两件事都算在里面，和 PDFium 渲染时用的换算一致
    /// （`CPDF_Page` 的页面矩阵再翻转 y 轴）。记可见区域为 `[l, b, r, t]`：
    ///
    /// | `/Rotate` | 显示坐标 |
    /// |---|---|
    /// | 0 | `(x − l, t − y)` |
    /// | 90 | `(y − b, x − l)` |
    /// | 180 | `(r − x, y − b)` |
    /// | 270 | `(t − y, r − x)` |
    ///
    /// 界面把对象的包围盒换算到画布上用的是同一张表（`packages/editor` 的 `pageSpace.ts`），两边要一起改。
    /// 拿不到可见区域时返回 `None`。
    pub fn display_matrix(&self) -> Option<Matrix> {
        let Rect {
            left,
            bottom,
            right,
            top,
        } = self.bounds?;
        let (a, b, c, d, e, f) = match self.rotation.rem_euclid(4) {
            0 => (1.0, 0.0, 0.0, -1.0, -left, top),
            1 => (0.0, 1.0, 1.0, 0.0, -bottom, -left),
            2 => (-1.0, 0.0, 0.0, 1.0, right, -bottom),
            _ => (0.0, -1.0, -1.0, 0.0, top, right),
        };
        Some(Matrix { a, b, c, d, e, f })
    }
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
    /// 文档是从这块数据打开的。PDFium 只读它，从不改写：打开之后的修改都记在 PDFium 自己的对象里。
    /// 所以它可以和 [`Document::reopen`] 得到的文档共用；新建的文档没有数据，这里是空的。
    data: Arc<Vec<u8>>,
    /// 一直开着的页面（[`Document::keep_page`]）。
    kept: RefCell<Vec<KeptPage>>,
    /// 给 [`KeptPage::used`] 编号：越大越是最近用过的。
    uses: Cell<u64>,
    _lib: PhantomData<&'lib Library>,
}

/// 一直开着的一页。句柄归文档，[`Document::release_pages`] 或关文档时才关闭。
struct KeptPage {
    index: usize,
    handle: raw::FPDF_PAGE,
    used: u64,
}

impl std::fmt::Debug for Document<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("bytes", &self.data.len())
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
    pub fn load_bytes(lib: &'lib Library, data: Vec<u8>, password: Option<&str>) -> Result<Self> {
        Self::load_shared(lib, Arc::new(data), password)
    }

    fn load_shared(
        _lib: &'lib Library,
        data: Arc<Vec<u8>>,
        password: Option<&str>,
    ) -> Result<Self> {
        let password_c = password.map(CString::new).transpose()?;
        let password_ptr = password_c.as_ref().map_or(ptr::null(), |p| p.as_ptr());
        // SAFETY: data 的缓冲区由返回的 Document 持有（共用时每份文档各拿着一个引用计数），寿命覆盖整个
        // 文档句柄；PDFium 把它当只读的内存文件用，几份文档同时读同一块数据没有问题；
        // password 为有效 C 字符串或 NULL。
        let handle = unsafe {
            raw::FPDF_LoadMemDocument64(data.as_ptr().cast::<c_void>(), data.len(), password_ptr)
        };
        if handle.is_null() {
            return Err(Error::Load(LoadError::last()));
        }
        Ok(Self::wrap(handle, data))
    }

    fn wrap(handle: raw::FPDF_DOCUMENT, data: Arc<Vec<u8>>) -> Self {
        Self {
            handle,
            data,
            kept: RefCell::new(Vec::new()),
            uses: Cell::new(0),
            _lib: PhantomData,
        }
    }

    /// 把这份文档打开时用的数据再打开一遍，得到一份互不相干的文档：它是文件原来的样子，不含打开之后
    /// 做过的任何修改。两份文档共用同一块只读数据，不复制，所以文件再大也只花解析的时间。
    ///
    /// 新建的文档（[`Document::create`]）没有数据，返回 `None`。
    pub fn reopen(&self, lib: &'lib Library, password: Option<&str>) -> Option<Result<Self>> {
        if self.data.is_empty() {
            return None;
        }
        Some(Self::load_shared(lib, Arc::clone(&self.data), password))
    }

    /// 新建一份空文档（还没有页面）。
    pub fn create(_lib: &'lib Library) -> Result<Self> {
        // SAFETY: 库已初始化；返回的文档句柄归 Document 所有。
        let handle = unsafe { raw::FPDF_CreateNewDocument() };
        if handle.is_null() {
            return Err(Error::Load(LoadError::last()));
        }
        Ok(Self::wrap(handle, Arc::new(Vec::new())))
    }

    /// 插入一张空白页，尺寸单位是点。`index` 可以等于当前页数，表示追加。
    /// 一直开着的页面跟着往后挪，仍然对得上各自的页序号。
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
        for page in self.kept.borrow_mut().iter_mut() {
            if page.index >= index {
                page.index += 1;
            }
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

    /// 第 `index` 页的显示尺寸（点，已按页面旋转换好宽高）。不加载页面，也不解析内容流，
    /// 打开大文档时用它给全部页面排版。
    pub fn page_size(&self, index: usize) -> Result<(f32, f32)> {
        let count = self.page_count();
        if index >= count {
            return Err(Error::PageIndex { index, count });
        }
        let idx = c_int::try_from(index).map_err(|_| Error::PageIndex { index, count })?;
        let mut size = raw::FS_SIZEF {
            width: 0.0,
            height: 0.0,
        };
        // SAFETY: handle 有效，idx 已检查在范围内，size 是有效的输出位置。
        let ok = unsafe { raw::FPDF_GetPageSizeByIndexF(self.handle, idx, &mut size) };
        if ok == 0 {
            return Err(Error::LoadPage(LoadError::last()));
        }
        Ok((size.width, size.height))
    }

    /// 文件头声明的 PDF 版本 ×10，例如 1.7 返回 17。
    pub fn file_version(&self) -> Option<i32> {
        let mut v: c_int = 0;
        // SAFETY: handle 有效，v 是有效的输出位置。
        let ok = unsafe { raw::FPDF_GetFileVersion(self.handle, &mut v) };
        (ok != 0).then_some(v)
    }

    /// 第 `index` 页（从 0 开始）。这一页一直开着的话（[`Document::keep_page`]），给的就是那一份：在上面做的
    /// 修改别处都看得到，返回值丢掉也不会关闭它。没有开着就加载一份，丢掉时关闭。
    pub fn page(&self, index: usize) -> Result<Page<'_>> {
        if let Some(handle) = self.kept_handle(index) {
            // SAFETY: handle 是本文档一直开着的页面；要 `&mut self`（`release_pages`）或关文档才会关闭它，
            // 返回的 Page 借着 `&self`，所以在它用完之前句柄一直有效。
            return Ok(unsafe { Page::kept(handle, self) });
        }
        self.load_page(index)
    }

    /// 让第 `index` 页一直开着，返回它。之后 [`Document::page`] 给的都是这一份，渲染、列对象、修改都不用
    /// 再解析一遍内容流（方案第 7 节：可见页和有未保存修改的页常驻内存）。用 [`Document::release_pages`] 关掉。
    pub fn keep_page(&self, index: usize) -> Result<Page<'_>> {
        if let Some(handle) = self.kept_handle(index) {
            // SAFETY: 同 `page`。
            return Ok(unsafe { Page::kept(handle, self) });
        }
        let handle = self.load_page(index)?.into_raw();
        let used = self.next_use();
        self.kept.borrow_mut().push(KeptPage {
            index,
            handle,
            used,
        });
        // SAFETY: handle 刚交给本文档保管，同 `page`。
        Ok(unsafe { Page::kept(handle, self) })
    }

    /// 一直开着的页面只留最近用过的 `keep` 页，其余关掉。关掉的页下次用到时重新加载：内存里没有生成内容流的
    /// 修改会跟着丢掉，所以改完页面要先 [`Page::generate_content`]。
    pub fn release_pages(&mut self, keep: usize) {
        let kept = self.kept.get_mut();
        if kept.len() <= keep {
            return;
        }
        kept.sort_by_key(|page| std::cmp::Reverse(page.used));
        for page in kept.drain(keep..) {
            // SAFETY: 句柄归本文档保管；`&mut self` 保证没有借着它的 Page 还活着。
            unsafe { raw::FPDF_ClosePage(page.handle) };
        }
    }

    /// 一直开着的是哪几页，按页序号从小到大。
    pub fn kept_pages(&self) -> Vec<usize> {
        let mut pages: Vec<usize> = self.kept.borrow().iter().map(|page| page.index).collect();
        pages.sort_unstable();
        pages
    }

    /// 一直开着的第 `index` 页的句柄，顺便记下用了一次。
    fn kept_handle(&self, index: usize) -> Option<raw::FPDF_PAGE> {
        let used = self.next_use();
        let mut kept = self.kept.borrow_mut();
        let page = kept.iter_mut().find(|page| page.index == index)?;
        page.used = used;
        Some(page.handle)
    }

    fn next_use(&self) -> u64 {
        let used = self.uses.get() + 1;
        self.uses.set(used);
        used
    }

    /// 另外加载一份第 `index` 页，不管它是不是一直开着。这一份和别的句柄互不相干：在上面做的事（比如分层渲染时
    /// 暂时隐藏对象，见 [`crate::render_rect_filtered`]）一直开着的那一份看不到，丢掉时关闭。
    pub fn load_page(&self, index: usize) -> Result<Page<'_>> {
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

    /// 页面尺寸、旋转和可见区域。要加载页面（`FPDF_LoadPage` 会解析内容流），读完立即关闭；
    /// 只要显示尺寸时用 [`Document::page_size`]，它不加载页面。
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
        for page in self.kept.get_mut().drain(..) {
            // SAFETY: 一直开着的页面归本文档保管，借着它的 Page 已全部析构；页面要先于文档关闭。
            unsafe { raw::FPDF_ClosePage(page.handle) };
        }
        // SAFETY: handle 有效且只关闭一次；借用本文档的 Page 已全部析构。
        unsafe { raw::FPDF_CloseDocument(self.handle) };
    }
}
