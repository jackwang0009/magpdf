//! 增量更新写出（P0 计划 V17 第 5 条“写回方式”）。
//!
//! 只追加：原文件字节原样保留，新对象与新的一节交叉引用追加在末尾。写法要让 PDFium 重新打开时
//! 认新的对象，所以跟着 PDFium 读交叉引用的规则走（见 [`super::reader`]）：
//!
//! - 原文件最新的一节交叉引用是表：追加一节表，`/Prev` 指向原来的那一节；
//! - 原文件最新的一节是交叉引用流：追加一节交叉引用流（PDFium 沿 `/Prev` 往回找时，表和流不能混着接）；
//! - 原文件的交叉引用读不了、是靠扫描重建出来的：没有可以接的 `/Prev`，写一份**完整**的交叉引用
//!   （重建时认到的全部对象 + 新对象），PDFium 再打开时直接用这一份，不必再重建；
//! - 被替换的对象原来放在对象流里：新内容放进一条**新的对象流**，交叉引用里仍然登记成“在对象流里”。
//!   改写成普通对象在规范里是允许的，但 PDFium 的交叉引用表合并规则可能不认（见
//!   `reader.rs` 里 `KEEP_COMPRESSED_OVER_NORMAL_GEN0` 的说明），所以不那么写。这种写法只有新的一节
//!   是交叉引用流时才能用。
//!
//! 偏移一律相对 `%PDF` 文件头（文件头之前有别的字节时 PDFium 也是这样算的）。

use std::collections::BTreeMap;

use super::object::{PdfDict, PdfObject, PdfStreamRef, encode_name};
use super::reader::{EntryLocation, PdfDocument};

/// 写不出来的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    #[error("对象 {0} 不在交叉引用里，不能替换")]
    NoSuchObject(u32),
    #[error("对象 {0} 放在对象流里，而这份文件的交叉引用是表，改写它 PDFium 不一定认")]
    CompressedInTableFile(u32),
    #[error("对象 {0} 放在对象流里，不能换成流对象")]
    StreamIntoObjectStream(u32),
    #[error("文件太大，偏移写不进交叉引用")]
    TooLarge,
}

/// 新的一节交叉引用里的一个条目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XrefEntry {
    Free,
    Normal { offset: usize, generation: u16 },
    Compressed { stream: u32, index: u32 },
}

/// 增量更新收集器：先登记要新增、要替换的对象，最后一次写出。
#[derive(Debug)]
pub struct IncrementalWriter<'a> {
    doc: &'a PdfDocument,
    next_object: u32,
    /// 写成普通对象的：对象号 → (代号, 对象)。
    objects: BTreeMap<u32, (u16, PdfObject)>,
    /// 要放进新对象流的：对象号 → 对象。
    packed: BTreeMap<u32, PdfObject>,
}

impl<'a> IncrementalWriter<'a> {
    pub fn new(doc: &'a PdfDocument) -> Self {
        let size = doc
            .trailer_dict()
            .get(b"Size")
            .map_or(0, PdfObject::get_integer)
            .max(0) as u32;
        Self {
            doc,
            next_object: doc.max_object_number().max(size.saturating_sub(1)) + 1,
            objects: BTreeMap::new(),
            packed: BTreeMap::new(),
        }
    }

    /// 新的一节交叉引用能不能登记“在对象流里”的条目。
    fn section_is_stream(&self) -> bool {
        self.doc.rebuilt || self.doc.uses_xref_stream
    }

    /// 新增一个对象，返回分到的对象号（代号 0）。
    pub fn add(&mut self, object: PdfObject) -> u32 {
        let number = self.next_object;
        self.next_object += 1;
        self.objects.insert(number, (0, object));
        number
    }

    /// 替换这个对象行不行；不行时给出原因。`is_stream` 是新内容是不是流。
    pub fn check_replace(&self, number: u32, is_stream: bool) -> Result<(), WriteError> {
        if self.doc.object_offset(number).is_some() {
            return Ok(());
        }
        if !self.doc.is_compressed(number) {
            return Err(WriteError::NoSuchObject(number));
        }
        if self.doc.can_replace_in_place(number) {
            return Ok(());
        }
        if is_stream {
            return Err(WriteError::StreamIntoObjectStream(number));
        }
        if !self.section_is_stream() {
            return Err(WriteError::CompressedInTableFile(number));
        }
        Ok(())
    }

    /// 用新内容替换一个已有的对象（对象号、代号不变）。
    pub fn replace(&mut self, number: u32, object: PdfObject) -> Result<(), WriteError> {
        self.check_replace(number, matches!(object, PdfObject::Stream(_)))?;
        if self.doc.object_offset(number).is_some() || self.doc.can_replace_in_place(number) {
            let generation = self.doc.generation(number);
            self.packed.remove(&number);
            self.objects.insert(number, (generation, object));
        } else {
            self.objects.remove(&number);
            self.packed.insert(number, object);
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.packed.is_empty()
    }

    /// 写出：原文件字节 + 追加的一节。什么都没登记时原样返回原文件。
    pub fn finish(mut self) -> Result<Vec<u8>, WriteError> {
        let doc = self.doc;
        let mut out = doc.bytes().to_vec();
        if self.is_empty() {
            return Ok(out);
        }
        let base = doc.header_offset();
        out.push(b'\n');

        let mut entries: BTreeMap<u32, XrefEntry> = BTreeMap::new();
        if doc.rebuilt {
            entries.insert(0, XrefEntry::Free);
            for (number, generation, location) in doc.live_entries() {
                let entry = match location {
                    EntryLocation::Offset(offset) => XrefEntry::Normal { offset, generation },
                    EntryLocation::InStream { stream, index } => {
                        XrefEntry::Compressed { stream, index }
                    }
                };
                entries.insert(number, entry);
            }
        }

        for (number, (generation, object)) in &self.objects {
            let offset = out.len() - base;
            entries.insert(
                *number,
                XrefEntry::Normal {
                    offset,
                    generation: *generation,
                },
            );
            out.extend_from_slice(format!("{number} {generation} obj\n").as_bytes());
            write_object(&mut out, object);
            out.extend_from_slice(b"\nendobj\n");
        }

        if !self.packed.is_empty() {
            let archive = self.next_object;
            self.next_object += 1;
            let mut header = Vec::new();
            let mut body = Vec::new();
            for (index, (number, object)) in self.packed.iter().enumerate() {
                header.extend_from_slice(format!("{number} {} ", body.len()).as_bytes());
                write_object(&mut body, object);
                body.push(b'\n');
                entries.insert(
                    *number,
                    XrefEntry::Compressed {
                        stream: archive,
                        index: index as u32,
                    },
                );
            }
            let mut dict = PdfDict::new();
            dict.set(b"Type", PdfObject::Name(b"ObjStm".to_vec()));
            dict.set(b"N", PdfObject::Int(self.packed.len() as i64));
            dict.set(b"First", PdfObject::Int(header.len() as i64));
            let mut data = header;
            data.extend_from_slice(&body);
            let offset = out.len() - base;
            entries.insert(
                archive,
                XrefEntry::Normal {
                    offset,
                    generation: 0,
                },
            );
            out.extend_from_slice(format!("{archive} 0 obj\n").as_bytes());
            write_object(
                &mut out,
                &PdfObject::Stream(PdfStreamRef {
                    dict,
                    data,
                    span: (0, 0),
                }),
            );
            out.extend_from_slice(b"\nendobj\n");
        }

        let use_stream = if doc.rebuilt {
            entries
                .values()
                .any(|e| matches!(e, XrefEntry::Compressed { .. }))
        } else {
            doc.uses_xref_stream
        };
        let prev = (!doc.rebuilt).then_some(doc.startxref);
        let old_size = doc
            .trailer_dict()
            .get(b"Size")
            .map_or(0, PdfObject::get_integer)
            .max(0) as u32;

        let xref_offset = out.len() - base;
        if use_stream {
            let number = self.next_object;
            entries.insert(
                number,
                XrefEntry::Normal {
                    offset: xref_offset,
                    generation: 0,
                },
            );
            let size = old_size.max(number + 1);
            write_xref_stream(&mut out, doc, number, size, prev, &entries)?;
        } else {
            let last = entries.keys().next_back().copied().unwrap_or(0);
            let size = old_size.max(last + 1);
            write_xref_table(&mut out, doc, size, prev, &entries)?;
        }
        out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        Ok(out)
    }
}

/// 连续的对象号分成一段一段：`(起始号, 条目)`。
fn runs(entries: &BTreeMap<u32, XrefEntry>) -> Vec<(u32, Vec<XrefEntry>)> {
    let mut out: Vec<(u32, Vec<XrefEntry>)> = Vec::new();
    for (number, entry) in entries {
        match out.last_mut() {
            Some((start, items)) if *start + items.len() as u32 == *number => items.push(*entry),
            _ => out.push((*number, vec![*entry])),
        }
    }
    out
}

/// trailer 里要带上的键（`/Size`、`/Prev` 另外写）。
fn carry_trailer_keys(doc: &PdfDocument, dict: &mut PdfDict) {
    for key in [&b"Root"[..], b"Info", b"ID", b"Encrypt"] {
        if let Some(value) = doc.trailer_dict().get(key) {
            dict.set(key, value.clone());
        }
    }
}

fn write_xref_table(
    out: &mut Vec<u8>,
    doc: &PdfDocument,
    size: u32,
    prev: Option<usize>,
    entries: &BTreeMap<u32, XrefEntry>,
) -> Result<(), WriteError> {
    out.extend_from_slice(b"xref\n");
    for (start, items) in runs(entries) {
        out.extend_from_slice(format!("{start} {}\n", items.len()).as_bytes());
        for item in items {
            match item {
                XrefEntry::Normal { offset, generation } => {
                    if offset > i32::MAX as usize {
                        return Err(WriteError::TooLarge);
                    }
                    out.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
                }
                // 表里登记不了“在对象流里”的条目；走到这里的只会是空闲条目。
                XrefEntry::Free | XrefEntry::Compressed { .. } => {
                    out.extend_from_slice(b"0000000000 65535 f \n");
                }
            }
        }
    }
    let mut dict = PdfDict::new();
    dict.set(b"Size", PdfObject::Int(i64::from(size)));
    if let Some(prev) = prev {
        dict.set(b"Prev", PdfObject::Int(prev as i64));
    }
    carry_trailer_keys(doc, &mut dict);
    out.extend_from_slice(b"trailer\n");
    write_object(out, &PdfObject::Dict(dict));
    out.push(b'\n');
    Ok(())
}

fn write_xref_stream(
    out: &mut Vec<u8>,
    doc: &PdfDocument,
    number: u32,
    size: u32,
    prev: Option<usize>,
    entries: &BTreeMap<u32, XrefEntry>,
) -> Result<(), WriteError> {
    let mut data = Vec::with_capacity(entries.len() * 9);
    let mut index = Vec::new();
    for (start, items) in runs(entries) {
        index.push(PdfObject::Int(i64::from(start)));
        index.push(PdfObject::Int(items.len() as i64));
        for item in items {
            let (kind, second, third) = match item {
                XrefEntry::Free => (0u8, 0u32, 65535u32),
                XrefEntry::Normal { offset, generation } => {
                    if offset > i32::MAX as usize {
                        return Err(WriteError::TooLarge);
                    }
                    (1, offset as u32, u32::from(generation))
                }
                XrefEntry::Compressed { stream, index } => (2, stream, index),
            };
            data.push(kind);
            data.extend_from_slice(&second.to_be_bytes());
            data.extend_from_slice(&third.to_be_bytes());
        }
    }
    let mut dict = PdfDict::new();
    dict.set(b"Type", PdfObject::Name(b"XRef".to_vec()));
    dict.set(b"Size", PdfObject::Int(i64::from(size)));
    dict.set(
        b"W",
        PdfObject::Array(vec![
            PdfObject::Int(1),
            PdfObject::Int(4),
            PdfObject::Int(4),
        ]),
    );
    dict.set(b"Index", PdfObject::Array(index));
    if let Some(prev) = prev {
        dict.set(b"Prev", PdfObject::Int(prev as i64));
    }
    carry_trailer_keys(doc, &mut dict);
    out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    write_object(
        out,
        &PdfObject::Stream(PdfStreamRef {
            dict,
            data,
            span: (0, 0),
        }),
    );
    out.extend_from_slice(b"\nendobj\n");
    Ok(())
}

/// 把对象写成 PDF 语法。字符串一律写成十六进制（这一层只写结构，不写内容流字节）。
pub fn write_object(out: &mut Vec<u8>, obj: &PdfObject) {
    match obj {
        PdfObject::Null => out.extend_from_slice(b"null"),
        PdfObject::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        PdfObject::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        PdfObject::Real(f) => out.extend_from_slice(format_real(*f).as_bytes()),
        PdfObject::Str(s) => {
            out.push(b'<');
            for b in s {
                out.extend_from_slice(format!("{b:02X}").as_bytes());
            }
            out.push(b'>');
        }
        PdfObject::Name(n) => {
            out.push(b'/');
            out.extend_from_slice(&encode_name(n));
        }
        PdfObject::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object(out, item);
            }
            out.push(b']');
        }
        PdfObject::Dict(d) => write_dict(out, d),
        PdfObject::Ref(n, g) => out.extend_from_slice(format!("{n} {g} R").as_bytes()),
        PdfObject::Stream(s) => {
            let mut dict = s.dict.clone();
            dict.set(b"Length", PdfObject::Int(s.data.len() as i64));
            write_dict(out, &dict);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&s.data);
            out.extend_from_slice(b"\nendstream");
        }
    }
}

fn write_dict(out: &mut Vec<u8>, d: &PdfDict) {
    out.extend_from_slice(b"<<");
    for (k, v) in d.iter() {
        out.push(b'\n');
        out.push(b'/');
        out.extend_from_slice(&encode_name(k));
        out.push(b' ');
        write_object(out, v);
    }
    out.extend_from_slice(b"\n>>");
}

/// 数字的写法：PDFium 把内容流和对象里的实数都读成单精度，所以按单精度写出“读回来还是同一个值”的
/// 最短十进制小数（不用指数记法，PDF 不认）；整数不带小数点；非有限值写 0。
pub fn format_real(v: f64) -> String {
    let single = v as f32;
    if !single.is_finite() {
        return "0".to_owned();
    }
    if single == 0.0 {
        return "0".to_owned();
    }
    format!("{single}")
}

#[cfg(test)]
mod tests {
    use super::super::pages::{Contents, walk_pages};
    use super::super::reader::tests::{CATALOG, PAGE, PAGES, build, with_object_stream};
    use super::*;

    fn stream(data: &[u8]) -> PdfObject {
        PdfObject::Stream(PdfStreamRef {
            dict: PdfDict::new(),
            data: data.to_vec(),
            span: (0, 0),
        })
    }

    fn simple() -> Vec<u8> {
        build(
            &[
                (1, CATALOG),
                (2, PAGES),
                (3, PAGE),
                (4, "<< /Length 3 >>\nstream\nq Q\nendstream"),
            ],
            "",
        )
    }

    fn stream_data(doc: &PdfDocument, number: u32) -> Vec<u8> {
        match doc.object(number) {
            Some(PdfObject::Stream(s)) => doc.decode_stream(&s),
            other => panic!("对象 {number} 不是流：{other:?}"),
        }
    }

    #[test]
    fn numbers_are_written_the_way_pdfium_reads_them() {
        assert_eq!(format_real(1.0), "1");
        assert_eq!(format_real(-12.5), "-12.5");
        assert_eq!(format_real(0.1), "0.1");
        assert_eq!(format_real(-0.0), "0");
        assert_eq!(format_real(f64::NAN), "0");
        assert_eq!(format_real(1e-7), "0.0000001");
        assert_eq!(format_real(123456.789), "123456.79");
    }

    #[test]
    fn nothing_registered_returns_the_original_bytes() {
        let raw = simple();
        let doc = PdfDocument::load(raw.clone()).unwrap();
        let out = IncrementalWriter::new(&doc).finish().unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn classic_table_update_is_appended_and_read_back() {
        let raw = simple();
        let doc = PdfDocument::load(raw.clone()).unwrap();
        let mut w = IncrementalWriter::new(&doc);
        w.replace(4, stream(b"0 g")).unwrap();
        let added = w.add(stream(b"1 g"));
        assert_eq!(added, 5);
        let out = w.finish().unwrap();
        assert!(out.starts_with(&raw), "原文件字节原样保留");
        let again = PdfDocument::load(out).unwrap();
        assert!(!again.rebuilt, "新的一节读得了，不用重建");
        assert!(!again.uses_xref_stream);
        assert_eq!(stream_data(&again, 4), b"0 g");
        assert_eq!(stream_data(&again, 5), b"1 g");
        assert!(again.root().is_some());
    }

    #[test]
    fn offsets_are_relative_to_the_header() {
        let mut raw = b"junk before the header\n".to_vec();
        raw.extend_from_slice(&simple());
        let doc = PdfDocument::load(raw).unwrap();
        assert!(!doc.rebuilt);
        let mut w = IncrementalWriter::new(&doc);
        w.replace(4, stream(b"0 g")).unwrap();
        let again = PdfDocument::load(w.finish().unwrap()).unwrap();
        assert!(!again.rebuilt);
        assert_eq!(stream_data(&again, 4), b"0 g");
    }

    #[test]
    fn generation_is_kept_when_replacing() {
        // 对象 4 的代号是 3：交叉引用表里写 00003，对象头写 `4 3 obj`。
        let raw = simple();
        let text = String::from_utf8(raw).unwrap();
        let text = text.replacen("4 0 obj", "4 3 obj", 1);
        let pos = text.rfind("00000 n").unwrap();
        let mut text = text;
        text.replace_range(pos..pos + 5, "00003");
        let doc = PdfDocument::load(text.into_bytes()).unwrap();
        assert!(!doc.rebuilt);
        assert_eq!(doc.generation(4), 3);
        let mut w = IncrementalWriter::new(&doc);
        w.replace(4, stream(b"0 g")).unwrap();
        let out = w.finish().unwrap();
        assert!(
            out.windows(8).any(|w| w == b"4 3 obj\n"),
            "对象头带着原来的代号"
        );
        let again = PdfDocument::load(out).unwrap();
        assert!(!again.rebuilt);
        assert_eq!(again.generation(4), 3);
        assert_eq!(stream_data(&again, 4), b"0 g");
    }

    #[test]
    fn xref_stream_file_gets_an_xref_stream_section() {
        let raw = with_object_stream();
        let doc = PdfDocument::load(raw.clone()).unwrap();
        assert!(doc.uses_xref_stream && !doc.rebuilt);
        let pages = walk_pages(&doc);
        let page = pages[0].clone().unwrap();
        assert!(doc.is_compressed(page.number), "页字典在对象流里");
        let Contents::Single(content) = page.contents else {
            panic!("应当是单条流");
        };
        let mut w = IncrementalWriter::new(&doc);
        w.replace(content, stream(b"0 g")).unwrap();
        // 页字典换一份（加一个键），放进新的对象流。
        let mut dict = page.dict.clone();
        dict.set(b"Rotate", PdfObject::Int(90));
        w.replace(page.number, PdfObject::Dict(dict)).unwrap();
        let out = w.finish().unwrap();
        assert!(out.starts_with(&raw));
        let again = PdfDocument::load(out).unwrap();
        assert!(again.uses_xref_stream && !again.rebuilt);
        assert_eq!(stream_data(&again, content), b"0 g");
        let pages = walk_pages(&again);
        let page = pages[0].clone().unwrap();
        assert_eq!(page.dict.get(b"Rotate"), Some(&PdfObject::Int(90)));
        assert!(again.is_compressed(page.number), "新的页字典仍在对象流里");
    }

    #[test]
    fn stream_cannot_replace_an_object_inside_an_object_stream() {
        let doc = PdfDocument::load(with_object_stream()).unwrap();
        let pages = walk_pages(&doc);
        let page = pages[0].clone().unwrap();
        let mut w = IncrementalWriter::new(&doc);
        if !doc.can_replace_in_place(page.number) {
            assert_eq!(
                w.replace(page.number, stream(b"x")),
                Err(WriteError::StreamIntoObjectStream(page.number))
            );
        }
        assert_eq!(
            w.replace(999, stream(b"x")),
            Err(WriteError::NoSuchObject(999))
        );
    }

    #[test]
    fn rebuilt_file_gets_a_complete_table() {
        // startxref 指到别处：PDFium 靠扫描重建。
        let raw = simple();
        let text = String::from_utf8(raw).unwrap();
        let cut = text.rfind("startxref").unwrap();
        let broken = format!("{}startxref\n1\n%%EOF\n", &text[..cut]).into_bytes();
        let doc = PdfDocument::load(broken.clone()).unwrap();
        assert!(doc.rebuilt);
        let mut w = IncrementalWriter::new(&doc);
        w.replace(4, stream(b"0 g")).unwrap();
        let out = w.finish().unwrap();
        assert!(out.starts_with(&broken));
        let again = PdfDocument::load(out).unwrap();
        assert!(!again.rebuilt, "新写的一节是完整的，不用再重建");
        assert_eq!(stream_data(&again, 4), b"0 g");
        assert_eq!(walk_pages(&again).len(), 1);
        assert!(again.object(1).is_some() && again.object(2).is_some());
    }
}
