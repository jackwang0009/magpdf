//! 几个集成测试文件共用的辅助函数。
// 每个测试文件各自编译一份，不是每个函数都会被每个文件用到。
#![allow(dead_code)]

use std::io::Read;

/// 取出保存结果里所有流的解码后字节。PDFium 保存时把没有过滤器的流统一 Flate 压缩，
/// 写成 `stream\r\n…\r\nendstream`；这里按该格式扫描并解压，解压失败的按原样返回。
pub fn saved_streams(saved: &[u8]) -> Vec<Vec<u8>> {
    const OPEN: &[u8] = b"stream\r\n";
    const CLOSE: &[u8] = b"\r\nendstream";
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(rel) = saved[pos..].windows(OPEN.len()).position(|w| w == OPEN) {
        let start = pos + rel;
        let data_start = start + OPEN.len();
        let is_endstream = start >= 3 && &saved[start - 3..start] == b"end";
        if is_endstream {
            pos = data_start;
            continue;
        }
        let Some(len) = saved[data_start..]
            .windows(CLOSE.len())
            .position(|w| w == CLOSE)
        else {
            break;
        };
        let raw = &saved[data_start..data_start + len];
        let mut inflated = Vec::new();
        let decoded = match flate2::read::ZlibDecoder::new(raw).read_to_end(&mut inflated) {
            Ok(_) => inflated,
            Err(_) => raw.to_vec(),
        };
        out.push(decoded);
        pos = data_start + len + CLOSE.len();
    }
    out
}
