//! 新建一段文字（组件原型的文本组件；以后的文字工具）：挑一份覆盖这段文字、许可允许嵌入的字体，塑形、
//! 子集化，以 Identity-H 嵌入（方案 5.3、5.4）。和改字第 3 级（[`crate::edit`]）用同一套字体匹配和子集化。
//!
//! 每次生成都嵌一份只含这段文字的子集：改了文字就再嵌一份新的，旧的随旧对象不再被引用，保存时去掉。

use pdfium_sys::{Document, Matrix, Page, PageObject, Rgba};

use crate::edit::{covering_match, load_subset_font, subset_text};
use crate::fontfile::coverage;
use crate::fontmatch::{Family, FontLibrary};

/// 生成好的文字对象，还没插进页面。
#[derive(Debug)]
pub struct NewText {
    pub object: PageObject<'static>,
    /// 用的字体（字体文件里的 PS 名），诊断用。
    pub font: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    /// 系统字体（和内置字体）里没有一份能覆盖这段文字、又允许嵌入的。
    #[error("找不到能显示这些字的字体：{}", .missing.iter().collect::<String>())]
    NoFont { missing: Vec<char> },
    #[error("{0}")]
    Failed(String),
}

/// 用 `family` 族（找不到就退到同类、再退到任意一份）里覆盖 `text` 的字体生成文字对象，字号 `size`，
/// 文字矩阵 `matrix`（原点在第一个字的基线上），黑色填充。
pub fn new_text_object(
    doc: &Document<'_>,
    page: &Page<'_>,
    text: &str,
    size: f32,
    matrix: Matrix,
    family: Family,
) -> Result<NewText, ComposeError> {
    let lib = FontLibrary::shared();
    let Some(candidate) = covering_match(lib, family, 400, false, text) else {
        let missing = lib
            .best_match(family, 400, false)
            .and_then(|c| lib.face(c).ok())
            .map(|face| coverage(&face, text).missing)
            .unwrap_or_else(|| text.chars().collect());
        return Err(ComposeError::NoFont { missing });
    };
    let face = lib
        .face(candidate)
        .map_err(|e| ComposeError::Failed(e.to_string()))?;
    let (subset, charcodes) =
        subset_text(&face, text).map_err(|e| ComposeError::Failed(e.to_string()))?;
    let loaded = load_subset_font(doc, &subset).map_err(ComposeError::Failed)?;
    let object = PageObject::new_text(page, &loaded, size, matrix)
        .map_err(|e| ComposeError::Failed(e.to_string()))?;
    // 复合字体走 Identity-H：字符码是子集里的新 GID，不能用 SetText（见改字第 3 级）
    if !object.set_charcodes(&charcodes) {
        page.destroy_object(object);
        return Err(ComposeError::Failed("写不进字符码".to_owned()));
    }
    let _ = object.set_fill_color(Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    });
    Ok(NewText {
        object,
        font: face.postscript_name().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有覆盖中文的字体时（GitHub 的 Windows 机器上就缺中文字体）跳过。
    #[test]
    fn chinese_text_gets_an_embedded_subset() {
        let lib = FontLibrary::shared();
        if covering_match(lib, Family::Hei, 400, false, "提单号").is_none() {
            eprintln!("跳过：这台机器上没有覆盖中文的字体");
            return;
        }
        let pdfium = pdfium_sys::Library::init().expect("PDFium");
        let doc = Document::create(&pdfium).unwrap();
        let page = doc.add_blank_page(0, 300.0, 100.0).unwrap();
        let made = new_text_object(
            &doc,
            &page,
            "提单号 BL-0042",
            12.0,
            Matrix::translation(10.0, 50.0),
            Family::Hei,
        )
        .unwrap();
        assert!(!made.font.is_empty());
        page.insert_object_at(made.object, 0).unwrap();
        page.generate_content().unwrap();
        let text: String = {
            let chars = page.text().unwrap();
            (0..chars.char_count())
                .filter_map(|i| chars.unicode(i).and_then(char::from_u32))
                .collect()
        };
        assert_eq!(
            text.trim(),
            "提单号 BL-0042",
            "提取得到原文（ToUnicode 写对了）"
        );
        let bytes = doc
            .save_to_vec(pdfium_sys::SaveFlags::NO_INCREMENTAL)
            .unwrap();
        // 只嵌了用到的几个字：整份中文字体有几 MB
        assert!(bytes.len() < 200_000, "文件 {} 字节", bytes.len());
    }
}
