//! 写回：把改过的内容流用增量更新写进文件。
//!
//! - 一条内容流只被一页用了一次：**原地替换**（对象号、代号不变），页字典和页树不用动；
//! - 一条内容流被多页共用，或者在同一页的 `/Contents` 里出现了不止一次：不能原地替换（别处也会跟着
//!   变），另建一条流，并重写这一页的页字典，让 `/Contents` 指向新流；
//! - 要重写页字典而重写不了（页字典直接写在 `/Kids` 里、同一个页对象占了几个页号、页字典在对象流里
//!   而文件的交叉引用是表）：这一页不写，记在 [`SavedDocument::rejected`] 里，由调用方交给 PDFium；
//! - 新内容是空的：写成不带过滤器、`/Length 0` 的流。压缩过的空内容解出来是空的，PDFium 这时会退回去
//!   用压缩后的原始字节当内容流。
//!
//! 一页要么全部写进去，要么完全不动：先检查，检查通过才登记。
//!
//! 没有发现的情况：内容流同时被别的地方引用（比如又被当成表单 XObject），原地替换会连带改到那里。

use std::collections::BTreeMap;

use crate::pdf::encode_flate;
use crate::pdf::object::{PdfDict, PdfObject, PdfStreamRef};
use crate::pdf::pages::{Contents, PageInfo};
use crate::pdf::reader::{EntryLocation, PdfDocument};
use crate::pdf::writer::{IncrementalWriter, WriteError, write_object};

/// 一页改过的内容流。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageUpdate {
    /// 页序（0 起）。
    pub index: usize,
    /// `/Contents` 每一项的新字节（解码后的）；`None` 表示这一项不写。给了就写，不比较有没有变。
    pub segments: Vec<Option<Vec<u8>>>,
}

/// 保存的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedDocument {
    /// 原文件字节 + 追加的一节。什么都没写时就是原文件。
    pub bytes: Vec<u8>,
    /// 写进去的页：页序 → 保存之后 `/Contents` 每一项的流对象号（不是流的项是 `None`）。
    pub pages: BTreeMap<usize, Vec<Option<u32>>>,
    /// 写不了的页：页序与原因。这些页一个字节都没有动。
    pub rejected: Vec<(usize, String)>,
    /// 因为共用而另建的流个数。
    pub deshared_streams: usize,
    /// 重写的页字典个数。
    pub page_dicts_rewritten: usize,
    /// 写出的内容流个数。
    pub streams_written: usize,
}

/// 一页的写法。检查通过之后才有。
struct Plan<'u> {
    /// 要写的段：段下标、原来的流对象号、新字节、是不是原地替换。
    writes: Vec<(usize, u32, &'u [u8], bool)>,
    /// 要重写页字典时：页对象号与原来的页字典。
    page: Option<(u32, PdfDict)>,
    /// `/Contents` 每一项原来的流对象号。
    slots: Vec<Option<u32>>,
}

/// 增量更新。
///
/// `pages` 是 [`crate::pdf::pages::walk_pages`] 的结果；`compress` 为真时新流用 Flate 压缩。
pub fn save_incremental(
    doc: &PdfDocument,
    pages: &[Option<PageInfo>],
    updates: &[PageUpdate],
    compress: bool,
) -> Result<SavedDocument, WriteError> {
    // 每条内容流被用了几次（跨页、同一页里重复都算）；每个页对象占了几个页号。
    let mut stream_uses: BTreeMap<u32, usize> = BTreeMap::new();
    let mut page_uses: BTreeMap<u32, usize> = BTreeMap::new();
    for info in pages.iter().flatten() {
        for number in info.contents.stream_numbers() {
            *stream_uses.entry(number).or_default() += 1;
        }
        if info.number != 0 {
            *page_uses.entry(info.number).or_default() += 1;
        }
    }

    let mut writer = IncrementalWriter::new(doc);
    let mut saved = SavedDocument::default();
    for update in updates {
        if saved.pages.contains_key(&update.index) {
            saved
                .rejected
                .push((update.index, "同一页给了两次".to_owned()));
            continue;
        }
        let plan = match check(doc, pages, update, &stream_uses, &page_uses, &writer) {
            Ok(Some(plan)) => plan,
            Ok(None) => continue,
            Err(reason) => {
                saved.rejected.push((update.index, reason));
                continue;
            }
        };
        let mut slots = plan.slots.clone();
        for (segment, number, data, in_place) in plan.writes {
            let stream = content_stream(doc, number, data, compress);
            if in_place {
                writer.replace(number, stream)?;
            } else {
                slots[segment] = Some(writer.add(stream));
                saved.deshared_streams += 1;
            }
            saved.streams_written += 1;
        }
        if let Some((number, mut dict)) = plan.page {
            let contents = match doc.resolve(dict.get(b"Contents")) {
                // 数组：没换的项照原样留着（包括不是流的项），换了的项指向新流。
                Some(PdfObject::Array(items)) => PdfObject::Array(
                    items
                        .into_iter()
                        .zip(plan.slots.iter().zip(&slots))
                        .map(|(item, (old, new))| match new {
                            Some(new) if old != &Some(*new) => PdfObject::Ref(*new, 0),
                            _ => item,
                        })
                        .collect(),
                ),
                _ => match slots.first() {
                    Some(Some(new)) => PdfObject::Ref(*new, 0),
                    _ => PdfObject::Null,
                },
            };
            dict.set(b"Contents", contents);
            writer.replace(number, PdfObject::Dict(dict))?;
            saved.page_dicts_rewritten += 1;
        }
        saved.pages.insert(update.index, slots);
    }
    saved.bytes = writer.finish()?;
    Ok(saved)
}

/// 检查一页写不写得了。`Ok(None)` 是没有要写的东西。
fn check<'u>(
    doc: &PdfDocument,
    pages: &[Option<PageInfo>],
    update: &'u PageUpdate,
    stream_uses: &BTreeMap<u32, usize>,
    page_uses: &BTreeMap<u32, usize>,
    writer: &IncrementalWriter<'_>,
) -> Result<Option<Plan<'u>>, String> {
    let info = pages
        .get(update.index)
        .and_then(Option::as_ref)
        .ok_or_else(|| "这一页打不开".to_owned())?;
    let slots: Vec<Option<u32>> = match &info.contents {
        Contents::None => Vec::new(),
        Contents::Single(number) => vec![Some(*number)],
        Contents::Array(items) => items.clone(),
    };
    if update.segments.len() != slots.len() {
        return Err(format!(
            "给了 {} 段，这一页的 /Contents 有 {} 项",
            update.segments.len(),
            slots.len()
        ));
    }
    let mut writes = Vec::new();
    let mut new_objects = false;
    for (index, (segment, slot)) in update.segments.iter().zip(&slots).enumerate() {
        let Some(data) = segment else {
            continue;
        };
        let Some(number) = slot else {
            if data.is_empty() {
                continue;
            }
            return Err("新内容落在 /Contents 里不是流的那一项上".to_owned());
        };
        let in_place = stream_uses.get(number).copied().unwrap_or(0) == 1
            && writer.check_replace(*number, true).is_ok();
        new_objects |= !in_place;
        writes.push((index, *number, data.as_slice(), in_place));
    }
    if writes.is_empty() {
        return Ok(None);
    }
    let page = if new_objects {
        const WHY: &str = "内容流与别处共用，要改页字典";
        if info.number == 0 {
            return Err(format!("{WHY}，而页字典直接写在 /Kids 里，没有对象号"));
        }
        if page_uses.get(&info.number).copied().unwrap_or(0) != 1 {
            return Err(format!("{WHY}，而这个页对象占了不止一个页号"));
        }
        let Some(PdfObject::Dict(dict)) = doc.object(info.number) else {
            return Err(format!("{WHY}，而页对象不是字典"));
        };
        writer
            .check_replace(info.number, false)
            .map_err(|e| format!("{WHY}：{e}"))?;
        if matches!(info.contents, Contents::Array(_)) {
            match doc.resolve(dict.get(b"Contents")) {
                Some(PdfObject::Array(items)) if items.len() == slots.len() => {}
                _ => return Err(format!("{WHY}，而页字典的 /Contents 和读到的对不上")),
            }
        }
        Some((info.number, dict))
    } else {
        None
    };
    Ok(Some(Plan {
        writes,
        page,
        slots,
    }))
}

/// 新的内容流对象：沿用原来的流字典里别的键，过滤器相关的键按新数据重写。
fn content_stream(doc: &PdfDocument, original: u32, data: &[u8], compress: bool) -> PdfObject {
    let mut dict = match doc.object(original) {
        Some(PdfObject::Stream(stream)) => stream.dict,
        _ => PdfDict::new(),
    };
    for key in [
        &b"Filter"[..],
        b"DecodeParms",
        b"DL",
        b"Length",
        b"F",
        b"FFilter",
        b"FDecodeParms",
    ] {
        dict.remove(key);
    }
    // 空内容不压缩，见本文件开头。
    let data = if compress && !data.is_empty() {
        dict.set(b"Filter", PdfObject::Name(b"FlateDecode".to_vec()));
        encode_flate(data)
    } else {
        data.to_vec()
    };
    PdfObject::Stream(PdfStreamRef {
        dict,
        data,
        span: (0, 0),
    })
}

/// 完整重写：把现存的对象照抄到一份新文件里（旧版本的字节、失效的对象都不带；流不重新压缩）。
/// 只用来和增量更新比体积。
pub fn save_full_rewrite(doc: &PdfDocument) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: BTreeMap<u32, (usize, u16)> = BTreeMap::new();
    for (number, generation, location) in doc.live_entries() {
        let Some(object) = doc.object(number) else {
            continue;
        };
        // 对象流和交叉引用流是容器：里面的对象各自照抄，容器本身不带。
        if let PdfObject::Stream(stream) = &object
            && matches!(
                stream.dict.get(b"Type").and_then(PdfObject::as_name),
                Some(b"ObjStm" | b"XRef")
            )
        {
            continue;
        }
        let generation = match location {
            EntryLocation::Offset(_) => generation,
            EntryLocation::InStream { .. } => 0,
        };
        offsets.insert(number, (out.len(), generation));
        out.extend_from_slice(format!("{number} {generation} obj\n").as_bytes());
        write_object(&mut out, &object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    let numbers: Vec<u32> = offsets.keys().copied().collect();
    for run in numbers.chunk_by(|a, b| a + 1 == *b) {
        out.extend_from_slice(format!("{} {}\n", run[0], run.len()).as_bytes());
        for number in run {
            let (offset, generation) = offsets[number];
            out.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
    }
    let mut trailer = PdfDict::new();
    let size = numbers.last().map_or(1, |n| i64::from(*n) + 1);
    trailer.set(b"Size", PdfObject::Int(size));
    for key in [&b"Root"[..], b"Info", b"ID", b"Encrypt"] {
        if let Some(value) = doc.trailer_dict().get(key) {
            trailer.set(key, value.clone());
        }
    }
    out.extend_from_slice(b"trailer\n");
    write_object(&mut out, &PdfObject::Dict(trailer));
    out.extend_from_slice(format!("\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::Content;
    use crate::pdf::pages::walk_pages;
    use crate::pdf::reader::tests::{CATALOG, PAGE, PAGES, build};

    const STREAM: &str = "<< /Length 3 >>\nstream\nq Q\nendstream";

    fn simple() -> Vec<u8> {
        build(&[(1, CATALOG), (2, PAGES), (3, PAGE), (4, STREAM)], "")
    }

    fn update(index: usize, segments: &[Option<&[u8]>]) -> PageUpdate {
        PageUpdate {
            index,
            segments: segments.iter().map(|s| s.map(<[u8]>::to_vec)).collect(),
        }
    }

    /// 重新打开，读出每一页拼接后的内容。
    fn reread(bytes: &[u8]) -> Vec<Vec<u8>> {
        let doc = PdfDocument::load(bytes.to_vec()).expect("保存的结果打得开");
        assert!(!doc.rebuilt, "保存的结果不用重建交叉引用");
        walk_pages(&doc)
            .iter()
            .map(|page| {
                let page = page.as_ref().expect("页打得开");
                Content::read(&doc, &page.contents).data
            })
            .collect()
    }

    #[test]
    fn single_use_stream_is_replaced_in_place() {
        let raw = simple();
        let doc = PdfDocument::load(raw.clone()).unwrap();
        let pages = walk_pages(&doc);
        for compress in [false, true] {
            let saved = save_incremental(&doc, &pages, &[update(0, &[Some(b"0 g 1 G")])], compress)
                .unwrap();
            assert!(saved.bytes.starts_with(&raw), "原文件字节原样保留");
            assert!(saved.rejected.is_empty());
            assert_eq!(saved.pages[&0], [Some(4)]);
            assert_eq!(
                (
                    saved.streams_written,
                    saved.deshared_streams,
                    saved.page_dicts_rewritten
                ),
                (1, 0, 0)
            );
            assert_eq!(reread(&saved.bytes), [b"0 g 1 G".to_vec()]);
        }
    }

    #[test]
    fn nothing_to_write_returns_the_original_file() {
        let raw = simple();
        let doc = PdfDocument::load(raw.clone()).unwrap();
        let pages = walk_pages(&doc);
        let saved = save_incremental(&doc, &pages, &[update(0, &[None])], true).unwrap();
        assert_eq!(saved.bytes, raw);
        assert!(saved.pages.is_empty() && saved.rejected.is_empty());
        let saved = save_incremental(&doc, &pages, &[], true).unwrap();
        assert_eq!(saved.bytes, raw);
    }

    #[test]
    fn writing_back_the_same_bytes_reads_back_identical() {
        let raw = simple();
        let doc = PdfDocument::load(raw).unwrap();
        let pages = walk_pages(&doc);
        let saved = save_incremental(&doc, &pages, &[update(0, &[Some(b"q Q")])], true).unwrap();
        assert_eq!(saved.streams_written, 1);
        assert_eq!(reread(&saved.bytes), [b"q Q".to_vec()]);
    }

    #[test]
    fn empty_content_is_written_without_a_filter() {
        let raw = simple();
        let doc = PdfDocument::load(raw).unwrap();
        let pages = walk_pages(&doc);
        let saved = save_incremental(&doc, &pages, &[update(0, &[Some(b"")])], true).unwrap();
        let again = PdfDocument::load(saved.bytes.clone()).unwrap();
        let Some(PdfObject::Stream(stream)) = again.object(4) else {
            panic!("对象 4 应当还是流");
        };
        assert!(stream.dict.get(b"Filter").is_none(), "空内容不压缩");
        assert_eq!(stream.dict.get(b"Length"), Some(&PdfObject::Int(0)));
        assert!(stream.data.is_empty());
        assert_eq!(reread(&saved.bytes), [Vec::<u8>::new()]);
    }

    /// 两页共用内容流 4。
    fn shared() -> Vec<u8> {
        build(
            &[
                (1, CATALOG),
                (2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>"),
                (3, PAGE),
                (4, STREAM),
                (5, PAGE),
            ],
            "",
        )
    }

    #[test]
    fn shared_stream_gets_a_new_object_and_a_new_page_dict() {
        let raw = shared();
        let doc = PdfDocument::load(raw.clone()).unwrap();
        let pages = walk_pages(&doc);
        assert_eq!(pages.len(), 2);
        let saved = save_incremental(&doc, &pages, &[update(1, &[Some(b"0 g")])], true).unwrap();
        assert!(saved.rejected.is_empty());
        assert_eq!(
            (
                saved.streams_written,
                saved.deshared_streams,
                saved.page_dicts_rewritten
            ),
            (1, 1, 1)
        );
        assert_eq!(saved.pages[&1], [Some(6)]);
        assert_eq!(
            reread(&saved.bytes),
            [b"q Q".to_vec(), b"0 g".to_vec()],
            "第 0 页还是原来的内容"
        );
        // 页字典里别的键还在。
        let again = PdfDocument::load(saved.bytes).unwrap();
        let page = again.object(5).unwrap();
        let dict = page.as_dict().unwrap();
        assert_eq!(dict.get(b"Contents"), Some(&PdfObject::Ref(6, 0)));
        assert_eq!(dict.get(b"Parent"), Some(&PdfObject::Ref(2, 0)));
        assert!(dict.get(b"MediaBox").is_some());
        // 两页都改：各自一条新流。
        let both = [update(0, &[Some(b"1 g")]), update(1, &[Some(b"2 g")])];
        let saved = save_incremental(&doc, &pages, &both, false).unwrap();
        assert_eq!(saved.deshared_streams, 2);
        assert_eq!(reread(&saved.bytes), [b"1 g".to_vec(), b"2 g".to_vec()]);
    }

    #[test]
    fn array_contents_keep_untouched_items() {
        // 流 4 在同一页里用了两次；中间有一项不是流；流 6 只用了一次。
        let raw = build(
            &[
                (1, CATALOG),
                (2, PAGES),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                     /Contents [4 0 R null 4 0 R 6 0 R] >>",
                ),
                (4, STREAM),
                (6, "<< /Length 3 >>\nstream\n0 g\nendstream"),
            ],
            "",
        );
        let doc = PdfDocument::load(raw).unwrap();
        let pages = walk_pages(&doc);
        assert_eq!(reread(doc.bytes()), [b"q Q  q Q 0 g ".to_vec()]);
        let segments = [Some(&b"1 g"[..]), None, None, Some(&b"2 g"[..])];
        let saved = save_incremental(&doc, &pages, &[update(0, &segments)], true).unwrap();
        assert!(saved.rejected.is_empty());
        assert_eq!(saved.pages[&0], [Some(7), None, Some(4), Some(6)]);
        assert_eq!(
            (
                saved.streams_written,
                saved.deshared_streams,
                saved.page_dicts_rewritten
            ),
            (2, 1, 1)
        );
        assert_eq!(reread(&saved.bytes), [b"1 g  q Q 2 g ".to_vec()]);
        let again = PdfDocument::load(saved.bytes).unwrap();
        let page = again.object(3).unwrap();
        assert_eq!(
            page.as_dict().unwrap().get(b"Contents"),
            Some(&PdfObject::Array(vec![
                PdfObject::Ref(7, 0),
                PdfObject::Null,
                PdfObject::Ref(4, 0),
                PdfObject::Ref(6, 0),
            ]))
        );
        // 空内容写在不是流的那一项上：当作没有；非空的写不了。
        let saved = save_incremental(
            &doc,
            &pages,
            &[update(0, &[None, Some(b""), None, None])],
            true,
        )
        .unwrap();
        assert!(saved.rejected.is_empty() && saved.pages.is_empty());
        let saved = save_incremental(
            &doc,
            &pages,
            &[update(0, &[None, Some(b"q"), None, None])],
            true,
        )
        .unwrap();
        assert_eq!(saved.rejected.len(), 1);
        assert!(saved.rejected[0].1.contains("不是流"));
        assert_eq!(saved.bytes, doc.bytes(), "写不了的页一个字节都不动");
    }

    #[test]
    fn pages_that_cannot_be_written_are_reported() {
        let raw = simple();
        let doc = PdfDocument::load(raw.clone()).unwrap();
        let pages = walk_pages(&doc);
        let bad = [
            update(7, &[Some(b"q")]),
            update(0, &[Some(b"q"), Some(b"Q")]),
            update(0, &[Some(b"0 g")]),
            update(0, &[Some(b"1 g")]),
        ];
        let saved = save_incremental(&doc, &pages, &bad, true).unwrap();
        let reasons: Vec<&str> = saved.rejected.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(reasons.len(), 3);
        assert!(reasons[0].contains("打不开"));
        assert!(reasons[1].contains("给了 2 段"));
        assert!(reasons[2].contains("两次"));
        assert_eq!(
            reread(&saved.bytes),
            [b"0 g".to_vec()],
            "合格的那一次照常写"
        );

        // 页字典直接写在 /Kids 里，内容流又是共用的：要改页字典而改不了。
        let inline = "<< /Type /Page /MediaBox [0 0 10 10] /Contents 4 0 R >>";
        let raw = build(
            &[
                (1, CATALOG),
                (
                    2,
                    &format!("<< /Type /Pages /Kids [{inline} {inline}] /Count 2 >>"),
                ),
                (4, STREAM),
            ],
            "",
        );
        let doc = PdfDocument::load(raw.clone()).unwrap();
        let pages = walk_pages(&doc);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].as_ref().map(|p| p.number), Some(0));
        let saved = save_incremental(&doc, &pages, &[update(0, &[Some(b"0 g")])], true).unwrap();
        assert_eq!(saved.rejected.len(), 1);
        assert!(saved.rejected[0].1.contains("没有对象号"));
        assert_eq!(saved.bytes, raw);

        // 同一个页对象占了两个页号。
        let raw = build(
            &[
                (1, CATALOG),
                (2, "<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>"),
                (3, PAGE),
                (4, STREAM),
            ],
            "",
        );
        let doc = PdfDocument::load(raw.clone()).unwrap();
        let pages = walk_pages(&doc);
        assert_eq!(pages.len(), 2);
        let saved = save_incremental(&doc, &pages, &[update(1, &[Some(b"0 g")])], true).unwrap();
        assert_eq!(saved.rejected.len(), 1);
        assert!(saved.rejected[0].1.contains("不止一个页号"));
        assert_eq!(saved.bytes, raw);
    }

    #[test]
    fn full_rewrite_keeps_the_live_objects() {
        let raw = shared();
        let doc = PdfDocument::load(raw).unwrap();
        let pages = walk_pages(&doc);
        let saved = save_incremental(&doc, &pages, &[update(0, &[Some(b"0 g")])], false).unwrap();
        let edited = PdfDocument::load(saved.bytes.clone()).unwrap();
        let full = save_full_rewrite(&edited);
        assert!(full.len() < saved.bytes.len(), "旧版本的页字典不带了");
        assert_eq!(reread(&full), [b"0 g".to_vec(), b"q Q".to_vec()]);
    }
}
