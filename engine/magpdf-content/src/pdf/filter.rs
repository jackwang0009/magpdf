//! 流过滤器：只实现内容流和常见结构里会遇到的几种。
//!
//! 两套入口：[`apply_filter_chain`] 严格，解不开就报错；[`decode_like_pdfium`] 照 PDFium 的容错规则，
//! 从不报错。取内容流、交叉引用流、对象流一律用后者，这样拿到的字节才和 PDFium 解释的那一串相同。

use std::io::{Read, Write};

use super::object::{PdfObject, hex_value, is_whitespace};

#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error("不支持的过滤器 {0}（内容流层只解码 Flate/LZW/ASCIIHex/ASCII85/RunLength）")]
    Unsupported(String),
    #[error("CRC 校验失败")]
    Checksum,
    #[error("Flate 解压失败：{0}")]
    Inflate(String),
    #[error("ASCII85 数据损坏")]
    Ascii85,
    #[error("LZW 数据损坏")]
    Lzw,
}

fn name_of(obj: &PdfObject) -> Option<String> {
    obj.as_name()
        .map(|n| String::from_utf8_lossy(n).into_owned())
}

/// 按 `/Filter`（名字或名字数组）与 `/DecodeParms`（字典或字典数组）依次解码。
pub fn apply_filter_chain(
    data: &[u8],
    filter: Option<&PdfObject>,
    parms: Option<&PdfObject>,
) -> Result<Vec<u8>, FilterError> {
    let filters: Vec<PdfObject> = match filter {
        None | Some(PdfObject::Null) => return Ok(data.to_vec()),
        Some(PdfObject::Array(a)) => a.clone(),
        Some(other) => vec![other.clone()],
    };
    let mut out = data.to_vec();
    for (i, f) in filters.iter().enumerate() {
        let parm = match parms {
            Some(PdfObject::Array(a)) => a.get(i).cloned(),
            other => {
                // 单个字典只对第一个过滤器有效（多余的参数在规范里被忽略）。
                if i == 0 { other.cloned() } else { None }
            }
        };
        out = apply_one(&out, f, parm.as_ref())?;
    }
    Ok(out)
}

fn parm_int(parms: Option<&PdfObject>, key: &[u8], default: i64) -> i64 {
    parms
        .and_then(PdfObject::as_dict)
        .and_then(|d| d.get(key))
        .and_then(PdfObject::as_int)
        .unwrap_or(default)
}

pub(crate) fn apply_one(
    data: &[u8],
    filter: &PdfObject,
    parms: Option<&PdfObject>,
) -> Result<Vec<u8>, FilterError> {
    let Some(name) = name_of(filter) else {
        return Err(FilterError::Unsupported(format!("{filter:?}")));
    };
    match name.as_str() {
        "FlateDecode" | "Fl" => {
            let raw = inflate(data)?;
            Ok(unpredict(&raw, parm_int(parms, b"Predictor", 1), parms))
        }
        "LZWDecode" | "LZW" => {
            let early = parm_int(parms, b"EarlyChange", 1);
            let raw = lzw_decode(data, early == 1)?;
            Ok(unpredict(&raw, parm_int(parms, b"Predictor", 1), parms))
        }
        "ASCIIHexDecode" | "AHx" => Ok(ascii_hex(data)),
        "ASCII85Decode" | "A85" => ascii85(data),
        "RunLengthDecode" | "RL" => Ok(run_length(data)),
        _ => Err(FilterError::Unsupported(name)),
    }
}

/// zlib 解压。FlateDecode 的数据按规范是 zlib 包装（带 2 字节头）。
pub fn inflate(data: &[u8]) -> Result<Vec<u8>, FilterError> {
    let mut out = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    decoder
        .read_to_end(&mut out)
        .map_err(|e| FilterError::Inflate(e.to_string()))?;
    Ok(out)
}

/// zlib 压缩（写回用）。
pub fn encode_flate(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    // 写内存缓冲不会失败；失败时退回未压缩数据由调用方处理。
    if enc.write_all(data).is_err() {
        return data.to_vec();
    }
    enc.finish().unwrap_or_default()
}

/// 照 PDFium 的 `CPDF_StreamAcc::LoadAllDataFiltered` 解码一条流，从不报错
/// （`GetDecoderArray`、`PDF_DataDecode`，BSD-3-Clause，见 `PDFIUM-NOTICE.md`）。
///
/// 内容流的字节必须和 PDFium 解释的那一串完全相同，所以解不开时怎么办也得照它的来：
///
/// - `/Filter` 既不是名字也不是数组、数组里有不是名字的项、除最后一个以外有不属于文本类的过滤器：
///   整条流按原始字节用；
/// - Flate 解到哪儿算哪儿，数据损坏时保留已经解出来的部分；
/// - LZW 解码失败、预测器的参数无效：整条流按原始字节用；
/// - 遇到不认识的过滤器（图像类的，或者这一版 PDFium 没编进去的，比如 Brotli）就停下，
///   用到此为止解出来的数据；
/// - 最后一个字节都没解出来时，用原始字节。
///
/// `filter`、`parms` 是已经解过一层引用的 `/Filter` 和 `/DecodeParms`；`resolve` 用来解数组里的引用。
pub fn decode_like_pdfium(
    data: &[u8],
    filter: Option<&PdfObject>,
    parms: Option<&PdfObject>,
    resolve: &dyn Fn(&PdfObject) -> Option<PdfObject>,
) -> Vec<u8> {
    decode_like_pdfium_checked(data, filter, parms, resolve).0
}

/// 同 [`decode_like_pdfium`]，另外说明这次解码是不是**完整**的：`/Filter` 里的每一个过滤器都认得、
/// 都解成功了（没有过滤器也算）。
///
/// 不完整时，PDFium 用的那一串字节（返回的第一项）只是它自己的看法：停在不认得的过滤器前面、解码失败后
/// 退回原始字节……别的阅读器（比如支持 Brotli 的）读同一条流，得到的可以是另一串。写回层不能拿这样的
/// 字节配上一个新的 `/Filter` 把原来的流换掉，见 `rewrite::save`。
///
/// 没有管的一种情况：Flate 数据损坏时 PDFium 用已经解出来的那一部分，这里照样算完整。
pub fn decode_like_pdfium_checked(
    data: &[u8],
    filter: Option<&PdfObject>,
    parms: Option<&PdfObject>,
    resolve: &dyn Fn(&PdfObject) -> Option<PdfObject>,
) -> (Vec<u8>, bool) {
    if data.is_empty() {
        return (Vec::new(), true);
    }
    let raw = |complete: bool| (data.to_vec(), complete);
    let mut decoders: Vec<(Vec<u8>, Option<PdfObject>)> = Vec::new();
    match filter {
        None | Some(PdfObject::Null) => return raw(true),
        Some(PdfObject::Array(items)) => {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                match resolve(item) {
                    Some(PdfObject::Name(name)) => names.push(name),
                    _ => return raw(false),
                }
            }
            let text_only = names.len().saturating_sub(1);
            if names[..text_only].iter().any(|n| !is_text_decoder(n)) {
                return raw(false);
            }
            let parm_items = match parms {
                Some(PdfObject::Array(a)) => Some(a),
                _ => None,
            };
            for (i, name) in names.into_iter().enumerate() {
                let parm = parm_items
                    .and_then(|a| a.get(i))
                    .and_then(resolve)
                    .filter(|p| p.as_dict().is_some());
                decoders.push((name, parm));
            }
        }
        Some(PdfObject::Name(name)) => {
            decoders.push((
                name.clone(),
                parms.filter(|p| p.as_dict().is_some()).cloned(),
            ));
        }
        Some(_) => return raw(false),
    }

    let mut complete = true;
    let mut decoded: Option<Vec<u8>> = None;
    for (name, parm) in &decoders {
        let src: &[u8] = decoded.as_deref().unwrap_or(data);
        let out = match name.as_slice() {
            // PDFium 跳过它；数据要是真的另外加过密，这里看到的就不是明文。
            b"Crypt" => {
                complete = false;
                continue;
            }
            b"FlateDecode" | b"Fl" => flate_or_lzw(false, src, parm.as_ref()),
            b"LZWDecode" | b"LZW" => flate_or_lzw(true, src, parm.as_ref()),
            b"ASCII85Decode" | b"A85" => Some(a85_decode_pdfium(src).0),
            b"ASCIIHexDecode" | b"AHx" => Some(hex_decode_pdfium(src).0),
            b"RunLengthDecode" | b"RL" => run_length_decode_pdfium(src).map(|r| r.0),
            // 当作图像解码器：到此为止。
            _ => {
                complete = false;
                break;
            }
        };
        match out {
            Some(out) => decoded = Some(out),
            None => return raw(false),
        }
    }
    match decoded {
        Some(out) if !out.is_empty() => (out, complete),
        // 有过滤器而一个字节都没解出来：PDFium 退回去用原始字节，那不是这条流真正的内容。
        _ => raw(false),
    }
}

fn is_text_decoder(name: &[u8]) -> bool {
    matches!(
        name,
        b"FlateDecode"
            | b"Fl"
            | b"LZWDecode"
            | b"LZW"
            | b"ASCII85Decode"
            | b"A85"
            | b"ASCIIHexDecode"
            | b"AHx"
            | b"RunLengthDecode"
            | b"RL"
    )
}

/// `FlateOrLZWDecode`：参数无效、LZW 失败、预测器失败时返回 `None`。
fn flate_or_lzw(lzw: bool, src: &[u8], parms: Option<&PdfObject>) -> Option<Vec<u8>> {
    let int = |key: &[u8], default: i64| parm_int(parms, key, default);
    let predictor = int(b"Predictor", 0);
    let colors = int(b"Colors", 1);
    let bpc = int(b"BitsPerComponent", 8);
    let columns = int(b"Columns", 1);
    if colors < 0 || bpc < 0 || columns < 0 {
        return None;
    }
    let bits = columns.checked_mul(colors)?.checked_mul(bpc)?;
    if bits > i64::from(i32::MAX) - 7 {
        return None;
    }
    let raw = if lzw {
        lzw_decode_pdfium(src, int(b"EarlyChange", 1) != 0)?.0
    } else {
        inflate_lenient(src).0
    };
    if predictor >= 10 || predictor == 2 {
        let row_bytes = (bits + 7) / 8;
        if row_bytes <= 0 {
            return None;
        }
        return Some(unpredict(&raw, predictor, parms));
    }
    Some(raw)
}

/// zlib 解压，解到流结束、出错或者输入用完为止；出错时保留已经解出来的部分。
/// 返回（解出来的数据，用掉的输入字节数）。
pub(crate) fn inflate_lenient(src: &[u8]) -> (Vec<u8>, usize) {
    let mut inflater = flate2::Decompress::new(true);
    let mut out: Vec<u8> = Vec::with_capacity(src.len().saturating_mul(3).clamp(256, 1 << 22));
    loop {
        if out.len() == out.capacity() {
            out.reserve(out.capacity().max(4096));
        }
        let before_in = inflater.total_in();
        let before_out = inflater.total_out();
        let offset = usize::try_from(before_in)
            .unwrap_or(src.len())
            .min(src.len());
        let status =
            inflater.decompress_vec(&src[offset..], &mut out, flate2::FlushDecompress::Sync);
        match status {
            Ok(flate2::Status::StreamEnd) | Err(_) => break,
            Ok(_) => {
                // 既没读也没写，而且输出还有空位：输入用完了。
                if inflater.total_in() == before_in
                    && inflater.total_out() == before_out
                    && out.len() < out.capacity()
                {
                    break;
                }
            }
        }
    }
    let consumed = usize::try_from(inflater.total_in())
        .unwrap_or(src.len())
        .min(src.len());
    (out, consumed)
}

/// `CLZWDecoder::Decode`。返回（解出来的数据，用掉的输入字节数）；一个字节都没解出来、
/// 或者码表引用无效时返回 `None`。
pub(crate) fn lzw_decode_pdfium(src: &[u8], early_change: bool) -> Option<(Vec<u8>, usize)> {
    const NO_CODE: u32 = 0xFFFF_FFFF;
    const STACK: usize = 4000;
    let early = u32::from(early_change);
    let mut codes = vec![0u32; 5021];
    let mut current_code = 0u32;
    let mut code_len = 9u32;
    let mut bit_pos = 0usize;
    let mut old_code = NO_CODE;
    let mut last_char = 0u8;
    let mut out: Vec<u8> = Vec::new();
    let mut stack: Vec<u8> = Vec::with_capacity(STACK);

    // `DecodeString`：把一个码展开成字节，倒着压栈。
    fn decode_string(mut code: u32, codes: &[u32], current: u32, stack: &mut Vec<u8>) {
        while let Some(index) = code.checked_sub(258).filter(|i| *i < current) {
            let data = codes[index as usize];
            if stack.len() >= STACK {
                return;
            }
            stack.push(data as u8);
            code = data >> 16;
        }
        if stack.len() >= STACK {
            return;
        }
        stack.push(code as u8);
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
            out.push(code as u8);
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
        stack.clear();
        if code - 258 >= current_code {
            // 码表里还没有这个码：是“上一串加上它自己的第一个字节”。
            stack.push(last_char);
            decode_string(old_code, &codes, current_code, &mut stack);
        } else {
            decode_string(code, &codes, current_code, &mut stack);
        }
        if out.len() + stack.len() > u32::MAX as usize {
            return None;
        }
        out.extend(stack.iter().rev());
        last_char = stack.last().copied().unwrap_or(last_char);
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
    if out.is_empty() {
        return None;
    }
    Some((out, bit_pos.div_ceil(8)))
}

fn is_a85_space(ch: u8) -> bool {
    matches!(ch, b'\r' | b'\n' | b' ' | b'\t')
}

/// PDFium 的 `A85Decode`：返回（数据，用掉的字节数）。遇到不合法的字符就停，之前的照常解。
pub(crate) fn a85_decode_pdfium(src: &[u8]) -> (Vec<u8>, usize) {
    // 开头就不是合法字符时，什么都不解。
    match src.first() {
        None => return (Vec::new(), 0),
        Some(&ch) if ch != b'z' && !(b'!'..=b'u').contains(&ch) && !is_a85_space(ch) => {
            return (Vec::new(), 0);
        }
        Some(_) => {}
    }
    let mut out = Vec::with_capacity(src.len() / 5 * 4 + 4);
    let mut state = 0usize;
    let mut res = 0u32;
    let mut pos = 0usize;
    while pos < src.len() {
        let ch = src[pos];
        pos += 1;
        if is_a85_space(ch) {
            continue;
        }
        if ch == b'z' {
            out.extend_from_slice(&[0, 0, 0, 0]);
            state = 0;
            res = 0;
            continue;
        }
        if !(b'!'..=b'u').contains(&ch) {
            break;
        }
        res = res.wrapping_mul(85).wrapping_add(u32::from(ch - 33));
        if state < 4 {
            state += 1;
            continue;
        }
        out.extend_from_slice(&res.to_be_bytes());
        state = 0;
        res = 0;
    }
    if state > 0 {
        for _ in state..5 {
            res = res.wrapping_mul(85).wrapping_add(84);
        }
        out.extend_from_slice(&res.to_be_bytes()[..state - 1]);
    }
    if pos < src.len() && src[pos] == b'>' {
        pos += 1;
    }
    (out, pos)
}

/// PDFium 的 `HexDecode`：返回（数据，用掉的字节数）。
pub(crate) fn hex_decode_pdfium(src: &[u8]) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(src.len() / 2 + 1);
    let mut hi: Option<u8> = None;
    let mut i = 0usize;
    while i < src.len() {
        let ch = src[i];
        if ch == b'>' {
            i += 1;
            break;
        }
        if let Some(v) = hex_value(ch) {
            match hi {
                None => hi = Some(v * 16),
                Some(h) => {
                    out.push(h + v);
                    hi = None;
                }
            }
        }
        i += 1;
    }
    if let Some(h) = hi {
        out.push(h);
    }
    (out, i)
}

/// PDFium 的 `RunLengthDecode`：返回（数据，用掉的字节数）；解出来的长度溢出或超过上限时返回 `None`。
pub(crate) fn run_length_decode_pdfium(src: &[u8]) -> Option<(Vec<u8>, usize)> {
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
    let mut out = Vec::with_capacity(dest_size as usize);
    i = 0;
    while i < src.len() {
        let b = src[i];
        if b == 128 {
            break;
        }
        if b < 128 {
            let want = usize::from(b) + 1;
            let have = (src.len() - i - 1).min(want);
            out.extend_from_slice(&src[i + 1..i + 1 + have]);
            // 数据不够时补 0。
            out.extend(std::iter::repeat_n(0u8, want - have));
            i += want + 1;
        } else {
            let fill = src.get(i + 1).copied().unwrap_or(0);
            out.extend(std::iter::repeat_n(fill, 257 - usize::from(b)));
            i += 2;
        }
    }
    Some((out, (i + 1).min(src.len())))
}

/// PNG / TIFF 预测器反解（预测器 10–15）。预览器 2 只在图像流里出现，这里也一并处理。
fn unpredict(data: &[u8], predictor: i64, parms: Option<&PdfObject>) -> Vec<u8> {
    if predictor <= 1 {
        return data.to_vec();
    }
    let colors = parm_int(parms, b"Colors", 1).clamp(1, 32) as usize;
    let bpc = parm_int(parms, b"BitsPerComponent", 8).clamp(1, 16) as usize;
    let columns = parm_int(parms, b"Columns", 1).max(1) as usize;
    let row_bytes = columns
        .saturating_mul(colors)
        .saturating_mul(bpc)
        .div_ceil(8);
    let bpp = colors.saturating_mul(bpc).div_ceil(8).max(1);
    if predictor == 2 {
        if bpc != 8 {
            return data.to_vec();
        }
        let mut out = data.to_vec();
        for row in out.chunks_mut(row_bytes) {
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        return out;
    }
    let row_len = row_bytes + 1;
    if row_len == 0 {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row_bytes];
    for chunk in data.chunks(row_len) {
        // 最后一行不完整时照样处理已有的部分（PDFium 的 `PNG_Predictor` 也是这样）。
        let ft = chunk[0];
        let mut row = chunk[1..].to_vec();
        for i in 0..row.len() {
            let left = if i >= bpp { row[i - bpp] } else { 0 };
            let up = prev[i];
            let up_left = if i >= bpp { prev[i - bpp] } else { 0 };
            let v = match ft {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth(left, up, up_left),
                _ => 0,
            };
            row[i] = row[i].wrapping_add(v);
        }
        prev.clone_from(&row);
        out.extend_from_slice(&row);
    }
    out
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i32::from(a) + i32::from(b) - i32::from(c);
    let pa = (p - i32::from(a)).abs();
    let pb = (p - i32::from(b)).abs();
    let pc = (p - i32::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut hi: Option<u8> = None;
    for &b in data {
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

fn ascii85(data: &[u8]) -> Result<Vec<u8>, FilterError> {
    let mut out = Vec::with_capacity(data.len());
    let mut group = [0u8; 5];
    let mut n = 0;
    let mut i = 0;
    // 跳过可选的 `<~` 前缀。
    while i < data.len() && is_whitespace(data[i]) {
        i += 1;
    }
    if data.get(i) == Some(&b'<') && data.get(i + 1) == Some(&b'~') {
        i += 2;
    }
    while i < data.len() {
        let b = data[i];
        i += 1;
        if is_whitespace(b) {
            continue;
        }
        if b == b'~' {
            break;
        }
        if b == b'z' && n == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            return Err(FilterError::Ascii85);
        }
        group[n] = b - b'!';
        n += 1;
        if n == 5 {
            let mut v: u32 = 0;
            for &g in &group {
                v = v.wrapping_mul(85).wrapping_add(u32::from(g));
            }
            out.extend_from_slice(&v.to_be_bytes());
            n = 0;
        }
    }
    if n > 0 {
        if n == 1 {
            return Err(FilterError::Ascii85);
        }
        let mut v: u32 = 0;
        for (i, &g) in group.iter().enumerate() {
            let g = if i < n { g } else { 84 };
            v = v.wrapping_mul(85).wrapping_add(u32::from(g));
        }
        let bytes = v.to_be_bytes();
        out.extend_from_slice(&bytes[..n - 1]);
    }
    Ok(out)
}

fn run_length(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let len = data[i];
        i += 1;
        if len == 128 {
            break;
        }
        if len < 128 {
            let n = len as usize + 1;
            let end = (i + n).min(data.len());
            out.extend_from_slice(&data[i..end]);
            i = end;
        } else if i < data.len() {
            let n = 257 - len as usize;
            out.extend(std::iter::repeat_n(data[i], n));
            i += 1;
        }
    }
    out
}

fn lzw_decode(data: &[u8], early_change: bool) -> Result<Vec<u8>, FilterError> {
    const CLEAR: u16 = 256;
    const EOD: u16 = 257;
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = (0..256u16).map(|i| vec![i as u8]).collect();
    table.push(Vec::new());
    table.push(Vec::new());
    let mut width = 9u32;
    let mut prev: Option<u16> = None;
    let mut bitpos = 0usize;
    let mut bitbuf = 0u32;
    let mut nbits = 0u32;
    loop {
        while nbits < width {
            let byte = data.get(bitpos / 8).copied();
            bitpos += 8;
            bitbuf = (bitbuf << 8) | u32::from(byte.unwrap_or(0));
            nbits += 8;
            if byte.is_none() && nbits < width {
                return Ok(out);
            }
        }
        let code = ((bitbuf >> (nbits - width)) & ((1 << width) - 1)) as u16;
        nbits -= width;
        if code == CLEAR {
            table.truncate(258);
            width = 9;
            prev = None;
            continue;
        }
        if code == EOD {
            break;
        }
        let entry = if (code as usize) < table.len() {
            table[code as usize].clone()
        } else if let Some(p) = prev {
            let mut e = table[p as usize].clone();
            e.push(table[p as usize][0]);
            e
        } else {
            return Err(FilterError::Lzw);
        };
        if let Some(p) = prev {
            let mut e = table[p as usize].clone();
            e.push(entry[0]);
            if table.len() < 4096 {
                table.push(e);
            }
        }
        out.extend_from_slice(&entry);
        prev = Some(code);
        let limit = table.len() + usize::from(early_change);
        width = match limit {
            0..=511 => 9,
            512..=1023 => 10,
            1024..=2047 => 11,
            _ => 12,
        };
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::object::PdfDict;

    #[test]
    fn flate_roundtrip() {
        let data = b"BT /F1 12 Tf (hello) Tj ET".repeat(10);
        let packed = encode_flate(&data);
        assert_eq!(inflate(&packed).unwrap(), data);
    }

    #[test]
    fn png_up_predictor() {
        // 3 列、1 字节/列、Up 预测器：每行数据是 [滤 2, 1, 2, 3]，第 0 列的差值等于原值。
        let parms = PdfObject::Dict(PdfDict::from_entries([(
            b"Columns".to_vec(),
            PdfObject::Int(3),
        )]));
        let raw = [2u8, 1, 2, 3, 2, 1, 2, 3];
        let out = unpredict(&raw, 12, Some(&parms));
        assert_eq!(out, [1, 2, 3, 2, 4, 6]);
    }

    #[test]
    fn ascii_filters() {
        assert_eq!(ascii_hex(b"48 65 6c6c 6F>"), b"Hello");
        assert_eq!(ascii_hex(b"414>"), [0x41, 0x40]);
        let enc: Vec<u8> = b"Hello World!".to_vec();
        // "Hello World!" 的 ASCII85：87cURD]i,"Ebo80
        let decoded = ascii85(b"87cURD]i,\"Ebo80~>").unwrap();
        assert_eq!(decoded, enc);
        let used = run_length(&[2, b'a', b'b', b'c', 253, b'z', 128]);
        assert_eq!(used, b"abczzzz");
    }

    #[test]
    fn lzw_small() {
        // 256(Clear) 65 66 65 257(EOD)，9 位码。
        let codes: [u16; 5] = [256, 65, 66, 65, 257];
        let mut bits: Vec<bool> = Vec::new();
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
        assert_eq!(lzw_decode(&bytes, true).unwrap(), b"ABA");
    }

    #[test]
    fn unsupported_filter_is_error() {
        let f = PdfObject::Name(b"DCTDecode".to_vec());
        assert!(apply_filter_chain(b"x", Some(&f), None).is_err());
    }

    #[test]
    fn checked_decode_tells_whether_every_filter_was_applied() {
        let name = |n: &[u8]| PdfObject::Name(n.to_vec());
        let resolve = |o: &PdfObject| Some(o.clone());
        let check = |data: &[u8], filter: Option<&PdfObject>| {
            decode_like_pdfium_checked(data, filter, None, &resolve)
        };
        // 没有过滤器、认得的过滤器：完整。
        assert_eq!(check(b"q Q", None), (b"q Q".to_vec(), true));
        assert_eq!(
            check(b"712051>", Some(&name(b"ASCIIHexDecode"))),
            (b"q Q".to_vec(), true)
        );
        let packed = encode_flate(b"0 g");
        assert_eq!(
            check(&packed, Some(&name(b"FlateDecode"))),
            (b"0 g".to_vec(), true)
        );
        // 不认得的过滤器：PDFium 停在它前面，用到此为止解出来的数据（一步都没解时是原始字节）。
        assert_eq!(
            check(b"\x1b\x51", Some(&name(b"BrotliDecode"))),
            (b"\x1b\x51".to_vec(), false)
        );
        let chain = PdfObject::Array(vec![name(b"ASCIIHexDecode"), name(b"BrotliDecode")]);
        assert_eq!(check(b"1b51>", Some(&chain)), (b"\x1b\x51".to_vec(), false));
        // /Crypt 被跳过；/Filter 的写法不对、图像过滤器不在最后：整条按原始字节用。都不算完整。
        assert_eq!(
            check(b"q Q", Some(&name(b"Crypt"))),
            (b"q Q".to_vec(), false)
        );
        assert_eq!(
            check(b"q Q", Some(&PdfObject::Int(1))),
            (b"q Q".to_vec(), false)
        );
        let wrong_order = PdfObject::Array(vec![name(b"DCTDecode"), name(b"ASCIIHexDecode")]);
        assert_eq!(
            check(b"712051>", Some(&wrong_order)),
            (b"712051>".to_vec(), false)
        );
        // 有过滤器而什么都没解出来：PDFium 用原始字节，那不是真正的内容。
        assert_eq!(
            check(b">", Some(&name(b"ASCIIHexDecode"))),
            (b">".to_vec(), false)
        );
        // 空数据谈不上解码。
        assert_eq!(check(b"", Some(&name(b"BrotliDecode"))), (Vec::new(), true));
        // 不带“完整”标记的那个函数给的字节不变。
        assert_eq!(
            decode_like_pdfium(b"1b51>", Some(&chain), None, &resolve),
            b"\x1b\x51"
        );
    }
}
