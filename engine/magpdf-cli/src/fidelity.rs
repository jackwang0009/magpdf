//! `fidelity` 子命令：逐页做保真校验（方案 2.3），报告每一页能不能修改原有对象。
//!
//! 校验的办法和编辑时用的是同一个（`magpdf_engine::edit::Session::check_fidelity`）：在文档的副本上把
//! 这一页的内容流重写一遍，比较前后的对象和画面。用它可以量出“换一个 PDFium 的构建之后，样本里有多少页
//! 能编辑”——不能编辑的页只能在上面叠加内容。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use magpdf_engine::edit::Session;
use pdfium_sys::Library;
use serde::Serialize;

use crate::roundtrip::collect_pdfs;

#[derive(Args, Debug)]
pub struct FidelityArgs {
    /// PDF 文件，或者目录（递归查找其中的 .pdf）
    pub input: PathBuf,
    /// 把逐页的结果写成 JSON
    #[arg(long)]
    pub json: Option<PathBuf>,
    /// 只打印汇总，不逐页列出
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Debug, Serialize)]
struct PageVerdict {
    /// 页序号，从 0 开始
    index: usize,
    editable: bool,
    /// 不能修改的原因
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct FileVerdict {
    path: String,
    /// 文件打不开，或者校验本身出了错
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    pages: Vec<PageVerdict>,
}

#[derive(Debug, Serialize)]
struct Report<'a> {
    pdfium_version: &'static str,
    files: &'a [FileVerdict],
    pages: usize,
    editable: usize,
    /// 不能修改的原因 → 页数
    reasons: &'a BTreeMap<String, usize>,
}

pub fn run(lib: &Library, args: &FidelityArgs) -> Result<()> {
    let (root, paths) = inputs(&args.input)?;
    anyhow::ensure!(
        !paths.is_empty(),
        "{} 下没有 PDF 文件",
        args.input.display()
    );

    let mut files = Vec::with_capacity(paths.len());
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let (mut pages, mut editable) = (0usize, 0usize);
    for path in &paths {
        let verdict = check_file(lib, &root, path);
        if !args.quiet {
            print_file(&verdict);
        }
        for page in &verdict.pages {
            pages += 1;
            match &page.reason {
                None => editable += 1,
                Some(reason) => *reasons.entry(reason.clone()).or_default() += 1,
            }
        }
        files.push(verdict);
    }

    let failed = files.iter().filter(|f| f.error.is_some()).count();
    println!(
        "PDFium {}：{} 个文件，{} 页；可以修改原有对象的 {} 页（{:.1}%），只能叠加内容的 {} 页",
        Library::linked_version(),
        files.len(),
        pages,
        editable,
        if pages == 0 {
            0.0
        } else {
            editable as f64 * 100.0 / pages as f64
        },
        pages - editable
    );
    for (reason, count) in &reasons {
        println!("  {count:>4} 页：{reason}");
    }
    if failed > 0 {
        println!("  另有 {failed} 个文件打不开或校验出错");
    }

    if let Some(out) = &args.json {
        let report = Report {
            pdfium_version: Library::linked_version(),
            files: &files,
            pages,
            editable,
            reasons: &reasons,
        };
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(out, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("写入 {}", out.display()))?;
        println!("JSON 结果：{}", out.display());
    }
    Ok(())
}

/// 输入是文件时只查它一个；是目录时递归找 PDF，按路径排序。返回（显示路径用的根目录，文件列表）。
fn inputs(input: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let meta = std::fs::metadata(input).with_context(|| format!("访问 {}", input.display()))?;
    if meta.is_file() {
        let root = input
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        return Ok((root, vec![input.to_path_buf()]));
    }
    let mut paths = Vec::new();
    collect_pdfs(input, &mut paths)?;
    paths.sort();
    Ok((input.to_path_buf(), paths))
}

fn check_file(lib: &Library, root: &Path, path: &Path) -> FileVerdict {
    let shown = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let mut verdict = FileVerdict {
        path: shown,
        error: None,
        pages: Vec::new(),
    };
    let mut session = match Session::open(lib, path, None) {
        Ok(session) => session,
        Err(e) => {
            verdict.error = Some(e.to_string());
            return verdict;
        }
    };
    for index in 0..session.page_count() {
        match session.check_fidelity(index) {
            Ok(reason) => verdict.pages.push(PageVerdict {
                index,
                editable: reason.is_none(),
                reason: reason.map(str::to_owned),
            }),
            Err(e) => {
                verdict.error = Some(format!("第 {} 页：{e}", index + 1));
                break;
            }
        }
    }
    verdict
}

fn print_file(file: &FileVerdict) {
    if let Some(error) = &file.error {
        println!("{}：出错：{error}", file.path);
    }
    for page in &file.pages {
        match &page.reason {
            None => println!("{} 第 {} 页：可以修改", file.path, page.index + 1),
            Some(reason) => println!(
                "{} 第 {} 页：只能叠加内容（{reason}）",
                file.path,
                page.index + 1
            ),
        }
    }
}
