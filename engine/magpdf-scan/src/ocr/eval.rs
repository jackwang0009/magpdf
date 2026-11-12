//! OCR 评测指标（测试样本收集与脱敏规范 §7）：
//!
//! - 字符准确率 = 1 − 各行编辑距离之和 ÷ 标注字符总数，按页面语言分别统计；
//! - 文字行召回率 = 与标注行交并比 ≥ 0.5 的检测行数 ÷ 标注行数。
//!
//! 标注里的 `□`（看不清的字）不计入字符数，编辑距离里与任何字符匹配都不计代价。
//! `seal` / `handwriting` / `###` 区域不参与召回与准确率；落在其中的识别行单独计数（误识别统计），
//! 不算误检。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::format::{Annotation, LineRecord, RegionKind, to_points};
use super::geometry::{Point, point_in_polygon, polygon_iou};

/// 看不清的字的占位符。
pub const UNREADABLE: char = '□';

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EvalOptions {
    /// 行匹配的交并比阈值。
    pub iou_thresh: f32,
    /// 比较文字前去掉全部空白（PP-OCR 中文输出通常不含空格，用于参考口径）。
    pub ignore_space: bool,
}

impl Default for EvalOptions {
    fn default() -> Self {
        Self {
            iou_thresh: 0.5,
            ignore_space: false,
        }
    }
}

/// 一条标注行与预测的匹配情况。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineMatch {
    pub gt_index: usize,
    /// 匹配到的预测行下标；未匹配为 `None`。
    pub pred_index: Option<usize>,
    pub iou: f32,
    pub gt_chars: usize,
    pub edit_distance: usize,
}

/// 一张图的评测结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PageMetrics {
    pub gt_lines: usize,
    pub matched_lines: usize,
    pub gt_chars: usize,
    pub edit_distance: usize,
    pub pred_lines: usize,
    /// 未匹配且不在印章/手写/忽略区域里的预测行。
    pub false_positives: usize,
    pub in_seal: usize,
    pub in_handwriting: usize,
    pub in_ignored: usize,
    /// 页级口径：标注与预测各自按阅读顺序拼接、去掉空白后的字符数与编辑距离（不受行切分差异影响）。
    pub page_gt_chars: usize,
    pub page_edit_distance: usize,
    pub matches: Vec<LineMatch>,
}

impl PageMetrics {
    pub fn line_recall(&self) -> Option<f32> {
        (self.gt_lines > 0).then(|| self.matched_lines as f32 / self.gt_lines as f32)
    }

    pub fn char_accuracy(&self) -> Option<f32> {
        (self.gt_chars > 0)
            .then(|| (1.0 - self.edit_distance as f32 / self.gt_chars as f32).clamp(0.0, 1.0))
    }

    pub fn page_char_accuracy(&self) -> Option<f32> {
        (self.page_gt_chars > 0).then(|| {
            (1.0 - self.page_edit_distance as f32 / self.page_gt_chars as f32).clamp(0.0, 1.0)
        })
    }
}

/// 累计量（按语言或总计）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub images: usize,
    pub gt_lines: usize,
    pub matched_lines: usize,
    pub gt_chars: usize,
    pub edit_distance: usize,
    pub pred_lines: usize,
    pub false_positives: usize,
    pub in_seal: usize,
    pub in_handwriting: usize,
    pub in_ignored: usize,
    pub page_gt_chars: usize,
    pub page_edit_distance: usize,
}

impl Totals {
    pub fn add(&mut self, m: &PageMetrics) {
        self.images += 1;
        self.gt_lines += m.gt_lines;
        self.matched_lines += m.matched_lines;
        self.gt_chars += m.gt_chars;
        self.edit_distance += m.edit_distance;
        self.pred_lines += m.pred_lines;
        self.false_positives += m.false_positives;
        self.in_seal += m.in_seal;
        self.in_handwriting += m.in_handwriting;
        self.in_ignored += m.in_ignored;
        self.page_gt_chars += m.page_gt_chars;
        self.page_edit_distance += m.page_edit_distance;
    }

    /// 页级字符准确率（附加口径，见 [`PageMetrics::page_gt_chars`]）。
    pub fn page_char_accuracy(&self) -> Option<f32> {
        (self.page_gt_chars > 0).then(|| {
            (1.0 - self.page_edit_distance as f32 / self.page_gt_chars as f32).clamp(0.0, 1.0)
        })
    }

    pub fn line_recall(&self) -> Option<f32> {
        (self.gt_lines > 0).then(|| self.matched_lines as f32 / self.gt_lines as f32)
    }

    /// 匹配行 ÷ 预测行（误检参考）。
    pub fn line_precision(&self) -> Option<f32> {
        (self.pred_lines > 0).then(|| self.matched_lines as f32 / self.pred_lines as f32)
    }

    pub fn char_accuracy(&self) -> Option<f32> {
        (self.gt_chars > 0)
            .then(|| (1.0 - self.edit_distance as f32 / self.gt_chars as f32).clamp(0.0, 1.0))
    }
}

/// 整体报告：按语言与总计。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub options: Option<EvalOptions>,
    pub per_lang: BTreeMap<String, Totals>,
    pub total: Totals,
}

impl Report {
    pub fn add(&mut self, lang: &str, m: &PageMetrics) {
        self.per_lang.entry(lang.to_owned()).or_default().add(m);
        self.total.add(m);
    }
}

/// 带 `□` 通配的编辑距离：标注里的 `□` 与任意字符替换、或被删除都不计代价。
pub fn edit_distance(gt: &[char], pred: &[char]) -> usize {
    let n = pred.len();
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur = vec![0usize; n + 1];
    for &g in gt {
        let del_cost = usize::from(g != UNREADABLE);
        cur[0] = prev[0] + del_cost;
        for (j, &p) in pred.iter().enumerate() {
            let sub_cost = usize::from(g != UNREADABLE && g != p);
            cur[j + 1] = (prev[j] + sub_cost)
                .min(prev[j + 1] + del_cost)
                .min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

fn chars_of(text: &str, ignore_space: bool) -> Vec<char> {
    text.chars()
        .filter(|c| !(ignore_space && c.is_whitespace()))
        .collect()
}

fn centroid(points: &[Point]) -> Point {
    let n = points.len().max(1) as f32;
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |(x, y), p| (x + p.x, y + p.y));
    Point::new(sx / n, sy / n)
}

/// 评测一张图：预测行 vs 标注。
pub fn evaluate_page(
    preds: &[LineRecord],
    annotations: &[Annotation],
    options: &EvalOptions,
) -> PageMetrics {
    let pred_polys: Vec<Vec<Point>> = preds.iter().map(|p| to_points(&p.points)).collect();
    let text_gt: Vec<(usize, &Annotation, Vec<Point>)> = annotations
        .iter()
        .enumerate()
        .filter(|(_, a)| a.kind == RegionKind::Text)
        .map(|(i, a)| (i, a, to_points(&a.points)))
        .collect();

    // 所有 (标注, 预测) 对按交并比降序贪心匹配。
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for (gi, (_, _, gpoly)) in text_gt.iter().enumerate() {
        for (pi, ppoly) in pred_polys.iter().enumerate() {
            let iou = polygon_iou(gpoly, ppoly);
            if iou >= options.iou_thresh {
                pairs.push((iou, gi, pi));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut gt_to_pred: Vec<Option<(usize, f32)>> = vec![None; text_gt.len()];
    let mut pred_used = vec![false; preds.len()];
    for (iou, gi, pi) in pairs {
        if gt_to_pred[gi].is_none() && !pred_used[pi] {
            gt_to_pred[gi] = Some((pi, iou));
            pred_used[pi] = true;
        }
    }

    let mut metrics = PageMetrics {
        gt_lines: text_gt.len(),
        pred_lines: preds.len(),
        ..PageMetrics::default()
    };
    for (gi, (gt_index, ann, _)) in text_gt.iter().enumerate() {
        let gt_chars_all = chars_of(&ann.transcription, options.ignore_space);
        let gt_chars = gt_chars_all.iter().filter(|&&c| c != UNREADABLE).count();
        let (pred_index, iou, distance) = match gt_to_pred[gi] {
            Some((pi, iou)) => {
                let pred_chars = chars_of(&preds[pi].text, options.ignore_space);
                (Some(pi), iou, edit_distance(&gt_chars_all, &pred_chars))
            }
            None => (None, 0.0, gt_chars),
        };
        if pred_index.is_some() {
            metrics.matched_lines += 1;
        }
        metrics.gt_chars += gt_chars;
        metrics.edit_distance += distance;
        metrics.matches.push(LineMatch {
            gt_index: *gt_index,
            pred_index,
            iou,
            gt_chars,
            edit_distance: distance,
        });
    }

    // 未匹配的预测：落在特殊区域里的分类计数，其余算误检。
    let special: Vec<(RegionKind, Vec<Point>)> = annotations
        .iter()
        .filter(|a| a.kind != RegionKind::Text)
        .map(|a| (a.kind, to_points(&a.points)))
        .collect();
    let mut in_special = vec![false; preds.len()];
    for (pi, ppoly) in pred_polys.iter().enumerate() {
        let center = centroid(ppoly);
        let hit = special.iter().find(|(_, region)| {
            point_in_polygon(center, region) || polygon_iou(region, ppoly) >= options.iou_thresh
        });
        in_special[pi] = hit.is_some();
        if pred_used[pi] {
            continue;
        }
        match hit.map(|(k, _)| *k) {
            Some(RegionKind::Seal) => metrics.in_seal += 1,
            Some(RegionKind::Handwriting) => metrics.in_handwriting += 1,
            Some(RegionKind::Ignore) => metrics.in_ignored += 1,
            _ => metrics.false_positives += 1,
        }
    }

    // 页级口径：各自按阅读顺序拼接、去掉空白。落在印章、手写、忽略区域里的识别行不算进去，
    // 但已经和标注行配上的要算：被印章压住的字是标注了的文字行，它的识别结果不能丢。
    let gt_text: Vec<char> = reading_order_text(
        text_gt
            .iter()
            .map(|(_, a, poly)| (a.transcription.as_str(), poly.as_slice())),
    );
    let pred_text: Vec<char> = reading_order_text(
        preds
            .iter()
            .zip(&pred_polys)
            .enumerate()
            .filter(|(pi, _)| pred_used[*pi] || !in_special[*pi])
            .map(|(_, (p, poly))| (p.text.as_str(), poly.as_slice())),
    );
    metrics.page_gt_chars = gt_text.iter().filter(|&&c| c != UNREADABLE).count();
    metrics.page_edit_distance = edit_distance(&gt_text, &pred_text);
    metrics
}

/// 按阅读顺序（先行后列）拼接文字并去掉全部空白。行的划分：中心 y 相差不超过半个行高视为同一行。
fn reading_order_text<'a>(items: impl Iterator<Item = (&'a str, &'a [Point])>) -> Vec<char> {
    let mut rows: Vec<(f32, f32, f32, &str)> = items
        .map(|(text, poly)| {
            let (x0, y0, _, y1) = super::geometry::bounding_box(poly);
            ((y0 + y1) / 2.0, x0, (y1 - y0).max(1.0), text)
        })
        .collect();
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut ordered: Vec<(f32, f32, f32, &str)> = Vec::with_capacity(rows.len());
    let mut i = 0;
    while i < rows.len() {
        let mut j = i + 1;
        while j < rows.len() && (rows[j].0 - rows[i].0).abs() <= rows[i].2 * 0.5 {
            j += 1;
        }
        let mut row: Vec<_> = rows[i..j].to_vec();
        row.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        ordered.extend(row);
        i = j;
    }
    ordered
        .iter()
        .flat_map(|(_, _, _, t)| t.chars())
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn edit_distance_basic_and_wildcard() {
        assert_eq!(edit_distance(&chars("kitten"), &chars("sitting")), 3);
        assert_eq!(edit_distance(&chars(""), &chars("abc")), 3);
        assert_eq!(edit_distance(&chars("abc"), &chars("")), 3);
        assert_eq!(edit_distance(&chars("提单号"), &chars("提单号")), 0);
        // □ 匹配任意字符或缺失都不计代价。
        assert_eq!(edit_distance(&chars("A□B"), &chars("AXB")), 0);
        assert_eq!(edit_distance(&chars("A□B"), &chars("AB")), 0);
        assert_eq!(edit_distance(&chars("A□B"), &chars("AXYB")), 1);
        assert_eq!(edit_distance(&chars("□□"), &chars("")), 0);
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<[f32; 2]> {
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    }

    fn pred(text: &str, points: Vec<[f32; 2]>) -> LineRecord {
        LineRecord {
            points,
            text: text.to_owned(),
            confidence: 0.9,
            low_confidence: false,
            det_score: 0.9,
            rotated_180: false,
            vertical: false,
            char_confidences: Vec::new(),
        }
    }

    fn ann(text: &str, points: Vec<[f32; 2]>, kind: RegionKind) -> Annotation {
        Annotation {
            transcription: text.to_owned(),
            points,
            kind,
        }
    }

    #[test]
    fn page_metrics_match_recall_and_accuracy() {
        let gts = vec![
            ann(
                "提单号 ABC123",
                rect(0.0, 0.0, 100.0, 10.0),
                RegionKind::Text,
            ),
            ann("Shipper", rect(0.0, 20.0, 50.0, 30.0), RegionKind::Text),
            ann("漏检的行", rect(0.0, 40.0, 50.0, 50.0), RegionKind::Text),
            ann("", rect(200.0, 0.0, 240.0, 40.0), RegionKind::Seal),
            ann("###", rect(300.0, 0.0, 340.0, 40.0), RegionKind::Ignore),
        ];
        let preds = vec![
            pred("提单号 ABC128", rect(1.0, 0.0, 101.0, 10.0)), // 匹配，1 个替换
            pred("Shipper", rect(0.0, 21.0, 50.0, 31.0)),       // 完全正确
            pred("印章字", rect(205.0, 5.0, 235.0, 15.0)),      // 落在印章里
            pred("噪声", rect(500.0, 500.0, 520.0, 510.0)),     // 误检
            pred("忽略", rect(310.0, 10.0, 330.0, 20.0)),       // 落在忽略区
        ];
        let m = evaluate_page(&preds, &gts, &EvalOptions::default());
        assert_eq!(m.gt_lines, 3);
        assert_eq!(m.matched_lines, 2);
        assert_eq!(m.pred_lines, 5);
        assert_eq!(m.in_seal, 1);
        assert_eq!(m.in_ignored, 1);
        assert_eq!(m.false_positives, 1);
        // 字符：10 + 7 + 4 = 21；编辑距离：1 + 0 + 4（漏检整行）= 5。
        assert_eq!(m.gt_chars, 21);
        assert_eq!(m.edit_distance, 5);
        assert!((m.line_recall().unwrap() - 2.0 / 3.0).abs() < 1e-6);
        assert!((m.char_accuracy().unwrap() - (1.0 - 5.0 / 21.0)).abs() < 1e-6);
        assert_eq!(m.matches[0].pred_index, Some(0));
        assert_eq!(m.matches[2].pred_index, None);
    }

    #[test]
    fn ignore_space_option_and_unreadable_chars() {
        let gts = vec![ann("A □B", rect(0.0, 0.0, 10.0, 10.0), RegionKind::Text)];
        let preds = vec![pred("AXB", rect(0.0, 0.0, 10.0, 10.0))];
        let strict = evaluate_page(&preds, &gts, &EvalOptions::default());
        // 字符数不含 □：3（A、空格、B）；"A □B" vs "AXB"：空格→X 替换 1，□ 删除 0 → 1。
        assert_eq!((strict.gt_chars, strict.edit_distance), (3, 1));
        let loose = evaluate_page(
            &preds,
            &gts,
            &EvalOptions {
                ignore_space: true,
                ..EvalOptions::default()
            },
        );
        assert_eq!((loose.gt_chars, loose.edit_distance), (2, 0));
    }

    #[test]
    fn page_level_text_ignores_line_segmentation() {
        // 标注把一行拆成两个字段，预测合成一行：行指标受影响，页级口径不受影响。
        let gts = vec![
            ann(
                "Invoice No. 1",
                rect(0.0, 0.0, 100.0, 10.0),
                RegionKind::Text,
            ),
            ann(
                "Date: 2024",
                rect(150.0, 0.0, 250.0, 10.0),
                RegionKind::Text,
            ),
            ann(
                "Second line",
                rect(0.0, 20.0, 100.0, 30.0),
                RegionKind::Text,
            ),
        ];
        let preds = vec![
            pred("Second line", rect(0.0, 20.0, 100.0, 30.0)),
            pred("Invoice No. 1 Date: 2024", rect(0.0, 0.0, 250.0, 10.0)),
        ];
        let m = evaluate_page(&preds, &gts, &EvalOptions::default());
        assert_eq!(m.matched_lines, 1);
        assert_eq!(
            m.page_gt_chars,
            "InvoiceNo.1Date:2024Secondline".chars().count()
        );
        assert_eq!(m.page_edit_distance, 0);
        assert_eq!(m.page_char_accuracy(), Some(1.0));
    }

    #[test]
    fn page_level_text_keeps_matched_lines_under_a_seal() {
        // 落款那一行被印章压住：它是标注了的文字行，识别结果要算进页级口径；
        // 印章里的字（没有对应的标注行）不算。
        let gts = vec![
            ann("第一行", rect(0.0, 0.0, 100.0, 10.0), RegionKind::Text),
            ann("日期：2024", rect(0.0, 40.0, 100.0, 50.0), RegionKind::Text),
            ann("", rect(20.0, 20.0, 90.0, 90.0), RegionKind::Seal),
        ];
        let preds = vec![
            pred("第一行", rect(0.0, 0.0, 100.0, 10.0)),
            pred("日期：2024", rect(0.0, 40.0, 100.0, 50.0)),
            pred("合同专用章", rect(30.0, 60.0, 80.0, 70.0)),
        ];
        let m = evaluate_page(&preds, &gts, &EvalOptions::default());
        assert_eq!(m.matched_lines, 2);
        assert_eq!(m.in_seal, 1);
        assert_eq!(m.false_positives, 0);
        assert_eq!(m.page_gt_chars, "第一行日期：2024".chars().count());
        assert_eq!(m.page_edit_distance, 0);
    }

    #[test]
    fn greedy_matching_prefers_higher_iou() {
        // 两条标注紧挨着，一条预测与第一条重合更多，只能匹配一条。
        let gts = vec![
            ann("甲", rect(0.0, 0.0, 100.0, 10.0), RegionKind::Text),
            ann("乙", rect(0.0, 8.0, 100.0, 18.0), RegionKind::Text),
        ];
        let preds = vec![pred("甲", rect(0.0, 0.0, 100.0, 11.0))];
        let m = evaluate_page(&preds, &gts, &EvalOptions::default());
        assert_eq!(m.matched_lines, 1);
        assert_eq!(m.matches[0].pred_index, Some(0));
        assert_eq!(m.matches[1].pred_index, None);
        assert_eq!(m.false_positives, 0);
    }

    #[test]
    fn report_aggregates_per_lang() {
        let mut r = Report::default();
        let m = PageMetrics {
            gt_lines: 2,
            matched_lines: 1,
            gt_chars: 10,
            edit_distance: 2,
            pred_lines: 3,
            false_positives: 1,
            ..PageMetrics::default()
        };
        r.add("zh", &m);
        r.add("zh", &m);
        r.add("en", &m);
        assert_eq!(r.per_lang["zh"].images, 2);
        assert_eq!(r.total.gt_lines, 6);
        assert!((r.total.char_accuracy().unwrap() - 0.8).abs() < 1e-6);
        assert!((r.per_lang["en"].line_recall().unwrap() - 0.5).abs() < 1e-6);
        assert!((r.total.line_precision().unwrap() - 3.0 / 9.0).abs() < 1e-6);
        assert!(Totals::default().char_accuracy().is_none());
    }
}
