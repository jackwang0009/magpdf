//! 管道服务端：会话建立、心跳、文档、瓦片渲染与取回、基准方法（《接口协议》第 4、6.3 节）。
//!
//! 一条连接一个读循环 + 一个写循环。读循环收到请求后按方法派发：不碰 PDFium 的（`session.*`、
//! `bench.*`、`tile.fetch`）在通信线程上直接应答；渲染请求立即应答 `requestIds`，瓦片在后台渲染完成后
//! 以 `render.tileReady` 事件送达。

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use magpdf_engine::edit::{EditError, HitMode, ObjectId, ObjectSummary};
use magpdf_ipc::frame::{self, Incoming};
use magpdf_ipc::messages::{
    self, BenchEngineParams, Color, ColorSpace, DocChanged, DocCloseParams, DocInfo, DocOpenParams,
    DocSnapshot, DocSnapshotParams, EchoParams, EditDeleteParams, EditResult, EditSetStyleParams,
    EditTransformParams, ErrorCode, Fidelity, FidelityCheckParams, FidelityResult, HelloParams,
    HelloResult, HistoryParams, JsonValue, LayerMode, OBJ_LIST_MAX, ObjHitTestParams,
    ObjListParams, PageChange, PageInfo, PingParams, RenderTilesParams, RenderTilesResult, Request,
    RequestId, Response, RoundtripParams, RoundtripResult, RpcError, StallParams, TileCoord,
    TileFetchParams, TileFetchResult, method,
};
use magpdf_ipc::shm::{self, Layout, MappedRegion};
use magpdf_ipc::{PROTOCOL_VERSION, pipe, pump};
use pdfium_sys::Matrix;
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::mpsc;

use crate::pdfium_thread::{EditOp, EngineHandle, Layer, Notice, PageFidelity};
use crate::recovery::{DocRecovery, RecoveryFile};
use crate::slots::{SlotTable, TileKey};

/// 引擎配置（命令行参数）。
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub pipe: String,
    pub session: String,
    pub recovery: Option<PathBuf>,
    pub slots: u32,
    /// 打开只给测试用的方法（`bench.stall`）。
    pub test_hooks: bool,
}

impl EngineConfig {
    /// 解析 `--pipe <名字> --session <ID> [--recovery <路径>] [--slots <n>] [--test-hooks]`。
    pub fn from_args(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut pipe = None;
        let mut session = None;
        let mut recovery = None;
        let mut slots = shm::DEFAULT_SLOTS;
        let mut test_hooks = false;
        let mut it = args;
        while let Some(flag) = it.next() {
            let mut value = || -> Result<String> {
                it.next().with_context(|| format!("参数 {flag} 缺少取值"))
            };
            match flag.as_str() {
                "--pipe" => pipe = Some(value()?),
                "--session" => session = Some(value()?),
                "--recovery" => recovery = Some(PathBuf::from(value()?)),
                "--slots" => slots = value()?.parse().context("槽位数不是整数")?,
                "--test-hooks" => test_hooks = true,
                other => anyhow::bail!("未知参数 {other}"),
            }
        }
        Ok(Self {
            pipe: pipe.context("缺少 --pipe")?,
            session: session.context("缺少 --session")?,
            recovery,
            slots,
            test_hooks,
        })
    }
}

/// 后台任务发给主进程的消息：要等 PDFium 线程的请求的响应，或事件（通知）。
/// 不等 PDFium 的请求由读循环直接应答，不走这里。
#[derive(Debug)]
pub enum Outgoing {
    Response(Response),
    Event(String, JsonValue),
}

/// 引擎会话的全部状态，通信线程与 PDFium 线程的交接点。
pub struct Engine {
    pub pipe: String,
    pub session_id: String,
    pub engine: EngineHandle,
    /// PDFium 线程主动发出来的通知（后台保真校验的结论）。连接建立时取走，转成事件发给主进程；
    /// 一个引擎只服务一条连接，所以只取一次。
    pub notices: Mutex<Option<mpsc::UnboundedReceiver<Notice>>>,
    pub shm: Mutex<MappedRegion>,
    pub slots: Mutex<SlotTable>,
    pub recovery: Mutex<RecoveryFile>,
    /// 已打开文档的路径（docId → 路径）。恢复文件按路径记账，通信线程查它就行，不用去等 PDFium 线程。
    pub doc_paths: Mutex<HashMap<u32, String>>,
    /// 引擎启动时从恢复文件读回的文档，在 `session.hello` 的返回值里告诉主进程。
    pub restored: Mutex<Vec<DocInfo>>,
    /// 渲染期间的测量开关：为 true 时把"渲染 + 写入共享内存"的耗时记进 `bench_samples`。
    pub measure: AtomicBool,
    pub bench_samples: Mutex<Vec<f64>>,
    pub shutdown: AtomicBool,
    pub next_request_id: AtomicU32,
    pub pdfium_revision: String,
    /// 是否认只给测试用的方法。
    pub test_hooks: bool,
}

impl Engine {
    /// 往指定连接发一条事件（渲染任务完成时用）。
    pub fn event_to<T: serde::Serialize>(
        &self,
        out: &mpsc::UnboundedSender<Outgoing>,
        name: &str,
        params: &T,
    ) {
        if let Some(value) = frame::json_value(params) {
            let _ = out.send(Outgoing::Event(name.to_owned(), value));
        }
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("pipe", &self.pipe)
            .field("session", &self.session_id)
            .field("slots", &self.slots.lock().map(|s| s.slots()))
            .finish_non_exhaustive()
    }
}

/// 运行引擎，直到 `session.shutdown`，或者主进程那一头的连接断开。
///
/// 一个引擎只服务启动它的那个主进程的一条连接：管道名里有主进程的 PID 和随机串，主进程每次都是
/// 换一个名字重新启动引擎，不会回头再连。所以连接一断（主进程崩溃、被结束，或者它决定换掉这个引擎）
/// 引擎就退出，不留下来等。
pub async fn run(config: EngineConfig) -> Result<i32> {
    let engine = Arc::new(Engine::start(config)?);
    let pipe_name = engine.pipe.clone();
    let restored = engine.restored.lock().map(|r| r.len()).unwrap_or(0);
    tracing::info!(
        pid = std::process::id(),
        pipe = %pipe_name,
        pdfium = %engine.pdfium_revision,
        restored,
        "engine started"
    );
    let mut server = pipe::create_server(&pipe_name)?;
    if pipe::accept(&mut server).await?.is_some() {
        serve_connection(engine.clone(), server).await;
        if !engine.shutdown.load(Ordering::SeqCst) {
            tracing::info!("主进程的连接断了，引擎退出");
        }
    }
    engine.engine.shutdown();
    Ok(0)
}

impl Engine {
    fn start(config: EngineConfig) -> Result<Self> {
        let recovery_path = config
            .recovery
            .clone()
            .unwrap_or_else(|| PathBuf::from("magpdf-recovery.json"));
        let mut recovery = RecoveryFile::load(&recovery_path);
        // 共享内存由引擎创建（引擎重启时沿用同一会话的映射）。
        let region = shm::create(&config.session, Layout::new(config.slots))?;
        let (engine, notices) = EngineHandle::spawn()?;
        // 恢复：重新打开恢复文件里的文档。
        let mut restored = Vec::new();
        for (index, doc) in recovery.state().docs.clone().iter().enumerate() {
            match engine.open_blocking(&doc.path) {
                Ok(info) => restored.push(info),
                // 日志里不写文件路径（《工程规范》4）。
                Err(e) => tracing::warn!("恢复文件里的第 {} 份文档打不开：{e}", index + 1),
            }
        }
        if !restored.is_empty() {
            let _ = recovery.save();
        }
        let doc_paths = restored
            .iter()
            .map(|d| (d.doc_id, d.path.clone()))
            .collect();
        Ok(Self {
            pipe: config.pipe,
            session_id: config.session,
            engine,
            notices: Mutex::new(Some(notices)),
            shm: Mutex::new(region),
            slots: Mutex::new(SlotTable::new(config.slots)),
            recovery: Mutex::new(recovery),
            doc_paths: Mutex::new(doc_paths),
            restored: Mutex::new(restored),
            measure: AtomicBool::new(false),
            bench_samples: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
            next_request_id: AtomicU32::new(1),
            pdfium_revision: format!("pdfium {}", pdfium_sys::Library::linked_version()),
            test_hooks: config.test_hooks,
        })
    }
}

/// 读循环能立刻应答的方法：不等 PDFium 线程。其余方法（包括不认识的）交给本连接的工作任务。
fn answered_inline(method_name: &str) -> bool {
    matches!(
        method_name,
        method::SESSION_HELLO
            | method::SESSION_PING
            | method::SESSION_SHUTDOWN
            | method::RENDER_TILES
            | method::RENDER_CANCEL
            | method::TILE_FETCH
            | method::BENCH_ECHO
            | method::BENCH_STALL
    )
}

/// 把一条待发消息编码成帧，排进写队列。
fn enqueue(msg: &Outgoing, pending: &mut VecDeque<Vec<u8>>) {
    let framed = match msg {
        Outgoing::Response(resp) => frame::encode(resp),
        Outgoing::Event(name, params) => frame::encode(&frame::notification(name, params)),
    };
    match framed {
        Ok(bytes) => pending.push_back(bytes),
        Err(e) => tracing::error!("编码消息失败：{e}"),
    }
}

/// 服务一条连接。命名管道是半双工的，读写放在同一个事件循环里（pump）：可读时把完整帧取出来处理，
/// 可写时把排队的响应与事件写出去。
///
/// 不等 PDFium 的请求（见 [`answered_inline`]）在读循环里直接应答：渲染请求立即应答 requestId，瓦片
/// 完成时以 `render.tileReady` 事件发出。要等 PDFium 线程的请求（`doc.open` 等）按到达顺序交给一个
/// 工作任务，响应经 `out_tx` 送回；读循环不等它们，所以 PDFium 忙的时候 `session.ping` 照样有应答。
async fn serve_connection(engine: Arc<Engine>, conn: NamedPipeServer) {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Outgoing>();
    let (slow_tx, mut slow_rx) = mpsc::unbounded_channel::<Request>();
    let worker = tokio::spawn({
        let engine = engine.clone();
        let out = out_tx.clone();
        async move {
            while let Some(req) = slow_rx.recv().await {
                let resp = dispatch(&engine, req.id, &req.method, req.params.as_ref(), &out).await;
                if let Some(resp) = resp
                    && out.send(Outgoing::Response(resp)).is_err()
                {
                    break;
                }
            }
        }
    });
    // PDFium 线程主动发出来的通知，转成事件。
    let notices = engine.notices.lock().ok().and_then(|mut n| n.take());
    let forwarder = notices.map(|mut notices| {
        let out = out_tx.clone();
        tokio::spawn(async move {
            while let Some(notice) = notices.recv().await {
                if let Some(event) = notice_event(notice)
                    && out.send(event).is_err()
                {
                    break;
                }
            }
        })
    });
    let mut reader = pump::FrameReader::new();
    let mut pending: VecDeque<Vec<u8>> = VecDeque::new();
    let mut written = 0usize;
    let mut closed = false;
    loop {
        // 1. 后台任务送来的响应与事件，编码后排队。
        while let Ok(msg) = out_rx.try_recv() {
            enqueue(&msg, &mut pending);
        }
        // 2. 尽量写。
        while let Some(front) = pending.front() {
            match pump::write_frame(&conn, &mut written, front) {
                Ok(()) => {
                    pending.pop_front();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    tracing::warn!("写管道失败，连接关闭：{e}");
                    closed = true;
                    break;
                }
            }
        }
        if closed {
            break;
        }
        // 收到 session.shutdown 之后，等它的应答写出去再退出。
        if engine.shutdown.load(Ordering::SeqCst) && pending.is_empty() {
            break;
        }
        // 3. 尽量读，取完整帧。
        if let Err(e) = reader.pump(&conn) {
            tracing::info!("主进程断开连接：{e}");
            break;
        }
        while let Ok(Some(body)) = reader.next_frame() {
            let Ok(value) = serde_json::from_slice(&body) else {
                tracing::warn!("收到无法解析的帧，忽略");
                continue;
            };
            match Incoming::from_value(value) {
                Ok(Incoming::Request(req)) if answered_inline(&req.method) => {
                    let resp =
                        dispatch(&engine, req.id, &req.method, req.params.as_ref(), &out_tx).await;
                    if let Some(resp) = resp {
                        enqueue(&Outgoing::Response(resp), &mut pending);
                    }
                }
                Ok(Incoming::Request(req)) => {
                    if slow_tx.send(req).is_err() {
                        tracing::error!("处理队列已经关闭，请求被丢弃");
                    }
                }
                Ok(Incoming::Notification(_)) => {}
                Ok(Incoming::Response(resp)) => {
                    tracing::warn!("引擎收到了一条响应（id {}），忽略", resp.id);
                }
                Err(e) => tracing::warn!("无法解析的消息：{e}"),
            }
        }
        // 4. 等下一件事：管道可读；后台任务送来了响应或事件；还有没写完的帧而管道可写。
        //    定时醒一次只是兜底。
        tokio::select! {
            _ = conn.readable() => {}
            msg = out_rx.recv() => {
                if let Some(msg) = msg {
                    enqueue(&msg, &mut pending);
                }
            }
            _ = conn.writable(), if !pending.is_empty() => {}
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
    worker.abort();
    if let Some(forwarder) = forwarder {
        forwarder.abort();
    }
}

/// PDFium 线程的通知对应哪条事件。
fn notice_event(notice: Notice) -> Option<Outgoing> {
    match notice {
        Notice::Fidelity {
            doc_id,
            page_id,
            fidelity,
        } => {
            let (status, reason) = wire_fidelity(fidelity);
            let result = FidelityResult {
                doc_id,
                page_id,
                status,
                reason,
            };
            frame::json_value(&result)
                .map(|value| Outgoing::Event(messages::event::FIDELITY_RESULT.to_owned(), value))
        }
    }
}

/// 保真校验状态在协议里的写法：状态和原因分开两个字段。
fn wire_fidelity(fidelity: PageFidelity) -> (Fidelity, Option<String>) {
    match fidelity {
        PageFidelity::Pending => (Fidelity::Pending, None),
        PageFidelity::Ok => (Fidelity::Ok, None),
        PageFidelity::OverlayOnly(reason) => (Fidelity::OverlayOnly, Some(reason.to_owned())),
    }
}

/// 派发一条请求；`None` 表示不需要应答。
async fn dispatch(
    engine: &Arc<Engine>,
    id: RequestId,
    method_name: &str,
    params: Option<&JsonValue>,
    out: &mpsc::UnboundedSender<Outgoing>,
) -> Option<Response> {
    match handle_request(engine, id, method_name, params, out).await {
        Ok(Some(resp)) => Some(resp),
        Ok(None) => None,
        Err(e) => Some(frame::response_err(id, e)),
    }
}

/// 处理一条请求。返回 `Ok(None)` 表示该请求的应答推迟（渲染类请求的瓦片走事件，但仍立即应答
/// `requestIds`，所以这里的 `None` 只留给 `session.shutdown`）。
async fn handle_request(
    engine: &Arc<Engine>,
    id: RequestId,
    method_name: &str,
    params: Option<&JsonValue>,
    out: &mpsc::UnboundedSender<Outgoing>,
) -> std::result::Result<Option<Response>, RpcError> {
    match method_name {
        method::SESSION_HELLO => {
            let p: HelloParams = decode(params)?;
            let mut result = hello_result(engine);
            if p.protocol != PROTOCOL_VERSION {
                // 协议不一致时也返回引擎的版本，主进程据此提示"安装文件不完整"。
                result.capabilities.clear();
            }
            Ok(Some(frame::response_ok(id, &result)))
        }
        method::SESSION_PING => {
            let p: PingParams = decode(params)?;
            Ok(Some(frame::response_ok(id, &p)))
        }
        method::SESSION_SHUTDOWN => {
            engine.shutdown.store(true, Ordering::SeqCst);
            Ok(Some(frame::response_ok(id, &true)))
        }
        method::DOC_OPEN => {
            let p: DocOpenParams = decode(params)?;
            let info = engine
                .engine
                .open(p.path.clone(), p.password)
                .await
                .map_err(map_engine_error)?;
            if let Ok(mut paths) = engine.doc_paths.lock() {
                paths.insert(info.doc_id, info.path.clone());
            }
            if let Ok(mut recovery) = engine.recovery.lock() {
                recovery.upsert(DocRecovery {
                    path: info.path.clone(),
                    page_id: 0,
                    scale: 1.0,
                    origin_x: 0,
                    origin_y: 0,
                });
                let _ = recovery.save();
            }
            Ok(Some(frame::response_ok(id, &info)))
        }
        method::DOC_CLOSE => {
            let p: DocCloseParams = decode(params)?;
            if !engine.engine.close_doc(p.doc_id).await {
                return Err(
                    ErrorCode::InvalidArgument.error_with(format!("文档 {} 不存在", p.doc_id))
                );
            }
            let path = engine
                .doc_paths
                .lock()
                .ok()
                .and_then(|mut paths| paths.remove(&p.doc_id));
            // 同一个文件可能打开了两次；还有别的 docId 指着它时，恢复文件里的记录要留着。
            let still_open = path.as_ref().is_some_and(|path| {
                engine
                    .doc_paths
                    .lock()
                    .is_ok_and(|paths| paths.values().any(|other| other == path))
            });
            if let Some(path) = path
                && !still_open
                && let Ok(mut recovery) = engine.recovery.lock()
            {
                recovery.remove(&path);
                let _ = recovery.save();
            }
            Ok(Some(frame::response_ok(id, &())))
        }
        method::DOC_SNAPSHOT => {
            let p: DocSnapshotParams = decode(params)?;
            let (version, shapes) = engine
                .engine
                .snapshot(p.doc_id, p.page_ids)
                .await
                .map_err(map_engine_error)?;
            let pages = shapes
                .iter()
                .map(|shape| {
                    let index = u32::try_from(shape.index).unwrap_or(u32::MAX);
                    let (fidelity, fidelity_reason) = wire_fidelity(shape.fidelity);
                    PageInfo {
                        page_id: index,
                        index,
                        width: shape.width,
                        height: shape.height,
                        fidelity,
                        fidelity_reason,
                        // PDFium 给的是 90° 的倍数（0–3），协议里写度数。
                        rotate: shape
                            .detail
                            .map(|d| u16::try_from(d.rotation.rem_euclid(4)).unwrap_or(0) * 90),
                        crop_box: shape
                            .detail
                            .and_then(|d| d.visible)
                            .map(|b| [b.left, b.bottom, b.right, b.top]),
                    }
                })
                .collect();
            let snapshot = DocSnapshot {
                version: wire_version(version),
                pages,
            };
            Ok(Some(frame::response_ok(id, &snapshot)))
        }
        method::OBJ_LIST => {
            let p: ObjListParams = decode(params)?;
            let objects = engine
                .engine
                .objects(p.doc_id, p.page_id)
                .await
                .map_err(map_engine_error)?;
            let limit = p.limit.unwrap_or(OBJ_LIST_MAX).min(OBJ_LIST_MAX) as usize;
            let page: Vec<messages::ObjectSummary> = objects
                .iter()
                .skip(p.offset as usize)
                .take(limit)
                .map(wire_object)
                .collect();
            Ok(Some(frame::response_ok(id, &page)))
        }
        method::OBJ_HIT_TEST => {
            let p: ObjHitTestParams = decode(params)?;
            let mode = match p.mode {
                messages::HitMode::Top => HitMode::Top,
                messages::HitMode::All => HitMode::All,
            };
            let hits = engine
                .engine
                .hit_test(p.doc_id, p.page_id, (p.point[0], p.point[1]), mode)
                .await
                .map_err(map_engine_error)?;
            let hits: Vec<messages::ObjectSummary> = hits.iter().map(wire_object).collect();
            Ok(Some(frame::response_ok(id, &hits)))
        }
        method::EDIT_TRANSFORM => {
            let p: EditTransformParams = decode(params)?;
            let [a, b, c, d, e, f] = p.matrix;
            let op = EditOp::Transform {
                ids: object_ids(&p.ids),
                matrix: Matrix { a, b, c, d, e, f },
            };
            apply_edit(engine, id, p.doc_id, Some(p.base_version), op, out).await
        }
        method::EDIT_SET_STYLE => {
            let p: EditSetStyleParams = decode(params)?;
            let Some(fill) = p.fill else {
                return Err(ErrorCode::InvalidArgument.error_with("没有给出要改的样式"));
            };
            let (r, g, b) = rgb_bytes(&fill)?;
            let op = EditOp::FillRgb {
                ids: object_ids(&p.ids),
                r,
                g,
                b,
            };
            apply_edit(engine, id, p.doc_id, Some(p.base_version), op, out).await
        }
        method::EDIT_DELETE => {
            let p: EditDeleteParams = decode(params)?;
            let op = EditOp::Delete {
                ids: object_ids(&p.ids),
            };
            apply_edit(engine, id, p.doc_id, Some(p.base_version), op, out).await
        }
        method::HISTORY_UNDO => {
            let p: HistoryParams = decode(params)?;
            apply_edit(engine, id, p.doc_id, None, EditOp::Undo, out).await
        }
        method::HISTORY_REDO => {
            let p: HistoryParams = decode(params)?;
            apply_edit(engine, id, p.doc_id, None, EditOp::Redo, out).await
        }
        method::FIDELITY_CHECK => {
            let p: FidelityCheckParams = decode(params)?;
            // 结论不在应答里：每一页各发一条 fidelity.result（《接口协议》6.9）。
            engine
                .engine
                .check_fidelity(p.doc_id, p.page_ids)
                .await
                .map_err(map_engine_error)?;
            Ok(Some(frame::response_ok(id, &())))
        }
        method::RENDER_TILES => {
            let p: RenderTilesParams = decode(params)?;
            if !(p.scale.is_finite() && p.scale > 0.0) {
                return Err(ErrorCode::InvalidArgument.error_with("scale 必须是正数"));
            }
            let request_ids = schedule_tiles(engine, p, out.clone());
            Ok(Some(frame::response_ok(
                id,
                &RenderTilesResult { request_ids },
            )))
        }
        method::RENDER_CANCEL => Ok(Some(frame::response_ok(id, &()))),
        method::TILE_FETCH => {
            let p: TileFetchParams = decode(params)?;
            let result = fetch_tile(engine, p)?;
            Ok(Some(frame::response_ok(id, &result)))
        }
        method::BENCH_ECHO => {
            let p: EchoParams = decode(params)?;
            Ok(Some(frame::response_ok(id, &p)))
        }
        method::BENCH_STALL if engine.test_hooks => {
            let p: StallParams = decode(params)?;
            // 故意把通信循环卡住，用来测主进程的心跳超时。这条请求在读循环里直接处理，
            // 所以卡住期间别的请求（包括 session.ping）都得不到应答。
            std::thread::sleep(Duration::from_millis(u64::from(p.ms)));
            Ok(Some(frame::response_ok(id, &())))
        }
        method::BENCH_ROUNDTRIP => {
            let p: RoundtripParams = decode(params)?;
            // 这里跑在通信线程上，不经过管道：测的是"处理一条请求"的开销，管道本身的开销由主进程测。
            let mut durations = Vec::with_capacity(p.count as usize);
            for i in 0..p.count {
                let started = std::time::Instant::now();
                let echo = EchoParams { t: u64::from(i) };
                let value = frame::json_value(&echo);
                let bytes = frame::encode(&frame::response_ok(id, &echo))
                    .map_err(|e| ErrorCode::Internal.error_with(e.to_string()))?;
                let _ = value;
                std::hint::black_box(&bytes);
                durations.push(started.elapsed().as_secs_f64() * 1e6);
            }
            Ok(Some(frame::response_ok(
                id,
                &RoundtripResult {
                    durations_us: durations,
                },
            )))
        }
        method::BENCH_ENGINE => {
            let p: BenchEngineParams = decode(params)?;
            let result = crate::bench::run(engine, p).await?;
            Ok(Some(frame::response_ok(id, &result)))
        }
        other => Err(ErrorCode::Protocol.error_with(format!("未知方法 {other}"))),
    }
}

fn hello_result(engine: &Engine) -> HelloResult {
    HelloResult {
        protocol: PROTOCOL_VERSION,
        engine_version: messages::ENGINE_VERSION.to_owned(),
        pdfium_revision: engine.pdfium_revision.clone(),
        capabilities: vec!["shm.tiles".to_owned(), "bench.engine".to_owned()],
        pid: std::process::id(),
        restored: engine
            .restored
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default(),
    }
}

/// 为每张瓦片建一个后台任务，返回立即应答的 requestId 列表。
fn schedule_tiles(
    engine: &Arc<Engine>,
    p: RenderTilesParams,
    out: mpsc::UnboundedSender<Outgoing>,
) -> Vec<u32> {
    // 分层渲染（方案 3.2）：这一批瓦片只画页面的一层。它是拖动预览要的，不是用户换了地方看，
    // 不记成视图：对象层的瓦片在被拖对象那里，记了的话恢复出来的视图就跑到那里去了。
    if p.layer.is_none() {
        record_view(engine, &p);
    }
    let layer = p.layer.as_ref().map(|layer| {
        let ids = object_ids(&layer.object_ids);
        Arc::new(match layer.mode {
            LayerMode::Without => Layer::Without(ids),
            LayerMode::Only => Layer::Only(ids),
        })
    });
    let fingerprint = layer.as_deref().map(layer_fingerprint);
    let mut ids = Vec::with_capacity(p.tiles.len());
    for TileCoord { x, y } in p.tiles {
        let request_id = engine.next_request_id.fetch_add(1, Ordering::Relaxed);
        ids.push(request_id);
        let whole = TileKey::new(p.doc_id, p.page_id, p.scale, x, y);
        let key = fingerprint.map_or(whole, |fingerprint| whole.in_layer(fingerprint));
        let engine = engine.clone();
        let out = out.clone();
        let layer = layer.clone();
        tokio::spawn(async move {
            render_and_publish(&engine, request_id, key, layer.as_deref(), &out).await;
        });
    }
    ids
}

/// 一层的指纹：留哪些对象、是去掉还是只留。只用来把各层的瓦片在槽位表里分开。
fn layer_fingerprint(layer: &Layer) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    layer.hash(&mut hasher);
    hasher.finish()
}

/// 记录视图（恢复文件用）：docId、页码、缩放与瓦片位置。每次渲染请求更新一次并立即落盘（演示级实现，
/// 文件很小；真实实现按方案 9.3 每 2 分钟写增量）。
fn record_view(engine: &Engine, p: &RenderTilesParams) {
    let Some(first) = p.tiles.first() else {
        return;
    };
    let Some(path) = engine
        .doc_paths
        .lock()
        .ok()
        .and_then(|paths| paths.get(&p.doc_id).cloned())
    else {
        return;
    };
    let doc = DocRecovery {
        path,
        page_id: p.page_id,
        scale: p.scale,
        origin_x: first.x,
        origin_y: first.y,
    };
    if let Ok(mut recovery) = engine.recovery.lock() {
        recovery.upsert(doc);
        let _ = recovery.save();
    }
}

/// 渲染一块瓦片、写进共享内存槽位，然后发 `render.tileReady`。
async fn render_and_publish(
    engine: &Arc<Engine>,
    request_id: u32,
    key: TileKey,
    layer: Option<&Layer>,
    out: &mpsc::UnboundedSender<Outgoing>,
) {
    let scale = key.scale();
    let (bitmap, version) = match engine
        .engine
        .render_tile(
            key.doc_id,
            key.page_id,
            scale,
            (key.x, key.y),
            layer.cloned(),
        )
        .await
    {
        Ok(rendered) => rendered,
        Err(e) => {
            tracing::warn!("瓦片渲染失败：{e}");
            return;
        }
    };
    let (slot, generation) = match write_tile(engine, &key, bitmap.bgra()) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("写入共享内存失败：{e}");
            return;
        }
    };
    let ready = messages::TileReady {
        request_id,
        doc_id: key.doc_id,
        page_id: key.page_id,
        scale,
        x: key.x,
        y: key.y,
        version: wire_version(version),
        slot,
        generation,
    };
    engine.event_to(out, messages::event::RENDER_TILE_READY, &ready);
}

fn write_tile(
    engine: &Engine,
    key: &TileKey,
    pixels: &[u8],
) -> std::result::Result<(u32, u32), String> {
    let slot = {
        let mut table = engine.slots.lock().map_err(|_| "槽位表锁失效")?;
        table.acquire(*key)
    };
    let started = std::time::Instant::now();
    let generation = {
        let mut region = engine.shm.lock().map_err(|_| "共享内存锁失效")?;
        region.write_tile(slot, pixels).map_err(|e| e.to_string())?
    };
    if engine.measure.load(Ordering::Relaxed)
        && let Ok(mut samples) = engine.bench_samples.lock()
    {
        samples.push(started.elapsed().as_secs_f64() * 1e6);
    }
    if let Ok(mut table) = engine.slots.lock() {
        table.commit(slot, generation);
    }
    Ok((slot, generation))
}

fn fetch_tile(
    engine: &Engine,
    p: TileFetchParams,
) -> std::result::Result<TileFetchResult, RpcError> {
    let region = engine.shm.lock().map_err(|_| ErrorCode::Internal.error())?;
    let generation = region
        .generation(p.slot)
        .map_err(|e| ErrorCode::TileGenerationStale.error_with(e.to_string()))?;
    if generation != p.generation {
        return Err(ErrorCode::TileGenerationStale.error_with(format!(
            "槽位 {} 期望代数 {}，当前 {}",
            p.slot, p.generation, generation
        )));
    }
    Ok(TileFetchResult {
        slot: p.slot,
        generation,
        width: shm::TILE_SIZE,
        height: shm::TILE_SIZE,
    })
}

fn decode<T: serde::de::DeserializeOwned>(
    params: Option<&JsonValue>,
) -> std::result::Result<T, RpcError> {
    let value = params.cloned().unwrap_or(JsonValue::Null);
    let value = serde_json::to_value(value)
        .map_err(|e| ErrorCode::InvalidArgument.error_with(format!("参数不合法：{e}")))?;
    serde_json::from_value(value)
        .map_err(|e| ErrorCode::InvalidArgument.error_with(format!("参数不合法：{e}")))
}

/// 执行一条修改命令（或撤销、重做）：成功后先推送 `doc.changed`，再应答新的版本号。
async fn apply_edit(
    engine: &Arc<Engine>,
    id: RequestId,
    doc_id: u32,
    base_version: Option<u32>,
    op: EditOp,
    out: &mpsc::UnboundedSender<Outgoing>,
) -> std::result::Result<Option<Response>, RpcError> {
    let change = engine
        .engine
        .edit(doc_id, base_version.map(u64::from), op)
        .await
        .map_err(map_engine_error)?;
    let version = wire_version(change.version);
    let changed = DocChanged {
        doc_id,
        version,
        changes: change
            .pages
            .iter()
            .map(|page| PageChange {
                page_id: u32::try_from(page.page).unwrap_or(u32::MAX),
                added: page.added.iter().map(|id| id.0).collect(),
                removed: page.removed.iter().map(|id| id.0).collect(),
                modified: page.modified.iter().map(|id| id.0).collect(),
                area: page.area.map(|r| [r.left, r.bottom, r.right, r.top]),
            })
            .collect(),
        pages_changed: false,
    };
    engine.event_to(out, messages::event::DOC_CHANGED, &changed);
    Ok(Some(frame::response_ok(id, &EditResult { version })))
}

/// 文档版本号在协议里是 32 位。
fn wire_version(version: u64) -> u32 {
    u32::try_from(version).unwrap_or(u32::MAX)
}

fn object_ids(ids: &[u64]) -> Vec<ObjectId> {
    ids.iter().copied().map(ObjectId).collect()
}

fn wire_object(object: &ObjectSummary) -> messages::ObjectSummary {
    messages::ObjectSummary {
        id: object.id.0,
        kind: match object.kind {
            pdfium_sys::ObjectKind::Text => messages::ObjectKind::Text,
            pdfium_sys::ObjectKind::Path => messages::ObjectKind::Path,
            pdfium_sys::ObjectKind::Image => messages::ObjectKind::Image,
            pdfium_sys::ObjectKind::Shading => messages::ObjectKind::Shading,
            pdfium_sys::ObjectKind::Form => messages::ObjectKind::Form,
            pdfium_sys::ObjectKind::Unknown => messages::ObjectKind::Unknown,
        },
        bbox: object.bounds.map(|b| [b.left, b.bottom, b.right, b.top]),
        z: u32::try_from(object.z).unwrap_or(u32::MAX),
        locked: false,
        pickable: object.pickable,
    }
}

/// 把协议里的颜色换成 8 位 RGB。原型只认 RGB，三个分量都要在 0–1 之间。
fn rgb_bytes(color: &Color) -> std::result::Result<(u8, u8, u8), RpcError> {
    let ColorSpace::Rgb = color.space;
    let [r, g, b] = color.components[..] else {
        return Err(ErrorCode::InvalidArgument.error_with("RGB 颜色要有三个分量"));
    };
    if ![r, g, b].iter().all(|c| (0.0..=1.0).contains(c)) {
        return Err(ErrorCode::InvalidArgument.error_with("颜色分量要在 0 到 1 之间"));
    }
    let byte = |c: f32| (c * 255.0).round() as u8;
    Ok((byte(r), byte(g), byte(b)))
}

fn map_engine_error(e: crate::pdfium_thread::EngineError) -> RpcError {
    use crate::pdfium_thread::EngineError;
    let code = match &e {
        EngineError::NoDocument { .. } | EngineError::NoPage { .. } => ErrorCode::InvalidArgument,
        EngineError::Open {
            error,
            with_password,
        } => open_error_code(error, *with_password),
        EngineError::Render(_) => ErrorCode::Internal,
        EngineError::VersionConflict { .. } => ErrorCode::VersionConflict,
        EngineError::Edit(edit) => edit_error_code(edit),
    };
    code.error_with(e.to_string())
}

/// 编辑失败时用哪个错误码（《接口协议》第 8 节）。
fn edit_error_code(error: &EditError) -> ErrorCode {
    match error {
        EditError::NoObject(_) => ErrorCode::ObjectNotFound,
        EditError::PageNotEditable { .. } => ErrorCode::PageOverlayOnly,
        // 协议里没有“没有可撤销的操作”这样的码，按参数不合法报。
        EditError::NoTarget
        | EditError::SingularMatrix
        | EditError::NothingToUndo
        | EditError::NothingToRedo => ErrorCode::InvalidArgument,
        EditError::Pdfium(_) | EditError::NoFill => ErrorCode::Internal,
    }
}

/// `doc.open` 失败时用哪个错误码（《接口协议》6.1、第 8 节）。
fn open_error_code(error: &pdfium_sys::Error, with_password: bool) -> ErrorCode {
    use pdfium_sys::{Error, LoadError};
    match error {
        Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound => ErrorCode::FileNotFound,
        Error::Load(LoadError::Password) if with_password => ErrorCode::PasswordWrong,
        Error::Load(LoadError::Password) => ErrorCode::PasswordRequired,
        // PDFium 报“格式不对”或“文件读不了”，都是修复之后仍然打不开。
        Error::Load(LoadError::Format | LoadError::File) => ErrorCode::FileDamaged,
        // 不认识的加密方式。
        Error::Load(LoadError::Security) => ErrorCode::Unsupported,
        _ => ErrorCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use pdfium_sys::{Error, LoadError};

    use super::*;

    #[test]
    fn open_errors_map_to_protocol_codes() {
        let missing = Error::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(open_error_code(&missing, false), ErrorCode::FileNotFound);
        let denied = Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(open_error_code(&denied, false), ErrorCode::Internal);
        let password = Error::Load(LoadError::Password);
        assert_eq!(
            open_error_code(&password, false),
            ErrorCode::PasswordRequired
        );
        assert_eq!(open_error_code(&password, true), ErrorCode::PasswordWrong);
        assert_eq!(
            open_error_code(&Error::Load(LoadError::Format), false),
            ErrorCode::FileDamaged
        );
        assert_eq!(
            open_error_code(&Error::Load(LoadError::Security), false),
            ErrorCode::Unsupported
        );
    }
}
