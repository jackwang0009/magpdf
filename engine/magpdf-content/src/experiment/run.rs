//! Per-file V17 experiment runner (identity, random edits, save, render, control).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use pdfium_sys::{Bitmap, Document, Library, Matrix, Page, RenderOptions, Rgba, SaveFlags};

use crate::content::TokenKind;
use crate::map::{self, PageMap, Unmatched};
use crate::pdf::pages::Contents;
use crate::pdf::pairs::{PagePair, pair_pages};
use crate::pdf::{PdfDocument, pages::PageInfo};
use crate::rewrite::{
    EditSet, EditTarget, StreamReplacement, plan_page, save_full_rewrite, save_incremental,
};

use super::{Config, FileResult, OutputStats, PageOutcome, Stats, Timings};

/// Entry used by [`super::run_file`].
pub fn run_file(
    lib: &Library,
    path: &Path,
    rel: &str,
    source: &str,
    config: &Config,
    save_dir: Option<&Path>,
    diff_dir: Option<&Path>,
) -> FileResult {
    let _ = diff_dir;
    let _ = config.lenient_flate;
    let mut timings = Timings::default();

    let t = Instant::now();
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            return fail(rel, source, 0, 0, timings, format!("读取失败：{e}"));
        }
    };
    let original_len = data.len() as u64;
    timings.read_us = micros(t);

    let pdf = match PdfDocument::load(data.clone()) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                rel,
                source,
                original_len,
                0,
                timings,
                format!("对象层打开失败：{e}"),
            );
        }
    };
    let pdfium = match Document::load_bytes(lib, data.clone(), None) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                rel,
                source,
                original_len,
                0,
                timings,
                format!("PDFium 打开失败：{e}"),
            );
        }
    };

    let page_count = pdfium.page_count();
    let mut geometry = Vec::with_capacity(page_count);
    for i in 0..page_count {
        match pdfium.page_geometry(i) {
            Ok(g) => geometry.push(g),
            Err(e) => {
                return fail(
                    rel,
                    source,
                    original_len,
                    page_count,
                    timings,
                    format!("第 {} 页几何读取失败：{e}", i + 1),
                );
            }
        }
    }

    let t = Instant::now();
    let pairs = pair_pages(&pdf, &geometry);

    let mut stats = Stats {
        pages: page_count,
        ..Stats::default()
    };
    let mut unmatched: BTreeMap<String, usize> = BTreeMap::new();
    let mut mapped: Vec<MappedPage> = Vec::new();

    for pair in &pairs {
        if pair.info.index >= page_count {
            bump(
                &mut unmatched,
                pair.mismatch.as_deref().unwrap_or("页序不一致"),
            );
            continue;
        }
        if !pair.consistent {
            bump(
                &mut unmatched,
                pair.mismatch.as_deref().unwrap_or("页序不一致"),
            );
            continue;
        }
        let pdfium_page = match pdfium.page(pair.info.index) {
            Ok(p) => p,
            Err(e) => {
                bump(&mut unmatched, "页面无法加载");
                tracing::debug!(page = pair.info.index, error = %e, "load page");
                continue;
            }
        };
        let mut map = match map::map_page(&pdf, &pair.info) {
            Ok(m) => m,
            Err(reason) => {
                bump(&mut unmatched, reason.label());
                continue;
            }
        };
        if map.objects.len() != pdfium_page.object_count() {
            map.unmatched.push(Unmatched::ObjectCountMismatch);
            map.unmatched.sort_unstable();
            map.unmatched.dedup();
        }
        if !map.is_matched() {
            for reason in &map.unmatched {
                bump(&mut unmatched, reason.label());
            }
            if map.unmatched.is_empty() {
                bump(&mut unmatched, Unmatched::ObjectCountMismatch.label());
            }
            continue;
        }

        let empty = EditSet::default();
        match plan_page(&map, &empty) {
            Ok(rewrite) => {
                let stream_n = content_nums(&pair.info.contents).len().max(1);
                stats.streams += stream_n;
                if rewrite.data == map.content.data {
                    stats.streams_identical += stream_n;
                }
                stats.ops_total += map
                    .tokens
                    .iter()
                    .filter(|tok| tok.kind == TokenKind::Operator)
                    .count();
                stats.bytes_original += map.content.data.len();
                stats.bytes_saved += rewrite.data.len();
                stats.pages_matched += 1;
                mapped.push(MappedPage {
                    info: pair.info.clone(),
                    map,
                });
            }
            Err(e) => {
                bump(&mut unmatched, e.to_string());
            }
        }
    }
    timings.map_us = micros(t);

    let edits_by_page = if config.modify_ratio > 0.0 {
        plan_random_edits(rel, config, &mapped)
    } else {
        BTreeMap::new()
    };

    let mut replacements: Vec<StreamReplacement> = Vec::new();
    let mut page_streams: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut any_changed = false;
    let mut fallback_pages = 0usize;
    let mut op_mismatches = Vec::new();

    // Unmatched pages among the PDFium page set become fallbacks.
    fallback_pages += page_count.saturating_sub(mapped.len());

    for page in &mapped {
        let mut set = EditSet::default();
        if let Some(edits) = edits_by_page.get(&page.info.index) {
            for (idx, edit) in edits {
                set.edits.insert(*idx, edit.clone());
            }
            stats.ops_changed += edits.len();
        }
        if set.edits.is_empty() && config.modify_ratio <= 0.0 {
            continue;
        }
        match plan_page(&page.map, &set) {
            Ok(rewrite) => {
                if rewrite.changed {
                    any_changed = true;
                    if let Some(first) = content_nums(&page.info.contents).first().copied() {
                        page_streams.insert(page.info.number, vec![first]);
                        replacements.push(StreamReplacement {
                            object_number: first,
                            data: Some(rewrite.data),
                            compress: true,
                        });
                    }
                }
            }
            Err(e) => {
                fallback_pages += 1;
                op_mismatches.push(format!("第 {} 页：{e}", page.info.index + 1));
            }
        }
    }

    let stream_uses = count_stream_uses(&pairs);
    let deshare: BTreeSet<u32> = stream_uses
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(k, _)| *k)
        .collect();
    let deshared_streams = replacements
        .iter()
        .filter(|r| deshare.contains(&r.object_number))
        .count();

    let t = Instant::now();
    let full = save_full_rewrite(&pdf, true);
    let full_rewrite_bytes = full.bytes.len() as u64;

    let (saved, incremental_bytes, saved_bytes) = if any_changed || !replacements.is_empty() {
        let out = save_incremental(&pdf, &page_streams, &replacements, &deshare, true);
        let saved_bytes = out.bytes.len() as u64;
        let incremental = saved_bytes.saturating_sub(original_len);
        (out.bytes, incremental, saved_bytes)
    } else {
        (data.clone(), 0, original_len)
    };
    timings.save_us = micros(t);

    if let Some(dir) = save_dir {
        let dest = dest_path(dir, rel);
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&dest, &saved) {
            return fail(
                rel,
                source,
                original_len,
                page_count,
                timings,
                format!("写出保存结果失败：{e}"),
            );
        }
    }

    let mut page_records: Vec<PageOutcome> = Vec::new();
    let mut control_stats: Option<Stats> = None;

    if config.modify_ratio > 0.0 || config.control {
        let t = Instant::now();
        let expected = match Document::load_bytes(lib, data.clone(), None) {
            Ok(doc) => {
                if let Err(e) = apply_edits_to_doc(&doc, &edits_by_page) {
                    timings.render_us = micros(t);
                    return fail(rel, source, original_len, page_count, timings, e);
                }
                Some(doc)
            }
            Err(e) => {
                return fail(
                    rel,
                    source,
                    original_len,
                    page_count,
                    timings,
                    format!("打开预期文档失败：{e}"),
                );
            }
        };

        if config.modify_ratio > 0.0 {
            match Document::load_bytes(lib, saved.clone(), None) {
                Ok(saved_doc) => {
                    let (records, passed) = compare_docs(
                        expected.as_ref(),
                        Some(&saved_doc),
                        page_count,
                        &mapped,
                        config,
                    );
                    page_records = records;
                    stats.pages_passed = passed;
                }
                Err(e) => {
                    page_records = vec![PageOutcome::Error; page_count];
                    tracing::debug!(error = %e, "reopen saved");
                }
            }
        }
        timings.render_us = micros(t);

        if config.control {
            let t = Instant::now();
            let mut cstats = Stats {
                pages: page_count,
                ..Stats::default()
            };
            match run_control(lib, &data, &edits_by_page, expected.as_ref(), config) {
                Ok(passed) => cstats.pages_passed = passed,
                Err(e) => {
                    timings.control_us = micros(t);
                    return fail(rel, source, original_len, page_count, timings, e);
                }
            }
            timings.control_us = micros(t);
            control_stats = Some(cstats);
        }
    }

    FileResult {
        path: rel.to_owned(),
        source: source.to_owned(),
        bytes: original_len,
        pages: page_count,
        error: None,
        timings,
        output: Some(OutputStats {
            saved_bytes,
            incremental_bytes,
            full_rewrite_bytes,
            stats,
            unmatched,
            op_mismatches,
            verify_failures: Vec::new(),
            fallback_pages,
            deshared_streams,
            bytes_delta: saved_bytes as i64 - original_len as i64,
        }),
        page_records,
        control: control_stats,
    }
}

struct MappedPage {
    info: PageInfo,
    map: PageMap,
}

fn fail(
    path: &str,
    source: &str,
    bytes: u64,
    pages: usize,
    timings: Timings,
    error: impl std::fmt::Display,
) -> FileResult {
    FileResult {
        path: path.to_owned(),
        source: source.to_owned(),
        bytes,
        pages,
        error: Some(error.to_string()),
        timings,
        output: None,
        page_records: Vec::new(),
        control: None,
    }
}

fn micros(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn bump(map: &mut BTreeMap<String, usize>, key: impl Into<String>) {
    *map.entry(key.into()).or_default() += 1;
}

fn content_nums(contents: &Option<Contents>) -> Vec<u32> {
    match contents {
        None => Vec::new(),
        Some(Contents::One(n)) => vec![*n],
        Some(Contents::Many(v)) => v.clone(),
    }
}

fn count_stream_uses(pairs: &[PagePair]) -> BTreeMap<u32, usize> {
    let mut uses = BTreeMap::new();
    for pair in pairs {
        for n in content_nums(&pair.info.contents) {
            *uses.entry(n).or_default() += 1;
        }
    }
    uses
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
        h = h.wrapping_mul(0x1000_0000_01b3);
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

fn plan_random_edits(
    rel: &str,
    config: &Config,
    mapped: &[MappedPage],
) -> BTreeMap<usize, Vec<(usize, EditTarget)>> {
    let mut rng = SplitMix64(config.seed ^ path_hash(rel));
    let mut out = BTreeMap::new();
    for page in mapped {
        let n = page.map.objects.len();
        if n == 0 {
            continue;
        }
        let mut k = ((n as f64) * config.modify_ratio).round() as usize;
        if k == 0 {
            k = 1;
        }
        k = k.min(n);
        let mut indices: Vec<usize> = (0..n).collect();
        for i in (1..indices.len()).rev() {
            let j = (rng.next_u64() as usize) % (i + 1);
            indices.swap(i, j);
        }
        let mut edits = Vec::new();
        for (cycle, idx) in indices.into_iter().take(k).enumerate() {
            let edit = match cycle % 3 {
                0 => EditTarget::Move { dx: 5.0, dy: 5.0 },
                1 => EditTarget::Recolor {
                    r: 0.9,
                    g: 0.1,
                    b: 0.1,
                },
                _ => {
                    if n == 0 {
                        continue;
                    }
                    EditTarget::Delete
                }
            };
            edits.push((idx, edit));
        }
        if !edits.is_empty() {
            out.insert(page.info.index, edits);
        }
    }
    out
}

fn apply_edits_to_doc(
    doc: &Document<'_>,
    edits_by_page: &BTreeMap<usize, Vec<(usize, EditTarget)>>,
) -> Result<(), String> {
    for (page_index, edits) in edits_by_page {
        let page = doc
            .page(*page_index)
            .map_err(|e| format!("打开第 {} 页失败：{e}", page_index + 1))?;
        apply_edits_to_page(&page, edits)?;
        // PDFium drops in-memory object-tree edits on ClosePage unless
        // GenerateContent runs on the same page handle first.
        mark_all_and_regenerate(&page)?;
    }
    Ok(())
}

fn apply_edits_to_page(page: &Page<'_>, edits: &[(usize, EditTarget)]) -> Result<(), String> {
    for (idx, edit) in edits {
        if matches!(edit, EditTarget::Delete) {
            continue;
        }
        let Some(obj) = page.object(*idx) else {
            return Err(format!("对象 {idx} 不存在"));
        };
        match edit {
            EditTarget::Move { dx, dy } => {
                obj.transform(Matrix::translation(*dx as f32, *dy as f32))
                    .map_err(|e| format!("平移对象 {idx} 失败：{e}"))?;
            }
            EditTarget::Recolor { r, g, b } => {
                obj.set_fill_color(Rgba {
                    r: to_u8(*r),
                    g: to_u8(*g),
                    b: to_u8(*b),
                    a: 255,
                })
                .map_err(|e| format!("改色对象 {idx} 失败：{e}"))?;
            }
            EditTarget::Delete | EditTarget::InsertBefore(_) | EditTarget::Scale { .. } => {}
        }
    }
    let mut deletes: Vec<usize> = edits
        .iter()
        .filter(|(_, e)| matches!(e, EditTarget::Delete))
        .map(|(i, _)| *i)
        .collect();
    deletes.sort_unstable();
    deletes.reverse();
    for idx in deletes {
        let Some(obj) = page.object(idx) else {
            return Err(format!("删除对象 {idx} 失败：对象不存在"));
        };
        page.remove_object(obj)
            .map_err(|e| format!("删除对象 {idx} 失败：{e}"))?;
    }
    Ok(())
}

fn to_u8(v: f64) -> u8 {
    let scaled = (v * 255.0).round();
    if scaled <= 0.0 {
        0
    } else if scaled >= 255.0 {
        255
    } else {
        scaled as u8
    }
}

fn mark_all_and_regenerate(page: &Page<'_>) -> Result<(), String> {
    for obj in page.objects() {
        obj.mark_modified()
            .map_err(|e| format!("标记对象失败：{e}"))?;
    }
    page.generate_content()
        .map_err(|e| format!("重新生成内容流失败：{e}"))
}

fn run_control(
    lib: &Library,
    data: &[u8],
    edits_by_page: &BTreeMap<usize, Vec<(usize, EditTarget)>>,
    expected: Option<&Document<'_>>,
    config: &Config,
) -> Result<usize, String> {
    let doc = Document::load_bytes(lib, data.to_vec(), None)
        .map_err(|e| format!("对照组打开失败：{e}"))?;
    apply_edits_to_doc(&doc, edits_by_page)?;
    for i in 0..doc.page_count() {
        let page = doc
            .page(i)
            .map_err(|e| format!("对照组第 {} 页打开失败：{e}", i + 1))?;
        mark_all_and_regenerate(&page)?;
    }
    let saved = doc
        .save_to_vec(SaveFlags::NO_INCREMENTAL)
        .map_err(|e| format!("对照组保存失败：{e}"))?;
    drop(doc);
    let reopened = Document::load_bytes(lib, saved, None)
        .map_err(|e| format!("对照组保存结果无法打开：{e}"))?;
    let page_count = reopened.page_count();
    let (_records, passed) = compare_docs(expected, Some(&reopened), page_count, &[], config);
    Ok(passed)
}

fn compare_docs(
    expected: Option<&Document<'_>>,
    saved: Option<&Document<'_>>,
    page_count: usize,
    mapped: &[MappedPage],
    config: &Config,
) -> (Vec<PageOutcome>, usize) {
    let mapped_idx: BTreeSet<usize> = mapped.iter().map(|p| p.info.index).collect();
    let render = RenderOptions {
        scale: config.scale,
        ..RenderOptions::default()
    };
    let mut records = Vec::with_capacity(page_count);
    let mut passed = 0usize;
    for i in 0..page_count {
        if !mapped.is_empty() && !mapped_idx.contains(&i) {
            records.push(PageOutcome::Fallback);
            continue;
        }
        let Some(exp_doc) = expected else {
            records.push(PageOutcome::Error);
            continue;
        };
        let Some(saved_doc) = saved else {
            records.push(PageOutcome::Error);
            continue;
        };
        let exp_page = match exp_doc.page(i) {
            Ok(p) => p,
            Err(_) => {
                records.push(PageOutcome::Error);
                continue;
            }
        };
        let saved_page = match saved_doc.page(i) {
            Ok(p) => p,
            Err(_) => {
                records.push(PageOutcome::Error);
                continue;
            }
        };
        let pixels = (exp_page.width() * config.scale).round().max(1.0) as usize
            * (exp_page.height() * config.scale).round().max(1.0) as usize;
        if pixels > config.max_pixels {
            records.push(PageOutcome::Error);
            continue;
        }
        let a = match exp_page.render(render) {
            Ok(b) => b,
            Err(_) => {
                records.push(PageOutcome::Error);
                continue;
            }
        };
        let b = match saved_page.render(render) {
            Ok(b) => b,
            Err(_) => {
                records.push(PageOutcome::Error);
                continue;
            }
        };
        if bitmaps_identical(&a, &b) {
            records.push(PageOutcome::Pass);
            passed += 1;
        } else {
            records.push(PageOutcome::Diff);
        }
    }
    (records, passed)
}

fn bitmaps_identical(a: &Bitmap, b: &Bitmap) -> bool {
    if a.width() != b.width() || a.height() != b.height() {
        return false;
    }
    for y in 0..a.height() {
        if a.row(y) != b.row(y) {
            return false;
        }
    }
    true
}
