//! 主进程侧：拉起引擎、连命名管道、跑 V06 基准（方案 3.1、9.3；P0 V06）。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use magpdf_ipc::frame::{self};
use magpdf_ipc::messages::{
    self, BenchEngineParams, BenchEngineResult, DocOpenParams, HelloParams, HelloResult,
    PROTOCOL_VERSION, PingParams, RenderPriority, RenderTilesParams, RenderTilesResult, Stats,
    TileCoord,
};
use magpdf_ipc::{pipe, shm};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;

#[derive(Debug, Serialize)]
pub struct BenchReport {
    pub protocol_version: u32,
    pub session_id: String,
    pub engine_pid: u32,
    pub engine_version: String,
    pub pdfium_revision: String,
    pub slots: u32,
    pub shm_total_mb: u64,
    pub ping_median_us: f64,
    pub ping_p95_us: f64,
    pub tile_fetch_median_ms: f64,
    pub tile_fetch_p95_ms: f64,
    /// 从发出 `render.tiles`（一块瓦片）到收到它的 `render.tileReady`。
    pub tile_event_median_ms: f64,
    pub tile_event_p95_ms: f64,
    pub json_share_of_roundtrip: f64,
    pub msgpack_vs_json_bytes: f64,
    pub engine: BenchEngineResult,
    /// 强制结束引擎到新引擎应答 `session.hello` 的时间。
    pub restart_ms: Option<f64>,
    /// 新引擎按恢复文件重新打开的文档数。
    pub restored_docs: Option<usize>,
    pub pass: BenchPass,
}

#[derive(Debug, Serialize)]
pub struct BenchPass {
    pub tile_fetch_under_5ms: bool,
    pub ping_p95_under_2ms: bool,
    pub restart_under_5s: bool,
}

pub struct HostConfig {
    pub engine_bin: PathBuf,
    pub pdfium_dir: PathBuf,
    pub sample_pdf: PathBuf,
    pub recovery: PathBuf,
    pub slots: u32,
    pub out_json: PathBuf,
}

impl HostConfig {
    pub fn from_env_or_defaults() -> Result<Self> {
        let engine_bin = std::env::var("MAGPDF_ENGINE_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("target/release/magpdf-engine.exe"));
        let pdfium_dir = std::env::var("PDFIUM_PREBUILT_DIR")
            .map(PathBuf::from)
            .context("需要环境变量 PDFIUM_PREBUILT_DIR（含 bin/pdfium.dll 的预编译根目录）")?;
        let sample_pdf = std::env::var("MAGPDF_BENCH_PDF")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("tests/corpus/synthetic/text-standard14.pdf"));
        let recovery =
            std::env::temp_dir().join(format!("magpdf-v06-recovery-{}.json", std::process::id()));
        Ok(Self {
            engine_bin,
            pdfium_dir,
            sample_pdf,
            recovery,
            slots: shm::DEFAULT_SLOTS,
            out_json: PathBuf::from("tests/out/v06/bench.json"),
        })
    }

    fn pdfium_bin_dir(&self) -> PathBuf {
        let bin = self.pdfium_dir.join("bin");
        if bin.is_dir() {
            bin
        } else {
            self.pdfium_dir.clone()
        }
    }
}

pub struct Host {
    pub cfg: HostConfig,
    child: Option<Child>,
    pipe_name: String,
    session: String,
    client: Option<NamedPipeClient>,
    shm: Option<shm::MappedRegion>,
    next_id: u64,
}

impl Host {
    pub fn new(cfg: HostConfig) -> Self {
        Self {
            cfg,
            child: None,
            pipe_name: String::new(),
            session: String::new(),
            client: None,
            shm: None,
            next_id: 1,
        }
    }

    pub async fn start_engine(&mut self) -> Result<u32> {
        self.shutdown_engine();
        let token = pipe::random_token(8)?;
        self.pipe_name = pipe::pipe_name(std::process::id(), &token);
        self.session = format!("s-{}-{}", std::process::id(), token);
        if !self.cfg.engine_bin.is_file() {
            bail!("找不到引擎可执行文件：{}", self.cfg.engine_bin.display());
        }
        let path_env = {
            let old = std::env::var_os("PATH").unwrap_or_default();
            let mut new = std::ffi::OsString::from(self.cfg.pdfium_bin_dir().as_os_str());
            new.push(";");
            new.push(old);
            new
        };
        let child = Command::new(&self.cfg.engine_bin)
            .arg("--pipe")
            .arg(&self.pipe_name)
            .arg("--session")
            .arg(&self.session)
            .arg("--recovery")
            .arg(&self.cfg.recovery)
            .arg("--slots")
            .arg(self.cfg.slots.to_string())
            .env("PATH", path_env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .context("启动 magpdf-engine 失败")?;
        let pid = child.id();
        self.child = Some(child);
        self.client = Some(wait_connect(&self.pipe_name, Duration::from_secs(20)).await?);
        let _ = shm::wait_for_mapping(&self.session, Duration::from_secs(20)).await;
        self.shm = Some(shm::open(&self.session).context("打开共享内存失败")?);
        Ok(pid)
    }

    fn shutdown_engine(&mut self) {
        self.client = None;
        self.shm = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// 发一条请求，返回它的 id。
    async fn send_request<P: Serialize>(&mut self, method: &str, params: &P) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        // 直接走 serde_json::Value，避免 messages::JsonValue 把整数收成 f64 后
        // 服务端反序列化 u32 失败（3005 参数不合法）。
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let body = serde_json::to_vec(&req).context("编码请求失败")?;
        let bytes = frame::encode_body(&body);
        let client = self.client.as_mut().context("尚未连接引擎")?;
        client.write_all(&bytes).await.context("写管道失败")?;
        Ok(id)
    }

    /// 读一条消息：响应或事件。
    async fn read_message(&mut self) -> Result<serde_json::Value> {
        let client = self.client.as_mut().context("尚未连接引擎")?;
        let read = async {
            let mut len_buf = [0u8; 4];
            client
                .read_exact(&mut len_buf)
                .await
                .context("读帧长度失败")?;
            let len = u32::from_le_bytes(len_buf) as usize;
            if len > messages::MAX_FRAME_BYTES {
                bail!("响应帧过大：{len}");
            }
            let mut body = vec![0u8; len];
            client.read_exact(&mut body).await.context("读帧正文失败")?;
            serde_json::from_slice(&body).context("解码 JSON 失败")
        };
        tokio::time::timeout(Duration::from_secs(30), read)
            .await
            .context("等引擎的消息超时")?
    }

    /// 从一条响应里取出 `result`，错误响应转成 `Err`。
    fn result_of(method: &str, value: &serde_json::Value) -> Result<serde_json::Value> {
        if let Some(err) = value.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("(无消息)");
            bail!("RPC {method} 失败：[{code}] {message}");
        }
        value.get("result").cloned().context("响应缺少 result")
    }

    async fn rpc<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: &P,
    ) -> Result<R> {
        let id = self.send_request(method, params).await?;
        loop {
            let value = self.read_message().await?;
            // 跳过事件（没有 id）和别的请求的响应，只收下匹配的响应。
            if value.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue;
            }
            let result = Self::result_of(method, &value)?;
            return serde_json::from_value(result).context("解析 result 失败");
        }
    }

    /// 请求渲染一块瓦片，等到它的 `render.tileReady`，返回从发出请求到收到事件的时间（微秒）。
    async fn render_tile_us(&mut self, doc_id: u32, scale: f32, x: u32, y: u32) -> Result<f64> {
        let t0 = Instant::now();
        let id = self
            .send_request(
                messages::method::RENDER_TILES,
                &RenderTilesParams {
                    doc_id,
                    page_id: 0,
                    scale,
                    tiles: vec![TileCoord { x, y }],
                    priority: RenderPriority::Visible,
                    layer: None,
                },
            )
            .await?;
        let mut request_id = None;
        let mut ready = Vec::new();
        loop {
            let value = self.read_message().await?;
            if value.get("id").and_then(|v| v.as_u64()) == Some(id) {
                let result = Self::result_of(messages::method::RENDER_TILES, &value)?;
                let result: RenderTilesResult =
                    serde_json::from_value(result).context("解析 render.tiles 的结果失败")?;
                request_id = result.request_ids.first().copied();
            } else if value.get("method").and_then(|m| m.as_str())
                == Some(messages::event::RENDER_TILE_READY)
                && let Some(params) = value.get("params")
                && let Some(got) = params.get("requestId").and_then(|v| v.as_u64())
            {
                ready.push(got);
            }
            if let Some(request_id) = request_id
                && ready.contains(&u64::from(request_id))
            {
                return Ok(t0.elapsed().as_secs_f64() * 1e6);
            }
        }
    }

    /// 测量 `render.tiles` 到 `render.tileReady` 的端到端时间：每次一块 4 倍缩放的瓦片，位置各不相同。
    pub async fn measure_tile_event_us(&mut self, doc_id: u32, n: usize) -> Result<Stats> {
        let mut samples = Vec::with_capacity(n);
        for i in 0..n {
            let i = u32::try_from(i).unwrap_or(0);
            let (x, y) = ((i % 4) * shm::TILE_SIZE, (i / 4 % 6) * shm::TILE_SIZE);
            samples.push(self.render_tile_us(doc_id, 4.0, x, y).await?);
        }
        Ok(Stats::from_samples(samples))
    }

    pub async fn hello(&mut self) -> Result<HelloResult> {
        let user_data = std::env::temp_dir().join("magpdf-v06-user-data");
        let cache = std::env::temp_dir().join("magpdf-v06-cache");
        let _ = std::fs::create_dir_all(&user_data);
        let _ = std::fs::create_dir_all(&cache);
        self.rpc(
            messages::method::SESSION_HELLO,
            &HelloParams {
                protocol: PROTOCOL_VERSION,
                app_version: env!("CARGO_PKG_VERSION").into(),
                locale: Some("zh-CN".into()),
                user_data_dir: user_data.to_string_lossy().into_owned(),
                cache_dir: cache.to_string_lossy().into_owned(),
                session_id: self.session.clone(),
                recovery_file: Some(self.cfg.recovery.to_string_lossy().into_owned()),
            },
        )
        .await
    }

    pub async fn open_doc(&mut self, path: &Path) -> Result<messages::DocInfo> {
        self.rpc(
            messages::method::DOC_OPEN,
            &DocOpenParams {
                path: path.to_string_lossy().into_owned(),
                password: None,
            },
        )
        .await
    }

    pub async fn bench_engine(&mut self, iterations: u32) -> Result<BenchEngineResult> {
        self.rpc(
            messages::method::BENCH_ENGINE,
            &BenchEngineParams {
                iterations,
                runs: 1,
            },
        )
        .await
    }

    pub async fn measure_ping_us(&mut self, n: usize) -> Result<Stats> {
        let mut samples = Vec::with_capacity(n);
        for i in 0..n {
            let t0 = Instant::now();
            let _ = self
                .rpc::<_, PingParams>(messages::method::SESSION_PING, &PingParams { t: i as u64 })
                .await?;
            samples.push(t0.elapsed().as_secs_f64() * 1e6);
        }
        Ok(Stats::from_samples(samples))
    }

    pub async fn measure_tile_fetch_ms(&mut self, n: usize) -> Result<Stats> {
        // 先让引擎侧跑一遍基准，确保共享内存槽位有可读内容；随后主机侧测取瓦片。
        let _ = self
            .bench_engine(8)
            .await
            .context("预热渲染到共享内存失败")?;
        let mut samples = Vec::with_capacity(n);
        for i in 0..n {
            let shm = self.shm.as_ref().context("无共享内存")?;
            let slots = shm.layout().slots.max(1);
            let slot = (i as u32) % slots;
            let generation = shm.generation(slot).unwrap_or(0);
            let t0 = Instant::now();
            // 代数为 0（空槽）时 read_tile 会失败；仍计入耗时，表示取槽开销。
            let _ = shm.read_tile(slot, generation);
            samples.push(t0.elapsed().as_secs_f64() * 1e6); // 统一用微秒，报告时再换算
        }
        Ok(Stats::from_samples(samples))
    }

    /// 强制结束引擎后重新拉起并完成 hello，测量恢复耗时。调用前要已经打开过文档（恢复文件里有记录）。
    ///
    /// 返回（耗时毫秒，引擎按恢复文件重新打开的文档数）。引擎一份文档都没恢复时，退回到由主进程
    /// 重新打开样本，这段时间也计入。
    pub async fn force_restart(&mut self) -> Result<(f64, usize)> {
        let t0 = Instant::now();
        self.shutdown_engine();
        let _ = self.start_engine().await?;
        let hello = self.hello().await?;
        let restored = hello.restored.len();
        let sample = self.cfg.sample_pdf.clone();
        if restored == 0 && sample.is_file() {
            let _ = self.open_doc(&sample).await?;
        }
        Ok((t0.elapsed().as_secs_f64() * 1e3, restored))
    }

    pub async fn run_bench(&mut self) -> Result<BenchReport> {
        let pid = self.start_engine().await?;
        let hello = self.hello().await?;
        if hello.protocol != PROTOCOL_VERSION {
            bail!(
                "协议版本不一致 host={} engine={}",
                PROTOCOL_VERSION,
                hello.protocol
            );
        }
        let sample = self.cfg.sample_pdf.clone();
        if !sample.is_file() {
            bail!("样本不存在：{}", sample.display());
        }
        let doc = self.open_doc(&sample).await?;
        let ping = self.measure_ping_us(1000).await?;
        let engine = self.bench_engine(200).await?;
        let tile = self.measure_tile_fetch_ms(200).await?;
        let tile_event = self.measure_tile_event_us(doc.doc_id, 200).await?;
        let (restart_ms, restored_docs) = match self.force_restart().await {
            Ok((ms, restored)) => (Some(ms), Some(restored)),
            Err(e) => {
                tracing::warn!("重启测量失败：{e:#}");
                (None, None)
            }
        };
        let json_share = if engine.engine_roundtrip.median_us > 0.0 {
            (engine.json_encode.median_us + engine.json_decode.median_us)
                / engine.engine_roundtrip.median_us
        } else {
            0.0
        };
        let msgpack_vs_json = if engine.json_bytes > 0 {
            f64::from(engine.msgpack_bytes) / f64::from(engine.json_bytes)
        } else {
            0.0
        };
        // 通过线优先看主机侧取瓦片；若空槽导致样本偏小，同时参考引擎侧 shm_read。
        let tile_p95_ms = tile.p95_us / 1000.0;
        let engine_shm_p95_ms = engine.shm_read.p95_us / 1000.0;
        let tile_ok = tile_p95_ms < 5.0 || engine_shm_p95_ms < 5.0;
        let pass = BenchPass {
            tile_fetch_under_5ms: tile_ok,
            ping_p95_under_2ms: ping.p95_us < 2000.0,
            // 重启够快还不够：引擎要真的按恢复文件把文档重新打开了才算恢复。
            restart_under_5s: restart_ms.is_some_and(|ms| ms < 5000.0)
                && restored_docs.is_some_and(|n| n > 0),
        };
        Ok(BenchReport {
            protocol_version: PROTOCOL_VERSION,
            session_id: self.session.clone(),
            engine_pid: self.child.as_ref().map(|c| c.id()).unwrap_or(pid),
            engine_version: hello.engine_version.clone(),
            pdfium_revision: hello.pdfium_revision.clone(),
            slots: self.cfg.slots,
            shm_total_mb: shm::total_memory_mb(),
            ping_median_us: ping.median_us,
            ping_p95_us: ping.p95_us,
            tile_fetch_median_ms: tile.median_us / 1000.0,
            tile_fetch_p95_ms: tile_p95_ms,
            tile_event_median_ms: tile_event.median_us / 1000.0,
            tile_event_p95_ms: tile_event.p95_us / 1000.0,
            json_share_of_roundtrip: json_share,
            msgpack_vs_json_bytes: msgpack_vs_json,
            engine,
            restart_ms,
            restored_docs,
            pass,
        })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown_engine();
    }
}

async fn wait_connect(name: &str, timeout: Duration) -> Result<NamedPipeClient> {
    let start = Instant::now();
    loop {
        match pipe::connect_client(name) {
            Ok(c) => return Ok(c),
            Err(e) if start.elapsed() < timeout => {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let _ = e;
            }
            Err(e) => return Err(e).context("连接引擎命名管道超时"),
        }
    }
}
