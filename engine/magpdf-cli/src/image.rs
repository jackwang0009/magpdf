//! PNG 输出与差异热图。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result};
use pdfium_sys::Bitmap;

pub fn write_png(bmp: &Bitmap, path: &Path) -> Result<()> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), bmp.width(), bmp.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&bmp.to_rgba())?;
    writer.finish()?;
    Ok(())
}

/// 差异热图：底图是 A 的淡化灰度；超过通道容差的像素画红色，有差异但在容差内的画黄色，
/// 便于人工归类时同时看到"判为差异的"和"接近阈值的"位置。尺寸不一致时整页标红。
pub fn heatmap(a: &Bitmap, b: &Bitmap, tolerance: u8) -> Result<Bitmap> {
    const RED: [u8; 4] = [0, 0, 255, 255];
    const YELLOW: [u8; 4] = [0, 200, 255, 255];
    let (w, h) = (a.width(), a.height());
    let same_size = (w, h) == (b.width(), b.height());
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        let ra = a.row(y);
        let rb = same_size.then(|| b.row(y));
        for x in 0..w as usize {
            let i = x * 4;
            let pa = &ra[i..i + 4];
            let d = rb.map_or(u8::MAX, |rb| {
                (0..3).map(|c| pa[c].abs_diff(rb[i + c])).max().unwrap_or(0)
            });
            let px = if d > tolerance {
                RED
            } else if d > 0 {
                YELLOW
            } else {
                let luma =
                    (u32::from(pa[2]) * 299 + u32::from(pa[1]) * 587 + u32::from(pa[0]) * 114)
                        / 1000;
                let v = u8::try_from(255 - (255 - luma) / 3).unwrap_or(255);
                [v, v, v, 255]
            };
            out.extend_from_slice(&px);
        }
    }
    Bitmap::from_bgra(w, h, w as usize * 4, out).context("构造热图位图")
}
