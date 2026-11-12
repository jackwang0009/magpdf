//! 资源查询：对应层只需要知道“这个资源名在 PDFium 眼里是什么”。
//!
//! PDFium 建不建对象取决于资源：`Do` 引用的 XObject 得是一条流，`/Subtype` 是 `Form` 或 `Image`；
//! `sh` 引用的渐变得能加载；`Tf` 引用的字体找不到时换成备用字体（不为空），找到了但加载失败时
//! 保持原来的字体。判定条件照 PDFium 的 `CPDF_StreamContentParser`、`CPDF_Font::Create`、
//! `CPDF_CIDFont::Load`、`CPDF_ShadingPattern::Load`/`Validate`、`CPDF_Function::Load`
//! 移植（BSD-3-Clause，见 `PDFIUM-NOTICE.md`）。
//!
//! 渐变那一段最容易和规范想的不一样：PDFium 要求函数的输入、输出个数和渐变类型、颜色空间的分量数
//! 对得上（比如 1 型渐变要 2 个输入，拼接函数只能有 1 个输入），对不上就当渐变加载失败、`sh` 不建
//! 对象。函数那几个文件（`cpdf_function.cpp`、`cpdf_expintfunc.cpp`、`cpdf_sampledfunc.cpp`、
//! `cpdf_stitchfunc.cpp`、`cpdf_psfunc.cpp`、`cpdf_psengine.cpp`）是凭对源码的记忆写的，没有逐行
//! 对照；写错的地方会在与 PDFium 核对对象个数时暴露（那一页归为“对不上”，交给回退）。

use crate::content::parser::{is_delimiter, is_line_ending, is_whitespace};
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

    /// `CPDF_ShadingPattern::Load` 加 `Validate`。
    fn shading(&self, name: &[u8]) -> bool {
        let Some(obj) = self.find(b"Shading", name) else {
            return false;
        };
        let is_stream = matches!(obj, PdfObject::Stream(_));
        let Some(dict) = obj.as_dict() else {
            return false;
        };
        // 函数：最多取数组的前 4 个，加载不了的占着位置。写成 `null` 也算“有一个加载不了的函数”。
        let raw_function = dict.get(b"Function");
        let functions: Vec<Option<FunctionShape>> = match self.doc.resolve(raw_function) {
            None => Vec::new(),
            Some(PdfObject::Array(items)) => items
                .iter()
                .take(4)
                .map(|f| self.function_shape(f, &mut Vec::new()))
                .collect(),
            Some(_) => vec![raw_function.and_then(|f| self.function_shape(f, &mut Vec::new()))],
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
        let kind = self.int_for(dict, b"ShadingType");
        if !(1..=7).contains(&kind) {
            return false;
        }
        let mesh = kind >= 4;
        if mesh && !is_stream {
            return false;
        }
        if family == Family::Indexed && (!mesh || !functions.is_empty()) {
            return false;
        }
        // `ValidateFunctions`：恰好这么多个函数，个个加载得了，输入输出个数都对。
        let validate = |count: usize, inputs: u32, outputs: u32| {
            functions.len() == count
                && functions
                    .iter()
                    .all(|f| f.is_some_and(|f| f.inputs == inputs && f.outputs == outputs))
        };
        let n = components as usize;
        match kind {
            // 一个“2 进 N 出”的函数，或者 N 个“2 进 1 出”的。
            1 => validate(1, 2, components) || validate(n, 2, 1),
            // 一个“1 进 N 出”的，或者 N 个“1 进 1 出”的。
            2 | 3 => validate(1, 1, components) || validate(n, 1, 1),
            // 网格渐变可以没有函数。
            _ => functions.is_empty() || validate(1, 1, components) || validate(n, 1, 1),
        }
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

/// 一个加载得了的函数有几个输入、几个输出（`CPDF_Function::InputCount`、`OutputCount`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FunctionShape {
    inputs: u32,
    outputs: u32,
}

impl DocResources<'_> {
    /// `GetIntegerFor`：没有这一项、不是数字都得 0。
    fn int_for(&self, dict: &PdfDict, key: &[u8]) -> i32 {
        self.direct(dict, key).map_or(0, |v| v.get_integer())
    }

    /// `GetArrayFor`。
    fn array_for(&self, dict: &PdfDict, key: &[u8]) -> Option<Vec<PdfObject>> {
        match self.direct(dict, key) {
            Some(PdfObject::Array(items)) => Some(items),
            _ => None,
        }
    }

    /// `CPDF_Function::Load`。`item` 是还没解引用的那一项；`visited` 是正在加载的函数的对象号，
    /// 拼接函数绕回自己时加载失败。
    fn function_shape(&self, item: &PdfObject, visited: &mut Vec<u32>) -> Option<FunctionShape> {
        let number = item.as_ref().map(|(n, _)| n);
        if number.is_some_and(|n| visited.contains(&n)) {
            return None;
        }
        let obj = self.doc.resolve(Some(item))?;
        if let Some(n) = number {
            visited.push(n);
        }
        let shape = self.function_init(&obj, visited);
        if number.is_some() {
            visited.pop();
        }
        shape
    }

    /// `CPDF_Function::Init` 和各类函数的 `v_Init` 里会让加载失败的那些检查。
    fn function_init(&self, obj: &PdfObject, visited: &mut Vec<u32>) -> Option<FunctionShape> {
        let dict = match obj {
            PdfObject::Stream(stream) => &stream.dict,
            PdfObject::Dict(dict) => dict,
            _ => return None,
        };
        // 没写 `FunctionType` 时取到 0，当成采样函数。
        let kind = self.int_for(dict, b"FunctionType");
        if !matches!(kind, 0 | 2 | 3 | 4) {
            return None;
        }
        let inputs = u32::try_from(self.array_for(dict, b"Domain")?.len() / 2).ok()?;
        if inputs == 0 {
            return None;
        }
        let mut outputs = match self.array_for(dict, b"Range") {
            Some(range) => u32::try_from(range.len() / 2).ok()?,
            None => 0,
        };
        // 采样函数和 PostScript 函数必须有 `Range`。
        if matches!(kind, 0 | 4) && outputs == 0 {
            return None;
        }
        match kind {
            0 => {
                let PdfObject::Stream(stream) = obj else {
                    return None;
                };
                let size = self.array_for(dict, b"Size")?;
                if size.is_empty() {
                    return None;
                }
                let bits = self.int_for(dict, b"BitsPerSample");
                if !matches!(bits, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) {
                    return None;
                }
                // 样本的总位数：每个样本的位数 × 输出个数 × 各维的大小，按 32 位无符号数算，溢出算失败。
                let mut total = (bits as u32).checked_mul(outputs)?;
                for index in 0..inputs as usize {
                    let n = size
                        .get(index)
                        .and_then(|v| self.doc.resolve(Some(v)))
                        .map_or(0, |v| v.get_integer());
                    if n <= 0 {
                        return None;
                    }
                    total = total.checked_mul(n as u32)?;
                }
                let bytes = total.checked_add(7)? / 8;
                if bytes == 0 || bytes as usize > self.doc.decode_stream(stream).len() {
                    return None;
                }
            }
            2 => {
                // `GetNumberFor("N")`：不解引用，得直接写着一个数。
                if !dict.get(b"N").is_some_and(PdfObject::is_number) {
                    return None;
                }
                if outputs == 0
                    && let Some(c0) = self.array_for(dict, b"C0")
                {
                    outputs = u32::try_from(c0.len()).ok()?;
                }
                if outputs == 0 {
                    outputs = 1;
                }
                outputs = outputs.checked_mul(inputs)?;
            }
            3 => {
                if inputs != 1 {
                    return None;
                }
                let functions = self.array_for(dict, b"Functions")?;
                let bounds = self.array_for(dict, b"Bounds")?;
                let encode = self.array_for(dict, b"Encode")?;
                if functions.is_empty()
                    || bounds.len() < functions.len() - 1
                    || encode.len() < functions.len().checked_mul(2)?
                {
                    return None;
                }
                // 每一段都得是“1 进”的，输出个数都一样、不为 0。
                let mut common: Option<u32> = None;
                for sub in &functions {
                    let shape = self.function_shape(sub, visited)?;
                    if shape.inputs != 1 || shape.outputs == 0 {
                        return None;
                    }
                    if common.is_some_and(|c| c != shape.outputs) {
                        return None;
                    }
                    common = Some(shape.outputs);
                }
                outputs = common?;
            }
            _ => {
                let program = match obj {
                    PdfObject::Stream(stream) => self.doc.decode_stream(stream),
                    _ => Vec::new(),
                };
                if !postscript_parses(&program) {
                    return None;
                }
            }
        }
        Some(FunctionShape { inputs, outputs })
    }
}

/// `CPDF_SimpleParser::GetWord`：PostScript 函数的切词。
struct SimpleWords<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SimpleWords<'a> {
    fn next_word(&mut self) -> Option<&'a [u8]> {
        let data = self.data;
        let take = |pos: &mut usize| -> Option<u8> {
            let ch = *data.get(*pos)?;
            *pos += 1;
            Some(ch)
        };
        // 跳过空白和注释。
        let mut ch;
        loop {
            ch = take(&mut self.pos)?;
            while is_whitespace(ch) {
                ch = take(&mut self.pos)?;
            }
            if ch != b'%' {
                break;
            }
            loop {
                ch = take(&mut self.pos)?;
                if is_line_ending(ch) {
                    break;
                }
            }
        }
        let start = self.pos - 1;
        let len = data.len();
        if !is_delimiter(ch) {
            while self.pos < len && !is_delimiter(data[self.pos]) && !is_whitespace(data[self.pos])
            {
                self.pos += 1;
            }
            return Some(&data[start..self.pos]);
        }
        match ch {
            b'/' => {
                while self.pos < len
                    && !is_delimiter(data[self.pos])
                    && !is_whitespace(data[self.pos])
                {
                    self.pos += 1;
                }
            }
            b'<' => {
                // `<<` 是一个词；否则一直到下一个 `>`（紧跟着的那个字符不管是什么都先吃掉）。
                if let Some(next) = take(&mut self.pos)
                    && next != b'<'
                {
                    while self.pos < len && data[self.pos] != b'>' {
                        self.pos += 1;
                    }
                    if self.pos < len {
                        self.pos += 1;
                    }
                }
            }
            b'>' => {
                if data.get(self.pos) == Some(&b'>') {
                    self.pos += 1;
                }
            }
            b'(' => {
                let mut level = 1;
                while self.pos < len {
                    let c = data[self.pos];
                    if c == b')' {
                        level -= 1;
                        if level == 0 {
                            break;
                        }
                    }
                    if c == b'\\' {
                        self.pos += 1;
                    } else if c == b'(' {
                        level += 1;
                    }
                    if self.pos >= len {
                        break;
                    }
                    self.pos += 1;
                }
                if self.pos < len {
                    self.pos += 1;
                }
            }
            _ => {}
        }
        Some(&data[start..self.pos.min(len)])
    }
}

/// `CPDF_PSEngine::Parse`：第一个词得是 `{`，花括号得配得上，嵌套不超过 128 层。
/// 不认识的词当成数字常量，不算错。
fn postscript_parses(program: &[u8]) -> bool {
    const MAX_DEPTH: usize = 128;
    let mut words = SimpleWords {
        data: program,
        pos: 0,
    };
    if words.next_word() != Some(b"{") {
        return false;
    }
    let mut depth = 0usize;
    loop {
        match words.next_word() {
            None => return false,
            Some(b"}") => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            Some(b"{") => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return false;
                }
            }
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::reader::tests::{CATALOG, PAGE, PAGES, build};

    /// 拼一份文档，资源里的渐变 `Sh` 是对象 10，问它加载得了吗。
    fn shading_loads(objects: &[(u32, &str)]) -> bool {
        let mut all = vec![(1, CATALOG), (2, PAGES), (3, PAGE), (9, "<< /Sh 10 0 R >>")];
        all.extend_from_slice(objects);
        let doc = PdfDocument::load(build(&all, "")).expect("测试文档应当读得进来");
        let resources = PdfDict::from_entries([(b"Shading".to_vec(), PdfObject::Ref(9, 0))]);
        DocResources::new(&doc, Some(resources)).shading(b"Sh")
    }

    /// 一条流对象的写法。
    fn stream(dict: &str, data: &str) -> String {
        format!(
            "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
            data.len()
        )
    }

    const EXP_1_TO_3: &str = "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >>";

    #[test]
    fn axial_shading_needs_one_input() {
        // 轴向渐变 + “1 进 3 出”的指数函数：加载得了。
        assert!(shading_loads(&[
            (
                10,
                "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Function 11 0 R >>"
            ),
            (11, EXP_1_TO_3),
        ]));
        // 输出个数和颜色空间的分量数对不上。
        assert!(!shading_loads(&[
            (
                10,
                "<< /ShadingType 2 /ColorSpace /DeviceCMYK /Coords [0 0 1 0] /Function 11 0 R >>"
            ),
            (11, EXP_1_TO_3),
        ]));
        // 三个“1 进 1 出”的函数，每个分量一个。
        assert!(shading_loads(&[
            (
                10,
                "<< /ShadingType 2 /ColorSpace /DeviceRGB /Function [11 0 R 11 0 R 11 0 R] >>"
            ),
            (11, "<< /FunctionType 2 /Domain [0 1] /N 1 >>"),
        ]));
        // 没有函数、函数是 null、没有颜色空间、颜色空间是 Pattern。
        assert!(!shading_loads(&[(
            10,
            "<< /ShadingType 2 /ColorSpace /DeviceRGB >>"
        )]));
        assert!(!shading_loads(&[
            (10, "<< /ShadingType 2 /Function 11 0 R >>"),
            (11, EXP_1_TO_3),
        ]));
        assert!(!shading_loads(&[
            (
                10,
                "<< /ShadingType 2 /ColorSpace /Pattern /Function 11 0 R >>"
            ),
            (11, EXP_1_TO_3),
        ]));
    }

    #[test]
    fn function_based_shading_needs_two_inputs() {
        // 1 型渐变要“2 进”的函数。指数函数的输出个数要乘上输入个数：2 进、C0 有 3 项时是 6 出，对不上。
        assert!(!shading_loads(&[
            (
                10,
                "<< /ShadingType 1 /ColorSpace /DeviceRGB /Function 11 0 R >>"
            ),
            (
                11,
                "<< /FunctionType 2 /Domain [0 1 0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >>"
            ),
        ]));
        // 拼接函数只能有 1 个输入：`bug_551460.pdf` 就是 1 型渐变配了 2 个输入的拼接函数。
        assert!(!shading_loads(&[
            (
                10,
                "<< /ShadingType 1 /ColorSpace /DeviceRGB /Function 11 0 R >>"
            ),
            (
                11,
                "<< /FunctionType 3 /Domain [0 1 0 1] /Functions [12 0 R] /Bounds [] /Encode [0 1] >>"
            ),
            (12, EXP_1_TO_3),
        ]));
        // PostScript 函数：2 进 3 出，对得上；程序括号不配对就加载失败。
        let shading = "<< /ShadingType 1 /ColorSpace /DeviceRGB /Function 11 0 R >>";
        let dict = "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]";
        let good = stream(dict, "{ 2 copy mul }");
        assert!(shading_loads(&[(10, shading), (11, good.as_str())]));
        let bad = stream(dict, "{ 2 copy mul");
        assert!(!shading_loads(&[(10, shading), (11, bad.as_str())]));
    }

    #[test]
    fn stitching_functions() {
        let shading =
            "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [0 0 0 0 0 1] /Function 11 0 R >>";
        // 两段，输出个数一样。
        assert!(shading_loads(&[
            (10, shading),
            (
                11,
                "<< /FunctionType 3 /Domain [0 1] /Functions [12 0 R 12 0 R] /Bounds [0.5] \
                 /Encode [0 1 0 1] >>"
            ),
            (12, EXP_1_TO_3),
        ]));
        // `Bounds` 少了、`Encode` 少了、缺 `Encode`。
        for stitch in [
            "<< /FunctionType 3 /Domain [0 1] /Functions [12 0 R 12 0 R] /Bounds [] /Encode [0 1 0 1] >>",
            "<< /FunctionType 3 /Domain [0 1] /Functions [12 0 R 12 0 R] /Bounds [0.5] /Encode [0 1] >>",
            "<< /FunctionType 3 /Domain [0 1] /Functions [12 0 R 12 0 R] /Bounds [0.5] >>",
            "<< /FunctionType 3 /Domain [0 1] /Functions [] /Bounds [] /Encode [] >>",
        ] {
            assert!(
                !shading_loads(&[(10, shading), (11, stitch), (12, EXP_1_TO_3)]),
                "{stitch}"
            );
        }
        // 两段的输出个数不一样。
        assert!(!shading_loads(&[
            (10, shading),
            (
                11,
                "<< /FunctionType 3 /Domain [0 1] /Functions [12 0 R 13 0 R] /Bounds [0.5] \
                 /Encode [0 1 0 1] >>"
            ),
            (12, EXP_1_TO_3),
            (13, "<< /FunctionType 2 /Domain [0 1] /N 1 >>"),
        ]));
        // 绕回自己。
        assert!(!shading_loads(&[
            (10, shading),
            (
                11,
                "<< /FunctionType 3 /Domain [0 1] /Functions [11 0 R] /Bounds [] /Encode [0 1] >>"
            ),
        ]));
    }

    #[test]
    fn function_details() {
        let shading = "<< /ShadingType 2 /ColorSpace /DeviceGray /Function 11 0 R >>";
        let loads = |function: &str| shading_loads(&[(10, shading), (11, function)]);
        // 指数函数：没有 `C0` 时 1 个输出；`N` 必须有；`Domain` 必须有。
        assert!(loads("<< /FunctionType 2 /Domain [0 1] /N 1 >>"));
        assert!(!loads("<< /FunctionType 2 /Domain [0 1] >>"));
        assert!(!loads("<< /FunctionType 2 /N 1 >>"));
        assert!(!loads("<< /FunctionType 2 /Domain [0] /N 1 >>"));
        // `Range` 优先于 `C0` 决定输出个数。
        assert!(loads(
            "<< /FunctionType 2 /Domain [0 1] /Range [0 1] /C0 [0 0 0] /N 1 >>"
        ));
        // 不认识的类型；采样函数、PostScript 函数写成字典（不是流）。
        assert!(!loads("<< /FunctionType 1 /Domain [0 1] /N 1 >>"));
        assert!(!loads(
            "<< /FunctionType 0 /Domain [0 1] /Range [0 1] /Size [2] /BitsPerSample 8 >>"
        ));
        assert!(!loads("<< /FunctionType 4 /Domain [0 1] /Range [0 1] >>"));
        // 采样函数：数据够长才行；位数得是规定的几种；`Range` 必须有。
        let sampled = |dict: &str, data: &str| {
            let body = stream(&format!("/FunctionType 0 /Domain [0 1] {dict}"), data);
            shading_loads(&[(10, shading), (11, body.as_str())])
        };
        assert!(sampled("/Range [0 1] /Size [2] /BitsPerSample 8", "ab"));
        assert!(!sampled("/Range [0 1] /Size [3] /BitsPerSample 8", "ab"));
        assert!(!sampled("/Range [0 1] /Size [2] /BitsPerSample 7", "ab"));
        assert!(!sampled("/Size [2] /BitsPerSample 8", "ab"));
        assert!(!sampled("/Range [0 1] /Size [0] /BitsPerSample 8", "ab"));
        assert!(!sampled("/Range [0 1] /BitsPerSample 8", "ab"));
    }

    #[test]
    fn mesh_shadings() {
        let mesh = |dict: &str| {
            let body = stream(dict, "ab");
            shading_loads(&[
                (10, body.as_str()),
                (11, EXP_1_TO_3),
                (12, "<< /FunctionType 2 /Domain [0 1] /N 1 >>"),
            ])
        };
        // 网格渐变可以没有函数，但得是流。
        assert!(mesh("/ShadingType 4 /ColorSpace /DeviceRGB"));
        assert!(!shading_loads(&[(
            10,
            "<< /ShadingType 4 /ColorSpace /DeviceRGB >>"
        )]));
        // 有函数时也要对得上个数；写成 null 算有一个坏函数。
        assert!(mesh(
            "/ShadingType 7 /ColorSpace /DeviceRGB /Function 11 0 R"
        ));
        assert!(!mesh(
            "/ShadingType 7 /ColorSpace /DeviceGray /Function 11 0 R"
        ));
        assert!(!mesh(
            "/ShadingType 6 /ColorSpace /DeviceRGB /Function null"
        ));
        // 类型出界。
        assert!(!mesh("/ShadingType 8 /ColorSpace /DeviceRGB"));
        assert!(!mesh("/ColorSpace /DeviceRGB"));
    }

    #[test]
    fn postscript_programs() {
        assert!(postscript_parses(b"{ 2 copy mul }"));
        assert!(postscript_parses(
            b"  % comment\n{ dup 0 gt { 1 } { 0 } ifelse }"
        ));
        assert!(postscript_parses(b"{}{"), "配上之后剩下的不看");
        assert!(postscript_parses(b"{unknownword 1.5}"));
        assert!(!postscript_parses(b""));
        assert!(!postscript_parses(b"2 copy"));
        assert!(!postscript_parses(b"{ 2 copy"));
        assert!(!postscript_parses(b"{ { 1 } "));
        // 嵌套 128 层可以，129 层不行。
        let nested = |n: usize| {
            let mut p = b"{".to_vec();
            p.extend(std::iter::repeat_n(b'{', n));
            p.extend(std::iter::repeat_n(b'}', n + 1));
            p
        };
        assert!(postscript_parses(&nested(128)));
        assert!(!postscript_parses(&nested(129)));
    }
}
