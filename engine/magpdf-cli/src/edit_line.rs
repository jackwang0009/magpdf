//! `edit-line` 子命令：加载 PDF、改一行字、保存并抽查提取结果（V08 冒烟）。

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use magpdf_text::{EditRequest, LayoutParams, PageText, analyze, edit_line};
use pdfium_sys::{Document, Library, SaveFlags};

#[derive(Args, Debug)]
pub struct EditLineArgs {
    /// PDF 文件
    pub file: PathBuf,
    /// 页码，从 1 开始
    #[arg(long, default_value_t = 1)]
    pub page: usize,
    /// 行号，从 0 开始（布局分析的结果）
    #[arg(long, default_value_t = 0)]
    pub line: usize,
    /// 新的一行文字
    #[arg(long)]
    pub text: String,
    /// 输出 PDF 路径
    #[arg(short, long)]
    pub output: PathBuf,
    /// 以 JSON 打印结果
    #[arg(long)]
    pub json: bool,
}

pub fn run(lib: &Library, args: EditLineArgs) -> Result<()> {
    anyhow::ensure!(args.page >= 1, "页码从 1 开始");
    let doc = Document::load_file(lib, &args.file, None)
        .with_context(|| format!("打开 {}", args.file.display()))?;
    let req = EditRequest::line(args.page - 1, args.line, args.text.clone());
    let outcome = edit_line(&doc, &req).with_context(|| "改字失败")?;
    let bytes_before = std::fs::metadata(&args.file).map(|m| m.len() as i64).ok();
    doc.save_to_file(&args.output, SaveFlags::NO_INCREMENTAL)
        .with_context(|| format!("保存 {}", args.output.display()))?;
    let bytes_after = std::fs::metadata(&args.output).map(|m| m.len() as i64).ok();
    let bytes_delta = match (bytes_before, bytes_after) {
        (Some(before), Some(after)) => Some(after - before),
        _ => None,
    };

    let extracted = extract_line(lib, &args.output, args.page - 1, args.line).unwrap_or_default();
    let contains = extracted.contains(&args.text);
    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "tier": outcome.tier.as_str(),
                "tier_n": outcome.tier.as_u8(),
                "font_replaced": outcome.font_replaced,
                "skipped": outcome.skipped.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "output": args.output.display().to_string(),
                "extracted": extracted,
                "contains_new_text": contains,
                "bytes_before": bytes_before,
                "bytes_after": bytes_after,
                "bytes_delta": bytes_delta,
            })
        );
    } else {
        println!(
            "级别 {}{}，已保存 {}",
            outcome.tier.as_str(),
            if outcome.font_replaced {
                "（已换字体）"
            } else {
                ""
            },
            args.output.display()
        );
        for s in &outcome.skipped {
            println!("  跳过：{s}");
        }
        println!(
            "重开提取：{}（新文本{}）",
            extracted,
            if contains { "已出现" } else { "未出现" }
        );
    }
    Ok(())
}

fn extract_line(lib: &Library, path: &std::path::Path, page: usize, line: usize) -> Result<String> {
    let doc = Document::load_file(lib, path, None)?;
    let p = doc.page(page)?;
    let params = LayoutParams::default();
    let text = PageText::from_page(&p, params.ruling_max_thickness)?;
    let layout = analyze(&text, &params);
    let line = layout
        .lines
        .get(line)
        .with_context(|| format!("重开后没有第 {line} 行"))?;
    Ok(text.text_of(&line.glyphs))
}
