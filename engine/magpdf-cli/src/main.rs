//! `magpdf-cli`：引擎的命令行入口，供 CI 与开发期使用（方案 12、13.2）。
//! P0 期间在这里增加 `roundtrip`（V02）、`edit-test`（13.2 第 2 项）等子命令。
#![forbid(unsafe_code)]

mod image;
mod ocr;
mod roundtrip;
mod edit_line;
mod text_blocks;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pdfium_sys::{Document, Library, ObjectKind, RenderFlags, RenderOptions};

use crate::image::write_png;

#[derive(Parser, Debug)]
#[command(name = "magpdf-cli", version, about = "MagPDF 引擎命令行")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 打印文档信息：页数、版本、每页尺寸与对象统计
    Info {
        /// PDF 文件
        file: PathBuf,
        /// 以 JSON 输出
        #[arg(long)]
        json: bool,
    },
    /// 渲染一页为 PNG
    Render {
        /// PDF 文件
        file: PathBuf,
        /// 页码，从 1 开始
        #[arg(short, long, default_value_t = 1)]
        page: usize,
        /// 分辨率（dpi）
        #[arg(long, default_value_t = 144.0)]
        dpi: f32,
        /// 同时渲染批注外观
        #[arg(long)]
        annots: bool,
        /// 输出 PNG 路径
        #[arg(short, long)]
        output: PathBuf,
    },
    /// 往返测试：每页对象全部标记为已修改、重新生成内容流并保存，渲染比对原件与保存结果（V02）
    Roundtrip(roundtrip::RoundtripArgs),
    /// `roundtrip --isolate` 的子进程入口：处理单个文件并把 JSON 结果写到标准输出
    #[command(hide = true)]
    RoundtripWorker(roundtrip::WorkerArgs),
    /// 改一行字（V08）：三级回退后保存并抽查提取
    EditLine(edit_line::EditLineArgs),
    /// 文字行/块聚类（V07）：统计、JSON、与标注比对
    TextBlocks(text_blocks::Args),
    /// 对图片、图片目录或 PDF 页做 OCR（V09）
    Ocr(ocr::OcrArgs),
    /// 用 PPOCRLabel 标注评测 OCR 结果：字符准确率、文字行召回率（V09）
    OcrEval(ocr::OcrEvalArgs),
}

#[derive(Debug, serde::Serialize)]
struct PageInfo {
    index: usize,
    width: f32,
    height: f32,
    rotation: i32,
    objects: usize,
    text: usize,
    path: usize,
    image: usize,
    shading: usize,
    form: usize,
    chars: usize,
}

#[derive(Debug, serde::Serialize)]
struct DocInfo {
    file: String,
    pdfium_version: &'static str,
    pdf_version: Option<i32>,
    pages: usize,
    page_info: Vec<PageInfo>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    let lib = Library::init()?;
    match cli.command {
        Command::Info { file, json } => info(&lib, &file, json),
        Command::Render {
            file,
            page,
            dpi,
            annots,
            output,
        } => render(&lib, &file, page, dpi, annots, &output),
        Command::Roundtrip(args) => roundtrip::run(&lib, &args),
        Command::RoundtripWorker(args) => roundtrip::run_worker(&lib, &args),
        Command::EditLine(args) => edit_line::run(&lib, args),
        Command::TextBlocks(args) => text_blocks::run(&lib, args),
        Command::Ocr(args) => ocr::ocr(&lib, args),
        Command::OcrEval(args) => ocr::ocr_eval(args),
    }
}

fn info(lib: &Library, file: &Path, json: bool) -> Result<()> {
    let doc =
        Document::load_file(lib, file, None).with_context(|| format!("打开 {}", file.display()))?;
    let mut page_info = Vec::with_capacity(doc.page_count());
    for index in 0..doc.page_count() {
        let page = doc.page(index)?;
        let mut pi = PageInfo {
            index,
            width: page.width(),
            height: page.height(),
            rotation: page.rotation(),
            objects: page.object_count(),
            text: 0,
            path: 0,
            image: 0,
            shading: 0,
            form: 0,
            chars: page.text().map(|t| t.char_count()).unwrap_or(0),
        };
        for obj in page.objects() {
            match obj.kind() {
                ObjectKind::Text => pi.text += 1,
                ObjectKind::Path => pi.path += 1,
                ObjectKind::Image => pi.image += 1,
                ObjectKind::Shading => pi.shading += 1,
                ObjectKind::Form => pi.form += 1,
                ObjectKind::Unknown => {}
            }
        }
        page_info.push(pi);
    }
    let out = DocInfo {
        file: file.display().to_string(),
        pdfium_version: Library::linked_version(),
        pdf_version: doc.file_version(),
        pages: doc.page_count(),
        page_info,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("文件：{}", out.file);
        println!(
            "PDFium：{}  PDF 版本：{}",
            out.pdfium_version,
            out.pdf_version
                .map_or("?".to_owned(), |v| format!("{}.{}", v / 10, v % 10))
        );
        println!("页数：{}", out.pages);
        for p in &out.page_info {
            println!(
                "  第 {} 页 {:.1}×{:.1} pt 旋转 {}°：对象 {}（文字 {} 路径 {} 图像 {} 渐变 {} Form {}），字符 {}",
                p.index + 1,
                p.width,
                p.height,
                p.rotation * 90,
                p.objects,
                p.text,
                p.path,
                p.image,
                p.shading,
                p.form,
                p.chars
            );
        }
    }
    Ok(())
}

fn render(
    lib: &Library,
    file: &Path,
    page_no: usize,
    dpi: f32,
    annots: bool,
    output: &Path,
) -> Result<()> {
    anyhow::ensure!(page_no >= 1, "页码从 1 开始");
    let doc =
        Document::load_file(lib, file, None).with_context(|| format!("打开 {}", file.display()))?;
    let page = doc.page(page_no - 1)?;
    let flags = if annots {
        RenderFlags::ANNOT
    } else {
        RenderFlags::NONE
    };
    let bmp = page.render(RenderOptions::dpi(dpi).with_flags(flags))?;
    write_png(&bmp, output).with_context(|| format!("写入 {}", output.display()))?;
    println!(
        "{} 第 {} 页 → {}（{}×{}）",
        file.display(),
        page_no,
        output.display(),
        bmp.width(),
        bmp.height()
    );
    Ok(())
}
