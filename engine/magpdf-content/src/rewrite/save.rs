//! 写回：把改过的内容流用增量更新写进文件。
//!
//! - 一条内容流只被一页用了一次：**原地替换**（对象号、代号不变），页字典和页树不用动；
//! - 一条内容流被多页共用，或者在同一页的 `/Contents` 里出现了不止一次：不能原地替换（别处也会跟着
//!   变），另建一条流，并重写这一页的页字典，让 `/Contents` 指向新流；
//! - 页字典也不是这一页独有的（同一个页对象在页树里出现了好几次、占着好几个页号；或者页字典直接
//!   写在 `/Kids` 里，没有对象号）：给这个页号**另起一个页对象**，页树里指向它的那一项改成新对象。
//!   那一项所在的节点要是也在页树里出现了好几次，就连节点一起另起一份，一直往上到一个只出现一次、
//!   可以就地改写的节点为止（见 [`TreeEdits`]）。另起的对象除了 `/Contents`（节点是 `/Kids`）之外
//!   原样照抄，`/Parent` 也不动，所以继承来的属性和原来一样；
//! - 上面这些都做不到（比如页字典在对象流里而文件的交叉引用是表，往上也没有能改的节点）：这一页
//!   不写，记在 [`SavedDocument::rejected`] 里，由调用方交给 PDFium；
//! - 新内容是空的：写成不带过滤器、`/Length 0` 的流。压缩过的空内容解出来是空的，PDFium 这时会退回去
//!   用压缩后的原始字节当内容流。
//!
//! 一页要么全部写进去，要么完全不动：先检查，检查通过才登记。
//!
//! 没有发现的情况：内容流同时被别的地方引用（比如又被当成表单 XObject），原地替换会连带改到那里。
//! 页对象出现了几次是按 PDFium 认到的页数来数的（`/Count` 比实际的少时，后面那些 PDFium 看不到的
//! 位置不算）。

use std::collections::{BTreeMap, BTreeSet};

use crate::pdf::encode_flate;
use crate::pdf::object::{PdfDict, PdfObject, PdfStreamRef};
use crate::pdf::pages::{Contents, PageInfo, PathStep};
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
    /// 重写的页字典个数（含另起的）。
    pub page_dicts_rewritten: usize,
    /// 另起了页对象的页数（页对象原来不是这一页独有的）。
    pub pages_split: usize,
    /// 为此连带另起的页树节点个数。
    pub tree_nodes_split: usize,
    /// 写出的内容流个数。
    pub streams_written: usize,
}

/// 页字典怎么处理。
enum PagePlan {
    /// 不用动：内容流都是原地替换。
    Keep,
    /// 页对象只占这一个页号：原地重写。
    Rewrite(u32, PdfDict),
    /// 页字典不是这一页独有的：照抄一份另起页对象，页树里指向它的那一项跟着改。
    Split(PdfDict, Hook),
}

/// 一页的写法。检查通过之后才有。
struct Plan<'u> {
    /// 要写的段：段下标、原来的流对象号、新字节、是不是原地替换。
    writes: Vec<(usize, u32, &'u [u8], bool)>,
    page: PagePlan,
    /// `/Contents` 每一项原来的流对象号。
    slots: Vec<Option<u32>>,
}

/// 页树里要改 `/Kids` 的一个节点。
struct NodeEdit {
    /// 就地改写的对象号；`None` 是照抄一份另起新对象（这个节点在页树里不止出现在这一处，
    /// 或者没有对象号）。
    in_place: Option<u32>,
    /// 节点的字典。
    dict: PdfDict,
    /// 它的 `/Kids`，换过的项已经指向新对象。
    kids: Vec<PdfObject>,
}

/// 把页树里的一项换成新对象之前准备好的东西（[`TreeEdits::prepare`] 的结果）。
struct Hook {
    /// 还没登记过、这次要登记的节点（位置与内容）。
    nodes: Vec<(Vec<usize>, NodeEdit)>,
    /// 要换的那一项在哪个节点里（位置）、是第几项。
    place: Vec<usize>,
    kid: usize,
}

/// 页树的修改。
///
/// 同一个对象可以在页树里出现好几次，所以节点不按对象号认，按**位置**认：从根数下来每一层在
/// `/Kids` 里的下标。要把某个位置上的一项换成新对象：它所在的节点只出现在这一处、又能就地改写，
/// 就改它的 `/Kids`；否则这个节点也得照抄一份（只有这一处用新的），再去改上一层，如此往上。
/// 几页的修改可以落在同一个节点上，所以先都登记下来，最后从深到浅一起写。
struct TreeEdits<'a> {
    doc: &'a PdfDocument,
    /// 每个节点对象出现在哪些位置。
    places: BTreeMap<u32, BTreeSet<Vec<usize>>>,
    edits: BTreeMap<Vec<usize>, NodeEdit>,
}

/// 路径上第 `level` 层那个节点的位置。
fn place_of(path: &[PathStep], level: usize) -> Vec<usize> {
    path[..level].iter().map(|step| step.kid).collect()
}

impl<'a> TreeEdits<'a> {
    fn new(doc: &'a PdfDocument, pages: &[Option<PageInfo>]) -> Self {
        let mut places: BTreeMap<u32, BTreeSet<Vec<usize>>> = BTreeMap::new();
        for info in pages.iter().flatten() {
            for (level, step) in info.path.iter().enumerate() {
                if step.node != 0 {
                    places
                        .entry(step.node)
                        .or_default()
                        .insert(place_of(&info.path, level));
                }
            }
        }
        Self {
            doc,
            places,
            edits: BTreeMap::new(),
        }
    }

    /// 路径上第 `level` 层的节点：字典和 `/Kids`。已经登记过修改的取登记的那一份。
    fn node(&self, path: &[PathStep], level: usize) -> Option<(PdfDict, Vec<PdfObject>)> {
        if let Some(edit) = self.edits.get(&place_of(path, level)) {
            return Some((edit.dict.clone(), edit.kids.clone()));
        }
        let object = match (path[level].node, level) {
            // 页树的根直接写在目录里。
            (0, 0) => return None,
            // 节点的字典直接写在上一层的 `/Kids` 里。
            (0, _) => {
                let (_, parent_kids) = self.node(path, level - 1)?;
                self.doc.resolve(parent_kids.get(path[level - 1].kid))?
            }
            (number, _) => self.doc.object(number)?,
        };
        let dict = object.as_dict()?.clone();
        let kids = match self.doc.resolve(dict.get(b"Kids")) {
            Some(PdfObject::Array(kids)) => kids,
            _ => return None,
        };
        Some((dict, kids))
    }

    /// 这个节点能不能就地改写：有对象号、在页树里只出现在一处、是个普通的字典对象、写得回去。
    fn in_place(&self, number: u32, writer: &IncrementalWriter<'_>) -> bool {
        number != 0
            && self.places.get(&number).is_some_and(|p| p.len() == 1)
            && matches!(self.doc.object(number), Some(PdfObject::Dict(_)))
            && writer.check_replace(number, false).is_ok()
    }

    /// 准备把 `path` 最后一步指的那一项换成新对象：从它所在的节点往上找，找到第一个能就地改写
    /// （或者已经登记过修改）的节点为止，中间的节点都要照抄。找不到就做不了。这里什么都不登记。
    fn prepare(&self, path: &[PathStep], writer: &IncrementalWriter<'_>) -> Result<Hook, String> {
        let Some(last) = path.len().checked_sub(1) else {
            return Err("页树的根自己就是这一页，没有上一层可改".to_owned());
        };
        let mut nodes = Vec::new();
        for level in (0..=last).rev() {
            let place = place_of(path, level);
            let anchored = self.edits.contains_key(&place);
            if !anchored {
                if level == 0 && path[0].node == 0 {
                    return Err("页树的根直接写在目录里，没有对象号，改不了".to_owned());
                }
                let Some((dict, kids)) = self.node(path, level) else {
                    return Err("页树的节点读不出来".to_owned());
                };
                if path[level].kid >= kids.len() {
                    return Err("页树节点的 /Kids 和读到的对不上".to_owned());
                }
                let in_place = self.in_place(path[level].node, writer);
                nodes.push((
                    place,
                    NodeEdit {
                        in_place: in_place.then_some(path[level].node),
                        dict,
                        kids,
                    },
                ));
                if !in_place {
                    continue;
                }
            }
            return Ok(Hook {
                nodes,
                place: place_of(path, last),
                kid: path[last].kid,
            });
        }
        Err("从这一页往上，页树里没有能就地改写的节点".to_owned())
    }

    /// 登记：把准备好的那一项换成对象 `replacement`。
    fn apply(&mut self, hook: Hook, replacement: u32) {
        for (place, edit) in hook.nodes {
            self.edits.entry(place).or_insert(edit);
        }
        if let Some(edit) = self.edits.get_mut(&hook.place)
            && let Some(slot) = edit.kids.get_mut(hook.kid)
        {
            *slot = PdfObject::Ref(replacement, 0);
        }
    }

    /// 写出登记的全部修改，返回另起的节点个数。从深到浅：照抄的节点先拿到新对象号，上一层的
    /// `/Kids` 才知道该指向谁。
    fn write(mut self, writer: &mut IncrementalWriter<'_>) -> Result<usize, WriteError> {
        let mut places: Vec<Vec<usize>> = self.edits.keys().cloned().collect();
        places.sort_by_key(|place| std::cmp::Reverse(place.len()));
        let mut split = 0usize;
        for place in places {
            let Some(mut edit) = self.edits.remove(&place) else {
                continue;
            };
            edit.dict.set(b"Kids", PdfObject::Array(edit.kids));
            match edit.in_place {
                Some(number) => writer.replace(number, PdfObject::Dict(edit.dict))?,
                None => {
                    let number = writer.add(PdfObject::Dict(edit.dict));
                    split += 1;
                    if let Some((kid, parent)) = place.split_last()
                        && let Some(parent) = self.edits.get_mut(parent)
                        && let Some(slot) = parent.kids.get_mut(*kid)
                    {
                        *slot = PdfObject::Ref(number, 0);
                    }
                }
            }
        }
        Ok(split)
    }
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
    let mut tree = TreeEdits::new(doc, pages);
    let mut saved = SavedDocument::default();
    for update in updates {
        if saved.pages.contains_key(&update.index) {
            saved
                .rejected
                .push((update.index, "同一页给了两次".to_owned()));
            continue;
        }
        let plan = match check(doc, pages, update, &stream_uses, &page_uses, &writer, &tree) {
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
        match plan.page {
            PagePlan::Keep => {}
            PagePlan::Rewrite(number, mut dict) => {
                set_contents(doc, &mut dict, &plan.slots, &slots);
                writer.replace(number, PdfObject::Dict(dict))?;
                saved.page_dicts_rewritten += 1;
            }
            PagePlan::Split(mut dict, hook) => {
                set_contents(doc, &mut dict, &plan.slots, &slots);
                let number = writer.add(PdfObject::Dict(dict));
                tree.apply(hook, number);
                saved.page_dicts_rewritten += 1;
                saved.pages_split += 1;
            }
        }
        saved.pages.insert(update.index, slots);
    }
    saved.tree_nodes_split = tree.write(&mut writer)?;
    saved.bytes = writer.finish()?;
    Ok(saved)
}

/// 把页字典的 `/Contents` 改成指向新流。`old`、`new` 是每一项原来的和现在的流对象号。
fn set_contents(doc: &PdfDocument, dict: &mut PdfDict, old: &[Option<u32>], new: &[Option<u32>]) {
    let contents = match doc.resolve(dict.get(b"Contents")) {
        // 数组：没换的项照原样留着（包括不是流的项），换了的项指向新流。
        Some(PdfObject::Array(items)) => PdfObject::Array(
            items
                .into_iter()
                .zip(old.iter().zip(new))
                .map(|(item, (old, new))| match new {
                    Some(new) if old != &Some(*new) => PdfObject::Ref(*new, 0),
                    _ => item,
                })
                .collect(),
        ),
        _ => match new.first() {
            Some(Some(new)) => PdfObject::Ref(*new, 0),
            _ => PdfObject::Null,
        },
    };
    dict.set(b"Contents", contents);
}

/// 检查一页写不写得了。`Ok(None)` 是没有要写的东西。
fn check<'u>(
    doc: &PdfDocument,
    pages: &[Option<PageInfo>],
    update: &'u PageUpdate,
    stream_uses: &BTreeMap<u32, usize>,
    page_uses: &BTreeMap<u32, usize>,
    writer: &IncrementalWriter<'_>,
    tree: &TreeEdits<'_>,
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
        // 页字典：有对象号的取文件里的那一份，直接写在 /Kids 里的取读页树时拿到的。
        let dict = if info.number == 0 {
            info.dict.clone()
        } else {
            match doc.object(info.number) {
                Some(PdfObject::Dict(dict)) => dict,
                _ => return Err(format!("{WHY}，而页对象不是字典")),
            }
        };
        if matches!(info.contents, Contents::Array(_)) {
            match doc.resolve(dict.get(b"Contents")) {
                Some(PdfObject::Array(items)) if items.len() == slots.len() => {}
                _ => return Err(format!("{WHY}，而页字典的 /Contents 和读到的对不上")),
            }
        }
        let own = info.number != 0 && page_uses.get(&info.number).copied().unwrap_or(0) == 1;
        match writer.check_replace(info.number, false) {
            Ok(()) if own => PagePlan::Rewrite(info.number, dict),
            // 页字典不是这一页独有的，或者就地写不回去：另起一个页对象，改页树。
            in_place => {
                let hook = tree.prepare(&info.path, writer).map_err(|why| {
                    let because = if info.number == 0 {
                        "页字典直接写在 /Kids 里".to_owned()
                    } else if !own {
                        "这个页对象占了不止一个页号".to_owned()
                    } else {
                        in_place.err().map_or_else(String::new, |e| e.to_string())
                    };
                    format!("{WHY}；{because}，要另起页对象，而{why}")
                })?;
                PagePlan::Split(dict, hook)
            }
        }
    } else {
        PagePlan::Keep
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

        // 同一个页对象占了两个页号，页树的根又直接写在目录里：要另起页对象而页树改不了。
        let raw = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages << /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >> >>",
                ),
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
        assert!(saved.rejected[0].1.contains("直接写在目录里"));
        assert_eq!(saved.bytes, raw, "写不了的页一个字节都不动");
    }

    /// 重新打开，列出每一页的页对象号。
    fn page_numbers(bytes: &[u8]) -> Vec<u32> {
        let doc = PdfDocument::load(bytes.to_vec()).unwrap();
        walk_pages(&doc)
            .iter()
            .map(|page| page.as_ref().expect("页打得开").number)
            .collect()
    }

    #[test]
    fn a_page_object_used_for_several_pages_is_split() {
        // 同一个页对象在 /Kids 里出现两次，占两个页号。
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
        // 只改第 1 页：它另起一个页对象，第 0 页还用原来的。
        let saved = save_incremental(&doc, &pages, &[update(1, &[Some(b"0 g")])], true).unwrap();
        assert!(saved.rejected.is_empty());
        assert!(saved.bytes.starts_with(&raw));
        assert_eq!(
            (
                saved.pages_split,
                saved.tree_nodes_split,
                saved.deshared_streams,
                saved.page_dicts_rewritten
            ),
            (1, 0, 1, 1)
        );
        assert_eq!(reread(&saved.bytes), [b"q Q".to_vec(), b"0 g".to_vec()]);
        let numbers = page_numbers(&saved.bytes);
        assert_eq!(numbers[0], 3);
        assert_ne!(numbers[1], 3);
        // 新的页对象除了 /Contents 照抄原来的。
        let again = PdfDocument::load(saved.bytes).unwrap();
        let page = again.object(numbers[1]).unwrap();
        let dict = page.as_dict().unwrap();
        assert_eq!(dict.get(b"Parent"), Some(&PdfObject::Ref(2, 0)));
        assert!(dict.get(b"MediaBox").is_some());
        assert_eq!(
            dict.get(b"Type").and_then(PdfObject::as_name),
            Some(&b"Page"[..])
        );
        // 两个页号都改：各有各的页对象、各有各的内容。
        let both = [update(0, &[Some(b"1 g")]), update(1, &[Some(b"2 g")])];
        let saved = save_incremental(&doc, &pages, &both, true).unwrap();
        assert!(saved.rejected.is_empty());
        assert_eq!(saved.pages_split, 2);
        assert_eq!(reread(&saved.bytes), [b"1 g".to_vec(), b"2 g".to_vec()]);
        let numbers = page_numbers(&saved.bytes);
        assert!(numbers[0] != numbers[1] && !numbers.contains(&3));
    }

    #[test]
    fn shared_tree_nodes_are_split_along_the_path() {
        // 中间节点 5 在根的 /Kids 里出现两次，页对象 3 在节点 5 里出现三次：一共 6 页，全是同一个
        // 页对象。没有 /Count（PDFium 自己数）。
        let raw = build(
            &[
                (1, CATALOG),
                (2, "<< /Type /Pages /Kids [5 0 R 5 0 R] >>"),
                (3, PAGE),
                (4, STREAM),
                (5, "<< /Type /Pages /Kids [3 0 R 3 0 R 3 0 R] >>"),
            ],
            "",
        );
        let doc = PdfDocument::load(raw).unwrap();
        let pages = walk_pages(&doc);
        assert_eq!(pages.len(), 6);
        let path = &pages[4].as_ref().unwrap().path;
        assert_eq!(
            path.iter().map(|s| (s.node, s.kid)).collect::<Vec<_>>(),
            [(2, 1), (5, 1)]
        );
        // 改第 0、1、4 页：前两页落在节点 5 的第一次出现里（共用一份照抄的节点），第 4 页在第二次里。
        let updates = [
            update(0, &[Some(b"1 g")]),
            update(4, &[Some(b"5 g")]),
            update(1, &[Some(b"2 g")]),
        ];
        let saved = save_incremental(&doc, &pages, &updates, true).unwrap();
        assert!(saved.rejected.is_empty());
        assert_eq!((saved.pages_split, saved.tree_nodes_split), (3, 2));
        let plain = b"q Q".to_vec();
        assert_eq!(
            reread(&saved.bytes),
            [
                b"1 g".to_vec(),
                b"2 g".to_vec(),
                plain.clone(),
                plain.clone(),
                b"5 g".to_vec(),
                plain.clone()
            ]
        );
        // 没改的页还是原来的页对象。
        let numbers = page_numbers(&saved.bytes);
        assert_eq!((numbers[2], numbers[3], numbers[5]), (3, 3, 3));
        // 在保存结果上再改一次：这回节点和页对象大多已经是各自独有的了。
        let again = PdfDocument::load(saved.bytes).unwrap();
        let pages = walk_pages(&again);
        let second = save_incremental(
            &again,
            &pages,
            &[update(0, &[Some(b"9 g")]), update(3, &[Some(b"8 g")])],
            true,
        )
        .unwrap();
        assert!(second.rejected.is_empty());
        assert_eq!(
            reread(&second.bytes),
            [
                b"9 g".to_vec(),
                b"2 g".to_vec(),
                plain.clone(),
                b"8 g".to_vec(),
                b"5 g".to_vec(),
                plain
            ]
        );
    }

    #[test]
    fn page_and_node_dicts_written_inline_in_kids() {
        // 页字典直接写在 /Kids 里（没有对象号），两页共用内容流 4。
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
        let doc = PdfDocument::load(raw).unwrap();
        let pages = walk_pages(&doc);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].as_ref().map(|p| p.number), Some(0));
        let saved = save_incremental(&doc, &pages, &[update(0, &[Some(b"0 g")])], true).unwrap();
        assert!(saved.rejected.is_empty());
        assert_eq!((saved.pages_split, saved.tree_nodes_split), (1, 0));
        assert_eq!(reread(&saved.bytes), [b"0 g".to_vec(), b"q Q".to_vec()]);
        let numbers = page_numbers(&saved.bytes);
        assert!(numbers[0] != 0 && numbers[1] == 0, "第 1 页还是直接写着的");

        // 中间节点直接写在根的 /Kids 里，它下面同一个页对象出现两次。
        let raw = build(
            &[
                (1, CATALOG),
                (
                    2,
                    "<< /Type /Pages /Count 2 /Kids \
                     [<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>] >>",
                ),
                (3, PAGE),
                (4, STREAM),
            ],
            "",
        );
        let doc = PdfDocument::load(raw).unwrap();
        let pages = walk_pages(&doc);
        assert_eq!(pages.len(), 2);
        let saved = save_incremental(&doc, &pages, &[update(1, &[Some(b"0 g")])], true).unwrap();
        assert!(saved.rejected.is_empty());
        assert_eq!((saved.pages_split, saved.tree_nodes_split), (1, 1));
        assert_eq!(reread(&saved.bytes), [b"q Q".to_vec(), b"0 g".to_vec()]);
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
