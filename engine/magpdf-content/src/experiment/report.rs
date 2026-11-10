//! 汇总与表格（V02 的 `roundtrip::summarize` 的替代，多出 V17 的几项指标）。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::{FileResult, OutputStats, Stats};

/// 按来源（或全部）汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceStats {
    pub files: usize,
    /// 打开/读取失败的文件。
    pub failed: usize,
    pub unmatched_files: usize,
    pub bytes: u64,
    #[serde(flatten)]
    pub stats: Stats,
    pub pages_fallback: usize,
    pub pages_error: usize,
    pub pages_over_limit: usize,
    /// 不可对应页的原因计数。
    pub unmatched: BTreeMap<String, usize>,
    pub op_mismatches: usize,
    pub verify_failures: usize,
    pub deshared_streams: usize,
    pub incremental_bytes: u64,
    pub full_rewrite_bytes: u64,
    pub saved_bytes: u64,
    pub timings: super::Timings,
    /// 对照组（PDFium 原版生成器）的统计，没跑时为 `None`。
    pub control: Option<Stats>,
}

impl SourceStats {
    fn add(&mut self, file: &FileResult) {
        self.files += 1;
        self.bytes += file.bytes;
        for (k, v) in [
            (0, file.timings.read_us),
            (1, file.timings.map_us),
            (2, file.timings.save_us),
            (3, file.timings.verify_us),
            (4, file.timings.render_us),
            (5, file.timings.control_us),
        ] {
            let slot = match k {
                0 => &mut self.timings.read_us,
                1 => &mut self.timings.map_us,
                2 => &mut self.timings.save_us,
                3 => &mut self.timings.verify_us,
                4 => &mut self.timings.render_us,
                _ => &mut self.timings.control_us,
            };
            *slot += v;
        }
        if file.error.is_some() {
            self.failed += 1;
            return;
        }
        let Some(out) = &file.output else {
            self.failed += 1;
            return;
        };
        self.stats.add(&out.stats);
        self.saved_bytes += out.saved_bytes;
        self.incremental_bytes += out.incremental_bytes;
        self.full_rewrite_bytes += out.full_rewrite_bytes;
        self.pages_fallback += out.fallback_pages;
        self.op_mismatches += out.op_mismatches.len();
        self.verify_failures += out.verify_failures.len();
        self.deshared_streams += out.deshared_streams;
        for (reason, n) in &out.unmatched {
            *self.unmatched.entry(reason.clone()).or_default() += n;
        }
        for rec in &file.page_records {
            if *rec == super::PageOutcome::Error {
                self.pages_error += 1;
            }
        }
    }
}

fn merge_control(into: &mut Option<Stats>, from: &Option<Stats>) {
    if let Some(from) = from {
        into.get_or_insert_with(Stats::default).add(from);
    }
}

/// 按来源汇总。
pub fn summarize(files: &[FileResult]) -> BTreeMap<String, SourceStats> {
    let mut out: BTreeMap<String, SourceStats> = BTreeMap::new();
    for f in files {
        let s = out.entry(f.source.clone()).or_default();
        s.add(f);
        merge_control(&mut s.control, &f.control);
    }
    out
}

/// 合计（只作参考）。
pub fn total(files: &[FileResult]) -> SourceStats {
    let mut s = SourceStats::default();
    for f in files {
        s.add(f);
        merge_control(&mut s.control, &f.control);
    }
    s
}

fn pct(v: Option<f64>) -> String {
    v.map_or_else(|| "—".to_owned(), |r| format!("{:.2}%", r * 100.0))
}

fn ratio(a: usize, b: usize) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

/// 主表：实验 1（逐字节相同）、实验 3（不可对应）与实验 4（体积）。
pub fn render_table(by_source: &BTreeMap<String, SourceStats>, total: &SourceStats) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| 来源 | 文件 | 失败 | 内容流 | 逐字节相同 | 比例 | 对象 | 页 | 可对应 | 不可对应 | 不可对应比例 | 退回生成器 | 原文件字节 | 保存字节 | 增量部分 | 完整重写字节 |"
    );
    let _ = writeln!(
        out,
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for (name, s) in by_source {
        let _ = writeln!(out, "{}", row(name, s));
    }
    if by_source.len() > 1 {
        let _ = writeln!(out, "{}", row("（合计，仅供参考）", total));
    }
    out
}

fn row(name: &str, s: &SourceStats) -> String {
    let unmatched = s.stats.pages.saturating_sub(s.stats.pages_matched);
    format!(
        "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        s.files,
        s.failed,
        s.stats.streams,
        s.stats.streams_identical,
        pct(s.stats.identity_ratio()),
        s.stats.ops_total,
        s.stats.pages,
        s.stats.pages_matched,
        unmatched,
        pct(ratio(unmatched, s.stats.pages)),
        s.pages_fallback,
        s.bytes,
        s.saved_bytes,
        s.incremental_bytes,
        s.full_rewrite_bytes,
    )
}

/// 明细：实验 2（随机修改后的像素比对、与原版生成器对照）、实验 3 的原因、实验 4 的耗时。
pub fn render_details(by_source: &BTreeMap<String, SourceStats>, total: &SourceStats) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| 来源 | 比对页 | 无差异 | 修改后无差异比例 | 对照组页 | 对照组无差异 | 对照组比例 | 操作符 | 操作符不变比例 | 读入 ms | 对应 ms | 保存 ms | 校验 ms | 渲染 ms | 对照 ms |"
    );
    let _ = writeln!(
        out,
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for (name, s) in by_source {
        let _ = writeln!(out, "{}", detail_row(name, s));
    }
    if by_source.len() > 1 {
        let _ = writeln!(out, "{}", detail_row("（合计，仅供参考）", total));
    }
    if !total.unmatched.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "不可对应原因（页数）：");
        for (k, v) in &total.unmatched {
            let _ = writeln!(out, "- {k}：{v}");
        }
    }
    out
}

fn detail_row(name: &str, s: &SourceStats) -> String {
    let compared = s.stats.pages_matched;
    let control = s.control.as_ref();
    format!(
        "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
        compared,
        s.stats.pages_passed,
        pct(ratio(s.stats.pages_passed, compared)),
        control.map_or(0, |c| c.pages),
        control.map_or(0, |c| c.pages_passed),
        pct(control.and_then(|c| ratio(c.pages_passed, c.pages))),
        s.stats.ops_total,
        pct(s.stats.ops_unchanged_ratio()),
        s.timings.read_us as f64 / 1000.0,
        s.timings.map_us as f64 / 1000.0,
        s.timings.save_us as f64 / 1000.0,
        s.timings.verify_us as f64 / 1000.0,
        s.timings.render_us as f64 / 1000.0,
        s.timings.control_us as f64 / 1000.0,
    )
}

// 供 `mod.rs` 里 `FileResult` 的两个附带字段用。
#[allow(dead_code)]
fn _assert_output_shape(o: &OutputStats) -> usize {
    o.op_mismatches.len() + o.verify_failures.len()
}
