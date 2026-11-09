//! 文字块识别（方案 5.1 第 1 条、P0 计划 V07）：把字形聚成行，再把行聚成文字块（段落），
//! 推断每块的对齐方式和行距。
//!
//! 流程：
//! 1. 按基线方向把字形分到 0°/90°/180°/270° 四个象限，每个象限转到规范坐标系
//!    （u 沿基线、v 垂直基线，见 [`Quadrant`]）；其他角度的字形按文字对象各自成块并标为非水平。
//! 2. 行：按 v 降序扫描一遍，用基线容差、字号比、水平间距和竖直细线决定字形归入哪条开放的行
//!    （[`lines`]）。开放的行只有当前基线附近的几条，一页上万字符也是线性时间。
//! 3. 段：行按 v 降序扫描，按水平重叠、基线距（相对页面与块内行距中位数）、字号、字体、
//!    水平细线决定归入哪个开放的块；再在块内按行距跳变、首行缩进、两端对齐段末短行二次切分
//!    （[`blocks`]）。
//! 4. 每块推断对齐方式（左/中/右/两端）与行距（基线距中位数），并汇总可编辑性与可见性。

mod blocks;
mod lines;
mod params;

use serde::{Deserialize, Serialize};

use crate::geom::{Quadrant, Rect};
use crate::glyph::PageText;

pub use params::LayoutParams;

/// 块的对齐方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Left,
    Center,
    Right,
    Justify,
}

/// 行距：块内相邻基线距离的中位数。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinePitch {
    /// 绝对值（点）。
    pub points: f32,
    /// 相对块主字号的倍数。
    pub em: f32,
}

/// 一行文字。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    /// 字形索引（[`PageText::glyphs`] 的下标，即 PDFium 字符索引），按阅读顺序。
    pub glyphs: Vec<usize>,
    /// 基线位置：水平文字（0°/180°）是页面 y，竖直方向（90°/270°）是页面 x。
    pub baseline: f32,
    /// 包围盒（页面坐标）。
    pub bbox: Rect,
    /// 主字体（按字形数最多者）。
    pub font_id: u32,
    pub font_name: String,
    /// 主字号（页面坐标系里的实际字号，取众数）。
    pub font_size: f32,
    /// 基线方向（度）。
    pub angle: f32,
    /// 没有"无 Unicode 映射且有可见字形"的字符（[`crate::Glyph::blocks_editing`]）。
    pub editable: bool,
    /// 全部字形都不可见（`Tr 3`/`Tr 7`）。
    pub invisible: bool,
}

/// 一个文字块（段落）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// 行索引（[`Layout::lines`] 的下标），自上而下。
    pub lines: Vec<usize>,
    pub bbox: Rect,
    pub alignment: Alignment,
    /// 只有一行时为 `None`。
    pub line_pitch: Option<LinePitch>,
    pub font_id: u32,
    pub font_name: String,
    pub font_size: f32,
    /// 块内没有"无 Unicode 映射且有可见字形"的字符（方案 5.1 第 2 条；[`crate::Glyph::blocks_editing`]）。
    pub editable: bool,
    /// 显示时水平：基线方向相对页面 `/Rotate` 为 0° 或 180°。v1 只对这类块做段落级编辑。
    pub horizontal: bool,
    /// 基线方向（度，内容流坐标系）。
    pub angle: f32,
    pub invisible: bool,
}

/// 聚类结果。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub blocks: Vec<Block>,
}

impl Layout {
    /// 某块的全部字形索引，按阅读顺序。
    pub fn block_glyphs(&self, block: &Block) -> Vec<usize> {
        block
            .lines
            .iter()
            .filter_map(|&i| self.lines.get(i))
            .flat_map(|l| l.glyphs.iter().copied())
            .collect()
    }
}

/// 规范坐标系里的一个字形（只含聚类要用的量）。
#[derive(Debug, Clone, Copy)]
struct CanonGlyph {
    /// [`PageText::glyphs`] 下标。
    index: usize,
    quadrant: Quadrant,
    /// 沿基线的范围。
    u0: f32,
    u1: f32,
    /// 基线。
    v: f32,
    /// 垂直基线方向的范围（字体度量估计）。
    v0: f32,
    v1: f32,
    size: f32,
    font_id: u32,
    text_object: Option<usize>,
}

/// 行的中间表示（规范坐标系）。
#[derive(Debug, Clone)]
struct LineBuild {
    quadrant: Quadrant,
    /// 若字形方向不在四个象限内，记录实际角度，且不参与块聚类。
    other_angle: Option<f32>,
    /// 规范坐标系里的字形。
    glyphs: Vec<CanonGlyph>,
    u0: f32,
    u1: f32,
    v: f32,
    v0: f32,
    v1: f32,
    size: f32,
    font_id: u32,
    /// 全部字形是同一字体。
    uniform_font: bool,
}

impl LineBuild {
    fn width(&self) -> f32 {
        self.u1 - self.u0
    }
}

/// 对一页字形做行、块聚类。
pub fn analyze(page: &PageText, params: &LayoutParams) -> Layout {
    let built = lines::cluster_lines(page, params);
    blocks::cluster_blocks(page, built, params)
}

/// 众数：按 `bucket` 宽度分桶，返回人数最多的桶里最小的值；平局取较小者；空输入返回 0。
fn mode_f32(values: &mut [f32], bucket: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    let key = |v: f32| (v / bucket).round() as i64;
    let (mut best, mut best_count) = (values[0], 0usize);
    let mut i = 0;
    while i < values.len() {
        let k = key(values[i]);
        let mut j = i;
        while j < values.len() && key(values[j]) == k {
            j += 1;
        }
        if j - i > best_count {
            best_count = j - i;
            best = values[i];
        }
        i = j;
    }
    best
}

/// 最大值与最小值之差；空输入返回 0。
fn spread(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().copied().fold(f32::MIN, f32::max)
        - values.iter().copied().fold(f32::MAX, f32::min)
}

/// 中位数；空输入返回 0。
fn median_f32(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) * 0.5
    }
}

/// 出现次数最多的值，平局取先出现者；空输入返回 0。
fn mode_u32(values: &[u32]) -> u32 {
    let mut counts: Vec<(u32, usize)> = Vec::new();
    for &v in values {
        match counts.iter_mut().find(|(k, _)| *k == v) {
            Some((_, c)) => *c += 1,
            None => counts.push((v, 1)),
        }
    }
    let mut best = (0u32, 0usize);
    for &(k, c) in &counts {
        if c > best.1 {
            best = (k, c);
        }
    }
    best.0
}

#[cfg(test)]
mod tests;
