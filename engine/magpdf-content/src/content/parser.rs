//! 内容流的词法层，照 PDFium 的 `CPDF_StreamParser` 移植
//! （`core/fpdfapi/page/cpdf_streamparser.cpp`，BSD-3-Clause，版权声明见本 crate 的 `PDFIUM-NOTICE.md`）。
//!
//! 对应层要数出和 PDFium 一样多的对象，前提是把字节切成和 PDFium 一样的元素。所以这里不按
//! PDF 规范“应该怎样”写，而是逐条照搬 PDFium 的做法，包括它的怪处：
//!
//! - 空白除了规范里的六个，还有 `0x80` 和 `0xFF`；
//! - 只由 `0-9 + - .` 组成的词就是数字（`--`、`1-2`、`.` 都算），取值按 `FX_Number`；
//! - 名字里的 `#` 只要后面还有两个字节就一起吃掉，不检查是不是十六进制；
//! - 顶层数组里不允许再嵌数组：里面的 `[` 被跳过，里面的 `]` 结束外层数组；
//! - 字典的键不是名字时整个对象作废（返回空）；
//! - 词最长 255 字节，超出的部分读过去但不记。
//!
//! 每个元素记下起止偏移，写回层靠它原样保留没改的字节。

/// 词的最大长度（`kMaxWordLength`）。
const MAX_WORD: usize = 255;
/// 字符串的最大长度（`kMaxStringLength`）。
const MAX_STRING: usize = 32767;
/// 对象嵌套的最大深度（`CPDF_SyntaxParser::kParserMaxRecursionDepth`）。
const MAX_RECURSION: u32 = 64;

/// 字节类别（PDFium 的 `kPDFCharTypes`）。
pub(crate) fn is_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20 | 0x80 | 0xFF)
}

pub(crate) fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'%' | b'(' | b')' | b'/' | b'<' | b'>' | b'[' | b']' | b'{' | b'}'
    )
}

pub(crate) fn is_numeric(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'+' | b'-' | b'.')
}

pub(crate) fn is_line_ending(b: u8) -> bool {
    matches!(b, b'\r' | b'\n')
}

/// `FXSYS_HexCharToInt`：不是十六进制数字时得 0。
fn hex_char_to_int(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// `PDF_NameDecode`：`#` 后面还有两个字节时，三个字节合成一个，不管那两个是不是十六进制数字。
pub fn name_decode(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            out.push(
                hex_char_to_int(raw[i + 1])
                    .wrapping_mul(16)
                    .wrapping_add(hex_char_to_int(raw[i + 2])),
            );
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    out
}

/// 数字，按 PDFium 的 `FX_Number`：没有小数点的是整数（无符号 32 位，带符号时是有符号 32 位），
/// 有小数点的是单精度浮点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    Unsigned(u32),
    Signed(i32),
    Float(f32),
}

impl Number {
    /// `FX_Number(ByteStringView)`。
    pub fn parse(word: &[u8]) -> Self {
        if word.is_empty() {
            return Self::Unsigned(0);
        }
        if word.contains(&b'.') {
            return Self::Float(string_to_float(word));
        }
        let (signed, negative, digits) = match word[0] {
            b'+' => (true, false, &word[1..]),
            b'-' => (true, true, &word[1..]),
            _ => (false, false, word),
        };
        // 溢出 32 位无符号数时整个值记 0。
        let mut value: Option<u32> = Some(0);
        for &c in digits {
            if !c.is_ascii_digit() {
                break;
            }
            value = value
                .and_then(|v| v.checked_mul(10))
                .and_then(|v| v.checked_add(u32::from(c - b'0')));
        }
        let mut value = value.unwrap_or(0);
        if !signed {
            return Self::Unsigned(value);
        }
        let limit = i32::MAX as u32;
        if value > if negative { limit + 1 } else { limit } {
            value = 0;
        }
        if negative {
            Self::Signed((value as i32).wrapping_neg())
        } else {
            Self::Signed(value as i32)
        }
    }

    /// `FX_Number::GetFloat`。
    pub fn to_f32(self) -> f32 {
        match self {
            Self::Unsigned(v) => v as f32,
            Self::Signed(v) => v as f32,
            Self::Float(v) => v,
        }
    }

    /// `FX_Number::GetSigned`。
    pub fn to_i32(self) -> i32 {
        match self {
            Self::Unsigned(v) => v as i32,
            Self::Signed(v) => v,
            // `saturated_cast`：NaN 得 0，越界取端点；Rust 的 `as` 正是这个语义。
            Self::Float(v) => v as i32,
        }
    }
}

/// `StringToFloat`：跳过前面的正负号（最后一个符号决定正负），再取最长的 `数字[.数字]` 前缀；
/// 一个数字都没有时得 0。
fn string_to_float(word: &[u8]) -> f32 {
    let mut i = 0;
    let mut negative = false;
    while i < word.len() && matches!(word[i], b'+' | b'-') {
        negative = word[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut digits = 0usize;
    while i < word.len() && word[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < word.len() && word[i] == b'.' {
        i += 1;
        while i < word.len() && word[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return 0.0;
    }
    let value = std::str::from_utf8(&word[start..i])
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.0);
    if negative { -value } else { value }
}

/// `ReadString`：`(` 已经读过，`pos` 在它后面；返回时 `pos` 在配对的 `)` 之后（没闭合时在数据末尾）。
/// 文件结构层（`CPDF_SyntaxParser::ReadString`）和内容流层用的是同一套状态机，这里不截断长度。
pub(crate) fn read_string_at(buf: &[u8], pos: &mut usize) -> Vec<u8> {
    let len = buf.len();
    let mut out = Vec::new();
    if *pos >= len {
        return out;
    }
    let mut depth = 0i32;
    let mut status = 0u8;
    let mut esc = 0u32;
    let mut ch = buf[*pos];
    *pos += 1;
    loop {
        // `continue 'again` 对应 PDFium 里不取下一个字节、用同一个字节再走一遍状态机。
        let mut again = true;
        while again {
            again = false;
            match status {
                0 => match ch {
                    b')' => {
                        if depth == 0 {
                            return out;
                        }
                        depth -= 1;
                        out.push(b')');
                    }
                    b'(' => {
                        depth += 1;
                        out.push(b'(');
                    }
                    b'\\' => status = 1,
                    _ => out.push(ch),
                },
                1 => {
                    if (b'0'..=b'7').contains(&ch) {
                        esc = u32::from(ch - b'0');
                        status = 2;
                    } else if ch == b'\r' {
                        status = 4;
                    } else {
                        match ch {
                            b'\n' => {}
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'b' => out.push(0x08),
                            b'f' => out.push(0x0C),
                            other => out.push(other),
                        }
                        status = 0;
                    }
                }
                2 => {
                    if (b'0'..=b'7').contains(&ch) {
                        esc = esc * 8 + u32::from(ch - b'0');
                        status = 3;
                    } else {
                        out.push(esc as u8);
                        status = 0;
                        again = true;
                    }
                }
                3 => {
                    if (b'0'..=b'7').contains(&ch) {
                        esc = esc * 8 + u32::from(ch - b'0');
                        out.push(esc as u8);
                        status = 0;
                    } else {
                        out.push(esc as u8);
                        status = 0;
                        again = true;
                    }
                }
                _ => {
                    status = 0;
                    if ch != b'\n' {
                        again = true;
                    }
                }
            }
        }
        if *pos >= len {
            return out;
        }
        ch = buf[*pos];
        *pos += 1;
    }
}

/// 内容流里的对象（操作数）。
#[derive(Debug, Clone, PartialEq)]
pub enum Obj {
    Null,
    Bool(bool),
    Number(Number),
    Name(Vec<u8>),
    /// 字符串解码之后的字节。
    Str(Vec<u8>),
    Array(Vec<Obj>),
    /// 键是解码后的名字；同名键后来的覆盖先前的（`CPDF_Dictionary::SetFor`）。
    Dict(Vec<(Vec<u8>, Obj)>),
}

impl Obj {
    /// `CPDF_Object::GetString`：名字和字符串返回内容，布尔值返回 `true`/`false`，别的返回空。
    ///
    /// PDFium 里数字对象会返回它的十进制写法；这里不需要那个值本身，只需要“不是空串”，
    /// 所以用一个不会和任何资源名相同的占位。
    pub fn get_string(&self) -> Vec<u8> {
        match self {
            Self::Name(v) | Self::Str(v) => v.clone(),
            Self::Bool(true) => b"true".to_vec(),
            Self::Bool(false) => b"false".to_vec(),
            Self::Number(_) => b"\x00number".to_vec(),
            _ => Vec::new(),
        }
    }

    /// `CPDF_Object::GetNumber`：只有数字有值，别的是 0。
    pub fn get_number(&self) -> f32 {
        match self {
            Self::Number(n) => n.to_f32(),
            _ => 0.0,
        }
    }

    /// `CPDF_Object::GetInteger`。
    pub fn get_integer(&self) -> i32 {
        match self {
            Self::Number(n) => n.to_i32(),
            _ => 0,
        }
    }

    pub fn as_array(&self) -> Option<&[Obj]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&[(Vec<u8>, Obj)]> {
        match self {
            Self::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn is_string(&self) -> bool {
        matches!(self, Self::Str(_))
    }

    pub fn is_name(&self) -> bool {
        matches!(self, Self::Name(_))
    }
}

/// 在内联字典里按键取值。
pub fn dict_get<'a>(dict: &'a [(Vec<u8>, Obj)], key: &[u8]) -> Option<&'a Obj> {
    dict.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// `CPDF_Dictionary::SetFor`：已有同名键时替换它的值。
pub fn dict_set(dict: &mut Vec<(Vec<u8>, Obj)>, key: Vec<u8>, value: Obj) {
    match dict.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => dict.push((key, value)),
    }
}

/// 元素类别（`CPDF_StreamParser::ElementType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Element {
    EndOfData,
    Number,
    Keyword,
    Name,
    /// 字符串、数组、字典、`true`/`false`/`null`，以及落单的 `)`、`>`、`]`、`{`、`}`（对象为空）。
    Other,
}

/// 词法分析器。`pos` 是下一次读取的位置。
#[derive(Debug)]
pub struct StreamParser<'a> {
    buf: &'a [u8],
    pos: usize,
    word: Vec<u8>,
    object: Option<Obj>,
    start: usize,
}

impl<'a> StreamParser<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            word: Vec::with_capacity(32),
            object: None,
            start: 0,
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos.min(self.buf.len());
    }

    pub fn buf(&self) -> &'a [u8] {
        self.buf
    }

    /// 刚读到的词（数字、名字连同开头的 `/`、关键字）。
    pub fn word(&self) -> &[u8] {
        &self.word
    }

    /// 刚读到的元素的第一个字节的偏移（跳过空白和注释之后）。
    pub fn start(&self) -> usize {
        self.start
    }

    /// 刚读到的 [`Element::Other`] 的对象。落单的分隔符没有对象。
    pub fn take_object(&mut self) -> Option<Obj> {
        self.object.take()
    }

    /// `ParseNextElement`。
    pub fn next_element(&mut self) -> Element {
        self.object = None;
        self.word.clear();
        self.start = self.pos;
        let len = self.buf.len();
        if self.pos >= len {
            return Element::EndOfData;
        }
        let mut ch = self.buf[self.pos];
        self.pos += 1;
        loop {
            while is_whitespace(ch) {
                if self.pos >= len {
                    return Element::EndOfData;
                }
                ch = self.buf[self.pos];
                self.pos += 1;
            }
            if ch != b'%' {
                break;
            }
            loop {
                if self.pos >= len {
                    return Element::EndOfData;
                }
                ch = self.buf[self.pos];
                self.pos += 1;
                if is_line_ending(ch) {
                    break;
                }
            }
        }
        self.start = self.pos - 1;

        if is_delimiter(ch) && ch != b'/' {
            self.pos -= 1;
            self.object = self.read_next_object(false, false, 0);
            return Element::Other;
        }

        let mut is_number = true;
        loop {
            if self.word.len() < MAX_WORD {
                self.word.push(ch);
            }
            if !is_numeric(ch) {
                is_number = false;
            }
            if self.pos >= len {
                break;
            }
            ch = self.buf[self.pos];
            self.pos += 1;
            if is_delimiter(ch) || is_whitespace(ch) {
                self.pos -= 1;
                break;
            }
        }
        if is_number {
            return Element::Number;
        }
        if self.word[0] == b'/' {
            return Element::Name;
        }
        match self.word.as_slice() {
            b"true" => {
                self.object = Some(Obj::Bool(true));
                Element::Other
            }
            b"false" => {
                self.object = Some(Obj::Bool(false));
                Element::Other
            }
            b"null" => {
                self.object = Some(Obj::Null);
                Element::Other
            }
            _ => Element::Keyword,
        }
    }

    /// `GetNextWord`。返回这个词是不是数字；读到数据末尾时词为空。
    fn next_word(&mut self) -> bool {
        self.word.clear();
        let len = self.buf.len();
        if self.pos >= len {
            return true;
        }
        let mut ch = self.buf[self.pos];
        self.pos += 1;
        loop {
            while is_whitespace(ch) {
                if self.pos >= len {
                    return true;
                }
                ch = self.buf[self.pos];
                self.pos += 1;
            }
            if ch != b'%' {
                break;
            }
            loop {
                if self.pos >= len {
                    return true;
                }
                ch = self.buf[self.pos];
                self.pos += 1;
                if is_line_ending(ch) {
                    break;
                }
            }
        }

        if is_delimiter(ch) {
            self.word.push(ch);
            match ch {
                b'/' => loop {
                    if self.pos >= len {
                        return false;
                    }
                    ch = self.buf[self.pos];
                    self.pos += 1;
                    if is_delimiter(ch) || is_whitespace(ch) {
                        self.pos -= 1;
                        return false;
                    }
                    if self.word.len() < MAX_WORD {
                        self.word.push(ch);
                    }
                },
                b'<' | b'>' => {
                    if self.pos >= len {
                        return false;
                    }
                    if self.buf[self.pos] == ch {
                        self.pos += 1;
                        self.word.push(ch);
                    }
                }
                _ => {}
            }
            return false;
        }

        let mut is_number = true;
        loop {
            if self.word.len() < MAX_WORD {
                self.word.push(ch);
            }
            if !is_numeric(ch) {
                is_number = false;
            }
            if self.pos >= len {
                return is_number;
            }
            ch = self.buf[self.pos];
            self.pos += 1;
            if is_delimiter(ch) || is_whitespace(ch) {
                self.pos -= 1;
                break;
            }
        }
        is_number
    }

    /// `ReadNextObject`。返回空表示这里读不出对象（`word` 里是读到的那个词）。
    pub fn read_next_object(
        &mut self,
        allow_nested_array: bool,
        in_array: bool,
        level: u32,
    ) -> Option<Obj> {
        // 无论如何先读一个词，保证每次调用都往前走。
        let is_number = self.next_word();
        if self.word.is_empty() || level > MAX_RECURSION {
            return None;
        }
        if is_number {
            return Some(Obj::Number(Number::parse(&self.word)));
        }
        let first = self.word[0];
        if first == b'<' && self.word.len() > 1 {
            let mut dict = Vec::new();
            loop {
                self.next_word();
                if self.word.len() == 2 && self.word[0] == b'>' {
                    break;
                }
                if self.word.is_empty() || self.word[0] != b'/' {
                    return None;
                }
                let key = name_decode(&self.word[1..]);
                let value = self.read_next_object(true, in_array, level + 1)?;
                dict_set(&mut dict, key, value);
            }
            return Some(Obj::Dict(dict));
        }
        if first == b'[' {
            if !allow_nested_array && in_array {
                return None;
            }
            let mut items = Vec::new();
            loop {
                match self.read_next_object(allow_nested_array, true, level + 1) {
                    Some(item) => items.push(item),
                    None => {
                        if self.word.is_empty() || self.word[0] == b']' {
                            break;
                        }
                    }
                }
            }
            return Some(Obj::Array(items));
        }
        self.read_leaf_object()
    }

    /// `ReadLeafObject`。
    fn read_leaf_object(&mut self) -> Option<Obj> {
        match self.word[0] {
            b'/' => Some(Obj::Name(name_decode(&self.word[1..]))),
            b'(' => Some(Obj::Str(self.read_string())),
            // 两个 `<` 的情况（字典）调用方已经处理了。
            b'<' => Some(Obj::Str(self.read_hex_string())),
            _ => match self.word.as_slice() {
                b"false" => Some(Obj::Bool(false)),
                b"true" => Some(Obj::Bool(true)),
                b"null" => Some(Obj::Null),
                _ => None,
            },
        }
    }

    /// `ReadString`：`(` 已经读过。
    fn read_string(&mut self) -> Vec<u8> {
        let mut out = read_string_at(self.buf, &mut self.pos);
        out.truncate(MAX_STRING);
        out
    }

    /// `ReadHexString`：`<` 已经读过。
    fn read_hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut first = true;
        let mut code = 0u8;
        while self.pos < self.buf.len() {
            let ch = self.buf[self.pos];
            self.pos += 1;
            if ch == b'>' {
                break;
            }
            if !ch.is_ascii_hexdigit() {
                continue;
            }
            let value = hex_char_to_int(ch);
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
        out.truncate(MAX_STRING);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elements(data: &[u8]) -> Vec<(Element, Vec<u8>, usize, usize)> {
        let mut p = StreamParser::new(data);
        let mut out = Vec::new();
        loop {
            let e = p.next_element();
            if e == Element::EndOfData {
                break;
            }
            out.push((e, p.word().to_vec(), p.start(), p.pos()));
        }
        out
    }

    #[test]
    fn words_are_classified_like_pdfium() {
        let got = elements(b"1 -2.5 +. -- /Name#20x q BT true null false falsey");
        let kinds: Vec<Element> = got.iter().map(|g| g.0).collect();
        assert_eq!(
            kinds,
            [
                Element::Number,
                Element::Number,
                Element::Number, // "+." 只由数字类字节组成
                Element::Number, // "--" 也是
                Element::Name,
                Element::Keyword,
                Element::Keyword,
                Element::Other,
                Element::Other,
                Element::Other,
                Element::Keyword,
            ]
        );
        assert_eq!(got[4].1, b"/Name#20x");
        assert_eq!(name_decode(&got[4].1[1..]), b"Name x");
    }

    #[test]
    fn extra_whitespace_bytes_and_comments() {
        // 0x80 和 0xFF 也是空白；注释到行尾。
        let got = elements(b"q\x80Q\xFF% comment q\nS%tail");
        let words: Vec<&[u8]> = got.iter().map(|g| g.1.as_slice()).collect();
        assert_eq!(words, [&b"q"[..], b"Q", b"S"]);
        // 元素的起止偏移不含前面的空白和注释。
        assert_eq!((got[2].2, got[2].3), (16, 17));
    }

    #[test]
    fn spans_cover_strings_arrays_and_dicts() {
        let data = b"(a(b)\\)c) <4 1> [1 (x) /N] <</K [1 2] /V (s)>> Tj";
        let got = elements(data);
        assert_eq!(got.len(), 5);
        let spans: Vec<&[u8]> = got.iter().map(|g| &data[g.2..g.3]).collect();
        assert_eq!(
            spans,
            [
                &b"(a(b)\\)c)"[..],
                b"<4 1>",
                b"[1 (x) /N]",
                b"<</K [1 2] /V (s)>>",
                b"Tj"
            ]
        );
        let mut p = StreamParser::new(data);
        assert_eq!(p.next_element(), Element::Other);
        assert_eq!(p.take_object(), Some(Obj::Str(b"a(b))c".to_vec())));
        assert_eq!(p.next_element(), Element::Other);
        assert_eq!(p.take_object(), Some(Obj::Str(vec![0x41])));
        assert_eq!(p.next_element(), Element::Other);
        let Some(Obj::Array(items)) = p.take_object() else {
            panic!("应当是数组")
        };
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn nested_array_at_top_level_is_flattened() {
        // 里面的 `[` 被跳过，里面的 `]` 结束外层数组；剩下的 `]` 是一个落单的分隔符。
        let data = b"[1 [2 3] 4] TJ";
        let mut p = StreamParser::new(data);
        assert_eq!(p.next_element(), Element::Other);
        let Some(Obj::Array(items)) = p.take_object() else {
            panic!("应当是数组")
        };
        assert_eq!(items.len(), 3);
        assert_eq!(p.next_element(), Element::Number);
        assert_eq!(p.word(), b"4");
        assert_eq!(p.next_element(), Element::Other);
        assert_eq!(p.take_object(), None);
        assert_eq!(p.next_element(), Element::Keyword);
    }

    #[test]
    fn dict_with_bad_key_is_abandoned() {
        let mut p = StreamParser::new(b"<< /A 1 2 /B 3 >> q");
        assert_eq!(p.next_element(), Element::Other);
        assert_eq!(p.take_object(), None);
        // 读到坏键为止；后面的内容接着按普通元素解析。
        assert_eq!(p.next_element(), Element::Name);
    }

    #[test]
    fn string_escapes_follow_the_state_machine() {
        let mut p = StreamParser::new(b"(\\101\\7x\\\r\ny\\n\\q) (un");
        assert_eq!(p.next_element(), Element::Other);
        assert_eq!(p.take_object(), Some(Obj::Str(b"A\x07xy\nq".to_vec())));
        // 没闭合的字符串读到数据末尾。
        assert_eq!(p.next_element(), Element::Other);
        assert_eq!(p.take_object(), Some(Obj::Str(b"un".to_vec())));
        assert_eq!(p.next_element(), Element::EndOfData);
    }

    #[test]
    fn numbers_follow_fx_number() {
        assert_eq!(Number::parse(b"12"), Number::Unsigned(12));
        assert_eq!(Number::parse(b"-12"), Number::Signed(-12));
        assert_eq!(Number::parse(b"+7"), Number::Signed(7));
        assert_eq!(Number::parse(b"1-2"), Number::Unsigned(1));
        assert_eq!(Number::parse(b"--"), Number::Signed(0));
        assert_eq!(Number::parse(b"99999999999"), Number::Unsigned(0));
        assert_eq!(Number::parse(b"-2147483648"), Number::Signed(i32::MIN));
        assert_eq!(Number::parse(b"-2147483649"), Number::Signed(0));
        assert_eq!(Number::parse(b".5").to_f32(), 0.5);
        assert_eq!(Number::parse(b"-.5").to_f32(), -0.5);
        assert_eq!(Number::parse(b"1.5.7").to_f32(), 1.5);
        assert_eq!(Number::parse(b".").to_f32(), 0.0);
        assert_eq!(Number::parse(b"4.").to_f32(), 4.0);
        assert_eq!(Number::Float(3.9).to_i32(), 3);
    }

    #[test]
    fn long_words_are_truncated_but_consumed() {
        let mut data = vec![b'a'; 300];
        data.extend_from_slice(b" q");
        let got = elements(&data);
        assert_eq!(got[0].1.len(), 255);
        assert_eq!((got[0].2, got[0].3), (0, 300));
        assert_eq!(got[1].1, b"q");
    }

    #[test]
    fn broken_input_terminates() {
        for data in [
            &b"<<"[..],
            b"[[[[",
            b"(((",
            b"<",
            b"<< /A",
            b"/",
            b"%",
            b"[ ] ] ] > ) }",
            b"<< /A << /B [ (x",
        ] {
            let got = elements(data);
            assert!(got.len() <= data.len(), "{data:?}");
        }
    }
}
