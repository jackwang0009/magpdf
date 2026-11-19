//! `fidelity` 子命令：逐页做保真校验（方案 2.3），报告每一页能不能修改原有对象。
//!
//! 校验的办法和编辑时用的是同一个（`magpdf_engine::edit::Session::check_fidelity`）：在文档的副本上把
//! 这一页的内容流重写一遍，比较前后的对象和画面。用它可以量出“换一个 PDFium 的构建之后，样本里有多少页
//! 能编辑”——不能编辑的页只能在上面叠加内容。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use magpdf_engine::edit::Session;
use pdfium_sys::Library;
use serde::{Deserialize, Serialize};

use crate::content::isolate::{ChildOutcome, run_child};
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
    /// 每个文件在子进程里校验：PDFium 崩溃或卡死只记在这一个文件上，不中断整批（每晚的测试用）
    #[arg(long)]
    pub isolate: bool,
    /// 隔离模式下同时跑几个子进程
    #[arg(long, default_value_t = 1)]
    pub jobs: usize,
    /// 隔离模式下单个文件最多给几秒
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,
    /// 子进程入口：只校验 `input` 这一个文件，结果写成一行 JSON
    #[arg(long, hide = true)]
    pub worker: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct PageVerdict {
    /// 页序号，从 0 开始
    index: usize,
    editable: bool,
    /// 不能修改的原因
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileVerdict {
    path: String,
    /// `ok`；`error`（打不开，或者校验本身出了错）；隔离模式下还有 `crashed`、`timed_out`
    status: String,
    /// 出错的说明。不带子进程的标准错误（可能有文档内容）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    pages: Vec<PageVerdict>,
}

#[derive(Debug, Serialize)]
struct Report<'a> {
    kind: &'static str,
    format: u32,
    pdfium_version: &'static str,
    files: &'a [FileVerdict],
    pages: usize,
    editable: usize,
    /// 不能修改的原因 → 页数
    reasons: &'a BTreeMap<String, usize>,
}

pub fn run(lib: &Library, args: &FidelityArgs) -> Result<()> {
    if args.worker {
        let verdict = check_file(lib, Path::new(""), &args.input);
        println!("{}", serde_json::to_string(&verdict)?);
        return Ok(());
    }
    let (root, paths) = inputs(&args.input)?;
    anyhow::ensure!(
        !paths.is_empty(),
        "{} 下没有 PDF 文件",
        args.input.display()
    );

    let verdicts = if args.isolate {
        check_isolated(&root, &paths, args)?
    } else {
        paths
            .iter()
            .map(|path| check_file(lib, &root, path))
            .collect()
    };
    let mut files = Vec::with_capacity(paths.len());
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let (mut pages, mut editable) = (0usize, 0usize);
    for verdict in verdicts {
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

    let failed = files.iter().filter(|f| f.status != "ok").count();
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
        println!("  另有 {failed} 个文件打不开、校验出错、崩溃或超时");
    }

    if let Some(out) = &args.json {
        let report = Report {
            kind: "magpdf-fidelity",
            format: 1,
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

/// 显示和 JSON 里用的相对路径，分隔符一律是 `/`。
fn shown_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn check_file(lib: &Library, root: &Path, path: &Path) -> FileVerdict {
    let mut verdict = FileVerdict {
        path: shown_path(root, path),
        status: "ok".to_owned(),
        error: None,
        pages: Vec::new(),
    };
    let mut session = match Session::open(lib, path, None) {
        Ok(session) => session,
        Err(e) => {
            verdict.status = "error".to_owned();
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
                verdict.status = "error".to_owned();
                verdict.error = Some(format!("第 {} 页：{e}", index + 1));
                break;
            }
        }
    }
    verdict
}

/// 每个文件起一个子进程（`fidelity <文件> --worker`），`jobs` 个同时跑，结果按输入的顺序排好。
fn check_isolated(root: &Path, paths: &[PathBuf], args: &FidelityArgs) -> Result<Vec<FileVerdict>> {
    let exe = std::env::current_exe().context("取当前可执行文件路径")?;
    let timeout = Duration::from_secs(args.timeout.max(1));
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<FileVerdict>>> =
        Mutex::new((0..paths.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..args.jobs.clamp(1, paths.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(path) = paths.get(i) else {
                        break;
                    };
                    let verdict = check_one_isolated(&exe, root, path, timeout);
                    if let Ok(mut results) = results.lock() {
                        results[i] = Some(verdict);
                    }
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    Ok(results
        .into_iter()
        .zip(paths)
        .map(|(verdict, path)| {
            verdict
                .unwrap_or_else(|| failed(root, path, "crashed", "子进程没有返回结果".to_owned()))
        })
        .collect())
}

fn failed(root: &Path, path: &Path, status: &str, error: String) -> FileVerdict {
    FileVerdict {
        path: shown_path(root, path),
        status: status.to_owned(),
        error: Some(error),
        pages: Vec::new(),
    }
}

fn check_one_isolated(exe: &Path, root: &Path, path: &Path, timeout: Duration) -> FileVerdict {
    let mut cmd = Command::new(exe);
    cmd.arg("fidelity").arg(path).arg("--worker");
    match run_child(&mut cmd, timeout) {
        ChildOutcome::Finished { stdout, .. } => {
            let line = String::from_utf8_lossy(&stdout);
            match serde_json::from_str::<FileVerdict>(line.trim()) {
                Ok(mut verdict) => {
                    verdict.path = shown_path(root, path);
                    verdict
                }
                Err(e) => failed(root, path, "crashed", format!("子进程输出无法解析：{e}")),
            }
        }
        ChildOutcome::Crashed { code, .. } => failed(
            root,
            path,
            "crashed",
            format!(
                "子进程异常退出（{}）",
                code.map_or_else(|| "被终止".to_owned(), |c| format!("0x{:08X}", c as u32))
            ),
        ),
        ChildOutcome::TimedOut { .. } => failed(
            root,
            path,
            "timed_out",
            format!("超过 {} s 未完成，已终止", timeout.as_secs()),
        ),
        ChildOutcome::SpawnFailed(e) => {
            failed(root, path, "crashed", format!("启动子进程失败：{e}"))
        }
    }
}

fn print_file(file: &FileVerdict) {
    if let Some(error) = &file.error {
        println!("{}：{}：{error}", file.path, file.status);
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
