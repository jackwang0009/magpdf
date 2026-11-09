//! 文字块识别的输入：一页的字形列表（[`PageText`]）。
//!
//! 字形数据与 `pdfium-sys` 解耦，核心算法只依赖这里的类型，单元测试用手工构造的布局；
//! 从 [`pdfium_sys::CharInfo`] 的转换在 [`PageText::push_char`]，从整页取数据在
//! [`PageText::from_page`]。

use serde::{Deserialize, Serialize};

use crate::geom::{Point, Rect};

/// PDF 字体描述符 Flags 的 ForceBold 位（PDF 32000-1 表 123）。
const FONT_FLAG_FORCE_BOLD: i32 = 1 << 18;

/// 一页里用到的一种字体，按名字去重（见 [`PageText::intern_font`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontInfo {
    /// PDFium 给出的 BaseFont 名，可能带子集前缀（`ABCDEF+`）。
    pub name: String,
    /// 字体描述符 Flags，没有时为 0。
    pub flags: i32,
    /// 字重（100–900），没有时为 `None`。
    pub weight: Option<i32>,
}

impl FontInfo {
    /// 去掉子集前缀后的名字：同一款字体的不同子集视为同一字体。
    pub fn family_key(&self) -> &str {
        match self.name.split_once('+') {
            Some((prefix, rest))
                if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) =>
            {
                rest
            }
            _ => &self.name,
        }
    }

    pub fn is_bold(&self) -> bool {
        self.flags & FONT_FLAG_FORCE_BOLD != 0
            || self.weight.is_some_and(|w| w >= 600)
            || self.name.to_ascii_lowercase().contains("bold")
    }
}

/// 一个字符的字形。索引与 PDFium 文字页的字符索引一致，标注文件也用这个索引。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Glyph {
    /// Unicode 码点；无映射时是 PDFium 填入的字符码（见 `unicode_map_error`）。
    pub unicode: u32,
    /// 字形紧包围盒（页面坐标）。无轮廓的字形（空格）在 PDFium 里是退化矩形。
    pub bbox: Rect,
    /// 按字体 ascent/descent 估计的宽松包围盒，没有时为 `None`。
    pub loose_bbox: Option<Rect>,
    /// 基线起点。
    pub origin: Point,
    /// 页面坐标系里的实际字号：`Tf` 字号 × 文字矩阵的纵向缩放。
    pub font_size: f32,
    /// 指向 [`PageText::fonts`] 的下标。
    pub font_id: u32,
    /// 基线方向（度，逆时针，`[0, 360)`）；0 是从左到右的水平文字。
    pub angle: f32,
    /// PDFium 为排版插入的字符（空格、换行），文件里没有对应字形，聚类时跳过。
    pub generated: bool,
    /// 没有 Unicode 映射（方案 5.1 第 2 条"不可编辑文字"）。
    pub unicode_map_error: bool,
    /// PDFium 判定的行尾连字符。
    pub hyphen: bool,
    /// 渲染模式为不可见（`Tr 3`/`Tr 7`），常见于扫描件的 OCR 文字层。
    pub invisible: bool,
    /// 所属文字对象的标识（同一页内唯一），生成字符没有。
    pub text_object: Option<usize>,
}

impl Glyph {
    /// 聚类用的纵向范围：优先用宽松包围盒，但它偶尔会因字体度量异常而极高，
    /// 超过 1.6 倍字号时退回按字号估计（下探 0.25em，上探 0.75em），再并上紧包围盒。
    pub fn vertical_extent(&self) -> (f32, f32) {
        let est_bottom = self.origin.y - 0.25 * self.font_size;
        let est_top = self.origin.y + 0.75 * self.font_size;
        let (b, t) = match self.loose_bbox {
            Some(l) if l.height() <= 1.6 * self.font_size && l.height() > 0.0 => (l.bottom, l.top),
            _ => (est_bottom, est_top),
        };
        if self.bbox.is_degenerate(1e-3) {
            (b, t)
        } else {
            (b.min(self.bbox.bottom), t.max(self.bbox.top))
        }
    }

    /// 是否是"无 Unicode 映射且确实有可见字形"的字符：块里有这种字符就不可编辑（方案 5.1 第 2 条）。
    ///
    /// 没有字形的字符（空格、`\r\n` 之类的控制码）即使映射失败也不影响编辑：重排时按进距当空白处理。
    /// PDFium 给无轮廓的字符一个零高度（或零宽度）的包围盒；旋转后的小方块按 0.3em 以内认定。
    pub fn blocks_editing(&self) -> bool {
        if !self.unicode_map_error {
            return false;
        }
        let (w, h) = (self.bbox.width(), self.bbox.height());
        let blank =
            h <= 1e-3 || w <= 1e-3 || (self.unicode < 0x20 && w.max(h) <= 0.3 * self.font_size);
        !blank
    }
}

/// 一页的文字输入。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PageText {
    /// 页面宽高（点，内容流坐标系，即未应用 `/Rotate` 的尺寸），未知时为 0。
    pub width: f32,
    pub height: f32,
    /// 页面 `/Rotate`，以顺时针四分之一圈计（0..=3）。字形角度都是内容流坐标系里的角度；
    /// 显示时端正的文字，其角度等于 `rotation × 90°`。
    #[serde(default)]
    pub rotation: u8,
    pub glyphs: Vec<Glyph>,
    pub fonts: Vec<FontInfo>,
    /// 细线路径（表格框线、分隔线）的包围盒，作为行与段的分隔依据；可以为空。
    pub rulings: Vec<Rect>,
}

impl PageText {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            ..Self::default()
        }
    }

    /// 登记字体，返回下标；同名字体只登记一次。
    pub fn intern_font(&mut self, name: &str, flags: i32, weight: Option<i32>) -> u32 {
        if let Some(i) = self.fonts.iter().position(|f| f.name == name) {
            return i as u32;
        }
        self.fonts.push(FontInfo {
            name: name.to_owned(),
            flags,
            weight,
        });
        (self.fonts.len() - 1) as u32
    }

    pub fn font(&self, id: u32) -> Option<&FontInfo> {
        self.fonts.get(id as usize)
    }

    /// 把 PDFium 的一个字符追加进来，返回它的索引（等于 PDFium 字符索引，前提是按顺序追加全部字符）。
    pub fn push_char(&mut self, info: &pdfium_sys::CharInfo) -> usize {
        let font_id = self.intern_font(&info.font_name, info.font_flags, info.font_weight);
        let (scale_v, angle) = match info.matrix {
            Some(m) => {
                // 文字矩阵 [a b c d]：x 轴映射到 (a, b)，y 轴映射到 (c, d)。
                let sv = m.c.hypot(m.d);
                let sh = m.a.hypot(m.b);
                let scale = if sv > 1e-6 {
                    sv
                } else if sh > 1e-6 {
                    sh
                } else {
                    1.0
                };
                let angle = if sh > 1e-6 {
                    m.b.atan2(m.a).to_degrees().rem_euclid(360.0)
                } else {
                    0.0
                };
                (scale, angle)
            }
            None => (1.0, 0.0),
        };
        let bbox: Rect = info.bbox.into();
        let mut font_size = info.font_size.abs() as f32 * scale_v;
        if font_size <= 1e-3 {
            font_size = if bbox.height() > 1e-3 {
                bbox.height()
            } else {
                1.0
            };
        }
        self.glyphs.push(Glyph {
            unicode: info.unicode,
            bbox,
            loose_bbox: info.loose_bbox.map(Into::into),
            origin: Point::new(info.origin.0 as f32, info.origin.1 as f32),
            font_size,
            font_id,
            angle,
            generated: info.generated,
            unicode_map_error: info.unicode_map_error,
            hyphen: info.hyphen,
            invisible: info.render_mode.is_some_and(|m| m.is_invisible()),
            text_object: info.text_object_id,
        });
        self.glyphs.len() - 1
    }

    /// 从已加载的文字页取全部字符。
    pub fn from_text_page(width: f32, height: f32, text: &pdfium_sys::TextPage<'_>) -> Self {
        let mut page = Self::new(width, height);
        page.glyphs.reserve(text.char_count());
        for info in text.chars() {
            page.push_char(&info);
        }
        page
    }

    /// 从页面取字符和细线路径（[`collect_rulings`]）。
    ///
    /// PDFium 报告的页面宽高已经应用了 `/Rotate`，而字符坐标没有；这里换回内容流坐标系的尺寸。
    pub fn from_page(
        page: &pdfium_sys::Page<'_>,
        ruling_max_thickness: f32,
    ) -> pdfium_sys::Result<Self> {
        let text = page.text()?;
        let rotation = u8::try_from(page.rotation().rem_euclid(4)).unwrap_or(0);
        let (width, height) = if rotation % 2 == 1 {
            (page.height(), page.width())
        } else {
            (page.width(), page.height())
        };
        let mut out = Self::from_text_page(width, height, &text);
        out.rotation = rotation;
        out.rulings = collect_rulings(page, ruling_max_thickness);
        Ok(out)
    }

    /// 把若干字形按给定顺序拼成字符串（调试与标注用；不要写进日志，《工程规范》4）。
    pub fn text_of(&self, indices: &[usize]) -> String {
        indices
            .iter()
            .filter_map(|&i| self.glyphs.get(i))
            .map(|g| {
                if g.hyphen {
                    '-'
                } else {
                    char::from_u32(g.unicode).unwrap_or('\u{FFFD}')
                }
            })
            .collect()
    }
}

/// 收集页面上的细线路径：一边不超过 `max_thickness` 点、另一边至少 4 点的路径对象包围盒，
/// 递归进入 Form XObject。表格框线和分隔线都是这种路径；下划线也是，聚类时按位置排除。
pub fn collect_rulings(page: &pdfium_sys::Page<'_>, max_thickness: f32) -> Vec<Rect> {
    fn visit(obj: &pdfium_sys::PageObject<'_>, max_thickness: f32, out: &mut Vec<Rect>, depth: u8) {
        match obj.kind() {
            pdfium_sys::ObjectKind::Path => {
                if let Some(b) = obj.bounds() {
                    let r: Rect = b.into();
                    let (w, h) = (r.width(), r.height());
                    let thin_h = h <= max_thickness && w >= 4.0;
                    let thin_v = w <= max_thickness && h >= 4.0;
                    if thin_h || thin_v {
                        out.push(r);
                    }
                }
            }
            pdfium_sys::ObjectKind::Form if depth < 8 => {
                for inner in obj.form_objects() {
                    visit(&inner, max_thickness, out, depth + 1);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for obj in page.objects() {
        visit(&obj, max_thickness, &mut out, 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_family_key_strips_subset_prefix() {
        let f = FontInfo {
            name: "ABCDEF+SimSun".into(),
            flags: 0,
            weight: None,
        };
        assert_eq!(f.family_key(), "SimSun");
        let g = FontInfo {
            name: "Arial+Bold".into(),
            flags: 0,
            weight: None,
        };
        assert_eq!(
            g.family_key(),
            "Arial+Bold",
            "不是 6 位大写前缀就不算子集标记"
        );
        assert!(g.is_bold());
        let h = FontInfo {
            name: "Calibri".into(),
            flags: FONT_FLAG_FORCE_BOLD,
            weight: None,
        };
        assert!(h.is_bold());
        let i = FontInfo {
            name: "Calibri".into(),
            flags: 0,
            weight: Some(700),
        };
        assert!(i.is_bold());
    }

    #[test]
    fn intern_font_dedups_by_name() {
        let mut p = PageText::new(100.0, 100.0);
        assert_eq!(p.intern_font("A", 0, None), 0);
        assert_eq!(p.intern_font("B", 0, None), 1);
        assert_eq!(p.intern_font("A", 4, Some(400)), 0);
        assert_eq!(p.fonts.len(), 2);
    }

    #[test]
    fn push_char_scales_font_size_by_matrix_and_derives_angle() {
        let mut p = PageText::new(100.0, 100.0);
        let base = pdfium_sys::CharInfo {
            unicode: u32::from('A'),
            bbox: pdfium_sys::Rect {
                left: 10.0,
                bottom: 20.0,
                right: 16.0,
                top: 28.0,
            },
            loose_bbox: None,
            origin: (10.0, 20.0),
            font_size: 1.0,
            font_name: "Helvetica".into(),
            font_flags: 32,
            font_weight: Some(400),
            matrix: Some(pdfium_sys::Matrix {
                a: 12.0,
                b: 0.0,
                c: 0.0,
                d: 12.0,
                e: 10.0,
                f: 20.0,
            }),
            fill_color: None,
            generated: false,
            unicode_map_error: false,
            hyphen: false,
            text_object_id: Some(7),
            render_mode: Some(pdfium_sys::TextRenderMode::Fill),
        };
        let i = p.push_char(&base);
        assert_eq!(i, 0);
        let g = &p.glyphs[0];
        assert!(
            (g.font_size - 12.0).abs() < 1e-4,
            "Tf 1 配 12 倍矩阵 → 12pt"
        );
        assert_eq!(g.angle, 0.0);
        assert!(!g.invisible);
        assert_eq!(g.text_object, Some(7));

        // 逆时针 90°：x 轴映射到 (0, 1)。
        let rotated = pdfium_sys::CharInfo {
            matrix: Some(pdfium_sys::Matrix {
                a: 0.0,
                b: 10.0,
                c: -10.0,
                d: 0.0,
                e: 0.0,
                f: 0.0,
            }),
            font_size: -1.5,
            render_mode: Some(pdfium_sys::TextRenderMode::Invisible),
            ..base
        };
        p.push_char(&rotated);
        let g = &p.glyphs[1];
        assert!((g.angle - 90.0).abs() < 1e-3, "{}", g.angle);
        assert!((g.font_size - 15.0).abs() < 1e-3, "负字号取绝对值再乘缩放");
        assert!(g.invisible);
        assert_eq!(p.fonts.len(), 1);
        assert_eq!(p.text_of(&[0, 1]), "AA");
    }

    #[test]
    fn vertical_extent_rejects_absurd_loose_box() {
        let g = Glyph {
            unicode: 65,
            bbox: Rect::new(0.0, 0.0, 5.0, 7.0),
            loose_bbox: Some(Rect::new(0.0, -40.0, 5.0, 60.0)),
            origin: Point::new(0.0, 0.0),
            font_size: 10.0,
            font_id: 0,
            angle: 0.0,
            generated: false,
            unicode_map_error: false,
            hyphen: false,
            invisible: false,
            text_object: None,
        };
        let (b, t) = g.vertical_extent();
        assert!((b + 2.5).abs() < 1e-5 && (t - 7.5).abs() < 1e-5, "{b} {t}");
        let sane = Glyph {
            loose_bbox: Some(Rect::new(0.0, -2.0, 5.0, 9.0)),
            ..g
        };
        assert_eq!(sane.vertical_extent(), (-2.0, 9.0));
    }

    #[test]
    fn unmapped_blank_glyphs_do_not_block_editing() {
        let base = Glyph {
            unicode: 0x66,
            bbox: Rect::new(0.0, 0.0, 5.0, 7.0),
            loose_bbox: None,
            origin: Point::new(0.0, 0.0),
            font_size: 10.0,
            font_id: 0,
            angle: 0.0,
            generated: false,
            unicode_map_error: true,
            hyphen: false,
            invisible: false,
            text_object: None,
        };
        assert!(base.blocks_editing(), "有轮廓、无映射：不可编辑");
        let mapped = Glyph {
            unicode_map_error: false,
            ..base.clone()
        };
        assert!(!mapped.blocks_editing());
        // 零高度包围盒：空格一类，无轮廓。
        let space = Glyph {
            unicode: 0x2,
            bbox: Rect::new(10.0, 0.0, 12.2, 0.0),
            ..base.clone()
        };
        assert!(!space.blocks_editing());
        // 旋转后的 \r：小方块，控制码。
        let cr = Glyph {
            unicode: 0x0D,
            bbox: Rect::new(73.1, 73.1, 75.3, 75.3),
            angle: 225.0,
            font_size: 12.0,
            ..base.clone()
        };
        assert!(!cr.blocks_editing());
        // TeX OT1 里 code 12 是 "fi" 连字：有轮廓，不能放过。
        let fi = Glyph {
            unicode: 12,
            bbox: Rect::new(0.0, 0.0, 5.5, 7.0),
            ..base
        };
        assert!(fi.blocks_editing());
    }
}
