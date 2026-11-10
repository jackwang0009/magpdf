//! 命名共享内存的瓦片槽位（《接口协议》3.3；V06 原型）。
//!
//! 名字 `Local\MagPDF-<会话 ID>-tiles`（`Local\` 命名空间只在当前登录会话内可见）。整块内存由引擎创建
//! （按 [`MAX_MAPPED_BYTES`] 申请，物理页按需提交），主进程用 `OpenFileMappingW` 打开并映射整块。
//!
//! 与规范的差距：映射对象的 DACL 由 [`security`] 显式给出——认证用户可打开（P0 原型为了便于测量与
//! 调试），当前用户与本地系统完全控制；`Local\` 命名空间把可见范围限制在当前登录会话。只允许当前
//! 用户的 SID 留到 P1 与引擎进程沙箱一起做。
//!
//! 布局：
//! ```text
//! 偏移 0             头部 64 字节：魔数 u64、槽位数 u32、槽位大小 u32、头部大小 u32、版本 u32
//! 偏移 64            槽位 0：代数 u32（原子）+ 保留 60 字节 + 512×512×4 字节 BGRA
//! 偏移 64 + 槽位大小  槽位 1 ……
//! ```
//!
//! 槽位的代数每被写入一次加 1，写入时先加 1 变奇数（正在写）、写完再加 1 变偶数（稳定），seqlock 的写法。
//! 读方先读代数，拷完像素再读一次：两次相等且为偶数才是完整瓦片。代数对不上时协议返回 410（错误码 3006，
//! `TileGenerationStale`），界面重新请求渲染。
//!
//! 所有 `unsafe` 都在 [`win32`] 里，这里只做布局计算与切片访问。

use std::time::Duration;

use thiserror::Error;

pub mod security;
pub mod win32;

/// 瓦片边长（设备像素）。
pub const TILE_SIZE: u32 = 512;
/// 槽位的像素区字节数：512×512×4。
pub const TILE_BYTES: usize = TILE_SIZE as usize * TILE_SIZE as usize * 4;
/// 槽位头部字节数（代数 + 保留），槽位从 64 的倍数开始。
pub const SLOT_HEADER_BYTES: usize = 64;
/// 一个槽位的总字节数。
pub const SLOT_BYTES: usize = SLOT_HEADER_BYTES + TILE_BYTES;
/// 映射头部的字节数。
pub const HEADER_BYTES: usize = 64;
/// 头部魔数 "MPDFTILE"。
pub const MAGIC: u64 = 0x454c_4954_4644_504d;
/// 头部版本。
pub const LAYOUT_VERSION: u32 = 1;
/// 默认槽位数（《接口协议》待定项 I2）。
pub const DEFAULT_SLOTS: u32 = 128;
/// 映射的整块字节数（256 个槽位）：引擎按它申请，主进程按它映射；实际槽位数以头部为准。
///
/// 映射长度必须与创建时一致（长度超过原映射时 `MapViewOfFile` 失败，而打开时还读不到头部），所以固定为
/// 上限值。`CreateFileMappingW` 只申请大小上限，物理内存与页文件按写入的页提交，没写过的槽位不占内存：
/// 实测 256 槽映射的空闲占用几乎为零（见 P0 报告 V06 的 I2 一节）。
pub const MAX_MAPPED_BYTES: usize = 256 * 1024 * 1024;

/// 共享内存的错误。
#[derive(Debug, Error)]
pub enum ShmError {
    #[error("Windows API 调用失败：{0}")]
    Os(#[from] std::io::Error),
    #[error("共享内存的头部不合法（魔数或版本不对）")]
    BadHeader,
    #[error("槽位 {slot} 越界（共 {count} 个）")]
    SlotOutOfRange { slot: u32, count: u32 },
    #[error("瓦片代数不符：期望 {expected}，当前 {actual}")]
    Generation { expected: u32, actual: u32 },
    #[error("只读视图不能写")]
    ReadOnly,
    #[error("像��长度必须是 512×512×4 = {TILE_BYTES} 字节")]
    PixelLength,
}

/// 布局参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub slots: u32,
}

impl Layout {
    pub fn new(slots: u32) -> Self {
        Self { slots }
    }

    /// 整块映射的字节数：头部 + 槽位数 × 槽位大小，向上按 64 KB 对齐。
    pub fn total_bytes(&self) -> usize {
        let raw = HEADER_BYTES + SLOT_BYTES * self.slots as usize;
        raw.div_ceil(0x1_0000) * 0x1_0000
    }

    /// 槽位 `slot` 的起始偏移。
    pub fn slot_offset(&self, slot: u32) -> Option<usize> {
        (slot < self.slots).then(|| HEADER_BYTES + SLOT_BYTES * slot as usize)
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::new(DEFAULT_SLOTS)
    }
}

/// 共享内存的名字：`Local\MagPDF-<会话 ID>-tiles`（人读形式）。
pub fn shm_name(session_id: &str) -> String {
    format!(r"Local\MagPDF-{session_id}-tiles")
}

/// 文件映射的名字（`CreateFileMappingW` / `OpenFileMappingW` 的 `lpName`，不带命名空间前缀）。
pub fn mapping_name(session_id: &str) -> String {
    format!("MagPDF-{session_id}-tiles")
}

/// 本机物理内存（MB），供 I2 的槽位建议使用。
pub fn total_memory_mb() -> u64 {
    win32::total_memory_bytes() / (1024 * 1024)
}

/// 引擎侧：创建共享内存，写头部，返回读写视图。
///
/// 一次会话只调用一次：Win32 的 `CreateFileMappingW` 遇到同名映射会打开已有句柄并获得
/// `ERROR_ACCESS_DENIED`（它要求写权限）。引擎重启时沿用同一会话的映射，跳过创建。
pub fn create(session_id: &str, layout: Layout) -> Result<MappedRegion, ShmError> {
    let mut view = win32::View::create(&mapping_name(session_id), MAX_MAPPED_BYTES)?;
    {
        let header = &mut view.bytes_mut()[..HEADER_BYTES];
        // 引擎重启后复用同一块内存时，先让版本失效，再重写其余字段，最后写版本。
        header[20..24].copy_from_slice(&0u32.to_le_bytes());
        win32::fence_release();
        header[0..8].copy_from_slice(&MAGIC.to_le_bytes());
        header[8..12].copy_from_slice(&layout.slots.to_le_bytes());
        header[12..16].copy_from_slice(&(SLOT_BYTES as u32).to_le_bytes());
        header[16..20].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
        win32::fence_release();
        header[20..24].copy_from_slice(&LAYOUT_VERSION.to_le_bytes());
    }
    Ok(MappedRegion {
        view,
        layout,
        writable: true,
    })
}

/// 主进程侧：打开已有的共享内存，只读映射，并校验头部。
pub fn open(session_id: &str) -> Result<MappedRegion, ShmError> {
    // 映射长度必须等于创建时的整块大小（大于原映射时 `MapViewOfFile` 失败），而打开时还读不到头部。
    // 因此按"固定上限大小"映射：引擎创建时也按同样大小的整块 mmap 文件映射申请（`CreateFileMappingW`
    // 只申请上限，物理页按需提交），主进程只按头部给出的槽位数访问。
    let view = win32::View::open_read(&mapping_name(session_id), MAX_MAPPED_BYTES)?;
    let head = view.bytes();
    if head.len() < HEADER_BYTES {
        return Err(ShmError::BadHeader);
    }
    let magic = u64::from_le_bytes(head[0..8].try_into().unwrap_or_default());
    let slots = u32::from_le_bytes(head[8..12].try_into().unwrap_or_default());
    let slot_bytes = u32::from_le_bytes(head[12..16].try_into().unwrap_or_default());
    let version = u32::from_le_bytes(head[20..24].try_into().unwrap_or_default());
    if magic != MAGIC || version != LAYOUT_VERSION || slot_bytes as usize != SLOT_BYTES {
        return Err(ShmError::BadHeader);
    }
    let layout = Layout::new(slots);
    Ok(MappedRegion {
        view,
        layout,
        writable: false,
    })
}

/// 等共享内存出现（引擎建好之后）；超时报错。
pub async fn wait_for_mapping(session_id: &str, timeout: Duration) -> Result<String, ShmError> {
    let started = std::time::Instant::now();
    loop {
        if win32::View::exists(&mapping_name(session_id)) {
            return Ok(shm_name(session_id));
        }
        if started.elapsed() > timeout {
            return Err(ShmError::Os(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("等待共享内存 {} 超时", shm_name(session_id)),
            )));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 一块已映射的共享内存。
pub struct MappedRegion {
    view: win32::View,
    layout: Layout,
    writable: bool,
}

impl std::fmt::Debug for MappedRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedRegion")
            .field("len", &self.view.len())
            .field("slots", &self.layout.slots)
            .field("writable", &self.writable)
            .finish()
    }
}

impl MappedRegion {
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// 映射的整块字节数（引擎按 [`MAX_MAPPED_BYTES`] 申请）。
    pub fn len(&self) -> usize {
        self.view.len()
    }

    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    fn offset(&self, slot: u32) -> Result<usize, ShmError> {
        self.layout
            .slot_offset(slot)
            .ok_or(ShmError::SlotOutOfRange {
                slot,
                count: self.layout.slots,
            })
    }

    /// 槽位当前的代数。
    pub fn generation(&self, slot: u32) -> Result<u32, ShmError> {
        let offset = self.offset(slot)?;
        Ok(win32::load_generation(self.view.bytes(), offset))
    }

    /// 读出槽位的像素（拷贝出 512×512×4 字节）。`expected` 与当前代数不符时返回 [`ShmError::Generation`]。
    pub fn read_tile(&self, slot: u32, expected: u32) -> Result<Vec<u8>, ShmError> {
        let offset = self.offset(slot)?;
        let bytes = self.view.bytes();
        let mut out = vec![0u8; TILE_BYTES];
        for _ in 0..8 {
            let before = win32::load_generation(bytes, offset);
            if before != expected {
                return Err(ShmError::Generation {
                    expected,
                    actual: before,
                });
            }
            if before % 2 == 1 {
                // 写入方正在写，等一拍再看。
                std::hint::spin_loop();
                continue;
            }
            out.copy_from_slice(&bytes[offset + SLOT_HEADER_BYTES..offset + SLOT_BYTES]);
            win32::fence_acquire();
            let after = win32::load_generation(bytes, offset);
            if after == before {
                return Ok(out);
            }
        }
        Err(ShmError::Generation {
            expected,
            actual: win32::load_generation(bytes, offset),
        })
    }

    /// 把像素写进槽位，返回写入后的代数（偶数）。引擎侧专用。
    pub fn write_tile(&mut self, slot: u32, pixels: &[u8]) -> Result<u32, ShmError> {
        if !self.writable {
            return Err(ShmError::ReadOnly);
        }
        if pixels.len() != TILE_BYTES {
            return Err(ShmError::PixelLength);
        }
        let offset = self.offset(slot)?;
        let bytes = self.view.bytes_mut();
        let _odd = win32::store_generation(bytes, offset);
        bytes[offset + SLOT_HEADER_BYTES..offset + SLOT_BYTES].copy_from_slice(pixels);
        win32::fence_release();
        Ok(win32::store_generation(bytes, offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_defaults_match_the_protocol() {
        let l = Layout::default();
        assert_eq!(l.slots, 128);
        assert_eq!(SLOT_BYTES, 64 + 512 * 512 * 4);
        let mb = l.total_bytes() / (1024 * 1024);
        assert!((127..=129).contains(&mb), "{mb} MB");
        assert_eq!(l.slot_offset(0), Some(HEADER_BYTES));
        assert_eq!(l.slot_offset(128), None);
    }

    #[test]
    fn create_write_read_roundtrip() {
        let id = format!("test{}", std::process::id());
        let mut region = create(&id, Layout::new(4)).unwrap();
        assert!(region.is_writable());
        let mut pixels = vec![0u8; TILE_BYTES];
        pixels[0] = 7;
        pixels[TILE_BYTES - 1] = 9;
        let gen1 = region.write_tile(2, &pixels).unwrap();
        assert_eq!(gen1, 2);

        let mut reader = open(&id).unwrap();
        assert!(!reader.is_writable());
        assert_eq!(reader.len(), MAX_MAPPED_BYTES);
        assert_eq!(reader.layout().slots, 4);
        let got = reader.read_tile(2, gen1).unwrap();
        assert_eq!(got[0], 7);
        assert_eq!(got[TILE_BYTES - 1], 9);
        assert!(matches!(
            reader.read_tile(2, gen1 - 1),
            Err(ShmError::Generation { .. })
        ));
        assert!(matches!(
            reader.read_tile(4, 0),
            Err(ShmError::SlotOutOfRange { .. })
        ));
        assert!(matches!(
            reader.write_tile(0, &pixels),
            Err(ShmError::ReadOnly)
        ));

        let gen2 = region.write_tile(2, &pixels).unwrap();
        assert_eq!(gen2, gen1 + 2);
        assert!(matches!(
            reader.read_tile(2, gen1),
            Err(ShmError::Generation { .. })
        ));
    }
}
