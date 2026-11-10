//! 页面树的遍历：找出所有页面的对象号、`/Contents`、资源（含继承）。
//!
//! PDFium 的 `CPDF_Page` 用自己的一套页树遍历；这里只按 PDF 规范的继承规则（7.7.3.4）
//! 收集，供内容流层定位页面。

use super::object::{PdfDict, PdfObject};
use super::reader::PdfDocument;

/// 一页的结构信息。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageInfo {
    /// 页面对象号。
    pub number: u32,
    /// `/Contents`：单个流或流数组（已解析成对象号列表）。空页为 `None`。
    pub contents: Option<Contents>,
    /// 继承得到的 `/Resources`。
    pub resources: Option<PdfObject>,
    /// 页树里的顺序（0 起）。
    pub index: usize,
}

/// `/Contents` 的两种形态。
#[derive(Debug, Clone, PartialEq)]
pub enum Contents {
    /// 单个流对象号。
    One(u32),
    /// 流数组。数组项可能不是流（PDFium 会跳过），这里保留对象号并在取流时检查类型。
    Many(Vec<u32>),
}

impl Contents {
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Many(v) if v.is_empty())
    }
}

/// 遍历页树。节点必须带对象号（页面树里的节点按规范都是间接对象）；`max_pages` 防止环导致死循环。
pub fn walk_pages(doc: &PdfDocument, max_pages: usize) -> Vec<PageInfo> {
    let mut out = Vec::new();
    let Some(root) = doc.resolve_or_none(doc.trailer_dict().get(b"Root")) else {
        return out;
    };
    let Some(root_dict) = root.as_dict() else {
        return out;
    };
    let Some(pages) = root_dict.get(b"Pages") else {
        return out;
    };
    let mut visited = std::collections::BTreeSet::new();
    walk_node(
        doc,
        pages,
        &Inherited::default(),
        &mut visited,
        &mut out,
        max_pages,
    );
    for (i, page) in out.iter_mut().enumerate() {
        page.index = i;
    }
    out
}

#[derive(Debug, Clone, Default)]
struct Inherited {
    resources: Option<PdfObject>,
}

fn walk_node(
    doc: &PdfDocument,
    node: &PdfObject,
    inherited: &Inherited,
    visited: &mut std::collections::BTreeSet<u32>,
    out: &mut Vec<PageInfo>,
    max_pages: usize,
) {
    if out.len() >= max_pages {
        return;
    }
    let Some(number) = node.as_ref().map(|(n, _)| n) else {
        return;
    };
    if !visited.insert(number) {
        return;
    }
    let Some(dict) = doc
        .resolve_or_none(Some(node))
        .and_then(|o| o.as_dict().cloned())
    else {
        return;
    };
    let mut inherited = inherited.clone();
    if let Some(res) = doc.resolve_or_none(dict.get(b"Resources")) {
        inherited.resources = Some(res);
    }
    let kind = doc.resolve(dict.get(b"Type"));
    let is_page = matches!(kind.as_ref().and_then(PdfObject::as_name), Some(b"Page"));
    if is_page || dict.get(b"Kids").is_none() {
        push_page(doc, number, &dict, &inherited, out);
        return;
    }
    let Some(kids) = dict.get(b"Kids").and_then(|k| doc.resolve_or_none(Some(k))) else {
        return;
    };
    let Some(items) = kids.as_array() else {
        return;
    };
    for kid in items {
        walk_node(doc, kid, &inherited, visited, out, max_pages);
        if out.len() >= max_pages {
            return;
        }
    }
}

fn push_page(
    doc: &PdfDocument,
    number: u32,
    dict: &PdfDict,
    inherited: &Inherited,
    out: &mut Vec<PageInfo>,
) {
    let contents = match dict.get(b"Contents") {
        None | Some(PdfObject::Null) => None,
        Some(c @ PdfObject::Ref(..)) => match doc.deref(c) {
            Some(PdfObject::Stream(_)) | Some(PdfObject::Ref(..)) => {
                c.as_ref().map(|(n, _)| Contents::One(n))
            }
            Some(PdfObject::Array(items)) => Some(Contents::Many(
                items
                    .iter()
                    .filter_map(PdfObject::as_ref)
                    .map(|(n, _)| n)
                    .collect(),
            )),
            _ => None,
        },
        Some(PdfObject::Stream(_)) => dict
            .get(b"Contents")
            .and_then(PdfObject::as_ref)
            .map(|(n, _)| Contents::One(n)),
        _ => None,
    };
    out.push(PageInfo {
        number,
        contents,
        resources: inherited.resources.clone(),
        index: out.len(),
    });
}
