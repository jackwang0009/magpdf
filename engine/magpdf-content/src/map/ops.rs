//! 操作符级比对（方案 2.3、13.2）：同一页重写前后，逐个对象比画它时生效的颜色空间、图形状态参数和
//! 文字状态。
//!
//! 口径照 `tests/corpus/tools/ops_diff.py`（每晚往返测试的操作符级比对）：
//! - 颜色：画这个对象实际用到的填充、描边颜色空间（[`MappedObject::painted_spaces`]）；
//! - 图形状态参数：`ca`、`CA`、`BM`、软蒙版（类型、蒙版组的内容、`BC`、`TR`，两边都有蒙版时再比 `gs`
//!   生效那一刻的变换矩阵）、`OP`、`op`、`OPM`、`TK`、`FL`、`RI`，ExtGState 里其余的条目按值比；
//!   描边的对象和表单另外比线宽、线帽、接头、斜接限制、虚线；
//! - 文字状态：`Tc`、`Tw`、渲染模式、字体（原来的字体找不到时不算），以及字号 × 水平缩放 × `Tm` × CTM 的
//!   线性部分。`Tz` 折算进 `Tm`、`TL`/`Ts` 折算进位置的不算差异。
//!
//! 对象按下标一一对应（两边都是 [`super::map_page`] 的结果，下标就是 PDFium 的页面对象索引）。

use std::collections::{BTreeMap, BTreeSet};

use super::{Detail, MappedObject, Matrix, PageMap};

/// 数值比较的相对容差（`ops_diff.py` 的 `TEXT_TOLERANCE`）。
const TOLERANCE: f64 = 1e-3;
/// 虚线数组的绝对容差（`ops_diff.py` 的 `VALUE_TOLERANCE`）。
const DASH_TOLERANCE: f64 = 2e-3;

/// 画对象时生效的、操作符级比对要看的状态（`ops_diff.py` 的 `State`，颜色另记在
/// [`MappedObject::fill_space`] 里）。
#[derive(Debug, Clone, PartialEq)]
pub struct OpParams {
    pub fill_alpha: f64,
    pub stroke_alpha: f64,
    pub blend: Vec<u8>,
    /// 软蒙版的描述；`None` 是没有蒙版。
    pub smask: String,
    /// 设软蒙版那一刻的变换矩阵（软蒙版按它摆放）；没有蒙版时为空。
    pub smask_ctm: Option<Matrix>,
    pub overprint_stroke: bool,
    pub overprint_fill: bool,
    pub overprint_mode: i64,
    pub knockout: bool,
    pub flatness: f64,
    pub intent: Vec<u8>,
    /// ExtGState 里上面没有单列的条目：键 → 值的描述。
    pub gs_other: BTreeMap<Vec<u8>, String>,
    pub line_width: f64,
    pub line_cap: i64,
    pub line_join: i64,
    pub miter_limit: f64,
    pub dash: (Vec<f64>, f64),
    pub char_spacing: f64,
    pub word_spacing: f64,
    pub horizontal_scale: f64,
    pub leading: f64,
    pub rise: f64,
    /// 字体写成 `子类型:BaseFont`，资源里没有时是 `Missing`。
    pub font: String,
}

impl Default for OpParams {
    fn default() -> Self {
        Self {
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            blend: b"Normal".to_vec(),
            smask: "None".to_owned(),
            smask_ctm: None,
            overprint_stroke: false,
            overprint_fill: false,
            overprint_mode: 0,
            knockout: true,
            flatness: 1.0,
            intent: b"RelativeColorimetric".to_vec(),
            gs_other: BTreeMap::new(),
            line_width: 1.0,
            line_cap: 0,
            line_join: 0,
            miter_limit: 10.0,
            dash: (Vec::new(), 0.0),
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 100.0,
            leading: 0.0,
            rise: 0.0,
            font: String::new(),
        }
    }
}

/// 一页重写前后的操作符级差异，三类各有几处（一个对象的一项算一处）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpDiff {
    pub color: usize,
    pub gs: usize,
    pub text: usize,
}

impl OpDiff {
    pub fn is_empty(&self) -> bool {
        self.color == 0 && self.gs == 0 && self.text == 0
    }
}

/// 比较同一页重写前后的两份解释结果。两边有一边解释不下去、对象个数或种类对不上时返回 `None`
/// （比不了）。
pub fn compare_pages(before: &PageMap, after: &PageMap) -> Option<OpDiff> {
    if before.blocked.is_some()
        || after.blocked.is_some()
        || before.objects.len() != after.objects.len()
    {
        return None;
    }
    let mut diff = OpDiff::default();
    for (a, b) in before.objects.iter().zip(&after.objects) {
        if a.kind != b.kind {
            return None;
        }
        compare_objects(a, b, &mut diff);
    }
    Some(diff)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= TOLERANCE * 1f64.max(a.abs()).max(b.abs())
}

fn close_matrix(a: &Matrix, b: &Matrix) -> bool {
    [
        (a.a, b.a),
        (a.b, b.b),
        (a.c, b.c),
        (a.d, b.d),
        (a.e, b.e),
        (a.f, b.f),
    ]
    .iter()
    .all(|(x, y)| close(*x, *y))
}

/// 画这个对象要用描边的那一套参数（`ops_diff.py` 里记 `line` 的事件）。
fn uses_stroke(object: &MappedObject) -> bool {
    match &object.detail {
        Detail::Path(path) => path.stroke,
        Detail::Text(text) => matches!(text.render_mode, 1 | 2 | 5 | 6),
        Detail::Form => true,
        _ => false,
    }
}

fn compare_objects(a: &MappedObject, b: &MappedObject, diff: &mut OpDiff) {
    let (fill_a, stroke_a) = a.painted_spaces();
    let (fill_b, stroke_b) = b.painted_spaces();
    diff.color += usize::from(fill_a != fill_b) + usize::from(stroke_a != stroke_b);

    let (pa, pb) = (&a.params, &b.params);
    let gs = [
        close(pa.fill_alpha, pb.fill_alpha),
        close(pa.stroke_alpha, pb.stroke_alpha),
        pa.blend == pb.blend,
        pa.smask == pb.smask,
        match (&pa.smask_ctm, &pb.smask_ctm) {
            (Some(x), Some(y)) => close_matrix(x, y),
            _ => true,
        },
        pa.overprint_stroke == pb.overprint_stroke,
        pa.overprint_fill == pb.overprint_fill,
        pa.overprint_mode == pb.overprint_mode,
        pa.knockout == pb.knockout,
        close(pa.flatness, pb.flatness),
        pa.intent == pb.intent,
    ];
    diff.gs += gs.iter().filter(|same| !**same).count();
    let keys: BTreeSet<&Vec<u8>> = pa.gs_other.keys().chain(pb.gs_other.keys()).collect();
    diff.gs += keys
        .into_iter()
        .filter(|key| pa.gs_other.get(*key) != pb.gs_other.get(*key))
        .count();
    if uses_stroke(a) && uses_stroke(b) {
        let dash_same = pa.dash.0.len() == pb.dash.0.len()
            && pa
                .dash
                .0
                .iter()
                .zip(&pb.dash.0)
                .all(|(x, y)| (x - y).abs() <= DASH_TOLERANCE)
            && (pa.dash.1 - pb.dash.1).abs() <= DASH_TOLERANCE;
        let line = [
            close(pa.line_width, pb.line_width),
            close(pa.miter_limit, pb.miter_limit),
            pa.line_cap == pb.line_cap,
            pa.line_join == pb.line_join,
            dash_same,
        ];
        diff.gs += line.iter().filter(|same| !**same).count();
    }

    if let (Detail::Text(ta), Detail::Text(tb)) = (&a.detail, &b.detail) {
        let text = [
            close(pa.char_spacing, pb.char_spacing),
            close(pa.word_spacing, pb.word_spacing),
            ta.render_mode == tb.render_mode,
            pa.font == pb.font || pa.font == "Missing",
            ta.lin.iter().zip(&tb.lin).all(|(x, y)| close(*x, *y)),
        ];
        diff.text += text.iter().filter(|same| !**same).count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::Content;
    use crate::map::{NoResources, map_content};

    fn diff(before: &[u8], after: &[u8]) -> Option<OpDiff> {
        let map = |data: &[u8]| map_content(Content::from_single(1, data.to_vec()), &NoResources);
        compare_pages(&map(before), &map(after))
    }

    #[test]
    fn text_state_changes_are_counted_and_folded_scaling_is_not() {
        assert_eq!(
            diff(b"BT /F1 10 Tf 2 Tc (a) Tj ET", b"BT /F1 10 Tf (a) Tj ET").map(|d| d.text),
            Some(1)
        );
        assert_eq!(
            diff(
                b"BT /F1 10 Tf 50 Tz (a) Tj ET",
                b"BT /F1 10 Tf 0.5 0 0 1 0 0 Tm (a) Tj ET"
            ),
            Some(OpDiff::default()),
            "Tz 折算进 Tm，字的大小方向一样"
        );
        assert_eq!(
            diff(b"BT /F1 10 Tf 50 Tz (a) Tj ET", b"BT /F1 10 Tf (a) Tj ET").map(|d| d.text),
            Some(1),
            "Tz 丢了：字变宽"
        );
    }

    #[test]
    fn line_parameters_count_only_for_stroked_objects() {
        assert_eq!(
            diff(b"2 w 0 0 1 1 re S", b"0 0 1 1 re S").map(|d| d.gs),
            Some(1)
        );
        assert_eq!(
            diff(b"2 w [3 1] 0 d 0 0 1 1 re f", b"0 0 1 1 re f"),
            Some(OpDiff::default())
        );
        assert_eq!(
            diff(b"0.5 g 0 0 1 1 re f", b"0.5 0.5 0.5 rg 0 0 1 1 re f").map(|d| d.color),
            Some(1)
        );
    }

    #[test]
    fn different_object_counts_are_not_comparable() {
        assert_eq!(diff(b"0 0 1 1 re f 0 0 1 1 re f", b"0 0 1 1 re f"), None);
        assert_eq!(diff(b"0 0 1 1 re f", b"BT /F1 1 Tf (a) Tj ET"), None);
    }
}
