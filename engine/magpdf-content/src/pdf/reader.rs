//! 文档读取：交叉引用（表 / 流 / 混合 / 重建）、对象解析缓存。
//!
//! 只读。写回在 `writer` 里，只支持增量更新（追加新对象和新的 xref）。
//!
//! 这一层要保证“按对象号取到的对象就是 PDFium 取到的那个”，否则后面拿到的内容流不是 PDFium
//! 解释的那一条，操作符和对象的对应就全错了。所以打开文件的流程照 PDFium 的 `CPDF_Parser` 移植
//! （`core/fpdfapi/parser/cpdf_parser.cpp`，BSD-3-Clause，版权声明见本 crate 的 `PDFIUM-NOTICE.md`）：
//!
//! - 文件头：在前 1025 个偏移里找 `%PDF`，找不到就当作从 0 开始；之后所有偏移都相对文件头；
//! - `startxref`：只在文件末尾 4096 字节里找；
//! - 交叉引用链：表和流都认，表的条目固定 20 字节；最老的一节是表时核对第一个对象的对象号；
//! - 任何一步失败就重建：从头扫 `N G obj`，后出现的覆盖先出现的，`trailer` 字典和 `/Type /XRef`
//!   的流字典合并成新的 trailer，对象流里的对象登记成压缩对象；
//! - 打开之后根对象取不到、或者一页都数不出来，也重建一次；
//! - 按偏移解析对象时，偏移处写的对象号必须和要取的一致；对象流里的对象按序号取，
//!   序号处登记的对象号也必须一致。
//!
//! 没有照搬的部分：对象体的语法用本模块自己的 [`Lexer`]（个别畸形写法的容错与 PDFium 不完全相同），
//! 加密文件不解密（上层据 [`PdfDocument::encrypt`] 整份回退）。

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::filter::{decode_like_pdfium, decode_like_pdfium_checked};
use super::lexer::{Lexer, atoi64, atoui, direct_length};
use super::object::{PdfDict, PdfObject, PdfStreamRef};
use super::{Error, Result};
use crate::content::parser::{is_delimiter, is_whitespace};

/// `kPDFHeaderSize`。
const HEADER_SIZE: usize = 9;
/// `CPDF_Parser::kMaxObjectNumber`。
const MAX_OBJECT_NUMBER: u32 = 4 * 1024 * 1024;
/// `kMaxXRefSize`。
const MAX_XREF_SIZE: i64 = MAX_OBJECT_NUMBER as i64 + 1;

/// PDFium 的 `CPDF_CrossRefTable::AddNormal` 里有一条：已有条目是压缩对象（在对象流里）、
/// 新来的普通条目代号是 0 时，不动已有条目。后果是增量更新里把对象流中的对象改写成普通对象
/// 不生效。这条规则是凭对 PDFium 源码的记忆写的（手头的参考副本里没有那个文件），所以单独放一个
/// 开关，并用实验核对过（见报告“第二轮”一节）。
const KEEP_COMPRESSED_OVER_NORMAL_GEN0: bool = true;

/// 交叉引用损坏。
#[derive(Debug, thiserror::Error)]
pub enum XrefError {
    #[error("交叉引用读不出来，重建也没有找到对象或 trailer")]
    RebuildFailed,
    #[error("根对象取不到（重建之后仍然如此）")]
    NoRoot,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Free,
    /// 文件内的对象（相对文件头的字节偏移）。
    Normal(usize),
    /// 在对象流里（流对象号，序号）。
    Compressed(u32, u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    kind: Kind,
    generation: u16,
    /// 这个对象是对象流（有别的对象登记在它里面）。
    is_object_stream: bool,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            kind: Kind::Free,
            generation: 0,
            is_object_stream: false,
        }
    }
}

/// 交叉引用表（`CPDF_CrossRefTable`）。
#[derive(Debug, Clone, Default)]
struct Table {
    entries: BTreeMap<u32, Entry>,
    trailer: Option<PdfDict>,
}

impl Table {
    /// `AddNormal`。已有条目的代号更高时不动它；已有的是压缩对象、新来的代号是 0 时也不动它。
    fn add_normal(&mut self, number: u32, generation: u16, pos: usize) -> bool {
        if number >= MAX_OBJECT_NUMBER {
            return false;
        }
        let entry = self.entries.entry(number).or_default();
        if entry.generation > generation {
            return true;
        }
        if KEEP_COMPRESSED_OVER_NORMAL_GEN0
            && matches!(entry.kind, Kind::Compressed(..))
            && generation == 0
        {
            return true;
        }
        entry.kind = Kind::Normal(pos);
        entry.generation = generation;
        true
    }

    /// `AddCompressed`。
    fn add_compressed(&mut self, number: u32, archive: u32, index: u32) {
        if number >= MAX_OBJECT_NUMBER || archive >= MAX_OBJECT_NUMBER {
            return;
        }
        let entry = self.entries.entry(number).or_default();
        if entry.generation > 0 || entry.is_object_stream {
            return;
        }
        entry.kind = Kind::Compressed(archive, index);
        entry.generation = 0;
        self.entries.entry(archive).or_default().is_object_stream = true;
    }

    /// `SetFree`。
    fn set_free(&mut self, number: u32, generation: u16) {
        if number >= MAX_OBJECT_NUMBER {
            return;
        }
        let entry = self.entries.entry(number).or_default();
        entry.kind = Kind::Free;
        entry.generation = generation;
    }

    /// `SetObjectMapSize`：保证 `size - 1` 这个对象号有条目（只影响“最大对象号”）。
    fn ensure_size(&mut self, size: u32) {
        if size > 0 {
            self.entries.entry(size - 1).or_default();
        }
    }

    fn last_object_number(&self) -> u32 {
        self.entries.keys().next_back().copied().unwrap_or(0)
    }

    /// 新的 trailer 盖在现有的上面（同名键取新的）。
    fn merge_trailer_over(&mut self, newer: PdfDict) {
        match &mut self.trailer {
            None => self.trailer = Some(newer),
            Some(current) => {
                for (key, value) in newer.iter() {
                    if key != b"Prev" && key != b"XRefStm" {
                        current.set(key, value.clone());
                    }
                }
            }
        }
    }

    /// 更老的 trailer 垫在现有的下面（同名键保留现有的）。
    fn merge_trailer_under(&mut self, older: PdfDict) {
        match &mut self.trailer {
            None => self.trailer = Some(older),
            Some(current) => {
                for (key, value) in older.iter() {
                    if current.get(key).is_none() {
                        current.set(key, value.clone());
                    }
                }
            }
        }
    }
}

/// 对象流（`CPDF_ObjectStream`）。
#[derive(Debug)]
struct ObjectStream {
    data: Vec<u8>,
    first: usize,
    /// 流头里登记的（对象号，偏移）。对象号是 0 的那些项不占位置。
    pairs: Vec<(u32, u32)>,
}

impl ObjectStream {
    /// `CPDF_ObjectStream::Create`：得是 `/Type /ObjStm` 的流，`/N`、`/First` 是直接写的非负整数。
    fn create(
        stream: &PdfStreamRef,
        resolve: &dyn Fn(&PdfObject) -> Option<PdfObject>,
    ) -> Option<Self> {
        let dict = &stream.dict;
        if dict.get(b"Type").and_then(PdfObject::as_name) != Some(b"ObjStm") {
            return None;
        }
        let Some(count @ PdfObject::Int(_)) = dict.get(b"N") else {
            return None;
        };
        let count = count.get_integer();
        if count < 0 || count >= MAX_OBJECT_NUMBER as i32 {
            return None;
        }
        let Some(first @ PdfObject::Int(_)) = dict.get(b"First") else {
            return None;
        };
        let first = usize::try_from(first.get_integer()).ok()?;
        let data = decode_like_pdfium(
            &stream.data,
            dict.get(b"Filter").and_then(resolve).as_ref(),
            dict.get(b"DecodeParms").and_then(resolve).as_ref(),
            resolve,
        );
        let mut lx = Lexer::new(&data);
        let mut pairs = Vec::new();
        for _ in 0..count {
            if lx.pos >= data.len() {
                break;
            }
            let number = lx.direct_num();
            let offset = lx.direct_num();
            if number == 0 {
                continue;
            }
            pairs.push((number, offset));
        }
        Some(Self { data, first, pairs })
    }

    /// `ParseObject`：序号处登记的对象号必须是要取的那个。
    fn parse(&self, number: u32, index: u32) -> Option<PdfObject> {
        let (registered, offset) = *self.pairs.get(index as usize)?;
        if registered != number {
            return None;
        }
        let at = self.first.checked_add(offset as usize)?;
        if at >= self.data.len() || at > i32::MAX as usize {
            return None;
        }
        Lexer::at(&self.data, at).read_body(false, 0)
    }
}

/// 读交叉引用的过程（`CPDF_Parser` 里与交叉引用有关的部分）。
struct Loader<'a> {
    data: &'a [u8],
    table: Table,
    xref_stream: bool,
}

impl Loader<'_> {
    /// 按已经读到的交叉引用取一个文件内的普通对象（读交叉引用流时解 `/Length` 等引用用）。
    fn object(&self, number: u32) -> Option<PdfObject> {
        match self.table.entries.get(&number)?.kind {
            Kind::Normal(pos) if pos > 0 => {
                let parsed = Lexer::at(self.data, pos).read_indirect(false, &direct_length)?;
                (parsed.number == number).then_some(parsed.object).flatten()
            }
            _ => None,
        }
    }

    /// 解一层引用。
    fn resolve(&self, value: &PdfObject) -> Option<PdfObject> {
        match value {
            PdfObject::Ref(number, _) => self.object(*number),
            other => Some(other.clone()),
        }
    }

    fn length_of(&self, value: &PdfObject) -> Option<i64> {
        self.resolve(value).as_ref().and_then(direct_length)
    }

    /// `LoadAllCrossRefTablesAndStreams`。
    fn load_all(&mut self, xref_offset: usize) -> bool {
        let mut pos = xref_offset;
        let is_stream = !self.load_table(&mut pos, true);
        if is_stream {
            let mut prev = xref_offset as i64;
            if !self.load_stream(&mut prev, true) {
                return false;
            }
        } else {
            let Some(trailer) = self.load_trailer(pos) else {
                return false;
            };
            self.table.trailer = Some(trailer);
            let size = self.trailer_direct_int(b"Size");
            if size > 0 && i64::from(size) <= MAX_XREF_SIZE {
                self.table.ensure_size(size as u32);
            }
        }

        // 从老到新排列；流的位置和表的位置分开记，0 表示这一节没有。
        let mut tables: Vec<i64> = Vec::new();
        let mut streams: Vec<i64> = Vec::new();
        if is_stream {
            tables.push(0);
            streams.push(xref_offset as i64);
        } else {
            tables.push(xref_offset as i64);
            streams.push(i64::from(self.trailer_direct_int(b"XRefStm")));
        }
        if !self.find_all(xref_offset as i64, &mut tables, &mut streams) {
            return false;
        }

        if tables[0] > 0 {
            let mut pos = tables[0] as usize;
            if !self.load_table(&mut pos, false) || !self.verify() {
                return false;
            }
        }
        // 表的条目优先于流的条目：每一节先读流、再读表。最老那一节的 XRefStm 不读。
        for i in 1..tables.len() {
            if streams[i] > 0 {
                let mut prev = streams[i];
                if !self.load_stream(&mut prev, false) {
                    return false;
                }
            }
            if tables[i] > 0 {
                let mut pos = tables[i] as usize;
                if !self.load_table(&mut pos, false) {
                    return false;
                }
            }
        }
        if is_stream {
            self.xref_stream = true;
        }
        true
    }

    /// `GetDirectIntegerFor`：只认直接写的数字。
    fn trailer_direct_int(&self, key: &[u8]) -> i32 {
        self.table
            .trailer
            .as_ref()
            .and_then(|t| t.get(key))
            .filter(|v| v.is_number())
            .map_or(0, PdfObject::get_integer)
    }

    /// `FindAllCrossReferenceTablesAndStream`：沿 `/Prev` 往回找。
    fn find_all(&mut self, main: i64, tables: &mut Vec<i64>, streams: &mut Vec<i64>) -> bool {
        let mut seen = BTreeSet::from([main]);
        let mut offset = i64::from(self.trailer_direct_int(b"Prev"));
        while offset > 0 {
            if !seen.insert(offset) {
                return false;
            }
            let mut prev = offset;
            if self.load_stream(&mut prev, false) {
                tables.insert(0, 0);
                streams.insert(0, offset);
                offset = prev;
            } else {
                let mut pos = offset as usize;
                let _ = self.load_table(&mut pos, true);
                let Some(trailer) = self.load_trailer(pos) else {
                    return false;
                };
                tables.insert(0, offset);
                streams.insert(
                    0,
                    trailer
                        .get(b"XRefStm")
                        .and_then(|v| self.resolve(v))
                        .map_or(0, |v| i64::from(v.get_integer())),
                );
                offset = trailer
                    .get(b"Prev")
                    .filter(|v| v.is_number())
                    .map_or(0, |v| i64::from(v.get_integer()));
                self.table.merge_trailer_under(trailer);
            }
        }
        true
    }

    /// `LoadTrailer`：`pos` 处应当是 `trailer` 加一个字典。
    fn load_trailer(&self, pos: usize) -> Option<PdfDict> {
        let mut lx = Lexer::at(self.data, pos);
        if lx.next_word().0 != b"trailer" {
            return None;
        }
        match lx.read_body(false, 0)? {
            PdfObject::Dict(dict) => Some(dict),
            _ => None,
        }
    }

    /// `LoadCrossRefTable`。`skip` 为真时只走过去、不登记条目。无论成败，`pos` 都改成读到的位置
    /// （紧接着读 trailer 要从这里开始）。
    fn load_table(&mut self, pos: &mut usize, skip: bool) -> bool {
        let mut lx = Lexer::at(self.data, *pos);
        let objects = Self::parse_table(&mut lx, skip);
        *pos = lx.pos;
        let Some(objects) = objects else {
            return false;
        };
        // `MergeCrossRefObjectsData`：表里的空闲条目代号是 0，不登记。
        for (number, generation, offset) in objects {
            if !self.table.add_normal(number, generation, offset) {
                return false;
            }
        }
        true
    }

    /// `ParseCrossRefTable`：返回在用的条目（对象号，代号，偏移）。
    fn parse_table(lx: &mut Lexer<'_>, skip: bool) -> Option<Vec<(u32, u16, usize)>> {
        const ENTRY: usize = 20;
        let data = lx.data;
        if lx.next_word().0 != b"xref" {
            return None;
        }
        let mut objects = Vec::new();
        let mut total = 0usize;
        loop {
            let saved = lx.pos;
            let (word, is_number) = lx.next_word();
            if word.is_empty() {
                return None;
            }
            if !is_number {
                lx.pos = saved;
                break;
            }
            let start = atoui(word);
            if start > MAX_OBJECT_NUMBER {
                return None;
            }
            let count = lx.direct_num() as usize;
            lx.skip_to_word();
            if count == 0 {
                continue;
            }
            let bytes = count.checked_mul(ENTRY)?;
            if skip {
                let next = lx
                    .pos
                    .checked_add(bytes)
                    .filter(|n| *n <= i32::MAX as usize)?;
                lx.pos = next.min(data.len());
                continue;
            }
            total = total.checked_add(count)?;
            if total as i64 > MAX_XREF_SIZE || total > data.len() / ENTRY {
                return None;
            }
            let block = data.get(lx.pos..lx.pos.checked_add(bytes)?)?;
            for (i, entry) in block.as_chunks::<ENTRY>().0.iter().enumerate() {
                if entry[17] == b'f' {
                    continue;
                }
                let offset = atoi64(&entry[..]);
                if i32::try_from(offset).is_err() {
                    return None;
                }
                if offset == 0 && !entry[..10].iter().all(u8::is_ascii_digit) {
                    return None;
                }
                let generation = atoi64(&entry[11..]) as u16;
                // 偏移不是正数的条目登记了也取不到对象。
                let offset = usize::try_from(offset).unwrap_or(0);
                objects.push((start.saturating_add(i as u32), generation, offset));
            }
            lx.pos += bytes;
        }
        Some(objects)
    }

    /// `VerifyCrossRefTable`：第一个位置大于 0 的条目，那个位置上写的对象号得是它自己。
    ///
    /// PDFium 的条目里“偏移”和“所在对象流的对象号”共用一块存储，压缩对象在这里被当成偏移是
    /// 对象流对象号的普通条目；照做。
    fn verify(&self) -> bool {
        for (number, entry) in &self.table.entries {
            let pos = match entry.kind {
                Kind::Normal(pos) => pos,
                Kind::Compressed(archive, _) => archive as usize,
                Kind::Free => 0,
            };
            if pos == 0 {
                continue;
            }
            let (word, is_number) = Lexer::at(self.data, pos).next_word();
            return is_number && !word.is_empty() && atoui(word) == *number;
        }
        true
    }

    /// `LoadCrossRefStream`。成功时把 `pos` 改成这条流的 `/Prev`。
    fn load_stream(&mut self, pos: &mut i64, is_main: bool) -> bool {
        let Ok(at) = usize::try_from(*pos) else {
            return false;
        };
        let Some(parsed) = Lexer::at(self.data, at).read_indirect(false, &|v| self.length_of(v))
        else {
            return false;
        };
        let Some(PdfObject::Stream(stream)) = parsed.object else {
            return false;
        };
        if parsed.number == 0 {
            return false;
        }
        let direct = |key: &[u8]| stream.dict.get(key).and_then(|v| self.resolve(v));
        let int = |key: &[u8]| direct(key).map_or(0, |v| v.get_integer());
        let prev = int(b"Prev");
        if prev < 0 {
            return false;
        }
        let size = int(b"Size");
        if size < 0 || i64::from(size) > MAX_XREF_SIZE {
            return false;
        }
        *pos = i64::from(prev);

        let mut index: Vec<(u32, u32)> = Vec::new();
        if let Some(PdfObject::Array(items)) = direct(b"Index") {
            for pair in items.as_chunks::<2>().0 {
                let (Some(start), Some(count)) = (self.resolve(&pair[0]), self.resolve(&pair[1]))
                else {
                    continue;
                };
                if !start.is_number() || !count.is_number() {
                    continue;
                }
                let (start, count) = (start.get_integer(), count.get_integer());
                if start < 0 || count <= 0 {
                    continue;
                }
                index.push((start as u32, count as u32));
            }
        }
        if index.is_empty() {
            index.push((0, size as u32));
        }
        let widths: Vec<u32> = match direct(b"W") {
            Some(PdfObject::Array(items)) => items
                .iter()
                .map(|v| self.resolve(v).map_or(0, |v| v.get_integer()) as u32)
                .collect(),
            _ => Vec::new(),
        };
        let data = decode_like_pdfium(
            &stream.data,
            direct(b"Filter").as_ref(),
            direct(b"DecodeParms").as_ref(),
            &|v| self.resolve(v),
        );

        if is_main {
            self.table = Table {
                entries: BTreeMap::new(),
                trailer: Some(stream.dict.clone()),
            };
            self.table.ensure_size(size as u32);
        } else {
            self.table.merge_trailer_under(stream.dict.clone());
        }

        if widths.len() < 3 {
            return false;
        }
        let Some(total_width) = widths.iter().try_fold(0u32, |acc, w| acc.checked_add(*w)) else {
            return false;
        };
        let total_width = total_width as usize;
        // 第 `which` 栏的值（`GetVarInt`：大端，超过 32 位的高位丢掉）。
        let field = |entry: &[u8], which: usize| -> u32 {
            let start: usize = widths[..which].iter().map(|w| *w as usize).sum();
            entry[start..start + widths[which] as usize]
                .iter()
                .fold(0u32, |acc, byte| {
                    acc.wrapping_mul(256).wrapping_add(u32::from(*byte))
                })
        };

        let mut seg_index = 0usize;
        for (start, count) in index {
            let count_usize = count as usize;
            let Some(seg_end) = seg_index
                .checked_add(count_usize)
                .and_then(|n| n.checked_mul(total_width))
                .filter(|n| *n <= u32::MAX as usize)
            else {
                continue;
            };
            if seg_end > data.len() {
                continue;
            }
            let Some(new_size) = start.checked_add(count) else {
                continue;
            };
            let current_size = if self.table.entries.is_empty() {
                0
            } else {
                self.table.last_object_number() + 1
            };
            let new_size = new_size.min(MAX_XREF_SIZE as u32);
            if new_size > current_size {
                self.table.ensure_size(new_size);
            }
            let segment = &data[seg_index * total_width..seg_end];
            for i in 0..count_usize {
                let number = start + i as u32;
                if number > MAX_OBJECT_NUMBER {
                    break;
                }
                let entry = &segment[i * total_width..(i + 1) * total_width];
                // 第一栏宽度是 0 时，类型按 1（普通对象）算。
                let kind = if widths[0] == 0 { 1 } else { field(entry, 0) };
                match kind {
                    0 => {
                        if let Ok(generation) = u16::try_from(field(entry, 2)) {
                            self.table.set_free(number, generation);
                        }
                    }
                    1 => {
                        let offset = field(entry, 1);
                        if let (Ok(offset), Ok(generation)) =
                            (i32::try_from(offset), u16::try_from(field(entry, 2)))
                        {
                            self.table.add_normal(number, generation, offset as usize);
                        }
                    }
                    2 => {
                        let archive = field(entry, 1);
                        if archive <= self.table.last_object_number() {
                            self.table.add_compressed(number, archive, field(entry, 2));
                        }
                    }
                    _ => {}
                }
            }
            seg_index += count_usize;
        }
        true
    }

    /// `RebuildCrossRef`：从头扫一遍文件。结果盖在已经读到的条目上面。
    fn rebuild(&mut self) -> bool {
        let data = self.data;
        let mut rebuilt = Table::default();
        let mut lx = Lexer::new(data);
        // 最近读到的两个数字和它们的位置。
        let mut numbers: Vec<(u32, usize)> = Vec::with_capacity(3);
        // 重建时没有对象表可查，引用一律解不了。
        let no_refs = |v: &PdfObject| match v {
            PdfObject::Ref(..) => None,
            other => Some(other.clone()),
        };
        loop {
            let (word, is_number) = lx.next_word();
            if word.is_empty() {
                break;
            }
            if is_number {
                numbers.push((atoui(word), lx.pos - word.len()));
                if numbers.len() > 2 {
                    numbers.remove(0);
                }
                continue;
            }
            match word {
                b"(" => {
                    lx.read_literal_string();
                }
                b"<" => {
                    lx.read_hex_string();
                }
                b"trailer" => {
                    if let Some(object) = lx.read_body(false, 0)
                        && let Some(dict) = object.as_dict()
                    {
                        rebuilt.merge_trailer_over(dict.clone());
                    }
                }
                b"obj" if numbers.len() == 2 => {
                    let (number, pos) = numbers[0];
                    let generation = numbers[1].0;
                    let mut at = Lexer::at(data, pos);
                    let mut stream = None;
                    if let Some(parsed) = at.read_indirect(true, &direct_length) {
                        // 接着从对象之后扫（对象体读不出来时，从读到的地方接着扫）。
                        lx.pos = parsed.end.max(lx.pos);
                        if let Some(PdfObject::Stream(s)) = parsed.object {
                            stream = Some(s);
                        }
                    }
                    if let Some(s) = &stream
                        && s.dict.get(b"Type").and_then(PdfObject::as_name) == Some(b"XRef")
                    {
                        rebuilt.merge_trailer_over(s.dict.clone());
                    }
                    if rebuilt.add_normal(number, generation as u16, pos)
                        && let Some(s) = &stream
                        && let Some(object_stream) = ObjectStream::create(s, &no_refs)
                    {
                        for (i, (inner, _)) in object_stream.pairs.iter().enumerate() {
                            rebuilt.add_compressed(*inner, number, i as u32);
                        }
                    }
                }
                _ => {}
            }
            numbers.clear();
        }

        // `MergeUp(原有, 重建)`：重建出来的条目盖在原有的上面，原有的“是对象流”标记保留。
        for (number, mut entry) in rebuilt.entries {
            if let Some(old) = self.table.entries.get(&number)
                && matches!(entry.kind, Kind::Normal(_))
                && old.is_object_stream
            {
                entry.is_object_stream = true;
            }
            self.table.entries.insert(number, entry);
        }
        if let Some(trailer) = rebuilt.trailer {
            self.table.merge_trailer_over(trailer);
        }
        self.table.trailer.is_some() && !self.table.entries.is_empty()
    }
}

/// `GetHeaderOffset`：前 1025 个偏移里找 `%PDF`；读不到 4 个字节时失败，找不到时按 0 算。
fn find_header_offset(raw: &[u8]) -> Option<usize> {
    for offset in 0..=1024usize {
        let window = raw.get(offset..offset + 4)?;
        if window == b"%PDF" {
            return Some(offset);
        }
    }
    Some(0)
}

/// `ParseStartXRef`：从 `长度 - 9` 往回、最多看 4096 个位置，找独立成词的 `startxref`，
/// 取它后面的数字。找不到或者数字无效时得 0。
fn parse_startxref(data: &[u8]) -> usize {
    const WORD: &[u8] = b"startxref";
    const LIMIT: usize = 4096;
    let len = data.len();
    if len < 2 * WORD.len() - 1 {
        return 0;
    }
    // 词的最后一个字节最远在 `len - 9`；被看到的位置都大于 `len - 9 - 4096`。
    let last_start = len - 2 * WORD.len() + 1;
    let first_start = (len - WORD.len() + 1).saturating_sub(LIMIT);
    let regular = |ch: u8| !is_whitespace(ch) && !is_delimiter(ch);
    let mut start = last_start + 1;
    while start > first_start {
        start -= 1;
        if &data[start..start + WORD.len()] != WORD {
            continue;
        }
        // `IsWholeWord(pos, limit, …)`：右边只在词尾不超过 4096 时才检查（PDFium 把“回看多远”
        // 当成了文件长度传进去），左边总是检查。
        let end = start + WORD.len();
        if end <= LIMIT && data.get(end).is_some_and(|ch| regular(*ch)) {
            continue;
        }
        if start > 0 && regular(data[start - 1]) {
            continue;
        }
        let (word, is_number) = Lexer::at(data, end).next_word();
        if !is_number || word.is_empty() {
            return 0;
        }
        let value = atoi64(word);
        if value > i64::from(i32::MAX) || value >= len as i64 {
            return 0;
        }
        return usize::try_from(value).unwrap_or(0);
    }
    0
}

/// 一个在用的对象在哪里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryLocation {
    /// 文件里，相对文件头的偏移。
    Offset(usize),
    /// 对象流里。
    InStream { stream: u32, index: u32 },
}

/// 一个已打开的 PDF。持有整个文件的字节。
pub struct PdfDocument {
    raw: Vec<u8>,
    /// `%PDF` 在文件里的位置；交叉引用里的偏移都相对它。
    header_offset: usize,
    entries: BTreeMap<u32, Entry>,
    trailer: PdfDict,
    cache: RefCell<BTreeMap<u32, PdfObject>>,
    /// 正在解析的对象号（防止循环引用）。
    parsing: RefCell<BTreeSet<u32>>,
    object_streams: RefCell<BTreeMap<u32, Option<Rc<ObjectStream>>>>,
    /// 最新的一节交叉引用是不是流（增量更新时新的一节跟着用流）。
    pub uses_xref_stream: bool,
    /// 交叉引用是重建出来的（原文件的读不了）。增量更新时要写一份完整的交叉引用。
    pub rebuilt: bool,
    /// trailer 里 `/Encrypt` 的原始写法（可能是引用）。有它就说明文件加了密，本层不解密。
    pub encrypt: Option<PdfObject>,
    /// 最新一节交叉引用相对文件头的偏移（增量更新时写 `/Prev`）；重建的文档是 0。
    pub startxref: usize,
}

impl std::fmt::Debug for PdfDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfDocument")
            .field("bytes", &self.raw.len())
            .field("objects", &self.entries.len())
            .field("rebuilt", &self.rebuilt)
            .finish_non_exhaustive()
    }
}

impl PdfDocument {
    /// 从原始字节打开（`CPDF_Parser::StartParseInternal`）。
    pub fn load(raw: Vec<u8>) -> Result<Self> {
        let header_offset = find_header_offset(&raw).ok_or(Error::NotPdf)?;
        if raw.len() < header_offset + HEADER_SIZE {
            return Err(Error::NotPdf);
        }
        let (table, xref_stream, mut rebuilt, startxref) = {
            let data = &raw[header_offset..];
            let last_xref = parse_startxref(data);
            let mut loader = Loader {
                data,
                table: Table::default(),
                xref_stream: false,
            };
            let mut rebuilt = false;
            if last_xref < HEADER_SIZE || !loader.load_all(last_xref) {
                loader.xref_stream = false;
                if !loader.rebuild() {
                    return Err(XrefError::RebuildFailed.into());
                }
                rebuilt = true;
            }
            (loader.table, loader.xref_stream, rebuilt, last_xref)
        };
        let mut doc = Self::from_table(raw, header_offset, table.clone(), xref_stream);
        // 根对象取不到，或者一页都数不出来：重建一次再看。
        if !doc.root_usable() {
            if rebuilt {
                return Err(XrefError::NoRoot.into());
            }
            let raw = std::mem::take(&mut doc.raw);
            let table = {
                let mut loader = Loader {
                    data: &raw[header_offset..],
                    table,
                    xref_stream: false,
                };
                if !loader.rebuild() {
                    return Err(XrefError::RebuildFailed.into());
                }
                loader.table
            };
            doc = Self::from_table(raw, header_offset, table, false);
            rebuilt = true;
            if doc.root().is_none() {
                return Err(XrefError::NoRoot.into());
            }
        }
        doc.rebuilt = rebuilt;
        doc.startxref = if rebuilt { 0 } else { startxref };
        doc.encrypt = doc.trailer.get(b"Encrypt").cloned();
        Ok(doc)
    }

    fn from_table(raw: Vec<u8>, header_offset: usize, table: Table, xref_stream: bool) -> Self {
        Self {
            raw,
            header_offset,
            entries: table.entries,
            trailer: table.trailer.unwrap_or_default(),
            cache: RefCell::new(BTreeMap::new()),
            parsing: RefCell::new(BTreeSet::new()),
            object_streams: RefCell::new(BTreeMap::new()),
            uses_xref_stream: xref_stream,
            rebuilt: false,
            encrypt: None,
            startxref: 0,
        }
    }

    /// 根对象（`/Root` 必须是引用，指向字典或流）。
    pub fn root(&self) -> Option<PdfDict> {
        let (number, _) = self.trailer.get(b"Root")?.as_ref()?;
        self.object(number)?.as_dict().cloned()
    }

    /// `GetRoot() && TryInit()`：根对象在，而且至少数得出一页。
    fn root_usable(&self) -> bool {
        self.root().is_some() && super::pages::page_count(self) > 0
    }

    /// 整个文件的字节（含文件头之前的部分）。
    pub fn bytes(&self) -> &[u8] {
        &self.raw
    }

    /// 文件头之后的部分；交叉引用里的偏移相对它。
    fn data(&self) -> &[u8] {
        &self.raw[self.header_offset..]
    }

    /// `%PDF` 在文件里的位置。
    pub fn header_offset(&self) -> usize {
        self.header_offset
    }

    pub fn trailer_dict(&self) -> &PdfDict {
        &self.trailer
    }

    /// 交叉引用里登记过的对象号个数。
    pub fn object_count(&self) -> usize {
        self.entries.len()
    }

    /// 登记过的最大对象号。
    pub fn max_object_number(&self) -> u32 {
        self.entries.keys().next_back().copied().unwrap_or(0)
    }

    /// 对象相对文件头的字节偏移（只有文件内的普通对象有）。
    pub fn object_offset(&self, number: u32) -> Option<usize> {
        match self.entries.get(&number)?.kind {
            Kind::Normal(pos) if pos > 0 => Some(pos),
            _ => None,
        }
    }

    /// 对象是不是放在对象流里。
    pub fn is_compressed(&self, number: u32) -> bool {
        matches!(
            self.entries.get(&number).map(|e| e.kind),
            Some(Kind::Compressed(..))
        )
    }

    /// 增量更新里把这个对象重写成普通对象（代号不变），PDFium 认不认。
    /// 对象流里的对象不认（见 [`KEEP_COMPRESSED_OVER_NORMAL_GEN0`]）。
    pub fn can_replace_in_place(&self, number: u32) -> bool {
        match self.entries.get(&number).map(|e| e.kind) {
            Some(Kind::Normal(pos)) => pos > 0,
            Some(Kind::Compressed(..)) => !KEEP_COMPRESSED_OVER_NORMAL_GEN0,
            _ => false,
        }
    }

    /// 全部在用的条目：`(对象号, 代号, 位置)`。重建过的文档写完整交叉引用时用。
    pub fn live_entries(&self) -> Vec<(u32, u16, EntryLocation)> {
        self.entries
            .iter()
            .filter_map(|(number, entry)| match entry.kind {
                Kind::Normal(pos) if pos > 0 => {
                    Some((*number, entry.generation, EntryLocation::Offset(pos)))
                }
                Kind::Compressed(stream, index) => {
                    Some((*number, 0, EntryLocation::InStream { stream, index }))
                }
                _ => None,
            })
            .collect()
    }

    /// 解析对象号对应的对象（带缓存）。取不到时返回 `None`。
    pub fn object(&self, number: u32) -> Option<PdfObject> {
        if number == 0 {
            return None;
        }
        if let Some(cached) = self.cache.borrow().get(&number) {
            return Some(cached.clone());
        }
        let entry = *self.entries.get(&number)?;
        // 正在解析这个对象时又绕回来：循环引用，当作没有。
        if !self.parsing.borrow_mut().insert(number) {
            return None;
        }
        let object = match entry.kind {
            Kind::Normal(pos) if pos > 0 => self.parse_at(pos, number),
            Kind::Compressed(archive, index) => self
                .object_stream(archive)
                .and_then(|s| s.parse(number, index)),
            _ => None,
        };
        self.parsing.borrow_mut().remove(&number);
        let object = object?;
        self.cache.borrow_mut().insert(number, object.clone());
        Some(object)
    }

    /// `ParseIndirectObjectAt`：偏移处写的对象号必须是要取的那个。
    fn parse_at(&self, pos: usize, number: u32) -> Option<PdfObject> {
        let parsed = Lexer::at(self.data(), pos).read_indirect(false, &|v| match v {
            PdfObject::Ref(n, _) => self.object(*n).as_ref().and_then(direct_length),
            other => direct_length(other),
        })?;
        (parsed.number == number).then_some(parsed.object).flatten()
    }

    /// `GetObjectStream`。
    fn object_stream(&self, number: u32) -> Option<Rc<ObjectStream>> {
        if self.parsing.borrow().contains(&number) {
            return None;
        }
        if let Some(cached) = self.object_streams.borrow().get(&number) {
            return cached.clone();
        }
        let entry = *self.entries.get(&number)?;
        let Kind::Normal(pos) = entry.kind else {
            return None;
        };
        if !entry.is_object_stream || pos == 0 {
            return None;
        }
        self.parsing.borrow_mut().insert(number);
        let stream = match self.parse_at(pos, number) {
            Some(PdfObject::Stream(s)) => {
                ObjectStream::create(&s, &|v| self.resolve(Some(v))).map(Rc::new)
            }
            _ => None,
        };
        self.parsing.borrow_mut().remove(&number);
        self.object_streams
            .borrow_mut()
            .insert(number, stream.clone());
        stream
    }

    /// 对象号是否在交叉引用里（且不是空闲）。
    pub fn has_object(&self, number: u32) -> bool {
        self.entries
            .get(&number)
            .is_some_and(|e| e.kind != Kind::Free)
    }

    /// 对象号的代号。
    pub fn generation(&self, number: u32) -> u16 {
        self.entries.get(&number).map_or(0, |e| e.generation)
    }

    /// 解析一层间接引用。非引用原样返回。
    pub fn resolve(&self, obj: Option<&PdfObject>) -> Option<PdfObject> {
        let obj = obj?;
        match obj {
            PdfObject::Ref(n, _) => self.object(*n),
            other => Some(other.clone()),
        }
    }

    /// 同 [`Self::resolve`]，但值是 null 时也返回 `None`。
    pub fn resolve_or_none(&self, obj: Option<&PdfObject>) -> Option<PdfObject> {
        self.resolve(obj).filter(|o| !o.is_null())
    }

    /// 解析一层引用，值为 null 或解析不到都返回 `None`；参数不是 `Option` 时的便捷版本。
    pub fn deref(&self, obj: &PdfObject) -> Option<PdfObject> {
        self.resolve_or_none(Some(obj))
    }

    /// 照 PDFium 的容错规则解码一条流（见 [`decode_like_pdfium`]）。
    pub fn decode_stream(&self, stream: &PdfStreamRef) -> Vec<u8> {
        self.decode_stream_checked(stream).0
    }

    /// 同 [`Self::decode_stream`]，另外说明解码是不是完整的（见
    /// [`crate::pdf::decode_like_pdfium_checked`]）。
    pub fn decode_stream_checked(&self, stream: &PdfStreamRef) -> (Vec<u8>, bool) {
        decode_like_pdfium_checked(
            &stream.data,
            self.resolve(stream.dict.get(b"Filter")).as_ref(),
            self.resolve(stream.dict.get(b"DecodeParms")).as_ref(),
            &|v| self.resolve(Some(v)),
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 拼一份最小的 PDF：对象按给定顺序写，交叉引用表的偏移是真实的。
    pub(crate) fn build(objects: &[(u32, &str)], trailer_extra: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        for (number, body) in objects {
            offsets.insert(*number, out.len());
            out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let size = offsets.keys().next_back().copied().unwrap_or(0) + 1;
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        for n in 0..size {
            match offsets.get(&n) {
                Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    pub(crate) const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
    pub(crate) const PAGES: &str = "<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
    pub(crate) const PAGE: &str =
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>";

    fn simple() -> Vec<u8> {
        build(
            &[
                (1, CATALOG),
                (2, PAGES),
                (3, PAGE),
                (4, "<< /Length 4 >>\nstream\nq Q \nendstream"),
            ],
            "",
        )
    }

    #[test]
    fn loads_a_classic_table() {
        let doc = PdfDocument::load(simple()).expect("能打开");
        assert!(!doc.rebuilt && !doc.uses_xref_stream);
        assert!(doc.startxref > 0);
        let Some(PdfObject::Stream(s)) = doc.object(4) else {
            panic!("对象 4 应当是流")
        };
        assert_eq!(s.data, b"q Q ");
        assert!(doc.object(9).is_none());
        assert_eq!(doc.max_object_number(), 4);
        assert!(doc.can_replace_in_place(4));
    }

    #[test]
    fn header_offset_shifts_every_position() {
        // 文件头前面有垃圾：偏移都相对 `%PDF`。
        let mut data = b"garbage before the header\n".to_vec();
        data.extend_from_slice(&simple());
        let doc = PdfDocument::load(data).expect("能打开");
        assert_eq!(doc.header_offset(), 26);
        assert!(!doc.rebuilt, "偏移相对文件头，交叉引用表仍然有效");
        assert!(matches!(doc.object(4), Some(PdfObject::Stream(_))));
    }

    #[test]
    fn broken_startxref_triggers_rebuild() {
        let text = String::from_utf8(simple()).expect("ascii");
        // 把 startxref 的偏移改错。
        let at = text.rfind("startxref\n").expect("有 startxref") + 10;
        let end = text[at..].find('\n').expect("有换行") + at;
        let broken = format!("{}{}{}", &text[..at], "7", &text[end..]);
        let doc = PdfDocument::load(broken.into_bytes()).expect("重建之后能打开");
        assert!(doc.rebuilt);
        assert_eq!(doc.startxref, 0);
        assert!(matches!(doc.object(4), Some(PdfObject::Stream(_))));
        assert!(doc.root().is_some());
    }

    #[test]
    fn wrong_offsets_fail_verification_and_rebuild() {
        // 文件头后面多出几个字节，表里的偏移全都对不上：重建。
        let text = String::from_utf8(simple()).expect("ascii");
        let shifted = text.replace("%PDF-1.7\n", "%PDF-1.7\n%abcd\n");
        let doc = PdfDocument::load(shifted.into_bytes()).expect("重建之后能打开");
        assert!(doc.rebuilt);
        assert!(matches!(doc.object(3), Some(PdfObject::Dict(_))));
    }

    #[test]
    fn later_definition_wins_when_rebuilding() {
        let mut text = String::from("%PDF-1.4\n");
        text.push_str(&format!("1 0 obj\n{CATALOG}\nendobj\n"));
        text.push_str(&format!("2 0 obj\n{PAGES}\nendobj\n"));
        text.push_str("3 0 obj\n<< /Type /Page /Old true >>\nendobj\n");
        text.push_str(&format!("3 0 obj\n{PAGE}\nendobj\n"));
        text.push_str("trailer\n<< /Root 1 0 R >>\n");
        let doc = PdfDocument::load(text.into_bytes()).expect("能打开");
        assert!(doc.rebuilt);
        let page = doc.object(3).expect("页对象");
        assert!(page.as_dict().expect("字典").get(b"Old").is_none());
    }

    #[test]
    fn object_number_at_offset_must_match() {
        // 交叉引用说对象 4 在对象 3 的位置上：取不到。
        let text = String::from_utf8(simple()).expect("ascii");
        let doc = PdfDocument::load(text.clone().into_bytes()).expect("能打开");
        let off3 = doc.object_offset(3).expect("偏移");
        let off4 = doc.object_offset(4).expect("偏移");
        let swapped = text.replace(
            &format!("{off4:010} 00000 n"),
            &format!("{off3:010} 00000 n"),
        );
        let doc = PdfDocument::load(swapped.into_bytes()).expect("能打开");
        assert!(doc.object(4).is_none());
        assert!(doc.object(3).is_some());
    }

    #[test]
    fn stream_length_reference_and_wrong_length() {
        let data = build(
            &[
                (1, CATALOG),
                (2, PAGES),
                (3, PAGE),
                (
                    4,
                    "<< /Length 5 0 R >>\nstream\nq 1 0 0 1 0 0 cm Q\nendstream",
                ),
                (5, "18"),
                // 长度写错：退回找 endstream，去掉紧挨着的一个行尾。
                (6, "<< /Length 99999 >>\nstream\nBT ET\n\nendstream"),
                // stream 后面有多余的空格，数据从下一行开始。
                (7, "<< /Length 3 >>\nstream   \nq Q\nendstream"),
            ],
            "",
        );
        let doc = PdfDocument::load(data).expect("能打开");
        let data_of = |n: u32| match doc.object(n) {
            Some(PdfObject::Stream(s)) => s.data,
            other => panic!("对象 {n} 应当是流：{other:?}"),
        };
        assert_eq!(data_of(4), b"q 1 0 0 1 0 0 cm Q");
        assert_eq!(data_of(6), b"BT ET\n");
        assert_eq!(data_of(7), b"q Q");
    }

    /// 对象 2、3 放在对象流 5 里；交叉引用是流（对象 6）。
    pub(crate) fn with_object_stream() -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        let mut push = |out: &mut Vec<u8>, number: u32, body: &[u8]| {
            offsets.insert(number, out.len());
            out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        };
        push(&mut out, 1, CATALOG.as_bytes());
        push(&mut out, 4, b"<< /Length 4 >>\nstream\nq Q \nendstream");
        let header = format!("2 0 3 {} ", PAGES.len() + 1);
        let body = format!("{header}{PAGES} {PAGE}");
        let objstm = format!(
            "<< /Type /ObjStm /N 2 /First {} /Length {} >>\nstream\n{body}\nendstream",
            header.len(),
            body.len()
        );
        push(&mut out, 5, objstm.as_bytes());
        let xref_pos = out.len();
        let mut rows: Vec<u8> = Vec::new();
        let mut row = |kind: u8, a: u32, b: u8| {
            rows.push(kind);
            rows.extend_from_slice(&a.to_be_bytes());
            rows.push(b);
        };
        row(0, 0, 0);
        row(1, offsets[&1] as u32, 0);
        row(2, 5, 0);
        row(2, 5, 1);
        row(1, offsets[&4] as u32, 0);
        row(1, offsets[&5] as u32, 0);
        row(1, xref_pos as u32, 0);
        out.extend_from_slice(
            format!(
                "6 0 obj\n<< /Type /XRef /Size 7 /W [1 4 1] /Root 1 0 R /Length {} >>\nstream\n",
                rows.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&rows);
        out.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{xref_pos}\n%%EOF\n").as_bytes(),
        );
        out
    }

    #[test]
    fn xref_stream_with_object_stream() {
        let out = with_object_stream();
        let doc = PdfDocument::load(out.clone()).expect("能打开");
        assert!(doc.uses_xref_stream && !doc.rebuilt);
        assert!(doc.is_compressed(3));
        let page = doc.object(3).expect("对象流里的页对象");
        assert_eq!(
            page.as_dict().and_then(|d| d.get(b"Contents")),
            Some(&PdfObject::Ref(4, 0))
        );
        assert_eq!(crate::pdf::pages::page_count(&doc), 1);

        // 同一份文件去掉 startxref：重建时对象流里的对象登记成压缩对象，trailer 取自 XRef 流的字典。
        let cut = out
            .windows(9)
            .rposition(|w| w == b"startxref")
            .expect("有 startxref");
        let doc = PdfDocument::load(out[..cut].to_vec()).expect("重建之后能打开");
        assert!(doc.rebuilt);
        assert!(doc.is_compressed(2) && doc.is_compressed(3));
        assert!(doc.root().is_some());
        assert!(doc.object(3).is_some());
    }

    #[test]
    fn not_a_pdf_and_empty_inputs() {
        assert!(PdfDocument::load(Vec::new()).is_err());
        assert!(PdfDocument::load(b"hello".to_vec()).is_err());
        // 没有文件头也没有任何对象：重建失败。
        assert!(PdfDocument::load(vec![b' '; 2000]).is_err());
    }

    #[test]
    fn startxref_is_only_searched_near_the_end() {
        let mut data = simple();
        // 后面接 5000 字节的注释：startxref 离文件尾太远，按重建处理。
        data.extend_from_slice(b"%");
        data.extend(std::iter::repeat_n(b'x', 5000));
        data.push(b'\n');
        let doc = PdfDocument::load(data).expect("能打开");
        assert!(doc.rebuilt);
    }

    #[test]
    fn startxref_search_window() {
        assert_eq!(parse_startxref(b"junk startxref\n12\n%%EOF\n--------"), 12);
        // 数字不小于数据长度：无效。
        assert_eq!(parse_startxref(b"junk startxref\n99\n%%EOF\n--------"), 0);
        // 词的最后一个字节必须在 `长度 - 9` 或更靠前。
        assert_eq!(parse_startxref(b"0123456789 startxref 5"), 0);
        // 左边紧挨着普通字符：不是独立的词。
        assert_eq!(parse_startxref(b"junkstartxref\n12\n%%EOF\n--------"), 0);
    }

    #[test]
    fn incremental_update_overrides_older_table() {
        // 原文件之后追加一节：对象 4 换了内容。
        let mut data = simple();
        let old_xref = String::from_utf8_lossy(&data)
            .rfind("xref\n0 5")
            .expect("有 xref");
        let new_pos = data.len();
        data.extend_from_slice(b"4 0 obj\n<< /Length 5 >>\nstream\nq Q S\nendstream\nendobj\n");
        let xref = data.len();
        data.extend_from_slice(
            format!(
                "xref\n4 1\n{new_pos:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Prev {old_xref} >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        let doc = PdfDocument::load(data).expect("能打开");
        assert!(!doc.rebuilt);
        let Some(PdfObject::Stream(s)) = doc.object(4) else {
            panic!("对象 4 应当是流")
        };
        assert_eq!(s.data, b"q Q S");
        assert_eq!(doc.object_offset(4), Some(new_pos));
    }
}
