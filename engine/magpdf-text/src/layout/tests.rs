use super::*;
use crate::eval::{ClusterLabel, PageLabels, score_layout};
use crate::testutil::{TextOpts, Typesetter, text_width};

fn line_texts(page: &PageText, layout: &Layout) -> Vec<String> {
    layout
        .lines
        .iter()
        .map(|l| page.text_of(&l.glyphs))
        .collect()
}

fn block_texts(page: &PageText, layout: &Layout) -> Vec<Vec<String>> {
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

fn default_layout(page: &PageText) -> Layout {
    analyze(page, &LayoutParams::default())
}

/// Word 默认版式：11pt，行距 1.32em，段后 8pt。
const BODY: f32 = 11.0;
const PITCH: f32 = 14.5;
const PARA_GAP: f32 = 14.5 + 8.0;

#[test]
fn single_column_paragraphs_split_by_spacing() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Calibri");
    let mut y = 760.0;
    let mut expected: Vec<Vec<usize>> = Vec::new();
    // 左对齐正文：行尾参差不齐。
    let texts = [
        "the quick brown fox jumps over the lazy dog again and",
        "again while the shipment waits at the terminal for its",
        "customs clearance and the forwarder calls the carrier",
        "short last line",
    ];
    for _ in 0..3 {
        let mut glyphs = Vec::new();
        for text in texts {
            glyphs.extend(ts.text(72.0, y, BODY, f, text));
            y -= PITCH;
        }
        y -= PARA_GAP - PITCH;
        expected.push(glyphs);
    }
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 12, "{:?}", line_texts(&page, &layout));
    assert_eq!(layout.blocks.len(), 3, "{:?}", block_texts(&page, &layout));
    for (b, exp) in layout.blocks.iter().zip(&expected) {
        assert_eq!(b.lines.len(), 4);
        assert_eq!(layout.block_glyphs(b), *exp);
        assert_eq!(b.alignment, Alignment::Left);
        let pitch = b.line_pitch.expect("pitch");
        assert!((pitch.points - PITCH).abs() < 0.01, "{pitch:?}");
        assert!((pitch.em - PITCH / BODY).abs() < 0.01, "{pitch:?}");
        assert!(b.editable && b.horizontal && !b.invisible);
        assert_eq!(b.angle, 0.0);
    }
    // 评测函数对完美结果给满分。
    let labels = PageLabels {
        page: 1,
        lines: layout
            .lines
            .iter()
            .map(|l| ClusterLabel::from_indices(&l.glyphs))
            .collect(),
        paragraphs: expected
            .iter()
            .map(|g| ClusterLabel::from_indices(g))
            .collect(),
        note: None,
    };
    let score = score_layout(&labels, &layout);
    assert_eq!(score.lines.agreeing_chars, score.lines.labelled_chars);
    assert_eq!(score.paragraphs.exact_matches, 3);
}

#[test]
fn two_columns_stay_separate() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Times");
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut y = 700.0;
    for i in 0..6 {
        left.extend(ts.text(60.0, y, 10.0, f, "left column text of the article here"));
        // 右栏基线故意错开半行，模拟两栏不对齐。
        let text = if i % 2 == 0 {
            "right column has other words"
        } else {
            "and continues down the page"
        };
        right.extend(ts.text(310.0, y - 6.0, 10.0, f, text));
        y -= 12.0;
    }
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 12);
    assert_eq!(layout.blocks.len(), 2, "{:?}", block_texts(&page, &layout));
    let mut got: Vec<Vec<usize>> = layout
        .blocks
        .iter()
        .map(|b| layout.block_glyphs(b))
        .collect();
    got.sort();
    let mut exp = vec![left, right];
    exp.sort();
    assert_eq!(got, exp);
}

#[test]
fn centered_title_then_left_body() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let bold = ts.font("Calibri-Bold");
    let f = ts.font("Calibri");
    let t1 = ts.text_centered(297.5, 780.0, 18.0, bold, "Annual Report on Shipping");
    let t2 = ts.text_centered(297.5, 756.0, 18.0, bold, "Volume Two");
    let mut body = Vec::new();
    let mut y = 720.0;
    for text in [
        "body text runs along the full width of the page",
        "and wraps to a second line that is a bit longer than",
        "the first one so the right edge is ragged",
    ] {
        body.extend(ts.text(72.0, y, BODY, f, text));
        y -= PITCH;
    }
    body.extend(ts.text(72.0, y, BODY, f, "and ends here"));
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 2, "{:?}", block_texts(&page, &layout));
    let title = &layout.blocks[0];
    assert_eq!(title.alignment, Alignment::Center);
    assert_eq!(layout.block_glyphs(title), [t1, t2].concat());
    assert!((title.font_size - 18.0).abs() < 1e-3);
    assert_eq!(title.font_name, "Calibri-Bold");
    let para = &layout.blocks[1];
    assert_eq!(para.alignment, Alignment::Left);
    assert_eq!(layout.block_glyphs(para), body);
}

#[test]
fn single_centered_line_uses_page_center() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Calibri");
    ts.text_centered(297.5, 700.0, 14.0, f, "Only Title");
    ts.text(72.0, 600.0, 11.0, f, "a lonely left line");
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 2);
    assert_eq!(layout.blocks[0].alignment, Alignment::Center);
    assert_eq!(layout.blocks[1].alignment, Alignment::Left);
    assert!(layout.blocks[0].line_pitch.is_none());
}

#[test]
fn justified_paragraphs_detected_and_split_at_ragged_line() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Times");
    let (x0, x1) = (72.0, 523.0);
    let mut y = 760.0;
    let mut p1 = Vec::new();
    for _ in 0..4 {
        p1.extend(ts.text_justified(
            x0,
            x1,
            y,
            10.5,
            f,
            "justified text fills the whole measure with stretched spaces between words",
        ));
        y -= 13.0;
    }
    p1.extend(ts.text(x0, y, 10.5, f, "last line is short."));
    y -= 13.0;
    // 第二段：同样的行距、没有段间距、无缩进——只有上一段的短行能说明这里换段。
    let mut p2 = Vec::new();
    for _ in 0..3 {
        p2.extend(ts.text_justified(
            x0,
            x1,
            y,
            10.5,
            f,
            "the second paragraph continues with the very same pitch and no indent at all",
        ));
        y -= 13.0;
    }
    p2.extend(ts.text(x0, y, 10.5, f, "end."));
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 9, "{:?}", line_texts(&page, &layout));
    assert_eq!(layout.blocks.len(), 2, "{:?}", block_texts(&page, &layout));
    assert_eq!(layout.block_glyphs(&layout.blocks[0]), p1);
    assert_eq!(layout.block_glyphs(&layout.blocks[1]), p2);
    assert_eq!(layout.blocks[0].alignment, Alignment::Justify);
    assert_eq!(layout.blocks[1].alignment, Alignment::Justify);
}

#[test]
fn chinese_paragraphs_split_by_first_line_indent() {
    // 公文：16pt 仿宋，固定行距 28pt，首行缩进 2 字，段间无空行。
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("FangSong");
    let x0 = 90.0;
    let full = "各部门认真贯彻落实会议精神切实加强安全生产管理工作确保全年"; // 29 字 = 464pt
    let first = "各部门认真贯彻落实会议精神切实加强安全生产管理工作确保"; // 27 字，前面缩进 2 字
    let x1 = x0 + text_width(full, 16.0);
    assert!((text_width(first, 16.0) + 32.0 - (x1 - x0)).abs() < 0.01);
    let mut y = 740.0;
    let mut paras: Vec<Vec<usize>> = Vec::new();
    for _ in 0..2 {
        let mut p = Vec::new();
        p.extend(ts.text(x0 + 32.0, y, 16.0, f, first));
        y -= 28.0;
        p.extend(ts.text(x0, y, 16.0, f, full));
        y -= 28.0;
        p.extend(ts.text(x0, y, 16.0, f, "目标任务顺利完成。"));
        y -= 28.0;
        paras.push(p);
    }
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 6);
    assert_eq!(layout.blocks.len(), 2, "{:?}", block_texts(&page, &layout));
    assert_eq!(layout.block_glyphs(&layout.blocks[0]), paras[0]);
    assert_eq!(layout.block_glyphs(&layout.blocks[1]), paras[1]);
    let pitch = layout.blocks[0].line_pitch.expect("pitch");
    assert!((pitch.points - 28.0).abs() < 0.01 && (pitch.em - 1.75).abs() < 0.01);
    // 等宽汉字排满整行时左对齐与两端对齐无法区分；中文公文默认两端对齐，按 Justify 报告。
    assert_eq!(layout.blocks[0].alignment, Alignment::Justify);
}

#[test]
fn table_cells_on_shared_baseline_become_separate_lines() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Arial");
    let cols = [72.0, 200.0, 330.0, 460.0];
    let rows = ["Item", "Qty", "Unit", "Amount"];
    let mut cells: Vec<Vec<usize>> = Vec::new();
    let mut y = 700.0;
    for r in 0..3 {
        for (c, &x) in cols.iter().enumerate() {
            let text = if r == 0 {
                rows[c].to_owned()
            } else {
                format!("v{r}{c}")
            };
            cells.push(ts.text(x, y, 9.0, f, &text));
        }
        y -= 20.0;
    }
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 12, "{:?}", line_texts(&page, &layout));
    let mut got: Vec<Vec<usize>> = layout.lines.iter().map(|l| l.glyphs.clone()).collect();
    got.sort();
    cells.sort();
    assert_eq!(got, cells);
    // 没有框线时每列的 3 个单元格按行距规律并成一块：4 块，各 3 行。
    assert_eq!(layout.blocks.len(), 4, "{:?}", block_texts(&page, &layout));
    assert!(layout.blocks.iter().all(|b| b.lines.len() == 3));
}

#[test]
fn table_rulings_separate_rows_and_columns() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Arial");
    // 两列两行的表格：列间距只有 1em（小于 1.5em 的词间距上限），靠竖线分开。
    let size = 10.0;
    let x_a = 72.0;
    let text_a = "wrapped cell text";
    let w_a = text_width(text_a, size);
    let x_line = x_a + w_a + 0.5 * size;
    let x_b = x_line + 0.5 * size;
    ts.ruling(Rect::new(x_line - 0.25, 600.0, x_line + 0.25, 700.0));
    // 横线把两行分开（行距 2em，正常段落的行距也可能这么大）。
    ts.ruling(Rect::new(60.0, 669.75, 400.0, 670.25));
    let a1 = ts.text(x_a, 680.0, size, f, text_a);
    let b1 = ts.text(x_b, 680.0, size, f, "second cell");
    let a2 = ts.text(x_a, 660.0, size, f, "another row");
    let b2 = ts.text(x_b, 660.0, size, f, "more text");
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 4, "{:?}", line_texts(&page, &layout));
    assert_eq!(layout.blocks.len(), 4, "{:?}", block_texts(&page, &layout));
    let mut got: Vec<Vec<usize>> = layout.lines.iter().map(|l| l.glyphs.clone()).collect();
    got.sort();
    let mut exp = vec![a1, b1, a2, b2];
    exp.sort();
    assert_eq!(got, exp);

    // 同样的几何、去掉竖线：相邻单元格并成一行。
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Arial");
    ts.text(x_a, 680.0, size, f, text_a);
    ts.text(x_b, 680.0, size, f, "second cell");
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 1);
}

#[test]
fn underline_is_not_a_separator() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Calibri");
    let mut y = 700.0;
    let mut glyphs = Vec::new();
    for _ in 0..3 {
        glyphs.extend(ts.text(
            72.0,
            y,
            BODY,
            f,
            "a paragraph with an underlined word inside",
        ));
        // 下划线：基线下 0.1em，细线。
        ts.ruling(Rect::new(72.0, y - 1.3, 200.0, y - 0.8));
        y -= PITCH;
    }
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 1, "{:?}", block_texts(&page, &layout));
    assert_eq!(layout.block_glyphs(&layout.blocks[0]), glyphs);
}

#[test]
fn mixed_font_sizes_share_a_line_and_block() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Calibri");
    let bold = ts.font("Calibri-Bold");
    let a = ts.text(72.0, 700.0, 14.0, bold, "Total:");
    let w = text_width("Total:", 14.0);
    let b = ts.text(
        72.0 + w + 3.0,
        700.0,
        10.0,
        f,
        "USD 1,234.00 payable on receipt",
    );
    let c = ts.text(72.0, 686.0, 10.0, f, "second line of the same block");
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 2, "{:?}", line_texts(&page, &layout));
    assert_eq!(layout.lines[0].glyphs, [a.clone(), b.clone()].concat());
    assert!(
        (layout.lines[0].font_size - 10.0).abs() < 1e-3,
        "主字号按字形数取众数"
    );
    assert_eq!(layout.lines[0].font_id, f);
    assert_eq!(layout.blocks.len(), 1, "{:?}", block_texts(&page, &layout));
    assert_eq!(layout.block_glyphs(&layout.blocks[0]), [a, b, c].concat());
}

#[test]
fn superscript_merges_into_host_line() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Calibri");
    let a = ts.text(72.0, 700.0, BODY, f, "area of 25 m");
    let w = text_width("area of 25 m", BODY);
    // 上标：字号 0.65 倍，基线抬高 0.33em。
    let sup = ts.text(72.0 + w, 700.0 + 0.33 * BODY, 0.65 * BODY, f, "2");
    let b = ts.text(72.0 + w + 4.0, 700.0, BODY, f, " and more");
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 1, "{:?}", line_texts(&page, &layout));
    assert_eq!(layout.lines[0].glyphs, [a, sup, b].concat());
}

#[test]
fn rotated_text_clusters_in_its_own_frame_and_odd_angles_are_flagged() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Arial");
    // 90° 文字（页面被旋转保存的常见情形）：向上书写，字的上方朝页面左侧，
    // 因此阅读顺序里的第一行在左、第二行在右。
    let r1 = ts.text_opts(
        488.0,
        100.0,
        10.0,
        f,
        "rotated line one",
        TextOpts {
            angle_deg: 90.0,
            ..TextOpts::default()
        },
    );
    let r2 = ts.text_opts(
        500.0,
        100.0,
        10.0,
        f,
        "rotated line two",
        TextOpts {
            angle_deg: 90.0,
            ..TextOpts::default()
        },
    );
    // 30° 的水印。
    let wm = ts.text_opts(
        150.0,
        300.0,
        30.0,
        f,
        "DRAFT",
        TextOpts {
            angle_deg: 30.0,
            ..TextOpts::default()
        },
    );
    let h = ts.text(72.0, 700.0, 10.0, f, "normal horizontal text");
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 3, "{:?}", block_texts(&page, &layout));
    let horiz = &layout.blocks[0];
    assert_eq!(layout.block_glyphs(horiz), h);
    assert!(horiz.horizontal);
    let rot = layout
        .blocks
        .iter()
        .find(|b| b.angle == 90.0)
        .expect("90° block");
    assert!(!rot.horizontal);
    assert_eq!(rot.lines.len(), 2);
    assert_eq!(layout.block_glyphs(rot), [r1.clone(), r2.clone()].concat());
    assert_eq!(layout.lines[rot.lines[0]].glyphs, r1);
    assert!((rot.line_pitch.expect("pitch").points - 12.0).abs() < 0.01);
    // 包围盒回到页面坐标：竖直的窄长条。
    assert!(rot.bbox.height() > rot.bbox.width(), "{:?}", rot.bbox);
    assert!(
        rot.bbox.left > 480.0 && rot.bbox.right < 510.0,
        "{:?}",
        rot.bbox
    );
    let odd = layout
        .blocks
        .iter()
        .find(|b| (b.angle - 30.0).abs() < 0.01)
        .expect("30° block");
    assert!(!odd.horizontal);
    assert_eq!(layout.block_glyphs(odd), wm);
}

#[test]
fn rotated_page_treats_its_upright_text_as_horizontal() {
    // /Rotate 90 的页：内容流里 90° 的文字显示时端正；块的阅读顺序按显示方向。
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Arial");
    let rot = TextOpts {
        angle_deg: 90.0,
        ..TextOpts::default()
    };
    // 显示时的"上方"是内容流的 −x 方向：x 越小越靠上。
    let lower = ts.text_opts(300.0, 100.0, 12.0, f, "second block", rot);
    let upper = ts.text_opts(100.0, 100.0, 12.0, f, "first block", rot);
    let odd = ts.text(50.0, 50.0, 12.0, f, "sideways in display");
    let mut page = ts.finish();
    page.rotation = 1;
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 3, "{:?}", block_texts(&page, &layout));
    assert_eq!(layout.block_glyphs(&layout.blocks[0]), upper);
    assert_eq!(layout.block_glyphs(&layout.blocks[1]), lower);
    assert_eq!(layout.block_glyphs(&layout.blocks[2]), odd);
    assert!(layout.blocks[0].horizontal && layout.blocks[1].horizontal);
    assert_eq!(layout.blocks[0].angle, 90.0);
    assert!(!layout.blocks[2].horizontal);
}

#[test]
fn upside_down_text_is_horizontal() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Arial");
    let g = ts.text_opts(
        300.0,
        400.0,
        12.0,
        f,
        "flipped",
        TextOpts {
            angle_deg: 180.0,
            ..TextOpts::default()
        },
    );
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 1);
    assert!(layout.blocks[0].horizontal);
    assert_eq!(layout.blocks[0].angle, 180.0);
    assert_eq!(
        layout.lines[0].glyphs, g,
        "180° 的阅读顺序沿文字方向，即页面 x 递减"
    );
}

#[test]
fn generated_spaces_are_skipped_but_indices_preserved() {
    let mut ts = Typesetter::new(595.0, 842.0);
    ts.generated_spaces = true;
    let f = ts.font("Calibri");
    let g = ts.text(72.0, 700.0, BODY, f, "two words here");
    let page = ts.finish();
    assert_eq!(page.glyphs.len(), 14, "12 个字形 + 2 个生成空格");
    assert!(page.glyphs[3].generated);
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 1);
    assert_eq!(layout.lines[0].glyphs, g);
    assert!(!layout.lines[0].glyphs.contains(&3));
    assert_eq!(page.text_of(&layout.lines[0].glyphs), "twowordshere");
}

#[test]
fn unicode_map_error_and_invisible_text_are_flagged() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("ABCDEF+Symbolic");
    ts.text_opts(
        72.0,
        700.0,
        BODY,
        f,
        "glyphs without unicode",
        TextOpts {
            unicode_map_error: true,
            ..TextOpts::default()
        },
    );
    ts.text(72.0, 700.0 - PITCH, BODY, f, "but this line is fine");
    ts.text_opts(
        72.0,
        500.0,
        BODY,
        f,
        "ocr layer",
        TextOpts {
            invisible: true,
            ..TextOpts::default()
        },
    );
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 2, "{:?}", block_texts(&page, &layout));
    assert!(!layout.blocks[0].editable, "块内有无映射字符就标为不可编辑");
    assert!(!layout.lines[0].editable && layout.lines[1].editable);
    assert!(!layout.blocks[0].invisible);
    assert!(layout.blocks[1].invisible && layout.blocks[1].editable);
}

#[test]
fn right_aligned_block() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Calibri");
    ts.text_right(523.0, 760.0, BODY, f, "SAMPLE Trading Co., Ltd.");
    ts.text_right(523.0, 760.0 - PITCH, BODY, f, "12 Harbour Road");
    ts.text_right(523.0, 760.0 - 2.0 * PITCH, BODY, f, "Shanghai, China");
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 1, "{:?}", block_texts(&page, &layout));
    assert_eq!(layout.blocks[0].alignment, Alignment::Right);
}

#[test]
fn bold_heading_same_size_is_its_own_block() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let bold = ts.font("Calibri-Bold");
    let f = ts.font("Calibri");
    let h = ts.text(72.0, 700.0, BODY, bold, "1. Scope");
    let b1 = ts.text(
        72.0,
        700.0 - PITCH,
        BODY,
        f,
        "this clause applies to all shipments booked",
    );
    let b2 = ts.text(72.0, 700.0 - 2.0 * PITCH, BODY, f, "under the terms below.");
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.blocks.len(), 2, "{:?}", block_texts(&page, &layout));
    assert_eq!(layout.block_glyphs(&layout.blocks[0]), h);
    assert_eq!(layout.block_glyphs(&layout.blocks[1]), [b1, b2].concat());
}

#[test]
fn bullet_items_start_new_blocks() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let sym = ts.font("Symbol");
    let f = ts.font("Calibri");
    let mut items: Vec<Vec<usize>> = Vec::new();
    let mut y = 700.0;
    for text in [
        "first item wraps onto a second",
        "second item",
        "third item",
    ] {
        let mut g = ts.text(90.0, y, BODY, sym, "\u{2022}");
        g.extend(ts.text(108.0, y, BODY, f, text));
        y -= PITCH;
        if text.starts_with("first") {
            g.extend(ts.text(108.0, y, BODY, f, "line of text"));
            y -= PITCH;
        }
        items.push(g);
    }
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 4, "{:?}", line_texts(&page, &layout));
    assert_eq!(layout.blocks.len(), 3, "{:?}", block_texts(&page, &layout));
    for (b, exp) in layout.blocks.iter().zip(&items) {
        assert_eq!(layout.block_glyphs(b), *exp);
    }
}

#[test]
fn same_text_object_survives_a_tab_sized_gap() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Courier");
    // 一个 TJ 里用大位移做出 2.5em 的空隙：仍是一行。
    let g = ts.text_opts(
        72.0,
        700.0,
        10.0,
        f,
        "Name:\u{3000}\u{3000}Value",
        TextOpts::default(),
    );
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 1, "{:?}", line_texts(&page, &layout));
    assert_eq!(layout.lines[0].glyphs, g);

    // 两个文字对象、同样的间隙：分成两行（表单的标签与内容）。
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Courier");
    ts.text(72.0, 700.0, 10.0, f, "Name:");
    ts.text(
        72.0 + text_width("Name:\u{3000}\u{3000}", 10.0),
        700.0,
        10.0,
        f,
        "Value",
    );
    let page = ts.finish();
    let layout = default_layout(&page);
    assert_eq!(layout.lines.len(), 2, "{:?}", line_texts(&page, &layout));
}

#[test]
fn large_page_is_linear_time() {
    // 两栏、每栏 100 行、每行 50 字 = 10000 字形。
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Times");
    let text = "abcdefghij klmnopqrst uvwxyzabcd efghijklmn opqrstuvwx";
    assert_eq!(text.chars().count(), 54);
    for col in 0..2 {
        let x = 40.0 + col as f32 * 280.0;
        let mut y = 830.0;
        for i in 0..100 {
            ts.text(x, y, 5.0, f, text);
            y -= if i % 10 == 9 { 11.0 } else { 7.0 };
        }
    }
    let page = ts.finish();
    assert_eq!(page.glyphs.len(), 10800);
    let start = std::time::Instant::now();
    let layout = default_layout(&page);
    let elapsed = start.elapsed();
    assert_eq!(layout.lines.len(), 200);
    assert_eq!(layout.blocks.len(), 20, "每 10 行一段，两栏");
    // 调试构建也应远快于秒级；精确耗时用 `cargo test --release -- --ignored --nocapture timing_10k`。
    assert!(elapsed.as_secs() < 5, "{elapsed:?}");
}

#[test]
#[ignore = "手工测耗时：cargo test -p magpdf-text --release -- --ignored --nocapture timing_10k"]
fn timing_10k_glyphs() {
    let mut ts = Typesetter::new(595.0, 842.0);
    let f = ts.font("Times");
    let text = "abcdefghij klmnopqrst uvwxyzabcd efghijklmn opqrstuvwx";
    for col in 0..2 {
        let x = 40.0 + col as f32 * 280.0;
        let mut y = 830.0;
        for i in 0..100 {
            ts.text(x, y, 5.0, f, text);
            y -= if i % 10 == 9 { 11.0 } else { 7.0 };
        }
    }
    let page = ts.finish();
    let params = LayoutParams::default();
    let _warm = analyze(&page, &params);
    let runs = 20;
    let start = std::time::Instant::now();
    for _ in 0..runs {
        let layout = analyze(&page, &params);
        assert_eq!(layout.lines.len(), 200);
    }
    let per_run = start.elapsed() / runs;
    println!(
        "timing_10k: {} 字形，{} 次平均 {:?}",
        page.glyphs.len(),
        runs,
        per_run
    );
}

#[test]
fn helpers_mode_and_median() {
    assert_eq!(
        mode_f32(&mut [10.0, 10.04, 12.0, 12.0, 12.0, 10.0], 0.1),
        10.0
    );
    assert_eq!(mode_f32(&mut [], 0.1), 0.0);
    assert_eq!(median_f32(&mut [3.0, 1.0, 2.0]), 2.0);
    assert_eq!(median_f32(&mut [4.0, 1.0, 2.0, 3.0]), 2.5);
    assert_eq!(mode_u32(&[2, 1, 1, 2, 3]), 2, "平局取先出现者");
    assert_eq!(mode_u32(&[]), 0);
    assert_eq!(spread(&[1.0, 5.0, 3.0]), 4.0);
}

#[test]
fn layout_serializes_to_json() {
    let mut ts = Typesetter::new(200.0, 100.0);
    let f = ts.font("Calibri");
    ts.text(10.0, 50.0, 10.0, f, "hi there");
    let page = ts.finish();
    let layout = default_layout(&page);
    let json = serde_json::to_string(&layout).unwrap();
    assert!(json.contains("\"alignment\":\"left\""), "{json}");
    let back: Layout = serde_json::from_str(&json).unwrap();
    assert_eq!(back, layout);
    let params = serde_json::to_string(&LayoutParams::default()).unwrap();
    let parsed: LayoutParams = serde_json::from_str("{\"max_word_gap_em\": 2.0}").unwrap();
    assert_eq!(parsed.max_word_gap_em, 2.0);
    assert_eq!(
        parsed.baseline_tol_em,
        LayoutParams::default().baseline_tol_em
    );
    assert!(params.contains("max_word_gap_em"));
}
