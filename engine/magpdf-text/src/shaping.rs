//! 塑形（方案 5.3）：HarfBuzz 的 Rust 移植 `harfrust`。
//!
//! 输入是完整的字体文件（可能是 TTC 的一个面），输出每个字形的 GID、进距、偏移和簇（对应哪些输入字符），
//! 从排版、子集化到写 PDF 的字符码都用这里的 GID，保证三处一致。
//!
//! 本机实测的 API 要点：harfrust 0.15 没有导出 `FontRef`，字体对象是 `harfrust::Font::new(source, index)`
//! （`source` 可直接传 `&[u8]`）；它的 `read-fonts`（0.46）与 `skrifa` 依赖的 `read-fonts`（0.39）
//! 是两个版本，所以 `maxp`、`head` 之类的表查询用 `skrifa` 的 `FontRef`，塑形走 `harfrust`。

use harfrust::{Buffer, Direction, ShapeOptions, ShaperFont};
use serde::{Deserialize, Serialize};
use skrifa::MetadataProvider;
use skrifa::raw::TableProvider;

use crate::error::{EditError, Result};

/// 一个塑形后的字形。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapedGlyph {
    /// 字体里的原字形 ID。
    pub gid: u16,
    /// 在 `em` 单位（`unitsPerEm`）下的横向进距。
    pub x_advance: i32,
    /// 在 `em` 单位下的纵向进距，横排恒为 0。
    pub y_advance: i32,
    /// 在 `em` 单位下的横向偏移（组合标记等）。
    pub x_offset: i32,
    /// 在 `em` 单位下的纵向偏移。
    pub y_offset: i32,
    /// 这个字形覆盖的输入文本的起点（字节下标），用于把字形分给不同的字体。
    pub cluster: usize,
}

/// 一次塑形的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    /// 字体里没有对应字形的字符（码点）。
    pub missing: Vec<char>,
    /// `unitsPerEm`。
    pub units_per_em: u16,
}

impl ShapedRun {
    /// 全部字形在 `em` 单位下的总宽度。
    pub fn total_advance(&self) -> i32 {
        self.glyphs.iter().map(|g| g.x_advance).sum()
    }

    /// 总宽度换算成指定字号的点数。
    pub fn width_pt(&self, font_size: f32) -> f32 {
        self.total_advance() as f32 * font_size / f32::from(self.units_per_em.max(1))
    }
}

/// 字体面里的基本信息（塑形前取，避免每个字形都查）。
struct FaceInfo {
    num_glyphs: u16,
    units_per_em: u16,
}

fn face_info(font_data: &[u8], index: u32) -> Result<FaceInfo> {
    let font = skrifa::FontRef::from_index(font_data, index)
        .map_err(|e| EditError::Shape(format!("字体面 {index} 无效：{e}")))?;
    let num_glyphs = font
        .maxp()
        .map_err(|e| EditError::Shape(format!("没有 maxp：{e}")))?
        .num_glyphs();
    let units_per_em = font.head().map(|h| h.units_per_em()).unwrap_or(1000);
    Ok(FaceInfo {
        num_glyphs,
        units_per_em,
    })
}

/// 对 `text` 塑形。`index` 是 TTC 中的面索引。方向由 `harfrust` 从文本推断。
pub fn shape(font_data: &[u8], index: u32, text: &str) -> Result<ShapedRun> {
    let info = face_info(font_data, index)?;
    let font = harfrust::Font::new(font_data.to_vec(), index)
        .ok_or_else(|| EditError::Shape("harfrust 打不开字体".into()))?;
    let shaper_font = ShaperFont::new(&font);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    harfrust::shape(&shaper_font, &mut buffer, ShapeOptions::new())
        .map_err(|e| EditError::Shape(e.to_string()))?;
    Ok(collect(&buffer, text, &info))
}

/// 指定方向的塑形，测试与将来竖排（P4）用。
pub fn shape_with_direction(
    font_data: &[u8],
    index: u32,
    text: &str,
    direction: Direction,
) -> Result<ShapedRun> {
    let info = face_info(font_data, index)?;
    let font = harfrust::Font::new(font_data.to_vec(), index)
        .ok_or_else(|| EditError::Shape("harfrust 打不开字体".into()))?;
    let shaper_font = ShaperFont::new(&font);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    harfrust::shape(&shaper_font, &mut buffer, ShapeOptions::new())
        .map_err(|e| EditError::Shape(e.to_string()))?;
    Ok(collect(&buffer, text, &info))
}

fn collect(buffer: &Buffer, text: &str, info: &FaceInfo) -> ShapedRun {
    let mut glyphs = Vec::with_capacity(buffer.glyph_infos().len());
    for (g, pos) in buffer.glyph_infos().iter().zip(buffer.glyph_positions()) {
        glyphs.push(ShapedGlyph {
            gid: u16::try_from(g.glyph_id).unwrap_or(0),
            x_advance: pos.x_advance,
            y_advance: pos.y_advance,
            x_offset: pos.x_offset,
            y_offset: pos.y_offset,
            cluster: usize::try_from(g.cluster).unwrap_or(0),
        });
    }
    let missing = missing_chars_owned(buffer, text, info);
    ShapedRun {
        glyphs,
        missing,
        units_per_em: info.units_per_em,
    }
}

/// 缺字：塑形结果落在 0 号字形（`.notdef`）上的簇对应的字符。
///
/// 只看 cmap 不够：`.notdef` 也可能是 cmap 显式映射到 0 号的。
fn missing_chars_owned(buffer: &Buffer, text: &str, info: &FaceInfo) -> Vec<char> {
    let mut clusters: Vec<u32> = buffer
        .glyph_infos()
        .iter()
        .filter(|g| g.glyph_id == 0 || g.glyph_id >= u32::from(info.num_glyphs))
        .map(|g| g.cluster)
        .collect();
    clusters.sort_unstable();
    clusters.dedup();
    clusters
        .into_iter()
        .filter_map(|c| {
            let c = usize::try_from(c).ok()?;
            text.get(c..).and_then(|s| s.chars().next())
        })
        .fold(Vec::new(), |mut acc, ch| {
            if !acc.contains(&ch) {
                acc.push(ch);
            }
            acc
        })
}

/// 字符在字体里是否有字形（用 `skrifa` 的 cmap 查，与子集化的判定一致）。
pub fn char_supported(font_data: &[u8], index: u32, ch: char) -> bool {
    let Ok(font) = skrifa::FontRef::from_index(font_data, index) else {
        return false;
    };
    let n = font.maxp().map(|m| m.num_glyphs()).unwrap_or(0);
    font.charmap()
        .map(ch)
        .is_some_and(|g| g.to_u32() != 0 && g.to_u32() < u32::from(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(name: &str) -> Option<Vec<u8>> {
        std::fs::read(std::path::Path::new(r"C:\Windows\Fonts").join(name)).ok()
    }

    #[test]
    fn shapes_chinese_with_expected_metrics() {
        let Some(data) = font("simfang.ttf") else {
            return;
        };
        let run = shape(&data, 0, "合同号：ABC-001").expect("塑形应成功");
        assert!(run.missing.is_empty(), "缺字 {:?}", run.missing);
        assert!(!run.glyphs.is_empty());
        for g in &run.glyphs {
            assert!(g.cluster <= "合同号：ABC-001".len());
            assert_ne!(g.gid, 0);
        }
        let w10 = run.width_pt(10.0);
        assert!(w10 > 20.0 && w10 < 200.0, "{w10}");
    }

    #[test]
    fn missing_characters_are_reported() {
        let Some(data) = font("arial.ttf") else {
            return;
        };
        let run = shape(&data, 0, "A永B").expect("塑形应成功");
        assert_eq!(run.missing, vec!['永']);
        assert!(run.glyphs.iter().any(|g| g.gid == 0), "缺字用 .notdef");
    }

    #[test]
    fn latin_kerning_shrinks_advance() {
        let Some(data) = font("arial.ttf") else {
            return;
        };
        let av = shape(&data, 0, "AV").unwrap();
        let ax = shape(&data, 0, "AX").unwrap();
        assert!(
            av.total_advance() < ax.total_advance(),
            "AV 应因 kerning 变窄"
        );
        assert_eq!(av.glyphs.len(), 2, "GPOS 调整进距，不增加字形");
    }

    #[test]
    fn char_supported_matches_charmap() {
        let Some(data) = font("simsun.ttc") else {
            return;
        };
        assert!(char_supported(&data, 0, '宋'));
        assert!(!char_supported(&data, 0, '\u{10FFFF}'));
    }
}
