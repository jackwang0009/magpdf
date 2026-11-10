//! 共享内存的 Windows 接口（`windows` crate，MIT OR Apache-2.0）。本 crate 里只有这个模块出现 `unsafe`。
//!
//! `unsafe` 的前提写在各自的 `// SAFETY:` 注释里。调用方遵守的约定：
//! - 映射视图在 `View` 析构前一直有效，`bytes()` 的切片寿命借用 `View`；
//! - 交给 Win32 的名字字符串在调用期间以 NUL 结尾、有效；
//! - 只有引擎侧持有读写视图，主进程侧只调用 `View::open_read` 得到只读视图。

use std::io;
use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::core::{HSTRING, PCWSTR};

/// 一块文件映射视图。析构时解除映射并关闭映射句柄。
#[derive(Debug)]
pub struct View {
    mapping: HANDLE,
    view: *mut u8,
    len: usize,
}

impl View {
    /// 映射视图的字节切片。内容可能被另一个进程同时修改，调用方用代数核对一致性。
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: MapViewOfFile 返回的视图覆盖 len 字节，在 self 存活期间有效（Drop 里才解除映射）。
        unsafe { std::slice::from_raw_parts(self.view, self.len) }
    }

    /// 映射视图的可变切片（只有本进程创建的读写视图才调用）。
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: 同 `bytes`；&mut self 保证本进程内同时只有一个可变引用。
        unsafe { std::slice::from_raw_parts_mut(self.view, self.len) }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 创建名为 `name` 的文件映射（由分页文件支持）并按 `len` 字节映射整块。
    pub fn create(name: &str, len: usize) -> io::Result<Self> {
        let name_w = HSTRING::from(name);
        let size = u64::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "共享内存长度过大"))?;
        let size_high = u32::try_from(size >> 32).unwrap_or(0);
        let size_low = size as u32;
        // SAFETY: 句柄为 INVALID_HANDLE_VALUE（由分页文件支持的内存映射）；名字在调用期间有效；
        // 长度为 len 字节。安全描述符显式给出"认证用户"可打开映射（沿用默认 DACL 时，另一个进程
        // 以只读方式打开 4 MB 以上的映射会得到 ERROR_ACCESS_DENIED，见 P0 报告 V06）。
        let mapping = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&crate::shm::security::security_attributes()),
                PAGE_READWRITE,
                size_high,
                size_low,
                PCWSTR(name_w.as_ptr()),
            )
        };
        let mapping = mapping.map_err(|_| io::Error::last_os_error())?;
        if mapping.is_invalid() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: mapping 是刚创建的读写映射；映射整块（偏移 0，长度 len）。
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if view.Value.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: mapping 由本函数创建，尚未关闭。
            unsafe {
                let _ = CloseHandle(mapping);
            }
            return Err(e);
        }
        Ok(Self {
            mapping,
            view: view.Value.cast::<u8>(),
            len,
        })
    }
}

impl View {
    /// 打开名为 `name` 的已有文件映射，映射整块。
    ///
    /// 必须映射整块：映射长度超过原映射时 `MapViewOfFile` 返回空指针（这里表现为 `ERROR_ACCESS_DENIED`），
    /// 因此调用方按头部给出的布局映射，不能按预留上限映射。访问权限用 `FILE_MAP_ALL_ACCESS`：映射对象的
    /// DACL 只授予 FILE_MAP_READ 时，请求读写也不会降低映射视图本身的读写属性（视图仍只在本进程内可见）。
    pub fn open_read(name: &str, len: usize) -> io::Result<Self> {
        let name_w = HSTRING::from(name);
        // SAFETY: 名字在调用期间有效。
        let mapping =
            unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name_w.as_ptr())) }
                .map_err(|_| io::Error::last_os_error())?;
        // SAFETY: 映射整块（偏移 0，长度 len，len 是创建方给出的整块长度）。
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if view.Value.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: mapping 由本函数打开，尚未关闭。
            unsafe {
                let _ = CloseHandle(mapping);
            }
            return Err(e);
        }
        Ok(Self {
            mapping,
            view: view.Value.cast::<u8>(),
            len,
        })
    }

    /// 映射是否存在（主进程轮询引擎是否已建好共享内存）。
    pub fn exists(name: &str) -> bool {
        let name_w = HSTRING::from(name);
        // SAFETY: 名字在调用期间有效。
        match unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(name_w.as_ptr())) } {
            Ok(handle) => {
                // SAFETY: 句柄由 OpenFileMappingW 打开，尚未关闭。
                unsafe {
                    let _ = CloseHandle(handle);
                }
                true
            }
            Err(_) => false,
        }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: view 由 MapViewOfFile 返回，只解除一次；mapping 由本对象持有，只关闭一次。
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.view.cast(),
            });
            let _ = CloseHandle(self.mapping);
        }
    }
}

// SAFETY: 视图只包含映射地址与句柄；跨线程使用由调用方保证不同时读写。
unsafe impl Send for View {}

/// 系统随机数的填充（`BCryptGenRandom`，系统首选随机源）。
pub fn system_random(buf: &mut [u8]) -> io::Result<()> {
    // SAFETY: buf 有效且可写；使用系统首选随机源时不需要算法句柄。
    let status = unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_err() {
        return Err(io::Error::other(format!(
            "BCryptGenRandom 失败：{status:?}"
        )));
    }
    Ok(())
}

/// 物理内存总量（字节）。
pub fn total_memory_bytes() -> u64 {
    let mut status = MEMORYSTATUSEX {
        dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: status 的 dwLength 已按结构体大小填好，指针有效。
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok.is_err() { 0 } else { status.ullTotalPhys }
}

fn generation_at(bytes: &[u8], offset: usize) -> &AtomicU32 {
    // SAFETY: 槽位头部的起始偏移是 64 的倍数；u32 只要求 4 字节对齐，满足；视图存活期间有效。
    unsafe { &*bytes.as_ptr().add(offset).cast::<AtomicU32>() }
}

/// 读代数（Acquire）。
pub fn load_generation(bytes: &[u8], offset: usize) -> u32 {
    generation_at(bytes, offset).load(Ordering::Acquire)
}

/// 代数加 1，返回加之后的值。
pub fn store_generation(bytes: &[u8], offset: usize) -> u32 {
    generation_at(bytes, offset).fetch_add(1, Ordering::SeqCst) + 1
}

/// 发布屏障：写完像素后让另一个进程读到完整数据。
pub fn fence_release() {
    std::sync::atomic::fence(Ordering::Release);
}

/// 获取屏障：读完像素后确认没有被改写。
pub fn fence_acquire() {
    std::sync::atomic::fence(Ordering::Acquire);
}
