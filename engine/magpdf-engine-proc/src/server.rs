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
use magpdf_ipc::frame::{self, Incoming};
use magpdf_ipc::messages::{
    self, BenchEngineParams, DocCloseParams, DocInfo, DocOpenParams, EchoParams, ErrorCode,
    HelloParams, HelloResult, JsonValue, PingParams, RenderTilesParams, RenderTilesResult, Request,
    RequestId, Response, RoundtripParams, RoundtripResult, RpcError, TileCoord, TileFetchParams,
    TileFetchResult, method,
};
use magpdf_ipc::shm::{self, Layout, MappedRegion};
use magpdf_ipc::{PROTOCOL_VERSION, pipe, pump};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::mpsc;

use crate::pdfium_thread::EngineHandle;
use crate::recovery::{DocRecovery, RecoveryFile};
use crate::slots::{SlotTable, TileKey};

/// 引擎配置（命令行参数）。
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub pipe: String,
    pub session: String,
    pub recovery: Option<PathBuf>,
    pub slots: u32,
}

impl EngineConfig {
    /// 解析 `--pipe <名字> --session <ID> [--recovery <路径>] [--slots <n>]`。
    pub fn from_args(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut pipe = None;
        let mut session = None;
        let mut recovery = None;
        let mut slots = shm::DEFAULT_SLOTS;
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
                other => anyhow::bail!("未知参数 {other}"),
            }
        }
        Ok(Self {
            pipe: pipe.context("缺少 --pipe")?,
            session: session.context("缺少 --session")?,
            recovery,
            slots,
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

/// 运行引擎，直到 `session.shutdown`，或主进程退出导致管道实例无法再创建。
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
    while pipe::accept(&mut server).await?.is_some() {
        serve_connection(engine.clone(), server).await;
        if engine.shutdown.load(Ordering::SeqCst) {
            break;
        }
        // 主进程断开（被结束或崩溃）后继续等下一个连接。
        server = match pipe::recreate(&pipe_name) {
            Ok(s) => s,
            Err(e) => {
                tracing::info!("主进程已退出，引擎结束：{e}");
                break;
            }
        };
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
        let engine = EngineHandle::spawn()?;
        // 恢复：重新打开恢复文件里的文档。
        let mut restored = Vec::new();
        for doc in recovery.state().docs.clone() {
            match engine.open_blocking(&doc.path) {
                Ok(info) => restored.push(info),
                Err(e) => tracing::warn!("恢复文档 {} 失败：{e}", doc.path),
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
                Ok(Incoming::Response(resp)) => tracing::warn!("引擎收到了响应 {resp:?}，忽略"),
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
        method::RENDER_TILES => {
            let p: RenderTilesParams = decode(params)?;
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
    record_view(engine, &p);
    let mut ids = Vec::with_capacity(p.tiles.len());
    for TileCoord { x, y } in p.tiles {
        let request_id = engine.next_request_id.fetch_add(1, Ordering::Relaxed);
        ids.push(request_id);
        let key = TileKey::new(p.doc_id, p.page_id, p.scale, x, y);
        let engine = engine.clone();
        let out = out.clone();
        tokio::spawn(async move {
            render_and_publish(&engine, request_id, key, &out).await;
        });
    }
    ids
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
    out: &mpsc::UnboundedSender<Outgoing>,
) {
    let scale = key.scale_milli as f32 / 1000.0;
    let bitmap = match engine
        .engine
        .render_tile(key.doc_id, key.page_id, scale, key.x, key.y)
        .await
    {
        Ok(b) => b,
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
        // 原型还没有修改命令，文档版本恒为 0。
        version: 0,
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

fn map_engine_error(e: crate::pdfium_thread::EngineError) -> RpcError {
    use crate::pdfium_thread::EngineError;
    match &e {
        EngineError::NoDocument { .. } | EngineError::NoPage { .. } => {
            ErrorCode::InvalidArgument.error_with(e.to_string())
        }
        EngineError::Open(message) if message.contains("not found") => {
            ErrorCode::FileNotFound.error_with(e.to_string())
        }
        _ => ErrorCode::Internal.error_with(e.to_string()),
    }
}
