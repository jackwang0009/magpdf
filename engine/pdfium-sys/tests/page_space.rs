//! 用户空间到显示坐标的换算（`PageGeometry::display_matrix`）对不对，拿 PDFium 自己的渲染结果来验：
//! 页面上只留一个对象渲染出来，有墨迹的像素必须落在“对象的包围盒换算到显示坐标”的范围里。
//! 换算错了（旋转方向反了、没减可见区域的原点），墨迹就会跑到范围外面去。
//!
//! 样本是旋转 90°/180°/270° 的页，以及可见区域不从原点开始的页（MediaBox 有偏移、CropBox 比
//! MediaBox 小、角点写反、负坐标）。界面用同一张换算表把对象画到画布上（方案 3.1：页面旋转和
//! 可见区域的换算由界面负责），这里打印出来的数值也是界面那边单元测试的对照数据。
// 测试代码允许 unwrap/expect（《工程规范》4）；集成测试不在 #[cfg(test)] 下，需显式放开。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use pdfium_sys::{Document, Library, RenderFlags};

/// 渲染分辨率：每 pt 多少像素。
const SCALE: f32 = 4.0;
/// 墨迹可以超出包围盒多少像素（抗锯齿、描边的半个线宽取整）。
const SLACK: f32 = 3.0;

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/synthetic")
        .join(name)
}

/// 像素范围 `(x0, y0, x1, y1)`，右下不含。
type PixelBox = (u32, u32, u32, u32);

/// 位图里不是白色的像素的范围。
fn ink_box(bitmap: &pdfium_sys::Bitmap) -> Option<PixelBox> {
    let mut found: Option<PixelBox> = None;
    for y in 0..bitmap.height() {
        for x in 0..bitmap.width() {
            let [b, g, r, _] = bitmap.pixel(x, y);
            if b > 250 && g > 250 && r > 250 {
                continue;
            }
            found = Some(match found {
                None => (x, y, x + 1, y + 1),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
            });
        }
    }
    found
}

#[test]
fn objects_land_where_the_display_matrix_says() {
    let lib = Library::init().unwrap();
    let mut checked = 0;
    for name in ["struct-page-rotate.pdf", "struct-mediabox-cropbox.pdf"] {
        let doc = Document::load_file(&lib, &sample(name), None).unwrap();
        for index in 0..doc.page_count() {
            let geometry = doc.page_geometry(index).unwrap();
            let matrix = geometry.display_matrix().expect("页面有可见区域");
            let visible = geometry.bounds.unwrap();
            println!(
                "{name} 第 {} 页：rotate {}，可见区域 [{}, {}, {}, {}]，显示 {}×{}",
                index + 1,
                geometry.rotation * 90,
                visible.left,
                visible.bottom,
                visible.right,
                visible.top,
                geometry.width,
                geometry.height
            );
            // 可见区域的四个角换算过去正好是显示范围的四个角。
            let corners = [
                matrix.apply(visible.left, visible.bottom),
                matrix.apply(visible.right, visible.top),
            ];
            let (min_x, max_x) = (
                corners[0].0.min(corners[1].0),
                corners[0].0.max(corners[1].0),
            );
            let (min_y, max_y) = (
                corners[0].1.min(corners[1].1),
                corners[0].1.max(corners[1].1),
            );
            assert!(
                min_x.abs() < 0.01
                    && min_y.abs() < 0.01
                    && (max_x - geometry.width).abs() < 0.01
                    && (max_y - geometry.height).abs() < 0.01,
                "{name} 第 {} 页：可见区域换算成了 ({min_x}, {min_y})–({max_x}, {max_y})",
                index + 1
            );

            let count = doc.page(index).unwrap().object_count();
            assert!(count > 0, "{name} 第 {} 页没有对象", index + 1);
            for z in 0..count {
                // 每次重新加载页面，把别的对象都拿掉（不生成内容流，文件本身不变），只渲染这一个。
                let page = doc.page(index).unwrap();
                let bounds = page.object(z).unwrap().bounds().expect("对象有包围盒");
                for other in (0..count).rev() {
                    if other != z {
                        page.remove_object(page.object(other).unwrap()).unwrap();
                    }
                }
                let (w, h) = (
                    (geometry.width * SCALE).round() as u32,
                    (geometry.height * SCALE).round() as u32,
                );
                let bitmap = page
                    .render_to_size(w, h, RenderFlags::NONE, 0xFFFF_FFFF)
                    .unwrap();
                let Some(ink) = ink_box(&bitmap) else {
                    panic!("{name} 第 {} 页对象 {z} 渲染出来是空白", index + 1);
                };

                let p = matrix.apply(bounds.left, bounds.bottom);
                let q = matrix.apply(bounds.right, bounds.top);
                let expected = (p.0.min(q.0), p.1.min(q.1), p.0.max(q.0), p.1.max(q.1));
                println!(
                    "  对象 {z}：用户空间 [{}, {}, {}, {}] → 显示 [{:.2}, {:.2}, {:.2}, {:.2}]；墨迹（像素） {ink:?}",
                    bounds.left,
                    bounds.bottom,
                    bounds.right,
                    bounds.top,
                    expected.0,
                    expected.1,
                    expected.2,
                    expected.3
                );
                // 对象超出可见区域的部分渲染不出来，所以拿来比的范围先裁到页面之内。
                let clipped = (
                    (expected.0 * SCALE).max(0.0),
                    (expected.1 * SCALE).max(0.0),
                    (expected.2 * SCALE).min(w as f32),
                    (expected.3 * SCALE).min(h as f32),
                );
                assert!(
                    ink.0 as f32 >= clipped.0 - SLACK
                        && ink.1 as f32 >= clipped.1 - SLACK
                        && ink.2 as f32 <= clipped.2 + SLACK
                        && ink.3 as f32 <= clipped.3 + SLACK,
                    "{name} 第 {} 页对象 {z}：墨迹 {ink:?} 超出了换算出来的范围 {clipped:?}",
                    index + 1
                );
                // 范围也不能大得离谱：墨迹至少占掉换算范围的四分之一（文字的包围盒比字形高一些）。
                let ink_area = (ink.2 - ink.0) as f32 * (ink.3 - ink.1) as f32;
                let box_area = (clipped.2 - clipped.0) * (clipped.3 - clipped.1);
                assert!(
                    ink_area >= box_area * 0.25,
                    "{name} 第 {} 页对象 {z}：墨迹 {ink:?} 比换算出来的范围 {clipped:?} 小得多",
                    index + 1
                );
                checked += 1;
            }
        }
    }
    assert!(checked >= 14, "只核对了 {checked} 个对象");
}
