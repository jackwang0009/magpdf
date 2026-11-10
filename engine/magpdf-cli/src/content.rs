//! `magpdf-cli content-roundtrip` 的实验框架（P0 计划 V17 的四项对比）。
//!
//! 每个文件的处理都在一个"工作线程"里建一个 [`pdfium_sys::Library`]（PDFium 不是线程安全的，
//! 一个线程一个库），这样默认就能并行；`--jobs 1` 与 V02 的 `--isolate` 等价，崩溃只影响当前文件。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args;
use magpdf_content::experiment::{self, Config};
use serde::Serialize;

#[derive(Args, Debug)]
pub struct ContentArgs {
    /// PDF 文件或目录（递归）
    pub input: PathBuf,
    /// 把全部文件归入这个来源类别；默认用输入目录下的一级子目录名
    #[arg(long)]
    pub source: Option<String>,
    /// 实验模式：roundtrip 只做"不改动直接保存"（实验 1、3、4）；modify 加"随机修改 5%"（实验 2）
    #[arg(long, value_enum, default_value_t = Mode::Roundtrip)]
    pub mode: Mode,
    /// 随机修改的比例（实验 2）
    #[arg(long, default_value_t = 0.05)]
    pub modify_ratio: f64,
    /// 随机种子（固定种子保证可复现）
    #[arg(long, default_value_t = 20261109)]
    pub seed: u64,
    /// 计数与行为与老库内置列过滤器一致的输入流（例如 CRC 流）但 Flate 流解析失败的文件，
    /// 改用 Flate 严格模式重读一次（与 PDFium 的宽松解析对齐；见报告第 5 节）
    #[arg(long)]
    pub lenient_flate: bool,
    /// PDFium 原版生成器对照组：每页全部对象标记为已修改、`FPDFPage_GenerateContent` 后保存
    #[arg(long)]
    pub control: bool,
    /// 预渲染/预期画面用 PDFium 内存对象完成（实验 2 的"预期画面"）
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub expect_from_memory: bool,
    /// 渲染倍率：2.0 即 144 dpi（V02 的判定分辨率）
    #[arg(long, default_value_t = 2.0)]
    pub scale: f32,
    /// 并行的工作线程数（每个线程一个 PDFium 库）
    #[arg(long, default_value_t = 1)]
    pub jobs: usize,
    /// 写出完整的机器可读结果
    #[arg(long)]
    pub json: Option<PathBuf>,
    /// 把保存结果写到这个目录（人工检查内容流、第二渲染器比对）
    #[arg(long)]
    pub save_dir: Option<PathBuf>,
    /// 为比对失败的页写三张 PNG（预期、保存结果、热图）
    #[arg(long)]
    pub diff_dir: Option<PathBuf>,
    /// 不在标准错误打印逐文件进度
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// 不改动任何对象直接保存
    Roundtrip,
    /// 随机修改 5% 的对象后保存
    Modify,
}

#[derive(Debug, Serialize)]
struct Report<'a> {
    tool: Tool,
    options: ReportOptions<'a>,
    elapsed_ms: u64,
    by_source: &'a std::collections::BTreeMap<String, experiment::SourceStats>,
    total: &'a experiment::SourceStats,
    files: &'a [experiment::FileResult],
}

#[derive(Debug, Serialize)]
struct Tool {
    name: &'static str,
    version: &'static str,
    pdfium: &'static str,
}

#[derive(Debug, Serialize)]
struct ReportOptions<'a> {
    input: String,
    mode: &'static str,
    modify_ratio: f64,
    seed: u64,
    scale: f32,
    control: bool,
    lenient_flate: bool,
    #[serde(flatten)]
    config: &'a Config,
}

struct Entry {
    path: PathBuf,
    rel: String,
    source: String,
}

/// 训练入口。
pub fn run(args: &ContentArgs) -> Result<()> {
    let started = Instant::now();
    let (root, entries) = enumerate(&args.input, args.source.as_deref())?;
    anyhow::ensure!(
        !entries.is_empty(),
        "{} 下没有 PDF 文件",
        args.input.display()
    );
    let ratio = match args.mode {
        Mode::Roundtrip => 0.0,
        Mode::Modify => args.modify_ratio.clamp(0.0, 1.0),
    };
    let base = Config {
        modify_ratio: ratio,
        seed: args.seed,
        scale: args.scale,
        control: args.control,
        lenient_flate: args.lenient_flate,
        ..Config::default()
    };
    if !args.quiet {
        eprintln!(
            "内容流往返：{} 个文件，PDFium {}，模式 {:?}，修改比例 {:.0}%，种子 {}{}",
            entries.len(),
            pdfium_sys::Library::linked_version(),
            args.mode,
            ratio * 100.0,
            args.seed,
            if args.control {
                "，含原版生成器对照"
            } else {
                ""
            }
        );
    }

    let results = run_parallel(
        &entries,
        &base,
        args.jobs.max(1),
        args.save_dir.as_deref(),
        args.diff_dir.as_deref(),
    );

    let by_source = experiment::summarize(&results);
    let total = experiment::total(&results);
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let mut stdout = std::io::stdout().lock();
    use std::io::Write as _;
    writeln!(
        stdout,
        "内容流往返（{:?}）：{} 个文件，耗时 {:.1} s（{}）",
        args.mode,
        total.files,
        elapsed_ms as f64 / 1000.0,
        root.display()
    )?;
    writeln!(stdout)?;
    write!(stdout, "{}", experiment::render_table(&by_source, &total))?;
    write!(
        stdout,
        "\n{}",
        experiment::render_details(&by_source, &total)
    )?;

    if let Some(json_path) = &args.json {
        let report = Report {
            tool: Tool {
                name: "magpdf-cli content-roundtrip",
                version: env!("CARGO_PKG_VERSION"),
                pdfium: pdfium_sys::Library::linked_version(),
            },
            options: ReportOptions {
                input: args.input.display().to_string(),
                mode: match args.mode {
                    Mode::Roundtrip => "roundtrip",
                    Mode::Modify => "modify",
                },
                modify_ratio: ratio,
                seed: args.seed,
                scale: args.scale,
                control: args.control,
                lenient_flate: args.lenient_flate,
                config: &base,
            },
            elapsed_ms,
            by_source: &by_source,
            total: &total,
            files: &results,
        };
        if let Some(parent) = json_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(json_path)
            .with_context(|| format!("创建 {}", json_path.display()))?;
        serde_json::to_writer_pretty(std::io::BufWriter::new(file), &report)?;
        writeln!(stdout)?;
        writeln!(stdout, "JSON 结果：{}", json_path.display())?;
    }
    Ok(())
}

fn run_parallel(
    entries: &[Entry],
    base: &Config,
    jobs: usize,
    save_dir: Option<&Path>,
    diff_dir: Option<&Path>,
) -> Vec<experiment::FileResult> {
    if jobs <= 1 {
        let lib = match pdfium_sys::Library::init() {
            Ok(lib) => lib,
            Err(e) => {
                return entries
                    .iter()
                    .map(|entry| {
                        experiment::FileResult::failed(
                            entry.rel.clone(),
                            &entry.source,
                            entry.path.to_string_lossy(),
                            format!("初始化 PDFium 失败：{e}"),
                        )
                    })
                    .collect();
            }
        };
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let started = Instant::now();
            let r =
                experiment::run_file(&lib, &e.path, &e.rel, &e.source, base, save_dir, diff_dir);
            if std::env::var_os("MAGPDF_PROGRESS").is_some() {
                eprintln!("{}  {} ms", r.path, started.elapsed().as_millis());
            }
            out.push(r);
        }
        return out;
    }
    let next = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let tx = tx.clone();
            let next = Arc::clone(&next);
            scope.spawn(move || {
                let lib = match pdfium_sys::Library::init() {
                    Ok(lib) => lib,
                    Err(_e) => {
                        return;
                    }
                };
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= entries.len() {
                        break;
                    }
                    let e = &entries[i];
                    let r = experiment::run_file(
                        &lib, &e.path, &e.rel, &e.source, base, save_dir, diff_dir,
                    );
                    let _ = tx.send((i, r));
                }
            });
        }
        drop(tx);
        let mut slots: Vec<Option<experiment::FileResult>> = vec![None; entries.len()];
        for (i, r) in rx {
            slots[i] = Some(r);
        }
        slots
    })
    .into_iter()
    .zip(entries)
    .map(|(slot, e)| {
        slot.unwrap_or_else(|| {
            experiment::FileResult::failed(
                e.rel.clone(),
                &e.source,
                e.path.to_string_lossy(),
                "工作线程没有返回结果".to_owned(),
            )
        })
    })
    .collect()
}

fn enumerate(input: &Path, source_override: Option<&str>) -> Result<(PathBuf, Vec<Entry>)> {
    let meta = std::fs::metadata(input).with_context(|| format!("访问 {}", input.display()))?;
    let (root, mut paths) = if meta.is_file() {
        let root = input
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        (root, vec![input.to_path_buf()])
    } else {
        let mut paths = Vec::new();
        collect_pdfs(input, &mut paths)?;
        (input.to_path_buf(), paths)
    };
    paths.sort();
    let root_name = root
        .file_name()
        .map_or_else(|| "root".to_owned(), |n| n.to_string_lossy().into_owned());
    let entries = paths
        .into_iter()
        .map(|path| {
            let rel = path.strip_prefix(&root).unwrap_or(&path);
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let source = source_override.map_or_else(
                || {
                    rel.parent()
                        .and_then(|p| p.components().next())
                        .map_or_else(
                            || root_name.clone(),
                            |c| c.as_os_str().to_string_lossy().into_owned(),
                        )
                },
                str::to_owned,
            );
            Entry {
                path,
                rel: rel_str,
                source,
            }
        })
        .collect();
    Ok((root, entries))
}

fn collect_pdfs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("读取目录 {}", dir.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            collect_pdfs(&path, out)?;
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        {
            out.push(path);
        }
    }
    Ok(())
}
