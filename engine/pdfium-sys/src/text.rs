use std::ffi::{c_int, c_uint, c_ulong, c_void};
use std::marker::PhantomData;

use crate::geom::{Matrix, Rect, Rgba};
use crate::object::PageObject;
use crate::raw;

/// 字符级信息，是文字块识别（方案 5.1、P0 计划 V07）的输入。
#[derive(Debug, Clone, PartialEq)]
pub struct CharInfo {
    /// Unicode 码点；PDFium 映射失败时可能是 0 或替代字符，见 [`TextPage::has_unicode_map_error`]。
    pub unicode: u32,
    /// 字形的紧包围盒（页面坐标）。
    pub bbox: Rect,
    /// 按字体度量估计的"宽松"包围盒，适合做行聚类。
    pub loose_bbox: Option<Rect>,
    /// 基线起点。
    pub origin: (f64, f64),
    pub font_size: f64,
    pub font_name: String,
    /// PDF 字体描述符的 Flags。
    pub font_flags: i32,
    pub font_weight: Option<i32>,
    /// 字符的文字矩阵（含 Tz、旋转）。
    pub matrix: Option<Matrix>,
    pub fill_color: Option<Rgba>,
    /// 是否是 PDFium 为排版插入的生成字符（空格、换行等），不是文件里真正的字形。
    pub generated: bool,
}

/// 一页的文字内容（`FPDF_TEXTPAGE`）。
pub struct TextPage<'page> {
    handle: raw::FPDF_TEXTPAGE,
    _marker: PhantomData<&'page ()>,
}

impl std::fmt::Debug for TextPage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextPage")
            .field("chars", &self.char_count())
            .finish()
    }
}

impl TextPage<'_> {
    /// # Safety
    /// `handle` 必须是 `FPDFText_LoadPage` 刚返回、尚未关闭的句柄。
    pub(crate) unsafe fn from_raw(handle: raw::FPDF_TEXTPAGE) -> Self {
        Self {
            handle,
            _marker: PhantomData,
        }
    }

    pub fn char_count(&self) -> usize {
        // SAFETY: handle 有效。
        let n = unsafe { raw::FPDFText_CountChars(self.handle) };
        usize::try_from(n).unwrap_or(0)
    }

    pub fn unicode(&self, index: usize) -> Option<u32> {
        let i = c_int::try_from(index).ok()?;
        // SAFETY: handle 有效；越界时返回 0。
        Some(unsafe { raw::FPDFText_GetUnicode(self.handle, i) })
    }

    pub fn char_box(&self, index: usize) -> Option<Rect> {
        let i = c_int::try_from(index).ok()?;
        let (mut l, mut r, mut b, mut t) = (0f64, 0f64, 0f64, 0f64);
        // SAFETY: handle 有效，输出指针有效。
        let ok =
            unsafe { raw::FPDFText_GetCharBox(self.handle, i, &mut l, &mut r, &mut b, &mut t) };
        (ok != 0).then_some(Rect {
            left: l as f32,
            bottom: b as f32,
            right: r as f32,
            top: t as f32,
        })
    }

    pub fn loose_char_box(&self, index: usize) -> Option<Rect> {
        let i = c_int::try_from(index).ok()?;
        let mut rect = raw::FS_RECTF {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        // SAFETY: handle 有效，rect 是有效的输出位置。
        let ok = unsafe { raw::FPDFText_GetLooseCharBox(self.handle, i, &mut rect) };
        (ok != 0).then(|| Rect::from_fs(rect))
    }

    pub fn char_origin(&self, index: usize) -> Option<(f64, f64)> {
        let i = c_int::try_from(index).ok()?;
        let (mut x, mut y) = (0f64, 0f64);
        // SAFETY: handle 有效，输出指针有效。
        let ok = unsafe { raw::FPDFText_GetCharOrigin(self.handle, i, &mut x, &mut y) };
        (ok != 0).then_some((x, y))
    }

    pub fn font_size(&self, index: usize) -> Option<f64> {
        let i = c_int::try_from(index).ok()?;
        // SAFETY: handle 有效。
        Some(unsafe { raw::FPDFText_GetFontSize(self.handle, i) })
    }

    /// 字体名（PostScript 名）和字体描述符 Flags。
    pub fn font_info(&self, index: usize) -> Option<(String, i32)> {
        let i = c_int::try_from(index).ok()?;
        let mut flags: c_int = 0;
        // SAFETY: buffer 为 NULL、buflen 为 0 时只查询所需长度。
        let needed = unsafe {
            raw::FPDFText_GetFontInfo(self.handle, i, std::ptr::null_mut(), 0, &mut flags)
        };
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0u8; usize::try_from(needed).ok()?];
        let buflen = c_ulong::try_from(buf.len()).ok()?;
        // SAFETY: buf 的长度正是上一次调用返回的所需字节数。
        let written = unsafe {
            raw::FPDFText_GetFontInfo(
                self.handle,
                i,
                buf.as_mut_ptr().cast::<c_void>(),
                buflen,
                &mut flags,
            )
        };
        if written == 0 {
            return None;
        }
        // 返回值含结尾的 NUL。
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf.truncate(end);
        Some((String::from_utf8_lossy(&buf).into_owned(), flags))
    }

    pub fn font_weight(&self, index: usize) -> Option<i32> {
        let i = c_int::try_from(index).ok()?;
        // SAFETY: handle 有效。
        let w = unsafe { raw::FPDFText_GetFontWeight(self.handle, i) };
        (w >= 0).then_some(w)
    }

    pub fn char_matrix(&self, index: usize) -> Option<Matrix> {
        let i = c_int::try_from(index).ok()?;
        let mut m = raw::FS_MATRIX {
            a: 0.0,
            b: 0.0,
            c: 0.0,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        };
        // SAFETY: handle 有效，m 是有效的输出位置。
        let ok = unsafe { raw::FPDFText_GetMatrix(self.handle, i, &mut m) };
        (ok != 0).then(|| Matrix::from_fs(m))
    }

    pub fn fill_color(&self, index: usize) -> Option<Rgba> {
        let i = c_int::try_from(index).ok()?;
        let (mut r, mut g, mut b, mut a): (c_uint, c_uint, c_uint, c_uint) = (0, 0, 0, 0);
        // SAFETY: handle 有效，输出指针有效。
        let ok =
            unsafe { raw::FPDFText_GetFillColor(self.handle, i, &mut r, &mut g, &mut b, &mut a) };
        let clamp = |v: c_uint| u8::try_from(v.min(255)).unwrap_or(255);
        (ok != 0).then(|| Rgba {
            r: clamp(r),
            g: clamp(g),
            b: clamp(b),
            a: clamp(a),
        })
    }

    /// 字符是否为 PDFium 生成（不是文件里的字形）。
    pub fn is_generated(&self, index: usize) -> bool {
        let Ok(i) = c_int::try_from(index) else {
            return false;
        };
        // SAFETY: handle 有效。
        unsafe { raw::FPDFText_IsGenerated(self.handle, i) == 1 }
    }

    /// 字符的 Unicode 映射是否有问题（无 ToUnicode 或映射失败），方案第 15 节"文字无 Unicode"的检测点。
    pub fn has_unicode_map_error(&self, index: usize) -> bool {
        let Ok(i) = c_int::try_from(index) else {
            return false;
        };
        // SAFETY: handle 有效。
        unsafe { raw::FPDFText_HasUnicodeMapError(self.handle, i) == 1 }
    }

    /// 字符所属的文字对象。
    pub fn text_object(&self, index: usize) -> Option<PageObject<'_>> {
        let i = c_int::try_from(index).ok()?;
        // SAFETY: handle 有效；失败时返回 NULL。
        let obj = unsafe { raw::FPDFText_GetTextObject(self.handle, i) };
        // SAFETY: obj 非空时归页面所有，寿命不短于文字页。
        (!obj.is_null()).then(|| unsafe { PageObject::from_raw(obj) })
    }

    /// 一次取齐某个字符的全部信息。
    pub fn char_info(&self, index: usize) -> Option<CharInfo> {
        let (font_name, font_flags) = self.font_info(index).unwrap_or_default();
        Some(CharInfo {
            unicode: self.unicode(index)?,
            bbox: self.char_box(index)?,
            loose_bbox: self.loose_char_box(index),
            origin: self.char_origin(index)?,
            font_size: self.font_size(index)?,
            font_name,
            font_flags,
            font_weight: self.font_weight(index),
            matrix: self.char_matrix(index),
            fill_color: self.fill_color(index),
            generated: self.is_generated(index),
        })
    }

    /// 遍历全部字符。
    pub fn chars(&self) -> impl Iterator<Item = CharInfo> + '_ {
        (0..self.char_count()).filter_map(move |i| self.char_info(i))
    }
}

impl Drop for TextPage<'_> {
    fn drop(&mut self) {
        // SAFETY: handle 有效且只关闭一次。
        unsafe { raw::FPDFText_ClosePage(self.handle) };
    }
}
