use std::ffi::{c_uint, c_ulong};
use std::marker::PhantomData;

use crate::error::{Error, Result};
use crate::font::Font;
use crate::geom::{Matrix, Rect, Rgba};
use crate::raw;

/// 页面对象类型，对应 `FPDF_PAGEOBJ_*`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Text,
    Path,
    Image,
    Shading,
    Form,
    Unknown,
}

impl ObjectKind {
    fn from_raw(v: i32) -> Self {
        match u32::try_from(v).unwrap_or(u32::MAX) {
            raw::FPDF_PAGEOBJ_TEXT => Self::Text,
            raw::FPDF_PAGEOBJ_PATH => Self::Path,
            raw::FPDF_PAGEOBJ_IMAGE => Self::Image,
            raw::FPDF_PAGEOBJ_SHADING => Self::Shading,
            raw::FPDF_PAGEOBJ_FORM => Self::Form,
            _ => Self::Unknown,
        }
    }
}

/// 文字渲染模式（`Tr` 操作符），对应 `FPDF_TEXTRENDERMODE_*`。
///
/// `Invisible`（`Tr 3`）常见于扫描件的 OCR 文字层（方案 4.3、6.2）；`*Clip` 模式的文字块
/// 要整体当作一个对象（方案 2.3 第 3 条）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextRenderMode {
    Fill,
    Stroke,
    FillStroke,
    Invisible,
    FillClip,
    StrokeClip,
    FillStrokeClip,
    Clip,
}

impl TextRenderMode {
    fn from_raw(v: raw::FPDF_TEXT_RENDERMODE) -> Option<Self> {
        Some(match v {
            raw::FPDF_TEXTRENDERMODE_FILL => Self::Fill,
            raw::FPDF_TEXTRENDERMODE_STROKE => Self::Stroke,
            raw::FPDF_TEXTRENDERMODE_FILL_STROKE => Self::FillStroke,
            raw::FPDF_TEXTRENDERMODE_INVISIBLE => Self::Invisible,
            raw::FPDF_TEXTRENDERMODE_FILL_CLIP => Self::FillClip,
            raw::FPDF_TEXTRENDERMODE_STROKE_CLIP => Self::StrokeClip,
            raw::FPDF_TEXTRENDERMODE_FILL_STROKE_CLIP => Self::FillStrokeClip,
            raw::FPDF_TEXTRENDERMODE_CLIP => Self::Clip,
            _ => return None,
        })
    }

    /// 字形是否不会画出来（`Tr 3` 与 `Tr 7`）。
    pub fn is_invisible(self) -> bool {
        matches!(self, Self::Invisible | Self::Clip)
    }

    /// 是否把字形加入剪切路径（`Tr 4`–`Tr 7`）。
    pub fn is_clip(self) -> bool {
        matches!(
            self,
            Self::FillClip | Self::StrokeClip | Self::FillStrokeClip | Self::Clip
        )
    }
}

/// 页面上的一个对象（文字、路径、图像、渐变或 Form XObject）。
///
/// 非拥有句柄：对象归页面（或其所在的 Form）所有，寿命由 `'page` 约束。
#[derive(Clone, Copy)]
pub struct PageObject<'page> {
    handle: raw::FPDF_PAGEOBJECT,
    _marker: PhantomData<&'page ()>,
}

impl std::fmt::Debug for PageObject<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageObject")
            .field("kind", &self.kind())
            .field("bounds", &self.bounds())
            .finish()
    }
}

impl PageObject<'_> {
    /// # Safety
    /// `handle` 必须是属于某个仍然存活的页面或 Form 的有效对象句柄。
    pub(crate) unsafe fn from_raw(handle: raw::FPDF_PAGEOBJECT) -> Self {
        Self {
            handle,
            _marker: PhantomData,
        }
    }

    pub(crate) fn raw_handle(&self) -> raw::FPDF_PAGEOBJECT {
        self.handle
    }

    /// 句柄的不透明标识：同一页面存活期间，指向同一个对象的句柄返回相同的值。
    ///
    /// 只用于在同一页面内判断"是不是同一个对象"（例如把字符归到所属的文字对象，方案 5.1），
    /// 页面关闭后没有意义，也不能跨页面、跨进程比较或持久化。
    pub fn handle_id(&self) -> usize {
        self.handle as usize
    }

    pub fn kind(&self) -> ObjectKind {
        // SAFETY: handle 有效。
        ObjectKind::from_raw(unsafe { raw::FPDFPageObj_GetType(self.handle) })
    }

    /// 文字对象的渲染模式；非文字对象或 PDFium 不认识的模式返回 `None`。
    pub fn text_render_mode(&self) -> Option<TextRenderMode> {
        if self.kind() != ObjectKind::Text {
            return None;
        }
        // SAFETY: handle 是有效的文字对象；非文字对象时 PDFium 返回 UNKNOWN，上面已排除。
        TextRenderMode::from_raw(unsafe { raw::FPDFTextObj_GetTextRenderMode(self.handle) })
    }

    /// 对象在页面坐标里的包围盒。
    pub fn bounds(&self) -> Option<Rect> {
        let mut r = Rect::default();
        // SAFETY: handle 有效，四个输出指针均指向有效的 f32。
        let ok = unsafe {
            raw::FPDFPageObj_GetBounds(
                self.handle,
                &mut r.left,
                &mut r.bottom,
                &mut r.right,
                &mut r.top,
            )
        };
        (ok != 0).then_some(r)
    }

    /// 对象矩阵。文字、图像、Form 对象有自己的矩阵；路径对象返回其变换矩阵。
    pub fn matrix(&self) -> Option<Matrix> {
        let mut m = raw::FS_MATRIX {
            a: 0.0,
            b: 0.0,
            c: 0.0,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        };
        // SAFETY: handle 有效，m 是有效的输出位置。
        let ok = unsafe { raw::FPDFPageObj_GetMatrix(self.handle, &mut m) };
        (ok != 0).then(|| Matrix::from_fs(m))
    }

    pub fn set_matrix(&self, matrix: Matrix) -> Result<()> {
        let m = matrix.to_fs();
        // SAFETY: handle 有效，m 在调用期间有效。
        let ok = unsafe { raw::FPDFPageObj_SetMatrix(self.handle, &m) };
        if ok == 0 {
            Err(Error::PageObject)
        } else {
            Ok(())
        }
    }

    /// 在现有矩阵上叠加一个变换。
    ///
    /// 使用 `FPDFPageObj_Transform`（六个 double）：对路径对象会变换路径点本身，
    /// `TransformF`/`SetMatrix` 只改矩阵，部分 PDFium 版本渲染与 `GenerateContent` 仍按原路径点输出。
    pub fn transform(&self, matrix: Matrix) -> Result<()> {
        // SAFETY: handle 有效；该 API 无返回值。
        unsafe {
            raw::FPDFPageObj_Transform(
                self.handle,
                matrix.a as f64,
                matrix.b as f64,
                matrix.c as f64,
                matrix.d as f64,
                matrix.e as f64,
                matrix.f as f64,
            );
        }
        Ok(())
    }

    /// 把对象标记为"已修改"，使 [`Page::generate_content`](crate::Page::generate_content) 重写它所在的内容流。
    ///
    /// 公共 API 没有直接的 `SetDirty`，这里用单位矩阵变换触发（PDFium 在 `FPDFPageObj_Transform*` 里置脏）。
    /// 往返测试（P0 计划 V02）依赖这一点，应在报告里核对保存后的内容流确实被重写。
    pub fn mark_modified(&self) -> Result<()> {
        self.transform(Matrix::IDENTITY)
    }

    pub fn fill_color(&self) -> Option<Rgba> {
        let (mut r, mut g, mut b, mut a): (c_uint, c_uint, c_uint, c_uint) = (0, 0, 0, 0);
        // SAFETY: handle 有效，四个输出指针有效。
        let ok =
            unsafe { raw::FPDFPageObj_GetFillColor(self.handle, &mut r, &mut g, &mut b, &mut a) };
        (ok != 0).then(|| to_rgba(r, g, b, a))
    }

    pub fn set_fill_color(&self, color: Rgba) -> Result<()> {
        // SAFETY: handle 有效。
        let ok = unsafe {
            raw::FPDFPageObj_SetFillColor(
                self.handle,
                color.r.into(),
                color.g.into(),
                color.b.into(),
                color.a.into(),
            )
        };
        if ok == 0 {
            Err(Error::PageObject)
        } else {
            Ok(())
        }
    }

    pub fn stroke_color(&self) -> Option<Rgba> {
        let (mut r, mut g, mut b, mut a): (c_uint, c_uint, c_uint, c_uint) = (0, 0, 0, 0);
        // SAFETY: handle 有效，四个输出指针有效。
        let ok =
            unsafe { raw::FPDFPageObj_GetStrokeColor(self.handle, &mut r, &mut g, &mut b, &mut a) };
        (ok != 0).then(|| to_rgba(r, g, b, a))
    }

    pub fn set_stroke_color(&self, color: Rgba) -> Result<()> {
        // SAFETY: handle 有效。
        let ok = unsafe {
            raw::FPDFPageObj_SetStrokeColor(
                self.handle,
                color.r.into(),
                color.g.into(),
                color.b.into(),
                color.a.into(),
            )
        };
        if ok == 0 {
            Err(Error::PageObject)
        } else {
            Ok(())
        }
    }

    /// 对象是否带透明（混合模式、透明度、软蒙版），拖动预览时需要区别处理（方案 3.2）。
    pub fn has_transparency(&self) -> bool {
        // SAFETY: handle 有效。
        unsafe { raw::FPDFPageObj_HasTransparency(self.handle) != 0 }
    }

    /// 文字对象使用的字体；非文字对象或字体缺失时返回 `None`。
    pub fn font(&self) -> Option<Font<'_>> {
        if self.kind() != ObjectKind::Text {
            return None;
        }
        // SAFETY: handle 是有效的文字对象；返回的是 PDFium 内部持有的非拥有引用。
        let font = unsafe { raw::FPDFTextObj_GetFont(self.handle) };
        // SAFETY: font 非空时归该文字对象所有，寿命由返回值的生命周期约束在本对象之内。
        (!font.is_null()).then(|| unsafe { Font::from_raw(font) })
    }

    /// 所属标记内容的 MCID，没有时返回 `None`。
    pub fn marked_content_id(&self) -> Option<i32> {
        // SAFETY: handle 有效。
        let id = unsafe { raw::FPDFPageObj_GetMarkedContentID(self.handle) };
        (id >= 0).then_some(id)
    }

    /// Form XObject 内部的对象数量；非 Form 对象返回 0。
    pub fn form_object_count(&self) -> usize {
        if self.kind() != ObjectKind::Form {
            return 0;
        }
        // SAFETY: handle 是有效的 Form 对象。
        let n = unsafe { raw::FPDFFormObj_CountObjects(self.handle) };
        usize::try_from(n).unwrap_or(0)
    }

    /// 取 Form XObject 内部第 `index` 个对象。
    pub fn form_object(&self, index: usize) -> Option<PageObject<'_>> {
        if self.kind() != ObjectKind::Form {
            return None;
        }
        let idx = c_ulong::try_from(index).ok()?;
        // SAFETY: handle 是有效的 Form 对象；越界时返回 NULL。
        let obj = unsafe { raw::FPDFFormObj_GetObject(self.handle, idx) };
        // SAFETY: obj 非空时归该 Form 所有，寿命不超过它。
        (!obj.is_null()).then(|| unsafe { PageObject::from_raw(obj) })
    }

    /// 遍历 Form XObject 内部的对象（一层，不递归）。
    pub fn form_objects(&self) -> impl Iterator<Item = PageObject<'_>> + '_ {
        (0..self.form_object_count()).filter_map(move |i| self.form_object(i))
    }
}

fn to_rgba(r: c_uint, g: c_uint, b: c_uint, a: c_uint) -> Rgba {
    let clamp = |v: c_uint| u8::try_from(v.min(255)).unwrap_or(255);
    Rgba {
        r: clamp(r),
        g: clamp(g),
        b: clamp(b),
        a: clamp(a),
    }
}
