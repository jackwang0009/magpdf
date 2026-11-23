//! 换掉一个文字对象的文字（界面上选中文字、在属性面板里改字）。
//!
//! 和 [`crate::edit`]（V08“改一行字”）用同一套三级回退（方案 5.2）：用原字体 → 补字形 → 换字体。
//! 不同的是**不动页面**：三级都生成一个新的文字对象交给调用方，调用方用它换掉旧对象，撤销时再换回来，
//! 旧对象原样留着。第 1 级也是新对象：用旧对象正在用的那份字体新建，不嵌新字体。
//!
//! 照抄的只有矩阵、字号、填充色、描边色、渲染模式。字间距、词间距、水平缩放这些文字状态 PDFium 的
//! 公开接口读不到，剪切路径和标记内容也不跟过来（原型的限制）。

use pdfium_sys::{Document, Page, PageObject};

use crate::edit::{
    EditTier, SkipOrErr, TierSkipped, build_with_face, copy_text_style, covering_match,
    embedded_full_face, font_map_from_page_font, same_name_font,
};
use crate::error::{EditError, Result};
use crate::fontfile::coverage;
use crate::fontmap::{self, FontMap};
use crate::fontmatch::{FLAG_ITALIC, Family, FontLibrary};
use crate::glyph::PageText;
use crate::layout::LayoutParams;
use crate::subset;

/// 一个文字对象现在的文字。
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectText {
    pub text: String,
    /// 字体名（去掉子集前缀）
    pub font: String,
    /// `Tf` 字号（pt）
    pub size: f32,
    /// 不能改的原因；能改时为 `None`。
    pub blocked: Option<String>,
}

/// 换好的文字对象，还没插进页面。
#[derive(Debug)]
pub struct Retext {
    pub object: PageObject<'static>,
    pub tier: EditTier,
    /// 换了字体（第 3 级）：界面提示“字体已替换”（方案 5.2）。
    pub font_replaced: bool,
    /// 新对象用的字体
    pub font: String,
    /// 前面几级为什么没用上
    pub skipped: Vec<TierSkipped>,
}

/// 这个对象在页面文字里的字（PDFium 排版插入的空格不算）、字体信息和能不能改。
struct Inspected {
    text: String,
    font_name: String,
    weight: i32,
    italic: bool,
    blocked: Option<EditError>,
}

fn inspect(page: &Page<'_>, object: &PageObject<'_>) -> Result<Inspected> {
    if !object.is_text() {
        return Err(EditError::NoTextPage);
    }
    let page_text = PageText::from_page(page, LayoutParams::default().ruling_max_thickness)
        .map_err(|_| EditError::NoTextPage)?;
    let id = object.handle_id();
    let glyphs: Vec<_> = page_text
        .glyphs
        .iter()
        .filter(|g| g.text_object == Some(id) && !g.generated)
        .collect();
    let text: String = glyphs
        .iter()
        .filter_map(|g| char::from_u32(g.unicode))
        .collect();
    let info = glyphs.first().and_then(|g| page_text.font(g.font_id));
    let font = object.font();
    let blocked = if font.is_some_and(|f| f.is_type3()) {
        Some(EditError::Type3Font)
    } else if glyphs.iter().any(|g| g.blocks_editing()) || glyphs.is_empty() {
        Some(EditError::NotEditable)
    } else {
        None
    };
    let font_name = font
        .map(|f| f.base_name())
        .filter(|n| !n.is_empty())
        .or_else(|| info.map(|f| f.name.clone()))
        .unwrap_or_default();
    Ok(Inspected {
        text,
        font_name: subset::strip_subset_prefix(&font_name).to_owned(),
        weight: info.and_then(|f| f.weight).unwrap_or(400),
        italic: info.is_some_and(|f| f.flags & FLAG_ITALIC as i32 != 0),
        blocked,
    })
}

/// 读一个文字对象现在的文字和能不能改。
pub fn object_text(page: &Page<'_>, object: &PageObject<'_>) -> Result<ObjectText> {
    let inspected = inspect(page, object)?;
    Ok(ObjectText {
        text: inspected.text,
        font: inspected.font_name,
        size: object.font_size().unwrap_or(0.0),
        blocked: inspected.blocked.map(|e| match e {
            EditError::Type3Font => {
                "这段文字用的是 Type3 字体，原型改不了（方案 5.5 放到 P4）".to_owned()
            }
            EditError::NotEditable => {
                "这段文字没有 Unicode 映射，读不出原来的字，改不了（方案 5.1 第 2 条）".to_owned()
            }
            other => other.to_string(),
        }),
    })
}

/// 生成一个文字换成 `new_text` 的新对象替换 `object`（三级回退，见本模块开头），页面不动。
pub fn retext_object(
    doc: &Document<'_>,
    page: &Page<'_>,
    object: &PageObject<'_>,
    new_text: &str,
) -> Result<Retext> {
    let inspected = inspect(page, object)?;
    if let Some(blocked) = inspected.blocked {
        return Err(blocked);
    }
    let font = object.font().ok_or(EditError::NoTextPage)?;
    let mut skipped = Vec::new();

    // 第 1 级：原字体里有这些字，用同一份字体新建
    match tier1(page, object, &font, &inspected.text, new_text) {
        Ok(new) => {
            return Ok(Retext {
                object: new,
                tier: EditTier::Tier1,
                font_replaced: false,
                font: inspected.font_name,
                skipped,
            });
        }
        Err(SkipOrErr::Skip(s)) => skipped.push(s),
        Err(SkipOrErr::Err(e)) => return Err(e),
    }

    // 第 2 级：嵌入的是整份字体，或者系统里有同名字体 → 补字形
    let lib = FontLibrary::shared();
    let same = embedded_full_face(&font, new_text).or_else(|| {
        same_name_font(lib, &inspected.font_name)
            .and_then(|c| lib.face(c).ok())
            .filter(|face| coverage(face, new_text).missing.is_empty())
    });
    match same {
        Some(face) => match build_with_face(doc, page, object, &face, new_text) {
            Ok(new) => {
                return Ok(Retext {
                    object: new,
                    tier: EditTier::Tier2,
                    font_replaced: false,
                    font: inspected.font_name,
                    skipped,
                });
            }
            Err(SkipOrErr::Skip(s)) => skipped.push(s),
            Err(SkipOrErr::Err(e)) => return Err(e),
        },
        None => skipped.push(TierSkipped::NoSameNameFont {
            name: inspected.font_name.clone(),
        }),
    }

    // 第 3 级：按族、字重、斜体换一份覆盖这些字的字体
    let family = Family::from_name(&inspected.font_name);
    match covering_match(lib, family, inspected.weight, inspected.italic, new_text) {
        Some(candidate) => {
            let face = lib.face(candidate)?;
            match build_with_face(doc, page, object, &face, new_text) {
                Ok(new) => {
                    return Ok(Retext {
                        object: new,
                        tier: EditTier::Tier3,
                        font_replaced: true,
                        font: face.postscript_name().to_owned(),
                        skipped,
                    });
                }
                Err(SkipOrErr::Skip(s)) => skipped.push(s),
                Err(SkipOrErr::Err(e)) => return Err(e),
            }
        }
        None => skipped.push(TierSkipped::NoFamilyMatch {
            family: format!("{family:?}"),
        }),
    }
    let detail = skipped
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("；");
    Err(EditError::NoTier(detail))
}

/// 不嵌入的标准字体（Helvetica、Times、Courier 和常见的别名 Arial、Times New Roman）、新字都在 WinAnsi
/// 编码里（ASCII 和拉丁 1）：PDFium 自己知道这些字体的编码，用同一份字体新建、`SetText` 就对。
/// 按原字体映射反查的路要先在系统里找同名字体，Windows 上没有叫 Helvetica 的字体，会一路落到换字体。
/// 不是这种情况返回 `None`。
fn standard_font_object(
    page: &Page<'_>,
    old: &PageObject<'_>,
    font: &pdfium_sys::Font<'_>,
    new_text: &str,
) -> std::result::Result<Option<PageObject<'static>>, SkipOrErr> {
    const STANDARD: [&str; 6] = [
        "helvetica",
        "times",
        "courier",
        "arial",
        "timesnewroman",
        "couriernew",
    ];
    let name = subset::strip_subset_prefix(&font.base_name()).to_ascii_lowercase();
    let standard = font.is_embedded() == Some(false)
        && STANDARD
            .iter()
            .any(|s| name.replace([' ', '-', ','], "").starts_with(s));
    let winansi = new_text
        .chars()
        .all(|c| (' '..='~').contains(&c) || ('\u{a0}'..='\u{ff}').contains(&c));
    if !standard || !winansi {
        return Ok(None);
    }
    let matrix = old.matrix().ok_or_else(|| {
        SkipOrErr::Skip(TierSkipped::ReplaceFailed {
            reason: "读不到文字矩阵".into(),
        })
    })?;
    let size = old.font_size().unwrap_or(12.0);
    let new = PageObject::new_text_with_font(page, font, size, matrix).map_err(EditError::from)?;
    if new.set_text(new_text).is_err() {
        page.destroy_object(new);
        return Ok(None);
    }
    copy_text_style(old, &new);
    Ok(Some(new))
}

/// 第 1 级：用旧对象的字体新建，字符码按原字体的映射反查（同 [`crate::edit`] 的第 1 级）。
fn tier1(
    page: &Page<'_>,
    old: &PageObject<'_>,
    font: &pdfium_sys::Font<'_>,
    sample: &str,
    new_text: &str,
) -> std::result::Result<PageObject<'static>, SkipOrErr> {
    if let Some(new) = standard_font_object(page, old, font, new_text)? {
        return Ok(new);
    }
    let needed: String = sample.chars().chain(new_text.chars()).collect();
    let map = font_map_from_page_font(font, sample, &needed);
    if let FontMap::Unavailable { reason } = &map {
        return Err(SkipOrErr::Skip(TierSkipped::MapUnavailable {
            reason: reason.clone(),
        }));
    }
    let charcodes = fontmap::tier1_charcodes(&map, new_text).map_err(|chars| {
        SkipOrErr::Skip(TierSkipped::MissingGlyphs {
            tier: EditTier::Tier1,
            chars,
        })
    })?;
    let matrix = old.matrix().ok_or_else(|| {
        SkipOrErr::Skip(TierSkipped::ReplaceFailed {
            reason: "读不到文字矩阵".into(),
        })
    })?;
    let size = old.font_size().unwrap_or(12.0);
    let new = PageObject::new_text_with_font(page, font, size, matrix).map_err(EditError::from)?;
    // 同 edit 的第 1 级：嵌入字体先试 SetText（按原字体的 ToUnicode 反查），不行再写字符码
    let wrote = if font.is_embedded() == Some(true) {
        new.set_text(new_text).is_ok() || new.set_charcodes(&charcodes)
    } else {
        new.set_charcodes(&charcodes)
    };
    if !wrote {
        page.destroy_object(new);
        return Err(SkipOrErr::Skip(TierSkipped::SetCharcodesFailed));
    }
    copy_text_style(old, &new);
    Ok(new)
}
