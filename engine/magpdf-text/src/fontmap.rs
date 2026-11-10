//! 字体映射的反查（方案 5.2 第 1 级）：从一个已加载的 PDF 字体里查出
//!
//! - 字符 → 字符码（PDF 内容流里写的值，Type0/Identity-H 下是 2 字节的 CID）；
//! - 字符码 → 字形 ID（Identity-H 下 CID = GID，PDFium 取字形时不需要额外映射）；
//! - 字符码 → 是否真的有字形（不是 `.notdef`）。
//!
//! 用到的 PDFium 公共 API（`pdfium-sys` 的追加部分，见报告）：
//!
//! ```c
//! // 字符码 → Unicode（有 ToUnicode 时用它，没有时用字体的内置编码）
//! unsigned int FPDFFont_GetUnicodeFromCharcode(FPDF_FONT font, unsigned int charcode);
//! // Unicode → 字符码；0 表示没有（Unicode 0 本身没有意义）
//! unsigned int FPDFFont_GetCharcodeFromUnicode(FPDF_FONT font, unsigned int unicode);
//! // 字符码对应的字形 ID（Identity-H 下等于字符码）
//! unsigned int FPDFFont_GetGlyphFromCharcode(FPDF_FONT font, unsigned int charcode);
//! ```
//!
//! 本模块只定义数据与算法（可单元测试），FFI 调用在 `edit::tier1` 里。
//! 现有原版 PDFium 没有这三个接口，`FontMap::Unavailable` 表示拿不到映射，
//! 第 1 级回退在 P0 只能对"有子集字符表可用"的情况生效，报告里说明。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 一种字体在文档里的编码方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodingKind {
    /// Type0 + Identity-H（CID = 字符码；CIDToGIDMap 为 Identity 时 GID = CID）。
    IdentityH,
    /// Type0 + Identity-V。
    IdentityV,
    /// 带 ToUnicode 的嵌入式 CMap。
    EmbeddedCMap,
    /// 简单字体（单字节）。
    Simple,
    /// 其它（预定义 CJK CMap、通过 CMapEncoding 的多字节编码）。
    Other,
}

impl EncodingKind {
    /// 这个编码下字符码是否等于字形 ID（可以跳过 CIDToGIDMap）。
    pub fn cid_is_gid(self) -> bool {
        matches!(self, Self::IdentityH | Self::IdentityV)
    }
}

/// 字体映射反查的句柄。
#[derive(Debug, Clone)]
pub enum FontMap {
    /// 拿到了映射：Unicode ↔ 字符码 ↔ GID。
    Table {
        encoding: EncodingKind,
        /// Unicode → 字符码。
        to_charcode: BTreeMap<char, u32>,
        /// 字符码 → Unicode（ToUnicode 的内容）。
        to_unicode: BTreeMap<u32, char>,
        /// 字符码 → GID。
        to_gid: BTreeMap<u32, u32>,
    },
    /// PDFium 没有提供反查接口（原版 PDFium，缺少 `FPDFFont_GetCharcodeFromUnicode` 等）。
    Unavailable { reason: String },
}

impl FontMap {
    /// 查新字符在原字体里的字符码；`None` 表示原字体里没有这个字形的映射。
    pub fn charcode(&self, ch: char) -> Option<u32> {
        match self {
            Self::Table { to_charcode, .. } => to_charcode.get(&ch).copied(),
            Self::Unavailable { .. } => None,
        }
    }

    /// 字符码对应哪个字符（复制用）。
    pub fn unicode(&self, code: u32) -> Option<char> {
        match self {
            Self::Table { to_unicode, .. } => to_unicode.get(&code).copied(),
            Self::Unavailable { .. } => None,
        }
    }

    /// 字符码是否为 `.notdef`（0 号字形）。
    pub fn is_notdef(&self, code: u32) -> bool {
        match self {
            Self::Table { to_gid, .. } => to_gid.get(&code).is_none_or(|g| *g == 0),
            Self::Unavailable { .. } => true,
        }
    }

    /// 全部新字符都能反查到字符码时返回字符码序列，否则 `None`。
    pub fn charcodes_for(&self, text: &str) -> Option<Vec<u32>> {
        text.chars().map(|ch| self.charcode(ch)).collect()
    }
}

/// 由"字符码 → Unicode"与"字符码 → GID"两张表构造（PDFium 接口的返回值整理成这里）。
///
/// 同一字符码对应多个 Unicode 的情况（罕见，理论上不会）取第一个非控制码点。
pub fn build_table(
    encoding: EncodingKind,
    charcode_to_unicode: impl IntoIterator<Item = (u32, u32)>,
    charcode_to_gid: impl IntoIterator<Item = (u32, u32)>,
) -> FontMap {
    let mut to_unicode = BTreeMap::new();
    let mut to_charcode: BTreeMap<char, u32> = BTreeMap::new();
    for (code, uni) in charcode_to_unicode {
        let Some(ch) = char::from_u32(uni) else {
            continue;
        };
        if ch < ' ' && ch != '\t' {
            continue;
        }
        to_unicode.insert(code, ch);
        to_charcode
            .entry(ch)
            .and_modify(|c| {
                if code < *c {
                    *c = code;
                }
            })
            .or_insert(code);
    }
    let to_gid = charcode_to_gid.into_iter().collect();
    FontMap::Table {
        encoding,
        to_charcode,
        to_unicode,
        to_gid,
    }
}

/// 第 1 级的判定：`text` 里每个字符都能在原字体里反查到字符码，且字符码有真实字形。
///
/// 返回可以交给 `FPDFText_SetCharcodes` 的序列；不满足时返回缺哪些字符（给报告用）。
pub fn tier1_charcodes(map: &FontMap, text: &str) -> Result<Vec<u32>, Vec<char>> {
    let mut out = Vec::with_capacity(text.len());
    let mut missing = Vec::new();
    for ch in text.chars() {
        match map.charcode(ch) {
            Some(code) if !map.is_notdef(code) => out.push(code),
            _ => {
                if !missing.contains(&ch) {
                    missing.push(ch);
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_table() -> FontMap {
        // 模拟一个 Identity-H 子集：字 'A' 是 CID 3、'B' 是 CID 7，CID 1 是空格。
        build_table(
            EncodingKind::IdentityH,
            [(1, 0x20), (3, 0x41), (7, 0x42)],
            [(1, 1), (3, 3), (7, 7)],
        )
    }

    #[test]
    fn reverse_lookup_hits_and_misses() {
        let map = identity_table();
        assert_eq!(map.charcode('A'), Some(3));
        assert_eq!(map.charcode('B'), Some(7));
        assert_eq!(map.charcode('C'), None);
        assert_eq!(map.unicode(3), Some('A'));
        assert!(
            map.charcode('A')
                .is_some_and(|_| map.encoding().cid_is_gid())
        );
    }

    impl FontMap {
        fn encoding(&self) -> EncodingKind {
            match self {
                Self::Table { encoding, .. } => *encoding,
                Self::Unavailable { .. } => EncodingKind::Other,
            }
        }
    }

    #[test]
    fn tier1_accepts_known_text_and_reports_missing() {
        let map = identity_table();
        assert_eq!(tier1_charcodes(&map, "A B").unwrap(), vec![3, 1, 7]);
        assert_eq!(tier1_charcodes(&map, "AB中").unwrap_err(), vec!['中']);
        assert_eq!(tier1_charcodes(&map, "中中C").unwrap_err(), vec!['中', 'C']);
    }

    #[test]
    fn notdef_charcodes_are_rejected() {
        let map = build_table(EncodingKind::IdentityH, [(5, 0x41)], [(5, 0)]);
        assert_eq!(tier1_charcodes(&map, "A").unwrap_err(), vec!['A']);
        assert!(map.is_notdef(5));
    }

    #[test]
    fn unavailable_map_never_matches() {
        let map = FontMap::Unavailable {
            reason: "原版 PDFium".into(),
        };
        assert!(tier1_charcodes(&map, "A").is_err());
    }

    #[test]
    fn control_codepoints_are_skipped() {
        let map = build_table(
            EncodingKind::Simple,
            [(1, 0x0D), (2, 0x41), (3, 0x41)],
            [(1, 1), (2, 2), (3, 3)],
        );
        assert_eq!(map.charcode('A'), Some(2), "重复映射取最小的字符码");
        assert_eq!(map.unicode(1), None, "控制码点不进 ToUnicode");
    }
}
