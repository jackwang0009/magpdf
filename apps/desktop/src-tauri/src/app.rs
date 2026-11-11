//! Tauri 外壳：窗口里用系统的 WebView2 跑界面，这里把界面和引擎接起来（方案 3.1；《接口协议》第 2、3 节）。
//!
//! 界面到主进程有四条路：
//! - 命令 `engine_request`：文档类请求原样转给引擎，不解析业务字段。`session.*`、`bench.*`、`tile.fetch`
//!   是主进程自己用的，`doc.open` 带着路径，都不让界面直接调。
//! - 命令 `open_document_dialog`：主进程弹“打开”对话框，用户选了文件就交给引擎打开。界面拿不到任意路径。
//! - 命令 `open_documents`：主进程知道的、已经开着的文档（启动时命令行给的，或者界面重新加载之前打开的）。
//! - 自定义协议 `magpdf://tile/<槽位>/<代数>`：从共享内存取一块瓦片的 BGRA 像素（WebView2 里的地址是
//!   `http://magpdf.localhost/tile/<槽位>/<代数>`）。
//!
//! 主进程到界面只有一个 Tauri 事件 [`ENGINE_EVENT`]，载荷是 `{ method, params }`：引擎推送的事件原样转发；
//! 引擎重启时主进程自己发 `engine.restarting`、`engine.restored`、`engine.failed`。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use magpdf_ipc::host::{EngineLaunch, HostError, HostEvent, Supervisor};
use magpdf_ipc::messages::{
    DocInfo, EngineFailed, EngineRestored, ErrorCode, RpcError, event, method,
};
use magpdf_ipc::shm::ShmError;
use serde_json::{Value, json};
use tauri::http::{Method, Request, Response, StatusCode, header};
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, State, UriSchemeContext, UriSchemeResponder,
    WebviewWindow,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// 界面收引擎事件用的 Tauri 事件名。Tauri 的事件名里不能有点号，所以协议里的事件名放在载荷的 `method` 里。
const ENGINE_EVENT: &str = "engine-event";

/// 取瓦片的自定义协议名（《接口协议》3.3）。
const TILE_SCHEME: &str = "magpdf";

/// 主进程这一侧的状态，交给 Tauri 托管。
struct Shell {
    supervisor: Supervisor,
    /// 引擎里开着的文档。界面重新加载之后靠它把文档找回来；引擎重启之后换成新引擎恢复出来的那些。
    docs: Mutex<Vec<DocInfo>>,
    /// 允许读瓦片的页面来源（`scheme://host[:port]`）。
    origins: Vec<String>,
    /// 恢复文件，正常退出时删掉。
    recovery: PathBuf,
}

impl Shell {
    /// 让引擎打开一份文档。同一个文件已经开着就直接返回它，不开第二份。
    async fn open(&self, path: &Path) -> Result<DocInfo, RpcError> {
        let path = path.to_string_lossy().into_owned();
        if let Some(open) = self
            .docs()
            .into_iter()
            .find(|doc| doc.path.eq_ignore_ascii_case(&path))
        {
            return Ok(open);
        }
        let value = self
            .supervisor
            .request(method::DOC_OPEN, json!({ "path": path }))
            .await?;
        let info: DocInfo = serde_json::from_value(value)
            .map_err(|e| ErrorCode::Internal.error_with(format!("doc.open 的返回值看不懂：{e}")))?;
        if let Ok(mut docs) = self.docs.lock() {
            docs.push(info.clone());
        }
        Ok(info)
    }

    fn docs(&self) -> Vec<DocInfo> {
        self.docs
            .lock()
            .map(|docs| docs.clone())
            .unwrap_or_default()
    }

    fn forget(&self, doc_id: u64) {
        if let Ok(mut docs) = self.docs.lock() {
            docs.retain(|doc| u64::from(doc.doc_id) != doc_id);
        }
    }

    fn replace_docs(&self, restored: Vec<DocInfo>) {
        if let Ok(mut docs) = self.docs.lock() {
            *docs = restored;
        }
    }
}

/// 界面能不能直接调这个方法。
fn forwardable(name: &str) -> bool {
    !(name.starts_with("session.")
        || name.starts_with("bench.")
        || name == method::DOC_OPEN
        || name == method::TILE_FETCH)
}

/// 文档类请求：原样转给引擎，原样把结果或错误带回来。
#[tauri::command]
async fn engine_request(
    shell: State<'_, Shell>,
    method: String,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    if !forwardable(&method) {
        return Err(ErrorCode::Protocol.error_with(format!("界面不能直接调用 {method}")));
    }
    let params = params.unwrap_or_else(|| json!({}));
    // 只为了记账看一眼 doc.close 的 docId，别的字段不碰。
    let closing = (method == method::DOC_CLOSE)
        .then(|| params.get("docId").and_then(Value::as_u64))
        .flatten();
    let result = shell.supervisor.request(&method, params).await?;
    if let Some(doc_id) = closing {
        shell.forget(doc_id);
    }
    Ok(result)
}

/// 弹“打开”对话框；用户选了文件就让引擎打开，取消返回 `null`。
#[tauri::command]
async fn open_document_dialog(
    window: WebviewWindow,
    shell: State<'_, Shell>,
) -> Result<Option<DocInfo>, RpcError> {
    let picked = rfd::AsyncFileDialog::new()
        .set_title("打开")
        .add_filter("PDF 文档", &["pdf"])
        .set_parent(&window)
        .pick_file()
        .await;
    match picked {
        Some(file) => shell.open(file.path()).await.map(Some),
        None => Ok(None),
    }
}

/// 已经开着的文档。界面启动（或重新加载）时调一次。
#[tauri::command]
fn open_documents(shell: State<'_, Shell>) -> Vec<DocInfo> {
    shell.docs()
}

/// `magpdf://tile/<槽位>/<代数>`：读共享内存是一次 1 MB 的拷贝，放到后台线程上做，不占窗口的消息循环。
fn tile_protocol(
    ctx: UriSchemeContext<'_, tauri::Wry>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let shell = app.state::<Shell>();
        responder.respond(tile_response(&shell, &request));
    });
}

fn tile_response(shell: &Shell, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    // 只有应用自己的页面能读到像素：别的来源拿不到 CORS 许可，浏览器不会把响应交给它。
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .filter(|origin| shell.origins.iter().any(|allowed| allowed == origin));
    let respond = |status: StatusCode, body: Vec<u8>| {
        let mut builder = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CACHE_CONTROL, "no-store");
        if let Some(origin) = origin {
            builder = builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        }
        // 状态码和头都是合法的常量，build 不会失败；真失败了就给一个空的 500。
        builder.body(body).unwrap_or_else(|_| {
            let mut fallback = Response::new(Vec::new());
            *fallback.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            fallback
        })
    };

    if request.method() != Method::GET {
        return respond(StatusCode::METHOD_NOT_ALLOWED, Vec::new());
    }
    let Some((slot, generation)) = parse_tile_path(request.uri().path()) else {
        return respond(StatusCode::NOT_FOUND, Vec::new());
    };
    match shell.supervisor.read_tile(slot, generation) {
        Ok(pixels) => respond(StatusCode::OK, pixels),
        // 槽位已经被别的瓦片占了，或者引擎换了一个：界面重新请求渲染（《接口协议》3.3）。
        Err(ShmError::Generation { .. }) => respond(StatusCode::GONE, Vec::new()),
        Err(ShmError::SlotOutOfRange { .. }) => respond(StatusCode::NOT_FOUND, Vec::new()),
        Err(error) => {
            tracing::warn!("读瓦片失败：{error}");
            respond(StatusCode::INTERNAL_SERVER_ERROR, Vec::new())
        }
    }
}

/// `/tile/<槽位>/<代数>` 里的两个数。
fn parse_tile_path(path: &str) -> Option<(u32, u32)> {
    let mut parts = path.strip_prefix("/tile/")?.split('/');
    let slot = parts.next()?.parse().ok()?;
    let generation = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((slot, generation))
}

/// 把监督者交出来的事件转给界面。
async fn forward_events(app: AppHandle, mut events: UnboundedReceiver<HostEvent>) {
    while let Some(host_event) = events.recv().await {
        let (name, params) = match host_event {
            HostEvent::Engine { method, params } => (method, params),
            HostEvent::Restarting => (event::ENGINE_RESTARTING.to_owned(), json!({})),
            HostEvent::Restored { restored } => {
                app.state::<Shell>().replace_docs(restored.clone());
                let payload = EngineRestored {
                    doc_ids: restored.iter().map(|doc| doc.doc_id).collect(),
                    docs: restored,
                };
                (event::ENGINE_RESTORED.to_owned(), json!(payload))
            }
            HostEvent::Failed { message } => {
                tracing::error!("引擎重启失败：{message}");
                (
                    event::ENGINE_FAILED.to_owned(),
                    json!(EngineFailed { message }),
                )
            }
        };
        if let Err(error) = app.emit(ENGINE_EVENT, json!({ "method": name, "params": params })) {
            tracing::warn!("事件没有发到界面：{error}");
        }
    }
}

/// 数据目录。默认是 `%LOCALAPPDATA%\MagPDF`（恢复文件、缓存、日志；方案 9.3、13.4）和
/// `%APPDATA%\MagPDF`（模板、组件、设置；方案 10）。设了 `MAGPDF_DATA_DIR` 就都放到它下面，测试用。
struct DataDirs {
    local: PathBuf,
    roaming: PathBuf,
}

impl DataDirs {
    fn locate() -> Self {
        if let Some(root) = std::env::var_os("MAGPDF_DATA_DIR") {
            let root = PathBuf::from(root);
            return Self {
                local: root.join("local"),
                roaming: root.join("roaming"),
            };
        }
        let known = |variable: &str| {
            std::env::var_os(variable)
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("MagPDF")
        };
        Self {
            local: known("LOCALAPPDATA"),
            roaming: known("APPDATA"),
        }
    }
}

/// 引擎的可执行文件：默认和本程序在同一个目录；`MAGPDF_ENGINE_BIN` 可以指到别处（开发时用）。
fn engine_bin() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("MAGPDF_ENGINE_BIN") {
        return Ok(PathBuf::from(path));
    }
    let exe = std::env::current_exe().context("取不到本程序的路径")?;
    let dir = exe.parent().context("本程序的路径没有上级目录")?;
    Ok(dir.join("magpdf-engine.exe"))
}

/// 允许读瓦片的页面来源：发布版是 Tauri 内置的 `tauri.localhost`，开发版另加开发服务器。
fn page_origins(context: &tauri::Context) -> Vec<String> {
    let mut origins = vec![
        "http://tauri.localhost".to_owned(),
        "https://tauri.localhost".to_owned(),
    ];
    if tauri::is_dev()
        && let Some(url) = &context.config().build.dev_url
    {
        origins.push(url.origin().ascii_serialization());
    }
    origins
}

/// wry 默认传给 WebView2 的参数：关掉 Edge 自带的界面和 SmartScreen（它会把访问的地址发给微软）。
/// 自己给参数的时候 wry 就不加默认的了，得一起带上。
const WEBVIEW_DEFAULT_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

/// 界面端到端测试要经 CDP 连进 WebView2（方案 13.2 第 8 项）。设了 `MAGPDF_WEBVIEW_DEBUG_PORT`
/// 才打开调试端口，窗口启动时也不抢焦点；平时不开，开着的话本机任何程序都能操纵界面。
fn webview_debug_port() -> Option<u16> {
    std::env::var("MAGPDF_WEBVIEW_DEBUG_PORT")
        .ok()?
        .parse()
        .ok()
}

/// 按 `tauri.conf.json` 里的窗口配置开主窗口。窗口在这里建而不是交给 Tauri 自动建，
/// 是因为调试端口要按环境变量决定加不加。
fn open_main_window(app: &AppHandle) -> tauri::Result<()> {
    let Some(config) = app.config().app.windows.first() else {
        return Err(tauri::Error::WindowNotFound);
    };
    let mut builder = tauri::WebviewWindowBuilder::from_config(app, config)?;
    if let Some(port) = webview_debug_port() {
        tracing::warn!("WebView2 的远程调试端口开在 {port}，只应在测试时使用");
        builder = builder
            .additional_browser_args(&format!(
                "{WEBVIEW_DEFAULT_ARGS} --remote-debugging-port={port}"
            ))
            // 测试时窗口不抢焦点：在开发机上跑测试，不能把正在用电脑的人的键盘输入截过来。
            .focused(false);
    }
    builder.build()?;
    Ok(())
}

/// 引擎没能启动时给用户看的话。
fn startup_failure(error: &HostError) -> String {
    match error {
        // 《接口协议》第 4 节：协议版本不一致，说明主程序和引擎不是同一次安装的。
        HostError::Protocol { .. } => "安装文件不完整，请重新安装。".to_owned(),
        other => format!("引擎没有启动，MagPDF 无法运行。\n\n{other}"),
    }
}

fn show_fatal(message: &str) {
    tracing::error!("{message}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("MagPDF")
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// 启动引擎、打开命令行给的文档，然后进窗口的消息循环；窗口关掉之后才返回。
pub fn run(open_paths: Vec<PathBuf>) -> Result<()> {
    let context = tauri::generate_context!();
    let dirs = DataDirs::locate();
    let recovery_dir = dirs.local.join("recovery");
    std::fs::create_dir_all(&recovery_dir).context("建不了恢复文件的目录")?;
    std::fs::create_dir_all(&dirs.roaming).context("建不了用户数据目录")?;
    // 恢复文件按主进程的 PID 分开，同时开两个 MagPDF 互不干扰；PID 被系统复用时先清掉上一个进程留下的。
    let recovery = recovery_dir.join(format!("session-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&recovery);

    let mut launch = EngineLaunch::new(engine_bin()?, recovery.clone(), env!("CARGO_PKG_VERSION"));
    launch.locale = Some("zh-CN".to_owned());
    launch.user_data_dir = dirs.roaming.to_string_lossy().into_owned();
    launch.cache_dir = dirs.local.to_string_lossy().into_owned();

    // 监督者的后台任务跑在 Tauri 的异步运行时上，和窗口同生共死。
    let (supervisor, events, _) = match tauri::async_runtime::block_on(Supervisor::start(launch)) {
        Ok(started) => started,
        Err(error) => {
            show_fatal(&startup_failure(&error));
            return Err(error).context("引擎没有启动");
        }
    };
    let shell = Shell {
        supervisor,
        docs: Mutex::new(Vec::new()),
        origins: page_origins(&context),
        recovery,
    };
    for path in &open_paths {
        // 日志里不记文件名和路径（《工程规范》）。
        if let Err(error) = tauri::async_runtime::block_on(shell.open(path)) {
            tracing::warn!("命令行给的文档没能打开：{error}");
        }
    }

    let app = tauri::Builder::default()
        .manage(shell)
        .register_asynchronous_uri_scheme_protocol(TILE_SCHEME, tile_protocol)
        .invoke_handler(tauri::generate_handler![
            engine_request,
            open_document_dialog,
            open_documents
        ])
        .setup(move |app| {
            open_main_window(app.handle())?;
            tauri::async_runtime::spawn(forward_events(app.handle().clone(), events));
            Ok(())
        })
        .build(context)
        .context("窗口没有建起来")?;

    app.run(|app, run_event| {
        if let RunEvent::Exit = run_event {
            let shell = app.state::<Shell>();
            tauri::async_runtime::block_on(shell.supervisor.shutdown());
            let _ = std::fs::remove_file(&shell.recovery);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_webview_may_only_forward_document_requests() {
        for allowed in [
            "doc.snapshot",
            "doc.close",
            "obj.list",
            "obj.hitTest",
            "edit.transform",
            "history.undo",
            "fidelity.check",
            "render.tiles",
            "render.cancel",
        ] {
            assert!(forwardable(allowed), "{allowed}");
        }
        for blocked in [
            "session.hello",
            "session.ping",
            "session.shutdown",
            "doc.open",
            "tile.fetch",
            "bench.echo",
            "bench.stall",
        ] {
            assert!(!forwardable(blocked), "{blocked}");
        }
    }

    #[test]
    fn tile_paths_carry_a_slot_and_a_generation() {
        assert_eq!(parse_tile_path("/tile/3/42"), Some((3, 42)));
        assert_eq!(parse_tile_path("/tile/0/0"), Some((0, 0)));
        for bad in [
            "/",
            "/tile",
            "/tile/3",
            "/tile/3/",
            "/tile/3/42/1",
            "/tile/-1/2",
            "/tile/a/b",
            "/tiles/3/42",
            "/tile/3/4294967296",
        ] {
            assert_eq!(parse_tile_path(bad), None, "{bad}");
        }
    }
}
