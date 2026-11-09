//! 端到端：手工拼一个带标题、段落、表格行、框线、旋转文字和不可见文字的 PDF，
//! 经 pdfium-sys 取字符，再用 magpdf-text 聚类，检查行/块/属性。
//! 同一进程只能有一个 Library，所以用互斥锁把用到 PDFium 的测试串行化。
// 测试代码允许 unwrap/expect（《工程规范》4）；集成测试不在 #[cfg(test)] 下，需显式放开。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use magpdf_text::{Alignment, Layout, LayoutParams, PageText, analyze};
use pdfium_sys::{Document, Library};

static SERIAL: Mutex<()> = Mutex::new(());

fn with_library(f: impl FnOnce(&Library)) {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = Library::init().expect("init pdfium");
    f(&lib);
}

/// 手工拼一个单页 PDF，交叉引用表偏移按实际字节计算（同 pdfium-sys 烟雾测试）。
fn single_page_pdf(
    (width, height): (u32, u32),
    content: &str,
    resources: &str,
    extra_objects: &[&str],
) -> Vec<u8> {
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Contents 4 0 R /Resources {resources} >>"
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

const RESOURCES: &str = "<< /Font << /F1 5 0 R /F2 6 0 R >> >>";
const FONTS: [&str; 2] = [
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>",
];

/// 400×300 点的页面：粗体标题、两段正文（行距 12）、同基线三个单元格、一条横框线、
/// 90° 旋转文字、不可见（3 Tr）文字。
const CONTENT: &str = "\
BT /F2 12 Tf 20 270 Td (Title Line) Tj ET
BT /F1 10 Tf 20 250 Td (First paragraph line one with words) Tj ET
BT /F1 10 Tf 20 238 Td (second line of the paragraph) Tj ET
BT /F1 10 Tf 20 226 Td (short end.) Tj ET
BT /F1 10 Tf 20 190 Td (Second paragraph starts after a gap) Tj ET
BT /F1 10 Tf 20 178 Td (and has two lines) Tj ET
BT /F1 10 Tf 20 140 Td (Cell A) Tj ET
BT /F1 10 Tf 150 140 Td (Cell B) Tj ET
BT /F1 10 Tf 280 140 Td (Cell C) Tj ET
0 0 0 rg 20 130 360 0.5 re f
BT /F1 10 Tf 0 1 -1 0 390 20 Tm (Rotated) Tj ET
BT /F1 10 Tf 3 Tr 20 40 Td (Hidden) Tj ET";

fn texts(page: &PageText, layout: &Layout) -> Vec<Vec<String>> {
    layout
        .blocks
        .iter()
        .map(|b| {
            b.lines
                .iter()
                .map(|&i| page.text_of(&layout.lines[i].glyphs))
                .collect()
        })
        .collect()
}

#[test]
fn clusters_a_synthetic_page_through_pdfium() {
    with_library(|lib| {
        let bytes = single_page_pdf((400, 300), CONTENT, RESOURCES, &FONTS);
        let doc = Document::load_bytes(lib, bytes, None).expect("load");
        let page = doc.page(0).expect("page");
        let params = LayoutParams::default();
        let text = PageText::from_page(&page, params.ruling_max_thickness).expect("page text");

        // 字体表与框线。
        let names: Vec<&str> = text.fonts.iter().map(|f| f.name.as_str()).collect();
        assert!(names.iter().any(|n| n.contains("Helvetica")), "{names:?}");
        assert_eq!(text.rulings.len(), 1, "{:?}", text.rulings);
        assert!(text.rulings[0].height() <= 0.5 + 1e-3 && text.rulings[0].width() >= 359.0);

        // 字号按文字矩阵缩放后仍是 Tf 值；旋转字符角度 90°。
        let visible: Vec<_> = text.glyphs.iter().filter(|g| !g.generated).collect();
        assert!(
            visible
                .iter()
                .all(|g| (g.font_size - 10.0).abs() < 0.01 || (g.font_size - 12.0).abs() < 0.01)
        );
        let rotated: Vec<_> = visible
            .iter()
            .filter(|g| (g.angle - 90.0).abs() < 0.01)
            .collect();
        assert_eq!(rotated.len(), "Rotated".len());
        assert!(visible.iter().filter(|g| g.invisible).count() == "Hidden".len());
        assert!(
            visible
                .iter()
                .all(|g| !g.unicode_map_error && g.text_object.is_some())
        );

        let layout = analyze(&text, &params);
        let got = texts(&text, &layout);
        assert_eq!(layout.lines.len(), 11, "{got:?}");
        assert_eq!(layout.blocks.len(), 8, "{got:?}");

        let title = &layout.blocks[0];
        assert_eq!(got[0], ["Title Line"]);
        assert!((title.font_size - 12.0).abs() < 0.01);
        assert!(title.font_name.contains("Bold"), "{}", title.font_name);
        assert!(title.horizontal && title.editable && !title.invisible);

        let p1 = &layout.blocks[1];
        assert_eq!(
            got[1],
            [
                "First paragraph line one with words",
                "second line of the paragraph",
                "short end."
            ]
        );
        assert_eq!(p1.alignment, Alignment::Left);
        let pitch = p1.line_pitch.expect("pitch");
        assert!(
            (pitch.points - 12.0).abs() < 0.05 && (pitch.em - 1.2).abs() < 0.01,
            "{pitch:?}"
        );
        assert!((p1.font_size - 10.0).abs() < 0.01);
        assert!(
            p1.bbox.left >= 19.0 && p1.bbox.left <= 21.0,
            "{:?}",
            p1.bbox
        );
        assert!(
            p1.bbox.top > 250.0 && p1.bbox.bottom < 226.0,
            "{:?}",
            p1.bbox
        );

        assert_eq!(
            got[2],
            ["Second paragraph starts after a gap", "and has two lines"]
        );

        // 同基线的三个单元格：各自成行、成块。
        let cells: Vec<&Vec<String>> = got
            .iter()
            .filter(|b| b.len() == 1 && b[0].starts_with("Cell"))
            .collect();
        assert_eq!(cells.len(), 3, "{got:?}");

        let rot = layout
            .blocks
            .iter()
            .find(|b| (b.angle - 90.0).abs() < 0.01)
            .expect("rotated block");
        assert!(!rot.horizontal && rot.editable);
        assert_eq!(text.text_of(&layout.block_glyphs(rot)), "Rotated");
        assert!(rot.bbox.height() > rot.bbox.width(), "{:?}", rot.bbox);
        assert!(
            rot.bbox.left > 375.0 && rot.bbox.right < 395.0,
            "{:?}",
            rot.bbox
        );

        let hidden = layout
            .blocks
            .iter()
            .find(|b| b.invisible)
            .expect("invisible block");
        assert_eq!(text.text_of(&layout.block_glyphs(hidden)), "Hidden");
        assert!(hidden.editable);

        // 每个可见字形恰好属于一行、一块。
        let mut seen = vec![0usize; text.glyphs.len()];
        for l in &layout.lines {
            for &g in &l.glyphs {
                seen[g] += 1;
            }
        }
        for (i, g) in text.glyphs.iter().enumerate() {
            assert_eq!(seen[i], usize::from(!g.generated), "glyph {i}");
        }
        let in_blocks: usize = layout.blocks.iter().map(|b| b.lines.len()).sum();
        assert_eq!(in_blocks, layout.lines.len());

        // JSON 往返。
        let json = serde_json::to_string(&layout).expect("json");
        let back: Layout = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, layout);
    });
}

#[test]
fn page_without_text_yields_empty_layout() {
    with_library(|lib| {
        let bytes = single_page_pdf((200, 100), "0 0 1 rg 20 20 60 60 re f", "<< >>", &[]);
        let doc = Document::load_bytes(lib, bytes, None).expect("load");
        let page = doc.page(0).expect("page");
        let text = PageText::from_page(&page, 2.5).expect("page text");
        assert!(text.glyphs.is_empty());
        assert!(text.rulings.is_empty(), "60×60 的方块不是细线");
        let layout = analyze(&text, &LayoutParams::default());
        assert!(layout.lines.is_empty() && layout.blocks.is_empty());
    });
}
