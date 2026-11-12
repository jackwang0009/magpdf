//! V17 实验的数据模型：参数、每页与每个文件的记录。
//!
//! 结果以页为单位记录（[`PageRecord`]），汇总（`report.rs`）全部从页记录算出来，
//! 这样 JSON 里的每个合计数都能逐页核对。

use magpdf_engine::compare::Thresholds;
use magpdf_engine::roundtrip::{DiffImages, DiffMetrics};
use serde::{Deserialize, Serialize};

/// 随机修改里“移动”的位移（点，页面坐标）。
pub const MOVE_DX: f32 = 5.0;
pub const MOVE_DY: f32 = 5.0;
/// 随机修改里“改色”换成的填充色（0–255）。不透明度沿用对象原来的。
pub const RECOLOR_RGB: [u8; 3] = [230, 26, 26];

/// 预期画面怎么得到。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedMode {
    /// 在 PDFium 内存里改完，页面不关，直接渲染。这是界面上改完看到的画面。
    Direct,
    /// 首轮的做法：改完之后把整页标记为已修改、让 PDFium 的生成器重写内容流，再重新解析渲染。
    /// 预期画面因此带上了生成器自己的损失，只在复查首轮数字时用。
    Regenerated,
}

/// 每个被抽中的对象做哪一种修改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum EditMix {
    /// 首轮的做法：按抽中的先后轮流取移动、改色、删除。对象少于 30 个的页只有一处修改，总是移动。
    Cycle,
    /// 每个对象随机取一种。
    Random,
}

/// 一份文件的实验参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// 随机修改的比例（0 = 不改动）。
    pub modify_ratio: f64,
    /// 随机种子。
    pub seed: u64,
    /// 渲染倍率。
    pub scale: f32,
    /// 是否跑对照组（整页都交给 PDFium 的生成器）。
    pub control: bool,
    pub expected: ExpectedMode,
    pub edit_mix: EditMix,
    /// 不改动时也把对得上的页经改写层写一遍，再从保存结果读回来核对。
    pub rewrite_all: bool,
    /// 渲染像素上限（超过的页不比对，记为页错误）。
    pub max_pixels: usize,
    /// 像素比对的判定规则（默认即 V02 的规则）。
    pub thresholds: Thresholds,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            modify_ratio: 0.0,
            seed: 20261109,
            scale: 2.0,
            control: false,
            expected: ExpectedMode::Direct,
            edit_mix: EditMix::Cycle,
            rewrite_all: false,
            max_pixels: 40_000_000,
            thresholds: Thresholds::default(),
        }
    }
}

/// 修改的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    Move,
    Recolor,
    Delete,
}

impl EditKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Move => "移动",
            Self::Recolor => "改色",
            Self::Delete => "删除",
        }
    }
}

/// 一处修改：改的是第几个对象（PDFium 的顶层对象序号）、对象类别、做了什么。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditRecord {
    pub object: usize,
    pub object_kind: String,
    pub kind: EditKind,
}

/// 这一页的内容怎么写进保存结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// 操作符和对象对上了：由自研改写层写，没改的字节原样保留。
    Matched,
    /// 对不上，或者改写层表达不了这次修改：退回 PDFium 的生成器整条重写。
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// 无差异（V02 的判定规则）。
    Pass,
    /// 有差异，或者保存结果里这一页打不开、渲染失败。
    Diff,
    /// 预期画面就渲染不出来（原件这一页打不开、页面过大），不计入比例。
    Error,
}

/// 一次像素比对的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 差异图（预期、保存结果、热图），只在给了输出目录而且有差异时写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<DiffImages>,
    /// 两张位图各自的指纹（FNV-1a 64 位，十六进制）。换一份 PDFium 重跑时用它分辨
    /// 变的是预期画面还是保存结果的画面。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_hash: Option<String>,
}

impl Check {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Error,
            diff: None,
            error: Some(message.into()),
            images: None,
            expected_hash: None,
            actual_hash: None,
        }
    }

    pub fn diff(message: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Diff,
            ..Self::error(message)
        }
    }
}

/// 一页的完整记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageRecord {
    /// 页序号，从 0 开始。
    pub index: usize,
    pub route: Route,
    /// 退回生成器的原因（只写类别，汇总按它计数）；对上的页为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    /// 细节：对象数各是多少、改写层报的错、回退时哪一步没做成。不出现文档内容。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// PDFium 这一页打不开时的错误。这样的页不参加任何统计。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// PDFium 的顶层对象数。
    pub objects: usize,
    /// 这一页的内容流条数。
    pub streams: usize,
    /// 不改任何对象、经改写层拆开再拼回去之后逐字节相同的条数。
    pub streams_identical: usize,
    /// 没能做这项核对的条数（内容流读不出来，或者改写层不接受这一页）。
    pub streams_unchecked: usize,
    /// 内容流里的操作符个数。
    pub operators: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<EditRecord>,
    /// 保存结果与预期画面的像素比对。回退的页比的是生成器重写之后的画面。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<Check>,
    /// 对照组：整页都交给 PDFium 的生成器，同一组修改。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Check>,
}

#[cfg(test)]
impl PageRecord {
    /// 一条空记录（测试里在它上面改字段）。
    pub fn new(index: usize) -> Self {
        Self {
            index,
            route: Route::Fallback,
            reasons: Vec::new(),
            notes: Vec::new(),
            error: None,
            objects: 0,
            streams: 0,
            streams_identical: 0,
            streams_unchecked: 0,
            operators: 0,
            edits: Vec::new(),
            verify: None,
            control: None,
        }
    }
}

/// 各阶段耗时（微秒，避免小文件全为 0）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timings {
    /// 读文件、自研对象层打开。
    pub read_us: u64,
    /// 取内容流、分词、对应。
    pub map_us: u64,
    /// 改写与写增量更新（只算自研的部分）。
    pub save_us: u64,
    /// 回退的页交给 PDFium 生成器重写并保存。
    #[serde(default)]
    pub fallback_us: u64,
    /// 从保存结果读回内容流核对。
    pub verify_us: u64,
    /// 渲染与像素比对。
    pub render_us: u64,
    /// 对照组。
    pub control_us: u64,
}

impl Timings {
    pub fn add(&mut self, other: &Self) {
        self.read_us += other.read_us;
        self.map_us += other.map_us;
        self.save_us += other.save_us;
        self.fallback_us += other.fallback_us;
        self.verify_us += other.verify_us;
        self.render_us += other.render_us;
        self.control_us += other.control_us;
    }
}

/// 文件级状态。崩溃与超时只在子进程隔离模式下由父进程填写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Ok,
    /// 读不了文件，或者 PDFium 打不开。
    LoadFailed,
    Crashed,
    TimedOut,
}

/// 保存结果的统计。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputStats {
    /// 保存结果的总字节数。
    pub saved_bytes: u64,
    /// 自研改写层追加的字节数。
    pub incremental_bytes: u64,
    /// 回退的页由 PDFium 增量保存追加的字节数。
    pub fallback_bytes: u64,
    /// 作为对比：把整个文件完整重写一遍的字节数（自研对象层打不开时为 0）。
    pub full_rewrite_bytes: u64,
    /// 修改后文件与原文件的字节差。
    pub bytes_delta: i64,
    /// 多页共用、因此复制了一份再改的内容流条数。
    pub deshared_streams: usize,
    /// 页对象不归这一页独有（同一个页对象占了几个页号，或者页字典直接写在 /Kids 里）、
    /// 因此另起了页对象的页数。
    #[serde(default)]
    pub pages_split: usize,
    /// 为了把另起的页对象挂回去而复制的页树节点数。
    #[serde(default)]
    pub tree_nodes_split: usize,
    /// 保存后核对不过的页，每页一条，写明原因。
    pub verify_failures: Vec<String>,
}

/// 一个文件的结果。`status` 不是 `Ok` 时该文件不计入任何比例（单列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileResult {
    pub path: String,
    pub source: String,
    pub bytes: u64,
    pub pages: usize,
    pub status: FileStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 自研对象层打不开这份文件的原因。PDFium 能打开时，全部页面退回生成器。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_layer_error: Option<String>,
    #[serde(default)]
    pub timings: Timings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputStats>,
    #[serde(default)]
    pub page_records: Vec<PageRecord>,
}

impl FileResult {
    /// 没有处理任何页面的失败结果。
    pub fn failed(
        path: impl Into<String>,
        source: impl Into<String>,
        status: FileStatus,
        error: impl std::fmt::Display,
    ) -> Self {
        Self {
            path: path.into(),
            source: source.into(),
            bytes: 0,
            pages: 0,
            status,
            error: Some(error.to_string()),
            object_layer_error: None,
            timings: Timings::default(),
            output: None,
            page_records: Vec::new(),
        }
    }
}
