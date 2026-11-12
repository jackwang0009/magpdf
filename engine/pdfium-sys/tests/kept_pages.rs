//! 一直开着的页面（`Document::keep_page`，方案第 7 节）：渲染、列对象、修改都用同一份页面句柄，
//! 不用每次重新解析内容流。
//! 同一进程只能有一个 Library，所以用互斥锁把用到 PDFium 的测试串行化。
// 测试代码允许 unwrap/expect（《工程规范》4）；集成测试不在 #[cfg(test)] 下，需显式放开。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use pdfium_sys::{
    Document, LayerFilter, Library, Page, PageObject, PageSurface, PixelRect, RenderFlags, Rgba,
    SaveFlags, render_rect_filtered,
};

mod common;

static SERIAL: Mutex<()> = Mutex::new(());

/// 在页面最上面加一个 20×20 的方块，不生成内容流。
fn add_square(page: &Page<'_>, x: f32) {
    let path = PageObject::new_path(x, 10.0).expect("path");
    path.line_to(x + 20.0, 10.0).unwrap();
    path.line_to(x + 20.0, 30.0).unwrap();
    path.line_to(x, 30.0).unwrap();
    path.close_path().unwrap();
    path.set_fill_color(Rgba {
        r: 200,
        g: 30,
        b: 30,
        a: 255,
    })
    .unwrap();
    path.set_path_fill(true, false).unwrap();
    page.insert_object_at(path, page.object_count()).unwrap();
}

fn blank(lib: &Library, pages: usize) -> Document<'_> {
    let doc = Document::create(lib).expect("create");
    for index in 0..pages {
        doc.add_blank_page(index, 200.0, 100.0).expect("page");
    }
    doc
}

#[test]
fn a_kept_page_is_one_handle_until_released() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().expect("init pdfium");
    let mut doc = blank(&lib, 2);
    assert!(doc.kept_pages().is_empty());
    {
        let kept = doc.keep_page(0).expect("keep");
        add_square(&kept, 10.0);
        // `page` 给的是同一份：还没生成内容流的修改也看得到
        assert_eq!(doc.page(0).unwrap().object_count(), 1);
        // 另开的一份从内容流解析，还没有这个对象
        assert_eq!(doc.load_page(0).unwrap().object_count(), 0);
        kept.generate_content().unwrap();
        assert_eq!(doc.load_page(0).unwrap().object_count(), 1);
    }
    // 返回值丢掉了，页面还开着；没开着的页每次都是新加载的
    assert_eq!(doc.kept_pages(), vec![0]);
    add_square(&doc.page(1).unwrap(), 10.0);
    assert_eq!(doc.page(1).unwrap().object_count(), 0);
    add_square(&doc.page(0).unwrap(), 40.0);
    assert_eq!(doc.page(0).unwrap().object_count(), 2);

    // 关掉之后重新加载：没生成内容流的那个对象跟着没了
    doc.release_pages(0);
    assert!(doc.kept_pages().is_empty());
    assert_eq!(doc.page(0).unwrap().object_count(), 1);
}

#[test]
fn release_keeps_the_most_recently_used_pages() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().expect("init pdfium");
    let mut doc = blank(&lib, 4);
    for index in [0, 1, 2] {
        doc.keep_page(index).unwrap();
    }
    // 第 0 页又用了一次，最久没用的是第 1 页
    doc.page(0).unwrap();
    doc.release_pages(2);
    assert_eq!(doc.kept_pages(), vec![0, 2]);
    doc.release_pages(5);
    assert_eq!(doc.kept_pages(), vec![0, 2]);
}

#[test]
fn inserting_a_page_shifts_the_kept_pages() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().expect("init pdfium");
    let doc = blank(&lib, 2);
    add_square(&doc.keep_page(1).unwrap(), 10.0);
    doc.add_blank_page(0, 300.0, 100.0).unwrap();
    assert_eq!(doc.kept_pages(), vec![2]);
    // 原来的第 2 页（现在是第 3 页）还是那一份，没生成内容流的对象还在
    assert_eq!(doc.page(2).unwrap().object_count(), 1);
    assert_eq!(doc.page(0).unwrap().width(), 300.0);
    doc.add_blank_page(3, 300.0, 100.0).unwrap();
    assert_eq!(doc.kept_pages(), vec![2]);
}

/// 引擎的用法：页面一直开着，分层渲染另开一份句柄（隐藏对象会给对象打上“已修改”的标记，见
/// `render_rect_filtered` 的说明）。画完再在一直开着的那一份上生成内容流，内容流一个字节也不变。
#[test]
fn a_layer_rendered_on_a_separate_handle_leaves_the_kept_page_alone() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().expect("init pdfium");
    let original = {
        let doc = blank(&lib, 1);
        let page = doc.page(0).unwrap();
        add_square(&page, 10.0);
        add_square(&page, 40.0);
        page.generate_content().unwrap();
        drop(page);
        doc.save_to_vec(SaveFlags::NO_INCREMENTAL).unwrap()
    };
    let streams = |doc: &Document<'_>| {
        common::saved_streams(&doc.save_to_vec(SaveFlags::NO_INCREMENTAL).unwrap())
    };
    let doc = Document::load_bytes(&lib, original, None).unwrap();
    let before = streams(&doc);
    let kept = doc.keep_page(0).unwrap();
    {
        let separate = doc.load_page(0).unwrap();
        let surface = PageSurface::new(&separate, 1.0);
        render_rect_filtered(
            &separate,
            surface,
            PixelRect::tile(0, 0),
            RenderFlags::NONE,
            0,
            LayerFilter::Without(&[1]),
        )
        .unwrap();
    }
    kept.generate_content().unwrap();
    drop(kept);
    assert_eq!(streams(&doc), before);
}
