//! 组件（方案第 11 节）的 P0 原型：“文本”和“Code 128 条码”两种。
//!
//! 组件在文件里是普通的页面对象，参数存在它的标记内容里（`/MagComponent << /type … /content … /size … >> BDC`），
//! 存盘再打开还认得出来；改参数时按参数重新生成一个对象换掉旧的（方案 11.4）。
//! 《文件私有数据格式规范》4.3 定的格式是 `/MagPDF << /Comp /Ver /Params … >>`，参数按《组件参数定义》。
//! 原型的参数对不上那套结构，故意用另一个名字，原型存出来的文件不会被当成正式的组件。
//!
//! 原型的限制：
//! - 一个组件就是一个对象：文本是一个文字对象，条码是一个路径对象（每根条一个矩形子路径），条码下面不带可读的字。
//! - 文本只有一行。全是英文字母、数字和常见符号时用标准字体 Helvetica，不嵌入；有别的字（中文）时挑一份
//!   覆盖这段文字的系统字体（先找黑体），只嵌用到的字（[`magpdf_text::compose`]），找不到就报参数不合法。
//! - Code 128 是自己编的（全是数字且位数为偶数时用 C 码集，否则用 B 码集），没有用方案定的 Zint；
//!   和 ZXing 的识别对过（`tests/prototypes/components/`）。
//! - 改参数时只保留放置的位置（[`ComponentSpec::build`] 的锚点），用户做过的缩放、旋转不保留。

use magpdf_text::compose::{ComposeError, new_text_object};
use magpdf_text::fontmatch::Family;
use pdfium_sys::{DetachedObject, LoadedFont, Matrix, Page, PageObject, Rgba};

/// 标记内容的名字。
pub const MARK: &str = "MagComponent";

/// 文本组件最多几个字。
const TEXT_MAX_CHARS: usize = 100;

/// 生成组件失败。
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error(transparent)]
    Pdfium(#[from] pdfium_sys::Error),
    /// 找不到能显示这些字的字体（或者嵌不进去），说明可以直接给用户看。
    #[error("{0}")]
    Font(String),
}

/// 组件的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    Text,
    Code128,
}

impl ComponentKind {
    /// 协议和标记内容里的名字。
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Code128 => "code128",
        }
    }

    /// 历史记录里的叫法。西文名两边带空格，夹在中文里不用再加。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Text => "文本",
            Self::Code128 => " Code 128 ",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Self::Text),
            "code128" => Some(Self::Code128),
            _ => None,
        }
    }

    /// 没给尺寸时用的：文本的字号、条码的条高（pt）。
    pub fn default_size(self) -> f32 {
        match self {
            Self::Text => 12.0,
            Self::Code128 => 36.0,
        }
    }

    /// 没给内容时用的。
    pub fn default_content(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Code128 => "SAMPLE-0001",
        }
    }

    fn size_range(self) -> (f32, f32) {
        match self {
            Self::Text => (4.0, 200.0),
            Self::Code128 => (10.0, 300.0),
        }
    }
}

/// 一个组件的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentSpec {
    pub kind: ComponentKind,
    pub content: String,
    /// 文本的字号，或条码的条高，单位 pt。
    pub size: f32,
}

/// 条码一个模块的宽度（pt），约 0.35 mm。
const MODULE: f32 = 1.0;

/// 参数哪里不对：`key` 是参数名（`content`、`size`），`message` 可以直接显示给用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentProblem {
    pub key: &'static str,
    pub message: String,
}

impl ComponentSpec {
    /// 参数能不能生成。
    pub fn validate(&self) -> Result<(), ComponentProblem> {
        let content = |message: &str| ComponentProblem {
            key: "content",
            message: message.to_owned(),
        };
        if self.content.is_empty() {
            return Err(content("内容不能是空的"));
        }
        match self.kind {
            ComponentKind::Text => {
                if self.content.chars().any(char::is_control) {
                    return Err(content(
                        "原型的文本组件只有一行，不能有换行、制表符这类控制字符",
                    ));
                }
                if self.content.chars().count() > TEXT_MAX_CHARS {
                    return Err(content("文本最多 100 个字"));
                }
            }
            ComponentKind::Code128 => {
                if !self.content.chars().all(|c| (' '..='~').contains(&c)) {
                    return Err(content(
                        "Code 128 只能编码英文字母、数字和常见符号（ASCII 32–126）",
                    ));
                }
                if self.content.chars().count() > 48 {
                    return Err(content("条码内容最多 48 个字符"));
                }
            }
        }
        let (min, max) = self.kind.size_range();
        if !(self.size.is_finite() && (min..=max).contains(&self.size)) {
            let what = match self.kind {
                ComponentKind::Text => "字号",
                ComponentKind::Code128 => "条高",
            };
            return Err(ComponentProblem {
                key: "size",
                message: format!("{what}要在 {min}–{max} pt 之间"),
            });
        }
        Ok(())
    }

    /// 生成对象，锚点放在 `(left, top)`（PDF 用户空间），带上记参数的标记内容。还没插进页面。
    /// 锚点：条码是包围盒的左上角；文字是基线往上一个字号处，大致是大写字母的左上角。
    pub fn build(
        &self,
        page: &Page<'_>,
        left: f32,
        top: f32,
    ) -> Result<DetachedObject, BuildError> {
        let object = match self.kind {
            ComponentKind::Text if !self.content.is_ascii() => {
                let made = new_text_object(
                    page.document(),
                    page,
                    &self.content,
                    self.size,
                    Matrix::translation(left, top - self.size),
                    Family::Hei,
                )
                .map_err(|error| match error {
                    ComposeError::NoFont { .. } => BuildError::Font(error.to_string()),
                    ComposeError::Failed(reason) => {
                        BuildError::Font(format!("这段文字的字体嵌不进去：{reason}"))
                    }
                })?;
                DetachedObject::adopt(made.object)
            }
            ComponentKind::Text => {
                let font = LoadedFont::load_standard(page.document(), "Helvetica")?;
                // Helvetica 的大写字母高约 0.72 em：基线放在左上角往下一个字号的地方。
                let object = PageObject::new_text(
                    page,
                    &font,
                    self.size,
                    Matrix::translation(left, top - self.size),
                )?;
                let object = DetachedObject::adopt(object);
                let live = object.as_object();
                live.set_text(&self.content)?;
                live.set_fill_color(Rgba {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                })?;
                object
            }
            ComponentKind::Code128 => {
                let modules =
                    code128_modules(&self.content).map_err(|_| pdfium_sys::Error::PageObject)?;
                let bottom = top - self.size;
                let object =
                    PageObject::new_path(left, bottom).ok_or(pdfium_sys::Error::PageObject)?;
                let object = DetachedObject::adopt(object);
                let live = object.as_object();
                let mut x = left;
                let mut first = true;
                for (index, width) in modules.iter().enumerate() {
                    let w = f32::from(*width) * MODULE;
                    if index.is_multiple_of(2) {
                        // 条（黑），一个矩形子路径
                        if !first {
                            live.move_to(x, bottom)?;
                        }
                        first = false;
                        live.line_to(x + w, bottom)?;
                        live.line_to(x + w, top)?;
                        live.line_to(x, top)?;
                        live.close_path()?;
                    }
                    x += w;
                }
                live.set_path_fill(true, false)?;
                live.set_fill_color(Rgba {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                })?;
                object
            }
        };
        let size = format!("{}", self.size);
        object.as_object().add_string_mark(
            page.document(),
            MARK,
            &[
                ("type", self.kind.name()),
                ("content", &self.content),
                ("size", &size),
            ],
        )?;
        Ok(object)
    }

    /// 从对象的标记内容读回参数；不是组件时为 `None`。
    pub fn read(object: &PageObject<'_>) -> Option<Self> {
        let kind = ComponentKind::parse(&object.mark_string(MARK, "type")?)?;
        let content = object.mark_string(MARK, "content")?;
        let size = object
            .mark_string(MARK, "size")
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| kind.default_size());
        Some(Self {
            kind,
            content,
            size,
        })
    }
}

/// Code 128 的 107 个符号（值 0–105 和终止符 106），每个是条、空交替的模块宽度（从条开始）。
const PATTERNS: [&str; 107] = [
    "212222", "222122", "222221", "121223", "121322", "131222", "122213", "122312", "132212",
    "221213", "221312", "231212", "112232", "122132", "122231", "113222", "123122", "123221",
    "223211", "221132", "221231", "213212", "223112", "312131", "311222", "321122", "321221",
    "312212", "322112", "322211", "212123", "212321", "232121", "111323", "131123", "131321",
    "112313", "132113", "132311", "211313", "231113", "231311", "112133", "112331", "132131",
    "113123", "113321", "133121", "313121", "211331", "231131", "213113", "213311", "213131",
    "311123", "311321", "331121", "312113", "312311", "332111", "314111", "221411", "431111",
    "111224", "111422", "121124", "121421", "141122", "141221", "112214", "112412", "122114",
    "122411", "142112", "142211", "241211", "221114", "413111", "241112", "134111", "111242",
    "121142", "121241", "114212", "124112", "124211", "411212", "421112", "421211", "212141",
    "214121", "412121", "111143", "111341", "131141", "114113", "114311", "411113", "411311",
    "113141", "114131", "311141", "411131", "211412", "211214", "211232", "2331112",
];
const START_B: u8 = 104;
const START_C: u8 = 105;
const STOP: u8 = 106;

/// 内容编成的符号值：起始符、数据、校验符、终止符。
pub fn code128_symbols(content: &str) -> Result<Vec<u8>, String> {
    let bytes = content.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(|b| (32..=126).contains(b)) {
        return Err("Code 128 只能编码 ASCII 32–126".to_owned());
    }
    let mut symbols = Vec::with_capacity(bytes.len() + 3);
    if bytes.len().is_multiple_of(2) && bytes.iter().all(u8::is_ascii_digit) {
        symbols.push(START_C);
        for pair in bytes.chunks(2) {
            symbols.push((pair[0] - b'0') * 10 + (pair[1] - b'0'));
        }
    } else {
        symbols.push(START_B);
        symbols.extend(bytes.iter().map(|b| b - 32));
    }
    let checksum = symbols
        .iter()
        .enumerate()
        .map(|(i, &v)| u32::from(v) * u32::try_from(i.max(1)).unwrap_or(1))
        .sum::<u32>()
        % 103;
    symbols.push(u8::try_from(checksum).unwrap_or(0));
    symbols.push(STOP);
    Ok(symbols)
}

/// 内容编成的条、空模块宽度，从条开始交替。
pub fn code128_modules(content: &str) -> Result<Vec<u8>, String> {
    Ok(code128_symbols(content)?
        .into_iter()
        .flat_map(|s| PATTERNS[usize::from(s)].bytes().map(|d| d - b'0'))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_symbol_is_eleven_modules_and_the_stop_is_thirteen() {
        for (value, pattern) in PATTERNS.iter().enumerate() {
            let sum: u32 = pattern.bytes().map(|d| u32::from(d - b'0')).sum();
            let (len, total) = if value == 106 { (7, 13) } else { (6, 11) };
            assert_eq!((pattern.len(), sum), (len, total), "符号 {value}");
        }
        let mut unique = PATTERNS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 107, "符号不能重复");
    }

    #[test]
    fn checksum_and_code_set_choice() {
        // B 码集：A=33、B=34、C=35，校验 (104 + 33·1 + 34·2 + 35·3) mod 103 = 1
        assert_eq!(
            code128_symbols("ABC").unwrap(),
            vec![104, 33, 34, 35, 1, 106]
        );
        // 全是数字且位数为偶数：C 码集两位一个符号，(105 + 12·1 + 34·2) mod 103 = 82
        assert_eq!(code128_symbols("1234").unwrap(), vec![105, 12, 34, 82, 106]);
        // 奇数位数字走 B 码集
        assert_eq!(code128_symbols("123").unwrap()[0], START_B);
        assert!(code128_symbols("提单").is_err());
        assert_eq!(
            code128_modules("ABC")
                .unwrap()
                .iter()
                .map(|&m| u32::from(m))
                .sum::<u32>(),
            11 * 5 + 13
        );
    }

    #[test]
    fn validation_messages() {
        let spec = |kind, content: &str, size| ComponentSpec {
            kind,
            content: content.to_owned(),
            size,
        };
        assert!(spec(ComponentKind::Text, "Hello", 12.0).validate().is_ok());
        assert!(spec(ComponentKind::Text, "", 12.0).validate().is_err());
        assert!(
            spec(ComponentKind::Text, "提单号 BL-0042", 12.0)
                .validate()
                .is_ok()
        );
        let problem = spec(ComponentKind::Text, "两\n行", 12.0)
            .validate()
            .unwrap_err();
        assert_eq!(problem.key, "content");
        assert!(problem.message.contains("一行"));
        assert!(
            spec(ComponentKind::Text, &"字".repeat(101), 12.0)
                .validate()
                .is_err()
        );
        assert!(
            spec(ComponentKind::Code128, "提单", 36.0)
                .validate()
                .is_err()
        );
        assert!(
            spec(ComponentKind::Code128, "ABC-123", 36.0)
                .validate()
                .is_ok()
        );
        let problem = spec(ComponentKind::Code128, "ABC", 5.0)
            .validate()
            .unwrap_err();
        assert_eq!(problem.key, "size");
        assert!(problem.message.contains("条高"));
        assert!(
            spec(ComponentKind::Code128, &"9".repeat(49), 36.0)
                .validate()
                .is_err()
        );
    }
}
