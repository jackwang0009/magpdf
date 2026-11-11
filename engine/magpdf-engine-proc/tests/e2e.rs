//! 引擎进程的端到端测试：启动 `magpdf-engine` 可执行文件，像主进程那样用命名管道和共享内存与它对话
//! （《接口协议》第 3、4、6、7 节）。每个测试起一个自己的引擎进程，管道名和会话 ID 各不相同。
// 测试代码允许 unwrap/expect（《工程规范》4）；集成测试不在 #[cfg(test)] 下，需显式放开。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use magpdf_ipc::messages::{ErrorCode, PROTOCOL_VERSION, event, method};
use magpdf_ipc::{frame, pipe, shm};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;

/// 单次等待的上限。正常情况下每一步都是毫秒级，超时说明引擎卡住了。
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

fn sample(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/synthetic")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// 一个被测的引擎进程，加上测试这一侧（扮演主进程）的连接。
struct Engine {
    child: Child,
    /// 测试这一侧的管道句柄；`hang_up` 之后是 `None`。
    client: Option<NamedPipeClient>,
    session: String,
    recovery: PathBuf,
    next_id: u64,
    /// 已经读到、还没被取走的响应：`(id, 到达时刻, result 或 error)`。
    responses: Vec<(u64, Instant, Result<Value, Value>)>,
    /// 已经读到、还没被取走的事件：`(到达时刻, 事件名, 参数)`。
    events: Vec<(Instant, String, Value)>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.recovery);
    }
}

impl Engine {
    /// 启动引擎并连上管道。`recovery` 为 `None` 时用一个本测试独有的恢复文件。
    async fn start(recovery: Option<PathBuf>) -> Self {
        let token = pipe::random_token(8).unwrap();
        let pipe_name = pipe::pipe_name(std::process::id(), &token);
        let session = format!("e2e-{}-{token}", std::process::id());
        let recovery =
            recovery.unwrap_or_else(|| std::env::temp_dir().join(format!("magpdf-{session}.json")));
        let child = Command::new(env!("CARGO_BIN_EXE_magpdf-engine"))
            .args(["--pipe", &pipe_name, "--session", &session, "--slots", "16"])
            .arg("--recovery")
            .arg(&recovery)
            .env("MAGPDF_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("启动 magpdf-engine");
        let started = Instant::now();
        let client = loop {
            match pipe::connect_client(&pipe_name) {
                Ok(client) => break client,
                Err(e) if started.elapsed() < STEP_TIMEOUT => {
                    let _ = e;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("连不上引擎的管道：{e}"),
            }
        };
        Self {
            child,
            client: Some(client),
            session,
            recovery,
            next_id: 1,
            responses: Vec::new(),
            events: Vec::new(),
        }
    }

    fn pipe(&mut self) -> &mut NamedPipeClient {
        self.client.as_mut().expect("管道已经关掉了")
    }

    /// 关掉测试这一侧的管道句柄。在引擎看来，这和主进程被结束是一回事。
    fn hang_up(&mut self) {
        self.client = None;
    }

    /// 发一条请求，不等响应。
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let framed = frame::encode(&request).unwrap();
        self.pipe().write_all(&framed).await.unwrap();
        id
    }

    /// 从管道读一条消息，按种类记进 `responses` 或 `events`。
    async fn read_one(&mut self) {
        let read = async {
            let mut len = [0u8; 4];
            self.pipe().read_exact(&mut len).await.unwrap();
            let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
            self.pipe().read_exact(&mut body).await.unwrap();
            body
        };
        let body = tokio::time::timeout(STEP_TIMEOUT, read)
            .await
            .expect("等引擎的消息超时");
        let at = Instant::now();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["jsonrpc"], "2.0", "{value}");
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            let outcome = match value.get("error") {
                Some(error) => Err(error.clone()),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            self.responses.push((id, at, outcome));
        } else {
            let name = value["method"]
                .as_str()
                .expect("事件要有 method")
                .to_owned();
            self.events.push((at, name, value["params"].clone()));
        }
    }

    fn has_response(&self, id: u64) -> bool {
        self.responses.iter().any(|(got, ..)| *got == id)
    }

    /// 等某条请求的响应，返回到达时刻和结果。
    async fn response(&mut self, id: u64) -> (Instant, Result<Value, Value>) {
        loop {
            if let Some(i) = self.responses.iter().position(|(got, ..)| *got == id) {
                let (_, at, outcome) = self.responses.remove(i);
                return (at, outcome);
            }
            self.read_one().await;
        }
    }

    /// 发请求并等成功的响应。
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        match self.response(id).await.1 {
            Ok(result) => result,
            Err(error) => panic!("{method} 失败：{error}"),
        }
    }

    /// 发请求并等错误响应。
    async fn call_err(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        match self.response(id).await.1 {
            Ok(result) => panic!("{method} 应该失败，却返回了 {result}"),
            Err(error) => error,
        }
    }

    /// 等一条指定名字的事件，返回到达时刻和参数。
    async fn event(&mut self, name: &str) -> (Instant, Value) {
        loop {
            if let Some(i) = self.events.iter().position(|(_, got, _)| got == name) {
                let (at, _, params) = self.events.remove(i);
                return (at, params);
            }
            self.read_one().await;
        }
    }

    async fn hello(&mut self) -> Value {
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        self.call(
            method::SESSION_HELLO,
            json!({
                "protocol": PROTOCOL_VERSION,
                "appVersion": env!("CARGO_PKG_VERSION"),
                "locale": "zh-CN",
                "userDataDir": dir,
                "cacheDir": dir,
                "sessionId": self.session,
                "recoveryFile": self.recovery.to_string_lossy(),
            }),
        )
        .await
    }

    async fn open(&mut self, name: &str) -> u64 {
        let doc = self
            .call(method::DOC_OPEN, json!({ "path": sample(name) }))
            .await;
        doc["docId"].as_u64().expect("docId")
    }

    /// 请求渲染一块瓦片，等它的 `render.tileReady`。返回（从发请求到事件到达的时间，事件参数）。
    async fn render_tile(&mut self, doc_id: u64, scale: f64, x: u32, y: u32) -> (Duration, Value) {
        let sent = Instant::now();
        let result = self
            .call(
                method::RENDER_TILES,
                json!({
                    "docId": doc_id,
                    "pageId": 0,
                    "scale": scale,
                    "tiles": [{ "x": x, "y": y }],
                    "priority": "visible",
                }),
            )
            .await;
        let request_id = result["requestIds"][0].as_u64().expect("requestIds");
        let (at, ready) = self.event(event::RENDER_TILE_READY).await;
        assert_eq!(ready["requestId"], request_id, "{ready}");
        (at.duration_since(sent), ready)
    }

    /// 第一页的全部对象（`obj.list`），按层级从下到上。
    async fn objects(&mut self, doc_id: u64) -> Vec<Value> {
        let list = self
            .call(method::OBJ_LIST, json!({ "docId": doc_id, "pageId": 0 }))
            .await;
        list.as_array().expect("obj.list 返回数组").clone()
    }

    /// 发一条修改命令（或撤销、重做），取回新的版本号和随之而来的 `doc.changed`。
    async fn edit(&mut self, method: &str, params: Value) -> (u64, Value) {
        let result = self.call(method, params).await;
        let version = result["version"].as_u64().expect("version");
        let (_, changed) = self.event(event::DOC_CHANGED).await;
        assert_eq!(changed["version"], version, "{changed}");
        (version, changed)
    }

    /// 恢复文件里记了几份文档。
    fn recovery_docs(&self) -> usize {
        let Ok(bytes) = std::fs::read(&self.recovery) else {
            return 0;
        };
        let state: Value = serde_json::from_slice(&bytes).unwrap();
        state["docs"].as_array().map_or(0, Vec::len)
    }
}

#[tokio::test]
async fn hello_open_render_and_read_the_tile_from_shared_memory() {
    let mut engine = Engine::start(None).await;
    let hello = engine.hello().await;
    assert_eq!(hello["protocol"], PROTOCOL_VERSION);
    assert_eq!(hello["restored"], json!([]));
    assert!(
        hello["pdfiumRevision"]
            .as_str()
            .is_some_and(|s| s.starts_with("pdfium ")),
        "{hello}"
    );

    let doc = engine
        .call(
            method::DOC_OPEN,
            json!({ "path": sample("freight-bl-composite.pdf") }),
        )
        .await;
    assert_eq!(doc["pageCount"], 1, "{doc}");
    let doc_id = doc["docId"].as_u64().unwrap();

    // 1 倍缩放下左上角那块瓦片盖住大半页，里面一定有内容。
    let region = shm::open(&engine.session).unwrap();
    let (_, ready) = engine.render_tile(doc_id, 1.0, 0, 0).await;
    assert_eq!(
        (&ready["docId"], &ready["pageId"], &ready["x"], &ready["y"]),
        (&json!(doc_id), &json!(0), &json!(0), &json!(0)),
        "{ready}"
    );
    assert_eq!(ready["scale"], 1.0);
    assert_eq!(ready["version"], 0);
    let slot = u32::try_from(ready["slot"].as_u64().unwrap()).unwrap();
    let generation = u32::try_from(ready["generation"].as_u64().unwrap()).unwrap();
    let pixels = region.read_tile(slot, generation).unwrap();
    assert_eq!(pixels.len(), shm::TILE_BYTES);
    let inked = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] < 250 || px[1] < 250 || px[2] < 250)
        .count();
    assert!(
        inked > 1000,
        "瓦片里应该有页面内容，非白像素只有 {inked} 个"
    );

    // 代数对不上的读取被拒绝，主进程据此返回 410。
    assert!(region.read_tile(slot, generation + 2).is_err());
    let stale = engine
        .call_err(
            method::TILE_FETCH,
            json!({ "slot": slot, "generation": generation + 2 }),
        )
        .await;
    assert_eq!(stale["code"], ErrorCode::TileGenerationStale.code());

    // 不在左上角的瓦片也要取对地方：这块落在表格中间，一定有内容（平移方向反了的话整块是白的）。
    // 缩放倍数原样用、原样带回：界面的缩放档是 √2 的幂乘 96/72，不是整的千分位。
    let scale = 3.771_236_f32;
    let (_, ready) = engine.render_tile(doc_id, f64::from(scale), 512, 512).await;
    assert_eq!((&ready["x"], &ready["y"]), (&json!(512), &json!(512)));
    assert_eq!(ready["scale"].as_f64().map(|s| s as f32), Some(scale));
    let slot = u32::try_from(ready["slot"].as_u64().unwrap()).unwrap();
    let generation = u32::try_from(ready["generation"].as_u64().unwrap()).unwrap();
    let pixels = region.read_tile(slot, generation).unwrap();
    let inked = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] < 250 || px[1] < 250 || px[2] < 250)
        .count();
    assert!(
        inked > 1000,
        "(512, 512) 那块瓦片里应该有页面内容，非白像素只有 {inked} 个"
    );

    // 缩放倍数不是正数：直接拒绝，不排渲染。
    for scale in [0.0, -1.0] {
        let invalid = engine
            .call_err(
                method::RENDER_TILES,
                json!({
                    "docId": doc_id,
                    "pageId": 0,
                    "scale": scale,
                    "tiles": [{ "x": 0, "y": 0 }],
                    "priority": "visible",
                }),
            )
            .await;
        assert_eq!(invalid["code"], ErrorCode::InvalidArgument.code());
    }
}

/// `doc.snapshot` 给出每一页的显示尺寸：旋转 90° 和 270° 的页宽高互换。
#[tokio::test]
async fn doc_snapshot_lists_pages_in_display_orientation() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;
    let doc_id = engine.open("struct-page-rotate.pdf").await;
    let snapshot = engine
        .call(method::DOC_SNAPSHOT, json!({ "docId": doc_id }))
        .await;
    assert_eq!(
        snapshot,
        json!({
            "version": 0,
            "pages": [
                { "pageId": 0, "index": 0, "width": 200.0, "height": 300.0 },
                { "pageId": 1, "index": 1, "width": 300.0, "height": 200.0 },
                { "pageId": 2, "index": 2, "width": 200.0, "height": 300.0 },
            ],
        })
    );
    let missing = engine
        .call_err(method::DOC_SNAPSHOT, json!({ "docId": doc_id + 100 }))
        .await;
    assert_eq!(missing["code"], ErrorCode::InvalidArgument.code());
}

/// 瓦片渲染完，事件要马上发出去，不能等读循环的定时器（修复前最坏要等 500 ms）。
#[tokio::test]
async fn tile_ready_event_is_not_held_back_on_an_idle_pipe() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;
    let doc_id = engine.open("freight-bl-composite.pdf").await;
    let mut worst = Duration::ZERO;
    for i in 0..6u32 {
        // 每次都等上一块完全结束再发下一块，让管道在渲染期间是空闲的。
        let (elapsed, _) = engine.render_tile(doc_id, 4.0, i * 512, 512).await;
        worst = worst.max(elapsed);
    }
    assert!(
        worst < Duration::from_millis(250),
        "从发出 render.tiles 到收到 render.tileReady 最慢 {worst:?}"
    );
}

/// PDFium 线程忙着的时候，心跳照样有应答（《接口协议》第 4 节）。
#[tokio::test]
async fn ping_is_answered_while_pdfium_is_busy() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;
    engine.open("freight-bl-composite.pdf").await;

    // bench.engine 要在 PDFium 线程上渲染几百块瓦片，足够慢。
    let iterations = 600;
    let bench = engine
        .send(
            method::BENCH_ENGINE,
            json!({ "runs": 1, "iterations": iterations }),
        )
        .await;
    let mut worst = Duration::ZERO;
    for t in 0..5u64 {
        let sent = Instant::now();
        let id = engine.send(method::SESSION_PING, json!({ "t": t })).await;
        let (at, pong) = engine.response(id).await;
        assert_eq!(pong.unwrap()["t"], t);
        worst = worst.max(at.duration_since(sent));
    }
    assert!(
        worst < Duration::from_millis(100),
        "PDFium 忙的时候 ping 最慢 {worst:?}"
    );
    // bench.engine 要是已经结束了，上面的断言就什么也没验证到。
    assert!(
        !engine.has_response(bench),
        "5 次 ping 回来的时候 bench.engine 应该还在跑，否则要加大 iterations"
    );

    let result = engine.response(bench).await.1.unwrap();
    assert_eq!(result["renderTile"]["samples"], iterations);
}

#[tokio::test]
async fn doc_close_forgets_the_document() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;
    let doc_id = engine.open("freight-bl-composite.pdf").await;
    assert_eq!(engine.recovery_docs(), 1);

    let closed = engine
        .call(
            method::DOC_CLOSE,
            json!({ "docId": doc_id, "discard": false }),
        )
        .await;
    assert_eq!(closed, Value::Null);
    assert_eq!(
        engine.recovery_docs(),
        0,
        "关闭后恢复文件里不该再有这份文档"
    );

    let again = engine
        .call_err(method::DOC_CLOSE, json!({ "docId": doc_id }))
        .await;
    assert_eq!(again["code"], ErrorCode::InvalidArgument.code());
}

/// 对象的包围盒 `[left, bottom, right, top]`。
fn bbox(object: &Value) -> [f64; 4] {
    let b = object["bbox"].as_array().expect("bbox");
    [0, 1, 2, 3].map(|i| b[i].as_f64().expect("bbox 分量"))
}

fn by_id(objects: &[Value], id: u64) -> &Value {
    objects
        .iter()
        .find(|o| o["id"] == id)
        .unwrap_or_else(|| panic!("对象 {id} 不在列表里"))
}

/// 列对象、点选、变换、删除、改填充色、撤销和重做，都经管道走一遍（《接口协议》6.4、6.5、6.8）。
#[tokio::test]
async fn edit_commands_change_the_document_and_can_be_undone() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;
    let doc_id = engine.open("freight-bl-composite.pdf").await;

    let objects = engine.objects(doc_id).await;
    assert_eq!(objects.len(), 44);
    for (z, object) in objects.iter().enumerate() {
        assert_eq!(object["z"], z, "按层级从下到上：{object}");
        assert_eq!(object["locked"], false);
    }
    let tail = engine
        .call(
            method::OBJ_LIST,
            json!({ "docId": doc_id, "pageId": 0, "offset": 40, "limit": 10 }),
        )
        .await;
    assert_eq!(tail.as_array().unwrap()[..], objects[40..]);

    // 取最上面的一个文字对象，在它中心点选。
    let target = objects
        .iter()
        .rev()
        .find(|o| o["kind"] == "text")
        .expect("样本里有文字对象");
    let id = target["id"].as_u64().unwrap();
    let [left, bottom, right, top] = bbox(target);
    let hits = engine
        .call(
            method::OBJ_HIT_TEST,
            json!({
                "docId": doc_id,
                "pageId": 0,
                "point": [(left + right) / 2.0, (bottom + top) / 2.0],
                "mode": "all",
            }),
        )
        .await;
    assert!(
        hits.as_array().unwrap().iter().any(|h| h["id"] == id),
        "{hits}"
    );

    // 右移 5 pt。
    let (version, changed) = engine
        .edit(
            method::EDIT_TRANSFORM,
            json!({
                "docId": doc_id,
                "baseVersion": 0,
                "ids": [id],
                "matrix": [1, 0, 0, 1, 5, 0],
                "clipFollows": true,
            }),
        )
        .await;
    assert_eq!(version, 1);
    assert_eq!(
        changed,
        json!({
            "docId": doc_id,
            "version": 1,
            "changes": [{ "pageId": 0, "added": [], "removed": [], "modified": [id] }],
            "pagesChanged": false,
        })
    );
    let moved = engine.objects(doc_id).await;
    assert!(
        (bbox(by_id(&moved, id))[0] - left - 5.0).abs() < 0.6,
        "{} -> {}",
        target,
        by_id(&moved, id)
    );

    // 基于旧版本的命令被拒绝，文档不变。
    let stale = engine
        .call_err(
            method::EDIT_DELETE,
            json!({ "docId": doc_id, "baseVersion": 0, "ids": [id] }),
        )
        .await;
    assert_eq!(stale["code"], ErrorCode::VersionConflict.code(), "{stale}");
    let ghost = engine
        .call_err(
            method::EDIT_DELETE,
            json!({ "docId": doc_id, "baseVersion": 1, "ids": [id, 999_999] }),
        )
        .await;
    assert_eq!(ghost["code"], ErrorCode::ObjectNotFound.code(), "{ghost}");
    assert_eq!(engine.objects(doc_id).await.len(), 44);

    // 删除，再撤销：对象回到原来的层级，编号不变。
    let (version, changed) = engine
        .edit(
            method::EDIT_DELETE,
            json!({ "docId": doc_id, "baseVersion": 1, "ids": [id] }),
        )
        .await;
    assert_eq!(version, 2);
    assert_eq!(changed["changes"][0]["removed"], json!([id]));
    assert_eq!(engine.objects(doc_id).await.len(), 43);

    let (version, changed) = engine
        .edit(method::HISTORY_UNDO, json!({ "docId": doc_id }))
        .await;
    assert_eq!(version, 3);
    assert_eq!(changed["changes"][0]["added"], json!([id]));
    let restored = engine.objects(doc_id).await;
    assert_eq!(restored.len(), 44);
    assert_eq!(by_id(&restored, id)["z"], target["z"]);

    let (version, changed) = engine
        .edit(method::HISTORY_REDO, json!({ "docId": doc_id }))
        .await;
    assert_eq!(
        (version, &changed["changes"][0]["removed"]),
        (4, &json!([id]))
    );
    let (version, _) = engine
        .edit(method::HISTORY_UNDO, json!({ "docId": doc_id }))
        .await;
    assert_eq!(version, 5);

    // 把一个路径对象的填充改成红色。带了还不支持的样式字段要明确拒绝。
    let path = objects
        .iter()
        .find(|o| o["kind"] == "path")
        .expect("样本里有路径对象")["id"]
        .as_u64()
        .unwrap();
    let (version, changed) = engine
        .edit(
            method::EDIT_SET_STYLE,
            json!({
                "docId": doc_id,
                "baseVersion": 5,
                "ids": [path],
                "fill": { "space": "rgb", "components": [1.0, 0.0, 0.0] },
            }),
        )
        .await;
    assert_eq!(version, 6);
    assert_eq!(changed["changes"][0]["modified"], json!([path]));
    let unsupported = engine
        .call_err(
            method::EDIT_SET_STYLE,
            json!({
                "docId": doc_id,
                "baseVersion": 6,
                "ids": [path],
                "lineWidth": 2,
            }),
        )
        .await;
    assert_eq!(unsupported["code"], ErrorCode::InvalidArgument.code());
    // 新命令清空了重做栈。
    let nothing = engine
        .call_err(method::HISTORY_REDO, json!({ "docId": doc_id }))
        .await;
    assert_eq!(nothing["code"], ErrorCode::InvalidArgument.code());

    // 之后渲染的瓦片带着当前的文档版本。
    let (_, ready) = engine.render_tile(doc_id, 1.0, 0, 0).await;
    assert_eq!(ready["version"], 6);
}

/// 生成器写不出渐变（方案 2.1），带渐变的页一改就会丢对象，所以修改命令要被拒绝（方案 2.3）。
/// 生成器以后修好了，这一页就应该能正常编辑，两种结果这里都接受。
#[tokio::test]
async fn pages_the_generator_cannot_rewrite_refuse_edits() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;
    let doc_id = engine.open("shading-axial.pdf").await;
    let objects = engine.objects(doc_id).await;
    let id = objects[0]["id"].as_u64().unwrap();

    let request = engine
        .send(
            method::EDIT_TRANSFORM,
            json!({ "docId": doc_id, "baseVersion": 0, "ids": [id], "matrix": [1, 0, 0, 1, 3, 0] }),
        )
        .await;
    match engine.response(request).await.1 {
        Err(error) => {
            assert_eq!(error["code"], ErrorCode::PageOverlayOnly.code(), "{error}");
            // 版本号没有动：基于第 0 版的命令还是因为同一个原因被拒绝，而不是版本冲突。
            let again = engine
                .call_err(
                    method::EDIT_DELETE,
                    json!({ "docId": doc_id, "baseVersion": 0, "ids": [id] }),
                )
                .await;
            assert_eq!(again["code"], ErrorCode::PageOverlayOnly.code());
        }
        Ok(result) => assert_eq!(result["version"], 1),
    }
    let after = engine.objects(doc_id).await;
    assert_eq!(
        after.iter().map(|o| &o["id"]).collect::<Vec<_>>(),
        objects.iter().map(|o| &o["id"]).collect::<Vec<_>>()
    );
}

/// `doc.open` 的失败用《接口协议》第 8 节的错误码区分。
#[tokio::test]
async fn doc_open_failures_use_the_protocol_error_codes() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;

    let missing = engine
        .call_err(
            method::DOC_OPEN,
            json!({ "path": sample("no-such-file.pdf") }),
        )
        .await;
    assert_eq!(missing["code"], ErrorCode::FileNotFound.code(), "{missing}");

    let garbage = std::env::temp_dir().join(format!("magpdf-{}-garbage.pdf", engine.session));
    std::fs::write(&garbage, b"this is not a pdf").unwrap();
    let damaged = engine
        .call_err(
            method::DOC_OPEN,
            json!({ "path": garbage.to_string_lossy() }),
        )
        .await;
    let _ = std::fs::remove_file(&garbage);
    assert_eq!(damaged["code"], ErrorCode::FileDamaged.code(), "{damaged}");

    assert_eq!(engine.recovery_docs(), 0, "打不开的文件不该记进恢复文件");
}

/// 引擎被强制结束后，新引擎按恢复文件把文档重新打开，并在 `session.hello` 里报告（方案 9.3）。
#[tokio::test]
async fn a_new_engine_reopens_documents_from_the_recovery_file() {
    let recovery = std::env::temp_dir().join(format!(
        "magpdf-e2e-recovery-{}-{}.json",
        std::process::id(),
        pipe::random_token(6).unwrap()
    ));
    let mut first = Engine::start(Some(recovery.clone())).await;
    assert_eq!(first.hello().await["restored"], json!([]));
    first.open("freight-bl-composite.pdf").await;
    // 结束进程，但留下恢复文件。
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    first.recovery = PathBuf::new();
    drop(first);

    let mut second = Engine::start(Some(recovery)).await;
    let hello = second.hello().await;
    let restored = hello["restored"].as_array().expect("restored");
    assert_eq!(restored.len(), 1, "{hello}");
    assert_eq!(restored[0]["path"], sample("freight-bl-composite.pdf"));
    assert_eq!(restored[0]["pageCount"], 1);

    // 恢复出来的文档直接可用。
    let doc_id = restored[0]["docId"].as_u64().unwrap();
    let (_, ready) = second.render_tile(doc_id, 1.0, 0, 0).await;
    assert_eq!(ready["docId"], doc_id);
}

#[tokio::test]
async fn unknown_method_is_rejected_and_shutdown_exits_cleanly() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;
    let error = engine.call_err("no.such.method", json!({})).await;
    assert_eq!(error["code"], ErrorCode::Protocol.code());

    let bad = engine
        .call_err(method::DOC_OPEN, json!({ "path": 42 }))
        .await;
    assert_eq!(bad["code"], ErrorCode::InvalidArgument.code());

    assert_eq!(engine.call(method::SESSION_SHUTDOWN, json!({})).await, true);
    let deadline = Instant::now() + STEP_TIMEOUT;
    let status = loop {
        if let Some(status) = engine.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "session.shutdown 之后引擎没有退出"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(status.success(), "{status}");
}

/// 主进程没了（崩溃或被结束，管道随之断开），引擎要自己退出。管道名里有主进程的 PID 和随机串，
/// 不会再有人连上来；留下来等只会变成没人管的进程，还占着文档。
#[tokio::test]
async fn the_engine_exits_when_the_host_goes_away() {
    let mut engine = Engine::start(None).await;
    engine.hello().await;
    engine.open("freight-bl-composite.pdf").await;

    engine.hang_up();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = engine.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "管道断开 5 秒之后引擎还在运行");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(status.success(), "{status}");
}
