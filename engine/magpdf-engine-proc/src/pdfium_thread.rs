//! PDFium 专用线程（方案 3.3）。
//!
//! PDFium 不是线程安全的，所有调用都在一个专用线程上串行执行；通信线程只把任务投递进来，PDFium 忙时
//! 仍能应答 `session.ping`。线程持有 PDFium 的 [`Library`] 与已打开的文档，并负责分配 docId。

use anyhow::{Context, Result};
use magpdf_ipc::messages::DocInfo;
use pdfium_sys::{Bitmap, Document, Library, PageSurface, PixelRect, RenderFlags, render_rect};
use tokio::sync::{mpsc, oneshot};

/// PDFium 线程处理的错误。
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("文档 {doc_id} 不存在")]
    NoDocument { doc_id: u32 },
    #[error("页面 {page_id} 不存在（文档 {doc_id}）")]
    NoPage { doc_id: u32, page_id: u32 },
    #[error("打开文档失败：{0}")]
    Open(String),
    #[error("渲染失败：{0}")]
    Render(String),
}

/// 对 PDFium 线程的请求。
pub enum Job {
    Open {
        path: String,
        password: Option<String>,
        reply: oneshot::Sender<std::result::Result<DocInfo, EngineError>>,
    },
    /// 渲染一块瓦片，返回 512×512 BGRA。
    RenderTile {
        doc_id: u32,
        page_id: u32,
        scale: f32,
        x: u32,
        y: u32,
        reply: oneshot::Sender<std::result::Result<Bitmap, EngineError>>,
    },
    /// 整页像素尺寸，供恢复后界面重建视图。
    PageSurface {
        doc_id: u32,
        page_id: u32,
        scale: f32,
        reply: oneshot::Sender<std::result::Result<(u32, u32), EngineError>>,
    },
    ListDocs {
        reply: oneshot::Sender<Vec<DocInfo>>,
    },
    #[expect(dead_code, reason = "V06 原型还没有 doc.close，文档只开不关")]
    CloseDoc {
        doc_id: u32,
    },
    Shutdown,
}

/// PDFium 线程的句柄。
#[derive(Debug, Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Job>,
}

impl EngineHandle {
    /// 启动专用线程并等 PDFium 初始化完成。
    pub fn spawn() -> Result<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        std::thread::Builder::new()
            .name("pdfium".to_owned())
            .spawn(move || {
                let lib = match Library::init() {
                    Ok(lib) => {
                        let _ = ready_tx.send(Ok(()));
                        lib
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(anyhow::anyhow!("初始化 PDFium 失败：{e}")));
                        return;
                    }
                };
                let mut state = PdfiumState {
                    docs: Vec::new(),
                    next_doc_id: 1,
                    _lib: lib,
                };
                // 专用线程里同步取任务；通信线程不依赖这条线程的响应速度来处理心跳。
                while let Some(job) = rx.blocking_recv() {
                    if matches!(job, Job::Shutdown) {
                        break;
                    }
                    state.handle(job);
                }
            })
            .context("启动 PDFium 线程失败")?;
        ready_rx.recv().context("PDFium 线程没有启动")??;
        Ok(Self { tx })
    }

    fn send(&self, job: Job) -> std::result::Result<(), EngineError> {
        self.tx
            .send(job)
            .map_err(|_| EngineError::Render("PDFium 线程已退出".into()))
    }

    fn closed() -> EngineError {
        EngineError::Render("PDFium 线程已退出".into())
    }

    pub async fn render_tile(
        &self,
        doc_id: u32,
        page_id: u32,
        scale: f32,
        x: u32,
        y: u32,
    ) -> std::result::Result<Bitmap, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::RenderTile {
            doc_id,
            page_id,
            scale,
            x,
            y,
            reply,
        })?;
        rx.await.map_err(|_| Self::closed())?
    }

    pub async fn open(
        &self,
        path: String,
        password: Option<String>,
    ) -> std::result::Result<DocInfo, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::Open {
            path,
            password,
            reply,
        })?;
        rx.await.map_err(|_| Self::closed())?
    }

    /// 同步等待打开完成（引擎启动时做恢复，通信线程还没起来）。
    ///
    /// 可能在 tokio 运行时里被调用（`server::run` → `Engine::start`），因此不能直接
    /// `blocking_recv`；有运行时时用 `block_in_place` 包一层。
    pub fn open_blocking(&self, path: &str) -> std::result::Result<DocInfo, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::Open {
            path: path.to_owned(),
            password: None,
            reply,
        })?;
        let recv = if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| rx.blocking_recv())
        } else {
            rx.blocking_recv()
        };
        recv.map_err(|_| Self::closed())?
    }

    pub async fn page_surface(
        &self,
        doc_id: u32,
        page_id: u32,
        scale: f32,
    ) -> std::result::Result<(u32, u32), EngineError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::PageSurface {
            doc_id,
            page_id,
            scale,
            reply,
        })?;
        rx.await.map_err(|_| Self::closed())?
    }

    pub async fn list_docs(&self) -> Vec<DocInfo> {
        let (reply, rx) = oneshot::channel();
        if self.send(Job::ListDocs { reply }).is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// 同步取已打开文档的快照（引擎启动时用，通信线程还没起来）。
    pub fn list_docs_snapshot(&self) -> Vec<DocInfo> {
        let (reply, rx) = oneshot::channel();
        if self.send(Job::ListDocs { reply }).is_err() {
            return Vec::new();
        }
        let recv = if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| rx.blocking_recv())
        } else {
            rx.blocking_recv()
        };
        recv.unwrap_or_default()
    }

    #[expect(dead_code, reason = "V06 原型还没有 doc.close，文档只开不关")]
    pub fn close_doc(&self, doc_id: u32) {
        let _ = self.send(Job::CloseDoc { doc_id });
    }

    pub fn shutdown(&self) {
        let _ = self.send(Job::Shutdown);
    }
}

struct PdfiumState {
    docs: Vec<OpenDoc>,
    next_doc_id: u32,
    // `docs` 声明的顺序在 `_lib` 之前：析构时文档先于 Library 释放。
    _lib: Library,
}

impl PdfiumState {
    fn handle(&mut self, job: Job) {
        match job {
            Job::Open {
                path,
                password,
                reply,
            } => {
                let _ = reply.send(self.open(path, password.as_deref()));
            }
            Job::RenderTile {
                doc_id,
                page_id,
                scale,
                x,
                y,
                reply,
            } => {
                let _ = reply.send(self.render_tile(doc_id, page_id, scale, x, y));
            }
            Job::PageSurface {
                doc_id,
                page_id,
                scale,
                reply,
            } => {
                let _ = reply.send(self.page_surface(doc_id, page_id, scale));
            }
            Job::ListDocs { reply } => {
                let _ = reply.send(self.docs.iter().map(info).collect());
            }
            Job::CloseDoc { doc_id } => self.docs.retain(|d| d.doc_id != doc_id),
            Job::Shutdown => {}
        }
    }

    fn open(
        &mut self,
        path: String,
        password: Option<&str>,
    ) -> std::result::Result<DocInfo, EngineError> {
        // 文档借用的 Library 由本结构体持有，与线程同寿命；`docs` 在 `_lib` 之前析构。
        let lib: &Library = &self._lib;
        // SAFETY: `lib` 指向 self._lib，在本线程整个生命周期内有效，且 `docs` 先于它析构；
        // 延长出的 &'static 只存在本线程内，不跨线程传递。
        let lib: &'static Library = unsafe { &*std::ptr::from_ref(lib) };
        let doc = Document::load_file(lib, std::path::Path::new(&path), password)
            .map_err(|e| EngineError::Open(e.to_string()))?;
        let doc_id = self.next_doc_id;
        self.next_doc_id += 1;
        let title = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let entry = OpenDoc {
            doc_id,
            path,
            title,
            doc,
        };
        let info = info(&entry);
        self.docs.push(entry);
        Ok(info)
    }

    fn find(&self, doc_id: u32) -> std::result::Result<&OpenDoc, EngineError> {
        self.docs
            .iter()
            .find(|d| d.doc_id == doc_id)
            .ok_or(EngineError::NoDocument { doc_id })
    }

    fn render_tile(
        &self,
        doc_id: u32,
        page_id: u32,
        scale: f32,
        x: u32,
        y: u32,
    ) -> std::result::Result<Bitmap, EngineError> {
        let d = self.find(doc_id)?;
        let count = u32::try_from(d.doc.page_count()).unwrap_or(0);
        if page_id >= count {
            return Err(EngineError::NoPage { doc_id, page_id });
        }
        let page = d
            .doc
            .page(page_id as usize)
            .map_err(|e| EngineError::Render(e.to_string()))?;
        let surface = PageSurface::new(&page, scale);
        render_rect(
            &page,
            surface,
            PixelRect::tile(x, y),
            RenderFlags::NONE,
            0xFFFF_FFFF,
        )
        .map_err(|e| EngineError::Render(e.to_string()))
    }

    fn page_surface(
        &self,
        doc_id: u32,
        page_id: u32,
        scale: f32,
    ) -> std::result::Result<(u32, u32), EngineError> {
        let d = self.find(doc_id)?;
        if page_id >= u32::try_from(d.doc.page_count()).unwrap_or(0) {
            return Err(EngineError::NoPage { doc_id, page_id });
        }
        let page = d
            .doc
            .page(page_id as usize)
            .map_err(|e| EngineError::Render(e.to_string()))?;
        let s = PageSurface::new(&page, scale);
        Ok((s.width, s.height))
    }
}

#[derive(Debug)]
struct OpenDoc {
    doc_id: u32,
    path: String,
    title: String,
    doc: Document<'static>,
}

fn info(d: &OpenDoc) -> DocInfo {
    let (mut page_width, mut page_height) = (0.0, 0.0);
    if let Ok(page) = d.doc.page(0) {
        page_width = page.width();
        page_height = page.height();
    }
    DocInfo {
        doc_id: d.doc_id,
        path: d.path.clone(),
        title: d.title.clone(),
        version: 0,
        page_count: u32::try_from(d.doc.page_count()).unwrap_or(0),
        page_width,
        page_height,
    }
}
