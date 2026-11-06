//! 烟雾测试：打开、渲染、枚举对象、重写内容流、保存、重新打开。
//! 同一进程只能有一个 Library，所以用互斥锁把用到 PDFium 的测试串行化。
// 测试代码允许 unwrap/expect（《工程规范》4）；集成测试不在 #[cfg(test)] 下，需显式放开。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use pdfium_sys::{Document, Library, ObjectKind, RenderOptions, SaveFlags};

static SERIAL: Mutex<()> = Mutex::new(());

fn with_library(f: impl FnOnce(&Library)) {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().expect("init pdfium");
    f(&lib);
}

/// 手工拼一个单页 PDF（200×100 点），交叉引用表偏移按实际字节计算。
fn minimal_pdf(content: &str, resources: &str, extra_objects: &[&str]) -> Vec<u8> {
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources {resources} >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    objects.extend(extra_objects.iter().map(|s| (*s).to_owned()));

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
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

const BLUE_SQUARE: &str = "0 0 1 rg 20 20 60 60 re f";

#[test]
fn linked_version_is_known() {
    assert_ne!(Library::linked_version(), "unknown");
}

#[test]
fn render_and_inspect_objects() {
    with_library(|lib| {
        let doc =
            Document::load_bytes(lib, minimal_pdf(BLUE_SQUARE, "<< >>", &[]), None).expect("load");
        assert_eq!(doc.page_count(), 1);
        assert_eq!(doc.file_version(), Some(17));

        let page = doc.page(0).expect("page");
        assert_eq!(page.width(), 200.0);
        assert_eq!(page.height(), 100.0);
        assert!(doc.page(1).is_err());

        let bmp = page.render(RenderOptions::default()).expect("render");
        assert_eq!((bmp.width(), bmp.height()), (200, 100));
        // 位图自顶向下，PDF 坐标 (50, 50) 对应像素 (50, 100 - 50)。
        assert_eq!(
            bmp.pixel(50, 50),
            [255, 0, 0, 255],
            "方块内应为蓝色（BGRA）"
        );
        assert_eq!(bmp.pixel(5, 5), [255, 255, 255, 255], "方块外应为白色");

        let bmp2x = page.render(RenderOptions::dpi(144.0)).expect("render 2x");
        assert_eq!((bmp2x.width(), bmp2x.height()), (400, 200));

        assert_eq!(page.object_count(), 1);
        let obj = page.object(0).expect("object 0");
        assert_eq!(obj.kind(), ObjectKind::Path);
        let b = obj.bounds().expect("bounds");
        assert!(
            (b.left - 20.0).abs() < 0.01 && (b.right - 80.0).abs() < 0.01,
            "{b:?}"
        );
        let fill = obj.fill_color().expect("fill color");
        assert_eq!((fill.r, fill.g, fill.b), (0, 0, 255));
        assert_eq!(page.text().expect("text page").char_count(), 0);
    });
}

#[test]
fn regenerate_content_and_save_roundtrip() {
    with_library(|lib| {
        let doc =
            Document::load_bytes(lib, minimal_pdf(BLUE_SQUARE, "<< >>", &[]), None).expect("load");
        let saved = {
            let page = doc.page(0).expect("page");
            for obj in page.objects() {
                obj.mark_modified().expect("mark modified");
            }
            page.generate_content().expect("generate content");
            doc.save_to_vec(SaveFlags::NO_INCREMENTAL).expect("save")
        };
        assert!(saved.starts_with(b"%PDF"), "保存结果应是 PDF");

        let reopened = Document::load_bytes(lib, saved, None).expect("reopen");
        assert_eq!(reopened.page_count(), 1);
        let page = reopened.page(0).expect("page");
        assert_eq!(page.object_count(), 1);
        let bmp = page.render(RenderOptions::default()).expect("render");
        assert_eq!(
            bmp.pixel(50, 50),
            [255, 0, 0, 255],
            "重写内容流后方块仍应为蓝色"
        );
    });
}

#[test]
fn text_page_reads_characters() {
    with_library(|lib| {
        let content = "BT /F1 24 Tf 20 40 Td (Hi) Tj ET";
        let resources = "<< /Font << /F1 5 0 R >> >>";
        let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
        let doc = Document::load_bytes(lib, minimal_pdf(content, resources, &[font]), None)
            .expect("load");
        let page = doc.page(0).expect("page");
        assert_eq!(page.object_count(), 1);
        assert_eq!(page.object(0).expect("obj").kind(), ObjectKind::Text);

        let text = page.text().expect("text page");
        assert_eq!(text.char_count(), 2);
        assert_eq!(text.unicode(0), Some(u32::from('H')));
        assert_eq!(text.unicode(1), Some(u32::from('i')));
        let info = text.char_info(0).expect("char info");
        assert!((info.font_size - 24.0).abs() < 0.01);
        assert!(
            info.font_name.contains("Helvetica"),
            "font name: {}",
            info.font_name
        );
        assert!(
            info.bbox.left >= 19.0 && info.bbox.right <= 60.0,
            "{:?}",
            info.bbox
        );
        assert!(!info.generated);
        assert_eq!(text.chars().count(), 2);
    });
}
