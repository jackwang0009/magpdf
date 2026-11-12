//! 内容流：取字节、拼接、分词。
//!
//! - [`Content`]：一页的内容流按 PDFium `CPDF_ContentParser` 的方式取出来、拼成一串字节，
//!   并记住每一段来自哪条流（写回时按段拆回去）；
//! - [`parser`]：内容流的分词器，`CPDF_StreamParser` 的移植；
//! - [`inline`]：内联图像 `BI … ID … EI` 的读法，`CPDF_StreamContentParser::Handle_BeginImage` 与
//!   `CPDF_StreamParser::ReadInlineStream` 的移植。
//!
//! 移植的出处与版权声明见本 crate 的 `PDFIUM-NOTICE.md`。损坏输入不 panic：所有读取都有边界检查。

pub mod inline;
pub mod parser;

use crate::pdf::object::PdfObject;
use crate::pdf::pages::Contents;
use crate::pdf::reader::PdfDocument;

/// 拼接后的缓冲里的一段：对应 `/Contents` 的一项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// 这一段来自哪条流；数组里不是流的项是 `None`（PDFium 当作空流，照样占一段）。
    pub stream: Option<u32>,
    /// 在 [`Content::data`] 里的区间 `[start, end)`，不含段后补的空格。
    pub start: usize,
    pub end: usize,
}

/// 一页的内容流，按 PDFium 的方式拼成一串。
///
/// `/Contents` 是单条流时数据原样使用；是数组时逐项解码后拼接，**每一项后面补一个空格**
/// （只有一项的数组也补）。记号可以跨段：`q` 与 `Q`、`BT` 与 `ET`、操作数与操作符都可能分在两条流里。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Content {
    pub data: Vec<u8>,
    pub segments: Vec<Segment>,
    /// `/Contents` 是单条流（不是数组）。
    pub single: bool,
}

impl Content {
    /// 从文档里取一页的内容流。
    pub fn read(doc: &PdfDocument, contents: &Contents) -> Self {
        let stream_data = |number: u32| match doc.object(number) {
            Some(PdfObject::Stream(s)) => doc.decode_stream(&s),
            _ => Vec::new(),
        };
        match contents {
            Contents::None => Self::default(),
            Contents::Single(number) => {
                let data = stream_data(*number);
                let end = data.len();
                Self {
                    data,
                    segments: vec![Segment {
                        stream: Some(*number),
                        start: 0,
                        end,
                    }],
                    single: true,
                }
            }
            Contents::Array(items) => {
                let parts: Vec<(Option<u32>, Vec<u8>)> = items
                    .iter()
                    .map(|item| (*item, item.map(stream_data).unwrap_or_default()))
                    .collect();
                Self::from_parts(&parts)
            }
        }
    }

    /// 单条流。
    pub fn from_single(number: u32, data: Vec<u8>) -> Self {
        let end = data.len();
        Self {
            data,
            segments: vec![Segment {
                stream: Some(number),
                start: 0,
                end,
            }],
            single: true,
        }
    }

    /// 流数组：每一项后面补一个空格。
    pub fn from_parts(parts: &[(Option<u32>, Vec<u8>)]) -> Self {
        let mut data = Vec::with_capacity(parts.iter().map(|(_, d)| d.len() + 1).sum());
        let mut segments = Vec::with_capacity(parts.len());
        for (stream, part) in parts {
            let start = data.len();
            data.extend_from_slice(part);
            segments.push(Segment {
                stream: *stream,
                start,
                end: data.len(),
            });
            data.push(b' ');
        }
        Self {
            data,
            segments,
            single: false,
        }
    }

    /// 偏移所在的段：`start <= pos <= end` 的那一段（`end` 是段后补的空格的位置，归这一段）。
    /// 一段也没有时返回 `None`。
    pub fn segment_of(&self, pos: usize) -> Option<usize> {
        let after = self.segments.partition_point(|s| s.start <= pos);
        after.checked_sub(1)
    }

    /// 每一段的字节（不含补的空格）。
    pub fn segment_data(&self, index: usize) -> &[u8] {
        self.segments
            .get(index)
            .and_then(|s| self.data.get(s.start..s.end))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_items_are_followed_by_a_space() {
        let c = Content::from_parts(&[
            (Some(4), b"q".to_vec()),
            (None, Vec::new()),
            (Some(5), b"Q".to_vec()),
        ]);
        assert_eq!(c.data, b"q  Q ");
        assert!(!c.single);
        assert_eq!(
            c.segments[0],
            Segment {
                stream: Some(4),
                start: 0,
                end: 1
            }
        );
        assert_eq!(
            c.segments[1],
            Segment {
                stream: None,
                start: 2,
                end: 2
            }
        );
        assert_eq!(
            c.segments[2],
            Segment {
                stream: Some(5),
                start: 3,
                end: 4
            }
        );
        assert_eq!(c.segment_of(0), Some(0));
        assert_eq!(c.segment_of(1), Some(0), "补的空格归前一段");
        assert_eq!(c.segment_of(2), Some(1));
        assert_eq!(c.segment_of(3), Some(2));
        assert_eq!(c.segment_of(4), Some(2));
        assert_eq!(c.segment_data(2), b"Q");
    }

    #[test]
    fn single_stream_is_used_as_is() {
        let c = Content::from_single(7, b"q Q".to_vec());
        assert_eq!(c.data, b"q Q");
        assert!(c.single);
        assert_eq!(c.segment_of(3), Some(0));
        assert_eq!(Content::default().segment_of(0), None);
    }
}
