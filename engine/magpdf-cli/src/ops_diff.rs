//! `ops-diff` 子命令：两份 PDF 逐页做操作符级比对（颜色空间、图形状态参数、文字状态），
//! 和引擎保真校验用的是同一套（`magpdf_content::map::ops`）。
//!
//! 用来和每晚往返测试的 `tests/corpus/tools/ops_diff.py` 对账：往返测试保存的结果和原件一对一对地比，
//! 两边报差异的页应当一样。不需要 PDFium。

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use magpdf_content::PdfDocument;
use magpdf_content::map::map_page;
use magpdf_content::map::ops::compare_pages;
use magpdf_content::pdf::pages::walk_pages;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct OpsDiffArgs {
    /// 原件
    pub original: PathBuf,
    /// 往返之后保存的结果
    pub saved: PathBuf,
}

#[derive(Debug, Serialize)]
struct PageResult {
    /// 页码，从 1 开始
    page: usize,
    /// 有一边解释不下去、对象个数或种类对不上，或者这一页只有一边有
    comparable: bool,
    color: usize,
    gs: usize,
    text: usize,
}

pub fn run(args: &OpsDiffArgs) -> Result<()> {
    let load = |path: &PathBuf| -> Result<PdfDocument> {
        let bytes = std::fs::read(path).with_context(|| format!("读 {}", path.display()))?;
        PdfDocument::load(bytes).with_context(|| format!("打开 {}", path.display()))
    };
    let (a, b) = (load(&args.original)?, load(&args.saved)?);
    let (pages_a, pages_b) = (walk_pages(&a), walk_pages(&b));
    let results: Vec<PageResult> = (0..pages_a.len().max(pages_b.len()))
        .map(|i| {
            let diff = match (pages_a.get(i), pages_b.get(i)) {
                (Some(Some(pa)), Some(Some(pb))) => {
                    compare_pages(&map_page(&a, pa), &map_page(&b, pb))
                }
                _ => None,
            };
            let d = diff.unwrap_or_default();
            PageResult {
                page: i + 1,
                comparable: diff.is_some(),
                color: d.color,
                gs: d.gs,
                text: d.text,
            }
        })
        .collect();
    println!("{}", serde_json::to_string(&results)?);
    Ok(())
}
