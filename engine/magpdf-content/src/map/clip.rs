//! 图形状态与剪切路径的跟踪（对应层只跟踪写回需要的那部分）。
//!
//! 与 PDFium 的 `CPDF_AllStates` 一致的部分：`q`/`Q` 栈、`cm`、颜色（填充/描边、是否有值）、
//! 文字状态（字体、大小、渲染模式、字距）、`W`/`W*` 的剪切路径。
//!
//! 与 PDFium 不完全一致的部分（写回层自己用，不影响对象对应）：
//! - 文字矩阵 `Tm` 只做标记（是否由 `Tm` 显式设置过），不累计位置；
//! - 剪切路径用"绘制后是否保留"来近似（填充/描边且 `W` 在其前面 → 保留；`W` 后 `n` → 新剪切；
//!   `Q` 恢复）。

/// 图形状态。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GState {
    /// 已设置的填充色操作符（`g`/`rg`/`k`/`cs sc`/`cs scn`），供删除/改色时还原。
    pub fill_color: Option<Vec<u8>>,
    pub stroke_color: Option<Vec<u8>>,
    pub fill_cs: Option<Vec<u8>>,
    pub stroke_cs: Option<Vec<u8>>,
    /// 被 `cs` 切换过的颜色是否"非纯 RGB/Gray"（Pattern、Separation、ICC 等），写回时不擅自改。
    pub fill_other_cs: bool,
    pub stroke_other_cs: bool,
    /// 当前字体与大小（`Tf`），仅用于诊断。
    pub font: Option<(Vec<u8>, f64)>,
    /// 文字渲染模式 `Tr` 的记号下标（只对 `BT … ET` 内的文字有效）。
    pub tr_token: Option<usize>,
    pub tr: u8,
    /// 字距 / 词距 / 水平缩放 / 上升，只记录记号下标与值。
    pub tc: f64,
    pub tw: f64,
    pub th: f64,
    pub ts: f64,
    pub tl: f64,
    /// 是否在 `BT … ET` 内。
    pub in_text: bool,
    /// 当前文字块是否由 `Tm` 显式定位过（否则第一条文字用 `Td`/`TD`/换行定位）。
    pub tm_set: bool,
    /// 剪切路径是否已经过多次裁剪（`W` 之后的 `n` 或绘制）。
    pub clip_depth: u32,
}

/// 剪切事件：给写回层判断"这一对象是否落在剪切里"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    /// `W`/`W*` 后面跟 `n`：剪切只改状态，不画。
    SetOnly,
    /// `W`/`W*` 后面跟绘制：既剪切又画（共用操作符）。
    SetAndPaint,
    /// 绘制时继承了之前留下的剪切。
    Inherited,
}

/// 一次剪切状态的转变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipEvent {
    pub token: usize,
    pub kind: ClipKind,
}

/// 剪切与文字块的状态机，写回层按事件重放。
#[derive(Debug, Clone, Default)]
pub struct Track {
    pub gs: GState,
    pub stack: Vec<GState>,
    pub events: Vec<ClipEvent>,
    /// `W`/`W*` 之后等待绘制操作的标志。
    pub pending_clip: bool,
}
