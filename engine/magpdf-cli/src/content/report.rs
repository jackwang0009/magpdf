//! 汇总与表格。合计数全部从页记录算出来，JSON 里每个数都能逐页核对。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::model::{Check, CheckStatus, FileResult, FileStatus, PageRecord, Route, Timings};
use super::run::REASON_REWRITE;

/// 一组像素比对的计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub pass: usize,
    pub diff: usize,
    /// 预期画面就渲染不出来的页，不计入比例。
    pub error: usize,
}

impl Tally {
    fn add(&mut self, check: &Check) {
        match check.status {
            CheckStatus::Pass => self.pass += 1,
            CheckStatus::Diff => self.diff += 1,
            CheckStatus::Error => self.error += 1,
        }
    }

    /// 参加比对的页数。
    pub fn compared(&self) -> usize {
        self.pass + self.diff
    }

    pub fn ratio(&self) -> Option<f64> {
        ratio(self.pass, self.compared())
    }
}

/// 一个来源类别（或全部）的汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceStats {
    pub files: usize,
    /// 文件读不了，或者 PDFium 打不开。这些文件不参加下面的统计。
    pub load_failed: usize,
    pub crashed: usize,
    pub timed_out: usize,
    /// PDFium 能打开、自研对象层打不开的文件（全部页面回退）。
    pub object_layer_failed: usize,
    pub bytes: u64,
    /// PDFium 报告的页数之和。
    pub pages: usize,
    /// PDFium 加载失败的页，不参加下面的统计。
    pub pages_error: usize,
    /// 没有页面对象的页。
    pub pages_empty: usize,
    pub pages_matched: usize,
    /// 回退给生成器的页。
    pub pages_fallback: usize,
    /// 回退的页里有页面对象的（没有对象的页谈不上重写）。
    pub pages_fallback_with_objects: usize,
    /// 回退的页里，对应本身没问题、只是改写层写不了这次修改的。
    pub pages_rewrite_unsupported: usize,
    /// 回退原因 → 页数（一页可以有不止一个原因）。
    pub fallback_reasons: BTreeMap<String, usize>,
    pub streams: usize,
    pub streams_identical: usize,
    pub streams_unchecked: usize,
    pub operators: usize,
    pub objects: usize,
    pub objects_edited: usize,
    pub pages_edited: usize,
    /// 修改种类 → 次数。
    pub edits: BTreeMap<String, usize>,
    /// 全部页的像素比对。
    pub verify: Tally,
    /// 其中由改写层写的页。
    pub verify_matched: Tally,
    /// 其中回退给生成器的页。
    pub verify_fallback: Tally,
    /// 其中做了修改的页。
    pub verify_edited: Tally,
    /// 对照组（整页都走 PDFium 的生成器），没跑时为 `None`。
    pub control: Option<Tally>,
    pub control_edited: Option<Tally>,
    /// 保存后核对不过的条数（读回的内容流不对，或者像素有差异）。
    pub verify_failures: usize,
    pub deshared_streams: usize,
    pub saved_bytes: u64,
    pub incremental_bytes: u64,
    pub fallback_bytes: u64,
    pub full_rewrite_bytes: u64,
    pub timings: Timings,
}

impl SourceStats {
    fn add_file(&mut self, file: &FileResult) {
        self.files += 1;
        self.timings.add(&file.timings);
        match file.status {
            FileStatus::Ok => {}
            FileStatus::LoadFailed => self.load_failed += 1,
            FileStatus::Crashed => self.crashed += 1,
            FileStatus::TimedOut => self.timed_out += 1,
        }
        if file.status != FileStatus::Ok {
            return;
        }
        self.bytes += file.bytes;
        self.pages += file.pages;
        if file.object_layer_error.is_some() {
            self.object_layer_failed += 1;
        }
        if let Some(out) = &file.output {
            self.saved_bytes += out.saved_bytes;
            self.incremental_bytes += out.incremental_bytes;
            self.fallback_bytes += out.fallback_bytes;
            self.full_rewrite_bytes += out.full_rewrite_bytes;
            self.deshared_streams += out.deshared_streams;
            self.verify_failures += out.verify_failures.len();
        }
        for page in &file.page_records {
            self.add_page(page);
        }
    }

    fn add_page(&mut self, page: &PageRecord) {
        if page.error.is_some() {
            self.pages_error += 1;
            return;
        }
        if page.objects == 0 {
            self.pages_empty += 1;
        }
        match page.route {
            Route::Matched => self.pages_matched += 1,
            Route::Fallback => {
                self.pages_fallback += 1;
                if page.objects > 0 {
                    self.pages_fallback_with_objects += 1;
                }
                if page.reasons.iter().all(|r| r == REASON_REWRITE) {
                    self.pages_rewrite_unsupported += 1;
                }
                for reason in &page.reasons {
                    *self.fallback_reasons.entry(reason.clone()).or_default() += 1;
                }
            }
        }
        self.streams += page.streams;
        self.streams_identical += page.streams_identical;
        self.streams_unchecked += page.streams_unchecked;
        self.operators += page.operators;
        self.objects += page.objects;
        self.objects_edited += page.edits.len();
        if !page.edits.is_empty() {
            self.pages_edited += 1;
        }
        for edit in &page.edits {
            *self.edits.entry(edit.kind.label().to_owned()).or_default() += 1;
        }
        if let Some(check) = &page.verify {
            self.verify.add(check);
            match page.route {
                Route::Matched => self.verify_matched.add(check),
                Route::Fallback => self.verify_fallback.add(check),
            }
            if !page.edits.is_empty() {
                self.verify_edited.add(check);
            }
        }
        if let Some(check) = &page.control {
            self.control.get_or_insert_with(Tally::default).add(check);
            if !page.edits.is_empty() {
                self.control_edited
                    .get_or_insert_with(Tally::default)
                    .add(check);
            }
        }
    }

    /// 参加统计的页数（PDFium 能加载的页）。
    pub fn pages_counted(&self) -> usize {
        self.pages.saturating_sub(self.pages_error)
    }

    /// 对不上的页：回退的页里去掉“对应没问题、只是这次修改写不了”的。
    pub fn pages_unmatched(&self) -> usize {
        self.pages_fallback
            .saturating_sub(self.pages_rewrite_unsupported)
    }
}

/// 按来源汇总。
pub fn summarize(files: &[FileResult]) -> BTreeMap<String, SourceStats> {
    let mut out: BTreeMap<String, SourceStats> = BTreeMap::new();
    for f in files {
        out.entry(f.source.clone()).or_default().add_file(f);
    }
    out
}

/// 合计（只作参考）。
pub fn total(files: &[FileResult]) -> SourceStats {
    let mut s = SourceStats::default();
    for f in files {
        s.add_file(f);
    }
    s
}

fn pct(v: Option<f64>) -> String {
    v.map_or_else(|| "—".to_owned(), |r| format!("{:.2}%", r * 100.0))
}

fn ratio(a: usize, b: usize) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

fn tally(t: &Tally) -> String {
    format!("{} / {}（{}）", t.pass, t.compared(), pct(t.ratio()))
}

fn opt_tally(t: Option<&Tally>) -> String {
    t.map_or_else(|| "—".to_owned(), tally)
}

fn rows(
    out: &mut String,
    by_source: &BTreeMap<String, SourceStats>,
    total: &SourceStats,
    row: impl Fn(&str, &SourceStats) -> String,
) {
    for (name, s) in by_source {
        let _ = writeln!(out, "{}", row(name, s));
    }
    if by_source.len() > 1 {
        let _ = writeln!(out, "{}", row("（合计，仅供参考）", total));
    }
}

/// 表一：对应、回退、不改动时内容流是否逐字节相同。
pub fn render_table(by_source: &BTreeMap<String, SourceStats>, total: &SourceStats) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| 来源 | 文件 | 打不开 | 崩溃/超时 | 对象层打不开 | 页 | 页错误 | 无对象页 | 对得上 | 回退 | 对不上的页比例 | 其中只是修改写不了 | 内容流 | 逐字节相同 | 未核对 | 相同比例 |"
    );
    let _ = writeln!(
        out,
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    rows(&mut out, by_source, total, |name, s| {
        format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            s.files,
            s.load_failed,
            s.crashed + s.timed_out,
            s.object_layer_failed,
            s.pages,
            s.pages_error,
            s.pages_empty,
            s.pages_matched,
            s.pages_fallback,
            pct(ratio(s.pages_unmatched(), s.pages_counted())),
            s.pages_rewrite_unsupported,
            s.streams,
            s.streams_identical,
            s.streams_unchecked,
            pct(ratio(s.streams_identical, s.streams)),
        )
    });
    out
}

/// 表二：像素比对；表三：字节与耗时；最后列出回退原因。
pub fn render_details(by_source: &BTreeMap<String, SourceStats>, total: &SourceStats) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| 来源 | 修改的对象 / 页 | 全部页无差异 | 改写层写的页 | 回退的页 | 做了修改的页 | 对照组全部页 | 对照组做了修改的页 | 核对不过（条） |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|");
    rows(&mut out, by_source, total, |name, s| {
        format!(
            "| {name} | {} / {} | {} | {} | {} | {} | {} | {} | {} |",
            s.objects_edited,
            s.pages_edited,
            tally(&s.verify),
            tally(&s.verify_matched),
            tally(&s.verify_fallback),
            tally(&s.verify_edited),
            opt_tally(s.control.as_ref()),
            opt_tally(s.control_edited.as_ref()),
            s.verify_failures,
        )
    });

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| 来源 | 原文件字节 | 保存字节 | 改写层追加 | 生成器追加 | 完整重写 | 读入 ms | 对应 ms | 保存 ms | 回退 ms | 读回 ms | 渲染比对 ms | 对照 ms |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    rows(&mut out, by_source, total, |name, s| {
        let ms = |us: u64| us as f64 / 1000.0;
        format!(
            "| {name} | {} | {} | {} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
            s.bytes,
            s.saved_bytes,
            s.incremental_bytes,
            s.fallback_bytes,
            s.full_rewrite_bytes,
            ms(s.timings.read_us),
            ms(s.timings.map_us),
            ms(s.timings.save_us),
            ms(s.timings.fallback_us),
            ms(s.timings.verify_us),
            ms(s.timings.render_us),
            ms(s.timings.control_us),
        )
    });

    if !total.fallback_reasons.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "回退原因（页数，一页可以有不止一个原因）：");
        let mut reasons: Vec<(&String, &usize)> = total.fallback_reasons.iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (reason, n) in reasons {
            let _ = writeln!(out, "- {reason}：{n}");
        }
    }
    if !total.edits.is_empty() {
        let _ = writeln!(out);
        let kinds = total
            .edits
            .iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect::<Vec<_>>()
            .join("，");
        let _ = writeln!(out, "修改种类：{kinds}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::model::{EditKind, EditRecord};

    fn check(status: CheckStatus) -> Check {
        Check {
            status,
            ..Check::error("")
        }
    }

    fn page(route: Route, objects: usize, verify: CheckStatus) -> PageRecord {
        PageRecord {
            route,
            objects,
            streams: 1,
            streams_identical: usize::from(route == Route::Matched),
            verify: Some(check(verify)),
            ..PageRecord::new(0)
        }
    }

    fn file(source: &str, pages: Vec<PageRecord>) -> FileResult {
        FileResult {
            path: "x.pdf".to_owned(),
            source: source.to_owned(),
            bytes: 100,
            pages: pages.len(),
            status: FileStatus::Ok,
            error: None,
            object_layer_error: None,
            timings: Timings::default(),
            output: None,
            page_records: pages,
        }
    }

    #[test]
    fn totals_are_computed_from_page_records() {
        let mut edited = page(Route::Matched, 4, CheckStatus::Diff);
        edited.edits.push(EditRecord {
            object: 1,
            object_kind: "路径".to_owned(),
            kind: EditKind::Move,
        });
        edited.control = Some(check(CheckStatus::Pass));
        let mut unmatched = page(Route::Fallback, 2, CheckStatus::Pass);
        unmatched.reasons.push("对象数不一致".to_owned());
        let mut unsupported = page(Route::Fallback, 3, CheckStatus::Diff);
        unsupported.reasons.push(REASON_REWRITE.to_owned());
        let mut broken = page(Route::Fallback, 0, CheckStatus::Error);
        broken.error = Some("PDFium 加载页面失败".to_owned());
        let files = vec![
            file(
                "synthetic",
                vec![
                    page(Route::Matched, 5, CheckStatus::Pass),
                    edited,
                    unmatched,
                    unsupported,
                    page(Route::Matched, 0, CheckStatus::Pass),
                    broken,
                ],
            ),
            FileResult::failed(
                "bad.pdf",
                "synthetic",
                FileStatus::LoadFailed,
                "PDFium 打不开",
            ),
            FileResult::failed("boom.pdf", "public", FileStatus::Crashed, "子进程异常退出"),
        ];
        let by_source = summarize(&files);
        let s = &by_source["synthetic"];
        assert_eq!(
            (s.files, s.load_failed, s.pages, s.pages_error),
            (2, 1, 6, 1)
        );
        assert_eq!(s.pages_counted(), 5);
        assert_eq!((s.pages_matched, s.pages_fallback), (3, 2));
        assert_eq!(s.pages_rewrite_unsupported, 1);
        assert_eq!(s.pages_unmatched(), 1, "只是修改写不了的页不算对不上");
        assert_eq!(s.pages_empty, 1);
        assert_eq!(s.fallback_reasons.get("对象数不一致"), Some(&1));
        assert_eq!((s.streams, s.streams_identical), (5, 3));
        assert_eq!((s.verify.pass, s.verify.diff, s.verify.error), (3, 2, 0));
        assert_eq!((s.verify_matched.pass, s.verify_matched.diff), (2, 1));
        assert_eq!((s.verify_fallback.pass, s.verify_fallback.diff), (1, 1));
        assert_eq!((s.verify_edited.pass, s.verify_edited.diff), (0, 1));
        assert_eq!(s.control.map(|t| t.pass), Some(1));
        assert_eq!((s.objects_edited, s.pages_edited), (1, 1));
        assert_eq!(s.edits.get("移动"), Some(&1));

        let all = total(&files);
        assert_eq!((all.files, all.crashed, all.load_failed), (3, 1, 1));
        let table = render_table(&by_source, &all);
        assert!(table.contains("| synthetic | 2 | 1 |"), "{table}");
        assert!(table.contains("20.00%"), "对不上 1 / 5：{table}");
        assert!(table.contains("合计"), "{table}");
        let details = render_details(&by_source, &all);
        assert!(details.contains("3 / 5（60.00%）"), "{details}");
        assert!(details.contains("- 对象数不一致：1"), "{details}");
    }

    #[test]
    fn stats_survive_json_roundtrip() {
        let files = vec![file(
            "synthetic",
            vec![page(Route::Matched, 1, CheckStatus::Pass)],
        )];
        let stats = total(&files);
        let json = serde_json::to_string(&stats).expect("serialize");
        let back: SourceStats = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, stats);
        let json = serde_json::to_string(&files).expect("serialize files");
        assert!(json.contains("\"route\":\"matched\""), "{json}");
        let back: Vec<FileResult> = serde_json::from_str(&json).expect("deserialize files");
        assert_eq!(back, files);
    }
}
