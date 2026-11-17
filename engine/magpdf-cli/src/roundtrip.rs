//! `magpdf-cli roundtrip`：往返测试（方案 13.2 第 1 项，P0 计划 V02、V14）。
//!
//! 文件遍历、来源分类、子进程隔离、差异图输出和报告写出在这里；单个文档的处理与汇总在
//! `magpdf_engine::roundtrip`。

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Args;
use magpdf_engine::roundtrip::{
    self, DiffImages, FileId, FileResult, FileStatus, Options, PageRecord, PageStatus,
    SourceSummary,
};
use pdfium_sys::{Bitmap, Library};
use serde::{Deserialize, Serialize};

use crate::image::{heatmap, write_png};

/// `--json` 结果的种类标记，和下面的格式版本一起写在最前面。
const REPORT_KIND: &str = "magpdf-roundtrip";

/// `--json` 结果的格式版本。字段的含义变了就加一，读它的工具（`tests/corpus/tools/roundtrip_model.py`）
/// 见到不认识的版本会拒收，免得字段改了却没人发现（V14 每晚往返报告第 10 节）。
///
/// - 1：最初的格式，没有 `kind`、`format` 这两个字段。崩溃文件的 `error` 后面接着“；stderr：…”。
/// - 2：加了 `kind`、`format`；`tool.pdfium_library` 是实际加载的 PDFium 库文件；崩溃、超时的文件只要
///   子进程打开了文档，`page_count` 就有数；子进程标准错误的末尾从 `error` 里分出来，放在 `stderr`。
const REPORT_FORMAT: u32 = 2;

#[derive(Args, Debug)]
pub struct RoundtripArgs {
    /// PDF 文件，或递归遍历的目录
    pub input: PathBuf,
    /// 写出完整的机器可读结果（每文件每页一条记录，加按来源汇总）
    #[arg(long)]
    pub json: Option<PathBuf>,
    /// 为未通过的页面写出原件渲染、保存结果渲染和差异热图三张 PNG
    #[arg(long)]
    pub diff_dir: Option<PathBuf>,
    /// 把重新生成后保存的 PDF 写到这个目录（人工检查内容流、第二渲染器比对）
    #[arg(long)]
    pub save_dir: Option<PathBuf>,
    /// 校准模式：不标记对象、不重新生成内容流，只保存再重开渲染，测量噪声底线（应全部无差异）
    #[arg(long)]
    pub calibrate: bool,
    /// 每个文件在子进程里处理：PDFium 崩溃或超时记为 crash/timeout，不中断整批（CI 必用）
    #[arg(long)]
    pub isolate: bool,
    /// 隔离模式下单个文件的超时（秒）
    #[arg(long, default_value_t = 60)]
    pub timeout: u64,
    /// 隔离模式下同时运行的子进程数
    #[arg(long, default_value_t = 1)]
    pub jobs: usize,
    /// 渲染倍率：2.0 即 144 dpi
    #[arg(long, default_value_t = 2.0)]
    pub scale: f32,
    /// 把全部文件归入这个来源类别；默认用输入目录下的一级子目录名（tests/corpus/README.md 的目录表）
    #[arg(long)]
    pub source: Option<String>,
    /// 不在标准错误打印逐文件进度
    #[arg(long)]
    pub quiet: bool,
}

/// 隐藏子命令 `roundtrip-worker`：在子进程里处理单个文件，把 `FileResult` 的 JSON 写到标准输出。
#[derive(Args, Debug)]
pub struct WorkerArgs {
    pub file: PathBuf,
    #[arg(long)]
    pub rel: String,
    #[arg(long)]
    pub source: String,
    #[arg(long)]
    pub calibrate: bool,
    #[arg(long, default_value_t = 2.0)]
    pub scale: f32,
    #[arg(long)]
    pub diff_dir: Option<PathBuf>,
    #[arg(long)]
    pub save_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct Entry {
    path: PathBuf,
    id: FileId,
}

#[derive(Debug, Serialize)]
struct Report<'a> {
    kind: &'static str,
    format: u32,
    tool: Tool,
    options: ReportOptions<'a>,
    started_unix_s: u64,
    elapsed_ms: u64,
    by_source: &'a BTreeMap<String, SourceSummary>,
    total: &'a SourceSummary,
    files: &'a [FileResult],
}

#[derive(Debug, Serialize)]
struct Tool {
    name: &'static str,
    version: &'static str,
    /// 编译时链接的那一份预编译库的版本号。修补过的构建和原版是同一个号。
    pdfium: &'static str,
    /// 实际加载的 PDFium 库文件的路径。是哪一份 PDFium，由读结果的工具拿这个文件算哈希来认
    /// （这里不算：命令行工具不为这一件事多带一个哈希库）。取不到时不写这个字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pdfium_library: Option<String>,
}

/// 子进程（`roundtrip-worker`）标准输出的第一行：文档打开之后立刻写出、刷新。
/// 后面处理页面时崩溃或超时，父进程从这一行知道这个文件有几页。
#[derive(Debug, Serialize, Deserialize)]
struct LoadedLine {
    loaded: Loaded,
}

#[derive(Debug, Serialize, Deserialize)]
struct Loaded {
    page_count: usize,
}

#[derive(Debug, Serialize)]
struct ReportOptions<'a> {
    input: String,
    root: String,
    isolate: bool,
    timeout_s: u64,
    jobs: usize,
    #[serde(flatten)]
    engine: &'a Options,
}

pub fn run(lib: &Library, args: &RoundtripArgs) -> Result<()> {
    let started = Instant::now();
    let started_unix_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let opts = Options {
        scale: args.scale,
        calibrate: args.calibrate,
        keep_saved: args.save_dir.is_some(),
        ..Options::default()
    };
    let (root, entries) = enumerate(&args.input, args.source.as_deref())?;
    anyhow::ensure!(
        !entries.is_empty(),
        "{} 下没有 PDF 文件",
        args.input.display()
    );
    if !args.quiet {
        eprintln!(
            "往返测试：{} 个文件，PDFium {}，{}{}",
            entries.len(),
            Library::linked_version(),
            if args.calibrate {
                "校准模式（不重新生成内容流）"
            } else {
                "重新生成全部内容流"
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
        run_in_process(lib, &entries, &opts, args)
    };

    let by_source = roundtrip::summarize(&results);
    let total = roundtrip::total(&results);
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "往返测试{}：{} 个文件，{} 页，耗时 {:.1} s（来源：{}）",
        if args.calibrate { "（校准）" } else { "" },
        total.files,
        total.pages,
        elapsed_ms as f64 / 1000.0,
        root.display()
    )?;
    writeln!(stdout)?;
    write!(stdout, "{}", roundtrip::render_table(&by_source, &total))?;

    if let Some(json_path) = &args.json {
        let report = Report {
            kind: REPORT_KIND,
            format: REPORT_FORMAT,
            tool: Tool {
                name: "magpdf-cli roundtrip",
                version: env!("CARGO_PKG_VERSION"),
                pdfium: Library::linked_version(),
                pdfium_library: Library::loaded_path().map(|p| p.display().to_string()),
            },
            options: ReportOptions {
                input: args.input.display().to_string(),
                root: root.display().to_string(),
                isolate: args.isolate,
                timeout_s: args.timeout,
                jobs: args.jobs.max(1),
                engine: &opts,
            },
            started_unix_s,
            elapsed_ms,
            by_source: &by_source,
            total: &total,
            files: &results,
        };
        if let Some(parent) = json_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let file =
            fs::File::create(json_path).with_context(|| format!("创建 {}", json_path.display()))?;
        serde_json::to_writer_pretty(std::io::BufWriter::new(file), &report)?;
        writeln!(stdout)?;
        writeln!(stdout, "JSON 结果：{}", json_path.display())?;
    }
    Ok(())
}

pub fn run_worker(lib: &Library, args: &WorkerArgs) -> Result<()> {
    let id = FileId {
        path: args.rel.clone(),
        source: args.source.clone(),
    };
    let opts = Options {
        scale: args.scale,
        calibrate: args.calibrate,
        keep_saved: args.save_dir.is_some(),
        ..Options::default()
    };
    let result = process_file(
        lib,
        &args.file,
        id,
        &opts,
        args.diff_dir.as_deref(),
        args.save_dir.as_deref(),
        &mut |page_count| {
            // 文档一打开就把页数写出去并刷新：后面崩溃了，父进程还读得到这一行。写不出去不算错。
            let line = LoadedLine {
                loaded: Loaded { page_count },
            };
            if let Ok(text) = serde_json::to_string(&line) {
                let mut stdout = std::io::stdout().lock();
                let _ = writeln!(stdout, "{text}");
                let _ = stdout.flush();
            }
        },
    );
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &result)?;
    writeln!(stdout)?;
    Ok(())
}

/// 读入一个文件、跑往返测试、按需写出差异图和保存结果。文档打开之后先调用一次 `on_loaded`（页数）。
fn process_file(
    lib: &Library,
    path: &Path,
    id: FileId,
    opts: &Options,
    diff_dir: Option<&Path>,
    save_dir: Option<&Path>,
    on_loaded: &mut roundtrip::LoadedHook<'_>,
) -> FileResult {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(e) => {
            return FileResult::failed(id, FileStatus::LoadFailed, format!("读取文件失败：{e}"));
        }
    };
    let mut sink = diff_sink(diff_dir, id.clone(), opts);
    let mut result = roundtrip::run_bytes_reporting(lib, id, data, opts, on_loaded, &mut sink);
    if let (Some(dir), Some(saved)) = (save_dir, result.saved.take())
        && let Err(e) = write_saved(dir, &result.id, &saved)
    {
        // 日志里不写路径（《工程规范》4）
        tracing::warn!(error = %e, "写保存结果失败");
    }
    result
}

fn write_saved(dir: &Path, id: &FileId, saved: &[u8]) -> Result<()> {
    fs::create_dir_all(dir)?;
    let rel = id.path.strip_suffix(".pdf").unwrap_or(&id.path);
    let name = format!("{}__{}.pdf", sanitize(&id.source), sanitize(rel));
    fs::write(dir.join(name), saved)?;
    Ok(())
}

/// 收集输入下的 PDF 文件，确定根目录与每个文件的来源类别。
fn enumerate(input: &Path, source_override: Option<&str>) -> Result<(PathBuf, Vec<Entry>)> {
    let meta = fs::metadata(input).with_context(|| format!("访问 {}", input.display()))?;
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
                id: FileId {
                    path: rel_str,
                    source,
                },
            }
        })
        .collect();
    Ok((root, entries))
}

/// 递归收集目录下的 PDF 文件（`fidelity` 子命令也用）。
pub(crate) fn collect_pdfs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("读取目录 {}", dir.display()))? {
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

fn run_in_process(
    lib: &Library,
    entries: &[Entry],
    opts: &Options,
    args: &RoundtripArgs,
) -> Vec<FileResult> {
    let mut results = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let result = process_file(
            lib,
            &entry.path,
            entry.id.clone(),
            opts,
            args.diff_dir.as_deref(),
            args.save_dir.as_deref(),
            &mut |_| {},
        );
        if !args.quiet {
            eprintln!("{}", progress_line(i + 1, entries.len(), &result));
        }
        results.push(result);
    }
    results
}

fn run_isolated(entries: &[Entry], args: &RoundtripArgs) -> Result<Vec<FileResult>> {
    let exe = std::env::current_exe().context("取当前可执行文件路径")?;
    let timeout = Duration::from_secs(args.timeout.max(1));
    let jobs = args.jobs.clamp(1, entries.len().max(1));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<FileResult>>> = Mutex::new(vec![None; entries.len()]);

    thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= entries.len() {
                        break;
                    }
                    let result = run_one_isolated(&exe, &entries[i], args, timeout);
                    let finished = done.fetch_add(1, Ordering::SeqCst) + 1;
                    if !args.quiet {
                        eprintln!("{}", progress_line(finished, entries.len(), &result));
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
        .map(|(slot, entry)| {
            slot.unwrap_or_else(|| {
                FileResult::failed(entry.id.clone(), FileStatus::Crashed, "子进程没有返回结果")
            })
        })
        .collect())
}

fn run_one_isolated(
    exe: &Path,
    entry: &Entry,
    args: &RoundtripArgs,
    timeout: Duration,
) -> FileResult {
    let mut cmd = Command::new(exe);
    cmd.arg("roundtrip-worker")
        .arg(&entry.path)
        .arg("--rel")
        .arg(&entry.id.path)
        .arg("--source")
        .arg(&entry.id.source)
        .arg("--scale")
        .arg(args.scale.to_string());
    if args.calibrate {
        cmd.arg("--calibrate");
    }
    if let Some(dir) = &args.diff_dir {
        cmd.arg("--diff-dir").arg(dir);
    }
    if let Some(dir) = &args.save_dir {
        cmd.arg("--save-dir").arg(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return FileResult::failed(
                entry.id.clone(),
                FileStatus::Crashed,
                format!("启动子进程失败：{e}"),
            );
        }
    };
    // 边等边读，避免子进程写满管道缓冲区后阻塞。
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = thread::spawn(move || read_all(stdout));
    let err_reader = thread::spawn(move || read_all(stderr));

    let deadline = started + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (loaded_pages, parsed) = parse_worker_output(&stdout);

    let mut result = match status {
        None => FileResult::failed(
            entry.id.clone(),
            FileStatus::TimedOut,
            format!("超过 {} s 未完成，已终止", timeout.as_secs()),
        ),
        Some(status) if status.success() => match parsed {
            Ok(result) => result,
            Err(e) => {
                let mut result = FileResult::failed(
                    entry.id.clone(),
                    FileStatus::Crashed,
                    format!("子进程输出无法解析：{e}"),
                );
                result.stderr = stderr_tail(&stderr);
                result
            }
        },
        Some(status) => {
            let code = status.code();
            let mut result = FileResult::failed(
                entry.id.clone(),
                FileStatus::Crashed,
                format!(
                    "子进程异常退出（{}）",
                    code.map_or_else(|| "被终止".to_owned(), |c| format!("0x{:08X}", c as u32)),
                ),
            );
            result.exit_code = code;
            result.stderr = stderr_tail(&stderr);
            result
        }
    };
    // 崩溃、超时的文件没有完整结果，页数用子进程打开文档之后报的那一行。
    if result.page_count == 0
        && let Some(pages) = loaded_pages
    {
        result.page_count = pages;
    }
    if result.timings.total_ms == 0 {
        result.timings.total_ms = elapsed;
    }
    result
}

/// 读子进程的标准输出：前面可以有一行页数（[`LoadedLine`]），最后一行是完整的 `FileResult`。
///
/// 子进程崩溃或被终止时最后一行不是完整结果，这时第二项是出错的原因，页数照样返回。
/// 旧版子进程只写一份结果、没有页数那一行，也读得了。
fn parse_worker_output(stdout: &[u8]) -> (Option<usize>, std::result::Result<FileResult, String>) {
    let mut loaded = None;
    let mut last: Option<&[u8]> = None;
    for line in stdout.split(|b| *b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        if let Ok(header) = serde_json::from_slice::<LoadedLine>(line) {
            loaded = Some(header.loaded.page_count);
        }
        last = Some(line);
    }
    let result = match last {
        Some(line) => serde_json::from_slice::<FileResult>(line).map_err(|e| e.to_string()),
        None => Err("子进程没有输出".to_owned()),
    };
    (loaded, result)
}

fn read_all(reader: Option<impl Read>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut reader) = reader {
        let _ = reader.read_to_end(&mut buf);
    }
    buf
}

/// 子进程标准错误的最后 300 个字符，换行换成空格；什么也没有时是 `None`。
/// 放进结果的 `stderr` 字段，不拼到 `error` 里：这一段可能带出文档里的东西。
fn stderr_tail(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let flat = text.replace(['\r', '\n'], " ");
    let tail: String = flat
        .chars()
        .rev()
        .take(300)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Some(tail)
}

fn progress_line(done: usize, total: usize, r: &FileResult) -> String {
    let (passed, diff, errored) =
        r.pages
            .iter()
            .fold((0, 0, 0), |(p, d, e), page| match page.status {
                PageStatus::Pass => (p + 1, d, e),
                PageStatus::Diff => (p, d + 1, e),
                PageStatus::Error => (p, d, e + 1),
            });
    let state = match r.status {
        FileStatus::Ok => format!(
            "{} 页：无差异 {passed}，有差异 {diff}，页错误 {errored}",
            r.page_count
        ),
        FileStatus::LoadFailed => "打开失败".to_owned(),
        FileStatus::SaveFailed => "保存失败".to_owned(),
        FileStatus::ReopenFailed => "保存结果无法打开".to_owned(),
        FileStatus::Crashed => "崩溃".to_owned(),
        FileStatus::TimedOut => "超时".to_owned(),
    };
    let error = r
        .error
        .as_deref()
        .map_or(String::new(), |e| format!("  [{e}]"));
    format!(
        "[{done}/{total}] {}  {state}  {} ms{error}",
        r.id.path, r.timings.total_ms
    )
}

/// 差异页回调：有输出目录时写三张 PNG 并把文件名记进记录。
fn diff_sink<'a>(
    dir: Option<&'a Path>,
    id: FileId,
    opts: &Options,
) -> impl FnMut(&mut PageRecord, Option<(&Bitmap, &Bitmap)>) + 'a {
    let tolerance = opts.thresholds.channel_tolerance;
    move |rec, bitmaps| {
        let (Some(dir), Some((a, b))) = (dir, bitmaps) else {
            return;
        };
        match write_diff_images(dir, &id, rec.index, a, b, tolerance) {
            Ok(images) => rec.diff_images = Some(images),
            // 日志里不写路径（《工程规范》4）
            Err(e) => tracing::warn!(page = rec.index, error = %e, "写差异图失败"),
        }
    }
}

fn write_diff_images(
    dir: &Path,
    id: &FileId,
    page_index: usize,
    a: &Bitmap,
    b: &Bitmap,
    tolerance: u8,
) -> Result<DiffImages> {
    fs::create_dir_all(dir)?;
    let stem = diff_stem(id, page_index);
    let images = DiffImages {
        a: format!("{stem}_a.png"),
        b: format!("{stem}_b.png"),
        heat: format!("{stem}_diff.png"),
    };
    write_png(a, &dir.join(&images.a))?;
    write_png(b, &dir.join(&images.b))?;
    write_png(&heatmap(a, b, tolerance)?, &dir.join(&images.heat))?;
    Ok(images)
}

/// 差异图文件名：来源、相对路径（去掉扩展名，分隔符改下划线）、页码。
fn diff_stem(id: &FileId, page_index: usize) -> String {
    let rel = id.path.strip_suffix(".pdf").unwrap_or(&id.path);
    format!(
        "{}__{}__p{:03}",
        sanitize(&id.source),
        sanitize(rel),
        page_index + 1
    )
}

/// 只保留字母数字和 `-`、`.`、`_`，其余（含路径分隔符）改成下划线。
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '.' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_stem_is_filesystem_safe() {
        let id = FileId {
            path: "sub dir/a:b.pdf".to_owned(),
            source: "office-word".to_owned(),
        };
        assert_eq!(diff_stem(&id, 0), "office-word__sub_dir_a_b__p001");
    }

    fn sample_result(pages: usize) -> FileResult {
        let id = FileId {
            path: "a.pdf".to_owned(),
            source: "s".to_owned(),
        };
        let mut result = FileResult::failed(id, FileStatus::Ok, "");
        result.error = None;
        result.page_count = pages;
        result
    }

    #[test]
    fn worker_output_is_a_page_count_line_then_the_result() {
        let result = serde_json::to_string(&sample_result(3)).unwrap();

        // 正常：先一行页数，再一行结果。
        let (loaded, parsed) = parse_worker_output(
            format!("{{\"loaded\":{{\"page_count\":3}}}}\n{result}\n").as_bytes(),
        );
        assert_eq!(loaded, Some(3));
        assert_eq!(parsed.unwrap(), sample_result(3));

        // 打开文档之后崩溃：只有页数那一行，结果读不出来，页数还在。
        let (loaded, parsed) = parse_worker_output(b"{\"loaded\":{\"page_count\":12}}\r\n");
        assert_eq!(loaded, Some(12));
        assert!(parsed.is_err());

        // 结果写到一半被终止。
        let cut = &result[..result.len() / 2];
        let (loaded, parsed) =
            parse_worker_output(format!("{{\"loaded\":{{\"page_count\":3}}}}\n{cut}").as_bytes());
        assert_eq!(loaded, Some(3));
        assert!(parsed.is_err());

        // 文档还没打开就崩溃：什么也没有。
        let (loaded, parsed) = parse_worker_output(b"");
        assert_eq!(loaded, None);
        assert_eq!(parsed.unwrap_err(), "子进程没有输出");

        // 打不开的文件不报页数，只有结果；旧版子进程也是只写一份结果、末尾没有换行。
        let (loaded, parsed) = parse_worker_output(result.as_bytes());
        assert_eq!(loaded, None);
        assert_eq!(parsed.unwrap().page_count, 3);
    }

    #[test]
    fn page_count_line_matches_what_the_worker_writes() {
        let line = serde_json::to_string(&LoadedLine {
            loaded: Loaded { page_count: 7 },
        })
        .unwrap();
        assert_eq!(line, r#"{"loaded":{"page_count":7}}"#);
        assert_eq!(parse_worker_output(line.as_bytes()).0, Some(7));
    }

    #[test]
    fn child_stderr_is_kept_apart_from_the_error_text() {
        assert_eq!(stderr_tail(b""), None);
        assert_eq!(stderr_tail(b" \r\n "), None);
        assert_eq!(
            stderr_tail(b"thread 'main' panicked\r\nat src/x.rs\n").as_deref(),
            Some("thread 'main' panicked  at src/x.rs")
        );
        let long = "x".repeat(400) + "END";
        let tail = stderr_tail(long.as_bytes()).unwrap();
        assert_eq!(tail.chars().count(), 300);
        assert!(tail.ends_with("END"));

        // 结果里：error 只有工具自己的话，stderr 另放；没有 stderr 时不写这个字段。
        let mut crashed = sample_result(0);
        crashed.status = FileStatus::Crashed;
        crashed.error = Some("子进程异常退出（0x80000003）".to_owned());
        crashed.stderr = stderr_tail(b"panicked at something");
        let json = serde_json::to_value(&crashed).unwrap();
        assert_eq!(json["error"], "子进程异常退出（0x80000003）");
        assert_eq!(json["stderr"], "panicked at something");
        assert!(
            serde_json::to_value(sample_result(1))
                .unwrap()
                .get("stderr")
                .is_none()
        );
    }

    #[test]
    fn report_starts_with_kind_and_format() {
        let by_source = BTreeMap::new();
        let total = SourceSummary::default();
        let opts = Options::default();
        let report = Report {
            kind: REPORT_KIND,
            format: REPORT_FORMAT,
            tool: Tool {
                name: "magpdf-cli roundtrip",
                version: "0.0.0",
                pdfium: "1.2.3.4",
                pdfium_library: Some(r"C:\x\pdfium.dll".to_owned()),
            },
            options: ReportOptions {
                input: "in".to_owned(),
                root: "in".to_owned(),
                isolate: true,
                timeout_s: 60,
                jobs: 2,
                engine: &opts,
            },
            started_unix_s: 0,
            elapsed_ms: 0,
            by_source: &by_source,
            total: &total,
            files: &[],
        };
        let text = serde_json::to_string(&report).unwrap();
        assert!(
            text.starts_with(r#"{"kind":"magpdf-roundtrip","format":2,"tool":{"#),
            "{text}"
        );
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["tool"]["pdfium_library"], r"C:\x\pdfium.dll");
        assert_eq!(json["tool"]["pdfium"], "1.2.3.4");
    }

    #[test]
    fn enumerate_uses_first_directory_as_source() {
        let tmp = std::env::temp_dir().join(format!("magpdf-rt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("office-word/deep")).unwrap();
        fs::create_dir_all(tmp.join("design")).unwrap();
        fs::write(tmp.join("office-word/deep/a.pdf"), b"x").unwrap();
        fs::write(tmp.join("design/b.PDF"), b"x").unwrap();
        fs::write(tmp.join("design/readme.txt"), b"x").unwrap();
        fs::write(tmp.join("c.pdf"), b"x").unwrap();

        let (root, entries) = enumerate(&tmp, None).unwrap();
        assert_eq!(root, tmp);
        let ids: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.id.path.as_str(), e.id.source.as_str()))
            .collect();
        let root_name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            ids,
            [
                ("c.pdf", root_name.as_str()),
                ("design/b.PDF", "design"),
                ("office-word/deep/a.pdf", "office-word"),
            ]
        );

        let (_, forced) = enumerate(&tmp, Some("public")).unwrap();
        assert!(forced.iter().all(|e| e.id.source == "public"));

        let (root, single) = enumerate(&tmp.join("design/b.PDF"), None).unwrap();
        assert_eq!(root, tmp.join("design"));
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].id.source, "design");

        let _ = fs::remove_dir_all(&tmp);
    }
}
