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

    /// PDFium 的 `CPDF_Object::GetInteger`：整数按 32 位取（超出的回绕），浮点数截断并饱和，
    /// 不是数字时得 0。移植 PDFium 的判断时用它，结果才逐位一致。
    pub fn get_integer(&self) -> i32 {
        match self {
            Self::Int(i) => *i as i32,
            Self::Real(f) => *f as i32,
            _ => 0,
        }
    }

    /// PDFium 的 `CPDF_Object::GetNumber`：不是数字时得 0。
    pub fn get_number(&self) -> f32 {
        match self {
            Self::Int(i) => *i as f32,
            Self::Real(f) => *f as f32,
            _ => 0.0,
        }
    }

    /// 是不是数字（`ToNumber` 不为空）。
    pub fn is_number(&self) -> bool {
        matches!(self, Self::Int(_) | Self::Real(_))
    }
}

/// 字典。键是解码之后的名字（不带 `/`）；保持文件里的先后顺序。
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

    /// 按名字查值。
    pub fn get(&self, key: &[u8]) -> Option<&PdfObject> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// 写入一个键：已有同名键则替换它的值（位置不变），否则追加到末尾。
    /// 解析时同名键后来的覆盖先前的，与 PDFium 的 `CPDF_Dictionary::SetFor` 一致。
    pub fn set(&mut self, key: &[u8], value: PdfObject) {
        match self.entries.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.entries.push((key.to_vec(), value)),
        }
    }

    /// 删除一个键。
    pub fn remove(&mut self, key: &[u8]) {
        self.entries.retain(|(k, _)| k != key);
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

/// 一条流（解码前）。`data` 是文件里的原始字节；解码用 [`crate::pdf::PdfDocument::decode_stream`]。
#[derive(Debug, Clone, PartialEq)]
pub struct PdfStreamRef {
    pub dict: PdfDict,
    /// 原始（未解码）数据。
    pub data: Vec<u8>,
    /// 数据的区间 `[start, end)`：普通对象是相对文件头的偏移，对象流里的对象没有意义。诊断用。
    pub span: (usize, usize),
}
