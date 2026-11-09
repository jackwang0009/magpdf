//! 编辑相关的封装（P0 V08 "改一行字"）：文字对象读写、新建字体与文字对象、字形度量、路径对象。
//!
//! 只追加新类型和新方法，不改动已有函数的签名与行为（《工程规范》5：`pdfium-sys` 由多个 agent 共用）。
//! 用到的公共 API 都在上游 `fpdf_edit.h`：`FPDFText_SetCharcodes`、`FPDFText_SetText`、
//! `FPDFText_LoadFont`、`FPDFText_LoadCidType2Font`、`FPDFPageObj_CreateTextObj`、
//! `FPDFPageObj_CreateNewPath`/`FPDFPath_MoveTo`/`FPDFPath_LineTo`/`FPDFPath_BezierTo`/`FPDFPath_Close`、
//! `FPDFPage_InsertObjectAtIndex`、`FPDFFont_GetGlyphWidth`、`FPDFFont_GetAscent`/`Descent`。

use std::ffi::c_int;

use crate::error::{Error, Result};
use crate::font::Font;
use crate::geom::Matrix;
use crate::object::{ObjectKind, PageObject, TextRenderMode};
use crate::page::Page;
use crate::raw;

/// 新建字体时 PDFium 的字体类型：`FPDF_FONT_TRUETYPE`（TrueType 轮廓，PDF 里作 CIDFontType2）。
pub const FONT_TYPE_TRUETYPE: c_int = raw::FPDF_FONT_TRUETYPE as c_int;
/// 新建字体时 PDFium 的字体类型：`FPDF_FONT_TYPE1`（CFF 轮廓同样被接受，PDF 里作 CIDFontType0）。
pub const FONT_TYPE_TYPE1: c_int = raw::FPDF_FONT_TYPE1 as c_int;

/// `FPDFText_LoadFont` / `FPDFText_LoadCidType2Font` 返回的字体句柄，调用方拥有。
///
/// 它已经登记在文档里（字体字典是文档的间接对象）；新建文字对象时交给 PDFium，
/// 用完后由 [`Drop`] 调 `FPDFFont_Close` 释放引用。
#[derive(Debug)]
pub struct LoadedFont<'doc> {
    handle: raw::FPDF_FONT,
    _doc: &'doc (),
}

impl<'doc> LoadedFont<'doc> {
    /// 整个字体文件作为复合字体（Type0 + Identity-H 字符码）加载。
    ///
    /// PDFium 内部（`LoadCompositeFont`）按字体的 cmap 生成 ToUnicode、按 GID 作为 CID 并生成 W 数组。
    /// **它嵌入传入的全部字体数据**，调用方必须先子集化。
    pub fn load_composite(doc: &'doc crate::Document<'_>, data: &[u8], cff: bool) -> Result<Self> {
        let Ok(size) = u32::try_from(data.len()) else {
            return Err(Error::PageObject);
        };
        let font_type = if cff {
            FONT_TYPE_TYPE1
        } else {
            FONT_TYPE_TRUETYPE
        };
        // SAFETY: doc 的句柄有效；data 在调用期间有效且长度与 size 一致；PDFium 会复制字体数据。
        let handle =
            unsafe { raw::FPDFText_LoadFont(doc.raw_handle(), data.as_ptr(), size, font_type, 1) };
        if handle.is_null() {
            return Err(Error::PageObject);
        }
        Ok(Self { handle, _doc: &() })
    }

    /// 作为 CIDFontType2 加载，自带 ToUnicode CMap 与 CIDToGIDMap（`LoadCustomCompositeFont`）。
    ///
    /// PDFium 按 CIDToGIDMap 的每一项生成 W 数组；字符码 = CID，取字形时查 CIDToGIDMap。
    /// `to_unicode` 会被补上结尾 NUL。CFF 字体经这条路加载后会被当作 TrueType 描述，不要用于 CFF。
    pub fn load_cid_type2(
        doc: &'doc crate::Document<'_>,
        data: &[u8],
        to_unicode: &[u8],
        cid_to_gid_map: &[u8],
    ) -> Result<Self> {
        let (Ok(size), Ok(map_size)) = (
            u32::try_from(data.len()),
            u32::try_from(cid_to_gid_map.len()),
        ) else {
            return Err(Error::PageObject);
        };
        let mut cmap = to_unicode.to_vec();
        if cmap.last() != Some(&0) {
            cmap.push(0);
        }
        // SAFETY: doc 有效；data、cmap、cid_to_gid_map 在调用期间有效，长度与参数一致。
        let handle = unsafe {
            raw::FPDFText_LoadCidType2Font(
                doc.raw_handle(),
                data.as_ptr(),
                size,
                cmap.as_ptr().cast(),
                cid_to_gid_map.as_ptr(),
                map_size,
            )
        };
        if handle.is_null() {
            return Err(Error::PageObject);
        }
        Ok(Self { handle, _doc: &() })
    }
}

impl Drop for LoadedFont<'_> {
    fn drop(&mut self) {
        // SAFETY: handle 由本类型拥有，只关闭一次；文档的寿命覆盖本类型（借用关系）。
        unsafe { raw::FPDFFont_Close(self.handle) }
    }
}

impl PageObject<'_> {
    /// 设置字符码序列（`FPDFText_SetCharcodes`）。
    ///
    /// 每个字符码的字节宽度由字体决定：Type0/Identity-H 下 PDFium 按 2 字节写（一个 CID），
    /// 简单字体按单字节写。PDFium 只接受字体编码里合法的字符码，非法值丢弃。
    pub fn set_charcodes(&self, charcodes: &[u32]) -> bool {
        // SAFETY: handle 是有效对象；charcodes 在调用期间有效。
        unsafe {
            raw::FPDFText_SetCharcodes(self.raw_handle(), charcodes.as_ptr(), charcodes.len()) != 0
        }
    }

    /// 按 Unicode 设置文字（`FPDFText_SetText`），PDFium 用字体的 ToUnicode/cmap 反查字符码。
    pub fn set_text(&self, text: &str) -> Result<()> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: handle 有效；wide 以 0 结尾且在调用期间有效。
        let ok = unsafe { raw::FPDFText_SetText(self.raw_handle(), wide.as_ptr()) };
        if ok == 0 {
            Err(Error::PageObject)
        } else {
            Ok(())
        }
    }

    /// 文字对象的字号（`FPDFTextObj_GetFontSize`）。
    pub fn font_size(&self) -> Option<f32> {
        let mut size = 0f32;
        // SAFETY: handle 有效，size 是有效的输出位置。
        let ok = unsafe { raw::FPDFTextObj_GetFontSize(self.raw_handle(), &mut size) };
        (ok != 0).then_some(size)
    }

    /// 设置字号（`FPDFTextObj_SetFontSize`）。上游已有，不需要 `MAG_` 扩展。
    pub fn set_font_size(&self, size: f32) -> Result<()> {
        // SAFETY: handle 有效。
        let ok = unsafe { raw::FPDFTextObj_SetFontSize(self.raw_handle(), size) };
        if ok == 0 {
            Err(Error::PageObject)
        } else {
            Ok(())
        }
    }

    /// 设置渲染模式（`FPDFTextObj_SetTextRenderMode`）。
    pub fn set_text_render_mode(&self, mode: TextRenderMode) -> Result<()> {
        let raw_mode = match mode {
            TextRenderMode::Fill => raw::FPDF_TEXTRENDERMODE_FILL,
            TextRenderMode::Stroke => raw::FPDF_TEXTRENDERMODE_STROKE,
            TextRenderMode::FillStroke => raw::FPDF_TEXTRENDERMODE_FILL_STROKE,
            TextRenderMode::Invisible => raw::FPDF_TEXTRENDERMODE_INVISIBLE,
            TextRenderMode::FillClip => raw::FPDF_TEXTRENDERMODE_FILL_CLIP,
            TextRenderMode::StrokeClip => raw::FPDF_TEXTRENDERMODE_STROKE_CLIP,
            TextRenderMode::FillStrokeClip => raw::FPDF_TEXTRENDERMODE_FILL_STROKE_CLIP,
            TextRenderMode::Clip => raw::FPDF_TEXTRENDERMODE_CLIP,
        };
        // SAFETY: handle 有效；模式是 PDFium 定义的取值。
        let ok = unsafe { raw::FPDFTextObj_SetTextRenderMode(self.raw_handle(), raw_mode) };
        if ok == 0 {
            Err(Error::PageObject)
        } else {
            Ok(())
        }
    }

    /// 新建文字对象（`FPDFPageObj_CreateTextObj`），尚未插入页面。
    pub fn new_text(
        page: &Page<'_>,
        font: &LoadedFont<'_>,
        font_size: f32,
        position: Matrix,
    ) -> Result<PageObject<'static>> {
        // SAFETY: page 所属文档与 font 的句柄有效。
        let handle = unsafe {
            raw::FPDFPageObj_CreateTextObj(page.document().raw_handle(), font.handle, font_size)
        };
        if handle.is_null() {
            return Err(Error::PageObject);
        }
        // SAFETY: handle 是新对象，尚未归页面所有；寿命由调用方保证（插入页面或销毁）。
        let obj = unsafe { PageObject::from_raw(handle) };
        obj.set_matrix(position)?;
        Ok(obj)
    }

    /// 是否文字对象。
    pub fn is_text(&self) -> bool {
        self.kind() == ObjectKind::Text
    }

    /// 新建路径对象（`FPDFPageObj_CreateNewPath`），以 `(x, y)` 为起点。
    pub fn new_path(x: f32, y: f32) -> Option<Self> {
        // SAFETY: 只创建对象，不涉及外部句柄。
        let handle = unsafe { raw::FPDFPageObj_CreateNewPath(x, y) };
        // SAFETY: handle 非空时是新对象，尚未归页面所有。
        (!handle.is_null()).then(|| unsafe { PageObject::from_raw(handle) })
    }

    /// 新子路径起点：PDFium 的公共 API 没有 `MoveTo`，这里用"闭合当前子路径 + 从 `(x, y)` 起笔"实现。
    pub fn start_subpath(&self, x: f32, y: f32) -> Result<()> {
        // SAFETY: handle 有效且是路径对象；点与上一子路径重叠，不改变外观。
        let ok = unsafe { raw::FPDFPath_Close(self.raw_handle()) };
        if ok == 0 {
            return Err(Error::PageObject);
        }
        self.line_to(x, y)
    }

    /// 直线到 `(x, y)`（`FPDFPath_LineTo`）。
    pub fn line_to(&self, x: f32, y: f32) -> Result<()> {
        // SAFETY: handle 有效且是路径对象。
        if unsafe { raw::FPDFPath_LineTo(self.raw_handle(), x, y) } == 0 {
            return Err(Error::PageObject);
        }
        Ok(())
    }

    /// 三次贝塞尔曲线（`FPDFPath_BezierTo`）。
    pub fn bezier_to(&self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) -> Result<()> {
        // SAFETY: handle 有效且是路径对象。
        let ok = unsafe { raw::FPDFPath_BezierTo(self.raw_handle(), x1, y1, x2, y2, x3, y3) };
        if ok == 0 {
            return Err(Error::PageObject);
        }
        Ok(())
    }

    /// 闭合当前子路径（`FPDFPath_Close`）。
    pub fn close_path(&self) -> Result<()> {
        // SAFETY: handle 有效且是路径对象。
        if unsafe { raw::FPDFPath_Close(self.raw_handle()) } == 0 {
            return Err(Error::PageObject);
        }
        Ok(())
    }
}

impl Font<'_> {
    /// 给定字号下的字形进距（`FPDFFont_GetGlyphWidth`；输入是该字体编码里的**字符码**，
    /// PDFium 内部先把它映射到字形）。
    pub fn glyph_width(&self, charcode: u32, font_size: f32) -> Option<f32> {
        let mut w = 0f32;
        // SAFETY: handle 有效；w 是有效的输出位置。
        let ok = unsafe {
            raw::FPDFFont_GetGlyphWidth(self.raw_font_handle(), charcode, font_size, &mut w)
        };
        (ok != 0).then_some(w)
    }

    /// ascent，单位同 `font_size`；PDFium 沿用 FreeType 的坐标，向上为负。
    pub fn ascent(&self, font_size: f32) -> Option<f32> {
        let mut v = 0f32;
        // SAFETY: handle 有效；v 是有效输出位置。
        let ok = unsafe { raw::FPDFFont_GetAscent(self.raw_font_handle(), font_size, &mut v) };
        (ok != 0).then_some(v)
    }

    /// descent，向下为正。
    pub fn descent(&self, font_size: f32) -> Option<f32> {
        let mut v = 0f32;
        // SAFETY: handle 有效；v 是有效输出位置。
        let ok = unsafe { raw::FPDFFont_GetDescent(self.raw_font_handle(), font_size, &mut v) };
        (ok != 0).then_some(v)
    }
}

impl Page<'_> {
    /// 在指定位置插入对象（`FPDFPage_InsertObjectAtIndex`）；插入后对象归页面所有。
    pub fn insert_object_at(&self, object: PageObject<'_>, index: usize) -> Result<()> {
        // SAFETY: page 与 object 的句柄有效；插入后所有权转移给页面，调用方不得再销毁。
        let ok = unsafe {
            raw::FPDFPage_InsertObjectAtIndex(self.raw_page_handle(), object.raw_handle(), index)
        };
        if ok == 0 {
            return Err(Error::PageObject);
        }
        Ok(())
    }

    /// 销毁一个尚未插入页面的对象（`FPDFPageObj_Destroy`）。
    pub fn destroy_object(&self, object: PageObject<'_>) {
        // SAFETY: object 未插入页面，所有权在调用方；只销毁一次。
        unsafe { raw::FPDFPageObj_Destroy(object.raw_handle()) }
    }
}
