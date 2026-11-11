use std::ffi::c_int;
use std::marker::PhantomData;
use std::ptr;

use crate::bitmap::Bitmap;
use crate::document::Document;
use crate::error::{Error, Result};
use crate::geom::Rect;
use crate::object::PageObject;
use crate::raw;
use crate::text::TextPage;

/// 已从页面摘下、由调用方持有的对象。丢弃时销毁；[`Page::attach`] 成功后不再销毁。
pub struct DetachedObject {
    handle: raw::FPDF_PAGEOBJECT,
}

impl std::fmt::Debug for DetachedObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetachedObject")
            .field("live", &!self.handle.is_null())
            .finish()
    }
}

impl Drop for DetachedObject {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: 句柄是摘下后尚未插入页面的对象，只销毁一次。
        unsafe { raw::FPDFPageObj_Destroy(self.handle) };
        self.handle = ptr::null_mut();
    }
}

/// 渲染标志，对应 `fpdfview.h` 的 `FPDF_*` 渲染位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderFlags(pub u32);

impl RenderFlags {
    pub const NONE: Self = Self(0);
    /// 渲染批注和表单外观。
    pub const ANNOT: Self = Self(raw::FPDF_ANNOT);
    pub const LCD_TEXT: Self = Self(raw::FPDF_LCD_TEXT);
    /// 按打印输出渲染：`/PrintState /OFF` 的图层不输出（方案 9.4、P0 计划 V16）。
    pub const PRINTING: Self = Self(raw::FPDF_PRINTING);
    pub const NO_SMOOTH_TEXT: Self = Self(raw::FPDF_RENDER_NO_SMOOTHTEXT);
    pub const NO_SMOOTH_IMAGE: Self = Self(raw::FPDF_RENDER_NO_SMOOTHIMAGE);
    pub const NO_SMOOTH_PATH: Self = Self(raw::FPDF_RENDER_NO_SMOOTHPATH);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// 渲染参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderOptions {
    /// 缩放倍数：1.0 为 72 dpi，2.0 为 144 dpi（往返测试的比对分辨率，方案 13.2）。
    pub scale: f32,
    pub flags: RenderFlags,
    /// 背景色 `0xAARRGGBB`，默认不透明白。
    pub background: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            flags: RenderFlags::NONE,
            background: 0xFFFF_FFFF,
        }
    }
}

impl RenderOptions {
    pub fn dpi(dpi: f32) -> Self {
        Self {
            scale: dpi / 72.0,
            ..Self::default()
        }
    }

    pub fn with_flags(mut self, flags: RenderFlags) -> Self {
        self.flags = flags;
        self
    }
}

/// 文档中的一页。借用 [`Document`]，析构时关闭页面。
pub struct Page<'doc> {
    handle: raw::FPDF_PAGE,
    doc: &'doc Document<'doc>,
    _marker: PhantomData<&'doc ()>,
}

impl std::fmt::Debug for Page<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}

impl<'doc> Page<'doc> {
    /// 原始页面句柄。瓦片渲染和文字对象插入都在本 crate 内使用。
    pub(crate) fn raw_handle(&self) -> raw::FPDF_PAGE {
        self.handle
    }

    /// [`raw_handle`] 的别名，编辑模块沿用这个名字。
    pub(crate) fn raw_page_handle(&self) -> raw::FPDF_PAGE {
        self.handle
    }

    /// # Safety
    /// `handle` 必须是 `FPDF_LoadPage` 刚返回、尚未关闭的页面句柄，且属于 `doc`。
    pub(crate) unsafe fn from_raw(handle: raw::FPDF_PAGE, doc: &'doc Document<'doc>) -> Self {
        Self {
            handle,
            doc,
            _marker: PhantomData,
        }
    }

    pub fn document(&self) -> &'doc Document<'doc> {
        self.doc
    }

    /// 页面宽度（点）。
    pub fn width(&self) -> f32 {
        // SAFETY: handle 有效。
        unsafe { raw::FPDF_GetPageWidthF(self.handle) }
    }

    /// 页面高度（点）。
    pub fn height(&self) -> f32 {
        // SAFETY: handle 有效。
        unsafe { raw::FPDF_GetPageHeightF(self.handle) }
    }

    /// 页面旋转：0、1、2、3 分别表示 0°、90°、180°、270°。
    pub fn rotation(&self) -> i32 {
        // SAFETY: handle 有效。
        unsafe { raw::FPDFPage_GetRotation(self.handle) }
    }

    /// 页面的可见区域（PDF 用户空间，未经旋转）：CropBox 与 MediaBox 的交集，角点已经按大小排好。
    /// 对象坐标换算到显示坐标要用它的原点和页面旋转，[`Page::width`]、[`Page::height`] 是换算之后的尺寸。
    pub fn bounding_box(&self) -> Option<Rect> {
        let mut rect = raw::FS_RECTF {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        // SAFETY: handle 有效，rect 指向一个可写的 FS_RECTF。
        let ok = unsafe { raw::FPDF_GetPageBoundingBox(self.handle, &mut rect) };
        (ok != 0).then(|| Rect::from_fs(rect))
    }

    /// 页面是否带透明（会影响像素比对的背景处理）。
    pub fn has_transparency(&self) -> bool {
        // SAFETY: handle 有效。
        unsafe { raw::FPDFPage_HasTransparency(self.handle) != 0 }
    }

    /// 渲染整页为 BGRA 位图。像素尺寸为页面点数乘以 `scale`，四舍五入后至少 1 像素。
    pub fn render(&self, options: RenderOptions) -> Result<Bitmap> {
        let width = (self.width() * options.scale).round().max(1.0) as u32;
        let height = (self.height() * options.scale).round().max(1.0) as u32;
        self.render_to_size(width, height, options.flags, options.background)
    }

    /// 渲染整页到指定像素尺寸。
    pub fn render_to_size(
        &self,
        width: u32,
        height: u32,
        flags: RenderFlags,
        background: u32,
    ) -> Result<Bitmap> {
        let (w, h) = match (c_int::try_from(width), c_int::try_from(height)) {
            (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
            _ => return Err(Error::InvalidSize { width, height }),
        };
        let format = c_int::try_from(raw::FPDFBitmap_BGRA).unwrap_or(4);
        // SAFETY: 尺寸为正；first_scan 为 NULL 时 PDFium 自己分配缓冲区，stride 传 0 由它计算。
        let bitmap = unsafe { raw::FPDFBitmap_CreateEx(w, h, format, ptr::null_mut(), 0) };
        if bitmap.is_null() {
            return Err(Error::Bitmap { width, height });
        }
        // SAFETY: bitmap 与 page 均有效；矩形覆盖整个位图；flags 为 PDFium 定义的位组合。
        let data = unsafe {
            raw::FPDFBitmap_FillRect(bitmap, 0, 0, w, h, background);
            raw::FPDF_RenderPageBitmap(
                bitmap,
                self.handle,
                0,
                0,
                w,
                h,
                0,
                c_int::try_from(flags.0).unwrap_or(0),
            );
            let stride = usize::try_from(raw::FPDFBitmap_GetStride(bitmap)).unwrap_or(0);
            let buffer = raw::FPDFBitmap_GetBuffer(bitmap).cast::<u8>();
            let len = stride * height as usize;
            // buffer 指向 stride*height 字节的有效内存，在 Destroy 之前一直有效；这里整块复制。
            let data = std::slice::from_raw_parts(buffer, len).to_vec();
            raw::FPDFBitmap_Destroy(bitmap);
            (stride, data)
        };
        Ok(Bitmap::new(width, height, data.0, data.1))
    }

    /// 页面顶层对象数量（Form XObject 内部的对象不计，见 [`PageObject::form_object_count`]）。
    pub fn object_count(&self) -> usize {
        // SAFETY: handle 有效。
        let n = unsafe { raw::FPDFPage_CountObjects(self.handle) };
        usize::try_from(n).unwrap_or(0)
    }

    /// 取第 `index` 个顶层对象。
    pub fn object(&self, index: usize) -> Option<PageObject<'_>> {
        let idx = c_int::try_from(index).ok()?;
        // SAFETY: handle 有效；越界时 PDFium 返回 NULL。
        let obj = unsafe { raw::FPDFPage_GetObject(self.handle, idx) };
        // SAFETY: obj 非空时是归页面所有的有效对象句柄，寿命不超过页面。
        (!obj.is_null()).then(|| unsafe { PageObject::from_raw(obj) })
    }

    /// 遍历顶层对象。
    pub fn objects(&self) -> impl Iterator<Item = PageObject<'_>> + '_ {
        (0..self.object_count()).filter_map(move |i| self.object(i))
    }

    /// 把页面对象重新写成内容流（`FPDFPage_GenerateContent`）。
    ///
    /// 只有被标记为已修改的对象所在的内容流会被重写，这正是 P0 往返测试（V02、V03）要检验的生成器；
    /// 调用后还需 [`Document::save_to_vec`] 才会落盘。
    pub fn generate_content(&self) -> Result<()> {
        // SAFETY: handle 有效。
        let ok = unsafe { raw::FPDFPage_GenerateContent(self.handle) };
        if ok == 0 {
            Err(Error::GenerateContent)
        } else {
            Ok(())
        }
    }

    /// 从页面摘下对象，所有权交给调用方。撤销删除时用 [`Page::attach`] 放回原位（方案 4.2）。
    ///
    /// 摘下后对象不再属于页面；[`DetachedObject`] 在丢弃时销毁它。
    pub fn detach_at(&self, index: usize) -> Result<DetachedObject> {
        let Some(object) = self.object(index) else {
            return Err(Error::PageObject);
        };
        let handle = object.raw_handle();
        // SAFETY: handle 属于本页面；成功后所有权转给 DetachedObject，页面不再释放它。
        let ok = unsafe { raw::FPDFPage_RemoveObject(self.handle, handle) };
        if ok == 0 {
            return Err(Error::PageObject);
        }
        Ok(DetachedObject { handle })
    }

    /// 把摘下的对象插回 `index`。成功后 `object` 不再拥有句柄。
    pub fn attach(&self, object: &mut DetachedObject, index: usize) -> Result<()> {
        if object.handle.is_null() {
            return Err(Error::PageObject);
        }
        // SAFETY: object 是本进程摘下、尚未销毁的对象；插入成功后归页面所有。
        let ok = unsafe { raw::FPDFPage_InsertObjectAtIndex(self.handle, object.handle, index) };
        if ok == 0 {
            return Err(Error::PageObject);
        }
        object.handle = ptr::null_mut();
        Ok(())
    }

    /// 从页面移除对象并销毁它。移除文字对象后，本页已有的 [`TextPage`] 失效，需重新加载。
    pub fn remove_object(&self, object: PageObject<'_>) -> Result<()> {
        let obj = object.raw_handle();
        // SAFETY: obj 属于本页面；移除成功后所有权转给调用方，随即销毁，之后不再使用。
        let ok = unsafe { raw::FPDFPage_RemoveObject(self.handle, obj) };
        if ok == 0 {
            return Err(Error::PageObject);
        }
        // SAFETY: obj 已从页面摘下，由我们负责释放。
        unsafe { raw::FPDFPageObj_Destroy(obj) };
        Ok(())
    }

    /// 加载文字页，用于字符级读取（方案 5.1 文字块识别的输入）。
    pub fn text(&self) -> Result<TextPage<'_>> {
        // SAFETY: handle 有效。
        let handle = unsafe { raw::FPDFText_LoadPage(self.handle) };
        if handle.is_null() {
            return Err(Error::LoadTextPage);
        }
        // SAFETY: handle 是刚返回的有效文字页句柄，归 TextPage 所有。
        Ok(unsafe { TextPage::from_raw(handle) })
    }
}

impl Drop for Page<'_> {
    fn drop(&mut self) {
        // SAFETY: handle 有效且只关闭一次；借用本页的对象与文字页已全部析构。
        unsafe { raw::FPDF_ClosePage(self.handle) };
    }
}
