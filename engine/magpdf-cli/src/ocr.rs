//! `ocr` 与 `ocr-eval` 子命令（P0 验证项 V09）。
//!
//! - `ocr`：对图片、图片目录或 PDF 页做 OCR，结果写入 `--json`；终端只打印数量与耗时，
//!   识别文字只有加 `--show-text` 才打印（工程规范 §5：日志不含文档内容）。
//! - `ocr-eval`：用 PPOCRLabel 标注评测预测结果，输出字符准确率与文字行召回率（脱敏规范 §7）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Args;
use magpdf_scan::ocr::det::LimitType;
use magpdf_scan::ocr::eval::{EvalOptions, PageMetrics, Report, evaluate_page};
use magpdf_scan::ocr::format::{PageResultFile, infer_lang_from_path, read_label_file};
use magpdf_scan::ocr::models::{Manifest, ModelKind, ModelSelection, find_models_dir};
use magpdf_scan::{ImageView, Lang, Millis, OcrEngine, OcrResult, StageTimings};
use pdfium_sys::{Document, Library, RenderOptions};

#[derive(Args, Debug)]
pub struct OcrArgs {
    /// 图片（PNG/JPEG）、图片目录或 PDF 文件
    pub input: PathBuf,
    /// PDF 页码，从 1 开始
    #[arg(short, long, default_value_t = 1)]
    pub page: usize,
    /// PDF 的全部页（忽略 --page；--json 须为目录）
    #[arg(long)]
    pub all_pages: bool,
    /// PDF 渲染分辨率（dpi）
    #[arg(long, default_value_t = 300.0)]
    pub dpi: f32,
    /// 结果 JSON 路径；输入为目录或 --all-pages 时为输出目录
    #[arg(long)]
    pub json: Option<PathBuf>,
    /// 语言：zh | en | auto（决定识别模型）
    #[arg(long, default_value = "auto")]
    pub lang: Lang,
    /// 模型目录（含 manifest.json）；默认 MAGPDF_MODELS_DIR 或向上查找 models/
    #[arg(long)]
    pub models_dir: Option<PathBuf>,
    /// 使用 server 集的检测/识别模型（高精度、慢）
    #[arg(long)]
    pub server: bool,
    /// 显式指定检测模型 id
    #[arg(long)]
    pub det: Option<String>,
    /// 显式指定识别模型 id
    #[arg(long)]
    pub rec: Option<String>,
    /// 显式指定方向分类模型 id
    #[arg(long)]
    pub cls: Option<String>,
    /// 不做文字行方向分类
    #[arg(long)]
    pub no_cls: bool,
    /// 不复核方向：和同页多数行方向不一样的行，照方向分类的判断识别，不再正反各试一次
    #[arg(long)]
    pub no_cls_verify: bool,
    /// 检测缩放规则：max（长边不超过 --det-limit-side）| min（短边不小于 --det-limit-side）
    #[arg(long, value_parser = parse_limit_type)]
    pub det_limit_type: Option<LimitType>,
    /// 检测缩放边长
    #[arg(long)]
    pub det_limit_side: Option<u32>,
    /// 每个会话的算子内线程数（默认交给 ONNX Runtime）
    #[arg(long)]
    pub threads: Option<usize>,
    /// 低置信度标记阈值
    #[arg(long)]
    pub low_confidence: Option<f32>,
    /// 每张图重复识别次数（计时用；JSON 记录每次耗时）
    #[arg(long, default_value_t = 1)]
    pub repeat: usize,
    /// 在终端打印识别文字（默认不打印）
    #[arg(long)]
    pub show_text: bool,
    /// 加载前校验模型文件的 SHA-256
    #[arg(long)]
    pub verify: bool,
}

#[derive(Args, Debug)]
pub struct OcrEvalArgs {
    /// 预测结果：`ocr --json` 写出的 JSON 文件或目录
    pub predictions: PathBuf,
    /// 标注：PPOCRLabel 的 Label.txt
    pub labels: PathBuf,
    /// 行匹配的交并比阈值
    #[arg(long, default_value_t = 0.5)]
    pub iou: f32,
    /// 比较前去掉全部空白（附加口径）
    #[arg(long)]
    pub ignore_space: bool,
    /// 图像主干名 → 语言（zh|en|mixed）的 JSON 映射；缺省按文件名推断
    #[arg(long)]
    pub lang_map: Option<PathBuf>,
    /// 把汇总报告写成 JSON
    #[arg(long)]
    pub json: Option<PathBuf>,
    /// 把逐行匹配明细（含文字）写成 JSON
    #[arg(long)]
    pub details: Option<PathBuf>,
}

fn parse_limit_type(s: &str) -> std::result::Result<LimitType, String> {
    match s.to_ascii_lowercase().as_str() {
        "max" => Ok(LimitType::Max),
        "min" => Ok(LimitType::Min),
        other => Err(format!("不支持的缩放规则 {other:?}，可选 max、min")),
    }
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg"];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// 一次识别任务：来源与页码。
enum Job {
    Image(PathBuf),
    PdfPage { file: PathBuf, page: usize },
}

impl Job {
    fn source(&self) -> &Path {
        match self {
            Job::Image(p) => p,
            Job::PdfPage { file, .. } => file,
        }
    }

    fn page(&self) -> usize {
        match self {
            Job::Image(_) => 1,
            Job::PdfPage { page, .. } => *page,
        }
    }

    fn output_name(&self) -> String {
        let stem = self
            .source()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("page");
        match self {
            Job::Image(_) => format!("{stem}.json"),
            Job::PdfPage { page, .. } => format!("{stem}-p{page:03}.json"),
        }
    }
}

pub fn ocr(lib: &Library, args: OcrArgs) -> Result<()> {
    anyhow::ensure!(args.page >= 1, "页码从 1 开始");
    anyhow::ensure!(args.repeat >= 1, "--repeat 至少为 1");

    // 模型目录与配置。
    let models_dir = match &args.models_dir {
        Some(d) => d.clone(),
        None => find_models_dir().context(
            "找不到模型目录：用 --models-dir 指定，或设置 MAGPDF_MODELS_DIR，或在仓库内运行（先执行 models/fetch-models.ps1）",
        )?,
    };
    let manifest = Manifest::load_dir(&models_dir)
        .with_context(|| format!("读取 {}", models_dir.join("manifest.json").display()))?;
    let selection = ModelSelection {
        lang: args.lang,
        server: args.server,
        det: args.det.clone(),
        rec: args.rec.clone(),
        cls: args.cls.clone(),
        no_cls: args.no_cls,
    };
    let missing = manifest.missing_files(&selection)?;
    if !missing.is_empty() {
        bail!(
            "缺少模型文件（运行 pwsh models/fetch-models.ps1 下载）：\n  {}",
            missing
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }
    if args.verify {
        let (det, rec, cls) = manifest.select(&selection)?;
        for entry in [Some(det), Some(rec), cls].into_iter().flatten() {
            let t = Instant::now();
            manifest
                .verify(entry)
                .with_context(|| format!("校验 {}", entry.id))?;
            println!(
                "校验通过 {}（{:.0} ms）",
                entry.id,
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
        // 字典文件也校验。
        if let Some(dict_rel) = &rec.dict
            && let Some(dict_entry) = manifest
                .models
                .iter()
                .find(|m| m.kind == ModelKind::Dict && &m.file == dict_rel)
        {
            manifest
                .verify(dict_entry)
                .with_context(|| format!("校验 {}", dict_entry.id))?;
            println!("校验通过 {}", dict_entry.id);
        }
    }
    let mut config = manifest.engine_config(&selection)?;
    if let Some(t) = args.threads {
        config.intra_threads = t;
    }
    if let Some(lt) = args.det_limit_type {
        config.det.limit_type = lt;
    }
    if let Some(side) = args.det_limit_side {
        config.det.limit_side_len = side;
    }
    if let Some(lc) = args.low_confidence {
        config.low_confidence = lc;
    }
    if args.no_cls_verify {
        config.cls.verify = false;
    }

    // 任务列表。
    let jobs = collect_jobs(lib, &args)?;
    anyhow::ensure!(!jobs.is_empty(), "输入里没有可识别的图片或 PDF 页");
    let batch_output = jobs.len() > 1 || args.all_pages || args.input.is_dir();
    if let Some(json) = &args.json
        && batch_output
    {
        std::fs::create_dir_all(json).with_context(|| format!("创建 {}", json.display()))?;
    }

    // 加载引擎。
    let mut engine = OcrEngine::new(config).context("加载 OCR 引擎")?;
    let load = engine.load_timings();
    println!(
        "模型：det={} rec={} cls={}（{}，字典 {}）",
        engine.config().identity.det,
        engine.config().identity.rec,
        engine.config().identity.cls.as_deref().unwrap_or("无"),
        engine.config().identity.runtime,
        engine.dict_source(),
    );
    println!(
        "加载耗时：运行时 {}，det {}，cls {}，rec {}，合计 {}",
        load.runtime, load.det, load.cls, load.rec, load.total
    );
    println!(
        "检测缩放：{:?} {}，线程 {}，{} 张/页，重复 {} 次",
        engine.config().det.limit_type,
        engine.config().det.limit_side_len,
        if engine.config().intra_threads == 0 {
            "默认".to_owned()
        } else {
            engine.config().intra_threads.to_string()
        },
        jobs.len(),
        args.repeat
    );

    let mut all_runs: Vec<StageTimings> = Vec::new();
    let mut total_lines = 0usize;
    let mut total_low = 0usize;
    let mut total_checked = 0usize;
    let mut total_overruled = 0usize;
    for job in &jobs {
        let done = run_job(lib, &mut engine, job, &args)?;
        let (width, height, result, runs) = (done.width, done.height, done.result, done.runs);
        total_lines += result.lines.len();
        total_low += result.lines.iter().filter(|l| l.low_confidence).count();
        total_checked += result.orientation_checked;
        total_overruled += result.orientation_overruled;
        let t = &result.timings;
        println!(
            "{}{}：{}×{}，{} 行（低置信度 {}，丢弃 {}，方向复核 {} 改 {}），det 输入 {}×{}，耗时 {} {} / det {} / cls {} / rec {} / 识别合计 {}",
            job.source().display(),
            match job {
                Job::PdfPage { page, .. } => format!(" 第 {page} 页"),
                Job::Image(_) => String::new(),
            },
            width,
            height,
            result.lines.len(),
            result.lines.iter().filter(|l| l.low_confidence).count(),
            result.dropped,
            result.orientation_checked,
            result.orientation_overruled,
            t.det.input_size.0,
            t.det.input_size.1,
            match job {
                Job::PdfPage { .. } => "渲染",
                Job::Image(_) => "解码",
            },
            done.acquire,
            t.det.infer,
            t.cls,
            t.rec,
            t.total
        );
        if args.show_text {
            for l in &result.lines {
                println!(
                    "  [{:.2}]{} {}",
                    l.confidence,
                    if l.low_confidence { "!" } else { " " },
                    l.text
                );
            }
        }
        if let Some(json) = &args.json {
            let dpi = matches!(job, Job::PdfPage { .. }).then_some(args.dpi);
            let mut file = PageResultFile::from_result(&result, job.source(), job.page(), dpi);
            file.load_timings = Some(load);
            file.acquire = Some(done.acquire);
            if runs.len() > 1 {
                file.runs = runs.clone();
            }
            let path = if batch_output {
                json.join(job.output_name())
            } else {
                json.clone()
            };
            file.write(&path)
                .with_context(|| format!("写入 {}", path.display()))?;
        }
        all_runs.extend(runs);
    }

    if all_runs.len() > 1 {
        let med = |f: fn(&StageTimings) -> f64| median(all_runs.iter().map(f));
        println!(
            "{} 次识别的中位耗时：det {:.1} ms（含预处理 {:.1}）/ cls {:.1} / rec {:.1} / 合计 {:.1} ms；共 {} 行，低置信度 {}",
            all_runs.len(),
            med(|t| t.det.infer.0),
            med(|t| t.det.resize.0 + t.det.infer.0 + t.det.postprocess.0),
            med(|t| t.cls.0),
            med(|t| t.rec.0),
            med(|t| t.total.0),
            total_lines,
            total_low
        );
    }
    if total_checked > 0 {
        println!("方向复核 {total_checked} 行，其中 {total_overruled} 行没有采用方向分类的判断");
    }
    if args.json.is_none() && !args.show_text {
        println!("提示：识别文字不打印到终端；用 --json 写出结果或 --show-text 查看。");
    }
    Ok(())
}

fn collect_jobs(lib: &Library, args: &OcrArgs) -> Result<Vec<Job>> {
    let input = &args.input;
    if input.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(input)
            .with_context(|| format!("读取目录 {}", input.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && is_image(p))
            .collect();
        files.sort();
        return Ok(files.into_iter().map(Job::Image).collect());
    }
    anyhow::ensure!(input.is_file(), "输入不存在：{}", input.display());
    if is_pdf(input) {
        let doc = Document::load_file(lib, input, None)
            .with_context(|| format!("打开 {}", input.display()))?;
        let count = doc.page_count();
        if args.all_pages {
            return Ok((1..=count)
                .map(|page| Job::PdfPage {
                    file: input.clone(),
                    page,
                })
                .collect());
        }
        anyhow::ensure!(args.page <= count, "PDF 只有 {count} 页");
        return Ok(vec![Job::PdfPage {
            file: input.clone(),
            page: args.page,
        }]);
    }
    if is_image(input) {
        return Ok(vec![Job::Image(input.clone())]);
    }
    bail!("不支持的输入类型：{}", input.display())
}

/// 一个任务跑完的结果。
struct JobDone {
    width: u32,
    height: u32,
    /// 最后一次识别的结果。
    result: OcrResult,
    /// 每次识别的耗时。
    runs: Vec<StageTimings>,
    /// 取得位图花的时间：图片是读文件加解码；PDF 是打开文档、加载页面、渲染。
    acquire: Millis,
}

/// 执行一个任务：取得位图，识别 `--repeat` 次。
fn run_job(lib: &Library, engine: &mut OcrEngine, job: &Job, args: &OcrArgs) -> Result<JobDone> {
    let started = Instant::now();
    match job {
        Job::Image(path) => {
            let img = image::ImageReader::open(path)
                .with_context(|| format!("打开 {}", path.display()))?
                .decode()
                .with_context(|| format!("解码 {}", path.display()))?
                .into_rgb8();
            let acquire = Millis::since(started);
            let (width, height) = (img.width(), img.height());
            let view = ImageView::rgb(width, height, img.as_raw())?;
            let (result, runs) = recognize_repeated(engine, &view, args.repeat)?;
            Ok(JobDone {
                width,
                height,
                result,
                runs,
                acquire,
            })
        }
        Job::PdfPage { file, page } => {
            let doc = Document::load_file(lib, file, None)
                .with_context(|| format!("打开 {}", file.display()))?;
            let pg = doc.page(page - 1)?;
            let bmp = pg.render(RenderOptions::dpi(args.dpi))?;
            let acquire = Millis::since(started);
            tracing::debug!(
                page,
                width = bmp.width(),
                height = bmp.height(),
                render_ms = acquire.0,
                "PDF 页已渲染"
            );
            let view = ImageView::bgra(bmp.width(), bmp.height(), bmp.stride(), bmp.bgra())?;
            let (result, runs) = recognize_repeated(engine, &view, args.repeat)?;
            Ok(JobDone {
                width: bmp.width(),
                height: bmp.height(),
                result,
                runs,
                acquire,
            })
        }
    }
}

fn recognize_repeated(
    engine: &mut OcrEngine,
    view: &ImageView<'_>,
    repeat: usize,
) -> Result<(OcrResult, Vec<StageTimings>)> {
    let mut runs = Vec::with_capacity(repeat);
    let mut last = None;
    for _ in 0..repeat {
        let r = engine.recognize(view)?;
        runs.push(r.timings);
        last = Some(r);
    }
    Ok((last.context("至少识别一次")?, runs))
}

fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

#[derive(Debug, serde::Serialize)]
struct DetailLine {
    gt_text: String,
    pred_text: Option<String>,
    iou: f32,
    edit_distance: usize,
    gt_chars: usize,
}

#[derive(Debug, serde::Serialize)]
struct ImageDetail {
    image: String,
    lang: String,
    metrics: PageMetrics,
    lines: Vec<DetailLine>,
    unmatched_predictions: Vec<String>,
}

pub fn ocr_eval(args: OcrEvalArgs) -> Result<()> {
    let labels = read_label_file(&args.labels)
        .with_context(|| format!("读取标注 {}", args.labels.display()))?;
    anyhow::ensure!(!labels.is_empty(), "标注文件为空");

    // 预测：文件或目录，按文件名主干索引。
    let mut preds: BTreeMap<String, PageResultFile> = BTreeMap::new();
    let pred_paths: Vec<PathBuf> = if args.predictions.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&args.predictions)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            })
            .collect();
        v.sort();
        v
    } else {
        vec![args.predictions.clone()]
    };
    for p in &pred_paths {
        let file = PageResultFile::read(p).with_context(|| format!("读取预测 {}", p.display()))?;
        preds.insert(file.source_stem(), file);
    }
    anyhow::ensure!(!preds.is_empty(), "没有预测结果");

    let lang_map: BTreeMap<String, String> = match &args.lang_map {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(p)?)
            .with_context(|| format!("解析 {}", p.display()))?,
        None => BTreeMap::new(),
    };
    let options = EvalOptions {
        iou_thresh: args.iou,
        ignore_space: args.ignore_space,
    };

    let mut report = Report {
        options: Some(options),
        ..Report::default()
    };
    let mut details = Vec::new();
    let mut missing_preds = Vec::new();
    for img in &labels {
        let stem = img.stem();
        let Some(pred) = preds.get(&stem) else {
            missing_preds.push(img.image.clone());
            continue;
        };
        let lang = lang_map
            .get(&stem)
            .cloned()
            .or_else(|| infer_lang_from_path(&img.image).map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        let metrics = evaluate_page(&pred.lines, &img.annotations, &options);
        report.add(&lang, &metrics);
        if args.details.is_some() {
            let mut used = vec![false; pred.lines.len()];
            let lines = metrics
                .matches
                .iter()
                .map(|m| {
                    if let Some(pi) = m.pred_index {
                        used[pi] = true;
                    }
                    DetailLine {
                        gt_text: img.annotations[m.gt_index].transcription.clone(),
                        pred_text: m.pred_index.map(|pi| pred.lines[pi].text.clone()),
                        iou: m.iou,
                        edit_distance: m.edit_distance,
                        gt_chars: m.gt_chars,
                    }
                })
                .collect();
            let unmatched_predictions = pred
                .lines
                .iter()
                .zip(&used)
                .filter(|(_, u)| !**u)
                .map(|(l, _)| l.text.clone())
                .collect();
            details.push(ImageDetail {
                image: img.image.clone(),
                lang,
                metrics,
                lines,
                unmatched_predictions,
            });
        }
    }

    print_report(&report);
    if !missing_preds.is_empty() {
        println!(
            "缺少预测的标注图像 {} 张（未计入）：{}",
            missing_preds.len(),
            missing_preds.join(", ")
        );
    }
    if let Some(p) = &args.json {
        std::fs::write(p, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("写入 {}", p.display()))?;
    }
    if let Some(p) = &args.details {
        std::fs::write(p, serde_json::to_string_pretty(&details)?)
            .with_context(|| format!("写入 {}", p.display()))?;
    }
    Ok(())
}

fn pct(v: Option<f32>) -> String {
    v.map_or("  n/a ".to_owned(), |x| format!("{:6.2}%", x * 100.0))
}

fn print_report(report: &Report) {
    println!(
        "{:<8} {:>5} {:>7} {:>7} {:>9} {:>9} {:>9} {:>9} {:>6} {:>6} {:>6}",
        "语言",
        "图像",
        "标注行",
        "匹配行",
        "行召回率",
        "行精确率",
        "字符准确率",
        "页级字符",
        "误检",
        "印章",
        "手写"
    );
    let mut rows: Vec<(&str, &magpdf_scan::ocr::eval::Totals)> = report
        .per_lang
        .iter()
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    rows.push(("总计", &report.total));
    for (name, t) in rows {
        println!(
            "{:<8} {:>5} {:>7} {:>7} {:>9} {:>9} {:>9} {:>9} {:>6} {:>6} {:>6}",
            name,
            t.images,
            t.gt_lines,
            t.matched_lines,
            pct(t.line_recall()),
            pct(t.line_precision()),
            pct(t.char_accuracy()),
            pct(t.page_char_accuracy()),
            t.false_positives,
            t.in_seal,
            t.in_handwriting
        );
    }
    println!(
        "口径：IoU ≥ {:.2} 匹配；字符准确率 = 1 − Σ编辑距离 ÷ 标注字符数（□ 不计）；页级字符 = 两边各按阅读顺序拼接并去空白后的附加口径；{}",
        report.options.map_or(0.5, |o| o.iou_thresh),
        if report.options.is_some_and(|o| o.ignore_space) {
            "已忽略空白"
        } else {
            "空白按原样比较"
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_values() {
        assert_eq!(median([3.0, 1.0, 2.0].into_iter()), 2.0);
        assert_eq!(median([4.0, 1.0, 3.0, 2.0].into_iter()), 2.5);
        assert_eq!(median(std::iter::empty()), 0.0);
    }

    #[test]
    fn job_output_names() {
        assert_eq!(Job::Image("a/zh_01.png".into()).output_name(), "zh_01.json");
        assert_eq!(
            Job::PdfPage {
                file: "doc.pdf".into(),
                page: 7
            }
            .output_name(),
            "doc-p007.json"
        );
        assert!(is_image(Path::new("x.JPG")));
        assert!(is_pdf(Path::new("x.Pdf")));
        assert!(!is_image(Path::new("x.pdf")));
    }
}
