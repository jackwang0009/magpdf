//! 流过滤器：只实现内容流和常见结构里会遇到的几种。
//!
//! 解码失败的流一律报错，由上层当作"流损坏"处理；不在这里做任何容错猜测。

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
        if chunk.len() < row_len {
            break;
        }
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
}
