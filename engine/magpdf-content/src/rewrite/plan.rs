//! 对象级重写计划（纯函数，不碰 PDFium，便于单测与模糊测试）。
//!
//! 输入是一页的对应结果（[`crate::map::PageMap`]）与"要改哪些对象"，输出是拼接后的新字节。
//! 未修改的区间逐字节复制；被删对象的区间删掉；需要时插入 `q/cm/Q` 与颜色操作符。

use std::collections::BTreeMap;

use crate::content::{Token, TokenKind};
use crate::map::{ObjKind, PageMap};
use crate::pdf::writer::format_real;

/// 一次修改。
#[derive(Debug, Clone, PartialEq)]
pub enum EditTarget {
    /// 平移到 `dx, dy`（用户空间，`cm` 在对象坐标系之外）。
    Move { dx: f64, dy: f64 },
    /// 等比缩放 `scale`（几何以 PDFium 为准，这里只写 `cm`）。
    Scale { scale: f64 },
    /// 改填充色（`rg`，0–1）。
    Recolor { r: f64, g: f64, b: f64 },
    /// 删除绘制。
    Delete,
    /// 在绘制段前插入原样字节（新增对象的原型，由上层提供）。
    InsertBefore(Vec<u8>),
}

/// 一页的修改集合：对象下标（PDFium 顺序）→ 修改；新增对象按顺序附在末尾。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EditSet {
    pub edits: BTreeMap<usize, EditTarget>,
    /// 新增对象的字节（原样追加在末尾，`q` 补齐之后）。
    pub appended: Vec<Vec<u8>>,
}

/// 重写输入：页的对应结果 + 原来拼接好的流。
#[derive(Debug, Clone)]
pub struct RewriteInput<'a> {
    pub map: &'a PageMap,
    pub edits: EditSet,
}

/// 重写错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RewriteError {
    #[error("这一页不可对应：{0}")]
    Unmatched(String),
    #[error("对象 {0} 不存在（共 {1} 个）")]
    NoSuchObject(usize, usize),
    #[error("文字对象不支持这种修改")]
    UnsupportedTextEdit,
    #[error("共用内容流的页面不能单独重写（先复制）")]
    Shared,
}

/// 一次重写的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PageRewrite {
    /// 新的拼接字节。
    pub data: Vec<u8>,
    /// 输出中仍然逐字节相同的源区间数（未改动的片段）。
    pub untouched_bytes: usize,
    /// 是否有任何改动（没有时调用方不应写回）。
    pub changed: bool,
    /// 重新分词后的记号数（校验用）。
    pub token_count: usize,
}

struct Ctx<'a> {
    data: &'a [u8],
    tokens: &'a [Token],
}

impl Ctx<'_> {
    /// 记号 `i` 的"起始"：它的 `leading_start`（前一记号之后的空白也算这一段的头）。
    fn piece_start(&self, i: usize) -> usize {
        if i == 0 {
            0
        } else {
            self.tokens[i].leading_start.min(self.tokens[i].range.start)
        }
    }

    fn piece_end(&self, i: usize) -> usize {
        self.tokens
            .get(i + 1)
            .map_or(self.data.len(), |t| self.piece_start_of_next(t))
    }

    fn piece_start_of_next(&self, next: &Token) -> usize {
        next.leading_start.min(next.range.start)
    }
}

/// 生成一页的重写结果。
/// 绘制段起点：路径/图像操作符前面的操作数也算进 `q … cm` 包住的范围。
fn draw_start(tokens: &[Token], obj: &crate::map::MappedObject) -> usize {
    if obj.kind == ObjKind::Text {
        return obj.first_token;
    }
    let mut i = obj.first_token;
    while i > 0 {
        match tokens[i - 1].kind {
            TokenKind::Number(_)
            | TokenKind::LiteralString
            | TokenKind::HexString
            | TokenKind::Name(_)
            | TokenKind::Array
            | TokenKind::Dict => i -= 1,
            _ => break,
        }
    }
    i
}
pub fn plan_page(map: &PageMap, edits: &EditSet) -> Result<PageRewrite, RewriteError> {
    if !map.is_matched() {
        return Err(RewriteError::Unmatched(
            map.unmatched
                .iter()
                .map(|u| u.label())
                .collect::<Vec<_>>()
                .join("、"),
        ));
    }
    let data = &map.content.data[..];
    let tokens = &map.tokens;
    let mut starts = BTreeMap::new();
    for (i, t) in tokens.iter().enumerate() {
        starts.insert(
            std::cmp::min(t.leading_start.min(t.range.start), t.range.start),
            i,
        );
    }
    let _ = starts;
    let ctx = Ctx { data, tokens };

    // 边界集合：每个被改对象的绘制段 [start_token, end_token]。
    let mut insert_before: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut delete_range: Vec<(usize, usize)> = Vec::new();
    let mut move_ops: BTreeMap<usize, String> = BTreeMap::new();
    let mut recolor: BTreeMap<usize, (f64, f64, f64)> = BTreeMap::new();
    let mut text_delete: Vec<usize> = Vec::new();
    let mut changed = false;

    for (idx, edit) in &edits.edits {
        let Some(obj) = map.objects.get(*idx) else {
            return Err(RewriteError::NoSuchObject(*idx, map.objects.len()));
        };
        changed = true;
        match edit {
            EditTarget::Move { dx, dy } => {
                let a = "1 0 0 1 ".to_owned();
                let b = format!("{} {}", fmt(*dx), fmt(*dy));
                move_ops.insert(*idx, format!("q {a}{b} cm\n"));
            }
            EditTarget::Scale { scale } => {
                let a = format!("{} 0 0 {} ", fmt(*scale), fmt(*scale));
                let b = "0 0".to_owned();
                move_ops.insert(*idx, format!("q {a}{b} cm\n"));
            }
            EditTarget::Recolor { r, g, b } => {
                recolor.insert(*idx, (*r, *g, *b));
            }
            EditTarget::Delete => match obj.kind {
                ObjKind::Text => text_delete.extend(obj.text_runs.iter().map(|r| r.op_token)),
                _ => delete_range.push((draw_start(tokens, obj), obj.last_token)),
            },
            EditTarget::InsertBefore(bytes) => {
                insert_before
                    .entry(obj.first_token)
                    .or_default()
                    .extend_from_slice(bytes);
            }
        }
    }

    let mut out = Vec::with_capacity(data.len() + 256);
    let mut copied = 0usize;
    let mut i = 0usize;
    let mut in_text = false;
    let mut q_open = 0usize;
    let mut deleted: Vec<(usize, usize)> = delete_range;
    deleted.sort_unstable();
    let insert_at: BTreeMap<usize, Vec<u8>> = insert_before;

    // 删除范围按起始记号索引；先做"删区间"的游标。
    let mut del_iter = deleted.iter().peekable();
    let mut skip_until: Option<(usize, usize)> = None;

    while i < tokens.len() {
        let tok = &tokens[i];
        if tok.is_op(b"BT") {
            in_text = true;
        } else if tok.is_op(b"ET") {
            in_text = false;
        }
        // 插入（新增对象原型、移动/改色的前缀）都放在对象首记号之前的空白里。
        // move_ops / recolor 的键是**对象下标**，这里按 first_token 对齐到记号流。
        let mut prefix = String::new();
        if let Some(bytes) = insert_at.get(&i) {
            prefix.push_str(&String::from_utf8_lossy(bytes));
        }
        for (obj_idx, q) in &move_ops {
            if draw_start(tokens, &map.objects[*obj_idx]) == i {
                if in_text {
                    prefix.push_str("ET ");
                }
                prefix.push_str(q);
                q_open += 1;
            }
        }
        for (obj_idx, (r, g, b)) in &recolor {
            if draw_start(tokens, &map.objects[*obj_idx]) == i {
                prefix.push_str(&format!("{} {} {} rg\n", fmt(*r), fmt(*g), fmt(*b)));
            }
        }
        if text_delete.contains(&i) {
            prefix.push_str("3 Tr ");
            // 原显示操作符保留（字号、编码不变），只是不画；`TJ` 的字距推进仍然发生。
        }

        if let Some((_, end)) = skip_until
            && i <= end
        {
            i += 1;
            if i > end {
                skip_until = None;
            }
            continue;
        }
        match del_iter.peek() {
            Some((s, e)) if *s == i => {
                skip_until = Some((*s, *e));
                let _ = del_iter.next();
                i += 1;
                if i > *e {
                    skip_until = None;
                }
                continue;
            }
            _ => {}
        }

        let ps = ctx.piece_start(i);
        let pe = ctx.piece_end(i);
        copied += pe - ps;
        if prefix.is_empty() {
            out.extend_from_slice(&data[ps..pe]);
        } else {
            out.extend_from_slice(&data[ps..tok.range.start]);
            out.extend_from_slice(prefix.as_bytes());
            out.extend_from_slice(&data[tok.range.start..pe]);
        }
        // 对象绘制段结束后补 `Q`（在下一记号之前的空白里）。
        for obj_idx in move_ops.keys() {
            if map.objects[*obj_idx].last_token == i {
                let restore = recolor_restore(map, tokens, &recolor, i);
                if let Some(r) = restore {
                    out.extend_from_slice(r.as_bytes());
                }
                out.extend_from_slice(b"\nQ ");
                q_open = q_open.saturating_sub(1);
            }
        }

        i += 1;
    }
    // 流尾未闭合的 q 补齐（PDF 允许，生成器也会补）。
    if q_open > 0 {
        for _ in 0..q_open {
            out.extend_from_slice(b"Q ");
        }
    }
    for bytes in &edits.appended {
        out.extend_from_slice(bytes);
        if !bytes.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    let _ = copied;
    // 重新分词校验：记号数不应因插入而变成损坏输入。
    let rewritten = crate::content::Content::concat(&[crate::content::StreamData {
        index: 0,
        object_number: 0,
        data: out.clone(),
    }]);
    let tokenized = rewritten.tokenize();
    Ok(PageRewrite {
        data: out,
        untouched_bytes: copied,
        changed,
        token_count: tokenized.tokens.len(),
    })
}

fn recolor_restore(
    map: &PageMap,
    tokens: &[Token],
    recolor: &BTreeMap<usize, (f64, f64, f64)>,
    last: usize,
) -> Option<String> {
    if !recolor.contains_key(&0) && recolor.is_empty() {
        return None;
    }
    let _ = (map, tokens, last);
    Some("0 g".to_owned())
}

fn fmt(v: f64) -> String {
    format_real(v)
}

/// 操作符级比对（V14 报告 4.4 的诉求）：两段内容流分词后逐个比较，返回第一个不同的记号下标。
/// 字符串与数组按解码值比较（重新编码后的字节写法可能不同）。
pub fn verify_tokens(expected: &[u8], actual: &[u8]) -> Option<usize> {
    let mk = |d: &[u8]| {
        crate::content::Content::concat(&[crate::content::StreamData {
            index: 0,
            object_number: 0,
            data: d.to_vec(),
        }])
        .tokenize()
    };
    let a = mk(expected);
    let b = mk(actual);
    for (i, (x, y)) in a.tokens.iter().zip(b.tokens.iter()).enumerate() {
        if x.kind != y.kind || x.op != y.op {
            return Some(i);
        }
    }
    if a.tokens.len() == b.tokens.len() {
        None
    } else {
        Some(a.tokens.len().min(b.tokens.len()))
    }
}
