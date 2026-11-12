//! 文件结构层的取词与对象解析，照 PDFium 的 `CPDF_SyntaxParser` 移植
//! （`core/fpdfapi/parser/cpdf_syntax_parser.cpp`，BSD-3-Clause，版权声明见本 crate 的 `PDFIUM-NOTICE.md`）。
//!
//! 与内容流的词法层（`crate::content::parser`）分开：这里读的是文件里的对象，要认间接引用和
//! `stream … endstream`。容错规则照 PDFium：
//!
//! - 字典里同名键后来的覆盖先前的；值读不出来的键跳过；遇到 `endobj` 当作字典结束；
//! - 数组读到任何读不成对象的词就结束（正常情况下这个词是 `]`）；
//! - 只由 `0-9 + - .` 组成的词是数字；`数字 数字 R` 是引用；
//! - 流的长度先信 `/Length`（之后紧跟 `endstream` 才算数），否则找 `endstream` 或 `endobj`。
//!
//! 所有循环都往前走，嵌套有深度上限，损坏的输入不会 panic 也不会死循环。

use super::object::{PdfDict, PdfObject, PdfStreamRef};
use super::{Error, Result};
use crate::content::parser::{
    Number, is_delimiter, is_numeric, is_whitespace, name_decode, read_string_at,
};

/// 嵌套层数上限（`kParserMaxRecursionDepth`）。
const MAX_DEPTH: usize = 64;

/// 把 `/Length` 的值变成整数的办法（可能要解引用）；`None` 表示没有可用的长度。
pub type LengthOf<'f> = &'f dyn Fn(&PdfObject) -> Option<i64>;

/// 只认直接写的数字（重建交叉引用时没有对象表可查）。
pub fn direct_length(value: &PdfObject) -> Option<i64> {
    match value {
        PdfObject::Int(_) | PdfObject::Real(_) => Some(i64::from(value.get_integer())),
        _ => None,
    }
}

/// 一个从字节串里读对象的解析器。
#[derive(Debug)]
pub struct Lexer<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) pos: usize,
    /// 最近读到的非空词的第一个字节（判断数组是不是被 `]` 结束的）。
    last_first: u8,
}

/// `FXSYS_atoui`：可选的正负号加十进制数字，溢出时取最大值，负数回绕。
pub(crate) fn atoui(word: &[u8]) -> u32 {
    let (negative, digits) = match word.first() {
        Some(b'-') => (true, &word[1..]),
        Some(b'+') => (false, &word[1..]),
        _ => (false, word),
    };
    let mut value = 0u32;
    for &c in digits {
        if !c.is_ascii_digit() {
            break;
        }
        let d = u32::from(c - b'0');
        if value > (u32::MAX - d) / 10 {
            return u32::MAX;
        }
        value = value * 10 + d;
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

/// `FXSYS_atoi64`：可选的正负号加十进制数字，溢出时取端点。
pub(crate) fn atoi64(word: &[u8]) -> i64 {
    let (negative, digits) = match word.first() {
        Some(b'-') => (true, &word[1..]),
        Some(b'+') => (false, &word[1..]),
        _ => (false, word),
    };
    let mut value = 0i64;
    for &c in digits {
        if !c.is_ascii_digit() {
            break;
        }
        let d = i64::from(c - b'0');
        if value > (i64::MAX - d) / 10 {
            return if negative { i64::MIN } else { i64::MAX };
        }
        value = value * 10 + d;
    }
    if negative { -value } else { value }
}

/// `CPDF_Number(ByteStringView)`：整数或单精度浮点。
fn number_object(word: &[u8]) -> PdfObject {
    match Number::parse(word) {
        Number::Unsigned(v) => PdfObject::Int(i64::from(v)),
        Number::Signed(v) => PdfObject::Int(i64::from(v)),
        Number::Float(v) => PdfObject::Real(f64::from(v)),
    }
}

/// `ReadEOLMarkers`：`pos` 处是 `\r\n` 得 2，单个 `\r` 或 `\n` 得 1，否则 0。
pub(crate) fn eol_markers(data: &[u8], pos: usize) -> usize {
    match data.get(pos) {
        Some(b'\r') => {
            if data.get(pos + 1) == Some(&b'\n') {
                2
            } else {
                1
            }
        }
        Some(b'\n') => 1,
        _ => 0,
    }
}

/// 普通字符：既不是空白也不是分隔符。
fn is_regular(ch: u8) -> bool {
    !is_whitespace(ch) && !is_delimiter(ch)
}

/// `IsWholeWord`（`checkKeyword` 为真的那种）：两边不能紧挨着普通字符或分隔符。
fn is_whole_keyword(data: &[u8], start: usize, len: usize) -> bool {
    let joins = |ch: u8| is_regular(ch) || is_delimiter(ch);
    if let Some(&after) = data.get(start + len)
        && joins(after)
    {
        return false;
    }
    !(start > 0 && joins(data[start - 1]))
}

/// `FindWordPos`：从 `from` 起找第一个独立成词的 `word`。
fn find_word(data: &[u8], from: usize, word: &[u8]) -> Option<usize> {
    let mut at = from;
    while at + word.len() <= data.len() {
        let off = data[at..].windows(word.len()).position(|w| w == word)?;
        let start = at + off;
        if is_whole_keyword(data, start, word.len()) {
            return Some(start);
        }
        at = start + word.len();
    }
    None
}

/// `FindStreamEndPos`：`/Length` 不可用时，数据到 `endstream` 或 `endobj`（取先出现的）之前为止，
/// 再去掉紧挨着的一个行尾。
fn find_stream_end(data: &[u8], start: usize) -> Option<usize> {
    let end_stream = find_word(data, start, b"endstream");
    let end_obj = find_word(data, start, b"endobj");
    let mut end = match (end_stream, end_obj) {
        (None, None) => return None,
        (Some(s), None) => s,
        (None, Some(o)) => o,
        (Some(s), Some(o)) => s.min(o),
    };
    if end >= 2 && eol_markers(data, end - 2) == 2 {
        end -= 2;
    } else if end >= 1 && eol_markers(data, end - 1) == 1 {
        end -= 1;
    }
    (end >= start).then_some(end)
}

/// 一个 `N G obj …`。
#[derive(Debug)]
pub struct Indirect {
    pub number: u32,
    pub generation: u32,
    /// 对象体；读不出来时为空。
    pub object: Option<PdfObject>,
    /// 读完之后的位置。
    pub end: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self::at(data, 0)
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos: pos.min(data.len()),
            last_first: 0,
        }
    }

    /// `ToNextWord`：跳过空白与注释，停在下一个词的第一个字节上。
    pub fn skip_to_word(&mut self) {
        let data = self.data;
        loop {
            while self.pos < data.len() && is_whitespace(data[self.pos]) {
                self.pos += 1;
            }
            if self.pos >= data.len() || data[self.pos] != b'%' {
                return;
            }
            while self.pos < data.len() && !matches!(data[self.pos], b'\r' | b'\n') {
                self.pos += 1;
            }
        }
    }

    /// `GetNextWord`：返回（词，是不是数字）。到数据末尾时词为空（这时“是数字”为真）。
    ///
    /// 词的种类：`/名字`（连同斜杠）、`<<`、`>>`、单个分隔符、一串普通字符。
    pub fn next_word(&mut self) -> (&'a [u8], bool) {
        let data = self.data;
        let len = data.len();
        self.skip_to_word();
        if self.pos >= len {
            return (&[], true);
        }
        let start = self.pos;
        let ch = data[self.pos];
        self.pos += 1;
        self.last_first = ch;
        if is_delimiter(ch) {
            match ch {
                b'/' => {
                    while self.pos < len && is_regular(data[self.pos]) {
                        self.pos += 1;
                    }
                }
                b'<' | b'>' if self.pos < len && data[self.pos] == ch => {
                    self.pos += 1;
                }
                _ => {}
            }
            return (&data[start..self.pos], false);
        }
        let mut is_number = is_numeric(ch);
        while self.pos < len && is_regular(data[self.pos]) {
            if !is_numeric(data[self.pos]) {
                is_number = false;
            }
            self.pos += 1;
        }
        (&data[start..self.pos], is_number)
    }

    /// `GetDirectNum`：下一个词是数字时取它的值，否则是 0。
    pub fn direct_num(&mut self) -> u32 {
        let (word, is_number) = self.next_word();
        if is_number { atoui(word) } else { 0 }
    }

    /// `ReadHexString`：`<` 已经读过。
    pub fn read_hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut first = true;
        let mut code = 0u8;
        while self.pos < self.data.len() {
            let ch = self.data[self.pos];
            self.pos += 1;
            if ch == b'>' {
                break;
            }
            let value = match ch {
                b'0'..=b'9' => ch - b'0',
                b'a'..=b'f' => ch - b'a' + 10,
                b'A'..=b'F' => ch - b'A' + 10,
                _ => continue,
            };
            if first {
                code = value * 16;
            } else {
                code += value;
                out.push(code);
            }
            first = !first;
        }
        if !first {
            out.push(code);
        }
        out
    }

    /// `ReadString`：`(` 已经读过。
    pub fn read_literal_string(&mut self) -> Vec<u8> {
        read_string_at(self.data, &mut self.pos)
    }

    /// 读一个完整对象（宽松模式）。字典后面跟着 `stream` 时不读流，停在字典之后。
    pub fn read_object(&mut self) -> Result<PdfObject> {
        self.read_body(false, 0)
            .ok_or_else(|| Error::Syntax("这里读不出对象".to_owned()))
    }

    /// `GetObjectBodyInternal`。`strict` 只影响最外层：字典里有读不出来的值、数组不是被 `]` 结束
    /// 时整个对象作废。
    pub(crate) fn read_body(&mut self, strict: bool, depth: usize) -> Option<PdfObject> {
        if depth > MAX_DEPTH {
            return None;
        }
        let saved = self.pos;
        let (word, is_number) = self.next_word();
        if word.is_empty() {
            return None;
        }
        if is_number {
            let after = self.pos;
            let (second, second_is_number) = self.next_word();
            if second_is_number {
                let (third, _) = self.next_word();
                if third == b"R" {
                    let number = atoui(word);
                    if number == u32::MAX {
                        return None;
                    }
                    return Some(PdfObject::Ref(number, atoui(second) as u16));
                }
            }
            self.pos = after;
            return Some(number_object(word));
        }
        match word {
            b"true" => Some(PdfObject::Bool(true)),
            b"false" => Some(PdfObject::Bool(false)),
            b"null" => Some(PdfObject::Null),
            b"(" => Some(PdfObject::Str(self.read_literal_string())),
            b"<" => Some(PdfObject::Str(self.read_hex_string())),
            b"[" => {
                let mut items = Vec::new();
                while let Some(item) = self.read_body(false, depth + 1) {
                    items.push(item);
                }
                (!strict || self.last_first == b']').then_some(PdfObject::Array(items))
            }
            b"<<" => self.read_dict(strict, depth).map(PdfObject::Dict),
            b">>" => {
                self.pos = saved;
                None
            }
            _ if word[0] == b'/' => Some(PdfObject::Name(name_decode(&word[1..]))),
            _ => None,
        }
    }

    /// 字典体：`<<` 已经读过。
    fn read_dict(&mut self, strict: bool, depth: usize) -> Option<PdfDict> {
        let mut dict = PdfDict::new();
        loop {
            let (inner, _) = self.next_word();
            if inner.is_empty() {
                return None;
            }
            if inner == b">>" {
                break;
            }
            if inner == b"endobj" {
                self.pos -= inner.len();
                break;
            }
            if inner[0] != b'/' {
                continue;
            }
            let key = name_decode(&inner[1..]);
            let Some(value) = self.read_body(false, depth + 1) else {
                if strict {
                    self.skip_line();
                    return None;
                }
                continue;
            };
            if !key.is_empty() {
                dict.set(&key, value);
            }
        }
        Some(dict)
    }

    /// `ToNextLine`：跳到下一个行尾之后（单独的 `\r` 也算行尾）。
    pub(crate) fn skip_line(&mut self) {
        let data = self.data;
        while self.pos < data.len() {
            let ch = data[self.pos];
            self.pos += 1;
            if ch == b'\n' {
                break;
            }
            if ch == b'\r' {
                if data.get(self.pos) == Some(&b'\n') {
                    self.pos += 1;
                }
                break;
            }
        }
    }

    /// `GetIndirectObject`：读 `N G obj` 和对象体；字典后面跟着 `stream` 时把流也读出来。
    /// 开头不是 `数字 数字 obj` 时返回 `None`。
    pub fn read_indirect(&mut self, strict: bool, length_of: LengthOf<'_>) -> Option<Indirect> {
        let (word, is_number) = self.next_word();
        if !is_number || word.is_empty() {
            return None;
        }
        let number = atoui(word);
        let (word, is_number) = self.next_word();
        if !is_number || word.is_empty() {
            return None;
        }
        let generation = atoui(word);
        if self.next_word().0 != b"obj" {
            return None;
        }
        let object = match self.read_body(strict, 0) {
            Some(PdfObject::Dict(dict)) => {
                let after_dict = self.pos;
                if self.next_word().0 == b"stream" {
                    self.read_stream(dict, length_of).map(PdfObject::Stream)
                } else {
                    self.pos = after_dict;
                    Some(PdfObject::Dict(dict))
                }
            }
            other => other,
        };
        Some(Indirect {
            number,
            generation,
            object,
            end: self.pos,
        })
    }

    /// `ReadStream`：`stream` 关键字已经读过。
    fn read_stream(&mut self, dict: PdfDict, length_of: LengthOf<'_>) -> Option<PdfStreamRef> {
        let data = self.data;
        self.skip_line();
        let start = self.pos;
        let mut len: i64 = dict.get(b"Length").and_then(length_of).unwrap_or(-1);
        if len > 0 && (start as i64).saturating_add(len) >= data.len() as i64 {
            len = -1;
        }
        let mut found = None;
        if len >= 0 {
            let candidate = start + len as usize;
            let mut probe = Lexer::at(data, candidate + eol_markers(data, candidate));
            // `/Length` 之后紧跟着以 `endstream` 开头的词才算数。
            if probe.next_word().0.starts_with(b"endstream") {
                found = Some((candidate, probe.pos));
            }
        }
        let (end, after) = match found {
            Some(found) => found,
            None => {
                let end = find_stream_end(data, start)?;
                let mut probe = Lexer::at(data, end);
                let after = if probe.next_word().0 == b"endstream" {
                    probe.pos
                } else {
                    end
                };
                (end, after)
            }
        };
        self.pos = after;
        Some(PdfStreamRef {
            dict,
            data: data[start..end].to_vec(),
            span: (start, end),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(data: &[u8]) -> PdfObject {
        let mut lx = Lexer::new(data);
        lx.read_object().unwrap()
    }

    fn indirect(data: &[u8], strict: bool) -> Option<Indirect> {
        Lexer::new(data).read_indirect(strict, &direct_length)
    }

    #[test]
    fn numbers_and_refs() {
        assert_eq!(parse(b"42"), PdfObject::Int(42));
        assert_eq!(parse(b"-4.5"), PdfObject::Real(-4.5));
        assert_eq!(parse(b".5"), PdfObject::Real(0.5));
        assert_eq!(parse(b"1."), PdfObject::Real(1.0));
        assert_eq!(parse(b"12 0 R"), PdfObject::Ref(12, 0));
        assert_eq!(parse(b"12 0 Rx"), PdfObject::Int(12));
        assert_eq!(parse(b"true"), PdfObject::Bool(true));
        assert_eq!(parse(b"null"), PdfObject::Null);
        // 只由数字类字符组成的词都是数字，取值按 FX_Number。
        assert_eq!(parse(b"1-2"), PdfObject::Int(1));
        assert_eq!(parse(b"--"), PdfObject::Int(0));
    }

    #[test]
    fn strings_and_names() {
        assert_eq!(parse(b"(a(b)c)"), PdfObject::Str(b"a(b)c".to_vec()));
        assert_eq!(parse(b"(a\\050b)"), PdfObject::Str(b"a(b".to_vec()));
        assert_eq!(parse(b"(x\\\ny)"), PdfObject::Str(b"xy".to_vec()));
        assert_eq!(parse(b"(a\\nb\\t)"), PdfObject::Str(b"a\nb\t".to_vec()));
        assert_eq!(parse(b"<41 4>"), PdfObject::Str(vec![0x41, 0x40]));
        assert_eq!(parse(b"/A#20B"), PdfObject::Name(b"A B".to_vec()));
    }

    #[test]
    fn arrays_and_dicts() {
        let PdfObject::Array(a) = parse(b"[1 2 3 0 R /x (s)]") else {
            panic!()
        };
        // `2 3 0` 后面不是 R，所以 2 是数字；`3 0 R` 是引用。
        assert_eq!(a.len(), 5);
        assert_eq!(a[1], PdfObject::Int(2));
        assert_eq!(a[2], PdfObject::Ref(3, 0));
        assert_eq!(a[3], PdfObject::Name(b"x".to_vec()));
        let PdfObject::Dict(d) = parse(b"<< /Type /Page /Count 3 /Kids [1 0 R] >>") else {
            panic!()
        };
        assert_eq!(d.get(b"Type"), Some(&PdfObject::Name(b"Page".to_vec())));
        assert_eq!(d.get(b"Count"), Some(&PdfObject::Int(3)));
    }

    #[test]
    fn dict_quirks_follow_pdfium() {
        // 同名键：后来的覆盖先前的。
        let PdfObject::Dict(d) = parse(b"<< /A 1 /A 2 >>") else {
            panic!()
        };
        assert_eq!(d.get(b"A"), Some(&PdfObject::Int(2)));
        assert_eq!(d.len(), 1);
        // 值读不出来的键跳过；不是名字的词跳过。
        let PdfObject::Dict(d) = parse(b"<< /A foo /B 1 junk /C >>") else {
            panic!()
        };
        assert!(d.get(b"A").is_none());
        assert_eq!(d.get(b"B"), Some(&PdfObject::Int(1)));
        assert!(d.get(b"C").is_none());
        // `endobj` 结束没闭合的字典，位置退回 `endobj` 之前。
        let data = b"<< /A 1 endobj";
        let mut lx = Lexer::new(data);
        assert!(matches!(lx.read_object(), Ok(PdfObject::Dict(_))));
        assert_eq!(&data[lx.pos..], b"endobj");
    }

    #[test]
    fn array_ends_at_the_first_unreadable_word() {
        let data = b"[1 2 foo 3] 4";
        let mut lx = Lexer::new(data);
        let Ok(PdfObject::Array(a)) = lx.read_object() else {
            panic!()
        };
        assert_eq!(a, [PdfObject::Int(1), PdfObject::Int(2)]);
        assert_eq!(&data[lx.pos..], b" 3] 4");
        // 严格模式下，不是被 `]` 结束的数组作废。
        assert!(Lexer::new(data).read_body(true, 0).is_none());
        assert!(Lexer::new(b"[1 2]").read_body(true, 0).is_some());
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
            b">>",
            b"]",
            b"",
        ] {
            let mut lx = Lexer::new(data);
            let _ = lx.read_object();
            let _ = indirect(data, true);
        }
        let deep = vec![b'['; 5000];
        assert!(Lexer::new(&deep).read_object().is_ok());
    }

    #[test]
    fn stream_with_length_and_without() {
        let got = indirect(
            b"1 0 obj << /Length 5 >>\nstream\nabcde\nendstream endobj",
            true,
        )
        .expect("是对象");
        assert_eq!((got.number, got.generation), (1, 0));
        let Some(PdfObject::Stream(s)) = got.object else {
            panic!()
        };
        assert_eq!(s.data, b"abcde");

        // 长度写错：找 endstream，去掉紧挨着的一个行尾。
        let got = indirect(
            b"1 0 obj << /Length 99 >>\nstream\nabcde\r\nendstream",
            true,
        )
        .unwrap();
        let Some(PdfObject::Stream(s)) = got.object else {
            panic!()
        };
        assert_eq!(s.data, b"abcde");

        // 长度偏短但后面不是 endstream：同样退回去找。
        let got = indirect(b"1 0 obj << /Length 2 >>\nstream\nabcde\nendstream", true).unwrap();
        let Some(PdfObject::Stream(s)) = got.object else {
            panic!()
        };
        assert_eq!(s.data, b"abcde");

        // 没有 endstream 时到 endobj 为止。
        let got = indirect(b"1 0 obj << >> stream\nq Q\nendobj", true).unwrap();
        let Some(PdfObject::Stream(s)) = got.object else {
            panic!()
        };
        assert_eq!(s.data, b"q Q");

        // 两个都没有：流读不出来。
        let got = indirect(b"1 0 obj << >> stream\nq Q", true).unwrap();
        assert!(got.object.is_none());
    }

    #[test]
    fn stream_keyword_line_and_zero_length() {
        // stream 之后到行尾的内容都跳过；单独的 \r 也算行尾。
        let got = indirect(b"3 0 obj << /Length 3 >> stream junk\rq Q\nendstream", true).unwrap();
        let Some(PdfObject::Stream(s)) = got.object else {
            panic!()
        };
        assert_eq!(s.data, b"q Q");
        let got = indirect(b"3 0 obj << /Length 0 >> stream\nendstream", true).unwrap();
        let Some(PdfObject::Stream(s)) = got.object else {
            panic!()
        };
        assert!(s.data.is_empty());
    }

    #[test]
    fn words_follow_pdfium() {
        let mut lx = Lexer::new(b"12 0 obj<</A/B>>% c\nendobj\x80x");
        let words: Vec<&[u8]> = std::iter::from_fn(|| {
            let (w, _) = lx.next_word();
            (!w.is_empty()).then_some(w)
        })
        .collect();
        assert_eq!(
            words,
            [
                &b"12"[..],
                b"0",
                b"obj",
                b"<<",
                b"/A",
                b"/B",
                b">>",
                b"endobj",
                b"x"
            ]
        );
        assert_eq!(atoui(b"12"), 12);
        assert_eq!(atoui(b"+7"), 7);
        assert_eq!(atoui(b"99999999999"), u32::MAX);
        assert_eq!(atoui(b"12abc"), 12);
        assert_eq!(atoi64(b"-15"), -15);
        assert_eq!(atoi64(b"0000000123 00000 n"), 123);
    }
}
