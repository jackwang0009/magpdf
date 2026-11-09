use serde::{Deserialize, Serialize};

/// 聚类参数。所有相对量以 em（相对字号的倍数）表示，默认值的依据见
/// `docs/p0/V07-文字块识别原型.md` 第 3 节。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutParams {
    // ---- 行 ----
    /// 同一行内字符基线的允许偏差（相对两者中较小字号）。上标抬高约 0.33em，须大于它才分开。
    pub baseline_tol_em: f32,
    /// 同一基线上相邻字形的水平间距超过此值就分成两行（表格相邻单元格、表单的标签与内容）。
    /// 词间距一般 0.25–0.35em，两端对齐拉伸后极少超过 1.5em；Word 表格两个单元格的最小间距约 1.05–1.45em。
    pub max_word_gap_em: f32,
    /// 属于同一个文字对象（同一个 `Tj`/`TJ`）的字形允许的更大间距：生成器把一行写进一个对象时，
    /// 大段 `TJ` 调整造成的空隙不应拆行。
    pub same_object_gap_em: f32,
    /// 同一行内允许的最大字号比（大/小）。
    pub max_size_ratio_in_line: f32,
    /// 上下标合并：字号不超过主行的此倍数、字数不超过 `satellite_max_glyphs`、
    /// 纵向与主行重叠且水平落在主行范围内（或紧挨行尾）的小片段并入主行。
    pub satellite_size_ratio: f32,
    pub satellite_max_glyphs: usize,
    /// 文字方向与 0/90/180/270° 的允许偏差（度）。
    pub angle_tol_deg: f32,

    // ---- 段（文字块） ----
    /// 两行基线距小于此值视为并排片段而不是上下行。
    pub min_line_gap_em: f32,
    /// 上下两行水平范围的重叠须达到较窄一方宽度的此比例。
    pub block_overlap_ratio: f32,
    /// 基线距相对块内（或页面）行距中位数的允许放大比例；超过即换段。
    pub pitch_tolerance: f32,
    /// 页面上估不出行距时，块的第一对行允许的最大基线距（em）。
    pub first_pitch_em: f32,
    /// 任何情况下允许的最大基线距（em）。
    pub max_pitch_em: f32,
    /// 上下行主字号之比超过此值视为不同块（标题与正文）。
    pub block_size_ratio: f32,
    /// 新行比块现有宽度宽出此倍数时不并入（窄的表格列后面接整行正文）。
    pub width_growth_ratio: f32,
    /// 首行缩进判定：行左缘比块左缘靠右超过此值（em）。
    pub indent_em: f32,
    /// 段末判定：行右缘比块右缘短超过此值（em）。
    pub ragged_em: f32,
    /// 对齐判定容差（em）。
    pub align_tol_em: f32,

    // ---- 细线 ----
    /// 收集细线路径时的最大线宽（点）。
    pub ruling_max_thickness: f32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            baseline_tol_em: 0.25,
            max_word_gap_em: 1.5,
            same_object_gap_em: 3.0,
            max_size_ratio_in_line: 2.0,
            satellite_size_ratio: 0.8,
            satellite_max_glyphs: 4,
            angle_tol_deg: 1.0,

            min_line_gap_em: 0.5,
            block_overlap_ratio: 0.5,
            pitch_tolerance: 0.3,
            first_pitch_em: 1.8,
            max_pitch_em: 3.0,
            block_size_ratio: 1.15,
            width_growth_ratio: 2.5,
            indent_em: 1.0,
            ragged_em: 1.0,
            align_tol_em: 0.4,

            ruling_max_thickness: 2.5,
        }
    }
}
