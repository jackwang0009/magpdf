//! 非阻塞的帧读写（Windows 命名管道是半双工的：读和写必须在同一个事件循环里，不能用一个阻塞的读
//! future 独占连接）。主进程和引擎共用。
//!
//! [`FrameReader`] 累积收到的字节、切出完整的帧；[`write_all`] 把一帧写出去，写不进去时返回
//! `WouldBlock`，由调用方等可写再重试。

use std::io;

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};

use crate::messages::MAX_FRAME_BYTES;

/// 管道连接的任一端（主进程是客户端，引擎是服务端），两种端都支持 `try_read` / `try_write`。
pub trait PipeIo {
    fn read_some(&self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_some(&self, buf: &[u8]) -> io::Result<usize>;
}

macro_rules! impl_pipe_io {
    ($t:ty) => {
        impl PipeIo for $t {
            fn read_some(&self, buf: &mut [u8]) -> io::Result<usize> {
                self.try_read(buf)
            }
            fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
                self.try_write(buf)
            }
        }
    };
}

impl_pipe_io!(NamedPipeServer);
impl_pipe_io!(NamedPipeClient);

/// 按帧切分的读缓冲。
#[derive(Debug, Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读一次管道，尽量多读；`WouldBlock` 视为没读到，连接关闭时返回错误。
    pub fn pump(&mut self, pipe: &impl PipeIo) -> io::Result<()> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match pipe.read_some(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if n < chunk.len() {
                        return Ok(());
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// 取出一个完整的帧正文（不含长度前缀）。
    pub fn next_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("帧长 {len} 超过上限"),
            ));
        }
        let end = 4 + len;
        if self.buf.len() < end {
            return Ok(None);
        }
        let body = self.buf[4..end].to_vec();
        self.buf.drain(..end);
        Ok(Some(body))
    }
}

/// 写一帧直到写完；写不进去时返回 `WouldBlock`，调用方等可写后重试（已经写出的字节由 `Written` 记录）。
pub fn write_frame(pipe: &impl PipeIo, written: &mut usize, framed: &[u8]) -> io::Result<()> {
    while *written < framed.len() {
        match pipe.write_some(&framed[*written..]) {
            Ok(0) => {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            Ok(n) => *written += n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            Err(e) => return Err(e),
        }
    }
    *written = 0;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frames_across_reads() {
        let mut reader = FrameReader::new();
        let a = crate::frame::encode_body(b"{\"a\":1}");
        let b = crate::frame::encode_body(b"{\"b\":2}");
        let mut all = a.clone();
        all.extend_from_slice(&b);
        // 一次喂一半，再喂另一半。
        let (first, second) = all.split_at(5);
        reader.buf.extend_from_slice(first);
        assert!(reader.next_frame().unwrap().is_none());
        reader.buf.extend_from_slice(second);
        assert_eq!(reader.next_frame().unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(reader.next_frame().unwrap().unwrap(), b"{\"b\":2}");
        assert!(reader.next_frame().unwrap().is_none());
    }
}
