//! 文字块识别的评测：标注文件格式与正确率计算（P0 计划 V07 的目标值：行 ≥ 98%，段 ≥ 90%）。
//!
//! 标注格式见 `docs/p0/V07-标注格式.md`。两种正确率（都按页算，再按字符数汇总）：
//!
//! - **字符一致率**（主指标）：把每个预测簇映射到与它重叠字符最多的标注簇，反过来也映射一次；
//!   一个字符算对，当且仅当它所在的预测簇和标注簇互为对方的最大重叠簇。被拆成两半的行只有
//!   较大的一半算对，被并到一起的两行只有较大的一行算对。
//! - **完全匹配率**：标注簇的字符集合与某个预测簇（只看标注过的字符）完全相同的比例，按簇数计。
//!
//! 没有标注的字符不参与评测（页眉页脚、页码可以不标）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::geom::Rect;
use crate::layout::Layout;

/// 标注文件当前格式版本。
pub const LABEL_FORMAT: &str = "magpdf-text-labels/1";

/// 一份标注文件（一个 PDF 的若干页）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelFile {
    /// 固定为 [`LABEL_FORMAT`]。
    pub format: String,
    /// 对应的样本编号或文件名，只作说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub pages: Vec<PageLabels>,
}

impl LabelFile {
    pub fn new(file: Option<String>, pages: Vec<PageLabels>) -> Self {
        Self {
            format: LABEL_FORMAT.to_owned(),
            file,
            pages,
        }
    }

    pub fn page(&self, page: usize) -> Option<&PageLabels> {
        self.pages.iter().find(|p| p.page == page)
    }
}

/// 一页的标注。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PageLabels {
    /// 页码，从 1 开始。
    pub page: usize,
    #[serde(default)]
    pub lines: Vec<ClusterLabel>,
    #[serde(default)]
    pub paragraphs: Vec<ClusterLabel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 一行或一段：用 PDFium 字符索引的闭区间列表表示。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClusterLabel {
    /// 闭区间 `[first, last]`，可以有多个（例如中间夹着不属于本行的字符）。
    pub chars: Vec<[usize; 2]>,
    /// 包围盒（页面坐标），给标注工具显示用，评测不看。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ClusterLabel {
    /// 把区间展开成去重、升序的字符索引。
    pub fn indices(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .chars
            .iter()
            .flat_map(|&[a, b]| {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                lo..=hi
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// 把一组索引压成闭区间列表。
    pub fn from_indices(indices: &[usize]) -> Self {
        let mut sorted = indices.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let mut chars: Vec<[usize; 2]> = Vec::new();
        for i in sorted {
            match chars.last_mut() {
                Some(last) if last[1] + 1 == i => last[1] = i,
                _ => chars.push([i, i]),
            }
        }
        Self {
            chars,
            bbox: None,
            note: None,
        }
    }
}

/// 一类簇（行或段）的评测结果。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ClusterScore {
    /// 标注过的字符数。
    pub labelled_chars: usize,
    /// 预测与标注一致的字符数。
    pub agreeing_chars: usize,
    /// 标注了但预测里没有的字符数（例如把 PDFium 生成的空格索引也标进去了）。
    pub unassigned_chars: usize,
    pub labelled_clusters: usize,
    /// 与标注字符有交集的预测簇数。
    pub predicted_clusters: usize,
    pub exact_matches: usize,
}

impl ClusterScore {
    pub fn char_agreement(&self) -> f64 {
        ratio(self.agreeing_chars, self.labelled_chars)
    }

    pub fn exact_match_rate(&self) -> f64 {
        ratio(self.exact_matches, self.labelled_clusters)
    }

    pub fn add(&mut self, other: &ClusterScore) {
        self.labelled_chars += other.labelled_chars;
        self.agreeing_chars += other.agreeing_chars;
        self.unassigned_chars += other.unassigned_chars;
        self.labelled_clusters += other.labelled_clusters;
        self.predicted_clusters += other.predicted_clusters;
        self.exact_matches += other.exact_matches;
    }
}

fn ratio(a: usize, b: usize) -> f64 {
    if b == 0 { 1.0 } else { a as f64 / b as f64 }
}

/// 一页的评测结果。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PageScore {
    pub page: usize,
    pub lines: ClusterScore,
    pub paragraphs: ClusterScore,
}

/// 多页汇总（按字符数、簇数求和）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Summary {
    pub pages: usize,
    pub lines: ClusterScore,
    pub paragraphs: ClusterScore,
}

impl Summary {
    pub fn add_page(&mut self, s: &PageScore) {
        self.pages += 1;
        self.lines.add(&s.lines);
        self.paragraphs.add(&s.paragraphs);
    }
}

/// 比较一组标注簇与一组预测簇。元素是字符索引；标注簇之间不应重叠，重叠时以先出现的为准。
pub fn score_clusters(labels: &[Vec<usize>], predicted: &[Vec<usize>]) -> ClusterScore {
    let mut label_of: HashMap<usize, usize> = HashMap::new();
    for (li, cluster) in labels.iter().enumerate() {
        for &c in cluster {
            label_of.entry(c).or_insert(li);
        }
    }
    let mut pred_of: HashMap<usize, usize> = HashMap::new();
    for (pi, cluster) in predicted.iter().enumerate() {
        for &c in cluster {
            if label_of.contains_key(&c) {
                pred_of.entry(c).or_insert(pi);
            }
        }
    }

    // overlap[(pi, li)]：预测簇 pi 与标注簇 li 共有的（标注过的）字符数。
    let mut overlap: HashMap<(usize, usize), usize> = HashMap::new();
    for (&c, &li) in &label_of {
        if let Some(&pi) = pred_of.get(&c) {
            *overlap.entry((pi, li)).or_insert(0) += 1;
        }
    }
    let mut best_label: HashMap<usize, (usize, usize)> = HashMap::new(); // pi -> (li, count)
    let mut best_pred: HashMap<usize, (usize, usize)> = HashMap::new(); // li -> (pi, count)
    let mut pred_size: HashMap<usize, usize> = HashMap::new(); // 预测簇里标注过的字符数
    for (&(pi, li), &n) in &overlap {
        *pred_size.entry(pi).or_insert(0) += n;
        let bl = best_label.entry(pi).or_insert((li, 0));
        if n > bl.1 || (n == bl.1 && li < bl.0) {
            *bl = (li, n);
        }
        let bp = best_pred.entry(li).or_insert((pi, 0));
        if n > bp.1 || (n == bp.1 && pi < bp.0) {
            *bp = (pi, n);
        }
    }

    let mut agreeing = 0usize;
    let mut unassigned = 0usize;
    for (&c, &li) in &label_of {
        match pred_of.get(&c) {
            None => unassigned += 1,
            Some(&pi) => {
                let ok = best_label.get(&pi).is_some_and(|&(l, _)| l == li)
                    && best_pred.get(&li).is_some_and(|&(p, _)| p == pi);
                if ok {
                    agreeing += 1;
                }
            }
        }
    }

    let mut exact = 0usize;
    for (li, cluster) in labels.iter().enumerate() {
        // 以 label_of 为准去掉重复标注的字符。
        let mine: Vec<usize> = cluster
            .iter()
            .copied()
            .filter(|c| label_of.get(c) == Some(&li))
            .collect();
        if mine.is_empty() {
            continue;
        }
        let Some(&(pi, n)) = best_pred.get(&li) else {
            continue;
        };
        if n == mine.len() && pred_size.get(&pi) == Some(&n) {
            exact += 1;
        }
    }

    ClusterScore {
        labelled_chars: label_of.len(),
        agreeing_chars: agreeing,
        unassigned_chars: unassigned,
        labelled_clusters: labels.iter().filter(|c| !c.is_empty()).count(),
        predicted_clusters: pred_size.len(),
        exact_matches: exact,
    }
}

/// 用一页的标注给聚类结果打分。
pub fn score_layout(labels: &PageLabels, layout: &Layout) -> PageScore {
    let line_labels: Vec<Vec<usize>> = labels.lines.iter().map(ClusterLabel::indices).collect();
    let para_labels: Vec<Vec<usize>> = labels
        .paragraphs
        .iter()
        .map(ClusterLabel::indices)
        .collect();
    let pred_lines: Vec<Vec<usize>> = layout.lines.iter().map(|l| l.glyphs.clone()).collect();
    let pred_blocks: Vec<Vec<usize>> = layout
        .blocks
        .iter()
        .map(|b| layout.block_glyphs(b))
        .collect();
    PageScore {
        page: labels.page,
        lines: score_clusters(&line_labels, &pred_lines),
        paragraphs: score_clusters(&para_labels, &pred_blocks),
    }
}

/// 把聚类结果写成标注（预标注，供标注人员修正）。
pub fn labels_from_layout(page: usize, layout: &Layout) -> PageLabels {
    PageLabels {
        page,
        lines: layout
            .lines
            .iter()
            .map(|l| ClusterLabel {
                bbox: Some(l.bbox),
                ..ClusterLabel::from_indices(&l.glyphs)
            })
            .collect(),
        paragraphs: layout
            .blocks
            .iter()
            .map(|b| ClusterLabel {
                bbox: Some(b.bbox),
                ..ClusterLabel::from_indices(&layout.block_glyphs(b))
            })
            .collect(),
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(range: std::ops::RangeInclusive<usize>) -> Vec<usize> {
        range.collect()
    }

    #[test]
    fn perfect_prediction_scores_one() {
        let labels = vec![v(0..=9), v(10..=19)];
        let s = score_clusters(&labels, &labels);
        assert_eq!(s.labelled_chars, 20);
        assert_eq!(s.agreeing_chars, 20);
        assert_eq!(s.exact_matches, 2);
        assert_eq!(s.predicted_clusters, 2);
        assert!((s.char_agreement() - 1.0).abs() < 1e-9);
        assert!((s.exact_match_rate() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn split_line_credits_only_larger_part() {
        // 标注一行 20 字；预测拆成 14 + 6。
        let labels = vec![v(0..=19)];
        let predicted = vec![v(0..=13), v(14..=19)];
        let s = score_clusters(&labels, &predicted);
        assert_eq!(s.agreeing_chars, 14);
        assert_eq!(s.exact_matches, 0);
        assert_eq!(s.predicted_clusters, 2);
    }

    #[test]
    fn merged_lines_credit_only_larger_line() {
        // 标注两行（12 字、8 字）；预测并成一行。
        let labels = vec![v(0..=11), v(12..=19)];
        let predicted = vec![v(0..=19)];
        let s = score_clusters(&labels, &predicted);
        assert_eq!(s.agreeing_chars, 12);
        assert_eq!(s.exact_matches, 0);
        assert_eq!(s.labelled_clusters, 2);
    }

    #[test]
    fn unlabelled_chars_are_ignored_and_unassigned_counted() {
        // 预测簇里多出未标注的字符（20–24）不影响完全匹配；标注的 30 没被预测到算 unassigned。
        let labels = vec![v(0..=9), vec![30]];
        let predicted = vec![[v(0..=9), v(20..=24)].concat()];
        let s = score_clusters(&labels, &predicted);
        assert_eq!(s.labelled_chars, 11);
        assert_eq!(s.agreeing_chars, 10);
        assert_eq!(s.unassigned_chars, 1);
        assert_eq!(s.exact_matches, 1);
        assert_eq!(s.labelled_clusters, 2);
    }

    #[test]
    fn exact_match_requires_identical_sets() {
        // 预测簇多包含一个标注过的字符（属于另一行）→ 不算完全匹配，但多数字符仍算对。
        let labels = vec![v(0..=9), v(10..=19)];
        let predicted = vec![v(0..=10), v(11..=19)];
        let s = score_clusters(&labels, &predicted);
        assert_eq!(s.exact_matches, 0);
        assert_eq!(s.agreeing_chars, 19);
    }

    #[test]
    fn cluster_label_ranges_round_trip() {
        let label = ClusterLabel::from_indices(&[5, 3, 4, 9, 10, 4]);
        assert_eq!(label.chars, vec![[3, 5], [9, 10]]);
        assert_eq!(label.indices(), vec![3, 4, 5, 9, 10]);
        let reversed = ClusterLabel {
            chars: vec![[5, 3]],
            bbox: None,
            note: None,
        };
        assert_eq!(reversed.indices(), vec![3, 4, 5], "区间端点写反也接受");
    }

    #[test]
    fn label_file_json_round_trip() {
        let file = LabelFile::new(
            Some("A-0001".into()),
            vec![PageLabels {
                page: 1,
                lines: vec![ClusterLabel::from_indices(&[0, 1, 2])],
                paragraphs: vec![ClusterLabel::from_indices(&[0, 1, 2])],
                note: None,
            }],
        );
        let json = serde_json::to_string_pretty(&file).unwrap();
        assert!(json.contains("\"format\": \"magpdf-text-labels/1\""));
        let back: LabelFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, file);
        // 省略 paragraphs/note 的最小写法也能读。
        let minimal =
            r#"{"format":"magpdf-text-labels/1","pages":[{"page":2,"lines":[{"chars":[[0,4]]}]}]}"#;
        let back: LabelFile = serde_json::from_str(minimal).unwrap();
        assert_eq!(
            back.page(2).unwrap().lines[0].indices(),
            vec![0, 1, 2, 3, 4]
        );
        assert!(back.page(2).unwrap().paragraphs.is_empty());
        assert!(back.page(1).is_none());
    }

    #[test]
    fn summary_accumulates() {
        let mut s = Summary::default();
        s.add_page(&PageScore {
            page: 1,
            lines: ClusterScore {
                labelled_chars: 10,
                agreeing_chars: 9,
                labelled_clusters: 2,
                exact_matches: 1,
                ..Default::default()
            },
            paragraphs: ClusterScore::default(),
        });
        s.add_page(&PageScore {
            page: 2,
            lines: ClusterScore {
                labelled_chars: 10,
                agreeing_chars: 10,
                labelled_clusters: 2,
                exact_matches: 2,
                ..Default::default()
            },
            paragraphs: ClusterScore::default(),
        });
        assert_eq!(s.pages, 2);
        assert!((s.lines.char_agreement() - 0.95).abs() < 1e-9);
        assert!((s.lines.exact_match_rate() - 0.75).abs() < 1e-9);
        assert!(
            (s.paragraphs.char_agreement() - 1.0).abs() < 1e-9,
            "没有标注时记 1"
        );
    }
}
