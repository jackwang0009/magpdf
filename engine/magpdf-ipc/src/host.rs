//! 主进程一侧：启动引擎进程、经命名管道收发、监督它（方案 3.1、9.3；《接口协议》第 2、4 节）。
//!
//! [`Supervisor`] 把这几件事包在一起：
//! - 启动引擎，连上管道，打开共享内存，发 `session.hello` 并核对协议版本；
//! - [`Supervisor::request`] 发一条请求、等它的响应，可以同时有多条请求在途；
//! - 引擎推送的事件（`doc.changed`、`render.tileReady` 等）经 [`HostEvent`] 交给调用方；
//! - 每隔一段时间发 `session.ping`，超时没有应答就判定引擎挂死；
//! - 引擎没了（崩溃、被结束或挂死）时自动重启：在途的请求以 `E_ENGINE_RESTARTED` 结束，先发
//!   [`HostEvent::Restarting`]，新引擎就绪后发 [`HostEvent::Restored`]，带上它按恢复文件重新打开的文档。
//!
//! 文档类请求原样转发，这里不解析业务字段（《接口协议》第 2 节）。

use std::collections::{HashMap, VecDeque};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use thiserror::Error;
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::messages::{DocInfo, ErrorCode, HelloResult, PROTOCOL_VERSION, RpcError, method};
use crate::shm::{self, MappedRegion, ShmError};
use crate::{frame, pipe, pump};

/// 怎么启动引擎。
#[derive(Debug, Clone)]
pub struct EngineLaunch {
    /// `magpdf-engine` 可执行文件。
    pub engine_bin: PathBuf,
    /// 放 `pdfium.dll` 的目录，加到引擎 `PATH` 的最前面。动态库就在可执行文件旁边时不用给。
    pub dll_dir: Option<PathBuf>,
    /// 恢复文件的路径。引擎重启后按它重新打开文档。
    pub recovery: PathBuf,
    /// 共享内存的槽位数。
    pub slots: u32,
    pub app_version: String,
    pub locale: Option<String>,
    pub user_data_dir: String,
    pub cache_dir: String,
    /// 心跳间隔（《接口协议》第 4 节：2 秒）。
    pub heartbeat_interval: Duration,
    /// 心跳多久没有应答算挂死（《接口协议》第 4 节：5 秒）。
    pub heartbeat_timeout: Duration,
    /// 追加到引擎命令行末尾的参数。测试用它打开引擎的测试开关。
    pub extra_args: Vec<String>,
}

impl EngineLaunch {
    /// 用协议规定的心跳参数和默认槽位数。
    pub fn new(engine_bin: PathBuf, recovery: PathBuf, app_version: impl Into<String>) -> Self {
        let temp = std::env::temp_dir().to_string_lossy().into_owned();
        Self {
            engine_bin,
            dll_dir: None,
            recovery,
            slots: shm::DEFAULT_SLOTS,
            app_version: app_version.into(),
            locale: None,
            user_data_dir: temp.clone(),
            cache_dir: temp,
            heartbeat_interval: Duration::from_secs(2),
            heartbeat_timeout: Duration::from_secs(5),
            extra_args: Vec::new(),
        }
    }
}

/// 交给界面的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// 引擎推送的事件，原样转发。
    Engine { method: String, params: Value },
    /// 引擎没了，正在重启（《接口协议》第 7 节 `engine.restarting`）。
    Restarting,
    /// 新引擎已经就绪。`restored` 是它按恢复文件重新打开的文档，`docId` 是新分配的
    /// （《接口协议》第 7 节 `engine.restored`）。
    Restored { restored: Vec<DocInfo> },
    /// 连续几次都没能把引擎重新拉起来，不再重试。
    Failed { message: String },
}

/// 启动或重启引擎失败。
#[derive(Debug, Error)]
pub enum HostError {
    #[error("启动引擎进程失败：{0}")]
    Spawn(std::io::Error),
    #[error("连不上引擎的管道：{0}")]
    Connect(std::io::Error),
    #[error("打不开引擎的共享内存：{0}")]
    Shm(#[from] ShmError),
    #[error("session.hello 失败：{0}")]
    Hello(RpcError),
    #[error("session.hello 的返回值看不懂：{0}")]
    HelloFormat(serde_json::Error),
    #[error("引擎的协议版本是 {engine}，主进程是 {host}")]
    Protocol { engine: u32, host: u32 },
}

/// `CreateProcess` 的 `CREATE_NO_WINDOW`：不给控制台程序开窗口。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

type Pending = HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>;

/// 一条活着的连接里，请求方要用到的东西。
struct Live {
    /// 待写出的帧。
    tx: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<Mutex<Pending>>,
    shm: MappedRegion,
    pid: u32,
}

struct Shared {
    live: Mutex<Option<Live>>,
    next_id: AtomicU64,
    shutting_down: AtomicBool,
    shutdown: Notify,
}

impl Shared {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let (reply, rx) = oneshot::channel();
        {
            let live = self.live.lock().map_err(|_| ErrorCode::Internal.error())?;
            let Some(live) = live.as_ref() else {
                return Err(ErrorCode::EngineRestarted.error());
            };
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            let framed = frame::encode(&message)
                .map_err(|e| ErrorCode::Internal.error_with(e.to_string()))?;
            live.pending
                .lock()
                .map_err(|_| ErrorCode::Internal.error())?
                .insert(id, reply);
            if live.tx.send(framed).is_err() {
                return Err(ErrorCode::EngineRestarted.error());
            }
        }
        // 连接断开时读写循环会给每条在途请求一个 E_ENGINE_RESTARTED；发送端被直接丢掉也按这个算。
        rx.await
            .unwrap_or_else(|_| Err(ErrorCode::EngineRestarted.error()))
    }
}

/// 一个正在运行的引擎：进程和它的读写循环。丢弃时结束进程。
struct Running {
    child: Child,
    io: JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.io.abort();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 引擎的监督者。可以克隆，各处共用同一个引擎。
#[derive(Clone)]
pub struct Supervisor {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor")
            .field("engine_pid", &self.engine_pid())
            .finish()
    }
}

impl Supervisor {
    /// 启动引擎并开始监督。返回监督者、事件的接收端，以及引擎启动时按恢复文件重新打开的文档。
    pub async fn start(
        launch: EngineLaunch,
    ) -> Result<(Self, mpsc::UnboundedReceiver<HostEvent>, Vec<DocInfo>), HostError> {
        let shared = Arc::new(Shared {
            live: Mutex::new(None),
            next_id: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
            shutdown: Notify::new(),
        });
        let (events, events_rx) = mpsc::unbounded_channel();
        let (running, restored) = start_engine(&launch, &shared, &events).await?;
        tokio::spawn(supervise(shared.clone(), launch, events, running));
        Ok((Self { shared }, events_rx, restored))
    }

    /// 发一条请求，等它的响应。引擎正在重启、或者在处理过程中没了，返回 `E_ENGINE_RESTARTED`。
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.shared.request(method, params).await
    }

    /// 从共享内存读出一块瓦片的 BGRA 像素。代数对不上（槽位已被复用，或者引擎已经换了一个）时
    /// 返回错误，界面据此重新请求渲染。
    pub fn read_tile(&self, slot: u32, generation: u32) -> Result<Vec<u8>, ShmError> {
        let live = self.shared.live.lock().map_err(|_| stale(generation))?;
        let live = live.as_ref().ok_or_else(|| stale(generation))?;
        live.shm.read_tile(slot, generation)
    }

    /// 当前引擎进程的 PID；正在重启时是 `None`。
    pub fn engine_pid(&self) -> Option<u32> {
        self.shared
            .live
            .lock()
            .ok()
            .and_then(|live| live.as_ref().map(|l| l.pid))
    }

    /// 让引擎正常退出，不再重启。
    pub async fn shutdown(&self) {
        self.shared.shutting_down.store(true, Ordering::SeqCst);
        // 给引擎一个机会自己退出；它不应答也没关系，监督任务会结束进程。
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            self.shared.request(method::SESSION_SHUTDOWN, json!({})),
        )
        .await;
        self.shared.shutdown.notify_waiters();
    }
}

fn stale(expected: u32) -> ShmError {
    ShmError::Generation {
        expected,
        actual: 0,
    }
}

/// 监督循环：看着一个引擎，没了就换一个，直到被要求退出。
async fn supervise(
    shared: Arc<Shared>,
    launch: EngineLaunch,
    events: mpsc::UnboundedSender<HostEvent>,
    mut running: Running,
) {
    loop {
        watch(&shared, &launch, &mut running).await;
        // 先摘掉连接，新的请求立刻得到 E_ENGINE_RESTARTED；在途的由读写循环退出时结束。
        if let Ok(mut live) = shared.live.lock() {
            *live = None;
        }
        drop(running);
        if shared.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let _ = events.send(HostEvent::Restarting);
        let mut attempt = 0u32;
        running = loop {
            match start_engine(&launch, &shared, &events).await {
                Ok((running, restored)) => {
                    let _ = events.send(HostEvent::Restored { restored });
                    break running;
                }
                Err(e) => {
                    attempt += 1;
                    tracing::warn!("重启引擎失败（第 {attempt} 次）：{e}");
                    if attempt >= 3 || shared.shutting_down.load(Ordering::SeqCst) {
                        let _ = events.send(HostEvent::Failed {
                            message: e.to_string(),
                        });
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
                }
            }
        };
    }
}

/// 等到这个引擎不能用了为止：管道断了、心跳超时，或者被要求退出。
async fn watch(shared: &Shared, launch: &EngineLaunch, running: &mut Running) {
    let mut ticker = tokio::time::interval(launch.heartbeat_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = &mut running.io => return,
            () = shared.shutdown.notified() => return,
            _ = ticker.tick() => {
                let ping = shared.request(method::SESSION_PING, json!({ "t": 0 }));
                match tokio::time::timeout(launch.heartbeat_timeout, ping).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        tracing::warn!("心跳失败：{e}");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!("引擎 {:?} 没有应答心跳，按挂死处理", launch.heartbeat_timeout);
                        return;
                    }
                }
            }
        }
        if shared.shutting_down.load(Ordering::SeqCst) {
            return;
        }
    }
}

/// 启动一个引擎进程：连管道、开共享内存、起读写循环、发 `session.hello`。
async fn start_engine(
    launch: &EngineLaunch,
    shared: &Arc<Shared>,
    events: &mpsc::UnboundedSender<HostEvent>,
) -> Result<(Running, Vec<DocInfo>), HostError> {
    let token = pipe::random_token(8).map_err(HostError::Spawn)?;
    let pipe_name = pipe::pipe_name(std::process::id(), &token);
    let session = format!("s-{}-{token}", std::process::id());

    let mut command = Command::new(&launch.engine_bin);
    command
        .arg("--pipe")
        .arg(&pipe_name)
        .arg("--session")
        .arg(&session)
        .arg("--recovery")
        .arg(&launch.recovery)
        .arg("--slots")
        .arg(launch.slots.to_string())
        .args(&launch.extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        // 引擎是控制台程序。主进程是窗口程序、自己没有控制台时，不加这个标志系统会给引擎另开一个黑窗口。
        .creation_flags(CREATE_NO_WINDOW);
    if let Some(dir) = &launch.dll_dir {
        let mut path = std::ffi::OsString::from(dir.as_os_str());
        path.push(";");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        command.env("PATH", path);
    }
    let child = command.spawn().map_err(HostError::Spawn)?;
    let pid = child.id();
    // 从这里起，任何一步失败都要把进程收掉：交给 Running 的 Drop。
    let mut running = Running {
        child,
        io: tokio::spawn(async {}),
    };

    let client = connect(&pipe_name, Duration::from_secs(20)).await?;
    shm::wait_for_mapping(&session, Duration::from_secs(20)).await?;
    let region = shm::open(&session)?;

    let (tx, rx) = mpsc::unbounded_channel();
    let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(HashMap::new()));
    running.io = tokio::spawn(io_loop(client, rx, pending.clone(), events.clone()));
    if let Ok(mut live) = shared.live.lock() {
        *live = Some(Live {
            tx,
            pending,
            shm: region,
            pid,
        });
    }

    let hello = json!({
        "protocol": PROTOCOL_VERSION,
        "appVersion": launch.app_version,
        "locale": launch.locale,
        "userDataDir": launch.user_data_dir,
        "cacheDir": launch.cache_dir,
        "sessionId": session,
        "recoveryFile": launch.recovery.to_string_lossy(),
    });
    let result = match tokio::time::timeout(
        Duration::from_secs(20),
        shared.request(method::SESSION_HELLO, hello),
    )
    .await
    {
        Ok(Ok(value)) => {
            serde_json::from_value::<HelloResult>(value).map_err(HostError::HelloFormat)
        }
        Ok(Err(e)) => Err(HostError::Hello(e)),
        Err(_) => Err(HostError::Hello(ErrorCode::EngineRestarted.error())),
    };
    let hello = match result {
        Ok(hello) if hello.protocol == PROTOCOL_VERSION => hello,
        Ok(hello) => {
            clear_live(shared);
            return Err(HostError::Protocol {
                engine: hello.protocol,
                host: PROTOCOL_VERSION,
            });
        }
        Err(e) => {
            clear_live(shared);
            return Err(e);
        }
    };
    Ok((running, hello.restored))
}

fn clear_live(shared: &Shared) {
    if let Ok(mut live) = shared.live.lock() {
        *live = None;
    }
}

async fn connect(name: &str, timeout: Duration) -> Result<NamedPipeClient, HostError> {
    let started = std::time::Instant::now();
    loop {
        match pipe::connect_client(name) {
            Ok(client) => return Ok(client),
            Err(e) if started.elapsed() >= timeout => return Err(HostError::Connect(e)),
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

/// 一条连接的读写循环。管道断开、帧不合法或者发送端都没了就结束；结束前把在途的请求全部以
/// `E_ENGINE_RESTARTED` 了结。
async fn io_loop(
    client: NamedPipeClient,
    mut outgoing: mpsc::UnboundedReceiver<Vec<u8>>,
    pending: Arc<Mutex<Pending>>,
    events: mpsc::UnboundedSender<HostEvent>,
) {
    pump_connection(&client, &mut outgoing, &pending, &events).await;
    if let Ok(mut pending) = pending.lock() {
        for (_, reply) in pending.drain() {
            let _ = reply.send(Err(ErrorCode::EngineRestarted.error()));
        }
    }
}

async fn pump_connection(
    client: &NamedPipeClient,
    outgoing: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    pending: &Mutex<Pending>,
    events: &mpsc::UnboundedSender<HostEvent>,
) {
    let mut reader = pump::FrameReader::new();
    let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
    let mut written = 0usize;
    loop {
        while let Ok(framed) = outgoing.try_recv() {
            queue.push_back(framed);
        }
        while let Some(front) = queue.front() {
            match pump::write_frame(client, &mut written, front) {
                Ok(()) => {
                    queue.pop_front();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return,
            }
        }
        if reader.pump(client).is_err() {
            return;
        }
        loop {
            match reader.next_frame() {
                Ok(Some(body)) => deliver(&body, pending, events),
                Ok(None) => break,
                Err(_) => return,
            }
        }
        tokio::select! {
            _ = client.readable() => {}
            framed = outgoing.recv() => match framed {
                Some(framed) => queue.push_back(framed),
                None => return,
            },
            _ = client.writable(), if !queue.is_empty() => {}
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}

/// 把收到的一帧交给等它的请求，或者当作事件转发。
fn deliver(body: &[u8], pending: &Mutex<Pending>, events: &mpsc::UnboundedSender<HostEvent>) {
    let Ok(mut message) = serde_json::from_slice::<Value>(body) else {
        tracing::warn!("引擎发来一帧不是 JSON 的消息，忽略");
        return;
    };
    let id = message.get("id").and_then(Value::as_u64);
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match (id, method) {
        (Some(id), None) => {
            let Some(reply) = pending.lock().ok().and_then(|mut p| p.remove(&id)) else {
                return;
            };
            let outcome = match message.get_mut("error").map(Value::take) {
                Some(error) => Err(serde_json::from_value::<RpcError>(error)
                    .unwrap_or_else(|e| ErrorCode::Protocol.error_with(e.to_string()))),
                None => Ok(message
                    .get_mut("result")
                    .map(Value::take)
                    .unwrap_or(Value::Null)),
            };
            let _ = reply.send(outcome);
        }
        (None, Some(method)) => {
            let params = message
                .get_mut("params")
                .map(Value::take)
                .unwrap_or(Value::Null);
            let _ = events.send(HostEvent::Engine { method, params });
        }
        _ => tracing::warn!("引擎发来一条既不是响应也不是事件的消息，忽略"),
    }
}
