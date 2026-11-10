//! 对象收集状态机：逐记号推进，按 PDFium 的规则建对象。
//!
//! 规则出处（`cpdf_streamcontentparser.cpp`，注释里的函数名均为该文件内的方法）：
//! - `m l c v y re h`：路径构造。PDFium 在 `HandleMoveTo`/`HandleLineTo` 等里维护 `m_PathStart`，
//!   真正的 `CPDF_PathObject` 在绘制操作符（`HandleCloseFillStrokePath` 等）才 `AddPathObject`；
//!   `m_PathStart == 0`（没有 `m`/`re`）时 `AddPathObject` 直接返回空，不建对象。
//! - `S s f F f* B B* b b*`：绘制并结束当前路径；`n`：不绘制，有 `W` 时记录剪切。
//! - `W`/`W*`：标记"紧接着的绘制/`n` 要设剪切"。
//! - `BT` 开文字块，`Tj TJ ' "` 各是一次 `AddTextObject`；同一个 `BT … ET` 内的多次显示在 PDFium 里
//!   共享一个内部文字对象（`m_TextObjects`），页面上仍是**每次显示一个对象**。
//! - `Tr` 4–7：字形进剪切路径，`AddTextObject` 之后 `m_TextClip` 让这次显示**不产生页面对象**。
//! - `Tj`/`'`/`"` 空串、`TJ` 里没有非空字符串：不产生对象（字形数为 0）。
//! - `Do`：图像/表单各一个对象；`sh`：渐变一个对象。
//!
//! 这一层不知道资源字典和目标类型（图像还是表单），`Do` 先记为表单，由调用方按资源类型改写。

use crate::content::{Token, TokenKind};
use crate::pdf::PdfObject;

use super::ops::{PaintKind, PdfiumOp};
use super::{MappedObject, ObjKind, TextRun, Unmatched};

#[derive(Debug, Clone, Default)]
struct PathDraft {
    first: usize,
    last: usize,
}

/// 收集状态。
#[derive(Debug, Default)]
pub(super) struct State {
    path: Option<PathDraft>,
    bt: Option<usize>,
    bt_obj: Option<usize>,
    objects: Vec<MappedObject>,
    unmatched: Vec<Unmatched>,
    diagnostics: Vec<String>,
    empty_shows: usize,
    clip_only: usize,
    pending_clip: bool,
    tr: u8,
    q_depth: u32,
    clip_depth: u32,
    clip_stack: Vec<u32>,
    last_operand: Option<PdfObject>,
}

/// 一次显示的字形数：`Tj`/`'`/`"` 有字符串即 1；`TJ` 数非空字符串元素。
fn glyph_count(tok: &Token) -> usize {
    match &tok.kind {
        TokenKind::LiteralString | TokenKind::HexString => 1,
        TokenKind::Array => tok
            .array
            .as_ref()
            .map(|a| {
                a.iter()
                    .filter(|o| matches!(o, PdfObject::Str(s) if !s.is_empty()))
                    .count()
            })
            .unwrap_or(0),
        _ => 0,
    }
}

/// 推进一个记号。
pub(super) fn feed(st: &mut State, tokens: &[Token], index: usize, tok: &Token) {
    match &tok.kind {
        TokenKind::Operator => {
            let class = PdfiumOp::classify(&tok.op);
            on_operator(st, tokens, index, &tok.op, class);
            st.last_operand = None;
        }
        TokenKind::InlineImage(_) => {
            st.objects.push(MappedObject::new(ObjKind::Image, index));
            st.last_operand = None;
        }
        TokenKind::Name(n) => st.last_operand = Some(PdfObject::Name(n.clone())),
        TokenKind::Number(v) => st.last_operand = Some(PdfObject::Real(*v)),
        _ => st.last_operand = None,
    }
}

fn on_operator(st: &mut State, tokens: &[Token], index: usize, op: &[u8], class: PdfiumOp) {
    match class {
        PdfiumOp::PathConstruct => match st.path {
            Some(ref mut p) => p.last = index,
            None => {
                st.path = Some(PathDraft {
                    first: index,
                    last: index,
                });
            }
        },
        PdfiumOp::PathPaint(paint) => {
            let Some(draft) = st.path.take() else {
                // `m_PathStart == 0`：PDFium 不建对象。
                return;
            };
            let mut obj = MappedObject::new(ObjKind::Path, draft.first);
            obj.last_token = index;
            obj.paint_token = Some(index);
            obj.fills = matches!(
                paint,
                PaintKind::Fill | PaintKind::FillStroke | PaintKind::CloseFillStroke
            );
            obj.starts_clipped = st.pending_clip || st.clip_depth > 0;
            st.pending_clip = false;
            st.objects.push(obj);
        }
        PdfiumOp::PathEnd => {
            let draft = st.path.take();
            let clipped = st.pending_clip;
            st.pending_clip = false;
            if clipped {
                st.clip_only += 1;
                push_clip(st);
            } else if draft.is_none() {
                // 没有路径的 `n`：什么也不做。
            }
        }
        PdfiumOp::Clip => st.pending_clip = true,
        PdfiumOp::TextBegin => {
            st.bt = Some(index);
            st.bt_obj = None;
            // `BT` 不重置 `Tr`（PDFium 的文字状态在 `BT` 里保留，见 `BeginText`）。
        }
        PdfiumOp::TextEnd => {
            if let Some(oi) = st.bt_obj {
                st.objects[oi].last_token = index;
            }
            st.bt = None;
            st.bt_obj = None;
        }
        PdfiumOp::TextState => {
            if op == b"Tr" {
                let v = index
                    .checked_sub(1)
                    .and_then(|i| tokens.get(i))
                    .and_then(Token::as_number)
                    .unwrap_or(0.0);
                st.tr = u8::try_from(v as i64).unwrap_or(0).min(7);
            }
        }
        PdfiumOp::TextShow => {
            let prev = index.checked_sub(1).map(|i| &tokens[i]);
            let count = prev.map_or(0, glyph_count);
            if st.bt.is_none() {
                if count > 0 {
                    st.diagnostics
                        .push(format!("记号 {index}：BT 外的文字显示，PDFium 不建对象"));
                    st.unmatched.push(Unmatched::Nesting);
                }
                return;
            }
            if count == 0 {
                st.empty_shows += 1;
                return;
            }
            if matches!(st.tr, 4..=7) {
                // 文字作为剪切：不建页面对象。
                st.clip_only += 1;
                return;
            }
            match st.bt_obj {
                Some(oi) => {
                    st.objects[oi].text_runs.push(TextRun { op_token: index });
                    st.objects[oi].last_token = index;
                }
                None => {
                    let bt = st.bt.unwrap_or(index);
                    let mut obj = MappedObject::new(ObjKind::Text, bt);
                    obj.text_runs.push(TextRun { op_token: index });
                    obj.last_token = index;
                    st.objects.push(obj);
                    st.bt_obj = Some(st.objects.len() - 1);
                }
            }
        }
        PdfiumOp::XObject => {
            let mut obj = MappedObject::new(ObjKind::Form, index);
            obj.resource = st
                .last_operand
                .as_ref()
                .and_then(|o| o.as_name().map(<[u8]>::to_vec));
            st.objects.push(obj);
        }
        PdfiumOp::Shading => {
            let mut obj = MappedObject::new(ObjKind::Shading, index);
            obj.resource = st
                .last_operand
                .as_ref()
                .and_then(|o| o.as_name().map(<[u8]>::to_vec));
            st.objects.push(obj);
        }
        PdfiumOp::GState => match op {
            b"q" => {
                st.q_depth += 1;
                st.clip_stack.push(st.clip_depth);
            }
            b"Q" => {
                if st.q_depth == 0 {
                    st.unmatched.push(Unmatched::Nesting);
                }
                st.q_depth = st.q_depth.saturating_sub(1);
                if let Some(d) = st.clip_stack.pop() {
                    st.clip_depth = d;
                }
            }
            _ => {}
        },
        PdfiumOp::Marked | PdfiumOp::TextPos | PdfiumOp::InlineImage | PdfiumOp::Other => {}
    }
}

fn push_clip(st: &mut State) {
    st.clip_depth += 1;
    match st.clip_stack.last_mut() {
        Some(last) => *last = st.clip_depth,
        None => st.clip_stack.push(st.clip_depth),
    }
}

/// 收尾用的上下文。
pub(super) struct FinishCtx<'a> {
    pub tokens: &'a [Token],
}

/// 收尾结果。
pub(super) struct Finished {
    pub objects: Vec<MappedObject>,
    pub unmatched: Vec<Unmatched>,
    pub diagnostics: Vec<String>,
    pub empty_shows: usize,
    pub clip_only: usize,
}

pub(super) fn finish(mut st: State, ctx: &FinishCtx<'_>) -> Finished {
    if st.path.is_some() {
        st.diagnostics
            .push("路径构造后没有绘制操作符，路径被丢弃".to_owned());
    }
    if st.q_depth > 0 {
        // 未闭合的 q 不一定是错误（PDF 允许到流尾），但写回时要补 Q；不算不可对应。
        st.diagnostics.push(format!("未闭合的 q {} 个", st.q_depth));
    }
    if st.bt.is_some() {
        if let Some(oi) = st.bt_obj {
            st.objects[oi].last_token = ctx.tokens.len().saturating_sub(1);
        } else {
            st.unmatched.push(Unmatched::Nesting);
        }
    }
    Finished {
        objects: st.objects,
        unmatched: st.unmatched,
        diagnostics: st.diagnostics,
        empty_shows: st.empty_shows,
        clip_only: st.clip_only,
    }
}
