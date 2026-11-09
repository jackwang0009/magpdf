//! `text-blocks` 子命令：对一页或整份文档做文字行/块聚类（P0 验证项 V07），
//! 打印统计，可选写出 JSON、与标注比对算正确率、生成预标注。
//!
//! 终端输出和日志默认不含任何文档文字（《工程规范》4）；只有显式给 `--with-text`
//! 时才在块摘要和输出文件里附上文字片段。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use magpdf_text::eval::{self, LabelFile, PageScore, Summary};
use magpdf_text::{Layout, LayoutParams, PageText};
use pdfium_sys::{Document, Library};

/// `text-blocks` 的参数。
#[derive(clap::Args, Debug)]
pub struct Args {
    /// PDF 文件
    file: PathBuf,
    /// 页码，从 1 开始；不给则处理全部页
    #[arg(short, long)]
    page: Option<usize>,
    /// 把每页的行/块（字形索引、包围盒、对齐、行距、评测结果）写成 JSON
    #[arg(long)]
    json: Option<PathBuf>,
    /// 标注文件（格式见 docs/p0/V07-标注格式.md），给出时计算行/段正确率
    #[arg(long)]
    labels: Option<PathBuf>,
    /// 把聚类结果写成标注文件（预标注，供人工修正）
    #[arg(long)]
    emit_labels: Option<PathBuf>,
    /// 聚类参数 JSON（缺省字段用默认值）
    #[arg(long)]
    params: Option<PathBuf>,
    /// 不用页面上的细线路径（表格框线）辅助分隔，用于对比
    #[arg(long)]
    no_rulings: bool,
    /// 终端与输出文件带上每块的文字片段。默认不输出任何文档文字。
    #[arg(long)]
    with_text: bool,
    /// 逐块打印摘要（默认只打印每页统计）
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, serde::Serialize)]
struct PageReport {
    page: usize,
    width: f32,
    height: f32,
    rotation: u8,
    chars: usize,
    generated: usize,
    unicode_map_errors: usize,
    invisible: usize,
    rulings: usize,
    extract_ms: f64,
    cluster_ms: f64,
    lines: usize,
    blocks: usize,
    non_horizontal_blocks: usize,
    uneditable_blocks: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<PageScore>,
    layout: Layout,
    /// 只在 `--with-text` 时有：每块的文字（按行）。
    #[serde(skip_serializing_if = "Option::is_none")]
    texts: Option<Vec<Vec<String>>>,
}

#[derive(Debug, serde::Serialize)]
struct Report {
    file: String,
    pdfium_version: &'static str,
    params: LayoutParams,
    pages: Vec<PageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<Summary>,
}

pub fn run(lib: &Library, args: Args) -> Result<()> {
    let params = load_params(args.params.as_deref())?;
    let labels = args.labels.as_deref().map(load_labels).transpose()?;
    let doc = Document::load_file(lib, &args.file, None)
        .with_context(|| format!("打开 {}", args.file.display()))?;
    let page_range: Vec<usize> = match args.page {
        Some(p) => {
            anyhow::ensure!(
                p >= 1 && p <= doc.page_count(),
                "页码超出范围（1..={}）",
                doc.page_count()
            );
            vec![p]
        }
        None => (1..=doc.page_count()).collect(),
    };

    let mut report = Report {
        file: args.file.display().to_string(),
        pdfium_version: Library::linked_version(),
        params: params.clone(),
        pages: Vec::with_capacity(page_range.len()),
        summary: labels.is_some().then(Summary::default),
    };
    let mut emitted: Vec<eval::PageLabels> = Vec::new();

    for page_no in page_range {
        let page = doc.page(page_no - 1)?;
        let t0 = Instant::now();
        let mut text = PageText::from_page(&page, params.ruling_max_thickness)?;
        if args.no_rulings {
            text.rulings.clear();
        }
        let extract_ms = ms(t0.elapsed());
        let t1 = Instant::now();
        let layout = magpdf_text::analyze(&text, &params);
        let cluster_ms = ms(t1.elapsed());

        let score = labels
            .as_ref()
            .and_then(|l| l.page(page_no))
            .map(|pl| eval::score_layout(pl, &layout));
        if let (Some(s), Some(sum)) = (&score, report.summary.as_mut()) {
            sum.add_page(s);
        }
        if args.emit_labels.is_some() {
            let mut pl = eval::labels_from_layout(page_no, &layout);
            if args.with_text {
                for (label, line) in pl.lines.iter_mut().zip(&layout.lines) {
                    label.note = Some(excerpt(&text.text_of(&line.glyphs), 60));
                }
                for (label, block) in pl.paragraphs.iter_mut().zip(&layout.blocks) {
                    label.note = Some(excerpt(&text.text_of(&layout.block_glyphs(block)), 60));
                }
            }
            emitted.push(pl);
        }

        let pr = PageReport {
            page: page_no,
            width: text.width,
            height: text.height,
            rotation: text.rotation,
            chars: text.glyphs.len(),
            generated: text.glyphs.iter().filter(|g| g.generated).count(),
            unicode_map_errors: text.glyphs.iter().filter(|g| g.unicode_map_error).count(),
            invisible: text
                .glyphs
                .iter()
                .filter(|g| g.invisible && !g.generated)
                .count(),
            rulings: text.rulings.len(),
            extract_ms,
            cluster_ms,
            lines: layout.lines.len(),
            blocks: layout.blocks.len(),
            non_horizontal_blocks: layout.blocks.iter().filter(|b| !b.horizontal).count(),
            uneditable_blocks: layout.blocks.iter().filter(|b| !b.editable).count(),
            score,
            texts: args.with_text.then(|| {
                layout
                    .blocks
                    .iter()
                    .map(|b| {
                        b.lines
                            .iter()
                            .map(|&i| text.text_of(&layout.lines[i].glyphs))
                            .collect()
                    })
                    .collect()
            }),
            layout,
        };
        print_page(&pr, &text, args.verbose, args.with_text);
        tracing::info!(
            page = page_no,
            chars = pr.chars,
            lines = pr.lines,
            blocks = pr.blocks,
            extract_ms = pr.extract_ms,
            cluster_ms = pr.cluster_ms,
            "text-blocks"
        );
        report.pages.push(pr);
    }

    if let Some(sum) = &report.summary {
        println!(
            "汇总（{} 页有标注）：行 {}；段 {}",
            sum.pages,
            describe(&sum.lines),
            describe(&sum.paragraphs)
        );
    }
    if let Some(path) = &args.json {
        fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("写入 {}", path.display()))?;
        println!("JSON → {}", path.display());
    }
    if let Some(path) = &args.emit_labels {
        let file_name = args
            .file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let lf = LabelFile::new(file_name, emitted);
        fs::write(path, serde_json::to_string_pretty(&lf)?)
            .with_context(|| format!("写入 {}", path.display()))?;
        println!("预标注 → {}", path.display());
    }
    Ok(())
}

fn load_params(path: Option<&Path>) -> Result<LayoutParams> {
    match path {
        None => Ok(LayoutParams::default()),
        Some(p) => {
            let s = fs::read_to_string(p).with_context(|| format!("读取 {}", p.display()))?;
            serde_json::from_str(&s).with_context(|| format!("解析参数 {}", p.display()))
        }
    }
}

fn load_labels(path: &Path) -> Result<LabelFile> {
    let s = fs::read_to_string(path).with_context(|| format!("读取 {}", path.display()))?;
    let lf: LabelFile =
        serde_json::from_str(&s).with_context(|| format!("解析标注 {}", path.display()))?;
    anyhow::ensure!(
        lf.format == eval::LABEL_FORMAT,
        "标注格式 {:?} 不是 {:?}",
        lf.format,
        eval::LABEL_FORMAT
    );
    Ok(lf)
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn describe(s: &eval::ClusterScore) -> String {
    format!(
        "字符一致 {:.1}%（{}/{}），完全匹配 {:.1}%（{}/{}）",
        s.char_agreement() * 100.0,
        s.agreeing_chars,
        s.labelled_chars,
        s.exact_match_rate() * 100.0,
        s.exact_matches,
        s.labelled_clusters
    )
}

fn print_page(pr: &PageReport, text: &PageText, verbose: bool, with_text: bool) {
    println!(
        "第 {} 页 {:.1}×{:.1} pt 旋转 {}°：字符 {}（生成 {}，无 Unicode 映射 {}，不可见 {}），细线 {}；提取 {:.2} ms，聚类 {:.2} ms；行 {}，块 {}（非水平 {}，不可编辑 {}）",
        pr.page,
        pr.width,
        pr.height,
        u32::from(pr.rotation) * 90,
        pr.chars,
        pr.generated,
        pr.unicode_map_errors,
        pr.invisible,
        pr.rulings,
        pr.extract_ms,
        pr.cluster_ms,
        pr.lines,
        pr.blocks,
        pr.non_horizontal_blocks,
        pr.uneditable_blocks
    );
    if let Some(s) = &pr.score {
        println!(
            "  评测：行 {}；段 {}",
            describe(&s.lines),
            describe(&s.paragraphs)
        );
    }
    if !verbose {
        return;
    }
    for (i, b) in pr.layout.blocks.iter().enumerate() {
        let glyphs = pr.layout.block_glyphs(b);
        let pitch = b.line_pitch.map_or("—".to_owned(), |p| {
            format!("{:.1}pt/{:.2}em", p.points, p.em)
        });
        let mut flags = Vec::new();
        if !b.horizontal {
            flags.push(format!("角度 {:.0}°", b.angle));
        }
        if !b.editable {
            flags.push("不可编辑".to_owned());
        }
        if b.invisible {
            flags.push("不可见".to_owned());
        }
        println!(
            "  块 {:>3}：{:>2} 行 {:>4} 字 {:>5.1}pt {:<24} {:<7} 行距 {:<14} [{:.1} {:.1} {:.1} {:.1}] {}",
            i + 1,
            b.lines.len(),
            glyphs.len(),
            b.font_size,
            excerpt(&b.font_name, 24),
            alignment_name(b.alignment),
            pitch,
            b.bbox.left,
            b.bbox.bottom,
            b.bbox.right,
            b.bbox.top,
            flags.join(" ")
        );
        if with_text {
            for &li in &b.lines {
                println!(
                    "        {}",
                    excerpt(&text.text_of(&pr.layout.lines[li].glyphs), 80)
                );
            }
        }
    }
}

fn alignment_name(a: magpdf_text::Alignment) -> &'static str {
    match a {
        magpdf_text::Alignment::Left => "左对齐",
        magpdf_text::Alignment::Center => "居中",
        magpdf_text::Alignment::Right => "右对齐",
        magpdf_text::Alignment::Justify => "两端",
    }
}

/// 截到 `max` 个字符，去掉换行等控制字符。
fn excerpt(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(max).collect();
        out.push('…');
        out
    }
}
