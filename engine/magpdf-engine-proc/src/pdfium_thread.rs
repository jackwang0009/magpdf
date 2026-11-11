//! PDFium 专用线程（方案 3.3）。
//!
//! PDFium 不是线程安全的，所有调用都在一个专用线程上串行执行；通信线程只把任务投递进来，PDFium 忙时
//! 仍能应答 `session.ping`。线程持有 PDFium 的 [`Library`] 与已打开的文档，并负责分配 docId。
//! 每份文档是一个编辑会话（[`Session`]）：对象编号、撤销栈和文档版本号都在里面。
//!
//! 任务队列空下来的时候，线程给显示过的页面做保真校验（方案 2.3“提前进行”），一次一页，
//! 做完一页就回头看有没有新任务。结论经 [`Notice`] 告诉通信线程。

use std::collections::{HashSet, VecDeque};

use anyhow::{Context, Result};
use magpdf_engine::edit::{Change, EditError, HitMode, ObjectId, ObjectSummary, Session};
use magpdf_ipc::messages::DocInfo;
use pdfium_sys::{
    Bitmap, Document, Library, Matrix, PageSurface, PixelRect, RenderFlags, render_rect,
};
use tokio::sync::{mpsc, oneshot};

/// PDFium 线程处理的错误。
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("文档 {doc_id} 不存在")]
    NoDocument { doc_id: u32 },
    #[error("页面 {page_id} 不存在（文档 {doc_id}）")]
    NoPage { doc_id: u32, page_id: u32 },
    /// 打开失败。`with_password` 记下请求里有没有带密码，用来区分“需要密码”和“密码错误”。
    #[error("打开文档失败：{error}")]
    Open {
        error: pdfium_sys::Error,
        with_password: bool,
    },
    #[error("渲染失败：{0}")]
    Render(String),
    #[error("文档现在是第 {current} 版，请求基于第 {base} 版")]
    VersionConflict { current: u64, base: u64 },
    #[error(transparent)]
    Edit(#[from] EditError),
}

/// 一条修改文档的操作。
#[derive(Debug, Clone)]
pub enum EditOp {
    Transform {
        ids: Vec<ObjectId>,
        matrix: Matrix,
    },
    /// 只换填充的颜色，不动不透明度。
    FillRgb {
        ids: Vec<ObjectId>,
        r: u8,
        g: u8,
        b: u8,
    },
    Delete {
        ids: Vec<ObjectId>,
    },
    Undo,
    Redo,
}

/// 一页的保真校验状态（方案 2.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFidelity {
    /// 还没有校验。
    Pending,
    /// 可以修改原有对象。
    Ok,
    /// 只能叠加内容；带着原因。
    OverlayOnly(&'static str),
}

impl PageFidelity {
    fn of(known: Option<Option<&'static str>>) -> Self {
        match known {
            None => Self::Pending,
            Some(None) => Self::Ok,
            Some(Some(reason)) => Self::OverlayOnly(reason),
        }
    }
}

/// PDFium 线程主动告诉通信线程的事，通信线程转成事件发给主进程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// 一页的保真校验有了结论。状态不会是 `Pending`。
    Fidelity {
        doc_id: u32,
        page_id: u32,
        fidelity: PageFidelity,
    },
}

/// `doc.snapshot` 里一页的信息。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageShape {
    pub index: usize,
    /// 显示尺寸（pt），已经按页面旋转换好宽高。
    pub width: f32,
    pub height: f32,
    /// 现在知道的保真校验状态；查它不会触发校验。
    pub fidelity: PageFidelity,
    /// 要加载页面才拿得到的部分；列全部页面时不取。
    pub detail: Option<PageDetail>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageDetail {
    /// 页面旋转，90° 的倍数：0–3。
    pub rotation: i32,
    /// 可见区域（PDF 用户空间）。
    pub visible: Option<pdfium_sys::Rect>,
}

type Reply<T> = oneshot::Sender<std::result::Result<T, EngineError>>;

/// 对 PDFium 线程的请求。
pub enum Job {
    Open {
        path: String,
        password: Option<String>,
        reply: Reply<DocInfo>,
    },
    /// 渲染一块瓦片，返回 512×512 BGRA 和渲染时的文档版本号。
    RenderTile {
        doc_id: u32,
        page_id: u32,
        scale: f32,
        x: u32,
        y: u32,
        reply: Reply<(Bitmap, u64)>,
    },
    /// 整页像素尺寸，供恢复后界面重建视图。
    PageSurface {
        doc_id: u32,
        page_id: u32,
        scale: f32,
        reply: Reply<(u32, u32)>,
    },
    ListDocs {
        reply: oneshot::Sender<Vec<DocInfo>>,
    },
    /// 关闭文档；应答是否找到了这份文档。
    CloseDoc {
        doc_id: u32,
        reply: oneshot::Sender<bool>,
    },
    /// 文档版本号和页面信息。`page_ids` 为空时列出全部页面的显示尺寸，不加载页面，大文档也很快；
    /// 给了页就只列这几页，加载页面取旋转和可见区域。
    Snapshot {
        doc_id: u32,
        page_ids: Option<Vec<u32>>,
        reply: Reply<(u64, Vec<PageShape>)>,
    },
    /// 一页上的全部对象，按层级从下到上。
    Objects {
        doc_id: u32,
        page_id: u32,
        reply: Reply<Vec<ObjectSummary>>,
    },
    HitTest {
        doc_id: u32,
        page_id: u32,
        x: f32,
        y: f32,
        mode: HitMode,
        reply: Reply<Vec<ObjectSummary>>,
    },
    /// 修改命令、撤销或重做。`base_version` 有值时先核对文档版本。
    Edit {
        doc_id: u32,
        base_version: Option<u64>,
        op: EditOp,
        reply: Reply<Change>,
    },
    /// 马上校验这几页（不等空闲）。每一页的结论都发一条 [`Notice::Fidelity`]，已经校验过的页也发。
    CheckFidelity {
        doc_id: u32,
        page_ids: Vec<u32>,
        reply: Reply<()>,
    },
    Shutdown,
}

/// PDFium 线程的句柄。
#[derive(Debug, Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Job>,
}

impl EngineHandle {
    /// 启动专用线程并等 PDFium 初始化完成。另一个返回值是线程主动发出来的通知（[`Notice`]）。
    pub fn spawn() -> Result<(Self, mpsc::UnboundedReceiver<Notice>)> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let (notices, notices_rx) = mpsc::unbounded_channel::<Notice>();
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
                // Library 与本线程同寿命。把它泄漏成 'static 借用，文档句柄才能和它放进同一个状态里；
                // 进程退出时由系统回收，不再调用 FPDF_DestroyLibrary。
                let lib: &'static Library = Box::leak(Box::new(lib));
                let mut state = PdfiumState {
                    docs: Vec::new(),
                    next_doc_id: 1,
                    lib,
                    notices,
                    background: VecDeque::new(),
                    queued: HashSet::new(),
                };
                // 专用线程里同步取任务；通信线程不依赖这条线程的响应速度来处理心跳。
                loop {
                    let job = match rx.try_recv() {
                        Ok(job) => job,
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                        Err(mpsc::error::TryRecvError::Empty) => {
                            // 队列空了：做一项后台工作，做完马上回头看有没有新任务。
                            if state.background_step() {
                                continue;
                            }
                            match rx.blocking_recv() {
                                Some(job) => job,
                                None => break,
                            }
                        }
                    };
                    if matches!(job, Job::Shutdown) {
                        break;
                    }
                    state.handle(job);
                }
            })
            .context("启动 PDFium 线程失败")?;
        ready_rx.recv().context("PDFium 线程没有启动")??;
        Ok((Self { tx }, notices_rx))
    }

    fn send(&self, job: Job) -> std::result::Result<(), EngineError> {
        self.tx.send(job).map_err(|_| Self::closed())
    }

    fn closed() -> EngineError {
        EngineError::Render("PDFium 线程已退出".into())
    }

    /// 投递一个带应答的任务，等 PDFium 线程做完。
    async fn ask<T>(
        &self,
        job: impl FnOnce(Reply<T>) -> Job,
    ) -> std::result::Result<T, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.send(job(reply))?;
        rx.await.map_err(|_| Self::closed())?
    }

    pub async fn render_tile(
        &self,
        doc_id: u32,
        page_id: u32,
        scale: f32,
        x: u32,
        y: u32,
    ) -> std::result::Result<(Bitmap, u64), EngineError> {
        self.ask(|reply| Job::RenderTile {
            doc_id,
            page_id,
            scale,
            x,
            y,
            reply,
        })
        .await
    }

    pub async fn open(
        &self,
        path: String,
        password: Option<String>,
    ) -> std::result::Result<DocInfo, EngineError> {
        self.ask(|reply| Job::Open {
            path,
            password,
            reply,
        })
        .await
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
        self.ask(|reply| Job::PageSurface {
            doc_id,
            page_id,
            scale,
            reply,
        })
        .await
    }

    pub async fn list_docs(&self) -> Vec<DocInfo> {
        let (reply, rx) = oneshot::channel();
        if self.send(Job::ListDocs { reply }).is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// 关闭文档。返回 `false` 表示没有这份文档。
    pub async fn close_doc(&self, doc_id: u32) -> bool {
        let (reply, rx) = oneshot::channel();
        if self.send(Job::CloseDoc { doc_id, reply }).is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub async fn snapshot(
        &self,
        doc_id: u32,
        page_ids: Option<Vec<u32>>,
    ) -> std::result::Result<(u64, Vec<PageShape>), EngineError> {
        self.ask(|reply| Job::Snapshot {
            doc_id,
            page_ids,
            reply,
        })
        .await
    }

    pub async fn objects(
        &self,
        doc_id: u32,
        page_id: u32,
    ) -> std::result::Result<Vec<ObjectSummary>, EngineError> {
        self.ask(|reply| Job::Objects {
            doc_id,
            page_id,
            reply,
        })
        .await
    }

    pub async fn hit_test(
        &self,
        doc_id: u32,
        page_id: u32,
        point: (f32, f32),
        mode: HitMode,
    ) -> std::result::Result<Vec<ObjectSummary>, EngineError> {
        self.ask(|reply| Job::HitTest {
            doc_id,
            page_id,
            x: point.0,
            y: point.1,
            mode,
            reply,
        })
        .await
    }

    pub async fn edit(
        &self,
        doc_id: u32,
        base_version: Option<u64>,
        op: EditOp,
    ) -> std::result::Result<Change, EngineError> {
        self.ask(|reply| Job::Edit {
            doc_id,
            base_version,
            op,
            reply,
        })
        .await
    }

    /// 马上校验这几页的保真度。结论不在返回值里，而是每页一条 [`Notice::Fidelity`]。
    pub async fn check_fidelity(
        &self,
        doc_id: u32,
        page_ids: Vec<u32>,
    ) -> std::result::Result<(), EngineError> {
        self.ask(|reply| Job::CheckFidelity {
            doc_id,
            page_ids,
            reply,
        })
        .await
    }

    pub fn shutdown(&self) {
        let _ = self.send(Job::Shutdown);
    }
}

struct PdfiumState {
    docs: Vec<OpenDoc>,
    next_doc_id: u32,
    lib: &'static Library,
    notices: mpsc::UnboundedSender<Notice>,
    /// 等着在空闲时做保真校验的页：（docId，页序号），按页面第一次显示的先后。
    background: VecDeque<(u32, usize)>,
    /// 进过 `background` 的页。每页只排一次：校验出错（没有结论）的页不会每渲染一块瓦片就再排一次。
    queued: HashSet<(u32, usize)>,
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
                let rendered = self.render_tile(doc_id, page_id, scale, x, y);
                if rendered.is_ok() {
                    // 这一页显示出来了：等队列空了，提前把它的保真校验做掉（方案 2.3）。
                    self.queue_background_check(doc_id, page_id as usize);
                }
                let _ = reply.send(rendered);
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
            Job::CloseDoc { doc_id, reply } => {
                let before = self.docs.len();
                self.docs.retain(|d| d.doc_id != doc_id);
                self.background.retain(|(doc, _)| *doc != doc_id);
                self.queued.retain(|(doc, _)| *doc != doc_id);
                let _ = reply.send(self.docs.len() != before);
            }
            Job::Snapshot {
                doc_id,
                page_ids,
                reply,
            } => {
                let _ = reply.send(self.snapshot(doc_id, page_ids.as_deref()));
            }
            Job::Objects {
                doc_id,
                page_id,
                reply,
            } => {
                let _ = reply.send(
                    self.page_of(doc_id, page_id)
                        .and_then(|(d, page)| Ok(d.session.objects(page)?)),
                );
            }
            Job::HitTest {
                doc_id,
                page_id,
                x,
                y,
                mode,
                reply,
            } => {
                let _ = reply.send(
                    self.page_of(doc_id, page_id)
                        .and_then(|(d, page)| Ok(d.session.hit_test(page, x, y, mode)?)),
                );
            }
            Job::Edit {
                doc_id,
                base_version,
                op,
                reply,
            } => {
                let outcome = self.edit(doc_id, base_version, op);
                // 修改命令会先校验它碰到的页；刚得出的结论（不管命令成没成）也要让界面知道。
                self.report_new_verdicts(doc_id);
                let _ = reply.send(outcome);
            }
            Job::CheckFidelity {
                doc_id,
                page_ids,
                reply,
            } => {
                let _ = reply.send(self.check_fidelity(doc_id, &page_ids));
            }
            Job::Shutdown => {}
        }
    }

    /// 没有别的任务时做一项后台工作：给一个显示过、还没有结论的页面做保真校验。
    /// 返回这一趟有没有做事；没有事可做时返回 `false`，线程去等下一个任务。
    fn background_step(&mut self) -> bool {
        while let Some((doc_id, page)) = self.background.pop_front() {
            // 文档已经关了，或者修改命令已经先校验过这一页
            let Ok(d) = self.find_mut(doc_id) else {
                continue;
            };
            if d.session.known_fidelity(page).is_some() {
                continue;
            }
            if let Err(e) = d.session.check_fidelity(page) {
                tracing::warn!("后台保真校验没有做成：{e}");
            }
            self.report_new_verdicts(doc_id);
            return true;
        }
        false
    }

    /// 页面显示过了（渲染过瓦片）：还没有校验结论的话，排到后台去校验。
    fn queue_background_check(&mut self, doc_id: u32, page: usize) {
        let pending = self
            .find(doc_id)
            .is_ok_and(|d| d.session.known_fidelity(page).is_none());
        if pending && self.queued.insert((doc_id, page)) {
            self.background.push_back((doc_id, page));
        }
    }

    /// 把一份文档里刚得出的校验结论发出去。
    fn report_new_verdicts(&mut self, doc_id: u32) {
        let Ok(d) = self.find_mut(doc_id) else {
            return;
        };
        for (page, verdict) in d.session.take_new_verdicts() {
            let _ = self.notices.send(Notice::Fidelity {
                doc_id,
                page_id: u32::try_from(page).unwrap_or(u32::MAX),
                fidelity: PageFidelity::of(Some(verdict)),
            });
        }
    }

    fn check_fidelity(
        &mut self,
        doc_id: u32,
        page_ids: &[u32],
    ) -> std::result::Result<(), EngineError> {
        // 先确认页都在，再动手：页号不对的请求一页也不校验、一条结论也不发。
        for &page_id in page_ids {
            self.page_of(doc_id, page_id)?;
        }
        for &page_id in page_ids {
            let (d, page) = self.page_of(doc_id, page_id)?;
            let verdict = d.session.check_fidelity(page)?;
            // 这一页的结论在下面直接发，不再走“刚得出的结论”那一路，免得发两遍
            d.session.take_new_verdicts();
            let _ = self.notices.send(Notice::Fidelity {
                doc_id,
                page_id,
                fidelity: PageFidelity::of(Some(verdict)),
            });
        }
        Ok(())
    }

    fn open(
        &mut self,
        path: String,
        password: Option<&str>,
    ) -> std::result::Result<DocInfo, EngineError> {
        let doc = Document::load_file(self.lib, std::path::Path::new(&path), password).map_err(
            |error| EngineError::Open {
                error,
                with_password: password.is_some(),
            },
        )?;
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
            session: Session::from_document(self.lib, doc),
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

    fn find_mut(&mut self, doc_id: u32) -> std::result::Result<&mut OpenDoc, EngineError> {
        self.docs
            .iter_mut()
            .find(|d| d.doc_id == doc_id)
            .ok_or(EngineError::NoDocument { doc_id })
    }

    fn snapshot(
        &self,
        doc_id: u32,
        page_ids: Option<&[u32]>,
    ) -> std::result::Result<(u64, Vec<PageShape>), EngineError> {
        let d = self.find(doc_id)?;
        let doc = d.session.document();
        let failed = |e: pdfium_sys::Error| EngineError::Render(e.to_string());
        let fidelity = |index: usize| PageFidelity::of(d.session.known_fidelity(index));
        let pages = match page_ids {
            None => (0..doc.page_count())
                .map(|index| {
                    let (width, height) = doc.page_size(index).map_err(failed)?;
                    Ok(PageShape {
                        index,
                        width,
                        height,
                        fidelity: fidelity(index),
                        detail: None,
                    })
                })
                .collect::<std::result::Result<Vec<_>, EngineError>>()?,
            Some(ids) => ids
                .iter()
                .map(|&page_id| {
                    let index = page_id as usize;
                    if index >= doc.page_count() {
                        return Err(EngineError::NoPage { doc_id, page_id });
                    }
                    let geometry = doc.page_geometry(index).map_err(failed)?;
                    Ok(PageShape {
                        index,
                        width: geometry.width,
                        height: geometry.height,
                        fidelity: fidelity(index),
                        detail: Some(PageDetail {
                            rotation: geometry.rotation,
                            visible: geometry.bounds,
                        }),
                    })
                })
                .collect::<std::result::Result<Vec<_>, EngineError>>()?,
        };
        Ok((d.session.version(), pages))
    }

    /// 找到文档并确认页码在范围内。原型里 `pageId` 就是页序号。
    fn page_of(
        &mut self,
        doc_id: u32,
        page_id: u32,
    ) -> std::result::Result<(&mut OpenDoc, usize), EngineError> {
        let d = self.find_mut(doc_id)?;
        let count = u32::try_from(d.session.page_count()).unwrap_or(0);
        if page_id >= count {
            return Err(EngineError::NoPage { doc_id, page_id });
        }
        Ok((d, page_id as usize))
    }

    fn edit(
        &mut self,
        doc_id: u32,
        base_version: Option<u64>,
        op: EditOp,
    ) -> std::result::Result<Change, EngineError> {
        let session = &mut self.find_mut(doc_id)?.session;
        if let Some(base) = base_version
            && base != session.version()
        {
            return Err(EngineError::VersionConflict {
                current: session.version(),
                base,
            });
        }
        Ok(match op {
            EditOp::Transform { ids, matrix } => session.transform(&ids, matrix)?,
            EditOp::FillRgb { ids, r, g, b } => session.set_fill_rgb(&ids, r, g, b)?,
            EditOp::Delete { ids } => session.delete(&ids)?,
            EditOp::Undo => session.undo()?,
            EditOp::Redo => session.redo()?,
        })
    }

    fn render_tile(
        &mut self,
        doc_id: u32,
        page_id: u32,
        scale: f32,
        x: u32,
        y: u32,
    ) -> std::result::Result<(Bitmap, u64), EngineError> {
        let (d, page) = self.page_of(doc_id, page_id)?;
        let version = d.session.version();
        let page = d
            .session
            .document()
            .page(page)
            .map_err(|e| EngineError::Render(e.to_string()))?;
        let surface = PageSurface::new(&page, scale);
        let bitmap = render_rect(
            &page,
            surface,
            PixelRect::tile(x, y),
            RenderFlags::NONE,
            0xFFFF_FFFF,
        )
        .map_err(|e| EngineError::Render(e.to_string()))?;
        Ok((bitmap, version))
    }

    fn page_surface(
        &self,
        doc_id: u32,
        page_id: u32,
        scale: f32,
    ) -> std::result::Result<(u32, u32), EngineError> {
        let d = self.find(doc_id)?;
        let doc = d.session.document();
        if page_id >= u32::try_from(doc.page_count()).unwrap_or(0) {
            return Err(EngineError::NoPage { doc_id, page_id });
        }
        let page = doc
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
    session: Session<'static>,
}

fn info(d: &OpenDoc) -> DocInfo {
    let doc = d.session.document();
    let (mut page_width, mut page_height) = (0.0, 0.0);
    if let Ok(page) = doc.page(0) {
        page_width = page.width();
        page_height = page.height();
    }
    DocInfo {
        doc_id: d.doc_id,
        path: d.path.clone(),
        title: d.title.clone(),
        version: u32::try_from(d.session.version()).unwrap_or(u32::MAX),
        page_count: u32::try_from(doc.page_count()).unwrap_or(0),
        page_width,
        page_height,
    }
}
