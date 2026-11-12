//! `perf` 子命令：引擎一侧各步各花多久（《P0 技术验证计划》V05）。
//!
//! 界面上“修改到重画完”的时间里，引擎占的那部分由这几步组成：执行修改（加载页面、改对象、重新生成
//! 内容流）、重新列一遍对象、把视口里的瓦片逐块渲染出来。这里不经过进程间通信，直接量每一步：
//!
//! - 加载页面（`FPDF_LoadPage` 会解析内容流）；
//! - 渲染一块 512×512 的瓦片，分“每块都重新加载页面”（引擎进程现在的做法）和“页面一直开着”两种；
//! - 列出一页的全部对象；
//! - 保真校验一页；
//! - 把最上面的对象挪 1 pt（一条修改命令）。
//!
//! 用发布版量：`cargo run --release -p magpdf-cli -- perf 某文件.pdf`。

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args;
use magpdf_engine::edit::Session;
use pdfium_sys::{
    Document, LayerFilter, Library, Matrix, PageSurface, PixelRect, RenderFlags, TILE_SIZE,
    render_rect, render_rect_filtered,
};
use serde::Serialize;

#[derive(Args, Debug)]
pub struct PerfArgs {
    /// PDF 文件
    pub file: PathBuf,
    /// 页码，从 1 开始
    #[arg(short, long, default_value_t = 1)]
    pub page: usize,
    /// 渲染倍数（每 pt 多少像素），可以给几个，用逗号分开。默认的两个是界面在 A4 页上按“适合页面”
    /// 显示时用到的瓦片档位：1440×900 的窗口（0.943）和 2560×1400 以上的窗口（1.886）
    #[arg(long, value_delimiter = ',', default_values_t = [0.942_809, 1.885_618])]
    pub scale: Vec<f32>,
    /// 每一项重复多少次
    #[arg(long, default_value_t = 20)]
    pub repeat: usize,
    /// 把结果写成 JSON
    #[arg(long)]
    pub json: Option<PathBuf>,
}

/// 一组耗时（毫秒）的统计。
#[derive(Debug, Clone, Copy, Serialize)]
struct Timing {
    samples: usize,
    median: f64,
    p95: f64,
    min: f64,
    max: f64,
}

impl Timing {
    /// P95 用最近秩法。没有样本时全是 0。
    fn of(mut samples: Vec<f64>) -> Self {
        samples.sort_by(f64::total_cmp);
        let n = samples.len();
        let at = |q: f64| -> f64 {
            let rank = ((q * n as f64).ceil() as usize).clamp(1, n.max(1));
            samples.get(rank - 1).copied().unwrap_or(0.0)
        };
        Self {
            samples: n,
            median: round(at(0.5)),
            p95: round(at(0.95)),
            min: round(samples.first().copied().unwrap_or(0.0)),
            max: round(samples.last().copied().unwrap_or(0.0)),
        }
    }
}

/// 保留两位小数。
fn round(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}

fn ms_since(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// 一种渲染倍数下的结果。
#[derive(Debug, Serialize)]
struct ScaleReport {
    /// 每 pt 多少像素
    scale: f32,
    /// 整页的像素尺寸
    surface: [u32; 2],
    /// 整页要几块 512×512 的瓦片
    tiles: usize,
    /// 每块瓦片都重新加载页面再渲染（引擎进程现在的做法）：单块的耗时
    tile_reloading: Timing,
    /// 同上：把整页的瓦片渲染一遍的总耗时
    page_reloading: Timing,
    /// 页面开着不关，第一遍渲染整页（图像还没有解码过）
    page_kept_first: f64,
    /// 页面开着不关：单块的耗时（第一遍之后）
    tile_kept: Timing,
    /// 同上：把整页的瓦片渲染一遍的总耗时
    page_kept: Timing,
    /// 分层渲染（方案 3.2），最上面的对象所在的那块瓦片，每次重新加载页面：底图（去掉这个对象）
    #[serde(skip_serializing_if = "Option::is_none")]
    layer_without: Option<Timing>,
    /// 同上：对象层（只有这个对象）
    #[serde(skip_serializing_if = "Option::is_none")]
    layer_only: Option<Timing>,
}

#[derive(Debug, Serialize)]
struct Report {
    pdfium_version: &'static str,
    /// 文件大小（字节）
    file_bytes: u64,
    pages: usize,
    /// 量的是哪一页，从 0 开始
    page: usize,
    objects: usize,
    repeat: usize,
    /// 读入文件并打开文档
    open: Timing,
    /// 加载页面再关掉
    page_load: Timing,
    /// 列出这一页的全部对象（含加载页面）
    object_list: Timing,
    /// 保真校验这一页（含建校验用的那份文档）
    fidelity_check: f64,
    /// 这一页能不能修改原有对象；不能时是原因
    #[serde(skip_serializing_if = "Option::is_none")]
    overlay_only: Option<String>,
    /// 把最上面的对象挪 1 pt：一条修改命令（加载页面、改对象、重新生成内容流）。页面不能修改时没有
    #[serde(skip_serializing_if = "Option::is_none")]
    edit: Option<Timing>,
    scales: Vec<ScaleReport>,
}

pub fn run(lib: &Library, args: &PerfArgs) -> Result<()> {
    anyhow::ensure!(args.page >= 1, "页码从 1 开始");
    anyhow::ensure!(args.repeat >= 1, "至少重复一次");
    anyhow::ensure!(
        args.scale.iter().all(|s| s.is_finite() && *s > 0.0),
        "渲染倍数必须是正数"
    );
    let page_index = args.page - 1;
    let repeat = args.repeat;
    let file_bytes = std::fs::metadata(&args.file)
        .with_context(|| format!("访问 {}", args.file.display()))?
        .len();

    // 打开文档：读文件 + 解析交叉引用表
    let mut open = Vec::with_capacity(repeat);
    for _ in 0..repeat {
        let start = Instant::now();
        let doc = Document::load_file(lib, &args.file, None)
            .with_context(|| format!("打开 {}", args.file.display()))?;
        std::hint::black_box(doc.page_count());
        open.push(ms_since(start));
    }

    let doc = Document::load_file(lib, &args.file, None)?;
    let pages = doc.page_count();
    anyhow::ensure!(page_index < pages, "文档只有 {pages} 页");

    // 加载页面：解析内容流，建出页面对象
    let mut page_load = Vec::with_capacity(repeat);
    for _ in 0..repeat {
        let start = Instant::now();
        let page = doc.page(page_index)?;
        std::hint::black_box(page.object_count());
        drop(page);
        page_load.push(ms_since(start));
    }
    let objects = doc.page(page_index)?.object_count();

    let mut scales = Vec::with_capacity(args.scale.len());
    for &scale in &args.scale {
        scales.push(measure_scale(&doc, page_index, scale, repeat)?);
    }
    drop(doc);

    // 下面几项走编辑会话，和引擎进程用的是同一套代码
    let mut session = Session::open(lib, &args.file, None)?;
    let mut object_list = Vec::with_capacity(repeat);
    let mut top = None;
    for _ in 0..repeat {
        let start = Instant::now();
        let listed = session.objects(page_index)?;
        object_list.push(ms_since(start));
        top = listed.last().map(|object| object.id);
    }

    let start = Instant::now();
    let verdict = session.check_fidelity(page_index)?;
    let fidelity_check = round(ms_since(start));

    let mut edit = None;
    if verdict.is_none()
        && let Some(id) = top
    {
        let mut samples = Vec::with_capacity(repeat * 2);
        for _ in 0..repeat {
            for shift in [1.0, -1.0] {
                let start = Instant::now();
                session.transform(&[id], Matrix::translation(shift, 0.0))?;
                samples.push(ms_since(start));
            }
        }
        edit = Some(Timing::of(samples));
    }

    let report = Report {
        pdfium_version: Library::linked_version(),
        file_bytes,
        pages,
        page: page_index,
        objects,
        repeat,
        open: Timing::of(open),
        page_load: Timing::of(page_load),
        object_list: Timing::of(object_list),
        fidelity_check,
        overlay_only: verdict.map(str::to_owned),
        edit,
        scales,
    };
    print(&report);
    if let Some(out) = &args.json {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(out, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("写入 {}", out.display()))?;
        println!("JSON 结果：{}", out.display());
    }
    Ok(())
}

/// 整页在这个倍数下的瓦片（左上角的像素坐标），按行排。
fn tile_origins(surface: PageSurface) -> Vec<(u32, u32)> {
    let mut origins = Vec::new();
    let mut y = 0;
    while y < surface.height {
        let mut x = 0;
        while x < surface.width {
            origins.push((x, y));
            x += TILE_SIZE;
        }
        y += TILE_SIZE;
    }
    origins
}

const PAPER: u32 = 0xFFFF_FFFF;
const CLEAR: u32 = 0;

fn measure_scale(
    doc: &Document<'_>,
    page_index: usize,
    scale: f32,
    repeat: usize,
) -> Result<ScaleReport> {
    let surface = PageSurface::new(&doc.page(page_index)?, scale);
    let origins = tile_origins(surface);

    // 引擎进程现在的做法：每块瓦片打开页面、渲染、关掉
    let mut tile_reloading = Vec::with_capacity(repeat * origins.len());
    let mut page_reloading = Vec::with_capacity(repeat);
    for _ in 0..repeat {
        let pass = Instant::now();
        for &(x, y) in &origins {
            let start = Instant::now();
            let page = doc.page(page_index)?;
            let bitmap = render_rect(
                &page,
                surface,
                PixelRect::tile(x, y),
                RenderFlags::NONE,
                PAPER,
            )?;
            std::hint::black_box(bitmap.bgra().len());
            drop(page);
            tile_reloading.push(ms_since(start));
        }
        page_reloading.push(ms_since(pass));
    }

    // 页面开着不关
    let page = doc.page(page_index)?;
    let mut tile_kept = Vec::with_capacity(repeat * origins.len());
    let mut page_kept = Vec::with_capacity(repeat);
    let mut page_kept_first = 0.0;
    for round_index in 0..=repeat {
        let pass = Instant::now();
        for &(x, y) in &origins {
            let start = Instant::now();
            let bitmap = render_rect(
                &page,
                surface,
                PixelRect::tile(x, y),
                RenderFlags::NONE,
                PAPER,
            )?;
            std::hint::black_box(bitmap.bgra().len());
            if round_index > 0 {
                tile_kept.push(ms_since(start));
            }
        }
        if round_index == 0 {
            page_kept_first = round(ms_since(pass));
        } else {
            page_kept.push(ms_since(pass));
        }
    }

    // 分层渲染：最上面的对象所在的那块瓦片
    let count = page.object_count();
    let target = count.checked_sub(1).and_then(|z| {
        let bounds = page.object(z)?.bounds()?;
        // 包围盒中心在整页像素网格上的位置（PDF 的 y 轴向上）
        let cx = (bounds.left + bounds.right) / 2.0 * scale;
        let cy = (page.height() - (bounds.bottom + bounds.top) / 2.0) * scale;
        let inside =
            cx >= 0.0 && cy >= 0.0 && cx < surface.width as f32 && cy < surface.height as f32;
        inside.then(|| {
            (
                z,
                (
                    cx as u32 / TILE_SIZE * TILE_SIZE,
                    cy as u32 / TILE_SIZE * TILE_SIZE,
                ),
            )
        })
    });
    drop(page);
    let mut layers = None;
    if let Some((z, (x, y))) = target {
        let mut without = Vec::with_capacity(repeat);
        let mut only = Vec::with_capacity(repeat);
        for _ in 0..repeat {
            for (samples, filter, background) in [
                (&mut without, LayerFilter::Without(&[z]), PAPER),
                (&mut only, LayerFilter::Only(&[z]), CLEAR),
            ] {
                let start = Instant::now();
                let page = doc.page(page_index)?;
                let bitmap = render_rect_filtered(
                    &page,
                    surface,
                    PixelRect::tile(x, y),
                    RenderFlags::NONE,
                    background,
                    filter,
                )?;
                std::hint::black_box(bitmap.bgra().len());
                drop(page);
                samples.push(ms_since(start));
            }
        }
        layers = Some((Timing::of(without), Timing::of(only)));
    }

    Ok(ScaleReport {
        scale,
        surface: [surface.width, surface.height],
        tiles: origins.len(),
        tile_reloading: Timing::of(tile_reloading),
        page_reloading: Timing::of(page_reloading),
        page_kept_first,
        tile_kept: Timing::of(tile_kept),
        page_kept: Timing::of(page_kept),
        layer_without: layers.map(|(without, _)| without),
        layer_only: layers.map(|(_, only)| only),
    })
}

fn print(report: &Report) {
    let show = |timing: &Timing| {
        format!(
            "中位数 {:.2}  P95 {:.2}  最大 {:.2} ms",
            timing.median, timing.p95, timing.max
        )
    };
    println!(
        "PDFium {}；文件 {:.1} MB，{} 页；第 {} 页有 {} 个对象；每项重复 {} 次",
        report.pdfium_version,
        report.file_bytes as f64 / 1e6,
        report.pages,
        report.page + 1,
        report.objects,
        report.repeat
    );
    println!("打开文档        {}", show(&report.open));
    println!("加载页面        {}", show(&report.page_load));
    println!("列出全部对象    {}", show(&report.object_list));
    println!(
        "保真校验        {:.2} ms（第一次，含建校验用的文档）",
        report.fidelity_check
    );
    match (&report.edit, &report.overlay_only) {
        (Some(edit), _) => println!("挪一个对象      {}", show(edit)),
        (None, Some(reason)) => println!("挪一个对象      这一页只能叠加内容（{reason}），没有量"),
        (None, None) => println!("挪一个对象      页面上没有对象，没有量"),
    }
    for scale in &report.scales {
        println!(
            "每 pt {:.3} 像素：整页 {}×{} 像素，{} 块瓦片",
            scale.scale, scale.surface[0], scale.surface[1], scale.tiles
        );
        println!("  每块都重新加载页面  单块 {}", show(&scale.tile_reloading));
        println!("                      整页 {}", show(&scale.page_reloading));
        println!("  页面开着不关        单块 {}", show(&scale.tile_kept));
        println!(
            "                      整页 {}（第一遍 {:.2} ms）",
            show(&scale.page_kept),
            scale.page_kept_first
        );
        if let (Some(without), Some(only)) = (&scale.layer_without, &scale.layer_only) {
            println!("  分层：底图一块      {}", show(without));
            println!("  分层：对象层一块    {}", show(only));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_uses_the_nearest_rank() {
        let timing = Timing::of((1..=20).map(f64::from).collect());
        assert_eq!(timing.samples, 20);
        assert_eq!(timing.median, 10.0);
        assert_eq!(timing.p95, 19.0);
        assert_eq!((timing.min, timing.max), (1.0, 20.0));

        let single = Timing::of(vec![3.456]);
        assert_eq!((single.median, single.p95, single.max), (3.46, 3.46, 3.46));

        let empty = Timing::of(Vec::new());
        assert_eq!((empty.samples, empty.median, empty.p95), (0, 0.0, 0.0));
    }

    #[test]
    fn tiles_cover_the_page_row_by_row() {
        let exact = PageSurface {
            width: 1024,
            height: 512,
        };
        assert_eq!(tile_origins(exact), vec![(0, 0), (512, 0)]);
        let ragged = PageSurface {
            width: 600,
            height: 513,
        };
        assert_eq!(
            tile_origins(ragged),
            vec![(0, 0), (512, 0), (0, 512), (512, 512)]
        );
    }
}
