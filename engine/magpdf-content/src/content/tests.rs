//! 分词器测试：语法边界、内联图像、多流拼接、损坏输入。

use crate::content::{Content, NoteKind, StreamData, TokenKind};

fn tok(content: &Content) -> Vec<(String, usize, usize)> {
    let tokenized = content.tokenize();
    tokenized
        .tokens
        .into_iter()
        .map(|t| {
            let text = match t.kind {
                TokenKind::Operator => String::from_utf8_lossy(&t.op).into_owned(),
                TokenKind::Number(v) => format!("{v}"),
                TokenKind::Name(n) => format!("/{}", String::from_utf8_lossy(&n)),
                TokenKind::LiteralString => "(s)".to_owned(),
                TokenKind::HexString => "<h>".to_owned(),
                TokenKind::Array => "[a]".to_owned(),
                TokenKind::Dict => "<<d>>".to_owned(),
                TokenKind::Bool(b) => format!("{b}"),
                TokenKind::Null => "null".to_owned(),
                TokenKind::InlineImage(_) => "BI..EI".to_owned(),
                TokenKind::StrayDelimiter(b) => format!("stray{}", b as char),
            };
            (text, t.range.start, t.range.end)
        })
        .collect()
}

fn single(data: &[u8]) -> Content {
    Content::concat(&[StreamData {
        index: 0,
        object_number: 1,
        data: data.to_vec(),
    }])
}
#[test]
fn operators_and_numbers() {
    let c = single(b"0 0 1 rg 10 20 m 30 40 l S");
    let t = tok(&c);
    let names: Vec<&str> = t.iter().map(|x| x.0.as_str()).collect();
    assert_eq!(
        names,
        ["0", "0", "1", "rg", "10", "20", "m", "30", "40", "l", "S"]
    );
    // 区间原样指回字节。
    let (s, e) = (t[3].1, t[3].2);
    assert_eq!(&c.data[s..e], b"rg");
}

#[test]
fn numbers_edge_cases() {
    let t = tok(&single(b".5 -.5 1. +3 1e5 4 ..5 -"));
    let names: Vec<&str> = t.iter().map(|x| x.0.as_str()).collect();
    // `1e5`、`..5`、`-` 不是数字（PDF 数字语法里没有指数），按操作符处理。
    assert_eq!(names, ["0.5", "-0.5", "1", "3", "1e5", "4", "..5", "-"]);
}

#[test]
fn strings_with_escapes_and_nesting() {
    let c = single(b"(a\\(b\\)c) <4142> (line\\\ncont) (\\101\\102)");
    let t = tok(&c);
    assert_eq!(t.len(), 4, "{t:?}");
    assert_eq!(t[0].0, "(s)");
    assert_eq!(&c.data[t[0].1..t[0].2], b"(a\\(b\\)c)");
}

#[test]
fn names_with_hex_escapes() {
    let c = single(b"/A#20B /F1 TJ");
    let t = tok(&c);
    assert_eq!(t[0].0, "/A B");
    assert_eq!(t[1].0, "/F1");
    assert_eq!(t[2].0, "TJ");
}

#[test]
fn comments_are_not_tokens() {
    let c = single(b"q % comment with ) and (\nQ");
    let t = tok(&c);
    assert_eq!(
        t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["q", "Q"]
    );
}

#[test]
fn arrays_and_dicts_are_single_tokens() {
    let c = single(b"[(A) -120 (B)] TJ << /K 1 /S /N >> BDC");
    let t = tok(&c);
    assert_eq!(
        t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["[a]", "TJ", "<<d>>", "BDC"]
    );
    let tokenized = c.tokenize();
    let arr = &tokenized.tokens[0].array.as_ref().expect("array")[..];
    assert_eq!(arr.len(), 3);
}

#[test]
fn multi_stream_concatenation_matches_pdfium() {
    // PDFium 在每条流之后补一个空格；记号可以跨流。
    let c = Content::concat(&[
        StreamData {
            index: 0,
            object_number: 10,
            data: b"q BT /F1 12 Tf".to_vec(),
        },
        StreamData {
            index: 1,
            object_number: 11,
            data: b"(Hi) Tj ET Q".to_vec(),
        },
    ]);
    let t = tok(&c);
    assert_eq!(
        t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["q", "BT", "/F1", "12", "Tf", "(s)", "Tj", "ET", "Q"]
    );
    let tokenized = c.tokenize();
    assert_eq!(tokenized.tokens[0].stream, 0);
    assert_eq!(tokenized.tokens[6].stream, 1);
    // 每条流之后补一个分隔空格，PDFium 据此拼接；流末尾的空格归属后一条流。
    assert_eq!(c.segment_of(15), 1);
    assert_eq!(c.segment_of(16), 1);
}

#[test]
fn operand_split_across_streams() {
    let c = Content::concat(&[
        StreamData {
            index: 0,
            object_number: 1,
            data: b"10 20".to_vec(),
        },
        StreamData {
            index: 1,
            object_number: 2,
            data: b"30 40 re f".to_vec(),
        },
    ]);
    let t = tok(&c);
    assert_eq!(
        t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["10", "20", "30", "40", "re", "f"]
    );
}

#[test]
fn inline_image_unfiltered() {
    // 2×2 灰度 8 位：4 字节数据。
    let c = single(b"BI /W 2 /H 2 /BPC 8 /CS /G ID \x01\x02\x03\x04 EI Q");
    let tokenized = c.tokenize();
    assert_eq!(tokenized.tokens.len(), 2, "{:?}", tokenized.tokens);
    let TokenKind::InlineImage(img) = &tokenized.tokens[0].kind else {
        panic!("not inline image")
    };
    assert_eq!(&c.data[img.data.0..img.data.1], b"\x01\x02\x03\x04");
    assert_eq!(img.decoded_len, Some(4));
    assert!(tokenized.tokens[1].is_op(b"Q"));
}

#[test]
fn inline_image_data_containing_ei_bytes() {
    // 数据里出现 "EI " 但长度已定，不应提前结束。
    let c = single(b"BI /W 4 /H 1 /BPC 8 /CS /G ID EI x EI Q");
    let tokenized = c.tokenize();
    let TokenKind::InlineImage(img) = &tokenized.tokens[0].kind else {
        panic!("not inline image")
    };
    assert_eq!(&c.data[img.data.0..img.data.1], b"EI x");
    assert!(tokenized.tokens[1].is_op(b"Q"));
}

#[test]
fn inline_image_flate() {
    let pixels = [1u8, 2, 3, 4, 5, 6];
    let packed = crate::pdf::encode_flate(&pixels);
    let mut src = b"BI /W 3 /H 2 /BPC 8 /CS /G /F /Fl ID ".to_vec();
    src.extend_from_slice(&packed);
    src.extend_from_slice(b" EI");
    let c = single(&src);
    let tokenized = c.tokenize();
    let TokenKind::InlineImage(img) = &tokenized.tokens[0].kind else {
        panic!("not inline image")
    };
    assert_eq!(img.decoded_len, Some(6));
    let data = &c.data[img.data.0..img.data.1];
    assert_eq!(&data[..packed.len()], &packed[..]);
}

#[test]
fn inline_image_image_mask() {
    let c = single(b"BI /W 8 /H 2 /IM true ID \xff\x00 EI");
    let tokenized = c.tokenize();
    let TokenKind::InlineImage(img) = &tokenized.tokens[0].kind else {
        panic!()
    };
    assert_eq!(img.decoded_len, Some(2));
}

#[test]
fn inline_image_without_ei_is_noted() {
    let c = single(b"BI /W 2 /H 1 /BPC 8 /CS /G ID ab");
    let tokenized = c.tokenize();
    assert!(
        tokenized
            .notes
            .iter()
            .any(|n| n.note == NoteKind::InlineImageUnterminated)
    );
}

#[test]
fn damaged_input_does_not_panic() {
    let cases: &[&[u8]] = &[
        b"(",
        b"<",
        b"<<",
        b"[",
        b"[(]",
        b"BI",
        b"BI /W x",
        b"BI /W 2 /H 2 ID",
        b"/",
        b"\xff\xfe\x00\x01",
        b"-------",
        b")>]}",
        b"BI /W 1 /H 1 /BPC 8 /CS /G /F /Fl ID \xff\xff EI",
    ];
    for case in cases {
        let c = single(case);
        let _ = c.tokenize();
    }
}

#[test]
fn every_token_range_is_within_data_and_ordered() {
    let c = single(b"q 1 0 0 1 10 10 cm /Im1 Do Q BT (x) Tj ET");
    let tokenized = c.tokenize();
    let mut last = 0;
    for t in &tokenized.tokens {
        assert!(t.range.start >= last, "{:?}", t);
        assert!(t.range.end <= c.data.len());
        last = t.range.end;
    }
}
