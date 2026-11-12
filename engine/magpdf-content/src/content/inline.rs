//! 内联图像（`BI … ID … EI`）的定界，照 PDFium 移植
//! （`CPDF_StreamContentParser::Handle_BeginImage`、`CPDF_StreamParser::ReadInlineStream` 和各过滤器的
//! “用掉了多少字节”，BSD-3-Clause，版权声明见本 crate 的 `PDFIUM-NOTICE.md`）。
//!
//! 内联图像的二进制数据里可以出现任何字节，包括看起来像 `EI` 的。PDFium 不是简单地找下一个
//! `EI`：没有过滤器时按宽、高、位深算出数据长度直接跳过；有过滤器时先让第一个过滤器的解码器
//! 走一遍、看它用掉多少字节，再从那里往后按普通元素解析直到遇到 `EI`。这里照做，数据的终点才
//! 和 PDFium 一致。
//!
//! `DCTDecode` 和 `CCITTFaxDecode` 的长度要靠 JPEG、传真解码器走完扫描行才知道，这里没有这两个
//! 解码器，遇到时报 [`InlineUnsupported`]，由对应层把这一页归为对不上。

use super::parser::{
    Element, Obj, StreamParser, dict_get, dict_set, is_line_ending, is_whitespace,
};

/// 内联图像用了这里定不了界的过滤器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineUnsupported {
    pub filter: String,
}

/// `BI` 的处理结果。
#[derive(Debug, Clone, PartialEq)]
pub enum InlineOutcome {
    /// 字典里遇到了不是 `ID` 的关键字：PDFium 把位置退回 `BI` 之后，当作什么都没发生。
    Aborted,
    /// 读完了：位置在 `EI` 之后（没找到 `EI` 时在数据末尾）。
    Done {
        /// 是否产生了图像对象（数据读得出来，而且找到了 `EI`）。
        object: bool,
        /// 展开缩写之后的字典。
        dict: Vec<(Vec<u8>, Obj)>,
    },
}

const KEY_ABBR: [(&[u8], &[u8]); 9] = [
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

const VALUE_ABBR: [(&[u8], &[u8]); 11] = [
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

fn full_name(table: &[(&[u8], &'static [u8])], abbr: &[u8]) -> Option<&'static [u8]> {
    table.iter().find(|(a, _)| *a == abbr).map(|(_, f)| *f)
}

/// `ReplaceAbbr`：键和名字值里的缩写换成全名，数组和字典里递归。
fn replace_abbr(obj: &mut Obj) {
    match obj {
        Obj::Dict(dict) => replace_abbr_in_dict(dict),
        Obj::Array(items) => {
            for item in items {
                match item {
                    Obj::Name(name) => {
                        if let Some(full) = full_name(&VALUE_ABBR, name) {
                            *name = full.to_vec();
                        }
                    }
                    other => replace_abbr(other),
                }
            }
        }
        _ => {}
    }
}

fn replace_abbr_in_dict(dict: &mut Vec<(Vec<u8>, Obj)>) {
    // PDFium 的字典按键排序遍历，替换也按这个顺序做（缩写和全名同时出现时，结果取决于顺序）。
    let mut keys: Vec<Vec<u8>> = dict.iter().map(|(k, _)| k.clone()).collect();
    keys.sort();
    for key in keys {
        let Some(index) = dict.iter().position(|(k, _)| *k == key) else {
            continue;
        };
        let mut key = key;
        if let Some(full) = full_name(&KEY_ABBR, &key) {
            // `ReplaceKey`：值挪到全名的键下面（已有就覆盖），缩写的键删掉。
            let (_, value) = dict.remove(index);
            dict_set(dict, full.to_vec(), value);
            key = full.to_vec();
        }
        let Some(slot) = dict.iter_mut().find(|(k, _)| *k == key) else {
            continue;
        };
        match &mut slot.1 {
            Obj::Name(name) => {
                if let Some(full) = full_name(&VALUE_ABBR, name) {
                    *name = full.to_vec();
                }
            }
            other => replace_abbr(other),
        }
    }
}

/// 颜色空间对象的分量数（`CPDF_ColorSpace::ComponentCount`）；加载不了时返回 `None`。
/// 只处理能直接写在内联字典里的写法。
pub fn colorspace_components(obj: &Obj) -> Option<u32> {
    match obj {
        Obj::Name(name) => stock_components(name),
        Obj::Array(items) => {
            let family = match items.first() {
                Some(Obj::Name(n)) => n.as_slice(),
                _ => return None,
            };
            if items.len() == 1 {
                return stock_components(family);
            }
            match family {
                b"CalGray" => Some(1),
                b"CalRGB" | b"Lab" => Some(3),
                b"Indexed" | b"I" => {
                    // 基础空间得能加载，而且不能是 Pattern 或 Indexed。
                    if items.len() < 4 {
                        return None;
                    }
                    match &items[1] {
                        Obj::Name(n) if n == b"Pattern" => None,
                        Obj::Array(a) if matches!(a.first(), Some(Obj::Name(n)) if n == b"Indexed" || n == b"I" || n == b"Pattern") => {
                            None
                        }
                        base => colorspace_components(base).map(|_| 1),
                    }
                }
                b"Separation" => Some(1),
                b"DeviceN" => match items.get(1) {
                    Some(Obj::Array(names)) if !names.is_empty() => Some(names.len() as u32),
                    _ => None,
                },
                b"Pattern" => Some(1),
                // ICCBased 要一条流，内联写不出来。
                _ => None,
            }
        }
        _ => None,
    }
}

fn stock_components(name: &[u8]) -> Option<u32> {
    match name {
        b"DeviceGray" | b"G" => Some(1),
        b"DeviceRGB" | b"RGB" => Some(3),
        b"DeviceCMYK" | b"CMYK" => Some(4),
        b"Pattern" => Some(1),
        _ => None,
    }
}

/// `Handle_BeginImage`：`BI` 已经读过，`parser` 的位置在它后面。
///
/// `resource_cs`：字典里的颜色空间是一个不属于三种设备空间的名字时，到页面资源里找它。
/// 返回 `None` 表示资源里没有（PDFium 这时当作没有颜色空间）；`Some(None)` 表示找到了但加载不了
/// （按 3 个分量算）；`Some(Some(n))` 是分量数。
pub fn handle_begin_image(
    parser: &mut StreamParser<'_>,
    resource_cs: &dyn Fn(&[u8]) -> Option<Option<u32>>,
) -> Result<InlineOutcome, InlineUnsupported> {
    let save = parser.pos();
    let mut dict: Vec<(Vec<u8>, Obj)> = Vec::new();
    loop {
        let element = parser.next_element();
        if element == Element::Keyword && parser.word() != b"ID" {
            parser.set_pos(save);
            return Ok(InlineOutcome::Aborted);
        }
        if element != Element::Name {
            // `ID`、数字、字符串、数据末尾都到这里：字典结束，后面当作图像数据。
            break;
        }
        // 键不解码（PDFium 这里用的是原文）。
        let key = parser.word()[1..].to_vec();
        match parser.read_next_object(false, false, 0) {
            Some(value) => dict_set(&mut dict, key, value),
            None => dict.retain(|(k, _)| *k != key),
        }
    }
    replace_abbr_in_dict(&mut dict);

    // 颜色空间：`None` = 没有；`Some(None)` = 有但加载不了；`Some(Some(n))` = 分量数。
    let cs: Option<Option<u32>> = match dict_get(&dict, b"ColorSpace") {
        None => None,
        Some(Obj::Name(name))
            if !matches!(
                name.as_slice(),
                b"DeviceRGB" | b"DeviceGray" | b"DeviceCMYK"
            ) =>
        {
            resource_cs(name)
        }
        Some(obj) => Some(colorspace_components(obj)),
    };

    let has_stream = read_inline_stream(parser, &dict, cs)?;
    loop {
        match parser.next_element() {
            Element::EndOfData => {
                return Ok(InlineOutcome::Done {
                    object: false,
                    dict,
                });
            }
            Element::Keyword if parser.word() == b"EI" => break,
            _ => {}
        }
    }
    Ok(InlineOutcome::Done {
        object: has_stream,
        dict,
    })
}

/// `CPDF_Dictionary::GetIntegerFor`。
pub fn dict_int(dict: &[(Vec<u8>, Obj)], key: &[u8]) -> i32 {
    dict_get(dict, key).map_or(0, Obj::get_integer)
}

fn dict_int_or(dict: &[(Vec<u8>, Obj)], key: &[u8], default: i32) -> i32 {
    match dict_get(dict, key) {
        Some(Obj::Number(n)) => n.to_i32(),
        _ => default,
    }
}

/// `fxge::CalculatePitch8`，再乘以高度；溢出时没有结果。
fn expected_size(bpc: u32, components: u32, width: u32, height: u32) -> Option<u32> {
    // 宽度按有符号整数传进去，负数使整个计算作废。
    if width > i32::MAX as u32 {
        return None;
    }
    let bits = u32::try_from(u64::from(bpc) * u64::from(components)).ok()?;
    let bits = bits.checked_mul(width)?.checked_add(7)?;
    (bits / 8).checked_mul(height)
}

/// `ReadInlineStream`。返回是否读出了数据流（读不出时 PDFium 不建对象，但照样往后找 `EI`）。
fn read_inline_stream(
    parser: &mut StreamParser<'_>,
    dict: &[(Vec<u8>, Obj)],
    cs: Option<Option<u32>>,
) -> Result<bool, InlineUnsupported> {
    let buf = parser.buf();
    let mut data_start = parser.pos();
    if data_start >= buf.len() {
        return Ok(false);
    }
    if is_whitespace(buf[data_start]) {
        data_start += 1;
        parser.set_pos(data_start);
        if data_start >= buf.len() {
            return Ok(false);
        }
    }

    let mut decoder: Vec<u8> = Vec::new();
    let mut params: Option<&[(Vec<u8>, Obj)]> = None;
    match dict_get(dict, b"Filter") {
        None => {}
        Some(Obj::Array(filters)) => {
            decoder = filters.first().map(Obj::get_string).unwrap_or_default();
            if let Some(Obj::Array(all)) = dict_get(dict, b"DecodeParms") {
                params = all.first().and_then(Obj::as_dict);
            }
        }
        Some(filter) => {
            decoder = filter.get_string();
            params = dict_get(dict, b"DecodeParms").and_then(Obj::as_dict);
        }
    }

    let width = dict_int(dict, b"Width") as u32;
    let height = dict_int(dict, b"Height") as u32;
    let (bpc, components) = match cs {
        Some(n) => (dict_int(dict, b"BitsPerComponent") as u32, n.unwrap_or(3)),
        None => (1, 1),
    };
    let Some(size) = expected_size(bpc, components, width, height) else {
        return Ok(false);
    };

    let remaining = buf.len() - data_start;
    if decoder.is_empty() {
        parser.set_pos(data_start + (size as usize).min(remaining));
        return Ok(true);
    }

    let consumed = consumed_by_decoder(&buf[data_start..], &decoder, params)?;
    let Some(mut actual) = consumed
        .filter(|n| *n <= i32::MAX as u32)
        .map(|n| n as usize)
    else {
        return Ok(false);
    };
    // 从解码器停下的地方往后，按普通元素解析到 `EI` 为止；中间跳过的元素也算进数据。
    parser.set_pos(data_start + actual);
    loop {
        let before = parser.pos();
        match parser.next_element() {
            Element::EndOfData => {
                parser.set_pos(data_start);
                return Ok(false);
            }
            Element::Keyword if parser.word() == b"EI" => break,
            _ => actual += parser.pos() - before,
        }
    }
    parser.set_pos(data_start + actual);
    Ok(true)
}

/// `DecodeInlineStream`：第一个过滤器的解码器用掉了多少字节。`None` 对应 PDFium 的无效偏移。
fn consumed_by_decoder(
    src: &[u8],
    decoder: &[u8],
    params: Option<&[(Vec<u8>, Obj)]>,
) -> Result<Option<u32>, InlineUnsupported> {
    let clamp = |n: usize| u32::try_from(n).ok();
    Ok(match decoder {
        b"FlateDecode" => {
            if !decode_params_valid(params) {
                None
            } else {
                clamp(flate_consumed(src))
            }
        }
        b"LZWDecode" => {
            if !decode_params_valid(params) {
                None
            } else {
                let early = params.is_none_or(|p| dict_int_or(p, b"EarlyChange", 1) != 0);
                lzw_consumed(src, early).and_then(clamp)
            }
        }
        b"ASCII85Decode" => clamp(a85_consumed(src)),
        b"ASCIIHexDecode" => clamp(hex_consumed(src)),
        b"RunLengthDecode" => run_length_consumed(src).and_then(clamp),
        b"DCTDecode" | b"CCITTFaxDecode" => {
            return Err(InlineUnsupported {
                filter: String::from_utf8_lossy(decoder).into_owned(),
            });
        }
        _ => None,
    })
}

/// `GetAndCheckDecodeParams`。
fn decode_params_valid(params: Option<&[(Vec<u8>, Obj)]>) -> bool {
    let Some(p) = params else { return true };
    let colors = dict_int_or(p, b"Colors", 1);
    let bpc = dict_int_or(p, b"BitsPerComponent", 8);
    let columns = dict_int_or(p, b"Columns", 1);
    if colors < 0 || bpc < 0 || columns < 0 {
        return false;
    }
    let Some(bits) = columns.checked_mul(colors).and_then(|v| v.checked_mul(bpc)) else {
        return false;
    };
    bits <= i32::MAX - 7
}

/// zlib 解到流结束、出错或者输入用完为止，返回用掉的输入字节数（`total_in`）。
pub(crate) fn flate_consumed(src: &[u8]) -> usize {
    let mut inflater = flate2::Decompress::new(true);
    let mut out = vec![0u8; 16 * 1024];
    loop {
        let before_in = inflater.total_in();
        let before_out = inflater.total_out();
        let offset = usize::try_from(before_in)
            .unwrap_or(src.len())
            .min(src.len());
        let status = inflater.decompress(&src[offset..], &mut out, flate2::FlushDecompress::Sync);
        match status {
            Ok(flate2::Status::Ok) => {
                // 既没读也没写，说明输入用完了。
                if inflater.total_in() == before_in && inflater.total_out() == before_out {
                    break;
                }
            }
            Ok(_) | Err(_) => break,
        }
    }
    usize::try_from(inflater.total_in())
        .unwrap_or(src.len())
        .min(src.len())
}

/// `CLZWDecoder::Decode` 的码流部分：只关心读到了哪一位，不保留解出来的字节。
/// 解码失败（一个字节都没解出来、码表引用无效）时返回 `None`。
fn lzw_consumed(src: &[u8], early_change: bool) -> Option<usize> {
    const NO_CODE: u32 = 0xFFFF_FFFF;
    const STACK: usize = 4000;
    let early = u32::from(early_change);
    let mut codes = vec![0u32; 5021];
    let mut current_code = 0u32;
    let mut code_len = 9u32;
    let mut bit_pos = 0usize;
    let mut produced = 0u64;
    let mut old_code = NO_CODE;
    let mut last_char = 0u8;

    // `DecodeString`：把一个码展开成字节（倒着压栈）。`len` 是栈里的字节数，`last` 是最后压进去的
    // 那个字节，也就是这一串的第一个字节。
    fn decode_string(mut code: u32, codes: &[u32], current: u32, len: &mut usize, last: &mut u8) {
        while let Some(index) = code.checked_sub(258).filter(|i| *i < current) {
            let data = codes[index as usize];
            if *len >= STACK {
                return;
            }
            *last = data as u8;
            *len += 1;
            code = data >> 16;
        }
        if *len >= STACK {
            return;
        }
        *last = code as u8;
        *len += 1;
    }

    // `AddCode`：码表加一项，项数到了临界值时码长加一位。
    fn add_code(
        codes: &mut [u32],
        current: &mut u32,
        len: &mut u32,
        early: u32,
        prefix: u32,
        append: u8,
    ) {
        if *current + early == 4094 {
            return;
        }
        codes[*current as usize] = (prefix << 16) | u32::from(append);
        *current += 1;
        match *current + early {
            254 => *len = 10,
            766 => *len = 11,
            1790 => *len = 12,
            _ => {}
        }
    }

    loop {
        if bit_pos + code_len as usize > src.len() * 8 {
            break;
        }
        let mut byte = bit_pos / 8;
        let shift = (bit_pos % 8) as u32;
        let mut left = code_len;
        let mut code = 0u32;
        if shift != 0 {
            left -= 8 - shift;
            code = (u32::from(src[byte]) & ((1 << (8 - shift)) - 1)) << left;
            byte += 1;
        }
        if left < 8 {
            code |= u32::from(src[byte]) >> (8 - left);
        } else {
            left -= 8;
            code |= u32::from(src[byte]) << left;
            byte += 1;
            if left != 0 {
                code |= u32::from(src[byte]) >> (8 - left);
            }
        }
        bit_pos += code_len as usize;

        if code < 256 {
            produced += 1;
            last_char = code as u8;
            if old_code != NO_CODE {
                add_code(
                    &mut codes,
                    &mut current_code,
                    &mut code_len,
                    early,
                    old_code,
                    last_char,
                );
            }
            old_code = code;
            continue;
        }
        if code == 256 {
            code_len = 9;
            current_code = 0;
            old_code = NO_CODE;
            continue;
        }
        if code == 257 {
            break;
        }
        if old_code == NO_CODE {
            return None;
        }
        let mut stack_len = 0usize;
        let mut first = last_char;
        if code - 258 >= current_code {
            // 码表里还没有这个码：是“上一串加上它自己的第一个字节”。
            stack_len = 1;
            decode_string(old_code, &codes, current_code, &mut stack_len, &mut first);
        } else {
            decode_string(code, &codes, current_code, &mut stack_len, &mut first);
        }
        produced += stack_len as u64;
        if produced > u64::from(u32::MAX) {
            return None;
        }
        last_char = first;
        if old_code >= 258 && old_code - 258 >= current_code {
            break;
        }
        add_code(
            &mut codes,
            &mut current_code,
            &mut code_len,
            early,
            old_code,
            last_char,
        );
        old_code = code;
    }
    if produced == 0 {
        return None;
    }
    Some(bit_pos.div_ceil(8))
}

/// `A85Decode` 用掉的字节数。
fn a85_consumed(src: &[u8]) -> usize {
    let skip = |ch: u8| is_line_ending(ch) || ch == b' ' || ch == b'\t';
    // 开头就不是合法字符时，什么都不解。
    match src.first() {
        None => return 0,
        Some(&ch) if ch != b'z' && !(b'!'..=b'u').contains(&ch) && !skip(ch) => return 0,
        Some(_) => {}
    }
    let mut pos = 0;
    while pos < src.len() {
        let ch = src[pos];
        pos += 1;
        if skip(ch) || ch == b'z' {
            continue;
        }
        if !(b'!'..=b'u').contains(&ch) {
            break;
        }
    }
    if pos < src.len() && src[pos] == b'>' {
        pos += 1;
    }
    pos
}

/// `HexDecode` 用掉的字节数。
fn hex_consumed(src: &[u8]) -> usize {
    match src.iter().position(|b| *b == b'>') {
        Some(i) => i + 1,
        None => src.len(),
    }
}

/// `RunLengthDecode` 用掉的字节数。
fn run_length_consumed(src: &[u8]) -> Option<usize> {
    const MAX_STREAM_SIZE: u32 = 20 * 1024 * 1024;
    let mut dest_size = 0u32;
    let mut i = 0usize;
    while i < src.len() {
        let b = src[i];
        if b == 128 {
            break;
        }
        if b < 128 {
            dest_size = dest_size.checked_add(u32::from(b) + 1)?;
            i += usize::from(b) + 2;
        } else {
            dest_size = dest_size.checked_add(257 - u32::from(b))?;
            i += 2;
        }
    }
    if dest_size >= MAX_STREAM_SIZE {
        return None;
    }
    Some((i + 1).min(src.len()))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn no_resources(_: &[u8]) -> Option<Option<u32>> {
        None
    }

    /// 处理一段以 `BI` 开头的数据，返回（是否有对象，处理完之后的位置）。
    fn run(data: &[u8]) -> (Option<bool>, usize) {
        let mut p = StreamParser::new(data);
        assert_eq!(p.next_element(), Element::Keyword);
        assert_eq!(p.word(), b"BI");
        match handle_begin_image(&mut p, &no_resources).expect("不该遇到定不了界的过滤器")
        {
            InlineOutcome::Aborted => (None, p.pos()),
            InlineOutcome::Done { object, .. } => (Some(object), p.pos()),
        }
    }

    #[test]
    fn raw_data_is_skipped_by_computed_size() {
        // 2×2、8 位灰度：4 个字节的数据里正好有 "EI "，不能被它骗了。
        let mut data = b"BI /W 2 /H 2 /BPC 8 /CS /G ID ".to_vec();
        data.extend_from_slice(b"EI E");
        data.extend_from_slice(b" EI Q");
        let (object, pos) = run(&data);
        assert_eq!(object, Some(true));
        assert_eq!(&data[pos..], b" Q");
    }

    #[test]
    fn image_mask_uses_one_bit_per_pixel() {
        // 9×2 的蒙版：每行 2 字节，共 4 字节。
        let data = b"BI /IM true /W 9 /H 2 ID \x01\x02\x03\x04 EI S";
        let (object, pos) = run(data);
        assert_eq!(object, Some(true));
        assert_eq!(&data[pos..], b" S");
    }

    #[test]
    fn unknown_colorspace_name_counts_as_no_colorspace() {
        // 资源里没有 /Cs1：PDFium 按 1 位、1 个分量算，4×1 只有 1 个字节。
        let data = b"BI /W 4 /H 1 /BPC 8 /CS /Cs1 ID \xAA EI S";
        let (object, pos) = run(data);
        assert_eq!(object, Some(true));
        assert_eq!(&data[pos..], b" S");
    }

    #[test]
    fn keyword_inside_the_dictionary_aborts() {
        let data = b"BI /W 2 q Q";
        let (object, pos) = run(data);
        assert_eq!(object, None);
        assert_eq!(&data[pos..], b" /W 2 q Q");
    }

    #[test]
    fn missing_ei_yields_no_object() {
        let data = b"BI /W 1 /H 1 /BPC 8 /CS /G ID x";
        let (object, pos) = run(data);
        assert_eq!(object, Some(false));
        assert_eq!(pos, data.len());
    }

    #[test]
    fn flate_data_ends_where_the_decoder_stops() {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&[0x45, 0x49, 0x20, 0x45, 0x49, 0x20, 0x00, 0xFF])
            .unwrap();
        let packed = enc.finish().unwrap();
        assert_eq!(flate_consumed(&packed), packed.len());
        let mut data = b"BI /W 8 /H 1 /BPC 8 /CS /G /F /Fl ID ".to_vec();
        data.extend_from_slice(&packed);
        data.extend_from_slice(b"\nEI\nS");
        let (object, pos) = run(&data);
        assert_eq!(object, Some(true));
        assert_eq!(&data[pos..], b"\nS");
    }

    #[test]
    fn ascii_filters_report_consumed_bytes() {
        assert_eq!(hex_consumed(b"00 FF> EI"), 6);
        assert_eq!(hex_consumed(b"00FF"), 4);
        assert_eq!(a85_consumed(b"87cUR~> EI"), 7);
        assert_eq!(a85_consumed(b"~>"), 0);
        assert_eq!(a85_consumed(b""), 0);
        let data = b"BI /W 1 /H 1 /BPC 8 /CS /G /F [/AHx /Fl] ID 78 9C> EI S";
        let (object, pos) = run(data);
        assert_eq!(object, Some(true));
        assert_eq!(&data[pos..], b" S");
    }

    #[test]
    fn run_length_and_lzw_consumed_bytes() {
        // 两段字面量 + EOD。
        assert_eq!(
            run_length_consumed(&[1, b'a', b'b', 255, b'z', 128, 9, 9]),
            Some(6)
        );
        // 9 位码：256(清表) 65 66 257(结束)，共 36 位，占 5 个字节。
        let codes: [u16; 4] = [256, 65, 66, 257];
        let mut bits = Vec::new();
        for c in codes {
            for b in (0..9).rev() {
                bits.push((c >> b) & 1 == 1);
            }
        }
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (i, b) in bits.iter().enumerate() {
            if *b {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        bytes.extend_from_slice(b"tail");
        assert_eq!(lzw_consumed(&bytes, true), Some(5));
        // 一上来就是结束码：一个字节都没解出来，算失败。
        assert_eq!(lzw_consumed(&[0x80, 0x80], true), None);
    }

    #[test]
    fn dct_is_reported_as_unsupported() {
        let mut p = StreamParser::new(b"BI /W 1 /H 1 /BPC 8 /CS /RGB /F /DCT ID xx EI");
        p.next_element();
        let err = handle_begin_image(&mut p, &no_resources).unwrap_err();
        assert_eq!(err.filter, "DCTDecode");
    }

    #[test]
    fn abbreviations_expand_recursively() {
        let mut p = StreamParser::new(
            b"BI /W 1 /H 1 /BPC 8 /CS [/I /RGB 0 <000000>] /F [/A85] /DP [<< /K 1 >>] ID !!~> EI",
        );
        p.next_element();
        let InlineOutcome::Done { object, dict } =
            handle_begin_image(&mut p, &no_resources).unwrap()
        else {
            panic!("不应中止")
        };
        assert!(object);
        assert_eq!(dict_int(&dict, b"Width"), 1);
        let Some(Obj::Array(cs)) = dict_get(&dict, b"ColorSpace") else {
            panic!("颜色空间应当是数组")
        };
        assert_eq!(cs[0], Obj::Name(b"Indexed".to_vec()));
        assert_eq!(cs[1], Obj::Name(b"DeviceRGB".to_vec()));
        assert_eq!(colorspace_components(&Obj::Array(cs.clone())), Some(1));
        let Some(Obj::Array(f)) = dict_get(&dict, b"Filter") else {
            panic!("过滤器应当是数组")
        };
        assert_eq!(f[0], Obj::Name(b"ASCII85Decode".to_vec()));
    }
}
