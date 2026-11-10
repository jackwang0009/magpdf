//! 增量更新写出（P0 计划 V17 第 5 条"写回方式"）。
//!
//! 只追加：原文件字节原样保留，新对象与新交叉引用追加在末尾。新 xref 一律用交叉引用表
//! （即使原文件用交叉引用流），读方按 `/Prev` 链即可找到旧表；这样写出的字节最少，
//! 也不需要为 xref 流再分配对象号。

use std::collections::BTreeMap;

use super::object::{PdfDict, PdfObject, encode_name};
use super::reader::PdfDocument;

/// 一条要写出的流。
#[derive(Debug, Clone)]
pub struct StreamPatch {
    pub dict: PdfDict,
    pub data: Vec<u8>,
}

/// 增量更新收集器。
#[derive(Debug)]
pub struct IncrementalWriter {
    base_len: usize,
    next_object: u32,
    objects: Vec<(u32, PdfObject)>,
    /// 这些对象号在旧文件里存在，新 xref 里要标成 `f`。
    freed: Vec<u32>,
    prev_xref: usize,
    size: u32,
    root: Option<PdfObject>,
    info: Option<PdfObject>,
    id: Option<PdfObject>,
}

impl IncrementalWriter {
    /// 从已打开的文档开始。`prev_xref` 是原文件 `startxref` 的值。
    pub fn new(doc: &PdfDocument, prev_xref: usize) -> Self {
        let size = doc
            .trailer_dict()
            .get(b"Size")
            .and_then(PdfObject::as_int)
            .unwrap_or(0)
            .max(0) as u32;
        Self {
            base_len: doc.bytes().len(),
            next_object: size,
            objects: Vec::new(),
            freed: Vec::new(),
            prev_xref,
            size,
            root: doc.trailer_dict().get(b"Root").cloned(),
            info: doc.trailer_dict().get(b"Info").cloned(),
            id: doc.trailer_dict().get(b"ID").cloned(),
        }
    }

    /// 把下一个对象号至少抬到 `n`（覆盖页 `/Contents` 对象时用，避免冲突）。
    pub fn set_min_next(&mut self, n: u32) {
        self.next_object = self.next_object.max(n);
    }

    /// 下一个将分配的对象号。
    pub fn next_number(&self) -> u32 {
        self.next_object
    }

    /// 写入一个新对象，返回对象号。
    pub fn add(&mut self, obj: PdfObject) -> u32 {
        let n = self.next_object;
        self.next_object += 1;
        self.size = self.size.max(n + 1);
        self.objects.push((n, obj));
        n
    }

    /// 用新内容覆盖旧对象号（增量 xref 里同一号指向新偏移，读方取最新条目）。
    pub fn replace(&mut self, n: u32, obj: PdfObject) {
        self.size = self.size.max(n + 1);
        self.next_object = self.next_object.max(n + 1);
        self.objects.push((n, obj));
    }

    /// 把某个旧对象号标为空闲。
    pub fn free(&mut self, n: u32) {
        if !self.freed.contains(&n) {
            self.freed.push(n);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.freed.is_empty()
    }

    /// 本次更新要追加的字节（不含原文件）。
    pub fn into_bytes(self) -> Vec<u8> {
        if self.objects.is_empty() && self.freed.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        out.push(b'\n');
        let mut offsets: BTreeMap<u32, usize> = BTreeMap::new();
        for (n, obj) in &self.objects {
            offsets.insert(*n, self.base_len + out.len());
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            write_object(&mut out, obj);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_pos = self.base_len + out.len();
        let size = self.size.max(self.next_object);
        out.extend_from_slice(b"xref\n");
        let mut subsection_count = 0usize;
        let mut write_sub = |out: &mut Vec<u8>, start: u32, entries: &[String]| {
            out.extend_from_slice(format!("{start} {}\n", entries.len()).as_bytes());
            for e in entries {
                out.extend_from_slice(e.as_bytes());
            }
            subsection_count += 1;
        };
        let mut free_entries = self.freed.clone();
        free_entries.sort_unstable();
        for n in free_entries {
            let e = format!("{n:010} 65535 f \n");
            write_sub(&mut out, n, std::slice::from_ref(&e));
        }
        for (n, _) in &self.objects {
            let off = offsets.get(n).copied().unwrap_or(0);
            let e = format!("{off:010} 00000 n \n");
            write_sub(&mut out, *n, std::slice::from_ref(&e));
        }
        if subsection_count == 0 {
            out.extend_from_slice(b"0 0\n");
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Prev {}", self.prev_xref).as_bytes(),
        );
        if let Some(root) = &self.root {
            out.extend_from_slice(b" /Root ");
            write_object(&mut out, root);
        }
        if let Some(info) = &self.info {
            out.extend_from_slice(b" /Info ");
            write_object(&mut out, info);
        }
        if let Some(id) = &self.id {
            out.extend_from_slice(b" /ID ");
            write_object(&mut out, id);
        }
        out.extend_from_slice(b" >>\nstartxref\n");
        out.extend_from_slice(xref_pos.to_string().as_bytes());
        out.extend_from_slice(b"\n%%EOF\n");
        out
    }
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

/// 数字的规范写法：整数不带小数点，实数最多 6 位小数并去掉尾随 0。
pub fn format_real(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_owned();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let mut s = format!("{v:.6}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" { "0".to_owned() } else { s }
}
