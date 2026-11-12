//! `magpdf-cli content-roundtrip`：V17 自研内容流读写层的实验（P0 计划 V17 的四项对比）。
//!
//! 文件遍历、子进程隔离、报告写出在这里；单个文件的处理在 `run.rs`，汇总在 `report.rs`。
//!
//! PDFium 一个进程只能有一个 `Library`，也不是线程安全的：不隔离时全部文件在本进程里顺序处理；
//! `--isolate` 时每个文件起一个子进程（同时跑 `--jobs` 个），PDFium 崩溃或卡死只影响当前文件。

mod isolate;
pub mod model;
mod report;
mod run;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::Serialize;

use self::isolate::{ChildOutcome, run_child};
use self::model::{CheckStatus, Config, EditMix, ExpectedMode, FileResult, FileStatus, Route};
use self::report::SourceStats;

#[derive(Args, Debug)]
pub struct ContentArgs {
    /// PDF 文件或目录（递归）
    pub input: PathBuf,
    /// 把全部文件归入这个来源类别；默认用输入目录下的一级子目录名
    #[arg(long)]
    pub source: Option<String>,
    /// 实验模式：roundtrip 不改动直接保存（对不上的页照样退回生成器重写并校验）；modify 随机修改一部分对象
    #[arg(long, value_enum, default_value_t = Mode::Roundtrip)]
    pub mode: Mode,
    /// 随机修改的比例
    #[arg(long, default_value_t = 0.05)]
    pub modify_ratio: f64,
    /// 随机种子（固定种子保证可复现）
    #[arg(long, default_value_t = 20261109)]
    pub seed: u64,
    /// 每个被抽中的对象做哪种修改：cycle 按抽中的先后轮流取移动、改色、删除（首轮的做法）；random 随机取
    #[arg(long, value_enum, default_value_t = EditMix::Cycle)]
    pub edit_mix: EditMix,
    /// 预期画面：direct 在 PDFium 内存里改完直接渲染；regenerated 是首轮的做法（改完让生成器重写再渲染）
    #[arg(long, value_enum, default_value_t = ExpectedMode::Direct)]
    pub expected: ExpectedMode,
    /// 不改动时也把对得上的页经改写层写一遍，再从保存结果读回核对
    #[arg(long)]
    pub rewrite_all: bool,
    /// 对照组：同一组修改，每页全部对象标记为已修改、交给 PDFium 的生成器重写
    #[arg(long)]
    pub control: bool,
    /// 渲染倍率：2.0 即 144 dpi（V02 的判定分辨率）
    #[arg(long, default_value_t = 2.0)]
    pub scale: f32,
    /// 每个文件在子进程里处理：PDFium 崩溃或超时记为 crash/timeout，不中断整批
    #[arg(long)]
    pub isolate: bool,
    /// 隔离模式下单个文件的超时（秒）
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,
    /// 隔离模式下同时运行的子进程数
    #[arg(long, default_value_t = 1)]
    pub jobs: usize,
    /// 写出完整的机器可读结果
    #[arg(long)]
    pub json: Option<PathBuf>,
    /// 写进 JSON 的说明（比如这次配的是哪一份 pdfium.dll）
    #[arg(long)]
    pub label: Option<String>,
    /// 把保存结果写到这个目录
    #[arg(long)]
    pub save_dir: Option<PathBuf>,
    /// 为比对不过的页写三张 PNG（预期、保存结果、热图）
    #[arg(long)]
    pub diff_dir: Option<PathBuf>,
    /// 不在标准错误打印逐文件进度
    #[arg(long)]
    pub quiet: bool,
    /// `--isolate` 的子进程入口：只处理 `input` 这一个文件，把结果的 JSON 写到标准输出
    #[arg(long, hide = true)]
    pub worker: bool,
    /// 子进程用：文件在输入目录里的相对路径
    #[arg(long, hide = true)]
    pub rel: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// 不改动任何对象直接保存
    Roundtrip,
    /// 随机修改一部分对象后保存
    Modify,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Roundtrip => "roundtrip",
            Self::Modify => "modify",
        }
    }
}

#[derive(Debug, Serialize)]
struct Report<'a> {
    tool: Tool,
    options: ReportOptions<'a>,
    started_unix_s: u64,
    elapsed_ms: u64,
    by_source: &'a std::collections::BTreeMap<String, SourceStats>,
    total: &'a SourceStats,
    files: &'a [FileResult],
}

#[derive(Debug, Serialize)]
struct Tool {
    name: &'static str,
    version: &'static str,
    /// 编译时链接的导入库版本。运行时实际装载的是可执行文件旁边的 `pdfium.dll`，见 `pdfium_dll`。
    pdfium: &'static str,
    pdfium_dll: Option<DllInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

/// 可执行文件旁边那份 `pdfium.dll`（Windows 先从这里装载）。同一个可执行文件换一份 DLL 重跑时，
/// 靠它分辨结果是配哪一份跑出来的。
#[derive(Debug, Serialize)]
struct DllInfo {
    bytes: u64,
    modified_unix_s: u64,
}

#[derive(Debug, Serialize)]
struct ReportOptions<'a> {
    input: String,
    mode: &'static str,
    isolate: bool,
    timeout_s: u64,
    jobs: usize,
    #[serde(flatten)]
    config: &'a Config,
}

struct Entry {
    path: PathBuf,
    rel: String,
    source: String,
}

fn config_from(args: &ContentArgs) -> Config {
    Config {
        modify_ratio: match args.mode {
            Mode::Roundtrip => 0.0,
            Mode::Modify => args.modify_ratio.clamp(0.0, 1.0),
        },
        seed: args.seed,
        scale: args.scale,
        control: args.control,
        expected: args.expected,
        edit_mix: args.edit_mix,
        rewrite_all: args.rewrite_all,
        ..Config::default()
    }
}

/// 子命令入口。
pub fn run(args: &ContentArgs) -> Result<()> {
    if args.worker {
        return run_worker(args);
    }
    let started = Instant::now();
    let started_unix_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (root, entries) = enumerate(&args.input, args.source.as_deref())?;
    anyhow::ensure!(
        !entries.is_empty(),
        "{} 下没有 PDF 文件",
        args.input.display()
    );
    let config = config_from(args);
    if !args.quiet {
        eprintln!(
            "内容流往返：{} 个文件，PDFium {}，模式 {}{}{}{}",
            entries.len(),
            pdfium_sys::Library::linked_version(),
            args.mode.name(),
            if config.modify_ratio > 0.0 {
                format!(
                    "，修改比例 {:.0}%，种子 {}",
                    config.modify_ratio * 100.0,
                    config.seed
                )
            } else {
                String::new()
            },
            if args.control {
                "，含生成器对照组"
            } else {
                ""
            },
            if args.isolate {
                format!(
                    "，子进程隔离，超时 {} s，并行 {}",
                    args.timeout,
                    args.jobs.max(1)
                )
            } else {
                String::new()
            }
        );
    }

    let results = if args.isolate {
        run_isolated(&entries, args)?
    } else {
        run_in_process(&entries, &config, args)?
    };

    let by_source = report::summarize(&results);
    let total = report::total(&results);
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let mut stdout = std::io::stdout().lock();
    use std::io::Write as _;
    writeln!(
        stdout,
        "内容流往返（{}）：{} 个文件，{} 页，耗时 {:.1} s（{}）",
        args.mode.name(),
        total.files,
        total.pages,
        elapsed_ms as f64 / 1000.0,
        root.display()
    )?;
    writeln!(stdout)?;
    write!(stdout, "{}", report::render_table(&by_source, &total))?;
    write!(stdout, "\n{}", report::render_details(&by_source, &total))?;

    if let Some(json_path) = &args.json {
        let report = Report {
            tool: Tool {
                name: "magpdf-cli content-roundtrip",
                version: env!("CARGO_PKG_VERSION"),
                pdfium: pdfium_sys::Library::linked_version(),
                pdfium_dll: loaded_dll_info(),
                label: args.label.clone(),
            },
            options: ReportOptions {
                input: args.input.display().to_string(),
                mode: args.mode.name(),
                isolate: args.isolate,
                timeout_s: args.timeout,
                jobs: args.jobs.max(1),
                config: &config,
            },
            started_unix_s,
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

fn loaded_dll_info() -> Option<DllInfo> {
    let exe = std::env::current_exe().ok()?;
    let meta = std::fs::metadata(exe.parent()?.join("pdfium.dll")).ok()?;
    let modified_unix_s = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(DllInfo {
        bytes: meta.len(),
        modified_unix_s,
    })
}

/// 子进程：处理一个文件，把 `FileResult` 的 JSON 写到标准输出。
fn run_worker(args: &ContentArgs) -> Result<()> {
    let lib = pdfium_sys::Library::init()?;
    let rel = args
        .rel
        .clone()
        .unwrap_or_else(|| args.input.display().to_string());
    let source = args.source.clone().unwrap_or_default();
    let result = run::run_file(
        &lib,
        &args.input,
        &rel,
        &source,
        &config_from(args),
        args.save_dir.as_deref(),
        args.diff_dir.as_deref(),
    );
    serde_json::to_writer(std::io::stdout().lock(), &result)?;
    Ok(())
}

fn run_in_process(
    entries: &[Entry],
    config: &Config,
    args: &ContentArgs,
) -> Result<Vec<FileResult>> {
    let lib = pdfium_sys::Library::init()?;
    let mut out = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let started = Instant::now();
        let r = run::run_file(
            &lib,
            &e.path,
            &e.rel,
            &e.source,
            config,
            args.save_dir.as_deref(),
            args.diff_dir.as_deref(),
        );
        if !args.quiet {
            eprintln!(
                "{}",
                progress_line(i + 1, entries.len(), &r, started.elapsed())
            );
        }
        out.push(r);
    }
    Ok(out)
}

fn run_isolated(entries: &[Entry], args: &ContentArgs) -> Result<Vec<FileResult>> {
    let exe = std::env::current_exe().context("取当前可执行文件路径")?;
    let timeout = Duration::from_secs(args.timeout.max(1));
    let jobs = args.jobs.clamp(1, entries.len().max(1));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<FileResult>>> = Mutex::new(vec![None; entries.len()]);

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= entries.len() {
                        break;
                    }
                    let started = Instant::now();
                    let result = run_one_isolated(&exe, &entries[i], args, timeout);
                    let finished = done.fetch_add(1, Ordering::SeqCst) + 1;
                    if !args.quiet {
                        eprintln!(
                            "{}",
                            progress_line(finished, entries.len(), &result, started.elapsed())
                        );
                    }
                    if let Ok(mut slots) = slots.lock() {
                        slots[i] = Some(result);
                    }
                }
            });
        }
    });

    let slots = slots.into_inner().unwrap_or_else(|e| e.into_inner());
    Ok(slots
        .into_iter()
        .zip(entries)
        .map(|(slot, e)| {
            slot.unwrap_or_else(|| {
                FileResult::failed(
                    e.rel.clone(),
                    e.source.clone(),
                    FileStatus::Crashed,
                    "子进程没有返回结果",
                )
            })
        })
        .collect())
}

fn run_one_isolated(
    exe: &Path,
    entry: &Entry,
    args: &ContentArgs,
    timeout: Duration,
) -> FileResult {
    let mut cmd = Command::new(exe);
    cmd.arg("content-roundtrip")
        .arg(&entry.path)
        .arg("--worker")
        .arg("--rel")
        .arg(&entry.rel)
        .arg("--source")
        .arg(&entry.source)
        .arg("--mode")
        .arg(args.mode.name())
        .arg("--modify-ratio")
        .arg(args.modify_ratio.to_string())
        .arg("--seed")
        .arg(args.seed.to_string())
        .arg("--scale")
        .arg(args.scale.to_string())
        .arg("--edit-mix")
        .arg(value_name(&args.edit_mix))
        .arg("--expected")
        .arg(value_name(&args.expected));
    if args.rewrite_all {
        cmd.arg("--rewrite-all");
    }
    if args.control {
        cmd.arg("--control");
    }
    if let Some(dir) = &args.save_dir {
        cmd.arg("--save-dir").arg(dir);
    }
    if let Some(dir) = &args.diff_dir {
        cmd.arg("--diff-dir").arg(dir);
    }
    let failed = |status: FileStatus, error: String| {
        let mut r = FileResult::failed(entry.rel.clone(), entry.source.clone(), status, error);
        r.bytes = std::fs::metadata(&entry.path).map_or(0, |m| m.len());
        r
    };
    match run_child(&mut cmd, timeout) {
        ChildOutcome::Finished(stdout) => match serde_json::from_slice::<FileResult>(&stdout) {
            Ok(result) => result,
            Err(e) => failed(FileStatus::Crashed, format!("子进程输出无法解析：{e}")),
        },
        ChildOutcome::Crashed { code, stderr } => failed(
            FileStatus::Crashed,
            format!(
                "子进程异常退出（{}）{}",
                code.map_or_else(|| "被终止".to_owned(), |c| format!("0x{:08X}", c as u32)),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!("；stderr：{stderr}")
                }
            ),
        ),
        ChildOutcome::TimedOut => failed(
            FileStatus::TimedOut,
            format!("超过 {} s 未完成，已终止", timeout.as_secs()),
        ),
        ChildOutcome::SpawnFailed(e) => failed(FileStatus::Crashed, format!("启动子进程失败：{e}")),
    }
}

/// 枚举值在命令行上的写法。
fn value_name<T: ValueEnum>(value: &T) -> String {
    value
        .to_possible_value()
        .map_or_else(String::new, |v| v.get_name().to_owned())
}

fn progress_line(done: usize, total: usize, r: &FileResult, elapsed: Duration) -> String {
    let state = match r.status {
        FileStatus::Ok => {
            let mut matched = 0;
            let mut fallback = 0;
            let (mut pass, mut diff) = (0, 0);
            for page in &r.page_records {
                if page.error.is_some() {
                    continue;
                }
                match page.route {
                    Route::Matched => matched += 1,
                    Route::Fallback => fallback += 1,
                }
                match page.verify.as_ref().map(|c| c.status) {
                    Some(CheckStatus::Pass) => pass += 1,
                    Some(CheckStatus::Diff) => diff += 1,
                    _ => {}
                }
            }
            format!(
                "{} 页：对得上 {matched}，回退 {fallback}；无差异 {pass}，有差异 {diff}",
                r.pages
            )
        }
        FileStatus::LoadFailed => "打不开".to_owned(),
        FileStatus::Crashed => "崩溃".to_owned(),
        FileStatus::TimedOut => "超时".to_owned(),
    };
    let error = r
        .error
        .as_deref()
        .map_or(String::new(), |e| format!("  [{e}]"));
    format!(
        "[{done}/{total}] {}  {state}  {} ms{error}",
        r.path,
        elapsed.as_millis()
    )
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser, Debug)]
    struct Cli {
        #[command(flatten)]
        args: ContentArgs,
    }

    #[test]
    fn roundtrip_mode_ignores_the_modify_ratio() {
        let cli = Cli::parse_from(["x", "in", "--modify-ratio", "0.3"]);
        assert_eq!(config_from(&cli.args).modify_ratio, 0.0);
        let cli = Cli::parse_from(["x", "in", "--mode", "modify", "--modify-ratio", "0.3"]);
        assert_eq!(config_from(&cli.args).modify_ratio, 0.3);
        let cli = Cli::parse_from(["x", "in", "--mode", "modify", "--modify-ratio", "7"]);
        assert_eq!(config_from(&cli.args).modify_ratio, 1.0);
    }

    #[test]
    fn enum_values_round_trip_through_the_command_line() {
        // 父进程把这些值原样传给子进程，名字必须是 clap 认得的写法。
        for mix in [EditMix::Cycle, EditMix::Random] {
            for expected in [ExpectedMode::Direct, ExpectedMode::Regenerated] {
                let cli = Cli::parse_from([
                    "x".to_owned(),
                    "in".to_owned(),
                    "--edit-mix".to_owned(),
                    value_name(&mix),
                    "--expected".to_owned(),
                    value_name(&expected),
                ]);
                assert_eq!(cli.args.edit_mix, mix);
                assert_eq!(cli.args.expected, expected);
            }
        }
    }

    #[test]
    fn progress_line_counts_routes_and_checks() {
        let mut ok = FileResult::failed("a.pdf", "s", FileStatus::Ok, "");
        ok.error = None;
        ok.pages = 2;
        let mut matched = model::PageRecord::new(0);
        matched.route = Route::Matched;
        matched.verify = Some(model::Check {
            status: CheckStatus::Pass,
            ..model::Check::error("")
        });
        let mut fallback = model::PageRecord::new(1);
        fallback.verify = Some(model::Check::diff("x"));
        ok.page_records = vec![matched, fallback];
        let line = progress_line(3, 9, &ok, Duration::from_millis(12));
        assert!(line.starts_with("[3/9] a.pdf"), "{line}");
        assert!(
            line.contains("对得上 1，回退 1；无差异 1，有差异 1"),
            "{line}"
        );
        let crashed = FileResult::failed("b.pdf", "s", FileStatus::Crashed, "子进程异常退出");
        let line = progress_line(1, 1, &crashed, Duration::ZERO);
        assert!(
            line.contains("崩溃") && line.contains("[子进程异常退出]"),
            "{line}"
        );
    }
}
