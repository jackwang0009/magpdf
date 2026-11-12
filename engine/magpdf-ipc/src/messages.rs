//! JSON-RPC 2.0 消息（《接口协议》3.1、第 4、7、8 节）。
//!
//! P0 只覆盖 V06 原型用到的消息：会话建立与心跳、渲染瓦片、瓦片取回、引擎重启与恢复、
//! 基准测量用的单程调用和批量往返。字段名用小驼峰。类型由 `ts-rs` 生成到 `packages/protocol`。

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 协议版本。不兼容修改时加 1（《接口协议》第 4 节）。
pub const PROTOCOL_VERSION: u32 = 1;

/// 引擎可执行文件的版本号（`session.hello` 返回）。
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 单帧上限 64 MB（《接口协议》3.2）。位图等大数据走共享内存。
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// 任意 JSON 值（`serde_json::Value`）。生成 TypeScript 时写成 `unknown`，参数的具体类型由调用方按方法收窄。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum JsonValue {
    Null,
    Bool(bool),
    /// Keep integers via serde_json::Number; f64 breaks HelloParams.protocol: u32.
    #[ts(as = "f64")]
    Number(serde_json::Number),
    String(String),
    Array(Vec<JsonValue>),
    Object(std::collections::BTreeMap<String, JsonValue>),
}

/// JSON-RPC 请求 ID：按《接口协议》3.1 用 JavaScript 安全整数。
pub type RequestId = u64;

/// JSON-RPC 请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Request {
    pub jsonrpc: JsonRpcTag,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<JsonValue>,
}

/// JSON-RPC 响应：`result` 与 `error` 恰有其一。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Response {
    pub jsonrpc: JsonRpcTag,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// JSON-RPC 通知（事件，没有 `id`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Notification {
    pub jsonrpc: JsonRpcTag,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<JsonValue>,
}

/// 协议中固定写 `"2.0"` 的字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum JsonRpcTag {
    #[default]
    #[serde(rename = "2.0")]
    V2,
}

/// 错误对象（《接口协议》第 8 节）：`message` 是可以直接显示给用户的中文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

impl RpcError {
    pub fn new(kind: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: kind.code(),
            message: message.into(),
            data: None,
        }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// 定义一组协议里的名字：每个名字一个常量，外加按声明顺序的 `ALL` 表（生成 TypeScript 常量时用）。
macro_rules! names {
    (
        $(#[$meta:meta])*
        pub mod $module:ident {
            $($(#[$item_meta:meta])* $name:ident = $value:literal;)*
        }
    ) => {
        $(#[$meta])*
        pub mod $module {
            $($(#[$item_meta])* pub const $name: &str = $value;)*

            /// 全部名字，按声明顺序：`(常量名, 协议里的名字)`。
            pub const ALL: &[(&str, &str)] = &[$((stringify!($name), $value)),*];
        }
    };
}

/// 定义错误码表：每一行是「变体 = 数值, 协议里的名称, 给用户看的中文说明」。
macro_rules! error_codes {
    ($( $(#[$meta:meta])* $variant:ident = $code:literal, $name:literal, $message:literal; )*) => {
        /// 《接口协议》第 8 节的错误码。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(i32)]
        pub enum ErrorCode {
            $( $(#[$meta])* $variant = $code, )*
        }

        impl ErrorCode {
            /// 全部错误码，按数值从小到大。
            pub const ALL: &[Self] = &[$(Self::$variant),*];

            /// 《接口协议》第 8 节里的名称，如 `E_PROTOCOL`。
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// 可以直接显示给用户的中文说明。
            pub const fn message(self) -> &'static str {
                match self {
                    $(Self::$variant => $message,)*
                }
            }
        }
    };
}

error_codes! {
    Protocol = 1001, "E_PROTOCOL", "协议版本不符或消息格式错误";
    EngineRestarted = 1002, "E_ENGINE_RESTARTED", "处理过程中引擎重启";
    Cancelled = 1003, "E_CANCELLED", "请求被取消";
    FileNotFound = 2001, "E_FILE_NOT_FOUND", "文件不存在";
    PasswordRequired = 2002, "E_PASSWORD_REQUIRED", "需要打开密码";
    PasswordWrong = 2003, "E_PASSWORD_WRONG", "密码错误";
    FileDamaged = 2004, "E_FILE_DAMAGED", "文件损坏，自动修复也失败";
    Unsupported = 2005, "E_UNSUPPORTED", "不支持的文件";
    VersionConflict = 3001, "E_VERSION_CONFLICT", "文档已经被别的操作修改";
    ObjectNotFound = 3002, "E_OBJECT_NOT_FOUND", "对象不存在";
    PageOverlayOnly = 3004, "E_PAGE_OVERLAY_ONLY", "页面保真校验未通过，只能叠加内容";
    InvalidArgument = 3005, "E_INVALID_ARGUMENT", "参数不合法";
    /// 瓦片的代数对不上：槽位已被复用（《接口协议》3.3，主进程转成 HTTP 410）。
    /// 《接口协议》第 8 节里还没有这个码，是 V06 原型加的。
    TileGenerationStale = 3006, "E_TILE_GENERATION_STALE", "瓦片已被新内容替换";
    Internal = 9001, "E_INTERNAL", "内部错误";
}

impl ErrorCode {
    pub const fn code(self) -> i32 {
        self as i32
    }
    pub fn error(self) -> RpcError {
        RpcError::new(self, self.message())
    }

    /// 带补充说明的错误（`data` 里放 `detail` 字段，界面的诊断日志用）。
    pub fn error_with(self, detail: impl Into<String>) -> RpcError {
        let mut error = RpcError::new(self, self.message());
        error.data = Some(JsonValue::Object(std::collections::BTreeMap::from([(
            "detail".to_owned(),
            JsonValue::String(detail.into()),
        )])));
        error
    }
}

names! {
    /// 方法名（《接口协议》第 4、6.3 节，加 V06 原型用的测量方法）。
    pub mod method {
        SESSION_HELLO = "session.hello";
        SESSION_PING = "session.ping";
        SESSION_SHUTDOWN = "session.shutdown";
        DOC_OPEN = "doc.open";
        DOC_CLOSE = "doc.close";
        DOC_SNAPSHOT = "doc.snapshot";
        OBJ_LIST = "obj.list";
        OBJ_HIT_TEST = "obj.hitTest";
        EDIT_TRANSFORM = "edit.transform";
        EDIT_SET_STYLE = "edit.setStyle";
        EDIT_DELETE = "edit.delete";
        HISTORY_UNDO = "history.undo";
        HISTORY_REDO = "history.redo";
        FIDELITY_CHECK = "fidelity.check";
        RENDER_TILES = "render.tiles";
        RENDER_CANCEL = "render.cancel";
        /// 不在《接口协议》里：主进程请引擎核对槽位的代数（V06 原型加的，见 V06 报告）。
        TILE_FETCH = "tile.fetch";
        BENCH_ECHO = "bench.echo";
        BENCH_ROUNDTRIP = "bench.roundtrip";
        BENCH_ENGINE = "bench.engine";
        /// 测试用：让引擎的通信循环卡住一段时间。只有带 `--test-hooks` 启动的引擎认它。
        BENCH_STALL = "bench.stall";
    }
}

names! {
    /// 事件名（《接口协议》第 7 节）。
    pub mod event {
        /// 修改命令、撤销或重做成功之后，文档变了。
        DOC_CHANGED = "doc.changed";
        /// 瓦片的像素已写入共享内存，可以取回。
        RENDER_TILE_READY = "render.tileReady";
        /// 一页的保真校验有了结论，参数见 [`FidelityResult`]。
        FIDELITY_RESULT = "fidelity.result";
        ENGINE_BUSY = "engine.busy";
        /// 主进程发：引擎没了，正在重启。在途的请求会以 `E_ENGINE_RESTARTED` 结束。
        ENGINE_RESTARTING = "engine.restarting";
        /// 主进程发：新引擎已经就绪，参数见 [`EngineRestored`]。
        ENGINE_RESTORED = "engine.restored";
        /// 主进程发：引擎连续几次都没能重新拉起来，参数见 [`EngineFailed`]。
        /// 《接口协议》里还没有这个事件。
        ENGINE_FAILED = "engine.failed";
    }
}

// ---- session ----

/// `session.hello` 的参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HelloParams {
    pub protocol: u32,
    pub app_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    pub user_data_dir: String,
    pub cache_dir: String,
    /// 主进程生成的会话 ID，共享内存名为 `Local\MagPDF-<会话 ID>-tiles`。
    pub session_id: String,
    /// 恢复文件路径；为空表示本次会话不做崩溃恢复。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_file: Option<String>,
}

/// `session.hello` 的返回值。`protocol` 是引擎认识的协议版本，主进程据此判断兼容性。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HelloResult {
    pub protocol: u32,
    pub engine_version: String,
    pub pdfium_revision: String,
    pub capabilities: Vec<String>,
    /// 本进程的 PID（恢复演示里用来结束进程）。
    pub pid: u32,
    /// 引擎启动时按恢复文件重新打开的文档。主进程据此向界面发 `engine.restored`（方案 9.3）；
    /// 文档的 `docId` 是新分配的，界面要丢掉旧快照重新拉取。
    #[serde(default)]
    pub restored: Vec<DocInfo>,
}

/// `session.ping` 的参数与返回值（同一结构）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PingParams {
    /// 主进程的单调时钟毫秒数，引擎原样回传（用来观察监控延迟）。
    pub t: u64,
}

/// `session.ping` 的返回值。
pub type PingResult = PingParams;

// ---- 文档 ----

/// `doc.open` 的参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DocOpenParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// 文档信息（《接口协议》5.3 的子集）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DocInfo {
    pub doc_id: u32,
    pub path: String,
    pub title: String,
    pub version: u32,
    pub page_count: u32,
    /// 第一页在缩放 1.0 下的尺寸（pt）。
    pub page_width: f32,
    pub page_height: f32,
}

/// `doc.open` 的返回值。
pub type DocOpenResult = DocInfo;

/// `doc.snapshot` 的参数（《接口协议》6.1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DocSnapshotParams {
    pub doc_id: u32,
    /// 只要这几页，按给出的顺序返回，并带上每页的 `rotate` 和 `cropBox`。
    /// 省略时返回全部页面，只有编号和显示尺寸：取旋转和可见区域要加载页面（解析内容流），
    /// 几百页的文档不能在打开时全做一遍。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_ids: Option<Vec<u32>>,
}

/// 页面的保真校验状态（《接口协议》5.3、方案 2.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum Fidelity {
    /// 还没有校验。页面显示之后，引擎在空闲时校验，结论经 `fidelity.result` 推送。
    Pending,
    /// 可以修改原有对象。
    Ok,
    /// 只能叠加内容（新增对象、在上面覆盖），不能修改原有对象。
    OverlayOnly,
}

/// 页面信息（《接口协议》5.3 `PageInfo` 的子集）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PageInfo {
    /// 页面在会话内的编号。原型还没有页面命令，它等于页序号。
    pub page_id: u32,
    pub index: u32,
    /// 显示宽度（pt），已经按页面旋转换好宽高。
    pub width: f32,
    /// 显示高度（pt）。
    pub height: f32,
    /// 现在知道的保真校验状态。查快照不会触发校验。
    pub fidelity: Fidelity,
    /// `fidelity` 为 `overlayOnly` 时的原因，给技术支持看的；给用户看的说法由界面统一写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fidelity_reason: Option<String>,
    /// 页面的 `/Rotate`：0、90、180、270。只有按 `pageIds` 请求时才有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<u16>,
    /// 页面的可见区域 `[left, bottom, right, top]`（PDF 用户空间，未经旋转）：CropBox 与 MediaBox 的交集，
    /// 角点已经按大小排好。界面用它和 `rotate` 把对象坐标换算成显示坐标（《接口协议》3.1）。
    /// 只有按 `pageIds` 请求时才有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop_box: Option<[f32; 4]>,
}

/// `doc.snapshot` 的返回值。原型只给页面列表；对象用 `obj.list` 按页取。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DocSnapshot {
    pub version: u32,
    pub pages: Vec<PageInfo>,
}

/// `doc.close` 的参数（《接口协议》6.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DocCloseParams {
    pub doc_id: u32,
    /// 放弃未保存的修改。原型还没有修改命令，这个字段暂时不起作用。
    #[serde(default)]
    pub discard: bool,
}

// ---- 对象查询（《接口协议》6.4）----

/// 页面对象的种类（《接口协议》5.3 `ObjectSummary.kind` 的子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ObjectKind {
    Text,
    Path,
    Image,
    Shading,
    Form,
    Unknown,
}

/// 对象摘要（《接口协议》5.3 的子集）。几何量用 PDF 用户空间，单位 pt。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ObjectSummary {
    /// 会话内稳定的对象编号：编辑后不变，删除后不复用。
    pub id: u64,
    pub kind: ObjectKind,
    /// 包围盒 `[left, bottom, right, top]`。PDFium 给不出来时省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f32; 4]>,
    /// 页内层级，0 在最下面。
    pub z: u32,
    /// 原型还没有锁定功能，恒为 false。
    pub locked: bool,
    /// 在画布上点选、框选时算不算数（方案 4.3）：不可见文字、完全透明的对象、铺满页面的白色底块为 false。
    /// 这些对象仍然列出来，可以在图层面板里选中。《接口协议》5.3 里还没有这个字段：界面在本地按包围盒
    /// 做命中测试（悬停时每帧都要查，不能每次都问引擎），所以要引擎把这条规则的结果一起给出来。
    pub pickable: bool,
}

/// `obj.list` 一次最多返回的条数（《接口协议》5.3）。
pub const OBJ_LIST_MAX: u32 = 2000;

/// `obj.list` 的参数。返回 `ObjectSummary[]`，按层级从下到上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ObjListParams {
    pub doc_id: u32,
    pub page_id: u32,
    #[serde(default)]
    pub offset: u32,
    /// 省略或超过 [`OBJ_LIST_MAX`] 时按 [`OBJ_LIST_MAX`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// 命中测试的模式（《接口协议》6.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum HitMode {
    /// 最上面一个看得见的对象。
    Top,
    /// 光标下的全部对象，自上而下。
    All,
}

/// `obj.hitTest` 的参数。返回 `ObjectSummary[]`。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ObjHitTestParams {
    pub doc_id: u32,
    pub page_id: u32,
    /// `[x, y]`，PDF 用户空间。
    pub point: [f32; 2],
    pub mode: HitMode,
}

// ---- 编辑与撤销（《接口协议》6.5、6.8）----

/// `edit.transform` 的参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditTransformParams {
    pub doc_id: u32,
    pub base_version: u32,
    pub ids: Vec<u64>,
    /// `[a, b, c, d, e, f]`，左乘到对象上。
    pub matrix: [f32; 6],
    /// 剪切路径是否一起变换。原型里还不起作用。
    #[serde(default)]
    pub clip_follows: bool,
}

/// 颜色所在的色彩空间。原型只支持 RGB。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ColorSpace {
    Rgb,
}

/// 带色彩空间的颜色。分量在 0–1 之间，RGB 是三个分量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Color {
    pub space: ColorSpace,
    pub components: Vec<f32>,
}

/// `edit.setStyle` 的参数。原型只支持 `fill`；带了别的样式字段会被当作参数不合法拒绝，而不是悄悄忽略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EditSetStyleParams {
    pub doc_id: u32,
    pub base_version: u32,
    pub ids: Vec<u64>,
    /// 填充色。只换颜色，对象的不透明度不动。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Color>,
}

/// `edit.delete` 的参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditDeleteParams {
    pub doc_id: u32,
    pub base_version: u32,
    pub ids: Vec<u64>,
}

/// `history.undo`、`history.redo` 的参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HistoryParams {
    pub doc_id: u32,
}

/// 修改命令、撤销、重做的返回值：新的文档版本号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditResult {
    pub version: u32,
}

/// `doc.changed` 里一页上的变化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PageChange {
    pub page_id: u32,
    pub added: Vec<u64>,
    pub removed: Vec<u64>,
    pub modified: Vec<u64>,
    /// 这一页画面可能变了的地方 `[left, bottom, right, top]`（PDF 用户空间）：动过的对象改之前和改之后
    /// 包围盒的并集。界面只重画和它相交的瓦片；没有这一项时整页重画。V05 加的，《接口协议》第 7 节还没写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<[f32; 4]>,
}

/// `doc.changed` 事件的参数（《接口协议》第 7 节）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DocChanged {
    pub doc_id: u32,
    pub version: u32,
    pub changes: Vec<PageChange>,
    /// 页面的增删或顺序有没有变。原型还没有页面命令，恒为 false。
    pub pages_changed: bool,
}

// ---- 保真校验（《接口协议》6.9）----

/// `fidelity.check` 的参数：马上校验这几页，不等引擎空闲。应答是空的；每一页的结论各发一条
/// `fidelity.result`，已经校验过的页也发。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FidelityCheckParams {
    pub doc_id: u32,
    pub page_ids: Vec<u32>,
}

/// `fidelity.result` 事件的参数（《接口协议》第 7 节）：一页的保真校验有了结论。
/// 三种情况下会发：页面显示之后引擎空闲时校验的；修改命令顺带校验的；`fidelity.check` 要的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FidelityResult {
    pub doc_id: u32,
    pub page_id: u32,
    /// `ok` 或 `overlayOnly`，不会是 `pending`。
    pub status: Fidelity,
    /// `overlayOnly` 时的原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---- 瓦片 ----

/// `render.tiles` 里的瓦片位置：整页像素网格上瓦片左上角。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TileCoord {
    pub x: u32,
    pub y: u32,
}

impl fmt::Display for TileCoord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

/// 分层渲染里的一层留哪些对象（拖动预览，方案 3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum LayerMode {
    /// 底图：去掉指定的对象，其余照常画在白纸上。
    Without,
    /// 对象层：只画指定的对象，别处全透明。像素是**没有预乘**的 BGRA，界面合成时自己预乘。
    Only,
}

/// `render.tiles` 的 `layer`：这一批瓦片只画页面的一层。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TileLayer {
    pub mode: LayerMode,
    /// 对象在会话内的编号，都要在这一页上；有对不上的，这一批瓦片渲染不出来。
    pub object_ids: Vec<u64>,
}

/// `render.tiles` 的参数（《接口协议》6.3）。V06 原型固定 512×512 瓦片。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderTilesParams {
    pub doc_id: u32,
    pub page_id: u32,
    pub scale: f32,
    pub tiles: Vec<TileCoord>,
    pub priority: RenderPriority,
    /// 分层渲染（拖动预览，方案 3.2）：只画页面的一层。省略时画整页。
    ///
    /// 《接口协议》6.3 写的是单独的 `render.layered`，一次返回两张视口大小的位图。原型里位图只有瓦片
    /// 这一条路（共享内存的槽位是 512×512），所以做成 `render.tiles` 的一个参数：两层各发一批瓦片。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<TileLayer>,
}

/// 渲染优先级（《接口协议》6.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum RenderPriority {
    Visible,
    Prefetch,
    Thumb,
}

/// `render.tiles` 的立即应答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderTilesResult {
    pub request_ids: Vec<u32>,
}

/// `render.tileReady` 事件的参数（《接口协议》第 7 节）：瓦片已写入共享内存。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TileReady {
    pub request_id: u32,
    pub doc_id: u32,
    pub page_id: u32,
    pub scale: f32,
    pub x: u32,
    pub y: u32,
    /// 渲染时的文档版本号。界面据此判断这块瓦片是不是已经过时。
    pub version: u32,
    pub slot: u32,
    pub generation: u32,
}

/// `tile.fetch` 的参数：主进程请引擎确认槽位的代数后从共享内存取像素。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TileFetchParams {
    pub slot: u32,
    pub generation: u32,
}

/// `tile.fetch` 的返回值。代数对不上时返回错误码 3006（主进程转成 HTTP 410）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TileFetchResult {
    pub slot: u32,
    pub generation: u32,
    pub width: u32,
    pub height: u32,
}

// ---- 主进程发给界面的事件（《接口协议》第 7 节）----

/// `engine.restored` 事件的参数：新引擎已经就绪，这些文档按恢复文件重新打开了。
///
/// 《接口协议》只写了 `docIds`。引擎重启后 `docId` 是重新分配的，界面要靠 `docs` 里的路径才能把
/// 手里的文档和新的 `docId` 对上；对不上的（没能恢复）就当作已经关掉。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EngineRestored {
    pub doc_ids: Vec<u32>,
    pub docs: Vec<DocInfo>,
}

/// `engine.failed` 事件的参数：引擎连续几次都没能重新拉起来，主进程不再重试。
/// 《接口协议》里还没有这个事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EngineFailed {
    pub message: String,
}

// ---- 基准 ----

/// `bench.echo` 的参数：原样返回，不碰 PDFium。
pub type EchoParams = PingParams;
/// `bench.echo` 的返回值。
pub type EchoResult = PingParams;

/// `bench.stall` 的参数：通信循环卡住多少毫秒。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StallParams {
    pub ms: u32,
}

/// `bench.roundtrip` 的参数：主进程连续发 `count` 次 `bench.echo`，测量主进程到引擎的往返。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RoundtripParams {
    pub count: u32,
}

/// `bench.roundtrip` 的返回值，耗时单位为微秒。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RoundtripResult {
    pub durations_us: Vec<f64>,
}

/// `bench.engine` 的参数：让引擎在内部做同口径测量（共享内存读写、JSON 编解码占比、I2 槽位数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BenchEngineParams {
    /// 每次运行重复的次数。
    pub runs: u32,
    /// 每组测量的迭代次数。
    pub iterations: u32,
}

/// 一项耗时的统计：样本单位为微秒。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Stats {
    pub samples: u32,
    pub median_us: f64,
    pub p95_us: f64,
    pub min_us: f64,
    pub max_us: f64,
}

impl Stats {
    /// 根据一组微秒样本算统计量；空样本返回全 0。P95 用最近秩法（落在 95% 处的样本）。
    pub fn from_samples(mut samples: Vec<f64>) -> Self {
        if samples.is_empty() {
            return Self {
                samples: 0,
                median_us: 0.0,
                p95_us: 0.0,
                min_us: 0.0,
                max_us: 0.0,
            };
        }
        samples.sort_by(|a, b| a.total_cmp(b));
        let n = samples.len();
        let at = |q: f64| -> f64 {
            let idx = ((q * n as f64).ceil() as usize).clamp(1, n) - 1;
            samples.get(idx).copied().unwrap_or(0.0)
        };
        Self {
            samples: u32::try_from(n).unwrap_or(u32::MAX),
            median_us: at(0.5),
            p95_us: at(0.95),
            min_us: samples.first().copied().unwrap_or(0.0),
            max_us: samples.last().copied().unwrap_or(0.0),
        }
    }
}

/// `bench.engine` 的一轮结果。每一项都是"每轮 `iterations` 次测量的统计"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BenchEngineResult {
    /// 引擎处理一条小消息的编解码合计：请求的 JSON 编码与解码、响应的编码与解码，不含管道读写。
    pub engine_roundtrip: Stats,
    /// 从共享内存读出 512×512 瓦片（`copy_from_slice`）。
    pub shm_read: Stats,
    /// 把一块瓦片写进共享内存槽位（含代数递增）。
    pub shm_write: Stats,
    /// 一块 512×512 瓦片的渲染（PDFium，不写入共享内存）。
    pub render_tile: Stats,
    /// 渲染 + 写入共享内存的合计。
    pub render_and_write: Stats,
    /// 一侧的 JSON 编码耗时。
    pub json_encode: Stats,
    /// 一侧的 JSON 解码耗时。
    pub json_decode: Stats,
    /// 一侧的 MessagePack 编码耗时（只作对照）。
    pub msgpack_encode: Stats,
    /// 一侧的 MessagePack 解码耗时（只作对照）。
    pub msgpack_decode: Stats,
    /// 请求与响应两种消息的 JSON 字节数与 MessagePack 字节数。
    pub json_bytes: u32,
    pub msgpack_bytes: u32,
    /// 引擎能拿到的物理内存（MB），用于 I2 建议。
    pub total_memory_mb: u64,
    /// 系统 CPU 占用（%，采样期间的平均值），说明测量是否受其他任务干扰。
    pub cpu_percent: f64,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Write as _;
    use std::path::PathBuf;

    use super::*;

    /// ts-rs 导出类型的目录：`TS_RS_EXPORT_DIR`（仓库里由 `.cargo/config.toml` 指到
    /// `packages/protocol/src/generated`），没设时是 crate 下的 `bindings/`。
    fn export_dir() -> PathBuf {
        std::env::var_os("TS_RS_EXPORT_DIR")
            .map_or_else(|| PathBuf::from("bindings"), PathBuf::from)
    }

    /// `constants.ts` 的内容：协议版本、方法名、事件名、错误码，以及界面要用到的几个数值上限。
    fn constants_ts() -> String {
        let mut out = String::from(
            "// 本文件由 magpdf-ipc 的测试 export_protocol_constants 生成（cargo test -p magpdf-ipc），不要手工修改。\n\n",
        );
        out.push_str("/** 协议版本：不兼容修改时加 1（《接口协议》第 4 节）。 */\n");
        writeln!(
            out,
            "export const PROTOCOL_VERSION = {PROTOCOL_VERSION} as const;\n"
        )
        .unwrap();
        out.push_str("/** 方法名（《接口协议》第 4、6 节，以及 V06 原型的测量方法）。 */\n");
        out.push_str("export const METHOD = {\n");
        for (name, value) in method::ALL {
            writeln!(out, "  {name}: '{value}',").unwrap();
        }
        out.push_str("} as const;\n\n");
        out.push_str("/** 事件名（《接口协议》第 7 节）。 */\n");
        out.push_str("export const EVENT = {\n");
        for (name, value) in event::ALL {
            writeln!(out, "  {name}: '{value}',").unwrap();
        }
        out.push_str("} as const;\n\n");
        out.push_str("/** 错误码（《接口协议》第 8 节）。 */\n");
        out.push_str("export const ERROR_CODE = {\n");
        for code in ErrorCode::ALL {
            writeln!(out, "  {}: {},", code.name(), code.code()).unwrap();
        }
        out.push_str("} as const;\n\n");
        out.push_str(
            "/** `obj.list` 一次最多返回的条数；对象更多的页面用 `offset` 分几次取（《接口协议》5.3）。 */\n",
        );
        writeln!(out, "export const OBJ_LIST_MAX = {OBJ_LIST_MAX} as const;").unwrap();
        out
    }

    #[test]
    fn names_and_error_codes_are_unique() {
        for table in [method::ALL, event::ALL] {
            let values: BTreeSet<_> = table.iter().map(|(_, value)| *value).collect();
            assert_eq!(values.len(), table.len(), "协议名字不能重复：{table:?}");
        }
        let codes: Vec<i32> = ErrorCode::ALL.iter().map(|c| c.code()).collect();
        assert!(codes.is_sorted(), "错误码按数值从小到大列出：{codes:?}");
        let names: BTreeSet<_> = ErrorCode::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), ErrorCode::ALL.len());
        assert!(names.iter().all(|n| n.starts_with("E_")));
    }

    /// 和 ts-rs 的导出测试做法一样：`cargo test` 时把协议常量写进导出目录，与生成的类型放在一起。
    #[test]
    fn export_protocol_constants() {
        let dir = export_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("constants.ts"), constants_ts()).unwrap();
    }

    /// `packages/protocol/src/messages.ts` 手工列出要再导出的类型；生成目录里多了类型而它没跟上时在这里报错。
    #[test]
    fn protocol_package_reexports_every_generated_type() {
        let dir = export_dir();
        let Ok(barrel) = std::fs::read_to_string(dir.join("../messages.ts")) else {
            // 导出目录不在协议包里（没有设 TS_RS_EXPORT_DIR）时没有可核对的文件。
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if path.extension().is_none_or(|ext| ext != "ts") || stem == "constants" {
                continue;
            }
            assert!(
                barrel.contains(&format!("'./generated/{stem}'")),
                "packages/protocol/src/messages.ts 没有再导出 {stem}"
            );
        }
    }
}
