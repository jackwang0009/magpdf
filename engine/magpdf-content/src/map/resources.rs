//! 资源查询：对应层只需要知道“这个资源名在 PDFium 眼里是什么”。
//!
//! PDFium 建不建对象取决于资源：`Do` 引用的 XObject 得是一条流，`/Subtype` 是 `Form` 或 `Image`；
//! `sh` 引用的渐变得能加载；`Tf` 引用的字体找不到时换成备用字体（不为空），找到了但加载失败时
//! 保持原来的字体。判定条件照 PDFium 的 `CPDF_StreamContentParser`、`CPDF_Font::Create`、
//! `CPDF_CIDFont::Load`、`CPDF_ShadingPattern::Load` 移植（BSD-3-Clause，见 `PDFIUM-NOTICE.md`）。
//! 渐变里函数的输入输出个数没有逐项核对，只核对了个数；这样的差别会在与 PDFium 核对对象时暴露。

use crate::pdf::object::{PdfDict, PdfObject};
use crate::pdf::reader::PdfDocument;

/// `Do` 引用的 XObject。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XObjectKind {
    /// 资源里没有，或者不是流：不建对象。
    Missing,
    Form,
    Image,
    /// 是流，但 `/Subtype` 既不是 `Form` 也不是 `Image`：不建对象。
    Other,
}

/// `Tf`（或 ExtGState 的 `/Font`）找字体的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontLookup {
    /// 资源里没有这个名字（或者不是字典）：PDFium 换成备用的 Helvetica，字体不为空。
    Stock,
    Loaded {
        type3: bool,
    },
    /// 字典在，但加载失败：`Tf` 保持原来的字体，ExtGState 把字体置空。
    Failed,
}

/// ExtGState 里影响“建不建对象”的两项。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExtGState {
    /// `/LC`：线帽。
    pub line_cap: Option<i32>,
    /// `/Font [名字 字号]`：PDFium 把数组第一项当成资源名去找字体（第一项按规范是字体字典的引用，
    /// 这时名字为空，结果是备用字体）。
    pub font: Option<(Vec<u8>, f32)>,
}

/// 页面资源的查询接口。测试里用假的实现。
pub trait Resources {
    fn xobject(&self, name: &[u8]) -> XObjectKind;
    /// `sh` 引用的渐变能不能加载。
    fn shading(&self, name: &[u8]) -> bool;
    fn font(&self, name: &[u8]) -> FontLookup;
    /// 资源里不是字典时返回 `None`（`gs` 整个被忽略）。
    fn ext_gstate(&self, name: &[u8]) -> Option<ExtGState>;
    /// `/ColorSpace` 资源里的颜色空间：`None` 没有这个名字；`Some(None)` 有但加载不了；
    /// `Some(Some(n))` 是分量数。
    fn colorspace(&self, name: &[u8]) -> Option<Option<u32>>;
    /// `/Pattern` 资源里有没有可用的图案（`PatternType` 是 1 或 2）。
    fn pattern(&self, name: &[u8]) -> bool;
}

/// 没有任何资源。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoResources;

impl Resources for NoResources {
    fn xobject(&self, _: &[u8]) -> XObjectKind {
        XObjectKind::Missing
    }
    fn shading(&self, _: &[u8]) -> bool {
        false
    }
    fn font(&self, _: &[u8]) -> FontLookup {
        FontLookup::Stock
    }
    fn ext_gstate(&self, _: &[u8]) -> Option<ExtGState> {
        None
    }
    fn colorspace(&self, _: &[u8]) -> Option<Option<u32>> {
        None
    }
    fn pattern(&self, _: &[u8]) -> bool {
        false
    }
}

/// 从文档里查页面资源。
#[derive(Debug)]
pub struct DocResources<'a> {
    doc: &'a PdfDocument,
    resources: Option<PdfDict>,
}

impl<'a> DocResources<'a> {
    /// `resources` 是页面（含继承）的 `/Resources` 字典。
    pub fn new(doc: &'a PdfDocument, resources: Option<PdfDict>) -> Self {
        Self { doc, resources }
    }

    /// `FindResourceObj`：某一类资源字典（字典，或者流的字典）里的一项，解一层引用。
    fn find(&self, kind: &[u8], name: &[u8]) -> Option<PdfObject> {
        let holder = self.doc.deref(self.resources.as_ref()?.get(kind)?)?;
        self.doc.deref(holder.as_dict()?.get(name)?)
    }

    fn direct(&self, dict: &PdfDict, key: &[u8]) -> Option<PdfObject> {
        self.doc.deref(dict.get(key)?)
    }

    /// `GetByteStringFor`：名字或字符串的内容，别的是空。
    fn byte_string(&self, dict: &PdfDict, key: &[u8]) -> Vec<u8> {
        match self.direct(dict, key) {
            Some(PdfObject::Name(v) | PdfObject::Str(v)) => v,
            _ => Vec::new(),
        }
    }
}

/// 颜色空间的族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Device,
    Cal,
    Icc,
    Indexed,
    Separation,
    DeviceN,
    Pattern,
}

fn stock(name: &[u8]) -> Option<(Family, u32)> {
    match name {
        b"DeviceGray" | b"G" => Some((Family::Device, 1)),
        b"DeviceRGB" | b"RGB" => Some((Family::Device, 3)),
        b"DeviceCMYK" | b"CMYK" => Some((Family::Device, 4)),
        b"Pattern" => Some((Family::Pattern, 1)),
        _ => None,
    }
}

/// 颜色空间对象能不能加载、有几个分量（`CPDF_ColorSpace::Load` 的主要分支）。
fn colorspace_info(doc: &PdfDocument, obj: &PdfObject, depth: u32) -> Option<(Family, u32)> {
    if depth > 8 {
        return None;
    }
    match obj {
        PdfObject::Name(name) => stock(name),
        PdfObject::Array(items) => {
            let family = doc.deref(items.first()?)?;
            let family = family.as_name()?;
            if items.len() == 1 {
                return stock(family);
            }
            let second = doc.deref(&items[1]);
            match family {
                b"CalGray" => second?.as_dict().map(|_| (Family::Cal, 1)),
                b"CalRGB" | b"Lab" => second?.as_dict().map(|_| (Family::Cal, 3)),
                b"ICCBased" => {
                    let PdfObject::Stream(stream) = second? else {
                        return None;
                    };
                    let n = doc.deref(stream.dict.get(b"N")?).and_then(|v| v.as_int())?;
                    matches!(n, 1 | 3 | 4).then_some((Family::Icc, n as u32))
                }
                b"Indexed" | b"I" => {
                    if items.len() < 4 {
                        return None;
                    }
                    let (base, _) = colorspace_info(doc, &second?, depth + 1)?;
                    (!matches!(base, Family::Indexed | Family::Pattern))
                        .then_some((Family::Indexed, 1))
                }
                b"Separation" => {
                    let alt = doc.deref(items.get(2)?)?;
                    let (alt_family, _) = colorspace_info(doc, &alt, depth + 1)?;
                    (alt_family != Family::Pattern).then_some((Family::Separation, 1))
                }
                b"DeviceN" => {
                    let names = second?;
                    let names = names.as_array()?;
                    let alt = doc.deref(items.get(2)?)?;
                    let (alt_family, _) = colorspace_info(doc, &alt, depth + 1)?;
                    (alt_family != Family::Pattern && !names.is_empty())
                        .then_some((Family::DeviceN, names.len() as u32))
                }
                b"Pattern" => Some((Family::Pattern, 1)),
                _ => None,
            }
        }
        _ => None,
    }
}

impl Resources for DocResources<'_> {
    fn xobject(&self, name: &[u8]) -> XObjectKind {
        match self.find(b"XObject", name) {
            Some(PdfObject::Stream(stream)) => {
                match self.byte_string(&stream.dict, b"Subtype").as_slice() {
                    b"Form" => XObjectKind::Form,
                    b"Image" => XObjectKind::Image,
                    _ => XObjectKind::Other,
                }
            }
            _ => XObjectKind::Missing,
        }
    }

    fn shading(&self, name: &[u8]) -> bool {
        let Some(obj) = self.find(b"Shading", name) else {
            return false;
        };
        let is_stream = matches!(obj, PdfObject::Stream(_));
        let Some(dict) = obj.as_dict() else {
            return false;
        };
        let Some(cs) = self.direct(dict, b"ColorSpace") else {
            return false;
        };
        let Some((family, components)) = colorspace_info(self.doc, &cs, 0) else {
            return false;
        };
        if family == Family::Pattern {
            return false;
        }
        let kind = self
            .direct(dict, b"ShadingType")
            .and_then(|v| v.as_int())
            .unwrap_or(0);
        if !(1..=7).contains(&kind) {
            return false;
        }
        let mesh = kind >= 4;
        if mesh && !is_stream {
            return false;
        }
        // 函数：PDFium 最多取数组的前 4 个；每个都得加载得了。
        let functions: Vec<bool> = match self.direct(dict, b"Function") {
            None => Vec::new(),
            Some(PdfObject::Array(items)) => items
                .iter()
                .take(4)
                .map(|f| self.function_loads(self.doc.deref(f).as_ref()))
                .collect(),
            Some(single) => vec![self.function_loads(Some(&single))],
        };
        if family == Family::Indexed && (!mesh || !functions.is_empty()) {
            return false;
        }
        if mesh && functions.is_empty() {
            return true;
        }
        // 要么一个函数输出全部分量，要么每个分量一个函数。
        functions.iter().all(|ok| *ok)
            && (functions.len() == 1 || functions.len() == components as usize)
    }

    fn font(&self, name: &[u8]) -> FontLookup {
        let Some(PdfObject::Dict(dict)) = self.find(b"Font", name) else {
            return FontLookup::Stock;
        };
        match self.byte_string(&dict, b"Subtype").as_slice() {
            b"Type3" => FontLookup::Loaded { type3: true },
            b"Type0" => {
                // `CPDF_CIDFont::Load`：`/DescendantFonts` 得是恰好一项的数组、那一项是字典，
                // `/Encoding` 得是名字或流。
                let descendants = self.direct(&dict, b"DescendantFonts");
                let one_dict = match descendants.as_ref().and_then(PdfObject::as_array) {
                    Some([only]) => matches!(self.doc.deref(only), Some(PdfObject::Dict(_))),
                    _ => false,
                };
                let encoding_ok = matches!(
                    self.direct(&dict, b"Encoding"),
                    Some(PdfObject::Name(_) | PdfObject::Stream(_))
                );
                if one_dict && encoding_ok {
                    FontLookup::Loaded { type3: false }
                } else {
                    FontLookup::Failed
                }
            }
            _ => FontLookup::Loaded { type3: false },
        }
    }

    fn ext_gstate(&self, name: &[u8]) -> Option<ExtGState> {
        let PdfObject::Dict(dict) = self.find(b"ExtGState", name)? else {
            return None;
        };
        let line_cap = self.direct(&dict, b"LC").map(|v| v.get_integer());
        let font = match self.direct(&dict, b"Font") {
            Some(PdfObject::Array(items)) => {
                let name = match items.first().and_then(|f| self.doc.deref(f)) {
                    Some(PdfObject::Name(n) | PdfObject::Str(n)) => n,
                    _ => Vec::new(),
                };
                let size = items
                    .get(1)
                    .and_then(|s| self.doc.deref(s))
                    .and_then(|s| s.as_f64())
                    .unwrap_or(0.0) as f32;
                Some((name, size))
            }
            _ => None,
        };
        Some(ExtGState { line_cap, font })
    }

    fn colorspace(&self, name: &[u8]) -> Option<Option<u32>> {
        let obj = self.find(b"ColorSpace", name)?;
        Some(colorspace_info(self.doc, &obj, 0).map(|(_, n)| n))
    }

    fn pattern(&self, name: &[u8]) -> bool {
        let Some(obj) = self.find(b"Pattern", name) else {
            return false;
        };
        let Some(dict) = obj.as_dict() else {
            return false;
        };
        matches!(
            self.direct(dict, b"PatternType").and_then(|v| v.as_int()),
            Some(1 | 2)
        )
    }
}

impl DocResources<'_> {
    /// `CPDF_Function::Load` 的粗略版本：是字典或流，`FunctionType` 是 0、2、3、4 之一。
    fn function_loads(&self, obj: Option<&PdfObject>) -> bool {
        let Some(dict) = obj.and_then(PdfObject::as_dict) else {
            return false;
        };
        matches!(
            self.direct(dict, b"FunctionType").and_then(|v| v.as_int()),
            Some(0 | 2 | 3 | 4)
        )
    }
}
