//! 往返测试（方案 13.2 第 1 项，P0 计划 V02）。
//!
//! 对一个文档的每一页：把全部顶层对象标记为已修改，让 PDFium 重新生成内容流，完整保存，
//! 再把原件和保存结果分别渲染后按 [`crate::compare`] 的规则比对。结果按来源分类汇总
//! （方案 13.1：所有统计按来源分别报告）。
//!
//! 这里只做单个文档的处理与多文档结果的汇总，不涉及文件系统遍历、子进程隔离和图片输出，
//! 那些在 `magpdf-cli` 里；因此本模块可以用手工拼出的小 PDF 做单元测试。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Instant;

use pdfium_sys::{Bitmap, Document, Library, ObjectKind, Page, RenderOptions, SaveFlags};
use serde::{Deserialize, Serialize};

use crate::compare::{self, Comparison, Thresholds};

/// 单个文档往返测试的参数。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Options {
    /// 渲染倍率：2.0 即 144 dpi（方案 13.2"2 倍分辨率"）。
    pub scale: f32,
    pub thresholds: Thresholds,
    /// 校准模式（P0 计划 V02 的 W1 校准）：不标记、不重新生成内容流，只保存再重开渲染，
    /// 用来测量比对与渲染本身的噪声底线，应当全部无差异。
    pub calibrate: bool,
    /// 单页渲染像素数上限，超过的页记为错误而不是缩小比对分辨率，避免巨幅页面耗尽内存。
    pub max_pixels: usize,
    /// 把保存结果的字节留在 [`FileResult::saved`] 里，供调用方写出（第二渲染器比对、人工检查内容流）。
    #[serde(default)]
    pub keep_saved: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scale: 2.0,
            thresholds: Thresholds::default(),
            calibrate: false,
            max_pixels: 40_000_000,
            keep_saved: false,
        }
    }
}

/// 文件在测试文件库里的身份：相对路径与来源类别（目录名，见 `tests/corpus/README.md`）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileId {
    pub path: String,
    pub source: String,
}

/// 一页的对象统计，用于报告分类和差异归因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObjectStats {
    /// 顶层对象总数。
    pub objects: usize,
    pub text: usize,
    pub path: usize,
    pub image: usize,
    pub shading: usize,
    pub form: usize,
    /// Form XObject 内部（只数一层）的对象数。
    pub form_inner: usize,
    /// 带透明（混合模式、透明度、软蒙版）的顶层对象数。
    pub transparent: usize,
    /// 使用 Type3 字体的文字对象数（方案 2.1：原版生成器遇到 Type3 会损坏整条流）。
    pub type3_text: usize,
    /// 渲染模式 4–7（字形作为裁剪）的文字对象数（方案 2.3 第 3 条）。
    pub clip_text: usize,
    /// 页面是否声明了透明组。
    pub page_transparency: bool,
    /// 文字页的字符数（不含 PDFium 生成的排版字符）。
    pub chars: usize,
}

/// 一页的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    /// 无差异。
    Pass,
    /// 有差异，或保存结果里这一页打不开、渲染失败。
    Diff,
    /// 原件这一页就打不开或渲染不了，不计入无差异比例。
    Error,
}

/// 像素比对的数值结果。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DiffMetrics {
    pub diff_pixels: usize,
    pub total_pixels: usize,
    pub diff_ratio: f64,
    pub largest_region: usize,
    /// 差异包围盒 `[x0, y0, x1, y1]`（像素，右下开区间）。
    pub bbox: Option<[u32; 4]>,
}

impl From<&Comparison> for DiffMetrics {
    fn from(c: &Comparison) -> Self {
        Self {
            diff_pixels: c.diff_pixels,
            total_pixels: c.total_pixels,
            diff_ratio: c.diff_ratio(),
            largest_region: c.largest_region,
            bbox: c.diff_bbox.map(|(x0, y0, x1, y1)| [x0, y0, x1, y1]),
        }
    }
}

/// 往返前后文字位置的对比：字符数是否一致、有多少字符挪了位置。字距（`TJ`、`Tc`、`Tw`）
/// 丢失时字符会整体移位，这是方案 2.1 里最关心的一类缺陷。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextShift {
    pub chars_before: usize,
    pub chars_after: usize,
    /// 位置相差超过 [`TEXT_SHIFT_TOLERANCE_PT`] 的字符数（只在字符数一致时按序号对应）。
    pub moved: usize,
    pub max_shift_pt: f32,
}

/// 字符起点允许的误差（点）。生成器写浮点数只保留有限位小数，这点误差在 144 dpi 下不到 1/4 像素。
pub const TEXT_SHIFT_TOLERANCE_PT: f32 = 0.1;

/// 差异页的初步归因。只是从对象统计和前后对比推断出的线索，不是确诊；
/// 推断不出来的标为 [`Hint::Unclassified`]，留待人工归类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hint {
    /// 重开后渐变对象变少：生成器跳过了 `sh`。
    ShadingLost,
    /// 重开后图像对象变少：内联图像被生成器丢弃。
    ImageLost,
    /// 页面含 Type3 文字：生成器留下未闭合的 `q`、`BT`。
    Type3,
    /// 对象数变化且不能用上面几条解释：内容流结构被改坏。
    ObjectsChanged,
    /// 字符数变化。
    TextChanged,
    /// 字符数一致但位置移动：字距、`Tc`、`Tw` 等丢失。
    TextMoved,
    /// 对象的填充/描边颜色变化：非 RGB 色彩空间未写出。
    ColorChanged,
    /// 页面有透明对象：扩展图形状态（软蒙版、叠印等）可能丢失。
    Transparency,
    /// 页面有裁剪模式的文字。
    TextClip,
    /// 以上都不是。
    Unclassified,
}

impl Hint {
    /// 报告里用的中文标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::ShadingLost => "渐变丢失",
            Self::ImageLost => "图像丢失",
            Self::Type3 => "Type3",
            Self::ObjectsChanged => "对象数变化",
            Self::TextChanged => "文字变化",
            Self::TextMoved => "字距/文字位置",
            Self::ColorChanged => "颜色",
            Self::Transparency => "透明/ExtGState",
            Self::TextClip => "文字裁剪",
            Self::Unclassified => "待人工归类",
        }
    }
}

/// 各步耗时（毫秒）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageTimings {
    /// 对象统计、标记与重新生成内容流。
    pub prepare_ms: u64,
    pub render_a_ms: u64,
    pub render_b_ms: u64,
    pub compare_ms: u64,
    /// 差异页的前后对比分析。
    pub analyze_ms: u64,
}

/// 差异图路径，由调用方（CLI）写出后填入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffImages {
    pub a: String,
    pub b: String,
    pub heat: String,
}

/// 一页的完整记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageRecord {
    /// 页序号，从 0 开始。
    pub index: usize,
    pub width: f32,
    pub height: f32,
    pub status: PageStatus,
    pub error: Option<String>,
    pub diff: Option<DiffMetrics>,
    /// 原件的对象统计。
    pub stats: ObjectStats,
    /// 保存结果的对象统计（只对差异页收集）。
    pub stats_after: Option<ObjectStats>,
    pub text_shift: Option<TextShift>,
    /// 颜色变化的对象数（对象数一致时按序号对应；只对差异页收集）。
    pub color_changed: Option<usize>,
    /// 按优先级排列的归因线索，第一个视为主要归因。
    pub hints: Vec<Hint>,
    pub timings: PageTimings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_images: Option<DiffImages>,
}

impl PageRecord {
    fn new(index: usize) -> Self {
        Self {
            index,
            width: 0.0,
            height: 0.0,
            status: PageStatus::Pass,
            error: None,
            diff: None,
            stats: ObjectStats::default(),
            stats_after: None,
            text_shift: None,
            color_changed: None,
            hints: Vec::new(),
            timings: PageTimings::default(),
            diff_images: None,
        }
    }

    fn fail(&mut self, status: PageStatus, error: String) {
        self.status = status;
        self.error = Some(error);
    }
}

/// 文件级状态。崩溃与超时只在子进程隔离模式下由 CLI 填写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Ok,
    LoadFailed,
    SaveFailed,
    ReopenFailed,
    Crashed,
    TimedOut,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTimings {
    pub load_ms: u64,
    pub prepare_ms: u64,
    pub save_ms: u64,
    pub reopen_ms: u64,
    pub total_ms: u64,
}

/// 一个文件的完整结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileResult {
    #[serde(flatten)]
    pub id: FileId,
    pub status: FileStatus,
    pub error: Option<String>,
    /// 子进程退出码（隔离模式下崩溃时）。
    pub exit_code: Option<i32>,
    pub original_bytes: u64,
    pub saved_bytes: Option<u64>,
    pub page_count: usize,
    pub pages: Vec<PageRecord>,
    pub timings: FileTimings,
    /// 保存结果的字节，只在 [`Options::keep_saved`] 时填写，不进 JSON。
    #[serde(skip)]
    pub saved: Option<Vec<u8>>,
}

impl FileResult {
    /// 构造一个没有处理任何页面的失败结果（打开失败、崩溃、超时）。
    pub fn failed(id: FileId, status: FileStatus, error: impl Into<String>) -> Self {
        Self {
            id,
            status,
            error: Some(error.into()),
            exit_code: None,
            original_bytes: 0,
            saved_bytes: None,
            page_count: 0,
            pages: Vec::new(),
            timings: FileTimings::default(),
            saved: None,
        }
    }
}

/// 每页结果的接收方。差异页附带原件与保存结果的位图，供调用方写差异图；
/// 调用方可以改写记录（例如填入 [`PageRecord::diff_images`]）。
pub type PageSink<'a> = dyn FnMut(&mut PageRecord, Option<(&Bitmap, &Bitmap)>) + 'a;

/// 默认的页面准备步骤：把全部顶层对象标记为已修改并重新生成内容流。
///
/// Form XObject 内部的对象不标记：公共 API 只会重写页面流里对它的引用，
/// Form 内容的改写不在 P0 范围（任务说明）。
pub fn mark_all_and_regenerate(page: &Page<'_>) -> pdfium_sys::Result<()> {
    for obj in page.objects() {
        obj.mark_modified()?;
    }
    page.generate_content()
}

/// 对内存里的一个 PDF 跑往返测试。
pub fn run_bytes(
    lib: &Library,
    id: FileId,
    data: Vec<u8>,
    opts: &Options,
    sink: &mut PageSink<'_>,
) -> FileResult {
    run_bytes_with(lib, id, data, opts, &mut mark_all_and_regenerate, sink)
}

/// 同 [`run_bytes`]，但页面准备步骤由调用方提供（测试用它注入人为改动）。
/// 校准模式下不调用 `prepare`。
pub fn run_bytes_with(
    lib: &Library,
    id: FileId,
    data: Vec<u8>,
    opts: &Options,
    prepare: &mut dyn FnMut(&Page<'_>) -> pdfium_sys::Result<()>,
    sink: &mut PageSink<'_>,
) -> FileResult {
    let started = Instant::now();
    let mut result = FileResult {
        id,
        status: FileStatus::Ok,
        error: None,
        exit_code: None,
        original_bytes: data.len() as u64,
        saved_bytes: None,
        page_count: 0,
        pages: Vec::new(),
        timings: FileTimings::default(),
        saved: None,
    };
    let finish = |mut result: FileResult| {
        result.timings.total_ms = elapsed_ms(started);
        result
    };

    // 1. 打开，逐页统计并重新生成内容流。
    let t = Instant::now();
    let doc = match Document::load_bytes(lib, data.clone(), None) {
        Ok(doc) => doc,
        Err(e) => {
            result.status = FileStatus::LoadFailed;
            result.error = Some(e.to_string());
            return finish(result);
        }
    };
    result.timings.load_ms = elapsed_ms(t);
    let page_count = doc.page_count();
    result.page_count = page_count;

    let t = Instant::now();
    let mut records: Vec<PageRecord> = (0..page_count).map(PageRecord::new).collect();
    for rec in &mut records {
        let tp = Instant::now();
        match doc.page(rec.index) {
            Ok(page) => {
                rec.width = page.width();
                rec.height = page.height();
                rec.stats = object_stats(&page);
                if !opts.calibrate
                    && let Err(e) = prepare(&page)
                {
                    rec.fail(PageStatus::Error, format!("标记与重新生成：{e}"));
                }
            }
            Err(e) => rec.fail(PageStatus::Error, e.to_string()),
        }
        rec.timings.prepare_ms = elapsed_ms(tp);
    }
    result.timings.prepare_ms = elapsed_ms(t);

    // 2. 完整保存到内存。
    let t = Instant::now();
    let saved = match doc.save_to_vec(SaveFlags::NO_INCREMENTAL) {
        Ok(saved) => saved,
        Err(e) => {
            result.status = FileStatus::SaveFailed;
            result.error = Some(e.to_string());
            for rec in &mut records {
                if rec.status != PageStatus::Error {
                    rec.fail(PageStatus::Diff, "保存失败".to_owned());
                }
                sink(rec, None);
            }
            result.pages = records;
            return finish(result);
        }
    };
    result.timings.save_ms = elapsed_ms(t);
    result.saved_bytes = Some(saved.len() as u64);
    if opts.keep_saved {
        result.saved = Some(saved.clone());
    }
    drop(doc);

    // 3. 重开：原件（干净副本，未经 generate_content 触碰）与保存结果。
    let t = Instant::now();
    let pristine = match Document::load_bytes(lib, data, None) {
        Ok(doc) => doc,
        Err(e) => {
            result.status = FileStatus::LoadFailed;
            result.error = Some(format!("再次打开原件失败：{e}"));
            result.pages = records;
            return finish(result);
        }
    };
    let reopened = match Document::load_bytes(lib, saved, None) {
        Ok(doc) => Some(doc),
        Err(e) => {
            result.status = FileStatus::ReopenFailed;
            result.error = Some(format!("保存结果无法打开：{e}"));
            None
        }
    };
    result.timings.reopen_ms = elapsed_ms(t);

    // 4. 逐页渲染、比对、分析。
    let render = RenderOptions {
        scale: opts.scale,
        ..RenderOptions::default()
    };
    for rec in &mut records {
        if rec.status == PageStatus::Error {
            sink(rec, None);
            continue;
        }
        let pixels = (rec.width * opts.scale).round().max(1.0) as usize
            * (rec.height * opts.scale).round().max(1.0) as usize;
        if pixels > opts.max_pixels {
            rec.fail(
                PageStatus::Error,
                format!("页面过大：{pixels} 像素，超过上限 {}", opts.max_pixels),
            );
            sink(rec, None);
            continue;
        }

        let t = Instant::now();
        let page_a = match pristine.page(rec.index) {
            Ok(page) => page,
            Err(e) => {
                rec.fail(PageStatus::Error, format!("原件：{e}"));
                sink(rec, None);
                continue;
            }
        };
        let bmp_a = match page_a.render(render) {
            Ok(bmp) => bmp,
            Err(e) => {
                rec.fail(PageStatus::Error, format!("渲染原件：{e}"));
                sink(rec, None);
                continue;
            }
        };
        rec.timings.render_a_ms = elapsed_ms(t);

        let Some(reopened) = reopened.as_ref() else {
            rec.fail(PageStatus::Diff, "保存结果无法打开".to_owned());
            rec.hints = vec![Hint::ObjectsChanged];
            sink(rec, None);
            continue;
        };
        let t = Instant::now();
        let page_b = match reopened.page(rec.index) {
            Ok(page) => page,
            Err(e) => {
                rec.fail(PageStatus::Diff, format!("保存结果：{e}"));
                rec.hints = vec![Hint::ObjectsChanged];
                sink(rec, None);
                continue;
            }
        };
        let bmp_b = match page_b.render(render) {
            Ok(bmp) => bmp,
            Err(e) => {
                rec.fail(PageStatus::Diff, format!("渲染保存结果：{e}"));
                rec.hints = vec![Hint::ObjectsChanged];
                sink(rec, None);
                continue;
            }
        };
        rec.timings.render_b_ms = elapsed_ms(t);

        let t = Instant::now();
        let cmp = compare::compare(&bmp_a, &bmp_b, &opts.thresholds);
        rec.diff = Some(DiffMetrics::from(&cmp));
        rec.timings.compare_ms = elapsed_ms(t);
        if cmp.passes(&opts.thresholds) {
            rec.status = PageStatus::Pass;
            sink(rec, None);
            continue;
        }

        rec.status = PageStatus::Diff;
        let t = Instant::now();
        rec.stats_after = Some(object_stats(&page_b));
        rec.text_shift = Some(compare_text(&page_a, &page_b));
        rec.color_changed = count_color_changes(&page_a, &page_b);
        rec.hints = infer_hints(rec);
        rec.timings.analyze_ms = elapsed_ms(t);
        sink(rec, Some((&bmp_a, &bmp_b)));
    }

    result.pages = records;
    finish(result)
}

/// 统计一页的对象。
pub fn object_stats(page: &Page<'_>) -> ObjectStats {
    let mut s = ObjectStats {
        page_transparency: page.has_transparency(),
        ..ObjectStats::default()
    };
    for obj in page.objects() {
        s.objects += 1;
        match obj.kind() {
            ObjectKind::Text => {
                s.text += 1;
                if obj.font().is_some_and(|f| f.is_type3()) {
                    s.type3_text += 1;
                }
                if obj.text_render_mode().is_some_and(|m| m.is_clip()) {
                    s.clip_text += 1;
                }
            }
            ObjectKind::Path => s.path += 1,
            ObjectKind::Image => s.image += 1,
            ObjectKind::Shading => s.shading += 1,
            ObjectKind::Form => {
                s.form += 1;
                s.form_inner += obj.form_object_count();
            }
            ObjectKind::Unknown => {}
        }
        if obj.has_transparency() {
            s.transparent += 1;
        }
    }
    s.chars = page
        .text()
        .map(|t| (0..t.char_count()).filter(|&i| !t.is_generated(i)).count())
        .unwrap_or(0);
    s
}

/// 对比两页（原件与保存结果）的字符位置。
pub fn compare_text(a: &Page<'_>, b: &Page<'_>) -> TextShift {
    let origins = |page: &Page<'_>| -> Vec<(f64, f64)> {
        page.text()
            .map(|t| {
                (0..t.char_count())
                    .filter(|&i| !t.is_generated(i))
                    .filter_map(|i| t.char_origin(i))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (oa, ob) = (origins(a), origins(b));
    let mut shift = TextShift {
        chars_before: oa.len(),
        chars_after: ob.len(),
        moved: 0,
        max_shift_pt: 0.0,
    };
    if oa.len() != ob.len() {
        return shift;
    }
    for (pa, pb) in oa.iter().zip(&ob) {
        let d = ((pa.0 - pb.0).powi(2) + (pa.1 - pb.1).powi(2)).sqrt() as f32;
        if d > TEXT_SHIFT_TOLERANCE_PT {
            shift.moved += 1;
        }
        shift.max_shift_pt = shift.max_shift_pt.max(d);
    }
    shift
}

/// 对象数一致时，按序号对应比较填充/描边颜色，返回颜色变化的对象数；对象数不一致返回 `None`。
/// 允许 ±1 的量化误差（生成器写浮点数只保留有限位小数）。
pub fn count_color_changes(a: &Page<'_>, b: &Page<'_>) -> Option<usize> {
    if a.object_count() != b.object_count() {
        return None;
    }
    let same = |x: Option<pdfium_sys::Rgba>, y: Option<pdfium_sys::Rgba>| match (x, y) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            x.r.abs_diff(y.r) <= 1
                && x.g.abs_diff(y.g) <= 1
                && x.b.abs_diff(y.b) <= 1
                && x.a.abs_diff(y.a) <= 1
        }
        _ => false,
    };
    let changed = a
        .objects()
        .zip(b.objects())
        .filter(|(oa, ob)| {
            oa.kind() != ob.kind()
                || !same(oa.fill_color(), ob.fill_color())
                || !same(oa.stroke_color(), ob.stroke_color())
        })
        .count();
    Some(changed)
}

/// 根据前后统计推断差异页的归因线索，按优先级排列。
pub fn infer_hints(rec: &PageRecord) -> Vec<Hint> {
    let mut hints = Vec::new();
    let a = &rec.stats;
    if let Some(b) = &rec.stats_after {
        let shading_lost = a.shading.saturating_sub(b.shading);
        let image_lost = a.image.saturating_sub(b.image);
        if shading_lost > 0 {
            hints.push(Hint::ShadingLost);
        }
        if image_lost > 0 {
            hints.push(Hint::ImageLost);
        }
        if a.type3_text > 0 {
            hints.push(Hint::Type3);
        }
        if a.objects != b.objects && a.objects.abs_diff(b.objects) != shading_lost + image_lost {
            hints.push(Hint::ObjectsChanged);
        }
    } else if a.type3_text > 0 {
        hints.push(Hint::Type3);
    }
    if let Some(ts) = &rec.text_shift {
        if ts.chars_before != ts.chars_after {
            hints.push(Hint::TextChanged);
        } else if ts.moved > 0 {
            hints.push(Hint::TextMoved);
        }
    }
    if rec.color_changed.is_some_and(|n| n > 0) {
        hints.push(Hint::ColorChanged);
    }
    if a.transparent > 0 || a.page_transparency {
        hints.push(Hint::Transparency);
    }
    if a.clip_text > 0 {
        hints.push(Hint::TextClip);
    }
    if hints.is_empty() {
        hints.push(Hint::Unclassified);
    }
    hints
}

/// 一个来源类别（或全部）的汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSummary {
    pub files: usize,
    pub load_failed: usize,
    pub save_failed: usize,
    pub reopen_failed: usize,
    pub crashed: usize,
    pub timed_out: usize,
    /// 所有文件声明的页数之和（含没能比对的页）。
    pub pages: usize,
    pub passed: usize,
    pub diff: usize,
    /// 原件就打不开或渲染不了的页。
    pub errored: usize,
    /// 无差异页里没有任何对象的页（没有内容流可重写，无差异是必然的）。
    pub empty_passed: usize,
    /// 差异页按主要归因计数。
    pub hints: BTreeMap<Hint, usize>,
    pub elapsed_ms: u64,
}

impl SourceSummary {
    /// 参与比对的页数（无差异 + 有差异）。
    pub fn compared(&self) -> usize {
        self.passed + self.diff
    }

    /// 无差异比例；没有可比对的页时为 `None`。
    pub fn pass_ratio(&self) -> Option<f64> {
        let compared = self.compared();
        (compared > 0).then(|| self.passed as f64 / compared as f64)
    }

    fn add_file(&mut self, file: &FileResult) {
        self.files += 1;
        self.elapsed_ms += file.timings.total_ms;
        match file.status {
            FileStatus::Ok => {}
            FileStatus::LoadFailed => self.load_failed += 1,
            FileStatus::SaveFailed => self.save_failed += 1,
            FileStatus::ReopenFailed => self.reopen_failed += 1,
            FileStatus::Crashed => self.crashed += 1,
            FileStatus::TimedOut => self.timed_out += 1,
        }
        self.pages += file.page_count;
        for page in &file.pages {
            match page.status {
                PageStatus::Pass => {
                    self.passed += 1;
                    if page.stats.objects == 0 {
                        self.empty_passed += 1;
                    }
                }
                PageStatus::Diff => {
                    self.diff += 1;
                    let primary = page.hints.first().copied().unwrap_or(Hint::Unclassified);
                    *self.hints.entry(primary).or_default() += 1;
                }
                PageStatus::Error => self.errored += 1,
            }
        }
    }
}

/// 按来源分类汇总，键为来源名。
pub fn summarize(files: &[FileResult]) -> BTreeMap<String, SourceSummary> {
    let mut by_source: BTreeMap<String, SourceSummary> = BTreeMap::new();
    for file in files {
        by_source
            .entry(file.id.source.clone())
            .or_default()
            .add_file(file);
    }
    by_source
}

/// 全部文件的合计。只作参考：方案 13.1 要求按来源分别报告，不用总平均数掩盖某一类的问题。
pub fn total(files: &[FileResult]) -> SourceSummary {
    let mut all = SourceSummary::default();
    for file in files {
        all.add_file(file);
    }
    all
}

/// 把汇总渲染成 Markdown 表格（标准输出和报告共用）。
pub fn render_table(by_source: &BTreeMap<String, SourceSummary>, total: &SourceSummary) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| 来源 | 文件 | 打开失败 | 保存/重开失败 | 崩溃 | 超时 | 页数 | 比对页 | 无差异 | 无差异比例 | 其中无对象页 | 页错误 | 主要归因 |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (source, s) in by_source {
        let _ = writeln!(out, "{}", table_row(source, s));
    }
    if by_source.len() > 1 {
        let _ = writeln!(out, "{}", table_row("（合计，仅供参考）", total));
    }
    out
}

fn table_row(name: &str, s: &SourceSummary) -> String {
    let ratio = s
        .pass_ratio()
        .map_or_else(|| "—".to_owned(), |r| format!("{:.1}%", r * 100.0));
    let mut hints: Vec<(&Hint, &usize)> = s.hints.iter().collect();
    hints.sort_by(|x, y| y.1.cmp(x.1).then(x.0.cmp(y.0)));
    let hints = hints
        .iter()
        .take(3)
        .map(|(h, n)| format!("{} {n}", h.label()))
        .collect::<Vec<_>>()
        .join("，");
    format!(
        "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {ratio} | {} | {} | {} |",
        s.files,
        s.load_failed,
        s.save_failed + s.reopen_failed,
        s.crashed,
        s.timed_out,
        s.pages,
        s.compared(),
        s.passed,
        s.empty_passed,
        s.errored,
        if hints.is_empty() { "—" } else { &hints },
    )
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use pdfium_sys::Rgba;

    use super::*;

    fn with_library(f: impl FnOnce(&Library)) {
        let _guard = crate::pdfium_lock::lock();
        let lib = Library::init().expect("init pdfium");
        f(&lib);
    }

    /// 手工拼一个单页 PDF（200×100 点）：蓝色方块加一行 Helvetica 文字。
    fn sample_pdf() -> Vec<u8> {
        let content = "0 0 1 rg 20 20 60 60 re f BT /F1 18 Tf 100 40 Td (Hi) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn id(source: &str, path: &str) -> FileId {
        FileId {
            path: path.to_owned(),
            source: source.to_owned(),
        }
    }

    #[test]
    fn unchanged_page_has_no_difference() {
        with_library(|lib| {
            for calibrate in [false, true] {
                let opts = Options {
                    calibrate,
                    keep_saved: true,
                    ..Options::default()
                };
                let mut seen = 0;
                let result = run_bytes(
                    lib,
                    id("synthetic", "a.pdf"),
                    sample_pdf(),
                    &opts,
                    &mut |rec, bitmaps| {
                        seen += 1;
                        assert!(bitmaps.is_none(), "无差异页不应附带位图");
                        assert_eq!(rec.status, PageStatus::Pass, "{rec:?}");
                    },
                );
                assert_eq!(seen, 1);
                assert_eq!(result.status, FileStatus::Ok, "{result:?}");
                assert_eq!(result.page_count, 1);
                assert!(result.saved_bytes.is_some_and(|n| n > 0));
                assert!(result.saved.as_ref().is_some_and(
                    |s| s.starts_with(b"%PDF") && s.len() as u64 == result.saved_bytes.unwrap()
                ));
                let page = &result.pages[0];
                assert_eq!(page.status, PageStatus::Pass);
                let diff = page.diff.expect("diff metrics");
                assert_eq!(diff.diff_pixels, 0);
                assert_eq!(
                    diff.total_pixels,
                    400 * 200,
                    "scale 2.0 下 200×100 点为 400×200 像素"
                );
                assert_eq!(
                    (
                        page.stats.objects,
                        page.stats.text,
                        page.stats.path,
                        page.stats.chars
                    ),
                    (2, 1, 1, 2)
                );
                assert!(page.hints.is_empty() && page.stats_after.is_none());
            }
        });
    }

    #[test]
    fn recolored_object_is_detected() {
        with_library(|lib| {
            let mut prepare = |page: &Page<'_>| -> pdfium_sys::Result<()> {
                mark_all_and_regenerate(page)?;
                let square = page.object(0).ok_or(pdfium_sys::Error::PageObject)?;
                square.set_fill_color(Rgba {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                })?;
                page.generate_content()
            };
            let mut got_bitmaps = false;
            let result = run_bytes_with(
                lib,
                id("synthetic", "b.pdf"),
                sample_pdf(),
                &Options::default(),
                &mut prepare,
                &mut |rec, bitmaps| {
                    got_bitmaps = bitmaps.is_some();
                    assert_eq!(rec.status, PageStatus::Diff);
                },
            );
            assert!(got_bitmaps, "差异页应附带两张位图");
            let page = &result.pages[0];
            assert_eq!(page.status, PageStatus::Diff);
            let diff = page.diff.expect("diff metrics");
            // 方块 (20,20)–(80,80) 点，scale 2.0、自顶向下：x 40–160，y 40–160。
            assert_eq!(diff.diff_pixels, 120 * 120, "{diff:?}");
            assert_eq!(diff.bbox, Some([40, 40, 160, 160]));
            assert_eq!(diff.largest_region, 120 * 120);
            assert_eq!(page.color_changed, Some(1));
            assert_eq!(
                page.hints.first(),
                Some(&Hint::ColorChanged),
                "{:?}",
                page.hints
            );
            let after = page.stats_after.expect("stats after");
            assert_eq!(after.objects, 2);
            let shift = page.text_shift.expect("text shift");
            assert_eq!(
                (shift.chars_before, shift.chars_after, shift.moved),
                (2, 2, 0)
            );
        });
    }

    #[test]
    fn unreadable_file_is_load_failure() {
        with_library(|lib| {
            let result = run_bytes(
                lib,
                id("public", "broken.pdf"),
                b"not a pdf".to_vec(),
                &Options::default(),
                &mut |_, _| panic!("没有页面可回调"),
            );
            assert_eq!(result.status, FileStatus::LoadFailed);
            assert!(result.error.is_some());
            assert!(result.pages.is_empty());
        });
    }

    fn page(status: PageStatus, objects: usize, hints: &[Hint]) -> PageRecord {
        let mut rec = PageRecord::new(0);
        rec.status = status;
        rec.stats.objects = objects;
        rec.hints = hints.to_vec();
        rec
    }

    fn file(source: &str, status: FileStatus, pages: Vec<PageRecord>) -> FileResult {
        FileResult {
            id: id(source, "x.pdf"),
            status,
            error: None,
            exit_code: None,
            original_bytes: 1,
            saved_bytes: None,
            page_count: pages.len(),
            pages,
            timings: FileTimings::default(),
            saved: None,
        }
    }

    #[test]
    fn summary_groups_by_source() {
        let files = vec![
            file(
                "office-word",
                FileStatus::Ok,
                vec![
                    page(PageStatus::Pass, 5, &[]),
                    page(PageStatus::Pass, 0, &[]),
                    page(PageStatus::Diff, 3, &[Hint::TextMoved, Hint::Transparency]),
                ],
            ),
            file(
                "office-word",
                FileStatus::Ok,
                vec![page(PageStatus::Pass, 1, &[])],
            ),
            file(
                "design",
                FileStatus::Ok,
                vec![page(PageStatus::Diff, 9, &[Hint::ShadingLost])],
            ),
            file("design", FileStatus::LoadFailed, vec![]),
            file("public", FileStatus::Crashed, vec![]),
            file(
                "public",
                FileStatus::Ok,
                vec![page(PageStatus::Error, 0, &[])],
            ),
        ];
        let by_source = summarize(&files);
        assert_eq!(
            by_source.keys().collect::<Vec<_>>(),
            ["design", "office-word", "public"]
        );

        let word = &by_source["office-word"];
        assert_eq!(
            (word.files, word.pages, word.passed, word.diff),
            (2, 4, 3, 1)
        );
        assert_eq!(word.empty_passed, 1);
        assert_eq!(word.pass_ratio(), Some(0.75));
        assert_eq!(
            word.hints.get(&Hint::TextMoved),
            Some(&1),
            "只按主要归因计数"
        );
        assert_eq!(word.hints.get(&Hint::Transparency), None);

        let design = &by_source["design"];
        assert_eq!((design.files, design.load_failed, design.diff), (2, 1, 1));
        assert_eq!(design.pass_ratio(), Some(0.0));

        let public = &by_source["public"];
        assert_eq!(
            (public.crashed, public.errored, public.compared()),
            (1, 1, 0)
        );
        assert_eq!(public.pass_ratio(), None);

        let all = total(&files);
        assert_eq!((all.files, all.passed, all.diff, all.errored), (6, 3, 2, 1));

        let table = render_table(&by_source, &all);
        assert!(table.contains("| office-word | 2 |"), "{table}");
        assert!(table.contains("75.0%"), "{table}");
        assert!(table.contains("渐变丢失 1"), "{table}");
        assert!(table.contains("合计"), "{table}");
    }

    #[test]
    fn hints_follow_priority_and_fall_back_to_unclassified() {
        let mut rec = page(PageStatus::Diff, 4, &[]);
        rec.stats.shading = 1;
        rec.stats.text = 2;
        rec.stats.transparent = 1;
        rec.stats_after = Some(ObjectStats {
            objects: 3,
            shading: 0,
            text: 2,
            ..ObjectStats::default()
        });
        rec.text_shift = Some(TextShift {
            chars_before: 10,
            chars_after: 10,
            moved: 4,
            max_shift_pt: 1.5,
        });
        assert_eq!(
            infer_hints(&rec),
            [Hint::ShadingLost, Hint::TextMoved, Hint::Transparency],
            "对象数变化已由渐变丢失解释，不再报对象数变化"
        );

        let plain = page(PageStatus::Diff, 1, &[]);
        assert_eq!(infer_hints(&plain), [Hint::Unclassified]);

        let mut text_lost = plain.clone();
        text_lost.stats.type3_text = 1;
        text_lost.stats_after = Some(ObjectStats {
            objects: 0,
            ..ObjectStats::default()
        });
        text_lost.text_shift = Some(TextShift {
            chars_before: 3,
            chars_after: 0,
            moved: 0,
            max_shift_pt: 0.0,
        });
        assert_eq!(
            infer_hints(&text_lost),
            [Hint::Type3, Hint::ObjectsChanged, Hint::TextChanged]
        );
    }

    #[test]
    fn records_survive_json_roundtrip() {
        let files = vec![file(
            "freight",
            FileStatus::Ok,
            vec![page(PageStatus::Diff, 2, &[Hint::ColorChanged])],
        )];
        let json = serde_json::to_string(&files).expect("serialize");
        assert!(json.contains("\"source\":\"freight\""), "{json}");
        let back: Vec<FileResult> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, files);
        let summary = serde_json::to_string(&summarize(&files)).expect("serialize summary");
        assert!(summary.contains("\"color_changed\":1"), "{summary}");
    }
}
