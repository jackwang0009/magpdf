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

    // 从整页的 (50, 100) 起取 100×100：红块（设备坐标 x 0–99、y 100–199）落在这一块的左半边。
    let part = render_rect(
        &page,
        surface,
        PixelRect {
            x: 50,
            y: 100,
            width: 100,
            height: 100,
        },
        RenderFlags::NONE,
        0xFFFF_FFFF,
    )
    .unwrap();
    assert_eq!((part.width(), part.height()), (100, 100));
    assert!(is_red(part.pixel(5, 5)), "{:?}", part.pixel(5, 5));
    assert!(is_red(part.pixel(45, 95)), "{:?}", part.pixel(45, 95));
    assert_eq!(part.pixel(55, 5), [255, 255, 255, 255]);
    assert_eq!(part.pixel(95, 95), [255, 255, 255, 255]);
}

/// 旋转 90° 的页（MediaBox 300×200）显示出来是 200×300。瓦片要按显示方向取，
/// 和整页渲染的同一块区域一致；页面尺寸不加载页面也能问到。
#[test]
fn tiles_of_a_rotated_page_follow_the_display_orientation() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().unwrap();
    let doc = Document::load_file(&lib, &sample("struct-page-rotate.pdf"), None).unwrap();
    assert_eq!(doc.page_size(0).unwrap(), (200.0, 300.0));
    assert_eq!(doc.page_size(1).unwrap(), (300.0, 200.0));
    assert!(doc.page_size(3).is_err());

    let page = doc.page(0).unwrap();
    assert_eq!(page.rotation(), 1);
    let surface = PageSurface::new(&page, 1.0);
    assert_eq!((surface.width, surface.height), (200, 300));

    let full = page
        .render_to_size(
            surface.width,
            surface.height,
            RenderFlags::NONE,
            0xFFFF_FFFF,
        )
        .unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: surface.width,
        height: surface.height,
    };
    let tile = render_rect(&page, surface, rect, RenderFlags::NONE, 0xFFFF_FFFF).unwrap();
    let mut same = 0usize;
    let mut inked = 0usize;
    for y in 0..surface.height {
        for x in 0..surface.width {
            let px = full.pixel(x, y);
            if tile.pixel(x, y) == px {
                same += 1;
            }
            if px != [255, 255, 255, 255] {
                inked += 1;
            }
        }
    }
    let total = (surface.width * surface.height) as usize;
    assert!(inked > 100, "整页渲染里应该有内容，非白像素 {inked} 个");
    assert!(
        same as f64 / total as f64 > 0.999,
        "一致像素 {same}/{total}"
    );
}

/// 不在左上角的瓦片：整页渲染后切成 512 见方的格子，每一格都要和整页渲染里的同一块一致，
/// 整页之外的部分是背景色。只测 (0, 0) 那一格看不出平移方向反了。
/// 缩放取一个整数倍和一个界面实际会用的档位（√2 的幂乘 96/72）。
///
/// 和整页渲染相比，有两种已知的差异：
///
/// 1. 半透明内容的边缘上，个别像素差 1 个色阶。整页挪动之后浮点误差不同，看不出来，
///    这里把相差不超过 [`NOISE`] 的像素当作一致。
/// 2. 跨在瓦片左边界上的字形，在右边那块瓦片里横向错位不到 1 像素。原因在 PDFium：彩色位图上的
///    文字按 1/3 像素定位，`CFX_RenderDevice::DrawNormalText` 里
///    `x_subpixel = static_cast<int>(device_origin.x * 3) % 3` 对负的原点（字形从位图左边缘之外开始）
///    是向零截断再取余，得到 0、-1、-2，和 `floor` 出来的整数位置对不上。这种差异只允许出现在
///    左边缘往里 [`LEFT_EDGE_BAND`] 像素之内，而且每块瓦片不超过 0.5% 的像素；最左一列的瓦片
///    没有负的原点，不允许有。
///
/// 别处出现差异就是新问题。PDFium 分支修掉第 2 条之后，这条用例照样能过。
#[test]
fn every_tile_of_the_grid_matches_its_part_of_the_full_render() {
    /// 样本里跨边界的字形（7–12 pt，2 倍缩放以内）在瓦片里最多占这么宽。
    const LEFT_EDGE_BAND: u32 = 16;
    const NOISE: u8 = 2;
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().unwrap();
    let doc = Document::load_file(&lib, &sample("freight-bl-composite.pdf"), None).unwrap();
    let page = doc.page(0).unwrap();

    for scale in [2.0_f32, 1.885_618] {
        let surface = PageSurface::new(&page, scale);
        assert!(surface.width > TILE_SIZE && surface.height > TILE_SIZE);
        let full = page
            .render_to_size(
                surface.width,
                surface.height,
                RenderFlags::NONE,
                0xFFFF_FFFF,
            )
            .unwrap();

        let mut inked_tiles = 0;
        for ty in 0..surface.height.div_ceil(TILE_SIZE) {
            for tx in 0..surface.width.div_ceil(TILE_SIZE) {
                let (x0, y0) = (tx * TILE_SIZE, ty * TILE_SIZE);
                let at = format!("缩放 {scale}，瓦片 ({x0}, {y0})");
                let tile = render_rect(
                    &page,
                    surface,
                    PixelRect::tile(x0, y0),
                    RenderFlags::NONE,
                    0xFFFF_FFFF,
                )
                .unwrap();
                let (mut inside, mut inked) = (0usize, 0usize);
                let (mut differs, mut differs_past_band) = (0usize, 0usize);
                for y in 0..TILE_SIZE {
                    for x in 0..TILE_SIZE {
                        let px = tile.pixel(x, y);
                        if x0 + x >= surface.width || y0 + y >= surface.height {
                            assert_eq!(px, WHITE, "{at}：整页之外的 ({x}, {y}) 应该是背景色");
                            continue;
                        }
                        let want = full.pixel(x0 + x, y0 + y);
                        inside += 1;
                        inked += usize::from(want != WHITE);
                        if (0..4).any(|c| px[c].abs_diff(want[c]) > NOISE) {
                            differs += 1;
                            differs_past_band += usize::from(x >= LEFT_EDGE_BAND);
                        }
                    }
                }
                assert!(
                    (differs as f64) < inside as f64 * 0.005,
                    "{at}：{differs}/{inside} 个像素和整页渲染不一致（其中非白 {inked}）"
                );
                assert_eq!(
                    differs_past_band, 0,
                    "{at}：左边缘 {LEFT_EDGE_BAND} 像素之外不该有差异"
                );
                if x0 == 0 {
                    assert_eq!(differs, 0, "{at}：最左一列的瓦片应该和整页渲染一致");
                }
                inked_tiles += usize::from(inked > 100);
            }
        }
        assert!(
            inked_tiles >= 4,
            "缩放 {scale}：有内容的瓦片只有 {inked_tiles} 块，样本不够说明问题"
        );
    }
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
