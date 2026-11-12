//! 单个文件的处理：对应、随机修改、保存（含回退）、读回核对、像素比对、对照组。
//!
//! 保存结果是三段接起来的：原文件字节，自研改写层的增量更新（对得上的页），PDFium 的增量保存
//! （对不上、退回生成器整条重写的页）。之后重新打开保存结果，逐页与预期画面比对。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use magpdf_content::content::{Content, StreamData, TokenKind};
use magpdf_content::map::{self, PageMap, Unmatched};
use magpdf_content::pdf::PdfDocument;
use magpdf_content::pdf::pages::{Contents, PageInfo, walk_pages};
use magpdf_content::pdf::pairs::check_page;
use magpdf_content::rewrite::{
    EditSet, EditTarget, StreamReplacement, plan_page, save_full_rewrite, save_incremental,
};
use magpdf_engine::compare;
use magpdf_engine::roundtrip::{DiffImages, DiffMetrics, mark_all_and_regenerate};
use pdfium_sys::{
    Bitmap, Document, Library, Matrix, ObjectKind, Page, PageGeometry, RenderOptions, Rgba,
    SaveFlags,
};

use super::model::{
    Check, CheckStatus, Config, EditKind, EditMix, EditRecord, ExpectedMode, FileResult,
    FileStatus, MOVE_DX, MOVE_DY, OutputStats, PageRecord, RECOLOR_RGB, Route, Timings,
};
use crate::image::{heatmap, write_png};

/// 对应本身没问题、只是改写层写不了这次修改。汇总时这样的页不算“对不上”。
pub(super) const REASON_REWRITE: &str = "改写层写不了这次修改";

/// 退回生成器的原因（汇总时按它计数，所以只写类别，细节放进 [`PageWork::notes`]）。
mod reason {
    pub const OBJECT_LAYER: &str = "自研对象层打不开文件";
    pub const ENCRYPTED: &str = "文件加密，自研对象层不解密";
    pub const PAGE_COUNT: &str = "页树里的页数与 PDFium 不一致";
    pub const GEOMETRY: &str = "页面尺寸或旋转与 PDFium 不一致";
}

/// 处理中的一页。
struct PageWork {
    index: usize,
    /// PDFium 加载这一页失败。
    error: Option<String>,
    geometry: Option<PageGeometry>,
    objects: usize,
    kinds: Vec<ObjectKind>,
    info: Option<PageInfo>,
    map: Option<PageMap>,
    reasons: Vec<String>,
    notes: Vec<String>,
    streams: usize,
    streams_identical: usize,
    streams_unchecked: usize,
    operators: usize,
    edits: Vec<(usize, EditKind)>,
    /// 改写层要写回的内容（各条流拼接之后）。`None` 表示这一页不写。
    planned: Option<Vec<u8>>,
    verify: Option<Check>,
    control: Option<Check>,
}

impl PageWork {
    fn route(&self) -> Route {
        if self.reasons.is_empty() && self.map.is_some() {
            Route::Matched
        } else {
            Route::Fallback
        }
    }

    fn fail(&mut self, reason: &str, note: impl Into<String>) {
        if !self.reasons.iter().any(|r| r == reason) {
            self.reasons.push(reason.to_owned());
        }
        let note = note.into();
        if !note.is_empty() {
            self.notes.push(note);
        }
    }
}

/// 差异图写到哪里、文件名用什么开头。
struct DiffTarget<'a> {
    dir: &'a Path,
    source: &'a str,
    rel: &'a str,
}

/// 处理一个文件。
pub fn run_file(
    lib: &Library,
    path: &Path,
    rel: &str,
    source: &str,
    config: &Config,
    save_dir: Option<&Path>,
    diff_dir: Option<&Path>,
) -> FileResult {
    let mut timings = Timings::default();

    let t = Instant::now();
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            return FileResult::failed(
                rel,
                source,
                FileStatus::LoadFailed,
                format!("读取失败：{e}"),
            );
        }
    };
    let original_len = data.len() as u64;
    let pdf = PdfDocument::load(data.clone());
    timings.read_us = micros(t);

    // PDFium 这一侧：每页的尺寸、顶层对象数和类别。
    let mut work: Vec<PageWork> = {
        let pdfium = match Document::load_bytes(lib, data.clone(), None) {
            Ok(d) => d,
            Err(e) => {
                let mut failed = FileResult::failed(
                    rel,
                    source,
                    FileStatus::LoadFailed,
                    format!("PDFium 打不开：{e}"),
                );
                failed.bytes = original_len;
                failed.timings = timings;
                return failed;
            }
        };
        (0..pdfium.page_count())
            .map(|i| probe_page(&pdfium, i))
            .collect()
    };
    let page_count = work.len();

    let (pdf, object_layer_error) = match pdf {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(e.to_string())),
    };

    // 对应：自研这一侧取内容流、分词、按 PDFium 的规则数对象，再和 PDFium 的对象数核对。
    let t = Instant::now();
    match &pdf {
        None => {
            for w in &mut work {
                w.fail(reason::OBJECT_LAYER, "");
            }
        }
        Some(pdf) if pdf.encrypt.is_some() => {
            for w in &mut work {
                w.fail(reason::ENCRYPTED, "");
            }
        }
        Some(pdf) => {
            let infos = walk_pages(pdf, page_count.max(1).saturating_mul(2));
            if infos.len() == page_count {
                for (w, info) in work.iter_mut().zip(&infos) {
                    map_one(pdf, info, w);
                }
            } else {
                // 页数都对不上时，序号相同的页未必是同一页，整份文件都不走改写层。
                let note = format!("页树 {} 页，PDFium {} 页", infos.len(), page_count);
                for w in &mut work {
                    w.fail(reason::PAGE_COUNT, note.clone());
                }
            }
        }
    }
    timings.map_us = micros(t);

    // 随机修改：每页独立播种，抽哪些对象与这一页对不对得上无关。
    if config.modify_ratio > 0.0 {
        for w in &mut work {
            if w.error.is_none() && w.objects > 0 {
                w.edits = plan_random_edits(rel, w.index, &w.kinds, config);
            }
        }
    }

    // 对得上的页由改写层写。
    let t = Instant::now();
    for w in &mut work {
        if w.error.is_some() || w.route() != Route::Matched {
            continue;
        }
        if w.edits.is_empty() && !config.rewrite_all {
            continue;
        }
        let Some(map) = &w.map else { continue };
        match plan_page(map, &edit_set(&w.edits)) {
            Ok(rewrite) => {
                if rewrite.changed || config.rewrite_all {
                    w.planned = Some(rewrite.data);
                }
            }
            Err(e) => w.fail(REASON_REWRITE, e.to_string()),
        }
    }

    let mut deshared_streams = 0usize;
    let mut full_rewrite_bytes = 0u64;
    let mut stage_a: Option<Vec<u8>> = None;
    if let Some(pdf) = &pdf {
        let mut replacements: Vec<StreamReplacement> = Vec::new();
        let mut page_streams: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for w in &work {
            let (Some(planned), Some(info)) = (&w.planned, &w.info) else {
                continue;
            };
            let nums = content_nums(&info.contents);
            let Some((first, rest)) = nums.split_first() else {
                continue;
            };
            page_streams.insert(info.number, nums.clone());
            replacements.push(StreamReplacement {
                object_number: *first,
                data: Some(planned.clone()),
                compress: true,
            });
            // 改写层现在把整页写进第一条流，其余的流清空。
            for n in rest {
                replacements.push(StreamReplacement {
                    object_number: *n,
                    data: Some(Vec::new()),
                    compress: false,
                });
            }
        }
        let deshare = shared_streams(&work);
        deshared_streams = replacements
            .iter()
            .filter(|r| deshare.contains(&r.object_number))
            .count();
        full_rewrite_bytes = save_full_rewrite(pdf, true).bytes.len() as u64;
        if !replacements.is_empty() {
            stage_a =
                Some(save_incremental(pdf, &page_streams, &replacements, &deshare, true).bytes);
        }
    }
    timings.save_us = micros(t);
    let stage_a = stage_a.unwrap_or_else(|| data.clone());
    let incremental_bytes = (stage_a.len() as u64).saturating_sub(original_len);

    // 对不上的页：退回 PDFium 的生成器整条重写，结果接在后面。
    let t = Instant::now();
    let mut file_notes: Vec<String> = Vec::new();
    let needs_fallback = work
        .iter()
        .any(|w| w.error.is_none() && w.route() == Route::Fallback && w.objects > 0);
    let saved = if needs_fallback {
        match regenerate_fallback_pages(lib, &stage_a, &mut work) {
            Ok(bytes) => bytes,
            Err(e) => {
                file_notes.push(e);
                stage_a
            }
        }
    } else {
        stage_a
    };
    timings.fallback_us = micros(t);
    let fallback_bytes = (saved.len() as u64)
        .saturating_sub(original_len)
        .saturating_sub(incremental_bytes);

    if let Some(dir) = save_dir {
        let dest = dest_path(dir, rel);
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&dest, &saved) {
            file_notes.push(format!("写出保存结果失败：{e}"));
        }
    }

    // 读回核对：对得上的页，保存结果里的内容流应当正是写进去的（或者原样没动）。
    let t = Instant::now();
    let mut verify_failures = file_notes;
    if pdf.is_some() {
        verify_failures.extend(read_back(&saved, &work));
    }
    timings.verify_us = micros(t);

    // 像素比对：预期画面是在 PDFium 内存里做同一组修改之后的样子。
    let t = Instant::now();
    let diff_target = diff_dir.map(|dir| DiffTarget { dir, source, rel });
    match Document::load_bytes(lib, data.clone(), None) {
        Err(e) => {
            for w in &mut work {
                w.verify = Some(Check::error(format!("预期文档打不开：{e}")));
            }
            timings.render_us = micros(t);
        }
        Ok(expected) => {
            for w in &mut work {
                if w.error.is_some() || w.edits.is_empty() {
                    continue;
                }
                if let Err(e) = prepare_expected(&expected, w.index, &w.edits, config.expected) {
                    w.notes.push(format!("预期画面：{e}"));
                }
            }
            let saved_doc = Document::load_bytes(lib, saved.clone(), None)
                .map_err(|e| format!("保存结果 PDFium 打不开：{e}"));
            for w in &mut work {
                if w.error.is_some() {
                    continue;
                }
                w.verify = Some(compare_page(
                    &expected,
                    saved_doc.as_ref(),
                    w.index,
                    config,
                    diff_target.as_ref(),
                    "verify",
                ));
            }
            timings.render_us = micros(t);

            if config.control {
                let t = Instant::now();
                let control = run_control(lib, &data, &work).and_then(|bytes| {
                    Document::load_bytes(lib, bytes, None)
                        .map_err(|e| format!("对照组的保存结果打不开：{e}"))
                });
                for w in &mut work {
                    if w.error.is_some() {
                        continue;
                    }
                    w.control = Some(compare_page(
                        &expected,
                        control.as_ref(),
                        w.index,
                        config,
                        diff_target.as_ref(),
                        "control",
                    ));
                }
                timings.control_us = micros(t);
            }
        }
    }
    for w in &work {
        if let Some(line) = pixel_failure_line(w) {
            verify_failures.push(line);
        }
    }

    let page_records = work.iter().map(page_record).collect();
    FileResult {
        path: rel.to_owned(),
        source: source.to_owned(),
        bytes: original_len,
        pages: page_count,
        status: FileStatus::Ok,
        error: None,
        object_layer_error,
        timings,
        output: Some(OutputStats {
            saved_bytes: saved.len() as u64,
            incremental_bytes,
            fallback_bytes,
            full_rewrite_bytes,
            bytes_delta: saved.len() as i64 - original_len as i64,
            deshared_streams,
            verify_failures,
        }),
        page_records,
    }
}

/// 加载 PDFium 的一页，记下尺寸、对象数和各对象的类别。
fn probe_page(doc: &Document<'_>, index: usize) -> PageWork {
    let mut w = PageWork {
        index,
        error: None,
        geometry: None,
        objects: 0,
        kinds: Vec::new(),
        info: None,
        map: None,
        reasons: Vec::new(),
        notes: Vec::new(),
        streams: 0,
        streams_identical: 0,
        streams_unchecked: 0,
        operators: 0,
        edits: Vec::new(),
        planned: None,
        verify: None,
        control: None,
    };
    match doc.page(index) {
        Ok(page) => {
            w.geometry = Some(PageGeometry {
                index,
                width: page.width(),
                height: page.height(),
                rotation: page.rotation(),
                bounds: page.bounding_box(),
            });
            w.kinds = page.objects().map(|o| o.kind()).collect();
            w.objects = page.object_count();
        }
        Err(e) => w.error = Some(format!("PDFium 加载页面失败：{e}")),
    }
    w
}

/// 自研这一侧的对应，并与 PDFium 的对象数核对。
fn map_one(pdf: &PdfDocument, info: &PageInfo, w: &mut PageWork) {
    w.info = Some(info.clone());
    w.streams = content_nums(&info.contents).len();
    if w.error.is_some() {
        w.streams_unchecked = w.streams;
        return;
    }
    if let Some(g) = &w.geometry
        && let Err(why) = check_page(pdf, info, g)
    {
        w.fail(reason::GEOMETRY, why);
    }
    let mut page_map = match map::map_page(pdf, info) {
        Ok(m) => m,
        Err(why) => {
            w.fail(why.label(), "");
            w.streams_unchecked = w.streams;
            return;
        }
    };
    w.operators = page_map
        .tokens
        .iter()
        .filter(|tok| tok.kind == TokenKind::Operator)
        .count();
    if page_map.objects.len() != w.objects {
        page_map.unmatched.push(Unmatched::ObjectCountMismatch);
        page_map.unmatched.sort_unstable();
        page_map.unmatched.dedup();
        w.notes.push(format!(
            "自研数出 {} 个对象，PDFium {} 个",
            page_map.objects.len(),
            w.objects
        ));
    }
    for why in &page_map.unmatched {
        w.fail(why.label(), "");
    }
    // 不改任何对象：拆开再拼回去应当逐字节相同。
    match plan_page(&page_map, &EditSet::default()) {
        Ok(rewrite) if rewrite.data == page_map.content.data => {
            w.streams_identical = w.streams;
        }
        Ok(rewrite) => w.notes.push(format!(
            "不改动时拼回去的内容与原文不同：原文 {} 字节，拼回 {} 字节",
            page_map.content.data.len(),
            rewrite.data.len()
        )),
        Err(_) => w.streams_unchecked = w.streams,
    }
    w.map = Some(page_map);
}

fn edit_set(edits: &[(usize, EditKind)]) -> EditSet {
    let mut set = EditSet::default();
    for (index, kind) in edits {
        let target = match kind {
            EditKind::Move => EditTarget::Move {
                dx: f64::from(MOVE_DX),
                dy: f64::from(MOVE_DY),
            },
            EditKind::Recolor => EditTarget::Recolor {
                r: f64::from(RECOLOR_RGB[0]) / 255.0,
                g: f64::from(RECOLOR_RGB[1]) / 255.0,
                b: f64::from(RECOLOR_RGB[2]) / 255.0,
            },
            EditKind::Delete => EditTarget::Delete,
        };
        set.edits.insert(*index, target);
    }
    set
}

fn content_nums(contents: &Option<Contents>) -> Vec<u32> {
    match contents {
        None => Vec::new(),
        Some(Contents::One(n)) => vec![*n],
        Some(Contents::Many(v)) => v.clone(),
    }
}

/// 被不止一页用到的内容流对象号。
fn shared_streams(work: &[PageWork]) -> BTreeSet<u32> {
    let mut uses: BTreeMap<u32, usize> = BTreeMap::new();
    for w in work {
        if let Some(info) = &w.info {
            for n in content_nums(&info.contents) {
                *uses.entry(n).or_default() += 1;
            }
        }
    }
    uses.into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(k, _)| k)
        .collect()
}

fn micros(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn dest_path(dir: &Path, rel: &str) -> std::path::PathBuf {
    let mut dest = dir.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        dest.push(part);
    }
    dest
}

fn path_hash(rel: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for b in rel.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// 给一页抽修改：约 `modify_ratio` 的对象，至少一个。
///
/// 每页用 `种子 ^ 路径 ^ 页序号` 独立播种（第 1 页不掺页序号，所以单页文件抽到的对象与首轮相同）。
/// 改色只对有填充色可改的对象（路径、文字）有意义；轮到别的类别时改成移动。
fn plan_random_edits(
    rel: &str,
    page_index: usize,
    kinds: &[ObjectKind],
    config: &Config,
) -> Vec<(usize, EditKind)> {
    let n = kinds.len();
    if n == 0 {
        return Vec::new();
    }
    let page_mix = (page_index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut rng = SplitMix64(config.seed ^ path_hash(rel) ^ page_mix);
    let k = (((n as f64) * config.modify_ratio).round() as usize).clamp(1, n);
    let mut indices: Vec<usize> = (0..n).collect();
    for i in (1..indices.len()).rev() {
        let j = (rng.next_u64() as usize) % (i + 1);
        indices.swap(i, j);
    }
    let mut edits = Vec::with_capacity(k);
    for (turn, index) in indices.into_iter().take(k).enumerate() {
        let pick = match config.edit_mix {
            EditMix::Cycle => turn % 3,
            EditMix::Random => (rng.next_u64() % 3) as usize,
        };
        let kind = match pick {
            0 => EditKind::Move,
            1 if matches!(kinds[index], ObjectKind::Path | ObjectKind::Text) => EditKind::Recolor,
            1 => EditKind::Move,
            _ => EditKind::Delete,
        };
        edits.push((index, kind));
    }
    edits
}

/// 在 PDFium 的页面对象上做一组修改：先做移动和改色，最后从后往前删，序号才不会错位。
fn apply_edits(page: &Page<'_>, edits: &[(usize, EditKind)]) -> Result<(), String> {
    for (index, kind) in edits {
        let Some(obj) = page.object(*index) else {
            return Err(format!("对象 {index} 不存在"));
        };
        match kind {
            EditKind::Move => obj
                .transform(Matrix::translation(MOVE_DX, MOVE_DY))
                .map_err(|e| format!("移动对象 {index} 失败：{e}"))?,
            EditKind::Recolor => {
                // 只换颜色，不透明度沿用原来的（与引擎的 `set_fill_rgb` 相同）。
                let alpha = obj.fill_color().map_or(255, |c| c.a);
                obj.set_fill_color(Rgba {
                    r: RECOLOR_RGB[0],
                    g: RECOLOR_RGB[1],
                    b: RECOLOR_RGB[2],
                    a: alpha,
                })
                .map_err(|e| format!("改色对象 {index} 失败：{e}"))?;
            }
            EditKind::Delete => {}
        }
    }
    let mut deletes: Vec<usize> = edits
        .iter()
        .filter(|(_, kind)| *kind == EditKind::Delete)
        .map(|(index, _)| *index)
        .collect();
    deletes.sort_unstable();
    for index in deletes.into_iter().rev() {
        let Some(obj) = page.object(index) else {
            return Err(format!("删除对象 {index} 失败：对象不存在"));
        };
        page.remove_object(obj)
            .map_err(|e| format!("删除对象 {index} 失败：{e}"))?;
    }
    Ok(())
}

/// 回退：在 `base`（原文件加自研的增量更新）上打开 PDFium，对不上的页做同一组修改，
/// 整页标记为已修改、让生成器重写，再增量保存。返回完整的保存结果。
fn regenerate_fallback_pages(
    lib: &Library,
    base: &[u8],
    work: &mut [PageWork],
) -> Result<Vec<u8>, String> {
    let doc = Document::load_bytes(lib, base.to_vec(), None)
        .map_err(|e| format!("回退：PDFium 打不开改写层的保存结果：{e}"))?;
    for w in work.iter_mut() {
        if w.error.is_some() || w.route() != Route::Fallback || w.objects == 0 {
            continue;
        }
        let page = match doc.page(w.index) {
            Ok(p) => p,
            Err(e) => {
                w.notes.push(format!("回退：页面打不开：{e}"));
                continue;
            }
        };
        if let Err(e) = apply_edits(&page, &w.edits) {
            w.notes.push(format!("回退：{e}"));
        }
        if let Err(e) = mark_all_and_regenerate(&page) {
            w.notes.push(format!("回退：生成器重写失败：{e}"));
        }
    }
    doc.save_to_vec(SaveFlags::INCREMENTAL)
        .map_err(|e| format!("回退：PDFium 增量保存失败：{e}"))
}

/// 从保存结果读回对得上的页的内容流，与写进去的（没写的页与原文）逐字节比较。
fn read_back(saved: &[u8], work: &[PageWork]) -> Vec<String> {
    let mut failures = Vec::new();
    let matched: Vec<&PageWork> = work
        .iter()
        .filter(|w| w.error.is_none() && w.route() == Route::Matched)
        .collect();
    if matched.is_empty() {
        return failures;
    }
    let back = match PdfDocument::load(saved.to_vec()) {
        Ok(doc) => doc,
        Err(e) => {
            failures.push(format!("保存结果自研对象层读不回：{e}"));
            return failures;
        }
    };
    let infos = walk_pages(&back, work.len().max(1).saturating_mul(2));
    for w in matched {
        let Some(page_map) = &w.map else { continue };
        let expected = match (&w.planned, &w.info) {
            (Some(planned), Some(info)) => {
                let nums = content_nums(&info.contents);
                let streams: Vec<StreamData> = nums
                    .iter()
                    .enumerate()
                    .map(|(i, n)| StreamData {
                        index: i,
                        object_number: *n,
                        data: if i == 0 { planned.clone() } else { Vec::new() },
                    })
                    .collect();
                Content::concat(&streams).data
            }
            _ => page_map.content.data.clone(),
        };
        let got = infos
            .get(w.index)
            .and_then(|info| map::read_streams(&back, info.contents.as_ref()).ok())
            .map(|streams| Content::concat(&streams).data);
        match got {
            Some(got) if got == expected => {}
            Some(got) => {
                let at = got
                    .iter()
                    .zip(&expected)
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| got.len().min(expected.len()));
                failures.push(format!(
                    "第 {} 页：从保存结果读回的内容流与写进去的不一致（写入 {} 字节，读回 {} 字节，第一处不同在偏移 {}）",
                    w.index + 1,
                    expected.len(),
                    got.len(),
                    at
                ));
            }
            None => failures.push(format!(
                "第 {} 页：保存结果里读不出这一页的内容流",
                w.index + 1
            )),
        }
    }
    failures
}

/// 在预期文档上做这一页的修改。
fn prepare_expected(
    doc: &Document<'_>,
    index: usize,
    edits: &[(usize, EditKind)],
    mode: ExpectedMode,
) -> Result<(), String> {
    match mode {
        ExpectedMode::Direct => {
            // 页面一直开着，后面渲染用的就是改过的这一份。
            let page = doc.keep_page(index).map_err(|e| e.to_string())?;
            apply_edits(&page, edits)
        }
        ExpectedMode::Regenerated => {
            let page = doc.page(index).map_err(|e| e.to_string())?;
            apply_edits(&page, edits)?;
            mark_all_and_regenerate(&page).map_err(|e| format!("重新生成内容流失败：{e}"))
        }
    }
}

/// 对照组：同一组修改，每一页都标记为已修改、交给 PDFium 的生成器重写，完整保存。
fn run_control(lib: &Library, data: &[u8], work: &[PageWork]) -> Result<Vec<u8>, String> {
    let doc = Document::load_bytes(lib, data.to_vec(), None)
        .map_err(|e| format!("对照组打不开原件：{e}"))?;
    for w in work {
        if w.error.is_some() {
            continue;
        }
        let Ok(page) = doc.page(w.index) else {
            continue;
        };
        // 个别修改做不了、个别页生成失败都不中断：保存结果照样比对，差异会体现在这一页上。
        let _ = apply_edits(&page, &w.edits);
        let _ = mark_all_and_regenerate(&page);
    }
    doc.save_to_vec(SaveFlags::NO_INCREMENTAL)
        .map_err(|e| format!("对照组保存失败：{e}"))
}

/// 渲染预期文档和实际文档的同一页，按 V02 的规则比对。
fn compare_page(
    expected: &Document<'_>,
    actual: Result<&Document<'_>, &String>,
    index: usize,
    config: &Config,
    diff_target: Option<&DiffTarget<'_>>,
    tag: &str,
) -> Check {
    let render = RenderOptions {
        scale: config.scale,
        ..RenderOptions::default()
    };
    let exp_page = match expected.page(index) {
        Ok(p) => p,
        Err(e) => return Check::error(format!("预期文档这一页打不开：{e}")),
    };
    let pixels = (exp_page.width() * config.scale).round().max(1.0) as usize
        * (exp_page.height() * config.scale).round().max(1.0) as usize;
    if pixels > config.max_pixels {
        return Check::error(format!(
            "页面过大：{pixels} 像素，超过上限 {}",
            config.max_pixels
        ));
    }
    let a = match exp_page.render(render) {
        Ok(b) => b,
        Err(e) => return Check::error(format!("渲染预期画面失败：{e}")),
    };
    let actual = match actual {
        Ok(doc) => doc,
        Err(e) => return Check::diff(e.clone()),
    };
    let act_page = match actual.page(index) {
        Ok(p) => p,
        Err(e) => return Check::diff(format!("保存结果这一页打不开：{e}")),
    };
    let b = match act_page.render(render) {
        Ok(b) => b,
        Err(e) => return Check::diff(format!("渲染保存结果失败：{e}")),
    };
    let cmp = compare::compare(&a, &b, &config.thresholds);
    let passed = cmp.passes(&config.thresholds);
    let mut check = Check {
        status: if passed {
            CheckStatus::Pass
        } else {
            CheckStatus::Diff
        },
        diff: Some(DiffMetrics::from(&cmp)),
        error: None,
        images: None,
        expected_hash: Some(bitmap_hash(&a)),
        actual_hash: Some(bitmap_hash(&b)),
    };
    if !passed && let Some(target) = diff_target {
        match write_diff_images(
            target,
            index,
            tag,
            &a,
            &b,
            config.thresholds.channel_tolerance,
        ) {
            Ok(images) => check.images = Some(images),
            // 日志里不写路径（《工程规范》4）
            Err(e) => tracing::warn!(page = index, error = %e, "写差异图失败"),
        }
    }
    check
}

/// 位图的指纹（FNV-1a 64 位）。只用来判断两次运行渲染出的是不是同一张图。
fn bitmap_hash(bitmap: &Bitmap) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for y in 0..bitmap.height() {
        for byte in bitmap.row(y) {
            h ^= u64::from(*byte);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// 写三张 PNG：预期画面、保存结果、差异热图。
fn write_diff_images(
    target: &DiffTarget<'_>,
    page_index: usize,
    tag: &str,
    expected: &Bitmap,
    actual: &Bitmap,
    tolerance: u8,
) -> anyhow::Result<DiffImages> {
    std::fs::create_dir_all(target.dir)?;
    let rel = target.rel.strip_suffix(".pdf").unwrap_or(target.rel);
    let stem = format!(
        "{}__{}__p{:03}_{tag}",
        sanitize(target.source),
        sanitize(rel),
        page_index + 1
    );
    let images = DiffImages {
        a: format!("{stem}_expected.png"),
        b: format!("{stem}_saved.png"),
        heat: format!("{stem}_diff.png"),
    };
    write_png(expected, &target.dir.join(&images.a))?;
    write_png(actual, &target.dir.join(&images.b))?;
    write_png(
        &heatmap(expected, actual, tolerance)?,
        &target.dir.join(&images.heat),
    )?;
    Ok(images)
}

/// 只保留字母数字和 `-`、`.`、`_`，其余（含路径分隔符）改成下划线。
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '.' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Text => "文字",
        ObjectKind::Path => "路径",
        ObjectKind::Image => "图像",
        ObjectKind::Shading => "渐变",
        ObjectKind::Form => "表单",
        ObjectKind::Unknown => "未知",
    }
}

/// 像素比对不过的页：写一行原因，进 `verify_failures`。
fn pixel_failure_line(w: &PageWork) -> Option<String> {
    let check = w.verify.as_ref()?;
    if check.status != CheckStatus::Diff {
        return None;
    }
    let what = match (&check.diff, &check.error) {
        (Some(d), _) => format!(
            "差异像素 {}（{:.3}%），最大连通区 {} 像素，包围盒 {}",
            d.diff_pixels,
            d.diff_ratio * 100.0,
            d.largest_region,
            d.bbox.map_or_else(|| "无".to_owned(), |b| format!("{b:?}")),
        ),
        (None, Some(e)) => e.clone(),
        (None, None) => "原因不明".to_owned(),
    };
    let route = match w.route() {
        Route::Matched => "改写层".to_owned(),
        Route::Fallback => format!("回退生成器（{}）", w.reasons.join("、")),
    };
    let edits = if w.edits.is_empty() {
        "没有修改".to_owned()
    } else {
        w.edits
            .iter()
            .map(|(index, kind)| {
                let obj = w.kinds.get(*index).copied().map_or("?", kind_label);
                format!("{}第 {} 个对象（{obj}）", kind.label(), index + 1)
            })
            .collect::<Vec<_>>()
            .join("，")
    };
    Some(format!(
        "第 {} 页：保存结果与预期画面不一致：{what}；写回方式：{route}；修改：{edits}",
        w.index + 1
    ))
}

fn page_record(w: &PageWork) -> PageRecord {
    PageRecord {
        index: w.index,
        route: w.route(),
        reasons: w.reasons.clone(),
        notes: w.notes.clone(),
        error: w.error.clone(),
        objects: w.objects,
        streams: w.streams,
        streams_identical: w.streams_identical,
        streams_unchecked: w.streams_unchecked,
        operators: w.operators,
        edits: w
            .edits
            .iter()
            .map(|(index, kind)| EditRecord {
                object: *index,
                object_kind: w
                    .kinds
                    .get(*index)
                    .copied()
                    .map_or("?", kind_label)
                    .to_owned(),
                kind: *kind,
            })
            .collect(),
        verify: w.verify.clone(),
        control: w.control.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(mix: EditMix, ratio: f64) -> Config {
        Config {
            modify_ratio: ratio,
            edit_mix: mix,
            ..Config::default()
        }
    }

    #[test]
    fn random_edits_are_reproducible_and_bounded() {
        let kinds = vec![ObjectKind::Path; 100];
        let cfg = config(EditMix::Cycle, 0.05);
        let a = plan_random_edits("a.pdf", 0, &kinds, &cfg);
        let b = plan_random_edits("a.pdf", 0, &kinds, &cfg);
        assert_eq!(a, b, "同一个种子、同一页，抽到的应当相同");
        assert_eq!(a.len(), 5);
        let mut seen: Vec<usize> = a.iter().map(|(i, _)| *i).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 5, "不重复抽同一个对象");
        assert!(seen.iter().all(|i| *i < 100));
        // 轮流取移动、改色、删除。
        let order: Vec<EditKind> = a.iter().map(|(_, k)| *k).collect();
        assert_eq!(
            order,
            [
                EditKind::Move,
                EditKind::Recolor,
                EditKind::Delete,
                EditKind::Move,
                EditKind::Recolor
            ]
        );
        // 不同的页、不同的文件抽到的不同。
        assert_ne!(a, plan_random_edits("a.pdf", 1, &kinds, &cfg));
        assert_ne!(a, plan_random_edits("b.pdf", 0, &kinds, &cfg));
    }

    #[test]
    fn small_pages_get_exactly_one_edit() {
        let cfg = config(EditMix::Cycle, 0.05);
        for n in 1..30 {
            let kinds = vec![ObjectKind::Text; n];
            let edits = plan_random_edits("x.pdf", 0, &kinds, &cfg);
            assert_eq!(edits.len(), 1, "{n} 个对象");
            assert_eq!(edits[0].1, EditKind::Move);
        }
        assert!(plan_random_edits("x.pdf", 0, &[], &cfg).is_empty());
    }

    #[test]
    fn recolor_only_applies_to_paths_and_text() {
        let cfg = config(EditMix::Random, 1.0);
        let kinds: Vec<ObjectKind> = (0..300)
            .map(|i| match i % 5 {
                0 => ObjectKind::Path,
                1 => ObjectKind::Text,
                2 => ObjectKind::Image,
                3 => ObjectKind::Shading,
                _ => ObjectKind::Form,
            })
            .collect();
        let edits = plan_random_edits("mix.pdf", 0, &kinds, &cfg);
        assert_eq!(edits.len(), 300);
        let mut counts = BTreeMap::new();
        for (index, kind) in &edits {
            *counts.entry(*kind).or_insert(0usize) += 1;
            if *kind == EditKind::Recolor {
                assert!(matches!(kinds[*index], ObjectKind::Path | ObjectKind::Text));
            }
        }
        assert!(
            counts.len() == 3 && counts.values().all(|n| *n > 30),
            "三种修改都应当抽到不少：{counts:?}"
        );
    }

    #[test]
    fn failure_line_names_the_page_route_and_edits() {
        let mut w = PageWork {
            index: 2,
            error: None,
            geometry: None,
            objects: 3,
            kinds: vec![ObjectKind::Path, ObjectKind::Text, ObjectKind::Image],
            info: None,
            map: None,
            reasons: vec!["对象数不一致".to_owned()],
            notes: Vec::new(),
            streams: 1,
            streams_identical: 0,
            streams_unchecked: 1,
            operators: 0,
            edits: vec![(1, EditKind::Move)],
            planned: None,
            verify: Some(Check::diff("保存结果这一页打不开")),
            control: None,
        };
        let line = pixel_failure_line(&w).expect("有差异的页应当有一行");
        assert!(line.starts_with("第 3 页"), "{line}");
        assert!(line.contains("回退生成器（对象数不一致）"), "{line}");
        assert!(line.contains("移动第 2 个对象（文字）"), "{line}");
        w.verify = Some(Check {
            status: CheckStatus::Pass,
            ..Check::error("")
        });
        assert!(pixel_failure_line(&w).is_none());
    }

    #[test]
    fn saved_paths_stay_inside_the_output_directory() {
        let dest = dest_path(Path::new("out"), "../a/./b.pdf");
        assert_eq!(dest, Path::new("out").join("a").join("b.pdf"));
        assert_eq!(sanitize("sub dir/a:b"), "sub_dir_a_b");
    }
}
