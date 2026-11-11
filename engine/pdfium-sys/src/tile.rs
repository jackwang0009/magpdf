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
