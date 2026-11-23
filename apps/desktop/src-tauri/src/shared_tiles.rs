//! 取瓦片的另一条路：WebView2 的 SharedBuffer（方案 3.1 预留的做法，V06 报告 11.7）。
//!
//! 《接口协议》3.3 的做法是界面经自定义协议 `magpdf://tile/…` 取像素。V06 量到这一段要 5.4–5.6 ms，
//! 其中约 4.5 ms 花在 WebView2 接收 1 MB 的响应体上。这里换成：主进程建几块 WebView2 的共享缓冲区，
//! 交给页面一次；取瓦片时界面调命令 `fetch_tile_shared`，主进程把共享内存里的像素直接拷进其中一块，
//! 界面从那块缓冲区里读，不经过响应体。主进程转发 `render.tileReady` 时也会先拷好，事件里带上位置，
//! 界面连命令都不用调（`app.rs` 的 `push_tile`）。
//!
//! 默认不开：设了环境变量 `MAGPDF_TILE_TRANSPORT=shared-buffer` 才用，协议的写法没有改，换不换等协议
//! 评审（C18）。WebView2 运行时太旧、没有这个接口时，界面退回自定义协议。
//!
//! 每块缓冲区的布局：开头 [`HEADER_BYTES`] 字节是两个 `u32` 标记（写入前标记、写完后标记），后面是一块
//! 瓦片的 BGRA 像素。主进程写之前先改“写入前”，拷完再改“写完后”，两次写同一个新值；界面先看“写完后”
//! 等于命令返回的值，拷出像素，再看“写入前”还是这个值。中间被下一次取瓦片盖掉的话两次对不上，界面重取。

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use magpdf_ipc::shm::{ShmError, TILE_BYTES};
use serde::Serialize;
use tauri::WebviewWindow;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY, ICoreWebView2_17, ICoreWebView2Environment12,
    ICoreWebView2SharedBuffer,
};
use windows::core::{HSTRING, Interface};

/// 缓冲区开头的两个标记。
pub const HEADER_BYTES: usize = 16;
/// 缓冲区的块数。界面同时取的瓦片不多于这个数时，一块在界面读完之前不会被重用。
pub const BUFFER_COUNT: usize = 16;

const ENV_SWITCH: &str = "MAGPDF_TILE_TRANSPORT";

/// 有没有打开这条路。
pub fn enabled() -> bool {
    std::env::var(ENV_SWITCH).is_ok_and(|v| v == "shared-buffer")
}

/// 一次取瓦片的结果：像素在第 `index` 块里，两个标记都应当是 `tag`。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedTile {
    pub index: usize,
    pub tag: u32,
}

/// 一块缓冲区。`buffer` 只在窗口的线程上用（建、交给页面）；`base` 是它映射进本进程的地址，
/// 缓冲区对象不释放，地址一直有效。
struct Slot {
    buffer: ICoreWebView2SharedBuffer,
    base: usize,
    /// 写这一块时拿着，两次取瓦片不会同时写同一块。
    writing: Mutex<()>,
    tag: AtomicU32,
}

// SAFETY: `buffer` 是 COM 对象，只在窗口线程（`with_webview` 的回调里）调它的方法；别的线程只碰 `base`
// 指向的映射内存，用 `writing` 串起来。缓冲区对象跟着 `SharedTiles` 活到进程结束。
unsafe impl Send for Slot {}
// SAFETY: 同上，共享引用下只读 `base`、`tag`（原子）和拿 `writing` 锁，不碰 `buffer`。
unsafe impl Sync for Slot {}

/// 主进程这一侧的全部缓冲区。第一次交给页面时建。
#[derive(Default)]
pub struct SharedTiles {
    slots: Mutex<Vec<Arc<Slot>>>,
    next: AtomicUsize,
}

impl SharedTiles {
    /// 把缓冲区交给页面（还没有就先建），返回块数。页面每次加载都要调一次：重新加载之后页面那一侧的
    /// 缓冲区就没了。WebView2 太旧、没有这个接口时返回错误，界面退回自定义协议。
    pub async fn attach(self: Arc<Self>, window: &WebviewWindow) -> Result<usize, String> {
        let (done, wait) = tokio::sync::oneshot::channel();
        window
            .with_webview(move |webview| {
                let _ = done.send(self.attach_on_ui_thread(&webview));
            })
            .map_err(|e| format!("取不到 WebView：{e}"))?;
        wait.await.map_err(|_| "窗口线程没有回话".to_owned())?
    }

    fn attach_on_ui_thread(
        &self,
        webview: &tauri::webview::PlatformWebview,
    ) -> Result<usize, String> {
        let failed = |what: &str, e: windows::core::Error| format!("{what}：{e}");
        // SAFETY: 在窗口线程上调 WebView2 的 COM 接口；接口指针来自 Tauri，有效。
        unsafe {
            let core = webview
                .controller()
                .CoreWebView2()
                .map_err(|e| failed("取不到 CoreWebView2", e))?;
            let core: ICoreWebView2_17 = core
                .cast()
                .map_err(|e| failed("WebView2 运行时没有 SharedBuffer 接口", e))?;
            let environment: ICoreWebView2Environment12 = webview
                .environment()
                .cast()
                .map_err(|e| failed("WebView2 运行时没有 SharedBuffer 接口", e))?;
            // 建缓冲区不拿着表的锁：转发引擎事件的任务（`app.rs` 的 `push_tile`）也要拿这把锁，建 16 块的时候
            // 一直拿着，启动时所有事件都得等它。
            let existing = self
                .slots
                .lock()
                .map_err(|_| "缓冲区表坏了".to_owned())?
                .len();
            let mut created = Vec::new();
            for _ in existing..BUFFER_COUNT {
                let buffer = environment
                    .CreateSharedBuffer((HEADER_BYTES + TILE_BYTES) as u64)
                    .map_err(|e| failed("建不了共享缓冲区", e))?;
                let mut base: *mut u8 = std::ptr::null_mut();
                buffer
                    .Buffer(&mut base)
                    .map_err(|e| failed("取不到共享缓冲区的地址", e))?;
                created.push(Arc::new(Slot {
                    buffer,
                    base: base as usize,
                    writing: Mutex::new(()),
                    tag: AtomicU32::new(0),
                }));
            }
            let slots = {
                let mut slots = self.slots.lock().map_err(|_| "缓冲区表坏了".to_owned())?;
                slots.extend(created);
                slots.clone()
            };
            for (index, slot) in slots.iter().enumerate() {
                let data = HSTRING::from(format!("{{\"magpdfTileBuffer\":{index}}}"));
                core.PostSharedBufferToScript(
                    &slot.buffer,
                    COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
                    &data,
                )
                .map_err(|e| failed("共享缓冲区交不给页面", e))?;
            }
            Ok(slots.len())
        }
    }

    /// 把一块瓦片的像素拷进下一块缓冲区。`read` 把像素写进给它的切片（就是共享内存的读法）。
    pub fn fill(
        &self,
        read: impl FnOnce(&mut [u8]) -> Result<(), ShmError>,
    ) -> Result<SharedTile, FillError> {
        let slot = {
            let slots = self.slots.lock().map_err(|_| FillError::NotAttached)?;
            if slots.is_empty() {
                return Err(FillError::NotAttached);
            }
            let index = self.next.fetch_add(1, Ordering::Relaxed) % slots.len();
            (index, Arc::clone(&slots[index]))
        };
        let (index, slot) = slot;
        let _writing = slot.writing.lock().map_err(|_| FillError::NotAttached)?;
        let tag = slot.tag.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let base = slot.base as *mut u8;
        // SAFETY: `base` 指向 HEADER_BYTES + TILE_BYTES 字节的映射内存，缓冲区对象没有释放；这一块只有拿着
        // `writing` 的线程写。页面那一侧是只读的。
        unsafe {
            let marks = base.cast::<AtomicU32>();
            (*marks).store(tag, Ordering::Release);
            let pixels = std::slice::from_raw_parts_mut(base.add(HEADER_BYTES), TILE_BYTES);
            let result = read(pixels);
            std::sync::atomic::fence(Ordering::Release);
            (*marks.add(1)).store(tag, Ordering::Release);
            result.map_err(FillError::Shm)?;
        }
        Ok(SharedTile { index, tag })
    }
}

/// [`SharedTiles::fill`] 失败的原因。
#[derive(Debug)]
pub enum FillError {
    /// 缓冲区还没有交给页面。
    NotAttached,
    Shm(ShmError),
}
