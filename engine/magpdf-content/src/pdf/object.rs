//! PDF 对象模型（解析结果）。

/// 一个 PDF 对象。
#[derive(Debug, Clone, PartialEq)]
pub enum PdfObject {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    /// 字符串的解码结果（字面量串处理了转义，十六进制串每两位一个字节）。
    Str(Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<PdfObject>),
    Dict(PdfDict),
    /// 间接引用 `num gen R`。
    Ref(u32, u16),
    Stream(PdfStreamRef),
}

impl PdfObject {
    pub fn as_dict(&self) -> Option<&PdfDict> {
        match self {
            Self::Dict(d) => Some(d),
            Self::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[PdfObject]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Self::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(*i as f64),
            Self::Real(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Real(f) => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_ref(&self) -> Option<(u32, u16)> {
        match self {
            Self::Ref(n, g) => Some((*n, *g)),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

/// 字典。键保存为原始字节（不解码 `#xx`，比较时统一走 [`PdfDict::get`]）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PdfDict {
    entries: Vec<(Vec<u8>, PdfObject)>,
}

impl PdfDict {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (Vec<u8>, PdfObject)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    /// 按解码后的名字查值。同名键保留第一次出现的（PDF 规范 7.3.7：重复键以第一个为准）。
    pub fn get(&self, key: &[u8]) -> Option<&PdfObject> {
        self.entries
            .iter()
            .find(|(k, _)| same_name(k, key))
            .map(|(_, v)| v)
    }

    /// 写入一个键：已有同名键（按解码后的名字比较）则替换原位置的键值，否则追加到末尾。
    pub fn set(&mut self, key: &[u8], value: PdfObject) {
        match self.entries.iter_mut().find(|(k, _)| same_name(k, key)) {
            Some(slot) => slot.1 = value,
            None => self.entries.push((key.to_vec(), value)),
        }
    }

    /// 删除一个键（按解码后的名字比较）。
    pub fn remove(&mut self, key: &[u8]) {
        self.entries.retain(|(k, _)| !same_name(k, key));
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &PdfObject)> {
        self.entries.iter().map(|(k, v)| (k.as_slice(), v))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 写回时用的键（保留原文写法，去掉前导 `/`）。
    pub fn raw_key(&self, key: &[u8]) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(k, _)| same_name(k, key))
            .map(|(k, _)| k.as_slice())
    }
}

/// 名字解码：`#xx` 转成对应字节；`/` 前缀不在此处理。
pub fn decode_name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            let h = hex_value(raw[i + 1]);
            let l = hex_value(raw[i + 2]);
            if let (Some(h), Some(l)) = (h, l) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

/// 名字编码：# 与不可打印、非 ASCII 字节写成 `#xx`。
pub fn encode_name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for &b in raw {
        if b <= b' ' || b >= 0x7f || b == b'#' || is_delimiter(b) {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        } else {
            out.push(b);
        }
    }
    out
}

pub(crate) fn same_name(raw: &[u8], decoded: &[u8]) -> bool {
    if raw == decoded {
        return true;
    }
    if !raw.contains(&b'#') {
        return false;
    }
    decode_name(raw) == decoded
}

pub(crate) fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

pub(crate) fn is_whitespace(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

pub(crate) fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 一条流（解码前）。`data` 是文件里的原始字节区间；解码按需调用 [`crate::pdf::apply_filter_chain`]。
#[derive(Debug, Clone, PartialEq)]
pub struct PdfStreamRef {
    pub dict: PdfDict,
    /// 原始（未解码）数据。
    pub data: Vec<u8>,
    /// 数据在文件中的区间 `[start, end)`（诊断用；文件被重建时可能无意义）。
    pub span: (usize, usize),
}

impl PdfStreamRef {
    /// 按 `/Filter`、`/DecodeParms` 解码。解不开时返回错误，调用方决定是否当作内容流损坏。
    pub fn decoded(&self, doc: &crate::pdf::PdfDocument) -> super::Result<Vec<u8>> {
        let filter = doc.resolve(self.dict.get(b"Filter"));
        let parms = doc.resolve(self.dict.get(b"DecodeParms"));
        Ok(super::filter::apply_filter_chain(
            &self.data,
            filter.as_ref(),
            parms.as_ref(),
        )?)
    }
}
