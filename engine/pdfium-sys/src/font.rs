use std::ffi::c_char;
use std::marker::PhantomData;
use std::ptr;

use crate::raw;

/// 文字对象引用的字体（`FPDF_FONT`）。
///
/// 由 [`PageObject::font`](crate::PageObject::font) 取得，是 PDFium 内部持有的非拥有引用，
/// 寿命不超过所属的文字对象，不能也不需要 `FPDFFont_Close`。
#[derive(Clone, Copy)]
pub struct Font<'obj> {
    handle: raw::FPDF_FONT,
    _marker: PhantomData<&'obj ()>,
}

impl std::fmt::Debug for Font<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font")
            .field("base_name", &self.base_name())
            .field("embedded", &self.is_embedded())
            .field("type3", &self.is_type3())
            .finish()
    }
}

impl Font<'_> {
    /// # Safety
    /// `handle` 必须是 `FPDFTextObj_GetFont` 返回的非空句柄，且其所属文字对象仍然存活。
    pub(crate) unsafe fn from_raw(handle: raw::FPDF_FONT) -> Self {
        Self {
            handle,
            _marker: PhantomData,
        }
    }

    /// 字体字典的 `BaseFont` 名；Type3 字体通常没有，返回空串。
    pub fn base_name(&self) -> String {
        // SAFETY: handle 有效；buffer 为 NULL、length 为 0 时只查询所需长度（含结尾 NUL）。
        let needed = unsafe { raw::FPDFFont_GetBaseFontName(self.handle, ptr::null_mut(), 0) };
        if needed == 0 {
            return String::new();
        }
        let mut buf = vec![0u8; needed];
        // SAFETY: buf 的长度正是上一次调用返回的所需字节数。
        let written = unsafe {
            raw::FPDFFont_GetBaseFontName(self.handle, buf.as_mut_ptr().cast::<c_char>(), buf.len())
        };
        if written == 0 {
            return String::new();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf.truncate(end);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// 字体描述符的 Flags（ISO 32000-1 表 123），取不到时返回 `None`。
    pub fn flags(&self) -> Option<i32> {
        // SAFETY: handle 有效。
        let v = unsafe { raw::FPDFFont_GetFlags(self.handle) };
        (v >= 0).then_some(v)
    }

    /// 字体是否嵌入。注意 PDFium 对 Type3 字体恒返回 `true`（它没有外部字体程序可言）。
    pub fn is_embedded(&self) -> Option<bool> {
        // SAFETY: handle 有效。
        let v = unsafe { raw::FPDFFont_GetIsEmbedded(self.handle) };
        match v {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// 字体程序的字节数：嵌入字体为其字体文件长度，未嵌入字体为 PDFium 所选替代字体的长度，
    /// Type3 字体没有字体程序，为 0。
    pub fn font_data_len(&self) -> Option<usize> {
        let mut len: usize = 0;
        // SAFETY: handle 有效；buffer 为 NULL、buflen 为 0 时只写 out_buflen。
        let ok = unsafe { raw::FPDFFont_GetFontData(self.handle, ptr::null_mut(), 0, &mut len) };
        (ok != 0).then_some(len)
    }

    /// 是否 Type3 字体。公共 API 没有直接的字体类型查询；PDFium 的 `CPDF_Font::IsEmbedded()`
    /// 对 Type3 恒为真，而 Type3 又是唯一没有字体程序的"嵌入"字体，两者结合即可判定。
    /// 方案 2.1：原版生成器遇到 Type3 文字会留下未闭合的 `q`、`BT`，是往返测试要单独统计的一类。
    pub fn is_type3(&self) -> bool {
        self.is_embedded() == Some(true) && self.font_data_len() == Some(0)
    }
}
