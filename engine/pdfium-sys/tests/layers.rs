//! 分层渲染（拖动预览，方案 3.2）：只画页面上的一部分对象。
//! 同一进程只能有一个 Library，所以用互斥锁把用到 PDFium 的测试串行化。
// 测试代码允许 unwrap/expect（《工程规范》4）；集成测试不在 #[cfg(test)] 下，需显式放开。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use pdfium_sys::{
    Bitmap, Document, Error, LayerFilter, Library, Page, PageSurface, PixelRect, RenderFlags,
    render_rect, render_rect_filtered,
};

mod common;

static SERIAL: Mutex<()> = Mutex::new(());

const WHITE: u32 = 0xFFFF_FFFF;
const CLEAR: u32 = 0x0000_0000;

/// [`two_squares`] 那一页的内容流。
const CONTENT: &str = "/Opaque gs 0 0 1 rg 20 20 60 60 re f\n/Half gs 1 0 0 rg 50 50 60 40 re f";

/// 一页 200×100 pt，两个方块，从下到上：
///
/// - 0：不透明的蓝色，x 20–80、y 20–80；
/// - 1：一半透明的红色，x 50–110、y 50–90，压着蓝色方块的右上角。
///
/// 位图自顶向下，PDF 里的 y 对应第 `100 − y` 行。
fn two_squares() -> Vec<u8> {
    let content = CONTENT;
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources \
         << /ExtGState << /Opaque << /ca 1 >> /Half << /ca 0.5 >> >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// 四个有代表性的位置（像素坐标）：只有蓝色、蓝红重叠、只有红色、空白。
const BLUE_ONLY: (u32, u32) = (30, 60);
const OVERLAP: (u32, u32) = (65, 35);
const RED_ONLY: (u32, u32) = (95, 30);
const EMPTY: (u32, u32) = (150, 80);

fn with_page(f: impl FnOnce(&Page<'_>)) {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().expect("init pdfium");
    let doc = Document::load_bytes(&lib, two_squares(), None).expect("load");
    let page = doc.page(0).expect("page");
    assert_eq!(page.object_count(), 2);
    f(&page);
}

/// 整页按 1 倍渲染成一张图。
fn whole(page: &Page<'_>, background: u32, filter: Option<LayerFilter<'_>>) -> Bitmap {
    let surface = PageSurface::new(page, 1.0);
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: 200,
        height: 100,
    };
    match filter {
        None => render_rect(page, surface, rect, RenderFlags::NONE, background),
        Some(filter) => {
            render_rect_filtered(page, surface, rect, RenderFlags::NONE, background, filter)
        }
    }
    .expect("render")
}

fn at(bitmap: &Bitmap, (x, y): (u32, u32)) -> [u8; 4] {
    bitmap.pixel(x, y)
}

/// 两个颜色（BGRA）每个分量相差不超过 `tolerance`。
fn close(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tolerance)
}

const BLUE: [u8; 4] = [255, 0, 0, 255];
const PAPER: [u8; 4] = [255, 255, 255, 255];
const NOTHING: [u8; 4] = [0, 0, 0, 0];

#[test]
fn the_base_layer_leaves_out_the_listed_objects() {
    with_page(|page| {
        // 去掉红色方块：重叠处露出完整的蓝色，红色自己的地方是白纸
        let without_red = whole(page, WHITE, Some(LayerFilter::Without(&[1])));
        assert_eq!(at(&without_red, BLUE_ONLY), BLUE);
        assert_eq!(at(&without_red, OVERLAP), BLUE);
        assert_eq!(at(&without_red, RED_ONLY), PAPER);
        assert_eq!(at(&without_red, EMPTY), PAPER);

        // 去掉蓝色方块：蓝色的地方是白纸；红色一半透明，和白纸混成浅红
        let without_blue = whole(page, WHITE, Some(LayerFilter::Without(&[0])));
        assert_eq!(at(&without_blue, BLUE_ONLY), PAPER);
        let pink = [128, 128, 255, 255];
        assert!(
            close(at(&without_blue, RED_ONLY), pink, 1),
            "{:?}",
            at(&without_blue, RED_ONLY)
        );
        assert!(close(at(&without_blue, OVERLAP), pink, 1));

        // 什么都不去掉，就是平常的渲染
        let plain = whole(page, WHITE, None);
        assert_eq!(
            whole(page, WHITE, Some(LayerFilter::Without(&[]))).bgra(),
            plain.bgra()
        );
    });
}

#[test]
fn the_object_layer_is_transparent_except_for_the_listed_objects() {
    with_page(|page| {
        let blue = whole(page, CLEAR, Some(LayerFilter::Only(&[0])));
        assert_eq!(at(&blue, BLUE_ONLY), BLUE);
        // 压在它上面的红色方块不在这一层里：重叠处是完整的蓝色
        assert_eq!(at(&blue, OVERLAP), BLUE);
        assert_eq!(at(&blue, RED_ONLY), NOTHING);
        assert_eq!(at(&blue, EMPTY), NOTHING);

        // 一半透明的对象：颜色分量是对象本来的颜色（没有乘上透明度），透明度在第四个字节里。
        // 界面合成的时候要自己预乘。
        let red = whole(page, CLEAR, Some(LayerFilter::Only(&[1])));
        let half_red = at(&red, RED_ONLY);
        assert!(close(half_red, [0, 0, 255, 128], 1), "{half_red:?}");
        assert!(close(at(&red, OVERLAP), [0, 0, 255, 128], 1));
        assert_eq!(at(&red, BLUE_ONLY), NOTHING);

        // 一个都不留：整张都是透明的
        let none = whole(page, CLEAR, Some(LayerFilter::Only(&[])));
        assert!(none.bgra().iter().all(|&byte| byte == 0));

        // 两个都留：和平常的渲染画在透明背景上一样
        assert_eq!(
            whole(page, CLEAR, Some(LayerFilter::Only(&[1, 0]))).bgra(),
            whole(page, CLEAR, None).bgra()
        );
    });
}

/// 分层渲染靠暂时把对象设成“不活动”，PDFium 会因此把对象标成“已修改”。这个标记只在页面句柄里：
/// 页面关掉就没有了，文档不受影响；但在同一个页面句柄上接着重新生成内容流，标记就会起作用。
#[test]
fn a_layer_render_leaves_no_trace_once_the_page_is_closed() {
    use pdfium_sys::SaveFlags;

    // 保存出来的文件里，内容流还是不是原来那一段字节
    let untouched = |doc: &Document<'_>| {
        let saved = doc.save_to_vec(SaveFlags::NO_INCREMENTAL).expect("save");
        let streams = common::saved_streams(&saved);
        assert_eq!(streams.len(), 1, "只应有一条内容流");
        streams[0] == CONTENT.as_bytes()
    };
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().expect("init pdfium");

    // 引擎的用法：打开页面、渲染几层、关掉。之后再打开这一页重新生成内容流，没有东西要重写。
    let doc = Document::load_bytes(&lib, two_squares(), None).expect("load");
    {
        let page = doc.page(0).expect("page");
        whole(&page, CLEAR, Some(LayerFilter::Only(&[1])));
        whole(&page, WHITE, Some(LayerFilter::Without(&[1])));
    }
    {
        let page = doc.page(0).expect("page");
        page.generate_content().expect("generate content");
    }
    assert!(untouched(&doc), "页面关掉之后，分层渲染不该留下任何痕迹");

    // 不该有的用法：同一个页面句柄上渲染完接着重新生成。被隐藏过的对象所在的内容流被重写了。
    let doc = Document::load_bytes(&lib, two_squares(), None).expect("load");
    {
        let page = doc.page(0).expect("page");
        whole(&page, WHITE, Some(LayerFilter::Without(&[1])));
        page.generate_content().expect("generate content");
    }
    assert!(
        !untouched(&doc),
        "这条用例靠这一点才分得出上面那种情况是不是真的没有痕迹"
    );
}

#[test]
fn objects_are_back_once_a_layer_is_rendered() {
    with_page(|page| {
        let before = whole(page, WHITE, None);
        for filter in [
            LayerFilter::Without(&[0]),
            LayerFilter::Only(&[1]),
            LayerFilter::Only(&[]),
        ] {
            whole(page, WHITE, Some(filter));
            assert_eq!(page.object_count(), 2);
            assert_eq!(whole(page, WHITE, None).bgra(), before.bgra(), "{filter:?}");
        }

        // 序号不对：报错，什么也不改
        let surface = PageSurface::new(page, 1.0);
        for filter in [LayerFilter::Without(&[0, 2]), LayerFilter::Only(&[7])] {
            let result = render_rect_filtered(
                page,
                surface,
                PixelRect::tile(0, 0),
                RenderFlags::NONE,
                WHITE,
                filter,
            );
            assert!(matches!(result, Err(Error::PageObject)), "{filter:?}");
            assert_eq!(whole(page, WHITE, None).bgra(), before.bgra(), "{filter:?}");
        }
    });
}
