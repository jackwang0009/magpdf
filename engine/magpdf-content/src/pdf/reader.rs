//! 文档读取：交叉引用（表 / 流）、对象解析缓存。
//!
//! 只读。写回在 `writer` 里，只支持增量更新（追加新对象和新的 xref）。

use std::collections::BTreeMap;

use super::filter::apply_filter_chain;
use super::lexer::Lexer;
use super::lexer::find;
use super::object::{PdfDict, PdfObject};
use super::{Error, Result};

/// 交叉引用损坏。
#[derive(Debug, thiserror::Error)]
pub enum XrefError {
    #[error("没有 startxref")]
    NoStartXref,
    #[error("xref 偏移无效")]
    BadOffset,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XrefEntry {
    Free,
    /// 文件内的对象（字节偏移）。
    Offset(usize, u16),
    /// 在对象流里（流对象号，序号）。
    InStream(u32, usize),
}

/// 一个已打开的 PDF。持有整个文件的字节。
pub struct PdfDocument {
    data: Vec<u8>,
    entries: BTreeMap<u32, XrefEntry>,
    trailer: PdfDict,
    cache: std::cell::RefCell<BTreeMap<u32, PdfObject>>,
    /// 是否使用过交叉引用流（增量更新时决定新 xref 写表还是流）。
    pub uses_xref_stream: bool,
    /// 文件里原始 `/Encrypt` 的写法（原始值，可能是引用）。
    pub encrypt: Option<PdfObject>,
    /// 最近一次交叉引用节的字节偏移（增量更新时写 `/Prev`）。
    pub startxref: usize,
}

impl std::fmt::Debug for PdfDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfDocument")
            .field("bytes", &self.data.len())
            .field("objects", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl PdfDocument {
    /// 从原始字节打开。`load` 为真时读完整的 xref 链。
    pub fn load(data: Vec<u8>) -> Result<Self> {
        if find(&data[..data.len().min(1024)], b"%PDF-").is_none() {
            return Err(Error::NotPdf);
        }
        let mut doc = Self {
            data,
            entries: BTreeMap::new(),
            trailer: PdfDict::new(),
            cache: std::cell::RefCell::new(BTreeMap::new()),
            uses_xref_stream: false,
            encrypt: None,
            startxref: 0,
        };
        doc.startxref = doc.find_startxref().ok_or(XrefError::NoStartXref)?;
        doc.load_xref_chain()?;
        doc.encrypt = doc.trailer.get(b"Encrypt").cloned();
        Ok(doc)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn trailer_dict(&self) -> &PdfDict {
        &self.trailer
    }

    /// 对象总数（交叉引用里登记过的）。
    pub fn object_count(&self) -> usize {
        self.entries.len()
    }

    /// 对象在文件里的字节偏移（诊断用）。
    pub fn object_offset(&self, number: u32) -> Option<usize> {
        match self.entries.get(&number)? {
            XrefEntry::Offset(o, _) => Some(*o),
            _ => None,
        }
    }

    fn find_startxref(&self) -> Option<usize> {
        let startxref = find(&self.data, b"startxref")?;
        let mut lx = Lexer::at(&self.data, startxref + b"startxref".len());
        lx.skip_ws();
        let word = lx.read_word();
        std::str::from_utf8(word).ok()?.trim().parse().ok()
    }

    fn load_xref_chain(&mut self) -> std::result::Result<(), XrefError> {
        let mut offset = Some(self.startxref);
        let mut seen = std::collections::BTreeSet::new();
        let mut first = true;
        while let Some(off) = offset {
            if !seen.insert(off) || off >= self.data.len() {
                break;
            }
            let (entries, trailer) = self.read_xref_section(off)?;
            for (num, entry) in entries {
                self.entries.entry(num).or_insert(entry);
            }
            if first {
                self.trailer = trailer.clone();
                first = false;
            }
            offset = self.prev_xref(&trailer);
        }
        Ok(())
    }

    fn prev_xref(&self, trailer: &PdfDict) -> Option<usize> {
        let prev = trailer.get(b"Prev")?;
        if let Some(v) = prev.as_int() {
            return usize::try_from(v).ok();
        }
        self.resolve(Some(prev))
            .and_then(|o| o.as_int())
            .and_then(|v| usize::try_from(v).ok())
    }

    fn read_xref_section(
        &self,
        offset: usize,
    ) -> std::result::Result<(BTreeMap<u32, XrefEntry>, PdfDict), XrefError> {
        let mut lx = Lexer::at(&self.data, offset);
        lx.skip_ws();
        if self.data.get(lx.pos..lx.pos + 4) == Some(b"xref") {
            return self.read_xref_table(lx);
        }
        // 可能是 `N G obj` 形式的交叉引用流。
        self.read_xref_stream(offset)
    }

    fn read_xref_table(
        &self,
        mut lx: Lexer<'_>,
    ) -> std::result::Result<(BTreeMap<u32, XrefEntry>, PdfDict), XrefError> {
        lx.pos += 4;
        let mut entries = BTreeMap::new();
        loop {
            lx.skip_ws();
            if self.data.get(lx.pos..lx.pos + 7) == Some(b"trailer") {
                lx.pos += 7;
                break;
            }
            let start = lx.read_word();
            let Some(start) = parse_usize(start) else {
                return Err(XrefError::Other("xref 子段起始无效".to_owned()));
            };
            lx.skip_ws();
            let count = parse_usize(lx.read_word())
                .ok_or_else(|| XrefError::Other("xref 子段数量无效".to_owned()))?;
            for i in 0..count {
                lx.skip_ws();
                let off = parse_usize(lx.read_word());
                lx.skip_ws();
                let generation = parse_usize(lx.read_word());
                lx.skip_ws();
                let kind = lx.read_word();
                let (Some(off), Some(generation)) = (off, generation) else {
                    return Err(XrefError::Other("xref 条目无效".to_owned()));
                };
                let num = (start + i) as u32;
                let entry = if kind == b"n" && off != 0 {
                    XrefEntry::Offset(off, generation as u16)
                } else {
                    XrefEntry::Free
                };
                entries.insert(num, entry);
            }
        }
        let trailer = lx
            .read_object()
            .map_err(|e| XrefError::Other(e.to_string()))?;
        let trailer = match trailer {
            PdfObject::Dict(d) => d,
            _ => return Err(XrefError::Other("trailer 不是字典".to_owned())),
        };
        Ok((entries, trailer))
    }

    fn read_xref_stream(
        &self,
        offset: usize,
    ) -> std::result::Result<(BTreeMap<u32, XrefEntry>, PdfDict), XrefError> {
        let obj = self
            .parse_object_at(offset)
            .map_err(|e| XrefError::Other(e.to_string()))?;
        let PdfObject::Stream(stream) = obj else {
            return Err(XrefError::BadOffset);
        };
        let data = apply_filter_chain(
            &stream.data,
            self.resolve(stream.dict.get(b"Filter")).as_ref(),
            self.resolve(stream.dict.get(b"DecodeParms")).as_ref(),
        )
        .map_err(|e| XrefError::Other(e.to_string()))?;
        let size = stream
            .dict
            .get(b"Size")
            .and_then(PdfObject::as_int)
            .unwrap_or(0);
        let Some(w) = stream.dict.get(b"W").and_then(PdfObject::as_array) else {
            return Err(XrefError::Other("xref 流缺少 /W".to_owned()));
        };
        let widths: Vec<usize> = w
            .iter()
            .filter_map(PdfObject::as_int)
            .map(|v| v.max(0) as usize)
            .collect();
        if widths.len() < 3 {
            return Err(XrefError::Other("xref 流的 /W 不完整".to_owned()));
        }
        let index: Vec<i64> = match stream.dict.get(b"Index").and_then(PdfObject::as_array) {
            Some(a) => a.iter().filter_map(PdfObject::as_int).collect(),
            None => vec![0, size],
        };
        let row = widths.iter().sum::<usize>();
        let mut entries = BTreeMap::new();
        let mut pos = 0usize;
        for pair in index.chunks(2) {
            let (Some(&start), Some(&count)) = (pair.first(), pair.get(1)) else {
                break;
            };
            for i in 0..count.max(0) {
                let num = (start + i) as u32;
                if row == 0 || pos + row > data.len() {
                    break;
                }
                let mut fields = [0u64; 3];
                let mut p = pos;
                for (k, &width) in widths.iter().take(3).enumerate() {
                    let mut v = 0u64;
                    for _ in 0..width {
                        v = (v << 8) | u64::from(data[p]);
                        p += 1;
                    }
                    fields[k] = v;
                }
                pos += row;
                let kind = fields[0];
                let entry = match kind {
                    0 => XrefEntry::Free,
                    1 => XrefEntry::Offset(fields[1] as usize, fields[2] as u16),
                    2 => XrefEntry::InStream(fields[1] as u32, fields[2] as usize),
                    _ => continue,
                };
                entries.insert(num, entry);
            }
        }
        Ok((entries, stream.dict.clone()))
    }

    /// 在给定偏移解析一个 `N G obj ... endobj`。
    fn parse_object_at(&self, offset: usize) -> Result<PdfObject> {
        if offset >= self.data.len() {
            return Err(Error::Syntax("对象偏移越界".to_owned()));
        }
        let mut lx = Lexer::at(&self.data, offset);
        lx.skip_ws();
        while self.data.get(lx.pos).is_some_and(|b| b.is_ascii_digit()) {
            lx.pos += 1;
        }
        lx.skip_ws();
        while self.data.get(lx.pos).is_some_and(|b| b.is_ascii_digit()) {
            lx.pos += 1;
        }
        lx.skip_ws();
        if lx.read_word() != b"obj" {
            return Err(Error::Syntax("缺少 obj 关键字".to_owned()));
        }
        lx.skip_ws();
        let obj = lx.read_object()?;
        match obj {
            PdfObject::Dict(d) => {
                let save = lx.pos;
                lx.skip_ws();
                if lx.read_word() == b"stream" {
                    lx.pos = save;
                    lx.read_stream_data(d)
                } else {
                    Ok(PdfObject::Dict(d))
                }
            }
            other => Ok(other),
        }
    }

    /// 解析对象号对应的对象（带缓存）。
    pub fn object(&self, number: u32) -> Option<PdfObject> {
        if let Some(cached) = self.cache.borrow().get(&number) {
            return Some(cached.clone());
        }
        let entry = *self.entries.get(&number)?;
        let obj = match entry {
            XrefEntry::Free => return None,
            XrefEntry::Offset(off, _) => match self.parse_object_at(off) {
                Ok(o) => o,
                Err(e) => {
                    tracing::debug!(object = number, error = %e, "对象解析失败");
                    return None;
                }
            },
            XrefEntry::InStream(stream_num, idx) => self.object_from_stream(stream_num, idx)?,
        };
        self.cache.borrow_mut().insert(number, obj.clone());
        Some(obj)
    }

    /// 对象号是否在交叉引用里（且不是空闲）。
    pub fn has_object(&self, number: u32) -> bool {
        self.entries
            .get(&number)
            .is_some_and(|e| *e != XrefEntry::Free)
    }

    /// 对象号的代号。
    pub fn generation(&self, number: u32) -> u16 {
        match self.entries.get(&number) {
            Some(XrefEntry::Offset(_, g)) => *g,
            _ => 0,
        }
    }

    fn object_from_stream(&self, stream_num: u32, idx: usize) -> Option<PdfObject> {
        let stream = self.object(stream_num)?;
        let PdfObject::Stream(stream) = stream else {
            return None;
        };
        let data = apply_filter_chain(
            &stream.data,
            self.resolve(stream.dict.get(b"Filter")).as_ref(),
            self.resolve(stream.dict.get(b"DecodeParms")).as_ref(),
        )
        .ok()?;
        let n = stream
            .dict
            .get(b"N")
            .and_then(PdfObject::as_int)
            .unwrap_or(0);
        let first = stream
            .dict
            .get(b"First")
            .and_then(PdfObject::as_int)
            .unwrap_or(0);
        let mut lx = Lexer::at(&data, 0);
        for _ in 0..n.max(0) {
            lx.skip_ws();
            let num = lx.read_word();
            lx.skip_ws();
            let _gen = lx.read_word();
            let obj = lx.read_object().ok()?;
            if parse_usize(num) == Some(idx) {
                return Some(obj);
            }
        }
        let _ = first;
        None
    }

    /// 解析一层间接引用。非引用原样返回。
    pub fn resolve<'a>(&'a self, obj: Option<&'a PdfObject>) -> Option<PdfObject> {
        let obj = obj?;
        match obj {
            PdfObject::Ref(n, _) => self.object(*n),
            other => Some(other.clone()),
        }
    }

    /// 同 [`Self::resolve`]，但解析不到时返回 `None`（与"值是 null"区分不开，调用方注意）。
    pub fn resolve_or_none(&self, obj: Option<&PdfObject>) -> Option<PdfObject> {
        self.resolve(obj).filter(|o| !o.is_null())
    }

    /// 解析一层引用，值为 null 或解析不到都返回 `None`；参数不是 `Option` 时的便捷版本。
    pub fn deref(&self, obj: &PdfObject) -> Option<PdfObject> {
        self.resolve_or_none(Some(obj))
    }

    /// 页树的遍历（`pages::walk_pages` 需要）。
    pub fn pages(&self, max_pages: usize) -> Vec<super::pages::PageInfo> {
        super::pages::walk_pages(self, max_pages)
    }
}

fn parse_usize(word: &[u8]) -> Option<usize> {
    std::str::from_utf8(word).ok()?.trim().parse().ok()
}
