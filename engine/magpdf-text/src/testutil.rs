//! 单元测试用的合成排版器：按简单的字宽规则把字符串铺成字形，构造 [`PageText`]。
//! 字宽：拉丁字母 0.5em、数字 0.55em、空格 0.28em、CJK 1em；每次调用 `text*` 是一个文字对象。

use crate::geom::{Point, Rect};
use crate::glyph::{Glyph, PageText};

pub(crate) struct Typesetter {
    page: PageText,
    next_object: usize,
    /// 是否像 PDFium 那样在词间插入生成的空格字符（文件里并没有这个字形）。
    pub generated_spaces: bool,
}

pub(crate) fn advance(c: char, size: f32) -> f32 {
    if c == ' ' {
        0.28 * size
    } else if c.is_ascii_digit() {
        0.55 * size
    } else if c.is_ascii() {
        0.5 * size
    } else if c == '\u{3000}' {
        size
    } else {
        // CJK 及其他全角字符。
        size
    }
}

pub(crate) fn text_width(text: &str, size: f32) -> f32 {
    text.chars().map(|c| advance(c, size)).sum()
}

impl Typesetter {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            page: PageText::new(width, height),
            next_object: 1,
            generated_spaces: false,
        }
    }

    pub fn font(&mut self, name: &str) -> u32 {
        let flags = if name.to_ascii_lowercase().contains("bold") {
            1 << 18
        } else {
            0
        };
        self.page.intern_font(name, flags, None)
    }

    pub fn ruling(&mut self, rect: Rect) {
        self.page.rulings.push(rect);
    }

    /// 在 `(x, y)`（基线起点）铺一行文字，返回字形索引（不含生成的空格）。
    pub fn text(&mut self, x: f32, y: f32, size: f32, font: u32, text: &str) -> Vec<usize> {
        self.text_opts(x, y, size, font, text, TextOpts::default())
    }

    /// 居中：中心在 `cx`。
    pub fn text_centered(
        &mut self,
        cx: f32,
        y: f32,
        size: f32,
        font: u32,
        text: &str,
    ) -> Vec<usize> {
        let w = text_width(text, size);
        self.text(cx - w / 2.0, y, size, font, text)
    }

    /// 右对齐：右缘在 `x1`。
    pub fn text_right(&mut self, x1: f32, y: f32, size: f32, font: u32, text: &str) -> Vec<usize> {
        let w = text_width(text, size);
        self.text(x1 - w, y, size, font, text)
    }

    /// 两端对齐：把多出来的宽度平均分给空格，使右缘落在 `x1`。
    pub fn text_justified(
        &mut self,
        x0: f32,
        x1: f32,
        y: f32,
        size: f32,
        font: u32,
        text: &str,
    ) -> Vec<usize> {
        let natural = text_width(text, size);
        let spaces = text.chars().filter(|&c| c == ' ').count();
        let extra = if spaces > 0 {
            (x1 - x0 - natural) / spaces as f32
        } else {
            0.0
        };
        self.text_opts(
            x0,
            y,
            size,
            font,
            text,
            TextOpts {
                extra_space: extra,
                ..TextOpts::default()
            },
        )
    }

    pub fn text_opts(
        &mut self,
        x: f32,
        y: f32,
        size: f32,
        font: u32,
        text: &str,
        opts: TextOpts,
    ) -> Vec<usize> {
        let object = self.next_object;
        self.next_object += 1;
        let (sin, cos) = opts.angle_deg.to_radians().sin_cos();
        let place = |u: f32, v: f32| Point::new(x + u * cos - v * sin, y + u * sin + v * cos);
        let mut out = Vec::new();
        let mut cursor = 0.0f32;
        for c in text.chars() {
            let adv = advance(c, size) + if c == ' ' { opts.extra_space } else { 0.0 };
            if c == ' ' && self.generated_spaces {
                // PDFium 生成的空格：退化矩形、无文字对象。
                let o = place(cursor, 0.0);
                self.page.glyphs.push(Glyph {
                    unicode: 0x20,
                    bbox: Rect::new(o.x, o.y, o.x, o.y),
                    loose_bbox: None,
                    origin: o,
                    font_size: 1.0,
                    font_id: font,
                    angle: 0.0,
                    generated: true,
                    unicode_map_error: false,
                    hyphen: false,
                    invisible: false,
                    text_object: None,
                });
                cursor += adv;
                continue;
            }
            let (bottom, top) = if c == ' ' {
                (0.0, 0.001)
            } else if "gjpqy".contains(c) {
                (-0.2 * size, 0.7 * size)
            } else if c.is_ascii() {
                (0.0, 0.7 * size)
            } else {
                (-0.12 * size, 0.83 * size)
            };
            let ink_w = if c == ' ' { adv } else { adv * 0.9 };
            let corners = [
                place(cursor, bottom),
                place(cursor + ink_w, bottom),
                place(cursor, top),
                place(cursor + ink_w, top),
            ];
            let bbox = Rect::new(
                corners.iter().map(|p| p.x).fold(f32::MAX, f32::min),
                corners.iter().map(|p| p.y).fold(f32::MAX, f32::min),
                corners.iter().map(|p| p.x).fold(f32::MIN, f32::max),
                corners.iter().map(|p| p.y).fold(f32::MIN, f32::max),
            );
            let lcorners = [
                place(cursor, -0.22 * size),
                place(cursor + adv, -0.22 * size),
                place(cursor, 0.78 * size),
                place(cursor + adv, 0.78 * size),
            ];
            let loose = Rect::new(
                lcorners.iter().map(|p| p.x).fold(f32::MAX, f32::min),
                lcorners.iter().map(|p| p.y).fold(f32::MAX, f32::min),
                lcorners.iter().map(|p| p.x).fold(f32::MIN, f32::max),
                lcorners.iter().map(|p| p.y).fold(f32::MIN, f32::max),
            );
            self.page.glyphs.push(Glyph {
                unicode: u32::from(c),
                bbox,
                loose_bbox: Some(loose),
                origin: place(cursor, 0.0),
                font_size: size,
                font_id: font,
                angle: opts.angle_deg.rem_euclid(360.0),
                generated: false,
                unicode_map_error: opts.unicode_map_error,
                hyphen: false,
                invisible: opts.invisible,
                text_object: Some(object),
            });
            out.push(self.page.glyphs.len() - 1);
            cursor += adv;
        }
        out
    }

    pub fn finish(self) -> PageText {
        self.page
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TextOpts {
    pub angle_deg: f32,
    pub extra_space: f32,
    pub unicode_map_error: bool,
    pub invisible: bool,
}
