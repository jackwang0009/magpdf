//! 瓦片渲染（V06）：只渲染页面上的一个设备像素矩形，尺寸固定为请求大小，超出整页的部分填背景色。
// 测试代码允许 unwrap/expect（《工程规范》4）；集成测试不在 #[cfg(test)] 下，需显式放开。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Mutex;

use pdfium_sys::{Document, Library, PageSurface, PixelRect, RenderFlags, TILE_SIZE, render_rect};

static SERIAL: Mutex<()> = Mutex::new(());

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/synthetic")
        .join(name)
}

/// 页面 100×100 点、左下角铺一块 50×50 的红色，用来核对瓦片的取景位置。
const RED_PAGE: &str = "%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << >> >> endobj\n\
4 0 obj << /Length 29 >> stream\n1 0 0 rg 0 0 50 50 re f\nendstream\nendobj\n\
trailer << /Root 1 0 R /Size 5 >>\n%%EOF\n";

/// 取 `(x, y)` 处是否偏红。
fn is_red(px: [u8; 4]) -> bool {
    px[2] > 200 && px[1] < 60 && px[0] < 60
}

#[test]
fn tile_keeps_requested_size_and_fills_outside_with_background() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().unwrap();
    let doc = Document::load_bytes(&lib, RED_PAGE.as_bytes().to_vec(), None).unwrap();
    let page = doc.page(0).unwrap();
    // 缩放 2 倍：整页 200×200 像素。
    let surface = PageSurface::new(&page, 2.0);
    assert_eq!((surface.width, surface.height), (200, 200));

    // 整页之外的瓦片全是背景色。
    let tile = render_rect(
        &page,
        surface,
        PixelRect::tile(512, 512),
        RenderFlags::NONE,
        0xFFFF_FFFF,
    )
    .unwrap();
    assert_eq!((tile.width(), tile.height()), (TILE_SIZE, TILE_SIZE));
    assert_eq!(tile.pixel(10, 10), [255, 255, 255, 255]);

    // (0, 0) 起的瓦片：页面在左上 200×200 像素，红块在设备坐标 (0, 100)–(99, 199)。
    // 注：start_x/start_y 只决定绘制的平移量，PDFium 不裁剪到矩形，所以瓦片必须正好对齐到整页网格
    // （见下一条测试），这里只核对完整瓦片的取景。
    let tile = render_rect(
        &page,
        surface,
        PixelRect::tile(0, 0),
        RenderFlags::NONE,
        0xFFFF_FFFF,
    )
    .unwrap();
    assert_eq!(tile.pixel(10, 10), [255, 255, 255, 255]);
    assert!(is_red(tile.pixel(10, 150)), "{:?}", tile.pixel(10, 150));
    assert!(is_red(tile.pixel(90, 199)), "{:?}", tile.pixel(90, 199));
    assert_eq!(tile.pixel(10, 250), [255, 255, 255, 255]);
    assert_eq!(tile.pixel(250, 150), [255, 255, 255, 255]);
}

#[test]
fn tiles_of_a_real_page_match_the_matching_part_of_the_full_render() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().unwrap();
    let doc = Document::load_file(&lib, &sample("freight-bl-composite.pdf"), None).unwrap();
    let page = doc.page(0).unwrap();
    let surface = PageSurface::new(&page, 1.0);
    let full = page
        .render_to_size(
            surface.width,
            surface.height,
            RenderFlags::NONE,
            0xFFFF_FFFF,
        )
        .unwrap();
    let (w, h) = (surface.width.min(TILE_SIZE), surface.height.min(TILE_SIZE));
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let tile = render_rect(&page, surface, rect, RenderFlags::NONE, 0xFFFF_FFFF).unwrap();
    let mut same = 0usize;
    let total = (w * h) as usize;
    for y in 0..h {
        for x in 0..w {
            if tile.pixel(x, y) == full.pixel(x, y) {
                same += 1;
            }
        }
    }
    // 两条渲染路径的采样网格一致，允许极少数抗锯齿像素因取整不同而不同。
    assert!(
        same as f64 / total as f64 > 0.999,
        "一致像素 {same}/{total}"
    );
}
