//! 按页面坐标矩形渲染一块"瓦片"（V06 进程架构原型，方案 7 的 512×512 瓦片）。
//!
//! [`Page::render_to_size`](crate::Page::render_to_size) 只能渲染整页；瓦片要求只渲染页面上的一个区域，
//! 并且按设备像素对齐。PDFium 的 `FPDF_RenderPageBitmap` 用 (start_x, start_y, size_x, size_y, rotate) 指定
//! 整页在输出位图坐标系里占的矩形，位图只留下和它相交的部分：把整页往左上方挪到 (-x, -y)，
//! 位图里剩下的正好是整页上从 (x, y) 开始的那一块。

use std::ffi::c_int;

use crate::bitmap::Bitmap;
use crate::error::{Error, Result};
use crate::page::{Page, RenderFlags};
use crate::raw;

/// 瓦片像素尺寸：方案 7 规定 512×512 设备像素。
pub const TILE_SIZE: u32 = 512;

/// 整页渲染参数：页面在给定缩放下的设备像素尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSurface {
    /// 整页像素宽度（含页面旋转后的显示方向）。
    pub width: u32,
    /// 整页像素高度。
    pub height: u32,
}

impl PageSurface {
    /// 按缩放倍数计算整页像素尺寸，至少 1 像素。
    ///
    /// [`Page::width`] 和 [`Page::height`] 已经是按页面旋转换好的显示尺寸，这里不用再换。
    pub fn new(page: &Page<'_>, scale: f32) -> Self {
        Self {
            width: (page.width() * scale).round().max(1.0) as u32,
            height: (page.height() * scale).round().max(1.0) as u32,
        }
    }
}

/// 整页像素网格上的一块矩形：左上角 `(x, y)`，大小 `width × height`，单位是设备像素。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    /// 左上角在 `(x, y)` 的一块标准瓦片（[`TILE_SIZE`] 见方）。
    pub const fn tile(x: u32, y: u32) -> Self {
        Self {
            x,
            y,
            width: TILE_SIZE,
            height: TILE_SIZE,
        }
    }
}

/// 从整页上取 `rect` 这一块像素，BGRA、自顶向下。
///
/// 超出整页的部分保持 `background` 色（`0xAARRGGBB`，通常是白色），瓦片边缘由此补齐到完整尺寸。
/// 返回的位图宽高总是等于请求的 `rect.width × rect.height`。
pub fn render_rect(
    page: &Page<'_>,
    surface: PageSurface,
    rect: PixelRect,
    flags: RenderFlags,
    background: u32,
) -> Result<Bitmap> {
    let PixelRect {
        x,
        y,
        width,
        height,
    } = rect;
    let (w, h) = match (c_int::try_from(width), c_int::try_from(height)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
        _ => return Err(Error::InvalidSize { width, height }),
    };
    // start_x/start_y 是"整页的左上角在输出位图里的位置"。瓦片从整页的 (x, y) 开始，
    // 所以整页要往左上方挪 (x, y)，传进去的是负数。
    let (sx, sy) = match (c_int::try_from(x), c_int::try_from(y)) {
        (Ok(x), Ok(y)) => (-x, -y),
        _ => {
            return Err(Error::InvalidSize {
                width: x,
                height: y,
            });
        }
    };
    let (sw, sh) = match (
        c_int::try_from(surface.width),
        c_int::try_from(surface.height),
    ) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
        _ => {
            return Err(Error::InvalidSize {
                width: surface.width,
                height: surface.height,
            });
        }
    };
    let format = c_int::try_from(raw::FPDFBitmap_BGRA).unwrap_or(4);
    // SAFETY: 尺寸为正；first_scan 为 NULL 时 PDFium 自己分配缓冲区，stride 传 0 由它计算。
    let bitmap = unsafe { raw::FPDFBitmap_CreateEx(w, h, format, std::ptr::null_mut(), 0) };
    if bitmap.is_null() {
        return Err(Error::Bitmap { width, height });
    }
    // SAFETY: bitmap 与 page 均有效；整页画在输出位图坐标系的 (sx, sy)–(sx+sw, sy+sh) 上，落在位图之外的
    // 部分由 PDFium 裁掉；flags 为 PDFium 定义的位组合。读取缓冲区后立即销毁位图，数据已复制出来。
    let data = unsafe {
        raw::FPDFBitmap_FillRect(bitmap, 0, 0, w, h, background);
        raw::FPDF_RenderPageBitmap(
            bitmap,
            page.raw_handle(),
            sx,
            sy,
            sw,
            sh,
            0,
            flags.0 as c_int,
        );
        let stride = usize::try_from(raw::FPDFBitmap_GetStride(bitmap)).unwrap_or(0);
        let buffer = raw::FPDFBitmap_GetBuffer(bitmap).cast::<u8>();
        let len = stride * height as usize;
        let data = std::slice::from_raw_parts(buffer, len).to_vec();
        raw::FPDFBitmap_Destroy(bitmap);
        (stride, data)
    };
    Ok(Bitmap::new(width, height, data.0, data.1))
}

/// 分层渲染时只画页面上的哪些对象（拖动预览，方案 3.2）。对象用页内层级序号指代，0 在最下面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerFilter<'a> {
    /// 去掉这些对象，其余照常画：拖动时的底图。
    Without(&'a [usize]),
    /// 只画这些对象：拖动时跟着鼠标走的那一层。背景通常给全透明（`0x0000_0000`）。
    Only(&'a [usize]),
}

/// 和 [`render_rect`] 一样取整页上的一块，但只画 `filter` 留下的对象。
///
/// 背景给全透明时，位图里是**没有预乘**的 BGRA：半透明的像素，颜色分量是对象本来的颜色，透明度单独在
/// 第四个字节里（`tests/layers.rs` 里有用例守着这一点）。
///
/// 用的是 PDFium 的公开接口，没有改分支：渲染前把不要的对象设成“不活动”（`FPDFPageObj_SetIsActive`），
/// 渲染完设回来。活动状态一变，PDFium 会给对象打上“已修改”的标记，这个标记记在页面句柄里的对象上：
///
/// - 页面句柄不重新生成内容流就关掉，什么也不会留下，文档没有变；
/// - 之后如果在**同一个页面句柄**上调 [`Page::generate_content`]，被隐藏过的对象所在的内容流会被重写，
///   即使这些对象并没有改过。不要这样用。
///
/// 所以页面一直开着的时候（[`crate::Document::keep_page`]），分层渲染要用 [`crate::Document::load_page`]
/// 另开一份句柄，画完关掉，别在一直开着的那一份上画。方案 3.2 设想的 `MAG_RenderPageFiltered`
/// （《接口协议》9.3）连这个标记也不会留下，能省掉另开的这一份，但要改 PDFium 分支。
///
/// 序号超出范围返回 [`Error::PageObject`]，这时什么也不画，对象的状态也不变。
pub fn render_rect_filtered(
    page: &Page<'_>,
    surface: PageSurface,
    rect: PixelRect,
    flags: RenderFlags,
    background: u32,
    filter: LayerFilter<'_>,
) -> Result<Bitmap> {
    let hidden = Hidden::apply(page, filter)?;
    let bitmap = render_rect(page, surface, rect, flags, background);
    drop(hidden);
    bitmap
}

/// 暂时不参与渲染的对象。析构时设回“活动”，中途出错也一样。
struct Hidden<'page> {
    objects: Vec<raw::FPDF_PAGEOBJECT>,
    /// 句柄属于这一页，页面要比它们活得久
    _page: std::marker::PhantomData<&'page Page<'page>>,
}

impl<'page> Hidden<'page> {
    fn apply(page: &'page Page<'_>, filter: LayerFilter<'_>) -> Result<Self> {
        let count = page.object_count();
        let (listed, hide_listed) = match filter {
            LayerFilter::Without(indices) => (indices, true),
            LayerFilter::Only(indices) => (indices, false),
        };
        let mut is_listed = vec![false; count];
        for &index in listed {
            *is_listed.get_mut(index).ok_or(Error::PageObject)? = true;
        }
        let mut hidden = Self {
            objects: Vec::new(),
            _page: std::marker::PhantomData,
        };
        for (index, listed) in is_listed.into_iter().enumerate() {
            if listed != hide_listed {
                continue;
            }
            let Some(object) = page.object(index) else {
                continue;
            };
            let handle = object.raw_handle();
            let mut active: raw::FPDF_BOOL = 0;
            // SAFETY: handle 是这一页上有效的对象句柄；active 指向栈上的变量。
            let known = unsafe { raw::FPDFPageObj_GetIsActive(handle, &raw mut active) };
            // 本来就不活动的对象不归这里管，渲染完也不去动它
            if known == 0 || active == 0 {
                continue;
            }
            // SAFETY: 同上。失败时提前返回，已经隐藏的对象由 `hidden` 析构时恢复。
            if unsafe { raw::FPDFPageObj_SetIsActive(handle, 0) } == 0 {
                return Err(Error::PageObject);
            }
            hidden.objects.push(handle);
        }
        Ok(hidden)
    }
}

impl Drop for Hidden<'_> {
    fn drop(&mut self) {
        for &handle in &self.objects {
            // SAFETY: 句柄在 `apply` 里取自仍然活着的页面，这期间没有对象被移除。
            unsafe { raw::FPDFPageObj_SetIsActive(handle, 1) };
        }
    }
}
