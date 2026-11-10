//! 子集化与 PDF 用的编码（方案 5.3、5.4）。
//!
//! 流程：塑形得到原字体里的 GID → `subsetter` 按 GID 集合产出新字体（GID 从 1 开始连续重排）→
//! 每个字形在 PDF 里的**字符码 = 子集字体里的新 GID**（Identity-H：字符码 = CID，CIDToGIDMap 为 Identity
//! 时 CID = GID，三者一致）→ ToUnicode 把字符码映回原字符，W 数组按新 GID 写同一套进距。
//!
//! 这一模块不碰 PDFium，纯 CPU 工作，单元测试直接覆盖映射与 ToUnicode 文本。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use subsetter::GlyphRemapper;

use crate::error::{EditError, Result};
use crate::fontfile::FontFace;

/// 子集化的结果。
#[derive(Debug, Clone)]
pub struct Subset {
    /// 子集字体文件（TrueType 轮廓为 glyf 版本，CFF 轮廓为 CFF 版本）。
    pub data: Vec<u8>,
    /// 原 GID → 新 GID。0 号（`.notdef`）恒在新字体的 0 号。
    pub new_gid: BTreeMap<u16, u16>,
    /// 新 GID → 该字形在文本里对应的码点（同一字形只记最小的非控制码点）。
    pub codepoint: BTreeMap<u16, char>,
    /// 新 GID → 进距，单位 1000 分之一 em。
    pub advance: BTreeMap<u16, u16>,
    /// 子集使用的原字号（`unitsPerEm`）。
    pub units_per_em: u16,
    /// 子集字体是否为 CFF 轮廓（PDF 里作 CIDFontType0）。
    pub cff: bool,
}

impl Subset {
    pub fn bytes(&self) -> usize {
        self.data.len()
    }

    /// 每个字形在 PDF 内容流里的字符码（= CID = 新 GID）。
    pub fn charcode(&self, original_gid: u16) -> Option<u32> {
        self.new_gid.get(&original_gid).map(|g| u32::from(*g))
    }
}

/// 对 `face` 做子集化，只保留 `glyphs`（原 GID）里的字形以及它们对应的码点。
///
/// `codepoints` 是原 GID → 码点，来自塑形的簇；没有码点的字形（连字、组合标记）在 ToUnicode 里不出现，
/// 复制这类文字时 PDFium 会用字体的 cmap 兜底。可以多给：子集里没有的字形会被忽略。
pub fn subset_face(
    face: &FontFace,
    glyphs: &BTreeSet<u16>,
    codepoints: &BTreeMap<u16, char>,
) -> Result<Subset> {
    let mut remapper = GlyphRemapper::new();
    let mut kept: Vec<u16> = glyphs.iter().copied().collect();
    kept.sort_unstable();
    kept.dedup();
    for gid in &kept {
        if *gid < face.num_glyphs() {
            remapper.remap(*gid);
        }
    }
    let data = subsetter::subset(face.data(), face.index(), &remapper)
        .map_err(|e| EditError::Subset(e.to_string()))?;
    let mut new_gid = BTreeMap::new();
    let mut advance = BTreeMap::new();
    let mut codepoint = BTreeMap::new();
    for gid in &kept {
        let Some(new) = remapper.get(*gid) else {
            continue;
        };
        new_gid.insert(*gid, new);
        advance.insert(new, face.advance_1000(*gid));
        if let Some(ch) = codepoints.get(gid) {
            codepoint
                .entry(new)
                .and_modify(|c: &mut char| {
                    *c = crate::fontfile::preferred_codepoint(*c, *ch);
                })
                .or_insert(*ch);
        }
    }
    Ok(Subset {
        data,
        new_gid,
        codepoint,
        advance,
        units_per_em: face.units_per_em(),
        cff: face.is_cff(),
    })
}

/// 把一个码点写成 PDF 的 UTF-16BE 十六进制串（ToUnicode 的 `<...>`、补充平面用代理对）。
pub fn utf16_hex(ch: char) -> String {
    let mut buf = [0u16; 2];
    let units = ch.encode_utf16(&mut buf);
    units.iter().map(|u| format!("{u:04X}")).collect()
}

/// 生成 ToUnicode CMap（Identity-H 的字符码范围，2 字节码）。
///
/// `bpe`（bytes per encoding）为 2 时字符码按 2 字节写；简单字体可以用 1。这里只生成 2 字节版本，
/// 简单字体的 ToUnicode 在 `tier1` 里单独生成（字符码最高 255，统一按 1 字节写）。
pub fn to_unicode_cmap(entries: &BTreeMap<u32, char>) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n",
    );
    out.push_str("<0000> <FFFF>\n");
    out.push_str("endcodespacerange\n");
    // 规范建议每个 bfchar 块最多 100 条。
    let items: Vec<(u32, char)> = entries.iter().map(|(c, ch)| (*c, *ch)).collect();
    for chunk in items.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (code, ch) in chunk {
            out.push_str(&format!("<{code:04X}> <{}>\n", utf16_hex(*ch)));
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

/// 生成 1 字节字符码的 ToUnicode（简单字体补字形时的第 2 级）。
pub fn to_unicode_cmap_simple(entries: &BTreeMap<u8, char>) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n",
    );
    out.push_str("<00> <FF>\n");
    out.push_str("endcodespacerange\n");
    let items: Vec<(u8, char)> = entries.iter().map(|(c, ch)| (*c, *ch)).collect();
    for chunk in items.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (code, ch) in chunk {
            out.push_str(&format!("<{code:02X}> <{}>\n", utf16_hex(*ch)));
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

/// 生成 CIDToGIDMap（每 CID 2 字节大端，长度覆盖到最大新 GID）。
pub fn cid_to_gid_map(new_gids: impl IntoIterator<Item = u16>) -> Vec<u8> {
    let max = new_gids.into_iter().max().unwrap_or(0);
    let mut map = vec![0u8; (usize::from(max) + 1) * 2];
    for gid in 0..=max {
        let at = usize::from(gid) * 2;
        map[at] = (gid >> 8) as u8;
        map[at + 1] = (gid & 0xFF) as u8;
    }
    map
}

/// 判断 PDF 里的 BaseFont 名是否像"整字体"（没有子集前缀）。
///
/// 子集前缀是 6 个大写字母加 `+`（PDF 32000-1 9.6.4）。注：Office 导出的一些"完整嵌入"字体
/// 也带假前缀，第 2 级会再用字体数据核对，见 `edit::tier2`。
pub fn is_subset_name(base_font: &str) -> bool {
    match base_font.split_once('+') {
        Some((prefix, rest)) => {
            prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) && !rest.is_empty()
        }
        None => false,
    }
}

/// 去掉子集前缀。
pub fn strip_subset_prefix(base_font: &str) -> &str {
    if is_subset_name(base_font) {
        base_font
            .split_once('+')
            .map(|(_, r)| r)
            .unwrap_or(base_font)
    } else {
        base_font
    }
}

/// 子集化后的诊断信息，写进报告的表格。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubsetStats {
    /// 原字体里用到的字形数。
    pub glyphs: usize,
    /// 子集字体字节数。
    pub bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(name: &str) -> Option<FontFace> {
        let path = std::path::Path::new(r"C:\Windows\Fonts").join(name);
        path.is_file()
            .then(|| FontFace::from_path(&path, 0).ok())
            .flatten()
    }

    #[test]
    fn subset_remaps_glyphs_and_keeps_advances() {
        let Some(face) = font("simhei.ttf") else {
            return;
        };
        let text = "合同甲乙丙";
        let mut glyphs = BTreeSet::new();
        let mut cps = BTreeMap::new();
        for ch in text.chars() {
            let gid = face.glyph_for_char(ch).expect("应有字形");
            glyphs.insert(gid);
            cps.insert(gid, ch);
        }
        let sub = subset_face(&face, &glyphs, &cps).expect("子集化应成功");
        assert!(!sub.cff);
        assert_eq!(sub.new_gid.len(), glyphs.len());
        // 新 GID 从 1 开始连续，字符码 = 新 GID
        let mut news: Vec<u16> = sub.new_gid.values().copied().collect();
        news.sort_unstable();
        assert_eq!(news, (1..=glyphs.len() as u16).collect::<Vec<_>>());
        for gid in &glyphs {
            let code = sub.charcode(*gid).unwrap();
            assert_eq!(
                sub.codepoint.get(&(code as u16)).copied(),
                cps.get(gid).copied()
            );
            assert_eq!(
                sub.advance.get(&(code as u16)),
                Some(&face.advance_1000(*gid))
            );
        }
        // 子集字体应远小于原字体，并能按新 GID 读出轮廓
        assert!(sub.bytes() < face.data().len() / 10, "{} 字节", sub.bytes());
        let subset_face = FontFace::from_bytes(sub.data.clone(), 0, "subset").unwrap();
        for code in news {
            assert!(subset_face.has_outline(code));
        }
    }

    #[test]
    fn cff_font_subset_is_cff() {
        let path = std::path::Path::new(r"C:\Windows\Fonts\ROGFonts-Regular_0.otf");
        if !path.is_file() {
            return;
        }
        let face = FontFace::from_path(path, 0).expect("系统里的 CFF 字体应能打开");
        let mut glyphs = BTreeSet::new();
        let mut cps = BTreeMap::new();
        for ch in "MagPDF".chars() {
            if let Some(gid) = face.glyph_for_char(ch) {
                glyphs.insert(gid);
                cps.insert(gid, ch);
            }
        }
        let sub = subset_face(&face, &glyphs, &cps).expect("CFF 子集化应成功");
        assert!(sub.cff, "CFF 字体子集后仍是 CFF");
        assert!(sub.data.len() < 200_000);
    }

    #[test]
    fn to_unicode_cmap_is_wellformed() {
        let mut map = BTreeMap::new();
        map.insert(1u32, 'A');
        map.insert(0x4E2D, '中');
        map.insert(3, '\u{20000}');
        let cmap = String::from_utf8(to_unicode_cmap(&map)).unwrap();
        assert!(cmap.contains("begincodespacerange"));
        assert!(cmap.contains("<0001> <0041>"));
        assert!(cmap.contains("<4E2D> <4E2D>"));
        assert!(
            cmap.contains("<0003> <D840DC00>"),
            "补充平面用代理对：{cmap}"
        );
        assert!(cmap.ends_with("end\nend\n"));
        let simple =
            String::from_utf8(to_unicode_cmap_simple(&BTreeMap::from([(65u8, 'A')]))).unwrap();
        assert!(simple.contains("<41> <0041>"));
    }

    #[test]
    fn cid_to_gid_identity() {
        let map = cid_to_gid_map([1u16, 3, 7]);
        assert_eq!(map.len(), 16);
        assert_eq!(&map[0..2], &[0, 0]);
        assert_eq!(&map[2..4], &[0, 1]);
        assert_eq!(&map[14..16], &[0, 7]);
    }

    #[test]
    fn subset_prefix_detection() {
        assert!(is_subset_name("ABCDEF+SimSun"));
        assert!(!is_subset_name("SimSun"));
        assert!(!is_subset_name("Arial+Bold"));
        assert!(!is_subset_name("abcdef+SimSun"), "小写不是子集前缀");
        assert_eq!(strip_subset_prefix("ABCDEF+SimSun"), "SimSun");
        assert_eq!(strip_subset_prefix("SimSun"), "SimSun");
    }
}
