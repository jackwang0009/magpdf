//! 分词器主体。第一段：词法与数组/字典/字符串。
//!
//! 字符分类、数组不动点（在 `[` 内遇到 `]` 前反复结束）、字符串转义都按 PDFium
//! `cpdf_streamparser.cpp` 的行为；注意 PDFium 的判断顺序是"先看分隔符、再看数字"，
//! 所以 `<`/`>` 永远不会被当成布尔以外的对象，`23 0 obj` 里的 `obj` 也是操作符。
use crate::content::{Content, Note, NoteKind, Token, TokenKind, TokenRange, Tokenized};

// 第二段（整个实现）见文件末尾。

use crate::pdf::lexer::parse_number;
use crate::pdf::object::{PdfDict, PdfObject, decode_name, hex_value, is_delimiter, is_whitespace};

/// 内部状态。
struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
    stream_starts: &'a [usize],
    notes: Vec<Note>,
}

impl<'a> Lexer<'a> {
    fn segment_of(&self, pos: usize) -> usize {
        self.stream_starts
            .partition_point(|&s| s <= pos)
            .saturating_sub(1)
    }

    fn byte(&self, i: usize) -> Option<u8> {
        self.data.get(i).copied()
    }

    /// 跳过空白与注释，返回 (起点, 结束位置)。结束位置是第一个非空白、非注释字节。
    fn skip_ws(&mut self) -> (usize, usize) {
        let start = self.pos;
        while let Some(b) = self.byte(self.pos) {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.byte(self.pos) {
                    self.pos += 1;
                    if c == b'\r' || c == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
        (start, self.pos)
    }

    fn read_word(&mut self) -> (usize, usize) {
        let start = self.pos;
        while let Some(b) = self.byte(self.pos) {
            if is_whitespace(b) || is_delimiter(b) {
                break;
            }
            self.pos += 1;
        }
        (start, self.pos)
    }

    /// 读一个记号的主体（调用时 `pos` 位于非空白处）。
    fn read_token(&mut self) -> TokenKind {
        let Some(b) = self.byte(self.pos) else {
            return TokenKind::Null;
        };
        match b {
            b'(' => {
                self.pos += 1;
                self.read_literal_string();
                TokenKind::LiteralString
            }
            b'<' => {
                if self.byte(self.pos + 1) == Some(b'<') {
                    self.pos += 2;
                    let _ = self.read_dict(0);
                    TokenKind::Dict
                } else {
                    self.pos += 1;
                    self.read_hex_string();
                    TokenKind::HexString
                }
            }
            b'[' => {
                self.pos += 1;
                let _ = self.read_array(0);
                TokenKind::Array
            }
            b'/' => {
                self.pos += 1;
                let (s, e) = self.read_word();
                let name = decode_name(&self.data[s..e]);
                if name.is_empty() {
                    self.notes.push(Note {
                        offset: s,
                        note: NoteKind::EmptyName,
                    });
                }
                TokenKind::Name(name)
            }
            b']' | b'>' | b')' | b'{' | b'}' => {
                self.pos += 1;
                TokenKind::StrayDelimiter(b)
            }
            _ => {
                let (s, e) = self.read_word();
                if s == e {
                    // 不认识的单字节（例如孤立的 `}` 已在上面处理）。
                    self.pos += 1;
                    return TokenKind::StrayDelimiter(b);
                }
                let word = &self.data[s..e];
                match word {
                    b"true" => TokenKind::Bool(true),
                    b"false" => TokenKind::Bool(false),
                    b"null" => TokenKind::Null,
                    _ => match parse_number(word) {
                        Some(PdfObject::Int(v)) => TokenKind::Number(v as f64),
                        Some(PdfObject::Real(v)) => TokenKind::Number(v),
                        _ => TokenKind::Operator,
                    },
                }
            }
        }
    }

    /// 字面量字符串，处理转义，返回解码结果。
    fn read_literal_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.literal_body(&mut out);
        out
    }

    /// 字面量字符串主体（`pos` 位于开头的 `(` 之后）。结束后 `pos` 指向结尾的 `)` 之**后**。
    fn literal_body(&mut self, out: &mut Vec<u8>) {
        let mut depth = 0usize;
        while let Some(b) = self.byte(self.pos) {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    out.push(b);
                }
                b'\\' => self.read_escape(out),
                _ => out.push(b),
            }
        }
        self.notes.push(Note {
            offset: self.pos,
            note: NoteKind::Unterminated,
        });
    }

    fn read_escape(&mut self, out: &mut Vec<u8>) {
        let Some(b) = self.byte(self.pos) else {
            return;
        };
        self.pos += 1;
        match b {
            b'0'..=b'7' => {
                let mut v = u16::from(b - b'0');
                for _ in 0..2 {
                    match self.byte(self.pos) {
                        Some(c @ b'0'..=b'7') => {
                            v = v * 8 + u16::from(c - b'0');
                            self.pos += 1;
                        }
                        _ => break,
                    }
                }
                out.push(v as u8);
            }
            b'\r' => {
                if self.byte(self.pos) == Some(b'\n') {
                    self.pos += 1;
                }
            }
            b'\n' => {}
            other => out.push(other),
        }
    }

    fn read_hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.byte(self.pos) {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(v) = hex_value(b) {
                match hi {
                    None => hi = Some(v),
                    Some(h) => {
                        out.push(h * 16 + v);
                        hi = None;
                    }
                }
            }
        }
        if let Some(h) = hi {
            out.push(h * 16);
        }
        out
    }

    /// 递归读一个对象（数组、字典），与 PDFium 的 `ReadNextObject` 对齐。
    fn read_object(&mut self, depth: usize) -> Option<PdfObject> {
        if depth > 64 {
            return None;
        }
        let _ = self.skip_ws();
        let b = self.byte(self.pos)?;
        match b {
            b'<' => {
                if self.byte(self.pos + 1) == Some(b'<') {
                    self.pos += 2;
                    return self.read_dict(depth);
                }
                self.pos += 1;
                Some(PdfObject::Str(self.read_hex_string()))
            }
            b'[' => {
                self.pos += 1;
                self.read_array(depth)
            }
            b'(' => {
                self.pos += 1;
                Some(PdfObject::Str(self.read_literal_string()))
            }
            b'/' => {
                self.pos += 1;
                let (s, e) = self.read_word();
                Some(PdfObject::Name(decode_name(&self.data[s..e])))
            }
            b']' | b'>' | b')' | b'}' | b'{' => {
                // PDFium 从分隔符位置读下一词（`]` 单独成一个词），这里同样消费一个字节。
                self.pos += 1;
                None
            }
            _ => {
                let (s, e) = self.read_word();
                if s == e {
                    self.pos += 1;
                    return None;
                }
                let word = &self.data[s..e];
                match word {
                    b"true" => Some(PdfObject::Bool(true)),
                    b"false" => Some(PdfObject::Bool(false)),
                    b"null" => Some(PdfObject::Null),
                    _ => parse_number(word),
                }
            }
        }
    }

    fn read_array(&mut self, depth: usize) -> Option<PdfObject> {
        if self.byte(self.pos) == Some(b']') {
            self.pos += 1;
            return Some(PdfObject::Array(Vec::new()));
        }
        let mut items = Vec::new();
        loop {
            let _ = self.skip_ws();
            let token_pos = self.pos;
            let peek = self.byte(self.pos);
            if let Some(o) = self.read_object(depth + 1) {
                items.push(o);
                continue;
            }
            if self.pos <= token_pos {
                self.pos += 1;
            }
            match peek {
                None => break,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
        }
        Some(PdfObject::Array(self.fix_array_refs(items)))
    }

    /// 数组里的 `n g R` 合成引用。
    fn fix_array_refs(&self, items: Vec<PdfObject>) -> Vec<PdfObject> {
        if items.len() < 3 {
            return items;
        }
        let mut out = Vec::with_capacity(items.len());
        let mut i = 0;
        while i < items.len() {
            if i + 2 < items.len()
                && let (PdfObject::Int(n), PdfObject::Int(g), PdfObject::Name(r)) =
                    (&items[i], &items[i + 1], &items[i + 2])
                && r == b"R"
            {
                out.push(PdfObject::Ref(*n as u32, *g as u16));
                i += 3;
                continue;
            }
            out.push(items[i].clone());
            i += 1;
        }
        out
    }

    fn read_dict(&mut self, depth: usize) -> Option<PdfObject> {
        if self.byte(self.pos + 1) == Some(b'>') {
            self.pos += 2;
            return Some(PdfObject::Dict(PdfDict::default()));
        }
        let mut entries: Vec<(Vec<u8>, PdfObject)> = Vec::new();
        loop {
            let _ = self.skip_ws();
            match self.byte(self.pos) {
                None => break,
                Some(b'>') => {
                    if self.byte(self.pos + 1) == Some(b'>') {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                Some(b'/') => {
                    self.pos += 1;
                    let (s, e) = self.read_word();
                    let key = decode_name(&self.data[s..e]);
                    let value = self.read_object(depth + 1);
                    match value {
                        Some(v) => entries.push((key, v)),
                        None => {
                            entries.push((key, PdfObject::Null));
                            // PDFium 读失败后从键重新开始；这里保留位置（分隔符已被 read_object 消费）。
                        }
                    }
                }
                Some(_) => {
                    let (s, e) = self.read_word();
                    if s == e {
                        self.pos += 1;
                    }
                }
            }
        }
        Some(PdfObject::Dict(PdfDict::from_entries(entries)))
    }
}

/// 内联图像缩写 → 全文名（PDFium `kInlineKeyAbbr`、`kInlineValueAbbr`）。
const INLINE_KEY_ABBR: &[(&[u8], &[u8])] = &[
    (b"BPC", b"BitsPerComponent"),
    (b"CS", b"ColorSpace"),
    (b"D", b"Decode"),
    (b"DP", b"DecodeParms"),
    (b"F", b"Filter"),
    (b"H", b"Height"),
    (b"IM", b"ImageMask"),
    (b"I", b"Interpolate"),
    (b"W", b"Width"),
];

const INLINE_VALUE_ABBR: &[(&[u8], &[u8])] = &[
    (b"G", b"DeviceGray"),
    (b"RGB", b"DeviceRGB"),
    (b"CMYK", b"DeviceCMYK"),
    (b"I", b"Indexed"),
    (b"AHx", b"ASCIIHexDecode"),
    (b"A85", b"ASCII85Decode"),
    (b"LZW", b"LZWDecode"),
    (b"Fl", b"FlateDecode"),
    (b"RL", b"RunLengthDecode"),
    (b"CCF", b"CCITTFaxDecode"),
    (b"DCT", b"DCTDecode"),
];

fn abbr_lookup(table: &[(&[u8], &[u8])], name: &[u8]) -> Option<Vec<u8>> {
    table
        .iter()
        .find(|(a, _)| *a == name)
        .map(|(_, full)| full.to_vec())
}

/// 缩写替换（只换字典这一层的键与名字值，PDFium 会递归进数组）。
fn replace_abbr(dict: &mut PdfDict) {
    let mut ops: Vec<(Vec<u8>, Vec<u8>, bool)> = Vec::new();
    for (key, value) in dict.iter() {
        let mut effective_key = key.to_vec();
        if let Some(full) = abbr_lookup(INLINE_KEY_ABBR, key) {
            ops.push((key.to_vec(), full.clone(), true));
            effective_key = full;
        }
        if let Some(PdfObject::Name(n)) = Some(value)
            && let Some(full) = abbr_lookup(INLINE_VALUE_ABBR, n)
        {
            ops.push((effective_key, full, false));
        }
    }
    for (key, value, is_key) in ops {
        if is_key {
            if let Some(PdfObject::Name(n)) = dict.get(&key).cloned() {
                dict.remove(&key);
                dict.set(&value, PdfObject::Name(n));
            }
        } else if let Some(old) = dict.get(&key).cloned() {
            let new = match old {
                PdfObject::Name(n) => {
                    PdfObject::Name(abbr_lookup(INLINE_VALUE_ABBR, &n).unwrap_or(n))
                }
                other => other,
            };
            dict.set(&key, new);
        }
    }
}

impl<'a> Lexer<'a> {
    /// `BI … ID`：读图像字典，`pos` 停在 `ID` 之后。读不到 `ID` 时返回 `None`（退回按普通记号解析）。
    fn read_inline_dict(&mut self) -> Option<PdfDict> {
        let mut entries: Vec<(Vec<u8>, PdfObject)> = Vec::new();
        loop {
            let _ = self.skip_ws();
            let b = self.byte(self.pos)?;
            if b == b'/' {
                self.pos += 1;
                let (s, e) = self.read_word();
                let key = decode_name(&self.data[s..e]);
                let value = self.read_object(1)?;
                entries.push((key, value));
                continue;
            }
            let (s, e) = self.read_word();
            if s < e && &self.data[s..e] == b"ID" {
                let mut dict = PdfDict::from_entries(entries);
                replace_abbr(&mut dict);
                // 解码器统一按全名取字段，缩写先展开。
                for (from, to) in [
                    (&b"W"[..], &b"Width"[..]),
                    (b"H", b"Height"),
                    (b"BPC", b"BitsPerComponent"),
                    (b"CS", b"ColorSpace"),
                    (b"F", b"Filter"),
                    (b"DP", b"DecodeParms"),
                    (b"IM", b"ImageMask"),
                    (b"D", b"Decode"),
                ] {
                    if let Some(v) = dict.get(from).cloned() {
                        dict.set(to, v);
                    }
                }
                return Some(dict);
            }
            if s == e {
                self.pos += 1;
                continue;
            }
            // 不是字典内容也不是 ID：这是对齐时的"读坏了"，把这个词原样当键值都失败，返回 None。
            return None;
        }
    }

    /// 读内联图像的像素数据，返回 `(数据起点, 数据终点, 解码长度, 是否兜底)`。
    ///
    /// PDFium 的顺序（`CPDF_StreamParser::ReadInlineStream`）：`ID` 之后跳过**一个**空白，
    /// 无过滤器时长度 = ceil(W × ncomp × bpc / 8) × H；有过滤器时解码，数据长度由解码器消费的字节决定，
    /// 再向后找到 `EI`。这里对 Flate/LZW/ASCIIHex/ASCII85/RunLength 做同样的解码求长度，
    /// 其他过滤器（DCT/CCITT）退回找 `EI` 边界。
    fn read_inline_data(&mut self, dict: &PdfDict) -> (usize, usize, Option<usize>, bool) {
        if self.data.get(self.pos).is_some_and(|b| is_whitespace(*b)) {
            self.pos += 1;
        }
        let start = self.pos;
        let width = dict
            .get(b"Width")
            .and_then(PdfObject::as_int)
            .map(|v| v.max(0) as u64)
            .unwrap_or(0);
        let height = dict
            .get(b"Height")
            .and_then(PdfObject::as_int)
            .map(|v| v.max(0) as u64)
            .unwrap_or(0);
        let is_mask = matches!(dict.get(b"ImageMask"), Some(PdfObject::Bool(true)));
        let has_cs = dict.get(b"ColorSpace").is_some();
        let bpc = if is_mask || !has_cs {
            1
        } else {
            dict.get(b"BitsPerComponent")
                .and_then(PdfObject::as_int)
                .unwrap_or(1)
                .max(1) as u64
        };
        let ncomp = if is_mask || !has_cs {
            1
        } else {
            component_count(dict.get(b"ColorSpace"))
        };
        let row_bytes = (width * ncomp * bpc).div_ceil(8);
        let size = row_bytes.saturating_mul(height);
        let filter = dict.get(b"Filter");
        let parms = dict.get(b"DecodeParms");
        let filter_names: Vec<String> = match filter {
            None | Some(PdfObject::Null) => Vec::new(),
            Some(PdfObject::Array(a)) => a
                .iter()
                .filter_map(|o| o.as_name().map(|n| String::from_utf8_lossy(n).into_owned()))
                .collect(),
            Some(other) => other
                .as_name()
                .map(|n| vec![String::from_utf8_lossy(n).into_owned()])
                .unwrap_or_default(),
        };
        let usable = filter_names.iter().all(|f| {
            matches!(
                f.as_str(),
                "FlateDecode"
                    | "LZWDecode"
                    | "ASCIIHexDecode"
                    | "ASCII85Decode"
                    | "RunLengthDecode"
            )
        });
        // 数据终点：无过滤器时按长度；有过滤器时向后找 `EI`（解码长度与原始长度不同，PDFium 也用 EI 定界）。
        let mut end = if filter_names.is_empty() {
            Some(start + usize::try_from(size).unwrap_or(usize::MAX))
        } else {
            find_ei_tail(&self.data[start..]).map(|o| start + o)
        };
        if end.is_none() || end.is_some_and(|e| e > self.data.len()) {
            end = Some(self.data.len());
        }
        let end = end.unwrap_or(self.data.len()).min(self.data.len());
        let mut decoded_ok = false;
        let mut decoded_len = None;
        if usable
            && !filter_names.is_empty()
            && let Ok(decoded) =
                crate::pdf::apply_filter_chain(&self.data[start..end], filter, parms)
        {
            decoded_len = Some(decoded.len());
            decoded_ok = true;
        }
        // 内联图像的数据后面必须有空白再跟 `EI`；没有时记为未终止。
        let mut probe = end;
        while probe < self.data.len() && is_whitespace(self.data[probe]) {
            probe += 1;
        }
        if !self.data[probe..].starts_with(b"EI") {
            self.notes.push(Note {
                offset: start,
                note: NoteKind::InlineImageUnterminated,
            });
            self.pos = self.data.len();
        } else {
            self.pos = probe + 2;
        }
        // 数据按 PDFium 的规则算出的长度与"解码成功"与否一起交给调用方。
        let decoded_len = if filter_names.is_empty() {
            Some(end.saturating_sub(start))
        } else {
            decoded_len
        };
        let fallback = !decoded_ok && !filter_names.is_empty();
        (start, end, decoded_len, fallback)
    }
}

/// 自 `from` 起找"`EI` 前有空白、后有空白/结尾"的位置，返回 `EI` 前的偏移（去掉多写的空白）。
fn find_ei_tail(data: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] == b'E'
            && data[i + 1] == b'I'
            && (i + 2 >= data.len() || is_ws_or_delim(data[i + 2]))
        {
            let mut end = i;
            while end > 0 && is_ws_or_delim(data[end - 1]) {
                end -= 1;
            }
            return Some(end);
        }
        i += 1;
    }
    None
}

fn is_ws_or_delim(b: u8) -> bool {
    is_whitespace(b) || is_delimiter(b)
}

/// 内联图像颜色空间的分量数。名字取已知编码，数组按族名，字典按 /N（Indexed 为 1）。
fn component_count(cs: Option<&PdfObject>) -> u64 {
    match cs {
        Some(PdfObject::Name(n)) => match n.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" | b"Indexed" | b"I" => 1,
            b"DeviceCMYK" | b"CMYK" => 4,
            _ => 3,
        },
        Some(PdfObject::Array(a)) => match a.first().and_then(PdfObject::as_name) {
            Some(b"Indexed") | Some(b"I") => 1,
            Some(b"DeviceN") => a
                .get(1)
                .and_then(PdfObject::as_array)
                .map_or(1, |n| n.len() as u64),
            Some(b"ICCBased") => a
                .get(1)
                .and_then(PdfObject::as_dict)
                .and_then(|d| d.get(b"N"))
                .and_then(PdfObject::as_int)
                .map_or(3, |n| n.max(1) as u64),
            _ => 3,
        },
        Some(PdfObject::Dict(d)) => d
            .get(b"N")
            .and_then(PdfObject::as_int)
            .map_or(3, |n| n.max(1) as u64),
        _ => 1,
    }
}

/// 分词入口。
pub fn tokenize(content: &Content) -> Tokenized {
    let mut lx = Lexer {
        data: &content.data,
        pos: 0,
        stream_starts: &content.segment_starts,
        notes: Vec::new(),
    };
    let mut tokens = Vec::new();
    loop {
        let (lead_start, token_start) = lx.skip_ws();
        if token_start >= lx.data.len() {
            break;
        }
        let stream = lx.segment_of(token_start);
        // 只有以 `BI` 开头的常规词按内联图像处理；其余都走普通记号。
        let first = lx.data[token_start];
        let _is_word = !is_delimiter(first);
        if lx.data.len() >= token_start + 2
            && lx.data[token_start] == b'B'
            && lx.data[token_start + 1] == b'I'
            && (lx.data.len() == token_start + 2
                || is_whitespace(lx.data[token_start + 2])
                || is_delimiter(lx.data[token_start + 2]))
        {
            lx.pos = token_start + 2;
            if let Some(dict) = lx.read_inline_dict() {
                let (ds, de, decoded_len, fallback) = lx.read_inline_data(&dict);
                if fallback {
                    lx.notes.push(Note {
                        offset: token_start,
                        note: NoteKind::InlineImageEidFallback,
                    });
                }
                // 跳过 `EI`（如果存在）。
                let before_ei = lx.pos;
                let _ = lx.skip_ws();
                let (es, ee) = lx.read_word();
                let end = if &lx.data[es..ee] == b"EI" {
                    lx.pos = ee;
                    ee
                } else {
                    lx.pos = before_ei;
                    lx.data.len()
                };
                let end = end.max(lx.pos);
                tokens.push(Token {
                    kind: TokenKind::InlineImage(crate::content::InlineImage {
                        dict,
                        data: (ds, de),
                        decoded_len,
                        ei_fallback: fallback,
                    }),
                    range: TokenRange {
                        start: token_start,
                        end: end.max(token_start + 1),
                    },
                    leading_start: lead_start,
                    stream,
                    op: b"BI".to_vec(),
                    array: None,
                });
                continue;
            }
            // 不是合法的 BI：按普通记号解析这个词。
            lx.pos = token_start;
        }
        let kind = lx.read_token();
        let end = lx.pos;
        let is_op = kind == TokenKind::Operator;
        let op = if is_op {
            lx.data[token_start..end].to_vec()
        } else {
            Vec::new()
        };
        let array = match &kind {
            TokenKind::Array => {
                // 为了拿到解码后的数组，单独再解析一次区间。
                let mut sub = Lexer {
                    data: lx.data,
                    pos: token_start,
                    stream_starts: lx.stream_starts,
                    notes: Vec::new(),
                };
                sub.pos = token_start + 1;
                let array = match sub.read_array(0) {
                    Some(PdfObject::Array(a)) => Some(a),
                    _ => None,
                };
                // 子词法分析器的区间限制在数组范围内，防止越过 `]` 读取。
                let _ = end;
                array
            }
            _ => None,
        };
        tokens.push(Token {
            kind,
            range: TokenRange {
                start: token_start,
                end,
            },
            leading_start: lead_start,
            stream,
            op,
            array,
        });
    }
    Tokenized {
        tokens,
        notes: lx.notes,
    }
}
