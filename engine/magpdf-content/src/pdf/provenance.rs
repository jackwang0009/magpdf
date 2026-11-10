//! 原图对象号记录：增量更新里被重写过的对象，在页的 `/Contents` 里仍写着旧对象号，
//! 原始对象也不在更新之后的对象层里。重写时在内容流容器字典上加 `/PrevNums`
//! （MagPDF 私有键，读方忽略）记下旧号，读回时按它找回源文件里的那条流。
//!
//! 取舍见报告第 2 节：这保证"保存后再打开、再保存仍逐字节相同"，避免每次保存都把整条流再写一遍。

use std::collections::BTreeMap;

use super::object::{PdfDict, PdfObject};
use super::reader::PdfDocument;

pub const PREV_NUMS_KEY: &[u8] = b"PrevNums";

/// 一条流的来源：文件里的对象号 + 代号 + 可选的链路键（到更早版本的对象号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSource {
    pub number: u32,
    pub generation: u16,
    pub prev_number: Option<u32>,
}

/// 从容器字典（`/Contents` 指向的对象，或页面字典本身）读出旧对象号表。键缺失时返回空表。
pub fn read_prev_nums(doc: &PdfDocument, dict: &PdfDict) -> BTreeMap<u32, StreamSource> {
    let Some(obj) = dict.get(PREV_NUMS_KEY) else {
        return BTreeMap::new();
    };
    let Some(items) = doc
        .deref(obj)
        .and_then(|o| o.as_array().map(<[PdfObject]>::to_vec))
    else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    let mut it = items.iter();
    while let Some(new) = it.next() {
        let Some(new) = new.as_int() else { continue };
        let Some(old) = it.next().and_then(PdfObject::as_int) else {
            break;
        };
        let Some(generation) = it.next().and_then(PdfObject::as_int) else {
            break;
        };
        let Some(prev) = it.next() else { break };
        let prev = match prev {
            PdfObject::Null => None,
            other => other.as_int().map(|v| v as u32),
        };
        out.insert(
            new as u32,
            StreamSource {
                number: old as u32,
                generation: generation as u16,
                prev_number: prev,
            },
        );
    }
    out
}

/// 生成 `/PrevNums` 数组对象的写法（键保留原对象号在数组里）。
pub fn prev_nums_object(pairs: &[StreamSource]) -> PdfObject {
    let mut items = Vec::with_capacity(pairs.len() * 4);
    for p in pairs {
        items.push(PdfObject::Int(i64::from(p.number)));
        items.push(PdfObject::Int(i64::from(p.number)));
        items.push(PdfObject::Int(i64::from(p.generation)));
        items.push(
            p.prev_number
                .map_or(PdfObject::Null, |n| PdfObject::Int(i64::from(n))),
        );
    }
    PdfObject::Array(items)
}
