//! 页面树：有几页、第 i 页是哪个对象、它的 `/Contents` 和资源是什么。
//!
//! 要和 PDFium 的页序逐页对上，所以数页数、找第 i 页、取继承属性、算页面尺寸都照 PDFium 的做法
//! 移植（`CPDF_Document::RetrievePageCount` / `CountPages` / `TraversePDFPages`、
//! `CPDF_Page::GetPageAttr` / `UpdateDimensions`、`CPDF_ContentParser` 的构造函数；BSD-3-Clause，
//! 版权声明见本 crate 的 `PDFIUM-NOTICE.md`）。几条和“按规范应该怎样”不同的地方：
//!
//! - 页数：节点的 `/Count` 在 `(0, 0xFFFFF)` 之内就直接信它，不数子节点；
//! - 找页：深度优先走 `/Kids`；没有 `/Kids` 键的子节点就是一页（不看 `/Type`）；子节点取不到或者
//!   不是字典时占一个页号，但那一页打不开；找到的页数和页数对不上时，多出来的页号打不开；
//! - 一页的 `/Type` 写了但不是 `Page` 时打不开；
//! - 继承属性沿 `/Parent` 链找，第一个取得到值的为准（值是 null 也算取到了）；
//! - `/Contents` 不继承；是流就是单条流，是非空数组就逐项取（不是流的项当作空流），别的都算没有内容。

use std::collections::BTreeSet;

use super::object::{PdfDict, PdfObject};
use super::reader::PdfDocument;

/// `CPDF_Document::kPageMaxNum`。
const PAGE_MAX_NUM: i32 = 0xFFFFF;
/// `kMaxPageLevel`。
const MAX_PAGE_LEVEL: usize = 1024;
/// 数页数时的递归深度上限（PDFium 没有这个上限，靠已访问集合防环；这里再加一道防栈溢出）。
const MAX_COUNT_DEPTH: usize = 256;

/// 从页树的根走到一页，经过的一层：在哪个节点的 `/Kids` 里、第几项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PathStep {
    /// 节点的对象号。节点的字典直接写在别处（上一层的 `/Kids` 里，或者目录的 `/Pages` 里）时是 0。
    pub node: u32,
    /// 在这个节点的 `/Kids` 里的下标。
    pub kid: usize,
}

/// 一页的结构信息。
#[derive(Debug, Clone, PartialEq)]
pub struct PageInfo {
    /// 页序（0 起），与 PDFium 的页号一致。
    pub index: usize,
    /// 页面对象号。页字典直接写在 `/Kids` 数组里（没有对象号）时是 0。
    pub number: u32,
    /// 页字典。
    pub dict: PdfDict,
    pub contents: Contents,
    /// 资源字典（含继承）。
    pub resources: Option<PdfDict>,
    /// 从页树的根到这一页的路径，根在前。同一个页对象（或者同一个中间节点）可以在页树里出现
    /// 好几次、占好几个页号，这时对象号相同而路径不同。页树的根自己就是一页时为空。
    pub path: Vec<PathStep>,
}

/// `/Contents` 的形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contents {
    /// 没有内容：键不存在、取不到、既不是流也不是数组、或者是空数组。
    None,
    /// 单条流的对象号。
    Single(u32),
    /// 流数组。每项是流的对象号；不是流的项是 `None`（PDFium 把它当空流，照样占一段）。
    Array(Vec<Option<u32>>),
}

impl Contents {
    /// 用到的流对象号（按出现顺序，含重复）。
    pub fn stream_numbers(&self) -> Vec<u32> {
        match self {
            Self::None => Vec::new(),
            Self::Single(n) => vec![*n],
            Self::Array(items) => items.iter().flatten().copied().collect(),
        }
    }
}

/// 页面尺寸与旋转（`CPDF_Page::UpdateDimensions`）。宽高是旋转之后的。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub width: f32,
    pub height: f32,
    /// 0–3，单位 90°。
    pub rotation: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeType {
    Branch,
    Leaf,
}

/// `GetDictFor`：解一层引用，字典或流（取流的字典）。
fn dict_for(doc: &PdfDocument, dict: &PdfDict, key: &[u8]) -> Option<PdfDict> {
    doc.resolve(dict.get(key))
        .and_then(|o| o.as_dict().cloned())
}

/// `GetArrayFor`：解一层引用，必须是数组。
fn array_for(doc: &PdfDocument, dict: &PdfDict, key: &[u8]) -> Option<Vec<PdfObject>> {
    match doc.resolve(dict.get(key)) {
        Some(PdfObject::Array(items)) => Some(items),
        _ => None,
    }
}

/// `GetNodeType`：`/Type`（只认直接写的名字）是 `Pages` 或 `Page` 时照它；否则看有没有 `/Kids`。
/// 第二个返回值表示类型是猜出来的（PDFium 这时会把内存里的 `/Type` 改成猜的结果）。
fn node_type(dict: &PdfDict) -> (NodeType, bool) {
    match dict.get(b"Type").and_then(PdfObject::as_name) {
        Some(b"Pages") => (NodeType::Branch, false),
        Some(b"Page") => (NodeType::Leaf, false),
        _ => {
            if dict.get(b"Kids").is_some() {
                (NodeType::Branch, true)
            } else {
                (NodeType::Leaf, true)
            }
        }
    }
}

/// 数页数的结果，连同“哪些对象的 `/Type` 被改成了 `Page`”。
struct Counted {
    count: usize,
    /// 数的过程中 `/Type` 被 PDFium 在内存里改成 `Page` 的对象号。
    fixed_as_page: BTreeSet<u32>,
}

/// `CountPages`。`None` 表示出错（页太多）。
fn count_pages(
    doc: &PdfDocument,
    pages: &PdfDict,
    visited: &mut BTreeSet<u32>,
    fixed_as_page: &mut BTreeSet<u32>,
    depth: usize,
) -> Option<i32> {
    let from_dict = doc
        .resolve(pages.get(b"Count"))
        .map_or(0, |v| v.get_integer());
    if from_dict > 0 && from_dict < PAGE_MAX_NUM {
        return Some(from_dict);
    }
    let Some(kids) = array_for(doc, pages, b"Kids") else {
        return Some(0);
    };
    if depth >= MAX_COUNT_DEPTH {
        return None;
    }
    let mut count = 0i32;
    for kid in &kids {
        let number = kid.as_ref().map(|(n, _)| n);
        let Some(kid_dict) = doc.resolve(Some(kid)).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        if number.is_some_and(|n| visited.contains(&n)) {
            continue;
        }
        match node_type(&kid_dict) {
            (NodeType::Branch, _) => {
                if let Some(n) = number {
                    visited.insert(n);
                }
                let local = count_pages(doc, &kid_dict, visited, fixed_as_page, depth + 1);
                if let Some(n) = number {
                    visited.remove(&n);
                }
                count = count.saturating_add(local?);
            }
            (NodeType::Leaf, guessed) => {
                if guessed && let Some(n) = number {
                    fixed_as_page.insert(n);
                }
                count += 1;
            }
        }
        if count >= PAGE_MAX_NUM {
            return None;
        }
    }
    Some(count)
}

/// 页树的根：`(对象号, 字典)`。
fn pages_root(doc: &PdfDocument) -> Option<(u32, PdfDict)> {
    let root = doc.root()?;
    let number = root
        .get(b"Pages")
        .and_then(PdfObject::as_ref)
        .map_or(0, |(n, _)| n);
    Some((number, dict_for(doc, &root, b"Pages")?))
}

fn counted(doc: &PdfDocument) -> Counted {
    let mut fixed_as_page = BTreeSet::new();
    let Some((number, pages)) = pages_root(doc) else {
        return Counted {
            count: 0,
            fixed_as_page,
        };
    };
    if pages.get(b"Kids").is_none() {
        return Counted {
            count: 1,
            fixed_as_page,
        };
    }
    let mut visited = BTreeSet::new();
    if number != 0 {
        visited.insert(number);
    }
    let count = count_pages(doc, &pages, &mut visited, &mut fixed_as_page, 0).unwrap_or(0);
    Counted {
        count: usize::try_from(count).unwrap_or(0),
        fixed_as_page,
    }
}

/// PDFium 眼里的页数（`RetrievePageCount`）。
pub fn page_count(doc: &PdfDocument) -> usize {
    counted(doc).count
}

/// `IsValidPageDictLoose`：`/Type` 没写，或者（解一层引用之后）是名字 `Page`。
fn is_valid_page_dict(doc: &PdfDocument, dict: &PdfDict) -> bool {
    match dict.get(b"Type") {
        None => true,
        Some(value) => doc
            .resolve(Some(value))
            .is_some_and(|v| v.as_name() == Some(b"Page")),
    }
}

/// 遍历中的一层。
struct Frame {
    number: u32,
    kids: Vec<PdfObject>,
    next: usize,
}

/// 列出 PDFium 的每一页。返回的长度就是 PDFium 的页数；`None` 表示 PDFium 打不开这一页。
///
/// 相当于按页号从小到大依次调用 `CPDF_Document::GetPageDictionary`。
pub fn walk_pages(doc: &PdfDocument) -> Vec<Option<PageInfo>> {
    walk(doc).0
}

/// 这份文件交给 PDFium 的内容流生成器会不会让进程中止。
///
/// 生成器写资源字典之前（`CPDF_PageContentGenerator::UpdateResourcesDict` →
/// `IsPageResourceShared`）要给文档的**每一页**各构造一个 `CPDF_Page`，而构造函数里有
/// `CHECK(IsValidPageDictLoose(…))`：只要有一页的字典取得到、`/Type` 写了却不是 `Page`
/// （`FPDF_LoadPage` 对这种页只是返回失败），进程就在这里中止。原版和修补版都一样
/// （公开测试集的 `bad_page_type.pdf`）。
///
/// 这里的判断偏保守：`IsPageResourceShared` 在走到那一页之前就得出结论的情形也算在内。
pub fn generator_may_abort(doc: &PdfDocument) -> bool {
    walk(doc).1
}

/// [`walk_pages`] 的实现。第二个返回值：有没有哪一页的字典取得到、但类型不对（见
/// [`generator_may_abort`]）。
fn walk(doc: &PdfDocument) -> (Vec<Option<PageInfo>>, bool) {
    let Counted {
        count,
        fixed_as_page,
    } = counted(doc);
    let mut found: Vec<Option<(u32, PdfDict, Vec<PathStep>)>> = Vec::new();
    if let Some((root_number, root)) = pages_root(doc) {
        match array_for(doc, &root, b"Kids") {
            None => {
                // 根节点没有 `/Kids` 数组：类型不是页树节点时，它自己就是第 0 页。
                // （类型是猜出来的时候 PDFium 把它的 `/Type` 改成 `Page`，所以一定打得开。）
                match node_type(&root) {
                    (NodeType::Leaf, guessed) => {
                        let mut dict = root.clone();
                        if guessed {
                            dict.set(b"Type", PdfObject::Name(b"Page".to_vec()));
                        }
                        found.push(Some((root_number, dict, Vec::new())));
                    }
                    (NodeType::Branch, _) => {}
                }
            }
            Some(kids) => {
                let mut stack = vec![Frame {
                    number: root_number,
                    kids,
                    next: 0,
                }];
                while found.len() < count {
                    let Some(frame) = stack.last_mut() else {
                        break;
                    };
                    let Some(kid) = frame.kids.get(frame.next).cloned() else {
                        // 这一层走完了，回到上一层的下一个子节点。
                        stack.pop();
                        if let Some(parent) = stack.last_mut() {
                            parent.next += 1;
                        }
                        continue;
                    };
                    let number = kid.as_ref().map_or(0, |(n, _)| n);
                    let Some(dict) = doc.resolve(Some(&kid)).and_then(|o| o.as_dict().cloned())
                    else {
                        // 取不到或者不是字典：占一个页号，那一页打不开。
                        found.push(None);
                        frame.next += 1;
                        continue;
                    };
                    if number != 0 && number == frame.number {
                        // 子节点就是它自己：跳过。
                        frame.next += 1;
                        continue;
                    }
                    if dict.get(b"Kids").is_none() {
                        // 每一层的 `next` 此刻正指着路径上的那一项。
                        let path = stack
                            .iter()
                            .map(|f| PathStep {
                                node: f.number,
                                kid: f.next,
                            })
                            .collect();
                        if let Some(frame) = stack.last_mut() {
                            frame.next += 1;
                        }
                        found.push(Some((number, dict, path)));
                        continue;
                    }
                    let Some(kids) = array_for(doc, &dict, b"Kids") else {
                        // 有 `/Kids` 键但不是数组：PDFium 不给它页号。
                        frame.next += 1;
                        continue;
                    };
                    if stack.len() >= MAX_PAGE_LEVEL {
                        // 嵌套太深（多半是环）：PDFium 从此一页都找不到。
                        break;
                    }
                    stack.push(Frame {
                        number,
                        kids,
                        next: 0,
                    });
                }
            }
        }
    }
    found.truncate(count);
    found.resize(count, None);
    let mut bad_type = false;
    let pages = found
        .into_iter()
        .enumerate()
        .map(|(index, node)| {
            let (number, dict, path) = node?;
            let type_fixed = number != 0 && fixed_as_page.contains(&number);
            if !type_fixed && !is_valid_page_dict(doc, &dict) {
                bad_type = true;
                return None;
            }
            let contents = contents_of(doc, &dict);
            let resources = page_attr(doc, &dict, b"Resources").and_then(|o| o.as_dict().cloned());
            Some(PageInfo {
                index,
                number,
                dict,
                contents,
                resources,
                path,
            })
        })
        .collect();
    (pages, bad_type)
}

/// 页的 `/Contents`（`CPDF_ContentParser` 的构造函数）。
fn contents_of(doc: &PdfDocument, dict: &PdfDict) -> Contents {
    let raw = dict.get(b"Contents");
    match doc.resolve(raw) {
        Some(PdfObject::Stream(_)) => match raw {
            Some(PdfObject::Ref(number, _)) => Contents::Single(*number),
            _ => Contents::None,
        },
        Some(PdfObject::Array(items)) if !items.is_empty() => Contents::Array(
            items
                .iter()
                .map(|item| match (item, doc.resolve(Some(item))) {
                    (PdfObject::Ref(number, _), Some(PdfObject::Stream(_))) => Some(*number),
                    _ => None,
                })
                .collect(),
        ),
        _ => Contents::None,
    }
}

/// `CPDF_Page::GetPageAttr`：沿 `/Parent` 链找第一个取得到值的。
pub fn page_attr(doc: &PdfDocument, page: &PdfDict, name: &[u8]) -> Option<PdfObject> {
    // 已经看过的节点（按 `/Parent` 引用的对象号认）；页字典自己没有对象号可认，另加步数上限。
    let mut visited: BTreeSet<u32> = BTreeSet::new();
    let mut current = page.clone();
    for _ in 0..4096 {
        if let Some(value) = doc.resolve(current.get(name)) {
            return Some(value);
        }
        let parent = current.get(b"Parent")?;
        if let Some((number, _)) = parent.as_ref()
            && !visited.insert(number)
        {
            return None;
        }
        current = doc.resolve(Some(parent))?.as_dict()?.clone();
    }
    None
}

/// 一个矩形：左、下、右、上。
type Rect = [f32; 4];

/// `CPDF_Page::GetBox`：属性得是 4 项的数组，否则是空矩形；取出来之后规整成左下、右上。
fn page_box(doc: &PdfDocument, page: &PdfDict, name: &[u8]) -> Rect {
    let Some(PdfObject::Array(items)) = page_attr(doc, page, name) else {
        return [0.0; 4];
    };
    if items.len() != 4 {
        return [0.0; 4];
    }
    let at = |i: usize| doc.resolve(Some(&items[i])).map_or(0.0, |v| v.get_number());
    normalize([at(0), at(1), at(2), at(3)])
}

fn normalize(r: Rect) -> Rect {
    [
        r[0].min(r[2]),
        r[1].min(r[3]),
        r[0].max(r[2]),
        r[1].max(r[3]),
    ]
}

fn is_empty(r: Rect) -> bool {
    r[0] >= r[2] || r[1] >= r[3]
}

/// `CPDF_Page::UpdateDimensions` 加 `GetPageRotation`。
pub fn page_geometry(doc: &PdfDocument, page: &PdfDict) -> Geometry {
    let mut media = page_box(doc, page, b"MediaBox");
    if is_empty(media) {
        media = [0.0, 0.0, 612.0, 792.0];
    }
    let mut bbox = page_box(doc, page, b"CropBox");
    if is_empty(bbox) {
        bbox = media;
    } else {
        // `CFX_FloatRect::Intersect`：不相交时得空矩形。
        bbox = [
            bbox[0].max(media[0]),
            bbox[1].max(media[1]),
            bbox[2].min(media[2]),
            bbox[3].min(media[3]),
        ];
        if bbox[0] > bbox[2] || bbox[1] > bbox[3] {
            bbox = [0.0; 4];
        }
    }
    let rotation = page_attr(doc, page, b"Rotate").map_or(0, |v| (v.get_integer() / 90) % 4);
    let rotation = if rotation < 0 { rotation + 4 } else { rotation };
    let (mut width, mut height) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
    if rotation == 1 || rotation == 3 {
        std::mem::swap(&mut width, &mut height);
    }
    Geometry {
        width,
        height,
        rotation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::reader::tests::build;

    fn pages_of(objects: &[(u32, &str)]) -> (PdfDocument, Vec<Option<PageInfo>>) {
        let doc = PdfDocument::load(build(objects, "")).expect("能打开");
        let pages = walk_pages(&doc);
        (doc, pages)
    }

    const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
    const STREAM: &str = "<< /Length 3 >>\nstream\nq Q\nendstream";

    #[test]
    fn count_is_trusted_when_plausible() {
        // /Count 说 3 页，实际只有 1 个子节点：PDFium 认为有 3 页，后两页打不开。
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
        ]);
        assert_eq!(pages.len(), 3);
        assert!(pages[0].is_some() && pages[1].is_none() && pages[2].is_none());

        // /Count 偏小：多出来的页看不到。
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R >>"),
        ]);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].as_ref().map(|p| p.number), Some(3));
    }

    #[test]
    fn missing_or_bad_count_is_recounted() {
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 0 >>"),
            (3, "<< /Type /Pages /Kids [4 0 R] >>"),
            (4, "<< /Type /Page /Parent 3 0 R >>"),
            (5, "<< /Parent 2 0 R >>"),
        ]);
        assert_eq!(
            pages
                .iter()
                .map(|p| p.as_ref().map(|p| p.number))
                .collect::<Vec<_>>(),
            [Some(4), Some(5)]
        );
    }

    #[test]
    fn null_kids_take_a_slot_and_self_reference_is_skipped() {
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            // 9 0 R 不存在；2 0 R 是它自己。/Count 3 被采信。
            (2, "<< /Type /Pages /Kids [9 0 R 2 0 R 3 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
        ]);
        assert_eq!(pages.len(), 3);
        assert!(pages[0].is_none());
        assert_eq!(pages[1].as_ref().map(|p| p.number), Some(3));
        assert!(pages[2].is_none());
    }

    #[test]
    fn wrong_type_page_cannot_be_loaded_unless_count_was_recounted() {
        // /Count 被采信：/Type 不对的页打不开。
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Foo /Parent 2 0 R >>"),
        ]);
        assert_eq!(pages, [None]);
        // /Count 不可信：数页数时 PDFium 把它的 /Type 改成了 Page，打得开。
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R] >>"),
            (3, "<< /Type /Foo /Parent 2 0 R >>"),
        ]);
        assert_eq!(pages.len(), 1);
        assert!(pages[0].is_some());
    }

    #[test]
    fn generator_aborts_only_on_a_page_dict_of_the_wrong_type() {
        let may_abort = |objects: &[(u32, &str)]| {
            generator_may_abort(&PdfDocument::load(build(objects, "")).expect("能打开"))
        };
        // bad_page_type.pdf 的结构：第二页的字典取得到，/Type 却不是 Page。
        assert!(may_abort(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (4, "<< /Type /Template /Parent 2 0 R >>"),
        ]));
        // 类型写成引用、指向的不是名字 Page，也一样。
        assert!(may_abort(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type 4 0 R /Parent 2 0 R >>"),
            (4, "(Page)"),
        ]));
        // 数页数时被 PDFium 改成了 Page 的不算。
        assert!(!may_abort(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R] >>"),
            (3, "<< /Type /Template /Parent 2 0 R >>"),
        ]));
        // 取不到字典的页（PDFium 的生成器会跳过）、没写 /Type 的页都不算。
        assert!(!may_abort(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [9 0 R 3 0 R 4 0 R] /Count 4 >>"),
            (3, "<< /Parent 2 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R >>"),
        ]));
    }

    #[test]
    fn root_without_kids_is_the_only_page() {
        let (_, pages) = pages_of(&[(1, CATALOG), (2, "<< /MediaBox [0 0 10 20] >>")]);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].as_ref().map(|p| p.number), Some(2));
    }

    #[test]
    fn cycles_terminate() {
        // 2 → 3 → 2 …：/Count 被采信为 5，实际走到深度上限为止一页也找不到。
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 5 >>"),
            (3, "<< /Type /Pages /Kids [2 0 R] /Count 5 >>"),
        ]);
        assert_eq!(pages, [None, None, None, None, None]);
        // /Count 不可信时数出来是 0 页（环被已访问集合挡住）。
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R] >>"),
            (3, "<< /Type /Pages /Kids [2 0 R] >>"),
        ]);
        assert!(pages.is_empty());
    }

    #[test]
    fn contents_forms() {
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R << /Type /Page >>] /Count 5 >>",
            ),
            (3, "<< /Type /Page /Contents 7 0 R >>"),
            (4, "<< /Type /Page /Contents [7 0 R 2 0 R 8 0 R 99 0 R] >>"),
            (5, "<< /Type /Page /Contents [] >>"),
            (6, "<< /Type /Page /Contents 9 0 R >>"),
            (7, STREAM),
            (8, STREAM),
            (9, "[7 0 R]"),
        ]);
        let contents: Vec<Contents> = pages
            .iter()
            .map(|p| p.as_ref().expect("页").contents.clone())
            .collect();
        assert_eq!(
            contents,
            [
                Contents::Single(7),
                // 不是流的项（页树节点、不存在的对象）当作空流，照样占位。
                Contents::Array(vec![Some(7), None, Some(8), None]),
                Contents::None,
                // 间接的数组。
                Contents::Array(vec![Some(7)]),
                Contents::None,
            ]
        );
        // 直接写在 /Kids 里的页字典没有对象号。
        assert_eq!(pages[4].as_ref().map(|p| p.number), Some(0));
    }

    #[test]
    fn resources_follow_the_parent_chain() {
        let (_, pages) = pages_of(&[
            (1, CATALOG),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /Resources << /A 1 >> >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R /Resources << /B 2 >> >>"),
            // 值是 null 也算取到了：不再往上找，结果是没有资源。
            (5, "<< /Type /Page /Parent 2 0 R /Resources null >>"),
        ]);
        let res = |i: usize| pages[i].as_ref().expect("页").resources.clone();
        assert!(res(0).is_some_and(|d| d.get(b"A").is_some()));
        assert!(res(1).is_some_and(|d| d.get(b"B").is_some() && d.get(b"A").is_none()));
        assert!(res(2).is_none());
    }

    #[test]
    fn geometry_follows_pdfium() {
        let (doc, pages) = pages_of(&[
            (1, CATALOG),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 /MediaBox [0 0 200 100] >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R /Rotate 90 >>"),
            // CropBox 与 MediaBox 不相交：0×0。
            (
                4,
                "<< /Type /Page /Parent 2 0 R /CropBox [500 500 600 600] >>",
            ),
            // MediaBox 是空的：按 612×792。
            (
                5,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 0 0] /Rotate -90 >>",
            ),
            // 坐标写反了也行；CropBox 取交集。
            (
                6,
                "<< /Type /Page /Parent 2 0 R /MediaBox [200 100 0 0] /CropBox [50 10 400 60] >>",
            ),
        ]);
        let geometry = |i: usize| page_geometry(&doc, &pages[i].as_ref().expect("页").dict);
        assert_eq!(
            geometry(0),
            Geometry {
                width: 100.0,
                height: 200.0,
                rotation: 1
            }
        );
        assert_eq!(
            geometry(1),
            Geometry {
                width: 0.0,
                height: 0.0,
                rotation: 0
            }
        );
        assert_eq!(
            geometry(2),
            Geometry {
                width: 792.0,
                height: 612.0,
                rotation: 3
            }
        );
        assert_eq!(
            geometry(3),
            Geometry {
                width: 150.0,
                height: 50.0,
                rotation: 0
            }
        );
    }
}
