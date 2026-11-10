//! 修改已有文字的三级回退（方案 5.2、V08「改一行字」）。
//!
//! 顺序：
//! 1. **用原字体**：嵌入子集里已经有这些字形 → 通过 [`FontMap`] 反查字符码 →
//!    `FPDFText_SetCharcodes`，不动字体、尽量保持原样式；
//! 2. **补字形**：嵌入的是整字体，或系统里有同名字体 → 塑形、子集化、把新字体载入文档，
//!    用新文字对象替换旧对象；
//! 3. **换字体**：用 [`FontLibrary`] 按族/字重/斜体匹配 → 同样嵌入子集（Identity-H + ToUnicode），
//!    并在结果里把 [`EditOutcome::font_replaced`] 设为 `true` 供 UI 提示。
//!
//! 「某一级不适用」用 [`TierSkipped`] 记下原因，继续下一级；三级都不行才返回
//! [`crate::EditError::NoTier`]。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use pdfium_sys::{Document, LoadedFont, Page, PageObject};

use crate::error::{EditError, Result};
use crate::fontfile::{FontFace, coverage};
use crate::fontmap::{self, EncodingKind, FontMap};
use crate::fontmatch::{FLAG_ITALIC, Family, FontLibrary};
use crate::glyph::{Glyph, PageText};
use crate::layout::{LayoutParams, analyze};
use crate::shaping;
use crate::subset::{self, Subset, subset_face};

/// 要改哪一段文字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    /// 布局分析得到的行号（从 0 开始）。
    Line { index: usize },
    /// [`PageText::glyphs`] 里的半开区间 `[start, end)`。
    GlyphRange { start: usize, end: usize },
}

impl EditTarget {
    pub fn line(index: usize) -> Self {
        Self::Line { index }
    }

    pub fn glyph_range(start: usize, end: usize) -> Self {
        Self::GlyphRange { start, end }
    }
}

/// 一次改字请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    /// 页码，从 0 开始。
    pub page: usize,
    pub target: EditTarget,
    pub new_text: String,
}

impl EditRequest {
    pub fn line(page: usize, line: usize, new_text: impl Into<String>) -> Self {
        Self {
            page,
            target: EditTarget::line(line),
            new_text: new_text.into(),
        }
    }
}

/// 实际落下的回退级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTier {
    Tier1,
    Tier2,
    Tier3,
}

impl EditTier {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Tier1 => 1,
            Self::Tier2 => 2,
            Self::Tier3 => 3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tier1 => "tier1",
            Self::Tier2 => "tier2",
            Self::Tier3 => "tier3",
        }
    }
}

impl fmt::Display for EditTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 某一级被跳过的原因（软失败，不是流程终止）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierSkipped {
    MapUnavailable { reason: String },
    MissingGlyphs { tier: EditTier, chars: Vec<char> },
    NotEmbedded,
    NoSameNameFont { name: String },
    NoFamilyMatch { family: String },
    Shape { reason: String },
    Subset { reason: String },
    LoadFont { reason: String },
    SetCharcodesFailed,
    ReplaceFailed { reason: String },
}

impl fmt::Display for TierSkipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapUnavailable { reason } => write!(f, "第 1 级：字体映射不可用（{reason}）"),
            Self::MissingGlyphs { tier, chars } => {
                write!(f, "{tier}：缺少 {} 个字形", chars.len())
            }
            Self::NotEmbedded => f.write_str("第 2 级：嵌入的不是整字体"),
            Self::NoSameNameFont { name } => {
                write!(f, "第 2 级：系统里找不到同名字体 {name}")
            }
            Self::NoFamilyMatch { family } => {
                write!(f, "第 3 级：按族 {family:?} 匹配不到能覆盖新字的字体")
            }
            Self::Shape { reason } => write!(f, "塑形失败：{reason}"),
            Self::Subset { reason } => write!(f, "子集化失败：{reason}"),
            Self::LoadFont { reason } => write!(f, "载入新字体失败：{reason}"),
            Self::SetCharcodesFailed => f.write_str("第 1 级：SetCharcodes 失败"),
            Self::ReplaceFailed { reason } => write!(f, "替换文字对象失败：{reason}"),
        }
    }
}

/// 改字成功后的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOutcome {
    pub tier: EditTier,
    /// 第 3 级换了字体时为 `true`，UI 应提示用户。
    pub font_replaced: bool,
    pub skipped: Vec<TierSkipped>,
    /// 保存前后的字节差；本次改页内不统计，留给调用方保存后再填。
    pub bytes_delta: Option<i64>,
}

impl EditOutcome {
    fn ok(tier: EditTier, font_replaced: bool, skipped: Vec<TierSkipped>) -> Self {
        Self {
            tier,
            font_replaced,
            skipped,
            bytes_delta: None,
        }
    }
}

/// 改一行（或一段字形）的文字，按 5.2 的三级回退执行。
///
/// 成功时已经调用过 [`Page::generate_content`]；调用方负责 `save`。
pub fn edit_line(doc: &Document<'_>, req: &EditRequest) -> Result<EditOutcome> {
    let page = doc.page(req.page)?;
    let params = LayoutParams::default();
    let page_text = PageText::from_page(&page, params.ruling_max_thickness)
        .map_err(|_| EditError::NoTextPage)?;
    let layout = analyze(&page_text, &params);
    let glyph_indices = resolve_target(&page_text, &layout, req)?;
    let resolved = inspect_target(&page_text, &glyph_indices)?;
    apply_tiers(doc, &page, &resolved, &req.new_text)
}

fn resolve_target(
    page_text: &PageText,
    layout: &crate::layout::Layout,
    req: &EditRequest,
) -> Result<Vec<usize>> {
    match req.target {
        EditTarget::Line { index } => {
            let line = layout.lines.get(index).ok_or(EditError::NoSuchLine {
                page: req.page,
                line: index,
                lines: layout.lines.len(),
            })?;
            Ok(line.glyphs.clone())
        }
        EditTarget::GlyphRange { start, end } => {
            if start > end || end > page_text.glyphs.len() {
                return Err(EditError::NoSuchLine {
                    page: req.page,
                    line: start,
                    lines: page_text.glyphs.len(),
                });
            }
            Ok((start..end).collect())
        }
    }
}

struct ResolvedLine {
    object_id: usize,
    sample: String,
    font_name: String,
    weight: i32,
    italic: bool,
}

fn inspect_target(page_text: &PageText, indices: &[usize]) -> Result<ResolvedLine> {
    let glyphs: Vec<&Glyph> = indices
        .iter()
        .filter_map(|&i| page_text.glyphs.get(i))
        .filter(|g| !g.generated)
        .collect();
    if glyphs.is_empty() {
        return Err(EditError::NotEditable);
    }
    if glyphs.iter().any(|g| g.blocks_editing()) {
        return Err(EditError::NotEditable);
    }
    let mut obj_ids: Vec<usize> = glyphs.iter().filter_map(|g| g.text_object).collect();
    obj_ids.sort_unstable();
    obj_ids.dedup();
    if obj_ids.len() != 1 {
        return Err(EditError::MixedLine);
    }
    let font_ids: BTreeSet<u32> = glyphs.iter().map(|g| g.font_id).collect();
    let mut font_ids = font_ids.into_iter();
    let (Some(font_id), None) = (font_ids.next(), font_ids.next()) else {
        return Err(EditError::MixedLine);
    };
    let info = page_text.font(font_id);
    let sample: String = glyphs
        .iter()
        .filter_map(|g| char::from_u32(g.unicode))
        .collect();
    Ok(ResolvedLine {
        object_id: obj_ids[0],
        sample,
        font_name: info.map(|f| f.name.clone()).unwrap_or_default(),
        weight: info.and_then(|f| f.weight).unwrap_or(400),
        italic: info.is_some_and(|f| f.flags & FLAG_ITALIC as i32 != 0),
    })
}

fn apply_tiers(
    doc: &Document<'_>,
    page: &Page<'_>,
    line: &ResolvedLine,
    new_text: &str,
) -> Result<EditOutcome> {
    let mut skipped = Vec::new();
    match try_tier1(page, line, new_text) {
        Ok(()) => return Ok(EditOutcome::ok(EditTier::Tier1, false, skipped)),
        Err(SkipOrErr::Skip(s)) => skipped.push(s),
        Err(SkipOrErr::Err(e)) => return Err(e),
    }
    match try_tier2(doc, page, line, new_text) {
        Ok(()) => return Ok(EditOutcome::ok(EditTier::Tier2, false, skipped)),
        Err(SkipOrErr::Skip(s)) => skipped.push(s),
        Err(SkipOrErr::Err(e)) => return Err(e),
    }
    match try_tier3(doc, page, line, new_text) {
        Ok(()) => return Ok(EditOutcome::ok(EditTier::Tier3, true, skipped)),
        Err(SkipOrErr::Skip(s)) => skipped.push(s),
        Err(SkipOrErr::Err(e)) => return Err(e),
    }
    let detail = skipped
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("；");
    Err(EditError::NoTier(detail))
}

enum SkipOrErr {
    Skip(TierSkipped),
    Err(EditError),
}

impl From<EditError> for SkipOrErr {
    fn from(value: EditError) -> Self {
        Self::Err(value)
    }
}

fn object_index(page: &Page<'_>, handle_id: usize) -> Result<usize> {
    for i in 0..page.object_count() {
        if page.object(i).is_some_and(|o| o.handle_id() == handle_id) {
            return Ok(i);
        }
    }
    Err(EditError::NoTextPage)
}

fn text_object<'p>(page: &'p Page<'_>, handle_id: usize) -> Result<(usize, PageObject<'p>)> {
    let idx = object_index(page, handle_id)?;
    let obj = page.object(idx).ok_or(EditError::NoTextPage)?;
    if !obj.is_text() {
        return Err(EditError::NoTextPage);
    }
    Ok((idx, obj))
}

/// 从页面文字对象的字体构造 [`FontMap`]。
///
/// 原版 PDFium 没有 `FPDFFont_GetCharcodeFromUnicode`，这里用嵌入字体程序的 cmap：
/// Type0/Identity-H 下字符码等于 GID；若探测到按 Unicode 单字节当字符码（简单字体的常见情况），
/// 则按简单编码建表。未嵌入时不能用 PDFium 的替代字体 cmap，否则字符码会对不上。
pub fn font_map_from_page_font(font: &pdfium_sys::Font<'_>, sample: &str, needed: &str) -> FontMap {
    if font.is_type3() {
        return FontMap::Unavailable {
            reason: "Type3 字体".into(),
        };
    }
    if font.is_embedded() == Some(true) {
        let Some(data) = font.font_data() else {
            return FontMap::Unavailable {
                reason: "读不到嵌入字体程序".into(),
            };
        };
        let face = match FontFace::from_bytes(data, 0, "embedded") {
            Ok(f) => f,
            Err(e) => {
                return FontMap::Unavailable {
                    reason: e.to_string(),
                };
            }
        };
        let encoding = detect_encoding(font, &face, sample);
        return build_map_from_face(&face, encoding);
    }
    // 未嵌入：不能用 PDFium 替代字体的 cmap（字符码会对不上）。
    // 同名系统字体的 cmap 与阅读器解析该 BaseFont 时一致，简单改写可以留在第 1 级。
    let base = font.base_name();
    let name = subset::strip_subset_prefix(&base);
    if let Some(cand) = same_name_font(FontLibrary::shared(), name) {
        match FontFace::from_path(&cand.path, cand.index) {
            Ok(face) => {
                // 用页面字体的 glyph_width 判断 Identity-H 还是简单编码。
                // 同名系统字体被 PDFium 选为替代字体时，宽度与真实字符码一致。
                let encoding = detect_encoding(font, &face, sample);
                return build_map_from_face_chars(&face, encoding, needed);
            }
            Err(e) => {
                return FontMap::Unavailable {
                    reason: format!("同名系统字体无法打开：{e}"),
                };
            }
        }
    }
    FontMap::Unavailable {
        reason: "字体未嵌入，不能用替代字体的 cmap 反查原字符码".into(),
    }
}

fn detect_encoding(font: &pdfium_sys::Font<'_>, face: &FontFace, sample: &str) -> EncodingKind {
    let probe = sample.chars().find_map(|ch| {
        if ch.is_whitespace() {
            return None;
        }
        face.glyph_for_char(ch).map(|gid| (ch, gid))
    });
    let Some((ch, gid)) = probe else {
        return if sample.chars().any(|c| (c as u32) > 0x7F) {
            EncodingKind::IdentityH
        } else {
            EncodingKind::Simple
        };
    };
    let size = 12.0;
    let w_gid = font.glyph_width(u32::from(gid), size);
    let w_simple = if (ch as u32) <= 255 {
        font.glyph_width(ch as u32, size)
    } else {
        None
    };
    if w_gid.is_some_and(|w| w > 0.0) {
        EncodingKind::IdentityH
    } else if w_simple.is_some_and(|w| w > 0.0) {
        EncodingKind::Simple
    } else if (ch as u32) > 0x7F {
        EncodingKind::IdentityH
    } else {
        EncodingKind::Simple
    }
}

fn build_map_from_face(face: &FontFace, encoding: EncodingKind) -> FontMap {
    let mut charcode_to_unicode = Vec::new();
    let mut charcode_to_gid = Vec::new();
    for gid in 0..face.num_glyphs() {
        let Some(ch) = face.char_for_glyph(gid) else {
            continue;
        };
        let code = match encoding {
            EncodingKind::Simple if (ch as u32) <= 255 => ch as u32,
            _ => u32::from(gid),
        };
        charcode_to_unicode.push((code, ch as u32));
        charcode_to_gid.push((code, u32::from(gid)));
    }
    fontmap::build_table(encoding, charcode_to_unicode, charcode_to_gid)
}

/// 只为需要的字符建表。未嵌入系统字体往往是整份 CJK，遍历全部 GID 在 debug 下会极慢。
fn build_map_from_face_chars(face: &FontFace, encoding: EncodingKind, needed: &str) -> FontMap {
    let mut charcode_to_unicode = Vec::new();
    let mut charcode_to_gid = Vec::new();
    let mut seen = Vec::new();
    for ch in needed.chars() {
        if seen.contains(&ch) {
            continue;
        }
        seen.push(ch);
        let Some(gid) = face.glyph_for_char(ch) else {
            continue;
        };
        let code = match encoding {
            EncodingKind::Simple if (ch as u32) <= 255 => ch as u32,
            _ => u32::from(gid),
        };
        charcode_to_unicode.push((code, ch as u32));
        charcode_to_gid.push((code, u32::from(gid)));
    }
    fontmap::build_table(encoding, charcode_to_unicode, charcode_to_gid)
}

fn try_tier1(
    page: &Page<'_>,
    line: &ResolvedLine,
    new_text: &str,
) -> std::result::Result<(), SkipOrErr> {
    let (_idx, obj) = text_object(page, line.object_id)?;
    let font = obj.font().ok_or(EditError::NoTextPage)?;
    if font.is_type3() {
        return Err(SkipOrErr::Err(EditError::Type3Font));
    }
    let needed: String = line.sample.chars().chain(new_text.chars()).collect();
    let map = font_map_from_page_font(&font, &line.sample, &needed);
    if let FontMap::Unavailable { reason } = &map {
        return Err(SkipOrErr::Skip(TierSkipped::MapUnavailable {
            reason: reason.clone(),
        }));
    }
    let charcodes = match fontmap::tier1_charcodes(&map, new_text) {
        Ok(codes) => codes,
        Err(chars) => {
            return Err(SkipOrErr::Skip(TierSkipped::MissingGlyphs {
                tier: EditTier::Tier1,
                chars,
            }));
        }
    };
    // 优先 SetText：靠原字体 ToUnicode 反查字符码，避免「按 GID 当 CID」在非 Identity
    // 字体上写坏内容流（例如 DengXian 子集会出现整段文字被清空）。
    // SetText 失败再退回 SetCharcodes（Identity-H 且 ToUnicode 不完整时仍可用）。
    // 未嵌入时 SetText 可能走 PDFium 替代字体 cmap，字符码会写错，只用同名系统字体推出的码。
    let wrote = if font.is_embedded() == Some(true) {
        match obj.set_text(new_text) {
            Ok(()) => true,
            Err(_) => obj.set_charcodes(&charcodes),
        }
    } else {
        obj.set_charcodes(&charcodes)
    };
    if !wrote {
        return Err(SkipOrErr::Skip(TierSkipped::SetCharcodesFailed));
    }
    obj.mark_modified().map_err(EditError::from)?;
    page.generate_content().map_err(EditError::from)?;
    Ok(())
}

fn try_tier2(
    doc: &Document<'_>,
    page: &Page<'_>,
    line: &ResolvedLine,
    new_text: &str,
) -> std::result::Result<(), SkipOrErr> {
    let (idx, obj) = text_object(page, line.object_id)?;
    let font = obj.font().ok_or(EditError::NoTextPage)?;
    if font.is_type3() {
        return Err(SkipOrErr::Err(EditError::Type3Font));
    }
    let base = if font.base_name().is_empty() {
        line.font_name.clone()
    } else {
        font.base_name()
    };
    let name = subset::strip_subset_prefix(&base).to_owned();

    if let Some(face) = embedded_full_face(&font, new_text) {
        return replace_with_face(doc, page, idx, &obj, &face, new_text);
    }
    let lib = FontLibrary::shared();
    let Some(cand) = same_name_font(lib, &name) else {
        return Err(SkipOrErr::Skip(TierSkipped::NoSameNameFont { name }));
    };
    let face = FontFace::from_path(&cand.path, cand.index)?;
    let miss = coverage(&face, new_text).missing;
    if !miss.is_empty() {
        return Err(SkipOrErr::Skip(TierSkipped::MissingGlyphs {
            tier: EditTier::Tier2,
            chars: miss,
        }));
    }
    replace_with_face(doc, page, idx, &obj, &face, new_text)
}

fn embedded_full_face(font: &pdfium_sys::Font<'_>, new_text: &str) -> Option<FontFace> {
    if font.is_embedded() != Some(true) {
        return None;
    }
    let data = font.font_data()?;
    let face = FontFace::from_bytes(data, 0, "embedded").ok()?;
    let miss = coverage(&face, new_text).missing;
    if miss.is_empty() {
        // 名字带子集前缀但数据其实覆盖了新字：仍可作为「补字形」来源
        //（嵌入子集恰好含新字时会在第 1 级就处理，走到这里说明第 1 级映射不可用）。
        Some(face)
    } else {
        None
    }
}

fn try_tier3(
    doc: &Document<'_>,
    page: &Page<'_>,
    line: &ResolvedLine,
    new_text: &str,
) -> std::result::Result<(), SkipOrErr> {
    let (idx, obj) = text_object(page, line.object_id)?;
    let font = obj.font().ok_or(EditError::NoTextPage)?;
    let base = if font.base_name().is_empty() {
        line.font_name.clone()
    } else {
        font.base_name()
    };
    let name = subset::strip_subset_prefix(&base);
    let family = Family::from_name(name);
    let lib = FontLibrary::shared();
    let Some(cand) = covering_match(lib, family, line.weight, line.italic, new_text) else {
        return Err(SkipOrErr::Skip(TierSkipped::NoFamilyMatch {
            family: format!("{family:?}"),
        }));
    };
    let face = FontFace::from_path(&cand.path, cand.index)?;
    replace_with_face(doc, page, idx, &obj, &face, new_text)
}

fn covering_match<'a>(
    lib: &'a FontLibrary,
    family: Family,
    weight: i32,
    italic: bool,
    text: &str,
) -> Option<&'a crate::fontmatch::Candidate> {
    if let Some(c) = lib.best_match(family, weight, italic)
        && face_covers(c, text)
    {
        return Some(c);
    }
    lib.candidates
        .iter()
        .filter(|c| c.embeddable() && c.family == family)
        .find(|c| face_covers(c, text))
        .or_else(|| {
            lib.candidates
                .iter()
                .filter(|c| c.embeddable())
                .find(|c| face_covers(c, text))
        })
}

fn face_covers(cand: &crate::fontmatch::Candidate, text: &str) -> bool {
    FontFace::from_path(&cand.path, cand.index)
        .ok()
        .is_some_and(|f| coverage(&f, text).missing.is_empty())
}

fn replace_with_face(
    doc: &Document<'_>,
    page: &Page<'_>,
    old_index: usize,
    old: &PageObject<'_>,
    face: &FontFace,
    new_text: &str,
) -> std::result::Result<(), SkipOrErr> {
    let (subset, charcodes) = match subset_text(face, new_text) {
        Ok(v) => v,
        Err(SkipOrErr::Skip(s)) => return Err(SkipOrErr::Skip(s)),
        Err(e) => return Err(e),
    };
    let loaded = load_subset_font(doc, &subset)
        .map_err(|reason| SkipOrErr::Skip(TierSkipped::LoadFont { reason }))?;
    let matrix = old.matrix().ok_or_else(|| {
        SkipOrErr::Skip(TierSkipped::ReplaceFailed {
            reason: "读不到文字矩阵".into(),
        })
    })?;
    let size = old.font_size().unwrap_or(12.0);
    let fill = old.fill_color();
    let stroke = old.stroke_color();
    let mode = old.text_render_mode();
    let new_obj = PageObject::new_text(page, &loaded, size, matrix).map_err(EditError::from)?;
    // 复合字体走 Identity-H，字符码必须是子集后的 GID；不要用 SetText（反查 cmap 常得到空 ToUnicode）。
    if !new_obj.set_charcodes(&charcodes) {
        page.destroy_object(new_obj);
        return Err(SkipOrErr::Skip(TierSkipped::SetCharcodesFailed));
    }
    if let Some(c) = fill {
        let _ = new_obj.set_fill_color(c);
    }
    if let Some(c) = stroke {
        let _ = new_obj.set_stroke_color(c);
    }
    if let Some(m) = mode {
        let _ = new_obj.set_text_render_mode(m);
    }
    // `remove_object` 会销毁旧对象，必须在插入新对象之前用它的句柄。
    // 这里重新按 index 取出旧对象再移除，避免和 `old` 借用冲突。
    let old_live = page.object(old_index).ok_or(EditError::NoTextPage)?;
    page.remove_object(old_live).map_err(EditError::from)?;
    page.insert_object_at(new_obj, old_index)
        .map_err(EditError::from)?;
    page.generate_content().map_err(EditError::from)?;
    Ok(())
}

fn subset_text(face: &FontFace, text: &str) -> std::result::Result<(Subset, Vec<u32>), SkipOrErr> {
    let miss = coverage(face, text).missing;
    if !miss.is_empty() {
        return Err(SkipOrErr::Skip(TierSkipped::MissingGlyphs {
            tier: EditTier::Tier2,
            chars: miss,
        }));
    }
    let shaped = shaping::shape(face.data(), face.index(), text).map_err(|e| {
        SkipOrErr::Skip(TierSkipped::Shape {
            reason: e.to_string(),
        })
    })?;
    if !shaped.missing.is_empty() {
        return Err(SkipOrErr::Skip(TierSkipped::MissingGlyphs {
            tier: EditTier::Tier2,
            chars: shaped.missing,
        }));
    }
    let mut glyphs = BTreeSet::new();
    glyphs.insert(0);
    let mut codepoints = BTreeMap::new();
    for ch in text.chars() {
        if let Some(gid) = face.glyph_for_char(ch) {
            glyphs.insert(gid);
            codepoints.insert(gid, ch);
        }
    }
    for g in &shaped.glyphs {
        glyphs.insert(g.gid);
        if let Some(ch) = text.get(g.cluster..).and_then(|s| s.chars().next()) {
            codepoints.entry(g.gid).or_insert(ch);
        }
    }
    let subset = subset_face(face, &glyphs, &codepoints).map_err(|e| {
        SkipOrErr::Skip(TierSkipped::Subset {
            reason: e.to_string(),
        })
    })?;
    // Identity-H：字符码 = 子集后的新 GID。
    let mut charcodes = Vec::with_capacity(shaped.glyphs.len());
    for g in &shaped.glyphs {
        let Some(cc) = subset.charcode(g.gid) else {
            return Err(SkipOrErr::Skip(TierSkipped::Subset {
                reason: format!("塑形 GID {} 不在子集里", g.gid),
            }));
        };
        charcodes.push(cc);
    }
    Ok((subset, charcodes))
}

fn load_subset_font<'doc>(
    doc: &'doc Document<'_>,
    subset: &Subset,
) -> std::result::Result<LoadedFont<'doc>, String> {
    // `Subset::codepoint` 的键已经是子集后的新 GID。
    let mut tu = BTreeMap::new();
    for (new_gid, ch) in &subset.codepoint {
        tu.insert(u32::from(*new_gid), *ch);
    }
    let cmap = subset::to_unicode_cmap(&tu);
    let map = subset::cid_to_gid_map(subset.new_gid.values().copied());

    if !subset.cff {
        // TrueType：显式 ToUnicode + CIDToGIDMap，避免 LoadFont 只靠 cmap cmap 导致提取成控制字符。
        match LoadedFont::load_cid_type2(doc, &subset.data, &cmap, &map) {
            Ok(font) => return Ok(font),
            Err(e1) => {
                return LoadedFont::load_composite(doc, &subset.data, false)
                    .map_err(|e2| format!("cid_type2: {e1}; composite: {e2}"));
            }
        }
    }
    LoadedFont::load_composite(doc, &subset.data, true).map_err(|e| e.to_string())
}

/// 去掉空白、连字符后的小写名字，用来对「同名字体」做宽松匹配。
pub fn font_name_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 再去掉 PostScript 常见后缀（`PSMT` / `MT` / `PS`，以及 `PS` 与字重词之间的连接），
/// 让 PDF 的 `TimesNewRoman` 能对上系统字体的 `TimesNewRomanPSMT`。
pub fn canon_font_key(name: &str) -> String {
    let mut s = font_name_key(name);
    if let Some(rest) = s.strip_suffix("psmt") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_suffix("mt") {
        s = rest.to_string();
    }
    for style in ["bolditalic", "italic", "oblique", "bold", "regular"] {
        let pat = format!("ps{style}");
        if let Some(rest) = s.strip_suffix(&pat) {
            s = format!("{rest}{style}");
            break;
        }
    }
    if let Some(rest) = s.strip_suffix("ps") {
        s = rest.to_string();
    }
    s
}

fn style_from_font_name(name: &str) -> (i32, bool) {
    let n = font_name_key(name);
    let italic = n.contains("italic") || n.contains("oblique");
    let weight = if n.contains("black") || n.contains("heavy") {
        900
    } else if n.contains("light") || n.contains("thin") {
        300
    } else if n.contains("bold") || n.contains("demibold") || n.contains("semibold") {
        700
    } else {
        400
    };
    (weight, italic)
}

fn candidate_name_keys(c: &crate::fontmatch::Candidate) -> Vec<String> {
    let mut keys = vec![
        font_name_key(&c.name),
        canon_font_key(&c.name),
        font_name_key(&c.family_name),
        canon_font_key(&c.family_name),
    ];
    if let Some(stem) = c.path.file_stem().and_then(|s| s.to_str()) {
        keys.push(font_name_key(stem));
        keys.push(canon_font_key(stem));
    }
    let mut fam_style = c.family_name.clone();
    if c.weight >= 700 {
        fam_style.push_str("Bold");
    }
    if c.italic {
        fam_style.push_str("Italic");
    }
    keys.push(font_name_key(&fam_style));
    keys.push(canon_font_key(&fam_style));
    keys.sort();
    keys.dedup();
    keys
}

fn same_name_font<'a>(lib: &'a FontLibrary, name: &str) -> Option<&'a crate::fontmatch::Candidate> {
    if let Some(c) = lib.by_name(name) {
        return Some(c);
    }
    let q = canon_font_key(name);
    if q.is_empty() {
        return None;
    }
    let mut q_keys = vec![font_name_key(name), q];
    q_keys.sort();
    q_keys.dedup();
    let (qw, qi) = style_from_font_name(name);
    lib.candidates
        .iter()
        .filter(|c| {
            candidate_name_keys(c)
                .iter()
                .any(|k| q_keys.iter().any(|qk| qk == k))
        })
        .min_by_key(|c| {
            (
                (c.weight - qw).abs(),
                u8::from(c.italic != qi),
                u8::from(!c.embeddable()),
                c.name.clone(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fontmap::{build_table, tier1_charcodes};

    #[test]
    fn request_line_helper() {
        let req = EditRequest::line(2, 1, "甲方");
        assert_eq!(req.page, 2);
        assert_eq!(req.target, EditTarget::line(1));
        assert_eq!(req.new_text, "甲方");
    }

    #[test]
    fn font_name_key_strips_separators() {
        assert_eq!(
            font_name_key("Microsoft YaHei"),
            font_name_key("MicrosoftYaHei")
        );
        assert_eq!(font_name_key("Times-New-Roman"), "timesnewroman");
        assert_eq!(font_name_key("ABCDEF+FangSong"), "abcdeffangsong");
    }

    #[test]
    fn canon_font_key_aliases_ps_suffixes() {
        assert_eq!(canon_font_key("Arial"), canon_font_key("ArialMT"));
        assert_eq!(canon_font_key("Arial-Bold"), canon_font_key("Arial-BoldMT"));
        assert_eq!(
            canon_font_key("TimesNewRoman"),
            canon_font_key("TimesNewRomanPSMT")
        );
        assert_eq!(
            canon_font_key("TimesNewRoman-Bold"),
            canon_font_key("TimesNewRomanPS-BoldMT")
        );
        assert_eq!(
            canon_font_key("CourierNew"),
            canon_font_key("CourierNewPSMT")
        );
        assert_eq!(
            canon_font_key("CourierNew-Bold"),
            canon_font_key("CourierNewPS-BoldMT")
        );
        assert_eq!(canon_font_key("Times-New-Roman"), "timesnewroman");
        // 「roman」是 Times New Roman 族名的一部分，不能当字重后缀剥掉。
        assert_eq!(canon_font_key("TimesNewRoman"), "timesnewroman");
    }

    #[test]
    fn same_name_font_matches_common_western_faces() {
        let lib = FontLibrary::shared();
        let some = |n: &str| same_name_font(lib, n);
        // 非 Windows / 没装这些字体的环境直接跳过。
        if some("Arial").is_none() && some("Calibri").is_none() {
            return;
        }
        for name in ["Arial", "TimesNewRoman", "CourierNew", "PalatinoLinotype"] {
            assert!(
                some(name).is_some(),
                "系统里应能按 PDF 名 {name:?} 匹配到同名字体，当前 candidates={}",
                lib.candidates.len()
            );
        }
        if let Some(c) = some("Arial-Bold") {
            assert!(c.weight >= 600, "Arial-Bold 不应落到常规体 {:?}", c.name);
        }
        if let Some(c) = some("TimesNewRoman-Bold") {
            assert!(
                c.weight >= 600,
                "TimesNewRoman-Bold 不应落到常规体 {:?}",
                c.name
            );
        }
    }

    #[test]
    fn tier_display_and_skipped_messages() {
        assert_eq!(EditTier::Tier2.as_u8(), 2);
        assert_eq!(EditTier::Tier1.to_string(), "tier1");
        let skip = TierSkipped::MissingGlyphs {
            tier: EditTier::Tier1,
            chars: vec!['中'],
        };
        assert!(
            skip.to_string().contains('1')
                || skip.to_string().contains("tier1")
                || skip.to_string().contains("缺少")
        );
        let outcome = EditOutcome::ok(EditTier::Tier3, true, vec![skip]);
        assert!(outcome.font_replaced);
        assert_eq!(outcome.skipped.len(), 1);
    }

    #[test]
    fn identity_map_feeds_set_charcodes_path() {
        let map = build_table(
            EncodingKind::IdentityH,
            [(3, 0x5408), (5, 0x540C)], // 合 同
            [(3, 3), (5, 5)],
        );
        assert_eq!(tier1_charcodes(&map, "合同").unwrap(), vec![3, 5]);
        assert!(tier1_charcodes(&map, "价款").is_err());
    }

    #[test]
    fn simple_encoding_uses_unicode_byte_as_charcode() {
        // 不依赖 PDF：直接覆盖简单编码建表的规则。
        // 'A' 的 GID 是 36，简单编码下字符码应是 0x41。
        // 这里用 build_table 模拟 detect_encoding == Simple 的结果。
        let map = build_table(
            EncodingKind::Simple,
            [(0x41, 0x41), (0x42, 0x42)],
            [(0x41, 36), (0x42, 37)],
        );
        assert_eq!(tier1_charcodes(&map, "AB").unwrap(), vec![0x41, 0x42]);
    }
}
