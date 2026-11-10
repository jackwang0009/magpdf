//! 实验层（V17 四项对比的文件级处理与汇总）。
//!
//! 依赖 `pdfium-sys` 的部分（渲染、原版生成器对照）都用 `pdfium_sys::` 全路径引用；
//! 纯计算部分（统计、随机修改计划、操作符级比对）不依赖 PDFium，可离线测试。
#![forbid(unsafe_code)]

mod report;

pub use report::{SourceStats, render_details, render_table, summarize, total};

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 一份文件的实验参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// 随机修改的比例（0 = 不改动）。
    pub modify_ratio: f64,
    /// 随机种子。
    pub seed: u64,
    /// 渲染倍率。
    pub scale: f32,
    /// 是否跑原版生成器对照组。
    pub control: bool,
    /// 严格 Flate 失败时是否再用宽松模式重读。
    pub lenient_flate: bool,
    /// 渲染像素上限（超过的页不比对，计入"页面过大"）。
    pub max_pixels: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            modify_ratio: 0.0,
            seed: 20261109,
            scale: 2.0,
            control: false,
            lenient_flate: false,
            max_pixels: 40_000_000,
        }
    }
}

/// 一项统计的累计值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub streams: usize,
    pub streams_identical: usize,
    pub bytes_original: usize,
    pub bytes_saved: usize,
    pub ops_total: usize,
    pub ops_changed: usize,
    pub pages: usize,
    pub pages_matched: usize,
    pub pages_passed: usize,
}

impl Stats {
    pub fn add(&mut self, other: &Self) {
        self.streams += other.streams;
        self.streams_identical += other.streams_identical;
        self.bytes_original += other.bytes_original;
        self.bytes_saved += other.bytes_saved;
        self.ops_total += other.ops_total;
        self.ops_changed += other.ops_changed;
        self.pages += other.pages;
        self.pages_matched += other.pages_matched;
        self.pages_passed += other.pages_passed;
    }

    pub fn identity_ratio(&self) -> Option<f64> {
        (self.streams > 0).then(|| self.streams_identical as f64 / self.streams as f64)
    }

    pub fn ops_unchanged_ratio(&self) -> Option<f64> {
        (self.ops_total > 0).then(|| {
            (self.ops_total.saturating_sub(self.ops_changed)) as f64 / self.ops_total as f64
        })
    }
}

/// 一个文件的结果。`error` 非空时该文件不计入任何比例（单列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileResult {
    pub path: String,
    pub source: String,
    pub bytes: u64,
    pub pages: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub timings: Timings,
    #[serde(default)]
    pub output: Option<OutputStats>,
    #[serde(default)]
    pub page_records: Vec<PageOutcome>,
    /// Control-group stats from the stock PDFium generator. Absent when not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Stats>,
}

impl FileResult {
    /// Placeholder used by the CLI when PDFium init or a worker fails.
    pub fn failed(
        path: impl Into<String>,
        source: impl AsRef<str>,
        path_display: impl std::fmt::Display,
        error: impl std::fmt::Display,
    ) -> Self {
        let _ = path_display;
        Self {
            path: path.into(),
            source: source.as_ref().to_owned(),
            bytes: 0,
            pages: 0,
            error: Some(error.to_string()),
            timings: Timings::default(),
            output: None,
            page_records: Vec::new(),
            control: None,
        }
    }
}

/// 各阶段耗时（微秒，避免小文件全为 0）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timings {
    pub read_us: u64,
    pub map_us: u64,
    pub save_us: u64,
    pub verify_us: u64,
    pub render_us: u64,
    pub control_us: u64,
}

/// 保存结果的统计（实验 1、3、4）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputStats {
    pub saved_bytes: u64,
    pub incremental_bytes: u64,
    pub full_rewrite_bytes: u64,
    #[serde(flatten)]
    pub stats: Stats,
    /// 不可对应页的原因计数（原因标签 → 页数）。
    pub unmatched: std::collections::BTreeMap<String, usize>,
    /// 操作符级比对的失败明细（每页一条）。
    pub op_mismatches: Vec<String>,
    /// 校验读回时内容流与预期不一致的页。
    pub verify_failures: Vec<String>,
    /// 退回 PDFium 生成器整页重写的页数。
    pub fallback_pages: usize,
    /// 去共享而复制的流数。
    pub deshared_streams: usize,
    /// 修改后文件与原文件的字节差（含增量更新开销）。
    pub bytes_delta: i64,
}

/// 一页的判定（实验 2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageOutcome {
    Pass,
    /// 像素比对失败（预期画面与保存结果不一致）。
    Diff,
    /// 不可对应，退回生成器（仍计入比对）。
    Fallback,
    /// 原件这一页就打不开或渲染不了。
    Error,
}

/// 单个文件的处理入口。实现见 `run.rs`（需要 PDFium 的部分）。
pub fn run_file(
    lib: &pdfium_sys::Library,
    path: &Path,
    rel: &str,
    source: &str,
    config: &Config,
    save_dir: Option<&Path>,
    diff_dir: Option<&Path>,
) -> FileResult {
    run::run_file(lib, path, rel, source, config, save_dir, diff_dir)
}

mod run;
