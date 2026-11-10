//! 写回：增量更新与完整重写的体积对比，以及"保存后再打开"的对象层。
//!
//! 取舍见报告第 2 节。要点：
//! - 增量更新默认**原地覆盖**内容流对象号（新 xref 条目覆盖旧偏移），页面字典与页树不用改。
//! - 共用内容流（多页 `/Contents` 指向同一流）时不能就地覆盖：新建流对象，并**原地覆盖**
//!   该页的页面字典（`/Contents` 换成新号）；Kids 仍指向原页面号，Catalog 不用动。
//! - [`IncrementalWriter::into_bytes`] 只返回追加段；本模块拼回 `原文件 || 追加`。

use std::collections::{BTreeMap, BTreeSet};

use crate::pdf::object::{PdfDict, PdfObject, PdfStreamRef};
use crate::pdf::reader::PdfDocument;
use crate::pdf::writer::IncrementalWriter;

/// 保存方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// 原文件字节 + 追加（默认）。
    Incremental,
    /// 只写当前对象层（`/Prev` 链消失，旧字节丢弃）。
    FullRewrite,
}

/// 一个要替换的内容流。
#[derive(Debug, Clone)]
pub struct StreamReplacement {
    /// 原来的流对象号。
    pub object_number: u32,
    /// 新的解码后字节。`None` 表示删除（`/Contents` 变空数组）。
    pub data: Option<Vec<u8>>,
    /// 新流是否用 Flate 压缩（与原文一致时压缩，体积更小；测试可关）。
    pub compress: bool,
}

/// 保存后的文档。
#[derive(Debug, Clone)]
pub struct SavedDocument {
    pub bytes: Vec<u8>,
    /// 新流对象号（与替换一一对应）。原地覆盖时新旧相同。
    pub new_streams: Vec<(u32, u32)>,
    /// 是否改动了页面字典/页树。
    pub pages_touched: usize,
}

/// 增量更新。
///
/// `page_streams`：页对象号 → 该页的内容流对象号列表（`/Contents` 单流或数组）。
/// `replacements`：要替换的流。
/// `deshare`：被多页共享的流对象号；这些流一律新建对象，并覆盖对应页面字典。
/// `compress`：为 true 时统一 Flate；各 `StreamReplacement.compress` 也可单独关闭（取 AND）。
pub fn save_incremental(
    doc: &PdfDocument,
    page_streams: &BTreeMap<u32, Vec<u32>>,
    replacements: &[StreamReplacement],
    deshare: &BTreeSet<u32>,
    compress: bool,
) -> SavedDocument {
    let mut w = IncrementalWriter::new(doc, doc.startxref);
    w.set_min_next(doc.object_count() as u32 + 1);
    let mut new_streams = Vec::new();
    let mut pages_touched = 0usize;

    // old stream number → effective stream number after this update
    let mut stream_map: BTreeMap<u32, u32> = BTreeMap::new();

    for r in replacements {
        let Some(data) = &r.data else {
            continue;
        };
        let old = doc.object(r.object_number);
        let old_dict = match &old {
            Some(PdfObject::Dict(d)) => Some(d.clone()),
            Some(PdfObject::Stream(s)) => Some(s.dict.clone()),
            _ => None,
        };
        let mut dict = old_dict.unwrap_or_else(PdfDict::new);
        dict.remove(b"Filter");
        dict.remove(b"DecodeParms");
        let do_compress = compress && r.compress;
        let data = if do_compress {
            let packed = crate::pdf::encode_flate(data);
            dict.set(b"Filter", PdfObject::Name(b"FlateDecode".to_vec()));
            packed
        } else {
            data.clone()
        };
        let stream = PdfObject::Stream(PdfStreamRef {
            dict,
            data,
            span: (0, 0),
        });

        if deshare.contains(&r.object_number) {
            let num = w.add(stream);
            new_streams.push((r.object_number, num));
            stream_map.insert(r.object_number, num);
        } else {
            w.replace(r.object_number, stream);
            new_streams.push((r.object_number, r.object_number));
            stream_map.insert(r.object_number, r.object_number);
        }
    }

    // 去共享：页面字典原地覆盖，/Contents 指向新流号。
    if !deshare.is_empty() {
        for (page, streams) in page_streams {
            let needs = streams
                .iter()
                .any(|s| deshare.contains(s) && stream_map.contains_key(s));
            if !needs {
                continue;
            }
            let Some(PdfObject::Dict(mut d)) = doc.object(*page) else {
                continue;
            };
            let mapped: Vec<u32> = streams
                .iter()
                .map(|s| stream_map.get(s).copied().unwrap_or(*s))
                .collect();
            let contents = if mapped.len() == 1 {
                PdfObject::Ref(mapped[0], 0)
            } else {
                PdfObject::Array(mapped.iter().map(|n| PdfObject::Ref(*n, 0)).collect())
            };
            d.set(b"Contents", contents);
            w.replace(*page, PdfObject::Dict(d));
            pages_touched += 1;
        }
    }

    let append = w.into_bytes();
    let mut bytes = doc.bytes().to_vec();
    if !append.is_empty() && append != b"\n" {
        // into_bytes always starts with '\n'; empty writer still emits a lone newline+empty xref.
        // Skip appending if no objects were written.
        if !new_streams.is_empty() || pages_touched > 0 {
            bytes.extend_from_slice(&append);
        }
    }

    SavedDocument {
        bytes,
        new_streams,
        pages_touched,
    }
}

/// 完整重写：遍历全部对象写到新文件（只做实验 4 的体积对比，不追求最优压缩）。
pub fn save_full_rewrite(doc: &PdfDocument, compress: bool) -> SavedDocument {
    use crate::pdf::writer::write_object;
    let count = doc.object_count() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets = Vec::new();
    for n in 1..=count {
        let Some(obj) = doc.object(n) else { continue };
        let obj = match obj {
            PdfObject::Stream(s) if compress => {
                let mut dict = s.dict.clone();
                dict.remove(b"Filter");
                dict.remove(b"DecodeParms");
                let data = crate::pdf::encode_flate(&s.data);
                dict.set(b"Filter", PdfObject::Name(b"FlateDecode".to_vec()));
                PdfObject::Stream(PdfStreamRef {
                    dict,
                    data,
                    span: (0, 0),
                })
            }
            other => other,
        };
        offsets.push((n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        write_object(&mut out, &obj);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = count + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    let map: BTreeMap<u32, usize> = offsets.iter().copied().collect();
    for n in 1..=count {
        match map.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(b"trailer\n<< /Size ");
    out.extend_from_slice(size.to_string().as_bytes());
    out.extend_from_slice(b" /Root ");
    write_object(
        &mut out,
        doc.trailer_dict().get(b"Root").unwrap_or(&PdfObject::Null),
    );
    for key in [&b"Info"[..], b"ID"] {
        if let Some(v) = doc.trailer_dict().get(key) {
            out.push(b' ');
            out.push(b'/');
            out.extend_from_slice(key);
            out.push(b' ');
            write_object(&mut out, v);
        }
    }
    out.extend_from_slice(b" >>\nstartxref\n");
    out.extend_from_slice(xref.to_string().as_bytes());
    out.extend_from_slice(b"\n%%EOF\n");
    SavedDocument {
        bytes: out,
        new_streams: Vec::new(),
        pages_touched: 0,
    }
}
