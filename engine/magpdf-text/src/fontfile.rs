//! 字体文件读取：打开（含 TTC 集合）、按码点取字形、字形度量与轮廓、OS/2 fsType。
//!
//! 用 `skrifa` 读；它同时是 `subsetter` 的依赖（同一版本的 `read-fonts`），不额外引入解析器。
//! 本模块只在改字流程里使用，不参与文字块识别。

use serde::{Deserialize, Serialize};
use skrifa::instance::{LocationRef, Size};
use skrifa::raw::types::Tag;
use skrifa::raw::{FontRef, TableProvider};
use skrifa::{GlyphId, MetadataProvider, OutlineGlyphCollection};

use crate::error::{EditError, Result};

/// OS/2 fsType 的"禁止嵌入"位。
pub const FS_TYPE_RESTRICTED: u16 = 0x0002;
/// OS/2 fsType 的"仅允许位图嵌入"位（可打印预览，但矢量字形不能嵌入）。
pub const FS_TYPE_BITMAP_ONLY: u16 = 0x0200;
/// 可编辑嵌入位（`fsType` 位 3）：置位且未限制时只能以可编辑方式嵌入。
pub const FS_TYPE_EDITABLE: u16 = 0x0008;

/// 按 PDF 的约定判断字体能否用于"写进 PDF"（方案 5.4：禁止嵌入的不用）。
///
/// - `0x0002`（Restricted）：拒绝；
/// - `0x0200`（Bitmap embedding only）：拒绝，我们嵌入的是矢量字形；
/// - 其余（含 Installable、Preview&Print、Editable）允许。Editable 只约束"可编辑",不禁止嵌入。
pub fn fs_type_allows_embedding(fs_type: u16) -> bool {
    fs_type & (FS_TYPE_RESTRICTED | FS_TYPE_BITMAP_ONLY) == 0
}

/// 一个已加载的字体面（TTC 里按索引取其中一个面）。
#[derive(Clone)]
pub struct FontFace {
    data: Vec<u8>,
    /// TTC 中的面索引，非集合为 0。
    index: u32,
    /// 字形数（maxp/numGlyphs）。
    num_glyphs: u16,
    /// `unitsPerEm`。
    units_per_em: u16,
    /// OS/2 fsType；字体没有 OS/2 表时按 0 处理（不可靠，见 [`Self::fs_type`] 的说明）。
    fs_type: Option<u16>,
    /// 字体文件名（不含路径），日志与报告里用。
    origin: String,
    /// 字体内部的 PostScript 名（`name` 表 ID 6），用于子集前缀与 BaseFont。
    postscript_name: String,
}

impl std::fmt::Debug for FontFace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontFace")
            .field("origin", &self.origin)
            .field("index", &self.index)
            .field("num_glyphs", &self.num_glyphs)
            .field("units_per_em", &self.units_per_em)
            .field("fs_type", &self.fs_type)
            .field("postscript_name", &self.postscript_name)
            .finish()
    }
}

impl FontFace {
    /// 从字体文件字节打开第 `index` 个面（TTC 时有效）。
    pub fn from_bytes(data: Vec<u8>, index: u32, origin: &str) -> Result<Self> {
        let font = FontRef::from_index(&data, index)
            .map_err(|e| EditError::FontData(format!("{origin}: 打不开字体面 {index}：{e}")))?;
        let num_glyphs = font
            .maxp()
            .map_err(|e| EditError::FontData(format!("{origin}: 没有 maxp：{e}")))?
            .num_glyphs();
        let units_per_em = font.head().map(|h| h.units_per_em()).unwrap_or(1000);
        let fs_type = font.os2().ok().map(|os2| os2.fs_type());
        let postscript_name = font
            .localized_strings(skrifa::string::StringId::POSTSCRIPT_NAME)
            .english_or_first()
            .map(|s| s.chars().collect::<String>())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| origin.to_owned());
        Ok(Self {
            data,
            index,
            num_glyphs,
            units_per_em,
            fs_type,
            origin: origin.to_owned(),
            postscript_name,
        })
    }

    /// 从磁盘读字体文件（`.ttf`/`.otf`/`.ttc`）。
    pub fn from_path(path: &std::path::Path, index: u32) -> Result<Self> {
        let data = std::fs::read(path).map_err(|source| EditError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::from_bytes(data, index, &name)
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// OS/2 fsType。`None` 表示字体没有 OS/2 表，此时允许嵌入但应在报告里标注。
    pub fn fs_type(&self) -> Option<u16> {
        self.fs_type
    }

    /// 是否允许嵌入（见 [`fs_type_allows_embedding`]）。没有 OS/2 表的按允许处理。
    pub fn embeddable(&self) -> bool {
        self.fs_type.map(fs_type_allows_embedding).unwrap_or(true)
    }

    /// 字体文件名（不含路径）。
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// 字体内部的 PostScript 名。
    pub fn postscript_name(&self) -> &str {
        &self.postscript_name
    }

    /// 字体是否为 CFF 轮廓（OpenType/CFF，PDF 里作 CIDFontType0）。
    pub fn is_cff(&self) -> bool {
        self.font_ref()
            .is_ok_and(|f| f.table_data(Tag::new(b"CFF ")).is_some())
    }

    fn font_ref(&self) -> std::result::Result<FontRef<'_>, skrifa::raw::ReadError> {
        FontRef::from_index(&self.data, self.index)
    }

    fn outline_glyphs(&self) -> Option<OutlineGlyphCollection<'_>> {
        self.font_ref().ok().map(|f| f.outline_glyphs())
    }

    /// 码点对应的字形 ID；字体里没有这个字时为 `None`。
    pub fn glyph_for_char(&self, ch: char) -> Option<u16> {
        let font = self.font_ref().ok()?;
        let id = font.charmap().map(ch)?.to_u32();
        (id != 0 && id < u32::from(self.num_glyphs)).then_some(id as u16)
    }

    /// 反向：字形 ID 对应哪个码点（同一字形可能对应多个码点，取最小的非控制码点）。
    pub fn char_for_glyph(&self, gid: u16) -> Option<char> {
        let font = self.font_ref().ok()?;
        let cmap = font.charmap();
        let mut best: Option<char> = None;
        for (code, g) in cmap.mappings() {
            if g.to_u32() != u32::from(gid) {
                continue;
            }
            let Some(ch) = char::from_u32(code) else {
                continue;
            };
            best = Some(match best {
                Some(prev) => preferred_codepoint(prev, ch),
                None => ch,
            });
        }
        best
    }

    /// 字形的横向进距，单位是 1000 分之一 em（PDF 的 W 数组单位）。
    pub fn advance_1000(&self, gid: u16) -> u16 {
        let upem = u32::from(self.units_per_em.max(1));
        let advance = self
            .font_ref()
            .ok()
            .and_then(|f| {
                f.glyph_metrics(Size::unscaled(), LocationRef::default())
                    .advance_width(GlyphId::new(u32::from(gid)))
            })
            .unwrap_or(0.0);
        let v = (advance * 1000.0 / upem as f32).round();
        u16::try_from(v.clamp(0.0, 65535.0) as u32).unwrap_or(0)
    }

    /// 字形是否有轮廓（空字形如空格返回 `false`）。
    pub fn has_outline(&self, gid: u16) -> bool {
        let Some(glyphs) = self.outline_glyphs() else {
            return false;
        };
        let Some(glyph) = glyphs.get(GlyphId::new(u32::from(gid))) else {
            return false;
        };
        matches!(
            glyph.format(),
            skrifa::outline::OutlineGlyphFormat::Glyf
                | skrifa::outline::OutlineGlyphFormat::Cff
                | skrifa::outline::OutlineGlyphFormat::Cff2
        )
    }

    /// 取字形路径（em 单位，y 向上，原点在基线起点）。
    pub fn outline(&self, gid: u16) -> Option<Vec<skrifa::raw::model::pen::PathElement>> {
        let glyphs = self.outline_glyphs()?;
        let glyph = glyphs.get(GlyphId::new(u32::from(gid)))?;
        let mut sink = Vec::new();
        // 不 hint：PDF 里保存的是轮廓，由阅读器自己光栅化。
        let settings =
            skrifa::outline::DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
        glyph
            .draw(settings, &mut sink)
            .ok()
            .map(|_| sink)
            .filter(|p| !p.is_empty())
    }

    /// 字体名（name 表 ID 1 与 ID 2），用于字体匹配的报告。
    pub fn family_name(&self) -> String {
        self.font_ref()
            .ok()
            .and_then(|f| {
                f.localized_strings(skrifa::string::StringId::FAMILY_NAME)
                    .english_or_first()
                    .map(|s| s.chars().collect::<String>())
            })
            .unwrap_or_default()
    }
}

/// 字形覆盖检查的结果。

/// 同一 GID 对应多个码点时，选提取 / ToUnicode 应使用的那个。
///
/// 优先 ASCII（空格保持 U+0020，避免被 NBSP 覆盖后 SetText 反查失败写成 NUL），
/// 其次 CJK 统一汉字（避免康熙部首 U+2F45 盖住「方」U+65B9），不选控制字符。
pub fn preferred_codepoint(a: char, b: char) -> char {
    if unicode_rank(b) < unicode_rank(a) {
        b
    } else {
        a
    }
}

fn unicode_rank(ch: char) -> (u8, u32) {
    let cp = ch as u32;
    if cp < 0x20 || (0x7F..0xA0).contains(&cp) {
        (5, cp)
    } else if (0x20..=0x7E).contains(&cp) {
        (0, cp)
    } else if (0x4E00..=0x9FFF).contains(&cp) {
        (1, cp)
    } else if (0x3400..=0x4DBF).contains(&cp) || (0x20000..=0x2A6DF).contains(&cp) {
        (2, cp)
    } else if (0x2F00..=0x2FDF).contains(&cp) || (0xF900..=0xFAFF).contains(&cp) {
        (4, cp)
    } else {
        (3, cp)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReport {
    /// 缺字形的字符（按出现顺序、去重）。
    pub missing: Vec<char>,
    /// `.notdef` 方框：字体有 cmap 条目但字形是 0 号。
    pub notdef: usize,
}

/// 检查一串字符在字体里的覆盖情况。
pub fn coverage(face: &FontFace, text: &str) -> CoverageReport {
    let mut missing = Vec::new();
    let mut notdef = 0;
    for ch in text.chars() {
        match face.glyph_for_char(ch) {
            Some(_) => {}
            None => {
                if ch == ' ' || ch == '\u{00A0}' {
                    // 空格没有字形也可以用：用 0 号字形会让复制结果多出 .notdef，交给上层决定。
                    missing.push(ch);
                } else {
                    notdef += 1;
                    missing.push(ch);
                }
            }
        }
    }
    CoverageReport { missing, notdef }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_type_policy() {
        assert!(fs_type_allows_embedding(0));
        assert!(!fs_type_allows_embedding(FS_TYPE_RESTRICTED));
        assert!(!fs_type_allows_embedding(
            FS_TYPE_RESTRICTED | FS_TYPE_EDITABLE
        ));
        assert!(!fs_type_allows_embedding(FS_TYPE_BITMAP_ONLY));
        assert!(fs_type_allows_embedding(FS_TYPE_EDITABLE));
        assert!(fs_type_allows_embedding(0x0100), "No subsetting 不禁止嵌入");
    }

    /// 本机系统字体的 fsType 只读检查；没有这些字体的机器上跳过。
    #[test]
    fn system_fonts_report_fs_type() {
        let root = std::path::Path::new(r"C:\Windows\Fonts");
        for name in [
            "simsun.ttc",
            "simhei.ttf",
            "simfang.ttf",
            "simkai.ttf",
            "msyh.ttc",
        ] {
            let path = root.join(name);
            if !path.is_file() {
                continue;
            }
            let face = FontFace::from_path(&path, 0).expect("系统字体应能打开");
            assert!(!face.family_name().is_empty());
            assert!(face.num_glyphs() > 100);
            assert!(face.glyph_for_char('永').is_some(), "{name} 应有汉字字形");
        }
    }
}
