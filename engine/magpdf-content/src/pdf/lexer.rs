//! PDF 词法与对象解析（文件结构层）。
//!
//! 与内容流分词器（`crate::content`）分开：这里的输入是文件里的对象语法，字符串和名字的解码
//! 规则与内容流里一致，但不需要记字节区间。解析全部是迭代式+显式深度上限，损坏输入不会 panic。

use super::object::{
    PdfDict, PdfObject, PdfStreamRef, decode_name, hex_value, is_delimiter, is_whitespace,
};
use super::{Error, Result};

/// 嵌套层数上限（数组、字典）。
const MAX_DEPTH: usize = 64;

/// 一个从字节串里读对象的解析器。
#[derive(Debug)]
pub struct Lexer<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub fn peek_byte(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// 跳过空白与注释。
    pub fn skip_ws(&mut self) {
        while let Some(&b) = self.data.get(self.pos) {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(&c) = self.data.get(self.pos) {
                    self.pos += 1;
                    if c == b'\r' || c == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    /// 读一个不带 `/` 的词（常规记号：名字体、数字、关键字）。
    pub fn read_word(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(&b) = self.data.get(self.pos) {
            if is_whitespace(b) || is_delimiter(b) {
                break;
            }
            self.pos += 1;
        }
        &self.data[start..self.pos]
    }

    /// 读 `<...>` 形式的十六进制串（`pos` 指向开头的 `<` 之后）。
    pub fn read_hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(&b) = self.data.get(self.pos) {
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

    /// 读 `(...)` 字面量字符串（`pos` 指向开头的 `(` 之后）。PDFium 的转义规则。
    pub fn read_literal_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        while let Some(&b) = self.data.get(self.pos) {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    out.push(b);
                }
                b'\\' => self.read_escape(&mut out),
                _ => out.push(b),
            }
        }
        out
    }

    fn read_escape(&mut self, out: &mut Vec<u8>) {
        let Some(&b) = self.data.get(self.pos) else {
            return;
        };
        self.pos += 1;
        match b {
            b'0'..=b'7' => {
                let mut v = u16::from(b - b'0');
                for _ in 0..2 {
                    match self.data.get(self.pos) {
                        Some(&c @ b'0'..=b'7') => {
                            v = v * 8 + u16::from(c - b'0');
                            self.pos += 1;
                        }
                        _ => break,
                    }
                }
                out.push(v as u8);
            }
            b'\r' => {
                if self.data.get(self.pos) == Some(&b'\n') {
                    self.pos += 1;
                }
            }
            b'\n' => {}
            other => out.push(other),
        }
    }

    /// 读一个完整对象。`refs` 为真时把 `num generation R` 识别成引用（内容流里不会出现）。
    pub fn read_object(&mut self) -> Result<PdfObject> {
        self.read_object_depth(0)
    }

    fn read_object_depth(&mut self, depth: usize) -> Result<PdfObject> {
        if depth > MAX_DEPTH {
            return Err(Error::Syntax("嵌套过深".to_owned()));
        }
        self.skip_ws();
        let Some(&b) = self.data.get(self.pos) else {
            return Err(Error::Syntax("意外的文件结尾".to_owned()));
        };
        match b {
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    self.read_dict(depth)
                } else {
                    self.pos += 1;
                    Ok(PdfObject::Str(self.read_hex_string()))
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.data.get(self.pos) {
                        None => return Err(Error::Syntax("数组未闭合".to_owned())),
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(_) => items.push(self.read_object_depth(depth + 1)?),
                    }
                }
                Ok(PdfObject::Array(dedupe_refs(items)))
            }
            b'(' => {
                self.pos += 1;
                Ok(PdfObject::Str(self.read_literal_string()))
            }
            b'/' => {
                self.pos += 1;
                Ok(PdfObject::Name(decode_name(self.read_word())))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => Ok(self.read_number_or_ref()),
            _ => {
                let word = self.read_word();
                match word {
                    b"true" => Ok(PdfObject::Bool(true)),
                    b"false" => Ok(PdfObject::Bool(false)),
                    b"null" => Ok(PdfObject::Null),
                    _ => Ok(PdfObject::Name(word.to_vec())),
                }
            }
        }
    }

    fn read_number_or_ref(&mut self) -> PdfObject {
        let word = self.read_word();
        let Some(first) = parse_number(word) else {
            return PdfObject::Null;
        };
        // 试探 `num generation R`
        if let PdfObject::Int(n) = first {
            let save = self.pos;
            self.skip_ws();
            let w2 = self.read_word();
            if let Some(generation) = parse_number(w2).and_then(|o| o.as_int()) {
                self.skip_ws();
                if self.read_word() == b"R" {
                    return PdfObject::Ref(n as u32, generation as u16);
                }
            }
            self.pos = save;
        }
        first
    }

    fn read_dict(&mut self, depth: usize) -> Result<PdfObject> {
        let mut entries = Vec::new();
        loop {
            self.skip_ws();
            match self.data.get(self.pos) {
                None => return Err(Error::Syntax("字典未闭合".to_owned())),
                Some(b'>') => {
                    if self.data.get(self.pos + 1) == Some(&b'>') {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                Some(b'/') => {
                    self.pos += 1;
                    let key = decode_name(self.read_word());
                    self.skip_ws();
                    if self.data.get(self.pos) == Some(&b'>') {
                        entries.push((key, PdfObject::Null));
                        continue;
                    }
                    let value = self.read_object_depth(depth + 1)?;
                    entries.push((key, value));
                }
                Some(_) => {
                    // 不合法的键（没有 `/`）：跳过它，保证不陷入死循环。
                    if self.read_word().is_empty() {
                        self.pos += 1;
                    }
                }
            }
        }
        Ok(PdfObject::Dict(PdfDict::from_entries(entries)))
    }

    /// 在 `<< ... >>` 之后读可选的 `stream ... endstream`。
    pub fn read_stream_data(&mut self, dict: PdfDict) -> Result<PdfObject> {
        self.skip_ws();
        let save = self.pos;
        if self.read_word() != b"stream" {
            self.pos = save;
            return Ok(PdfObject::Dict(dict));
        }
        // stream 关键字之后是 CRLF 或 LF（PDF 规范 7.3.8.1）。
        if self.data.get(self.pos) == Some(&b'\r') {
            self.pos += 1;
        }
        if self.data.get(self.pos) == Some(&b'\n') {
            self.pos += 1;
        }
        let start = self.pos;
        let length = dict
            .get(b"Length")
            .and_then(PdfObject::as_int)
            .filter(|n| *n >= 0)
            .map(|n| n as usize);
        let mut end = None;
        if let Some(len) = length
            && let Some(candidate) = start.checked_add(len)
            && candidate <= self.data.len()
        {
            let mut probe = Lexer::at(self.data, candidate);
            probe.skip_ws();
            let w = probe.read_word();
            if w == b"endstream" {
                end = Some(candidate);
                self.pos = probe.pos;
            }
        }
        let end = match end {
            Some(e) => e,
            None => {
                // `/Length` 不可信（间接引用、写错长度）：退回查找 `endstream`。
                match find(&self.data[start..], b"endstream") {
                    Some(off) => {
                        let mut e = start + off;
                        while e > start && matches!(self.data[e - 1], b'\r' | b'\n') {
                            e -= 1;
                        }
                        self.pos = start + off + b"endstream".len();
                        e
                    }
                    None => {
                        self.pos = self.data.len();
                        self.data.len()
                    }
                }
            }
        };
        Ok(PdfObject::Stream(PdfStreamRef {
            dict,
            data: self.data[start..end.max(start)].to_vec(),
            span: (start, end),
        }))
    }
}

/// PDF 数组里允许的 `n g R` 三段式：这里只是把连续三个记号识别成引用，
/// 在解析阶段就无法可靠判断，所以数组里保持"数字 数字 名字 R"的原样也无妨；
/// 需要引用时在 `read_object_depth` 的数字分支已经处理。此函数保留一个占位以便后续扩展。
fn dedupe_refs(items: Vec<PdfObject>) -> Vec<PdfObject> {
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

/// PDF 数字：`123`、`-4.5`、`.5`、`1.`。
pub(crate) fn parse_number(word: &[u8]) -> Option<PdfObject> {
    if word.is_empty() {
        return None;
    }
    let negative = word[0] == b'-';
    let positive = word[0] == b'+';
    let digits = &word[usize::from(negative || positive)..];
    if digits.is_empty() {
        return None;
    }
    if !digits.iter().all(|b| b.is_ascii_digit() || *b == b'.') {
        return None;
    }
    if digits.iter().filter(|b| **b == b'.').count() > 1 {
        return None;
    }
    let text = std::str::from_utf8(digits).ok()?;
    if !digits.contains(&b'.') {
        let v: i64 = text.parse().ok()?;
        return Some(PdfObject::Int(if negative { -v } else { v }));
    }
    let v: f64 = text.parse().ok()?;
    Some(PdfObject::Real(if negative { -v } else { v }))
}

/// 在字节串里找子串。
pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 在字节串里找子串最后一次出现的位置。
pub(crate) fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(data: &[u8]) -> PdfObject {
        let mut lx = Lexer::new(data);
        lx.read_object().unwrap()
    }

    #[test]
    fn numbers_and_refs() {
        assert_eq!(parse(b"42"), PdfObject::Int(42));
        assert_eq!(parse(b"-4.5"), PdfObject::Real(-4.5));
        assert_eq!(parse(b".5"), PdfObject::Real(0.5));
        assert_eq!(parse(b"1."), PdfObject::Real(1.0));
        assert_eq!(parse(b"12 0 R"), PdfObject::Ref(12, 0));
        assert_eq!(parse(b"true"), PdfObject::Bool(true));
        assert_eq!(parse(b"null"), PdfObject::Null);
    }

    #[test]
    fn strings_and_names() {
        assert_eq!(parse(b"(a(b)c)"), PdfObject::Str(b"a(b)c".to_vec()));
        assert_eq!(parse(b"(a\\050b)"), PdfObject::Str(b"a(b".to_vec()));
        assert_eq!(parse(b"(x\\\ny)"), PdfObject::Str(b"xy".to_vec()));
        assert_eq!(parse(b"<41 4>"), PdfObject::Str(vec![0x41, 0x40]));
        assert_eq!(parse(b"/A#20B"), PdfObject::Name(b"A B".to_vec()));
    }

    #[test]
    fn arrays_and_dicts() {
        let PdfObject::Array(a) = parse(b"[1 2 3 0 R /x (s)]") else {
            panic!()
        };
        assert_eq!(a.len(), 5);
        assert_eq!(a[1], PdfObject::Int(2));
        assert_eq!(a[3], PdfObject::Name(b"x".to_vec()));
        let PdfObject::Dict(d) = parse(b"<< /Type /Page /Count 3 /Kids [1 0 R] >>") else {
            panic!()
        };
        assert_eq!(d.get(b"Type"), Some(&PdfObject::Name(b"Page".to_vec())));
        assert_eq!(d.get(b"Count"), Some(&PdfObject::Int(3)));
    }

    #[test]
    fn broken_input_does_not_panic() {
        for data in [
            b"<<".as_slice(),
            b"[[[[[[",
            b"(unterminated",
            b"<3",
            b"<< /A >>",
            b"<< /A",
            b"/",
        ] {
            let mut lx = Lexer::new(data);
            let _ = lx.read_object();
        }
    }

    #[test]
    fn stream_with_length_and_without() {
        let src = b"<< /Length 5 >>\nstream\nabcde\nendstream";
        let mut lx = Lexer::new(src);
        let PdfObject::Dict(d) = lx.read_object().unwrap() else {
            panic!()
        };
        let PdfObject::Stream(s) = lx.read_stream_data(d).unwrap() else {
            panic!()
        };
        assert_eq!(s.data, b"abcde");

        let src = b"<< /Length 99 >>\nstream\nabcde\nendstream";
        let mut lx = Lexer::new(src);
        let PdfObject::Dict(d) = lx.read_object().unwrap() else {
            panic!()
        };
        let PdfObject::Stream(s) = lx.read_stream_data(d).unwrap() else {
            panic!()
        };
        assert_eq!(s.data, b"abcde");
    }
}
