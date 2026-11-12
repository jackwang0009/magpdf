//! 对象级重写计划（纯函数，不碰 PDFium，便于单测）。
//!
//! 输入是一页的对应结果（[`crate::map::PageMap`]）与“要改哪些对象”，输出是每一段内容流的新字节。
//! 做法是在原字节上**打补丁**：每个修改变成几处“在某个位置插入、把某一段换掉”，没有被补丁碰到的字节
//! 原样复制。所以没有修改时输出就是输入，逐字节相同，不依赖任何“重新生成”的正确性。
//!
//! 每种修改都按 PDFium 对内存里的对象做同一修改之后的画面来设计（`FPDFPageObj_Transform`、
//! `FPDFPageObj_SetFillColor`、`FPDFPage_RemoveObject`），写不出等价结果的情况明确拒绝
//! （[`RewriteError::Unsupported`]），由调用方把这一页交给 PDFium 的生成器：
//!
//! - 移动路径、图像、表单：`q 1 0 0 1 mx my cm … Q` 包住对象自己的字节，位移换算到当时的坐标系里；
//!   已经建立的剪切不受 `cm` 影响，留在原地（PDFium 移动这几类对象时也不动剪切）。路径自己又挂着
//!   `W` 时，先画一份移动过的副本（不带 `W`），原来的路径改成 `n`，只留下剪切。
//! - 移动渐变：PDFium 连它的剪切一起移动，所以在它所在的 `q … Q` 组开头插一个 `cm`；组里有别的
//!   对象、或者剪切是组外面建立的，就做不到。
//! - 移动文字：`vx vy Td` 挪过去、显示、`-vx -vy Td` 挪回来。`Td` 会把笔放回行首，所以只有本来就在
//!   行首的显示能这样做；后面还有文字接着它的笔位置写时，挪回来之后再不可见地显示一遍，把笔推到
//!   原来该到的地方。不在行首的显示要知道前面文字的宽度才挪得了，这一层没有字体度量，拒绝。
//! - 改色：前面插 `r g b rg`，后面把原来的填充色重放一遍设回去。
//! - 删除：整段去掉；路径挂着 `W` 或者夹着别的操作符时把绘制操作符换成 `n`；后面有文字接着写的
//!   显示改成不可见（`3 Tr`）而不是去掉。

use std::collections::BTreeMap;
use std::ops::Range;

use crate::map::{Detail, MappedObject, ObjKind, PageMap, ShowKind, TextDetail};
use crate::pdf::writer::format_real;

/// 一次修改。
#[derive(Debug, Clone, PartialEq)]
pub enum EditTarget {
    /// 平移 `(dx, dy)`（页面空间）。
    Move { dx: f64, dy: f64 },
    /// 改填充色（DeviceRGB，0–1）。
    Recolor { r: f64, g: f64, b: f64 },
    /// 删除。
    Delete,
}

/// 一页的修改集合：对象下标（PDFium 顺序）→ 修改。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EditSet {
    pub edits: BTreeMap<usize, EditTarget>,
}

/// 重写错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RewriteError {
    #[error("这一页对不上：{0}")]
    Unmatched(String),
    #[error("对象 {0} 不存在（共 {1} 个）")]
    NoSuchObject(usize, usize),
    #[error("对象 {object}（{kind}）：{reason}")]
    Unsupported {
        object: usize,
        kind: &'static str,
        reason: &'static str,
    },
}

/// 一次重写的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PageRewrite {
    /// 每一段（`/Contents` 的每一项）的新字节；`None` 表示这一段没有变。
    pub segments: Vec<Option<Vec<u8>>>,
    /// 新的拼接字节（PDFium 重新打开时读到的那一串）。
    pub data: Vec<u8>,
    /// 是否有任何一段变了。
    pub changed: bool,
}

/// 一处补丁：把 `[at, at + remove)` 换成 `insert`。`remove` 为 0 时是纯插入。
#[derive(Debug)]
struct Patch {
    at: usize,
    remove: usize,
    insert: Vec<u8>,
}

struct Planner<'a> {
    data: &'a [u8],
    patches: Vec<Patch>,
}

impl Planner<'_> {
    fn insert(&mut self, at: usize, text: impl AsRef<[u8]>) {
        self.patches.push(Patch {
            at,
            remove: 0,
            insert: text.as_ref().to_vec(),
        });
    }

    fn replace(&mut self, range: &Range<usize>, text: impl AsRef<[u8]>) {
        self.patches.push(Patch {
            at: range.start,
            remove: range.len(),
            insert: text.as_ref().to_vec(),
        });
    }

    fn bytes(&self, range: &Range<usize>) -> &[u8] {
        self.data.get(range.clone()).unwrap_or_default()
    }
}

fn num(v: f64) -> String {
    format_real(v)
}

/// `q 1 0 0 1 mx my cm`。
fn translate_open(mx: f64, my: f64) -> String {
    format!(" q 1 0 0 1 {} {} cm ", num(mx), num(my))
}

fn unsupported(object: usize, obj: &MappedObject, reason: &'static str) -> RewriteError {
    RewriteError::Unsupported {
        object,
        kind: obj.kind.label(),
        reason,
    }
}

/// 生成一页的重写结果。
pub fn plan_page(map: &PageMap, edits: &EditSet) -> Result<PageRewrite, RewriteError> {
    if let Some(blocked) = &map.blocked {
        return Err(RewriteError::Unmatched(blocked.label()));
    }
    let mut planner = Planner {
        data: &map.content.data,
        patches: Vec::new(),
    };
    for (index, edit) in &edits.edits {
        let Some(obj) = map.objects.get(*index) else {
            return Err(RewriteError::NoSuchObject(*index, map.objects.len()));
        };
        match edit {
            EditTarget::Move { dx, dy } => plan_move(&mut planner, *index, obj, *dx, *dy)?,
            EditTarget::Recolor { r, g, b } => {
                plan_recolor(&mut planner, *index, obj, [*r, *g, *b])?
            }
            EditTarget::Delete => plan_delete(&mut planner, *index, obj)?,
        }
    }
    Ok(apply(map, planner.patches))
}

fn plan_recolor(
    p: &mut Planner<'_>,
    index: usize,
    obj: &MappedObject,
    rgb: [f64; 3],
) -> Result<(), RewriteError> {
    if !matches!(obj.kind, ObjKind::Path | ObjKind::Text) {
        return Err(unsupported(index, obj, "只有路径和文字能改填充色"));
    }
    if obj.fill.overflow {
        return Err(unsupported(
            index,
            obj,
            "原来的填充色来历太长，改完设不回去",
        ));
    }
    let set = format!(" {} {} {} rg ", num(rgb[0]), num(rgb[1]), num(rgb[2]));
    // 设色操作符要紧挨着绘制/显示操作符的操作数之前：路径的构造和绘制之间可能夹着别的设色。
    p.insert(obj.op_operands, set);
    let mut restore = Vec::new();
    restore.push(b' ');
    if obj.fill.from_default {
        restore.extend_from_slice(b"0 g ");
    }
    for op in &obj.fill.ops {
        restore.extend_from_slice(p.bytes(op));
        restore.push(b' ');
    }
    p.insert(obj.op.end, restore);
    Ok(())
}

fn plan_delete(p: &mut Planner<'_>, index: usize, obj: &MappedObject) -> Result<(), RewriteError> {
    match &obj.detail {
        Detail::Path(path) => {
            if path.simple && path.clip.is_none() {
                p.replace(&obj.span, " ");
            } else {
                // 留下路径构造（和挂着的 `W`），只是不画。
                p.replace(&obj.op, " n ");
            }
        }
        Detail::Image(_) | Detail::Shading(_) | Detail::Form => p.replace(&obj.span, " "),
        Detail::Text(text) => {
            if !text.type3 && text.render_mode >= 4 {
                return Err(unsupported(
                    index,
                    obj,
                    "剪切模式的文字，删掉会改变后面对象的剪切",
                ));
            }
            if text.continued {
                hide_in_place(p, index, obj, text)?;
            } else {
                match text.show {
                    ShowKind::Show | ShowKind::ShowArray => p.replace(&obj.span, " "),
                    ShowKind::NextLineShow => p.replace(&obj.span, " T* "),
                    ShowKind::NextLineShowSpaced => {
                        let [aw, ac, _] = text.operands.as_slice() else {
                            // 操作数写法不规整，拆不出它设的间距：整个留下，只是不可见。
                            return hide_in_place(p, index, obj, text);
                        };
                        let mut out = b" ".to_vec();
                        out.extend_from_slice(p.bytes(aw));
                        out.extend_from_slice(b" Tw ");
                        out.extend_from_slice(p.bytes(ac));
                        out.extend_from_slice(b" Tc T* ");
                        p.replace(&obj.span, out);
                    }
                }
            }
        }
    }
    Ok(())
}

/// 后面有文字接着写：显示改成不可见，笔照样往前走。
fn hide_in_place(
    p: &mut Planner<'_>,
    index: usize,
    obj: &MappedObject,
    text: &TextDetail,
) -> Result<(), RewriteError> {
    if text.type3 {
        return Err(unsupported(
            index,
            obj,
            "Type3 字体不看渲染模式，后面又有文字接着写，藏不起来",
        ));
    }
    p.insert(obj.span.start, " 3 Tr ");
    p.insert(obj.op.end, format!(" {} Tr ", text.render_mode));
    Ok(())
}

fn plan_move(
    p: &mut Planner<'_>,
    index: usize,
    obj: &MappedObject,
    dx: f64,
    dy: f64,
) -> Result<(), RewriteError> {
    const SINGULAR: &str = "当时的变换矩阵不可逆，位移换算不过去";
    match &obj.detail {
        Detail::Path(path) => {
            if !path.simple {
                return Err(unsupported(
                    index,
                    obj,
                    "路径的构造和绘制之间夹着别的操作符，不能整体搬动",
                ));
            }
            let (mx, my) = obj
                .ctm
                .pull_back(dx, dy)
                .ok_or_else(|| unsupported(index, obj, SINGULAR))?;
            match &path.clip {
                None => {
                    p.insert(obj.span.start, translate_open(mx, my));
                    p.insert(obj.span.end, " Q ");
                }
                Some(clip) => {
                    // 先画一份移动过的副本（不带 `W`），原来的只留下剪切。
                    let mut copy = translate_open(mx, my).into_bytes();
                    copy.extend_from_slice(p.bytes(&(obj.span.start..clip.start)));
                    copy.extend_from_slice(p.bytes(&(clip.end..obj.span.end)));
                    copy.extend_from_slice(b" Q ");
                    p.insert(obj.span.start, copy);
                    p.replace(&obj.op, " n ");
                }
            }
        }
        Detail::Image(_) | Detail::Form => {
            let (mx, my) = obj
                .ctm
                .pull_back(dx, dy)
                .ok_or_else(|| unsupported(index, obj, SINGULAR))?;
            p.insert(obj.span.start, translate_open(mx, my));
            p.insert(obj.span.end, " Q ");
        }
        Detail::Shading(group) => {
            let Some(group) = group else {
                return Err(unsupported(
                    index,
                    obj,
                    "渐变要连剪切一起移动，而它不是独占一个自带剪切的 q … Q 组",
                ));
            };
            let (mx, my) = group
                .ctm
                .pull_back(dx, dy)
                .ok_or_else(|| unsupported(index, obj, SINGULAR))?;
            p.insert(
                group.after_q,
                format!(" 1 0 0 1 {} {} cm ", num(mx), num(my)),
            );
        }
        Detail::Text(text) => plan_move_text(p, index, obj, text, dx, dy)?,
    }
    Ok(())
}

fn plan_move_text(
    p: &mut Planner<'_>,
    index: usize,
    obj: &MappedObject,
    text: &TextDetail,
    dx: f64,
    dy: f64,
) -> Result<(), RewriteError> {
    if !text.type3 && text.render_mode >= 4 {
        return Err(unsupported(
            index,
            obj,
            "剪切模式的文字，移动会改变后面对象的剪切",
        ));
    }
    if !text.in_text_block {
        return Err(unsupported(index, obj, "文字不在 BT … ET 之内"));
    }
    if !text.at_line_start {
        return Err(unsupported(
            index,
            obj,
            "文字不在行首，移动它要知道前面文字的宽度",
        ));
    }
    if text.operands.is_empty() {
        return Err(unsupported(index, obj, "操作数写法不规整"));
    }
    if text.continued && text.type3 {
        return Err(unsupported(
            index,
            obj,
            "Type3 字体不看渲染模式，后面又有文字接着写，笔位置补不回去",
        ));
    }
    let (vx, vy) = text
        .tm
        .then(obj.ctm)
        .pull_back(dx, dy)
        .ok_or_else(|| unsupported(index, obj, "当时的文字矩阵不可逆，位移换算不过去"))?;

    // 显示本身：`'`、`"` 拆成“换行（和设间距）”与 `Tj`，位移插在两者之间。
    let mut before: Vec<u8> = b" ".to_vec();
    let mut show: Vec<u8> = Vec::new();
    match (text.show, text.operands.as_slice()) {
        (ShowKind::Show, [s]) => {
            show.extend_from_slice(p.bytes(s));
            show.extend_from_slice(b" Tj");
        }
        (ShowKind::ShowArray, [a]) => {
            show.extend_from_slice(p.bytes(a));
            show.extend_from_slice(b" TJ");
        }
        (ShowKind::NextLineShow, [s]) => {
            before.extend_from_slice(b"T* ");
            show.extend_from_slice(p.bytes(s));
            show.extend_from_slice(b" Tj");
        }
        (ShowKind::NextLineShowSpaced, [aw, ac, s]) => {
            before.extend_from_slice(p.bytes(aw));
            before.extend_from_slice(b" Tw ");
            before.extend_from_slice(p.bytes(ac));
            before.extend_from_slice(b" Tc T* ");
            show.extend_from_slice(p.bytes(s));
            show.extend_from_slice(b" Tj");
        }
        _ => return Err(unsupported(index, obj, "操作数写法不规整")),
    }

    let mut out = before;
    out.extend_from_slice(format!("{} {} Td ", num(vx), num(vy)).as_bytes());
    out.extend_from_slice(&show);
    out.extend_from_slice(format!(" {} {} Td ", num(-vx), num(-vy)).as_bytes());
    if text.continued {
        // 笔回到了行首：不可见地再显示一遍，把笔推到原来该到的地方。
        out.extend_from_slice(b"3 Tr ");
        out.extend_from_slice(&show);
        out.extend_from_slice(format!(" {} Tr ", text.render_mode).as_bytes());
    }
    p.replace(&obj.span, out);
    Ok(())
}

/// 把补丁打到各段上。
fn apply(map: &PageMap, mut patches: Vec<Patch>) -> PageRewrite {
    let content = &map.content;
    let data = &content.data;
    // 同一位置的补丁按登记的先后排（稳定排序）：前一个对象的后缀在后一个对象的前缀之前。
    patches.sort_by_key(|patch| patch.at);
    let mut patches = patches.into_iter().peekable();

    let mut segments: Vec<Option<Vec<u8>>> = Vec::with_capacity(content.segments.len());
    let mut rebuilt: Vec<u8> = Vec::with_capacity(data.len() + 64);
    let mut changed = false;
    // 已经处理到的位置；一处替换跨过段的边界时会超过下一段的起点。
    let mut pos = 0usize;
    let last = content.segments.len().saturating_sub(1);
    for (index, segment) in content.segments.iter().enumerate() {
        pos = pos.max(segment.start);
        let mut out: Vec<u8> = Vec::with_capacity(segment.end - segment.start);
        // 落在段末（补的那个空格的位置）的补丁归这一段；最后一段收下剩下的全部，不丢补丁。
        while let Some(patch) = patches.next_if(|patch| index == last || patch.at <= segment.end) {
            let at = patch.at.min(data.len());
            if at > pos {
                out.extend_from_slice(&data[pos..at]);
                pos = at;
            }
            out.extend_from_slice(&patch.insert);
            pos = pos.max(at.saturating_add(patch.remove));
        }
        if pos < segment.end {
            out.extend_from_slice(&data[pos..segment.end]);
            pos = segment.end;
        }
        // 上一段的替换伸进这一段、删掉了这一段开头的字节，也算变了。
        let original = &data[segment.start..segment.end];
        let differs = out != original;
        rebuilt.extend_from_slice(if differs { &out } else { original });
        if !content.single {
            rebuilt.push(b' ');
        }
        if differs {
            changed = true;
            segments.push(Some(out));
        } else {
            segments.push(None);
        }
    }
    PageRewrite {
        segments,
        data: rebuilt,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::Content;
    use crate::map::resources::{ExtGState, FontLookup, Resources, XObjectKind};
    use crate::map::{PageMap, map_content};

    /// 测试用的资源：`Im0` 是图像，`Fm0` 是表单，`Sh0` 是渐变，`F1` 是普通字体，`T3` 是 Type3。
    struct Fake;

    impl Resources for Fake {
        fn xobject(&self, name: &[u8]) -> XObjectKind {
            match name {
                b"Im0" => XObjectKind::Image,
                b"Fm0" => XObjectKind::Form,
                _ => XObjectKind::Missing,
            }
        }
        fn shading(&self, name: &[u8]) -> bool {
            name == b"Sh0"
        }
        fn font(&self, name: &[u8]) -> FontLookup {
            FontLookup::Loaded {
                type3: name == b"T3",
            }
        }
        fn ext_gstate(&self, _: &[u8]) -> Option<ExtGState> {
            None
        }
        fn colorspace(&self, name: &[u8]) -> Option<Option<u32>> {
            (name == b"CS0").then_some(Some(3))
        }
        fn pattern(&self, _: &[u8]) -> bool {
            false
        }
    }

    fn single(data: &[u8]) -> PageMap {
        map_content(Content::from_single(1, data.to_vec()), &Fake)
    }

    fn edit(index: usize, target: EditTarget) -> EditSet {
        EditSet {
            edits: BTreeMap::from([(index, target)]),
        }
    }

    const MOVE: EditTarget = EditTarget::Move { dx: 10.0, dy: 20.0 };
    const RED: EditTarget = EditTarget::Recolor {
        r: 1.0,
        g: 0.0,
        b: 0.0,
    };

    /// 重写单条流，返回新字节的文本（多余的空白压成一个空格，便于比较）。
    fn rewrite(data: &[u8], index: usize, target: EditTarget) -> String {
        let map = single(data);
        let out = plan_page(&map, &edit(index, target)).expect("这个修改应当写得出来");
        assert!(out.changed);
        let text = String::from_utf8(out.data).unwrap();
        text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn refused(data: &[u8], index: usize, target: EditTarget) -> &'static str {
        match plan_page(&single(data), &edit(index, target)) {
            Err(RewriteError::Unsupported { reason, .. }) => reason,
            other => panic!("应当拒绝，实际是 {other:?}"),
        }
    }

    /// 重写之后重新解释，返回各对象的类别。
    fn kinds_after(data: &[u8], index: usize, target: EditTarget) -> Vec<ObjKind> {
        let map = single(data);
        let out = plan_page(&map, &edit(index, target)).unwrap();
        let again = map_content(Content::from_single(1, out.data), &Fake);
        again.objects.iter().map(|o| o.kind).collect()
    }

    #[test]
    fn no_edits_means_identical_bytes() {
        let data = b"q 1 0 0 1 5 5 cm 0 0 10 10 re f Q BT /F1 12 Tf (a) Tj ET";
        let map = single(data);
        let out = plan_page(&map, &EditSet::default()).unwrap();
        assert!(!out.changed);
        assert_eq!(out.segments, vec![None]);
        assert_eq!(out.data, data);
    }

    #[test]
    fn move_wraps_the_object_in_its_own_coordinates() {
        // 当时的矩阵把坐标放大 2 倍：页面上挪 (10, 20) 相当于这里挪 (5, 10)。
        assert_eq!(
            rewrite(b"2 0 0 2 0 0 cm 0 0 10 10 re f /Im0 Do", 0, MOVE),
            "2 0 0 2 0 0 cm q 1 0 0 1 5 10 cm 0 0 10 10 re f Q /Im0 Do"
        );
        assert_eq!(
            rewrite(b"0 0 10 10 re f /Im0 Do /Fm0 Do", 1, MOVE),
            "0 0 10 10 re f q 1 0 0 1 10 20 cm /Im0 Do Q /Fm0 Do"
        );
        assert_eq!(rewrite(b"/Fm0 Do", 0, MOVE), "q 1 0 0 1 10 20 cm /Fm0 Do Q");
        assert_eq!(
            rewrite(b"q BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q", 0, MOVE),
            "q q 1 0 0 1 10 20 cm BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q Q"
        );
        // 矩阵不可逆。
        assert!(refused(b"0 0 0 0 0 0 cm /Im0 Do", 0, MOVE).contains("不可逆"));
    }

    #[test]
    fn move_path_that_also_clips() {
        // 路径自己挂着 W：先画移动过的副本，原来的只留下剪切。
        assert_eq!(
            rewrite(b"0 0 10 10 re W f 0 0 5 5 re f", 0, MOVE),
            "q 1 0 0 1 10 20 cm 0 0 10 10 re f Q 0 0 10 10 re W n 0 0 5 5 re f"
        );
        assert_eq!(
            kinds_after(b"0 0 10 10 re W f 0 0 5 5 re f", 0, MOVE),
            [ObjKind::Path, ObjKind::Path]
        );
        // 构造和绘制之间夹着别的操作符：拒绝。
        assert!(refused(b"0 0 10 10 re 1 0 0 rg f", 0, MOVE).contains("夹着"));
    }

    #[test]
    fn move_shading_with_its_clip_group() {
        assert_eq!(
            rewrite(b"q 0 0 9 9 re W n 2 0 0 2 0 0 cm /Sh0 sh Q", 0, MOVE),
            "q 1 0 0 1 10 20 cm 0 0 9 9 re W n 2 0 0 2 0 0 cm /Sh0 sh Q"
        );
        assert!(refused(b"/Sh0 sh", 0, MOVE).contains("渐变"));
        assert!(refused(b"q 0 0 9 9 re W n /Sh0 sh /Im0 Do Q", 0, MOVE).contains("渐变"));
    }

    #[test]
    fn recolor_sets_and_restores_the_fill() {
        assert_eq!(
            rewrite(b"0 0 1 rg 0 0 10 10 re f 0 0 5 5 re f", 0, RED),
            "0 0 1 rg 0 0 10 10 re 1 0 0 rg f 0 0 1 rg 0 0 5 5 re f"
        );
        // 没设过颜色：恢复成默认的黑色。
        assert_eq!(
            rewrite(b"0 0 10 10 re f", 0, RED),
            "0 0 10 10 re 1 0 0 rg f 0 g"
        );
        // 颜色空间加颜色：两个都重放。
        assert_eq!(
            rewrite(b"/CS0 cs 0 1 0 sc BT /F1 9 Tf (a) Tj ET", 0, RED),
            "/CS0 cs 0 1 0 sc BT /F1 9 Tf 1 0 0 rg (a) Tj /CS0 cs 0 1 0 sc ET"
        );
        // 路径的构造和绘制之间有别的设色：新颜色要插在绘制操作符跟前。
        assert_eq!(
            rewrite(b"0 0 10 10 re 0 1 0 rg f", 0, RED),
            "0 0 10 10 re 0 1 0 rg 1 0 0 rg f 0 1 0 rg"
        );
        assert!(refused(b"/Im0 Do", 0, RED).contains("只有路径和文字"));
    }

    #[test]
    fn delete_objects() {
        assert_eq!(
            rewrite(b"0 0 10 10 re f /Im0 Do /Fm0 Do", 0, EditTarget::Delete),
            "/Im0 Do /Fm0 Do"
        );
        assert_eq!(
            rewrite(b"0 0 10 10 re f /Im0 Do /Fm0 Do", 1, EditTarget::Delete),
            "0 0 10 10 re f /Fm0 Do"
        );
        assert_eq!(rewrite(b"q /Sh0 sh Q", 0, EditTarget::Delete), "q Q");
        // 挂着 W 的路径：剪切留下。
        assert_eq!(
            rewrite(b"0 0 10 10 re W f /Im0 Do", 0, EditTarget::Delete),
            "0 0 10 10 re W n /Im0 Do"
        );
        // 夹着别的操作符的路径：只把绘制换成 n。
        assert_eq!(
            rewrite(b"0 0 10 10 re 0 g S", 0, EditTarget::Delete),
            "0 0 10 10 re 0 g n"
        );
        assert_eq!(
            kinds_after(b"0 0 10 10 re W f /Im0 Do", 0, EditTarget::Delete),
            [ObjKind::Image]
        );
    }

    #[test]
    fn delete_text() {
        let data = b"BT /F1 9 Tf (a) Tj (b) Tj T* (c) ' 1 2 (d) \" ET";
        // a 后面有 b 接着写：藏起来，不能去掉。
        assert_eq!(
            rewrite(data, 0, EditTarget::Delete),
            "BT /F1 9 Tf 3 Tr (a) Tj 0 Tr (b) Tj T* (c) ' 1 2 (d) \" ET"
        );
        // b 后面是 T*：直接去掉。
        assert_eq!(
            rewrite(data, 1, EditTarget::Delete),
            "BT /F1 9 Tf (a) Tj T* (c) ' 1 2 (d) \" ET"
        );
        // `'` 去掉显示、留下换行；`"` 还要留下它设的间距。
        assert_eq!(
            rewrite(data, 2, EditTarget::Delete),
            "BT /F1 9 Tf (a) Tj (b) Tj T* T* 1 2 (d) \" ET"
        );
        assert_eq!(
            rewrite(data, 3, EditTarget::Delete),
            "BT /F1 9 Tf (a) Tj (b) Tj T* (c) ' 1 Tw 2 Tc T* ET"
        );
        // 藏起来之后渲染模式要设回原来的值。
        assert_eq!(
            rewrite(b"BT /F1 9 Tf 2 Tr (a) Tj (b) Tj ET", 0, EditTarget::Delete),
            "BT /F1 9 Tf 2 Tr 3 Tr (a) Tj 2 Tr (b) Tj ET"
        );
        assert!(refused(b"BT /T3 1 Tf (a) Tj (b) Tj ET", 0, EditTarget::Delete).contains("Type3"));
        assert!(refused(b"BT /F1 9 Tf 7 Tr (a) Tj ET", 0, EditTarget::Delete).contains("剪切模式"));
    }

    #[test]
    fn move_text() {
        // 行首、后面没人接着写：挪过去、显示、挪回来。文字矩阵放大 2 倍，位移减半。
        assert_eq!(
            rewrite(b"BT /F1 9 Tf 2 0 0 2 50 50 Tm (a) Tj ET", 0, MOVE),
            "BT /F1 9 Tf 2 0 0 2 50 50 Tm 5 10 Td (a) Tj -5 -10 Td ET"
        );
        // 后面有人接着写：挪回来之后不可见地再显示一遍。
        assert_eq!(
            rewrite(b"BT /F1 9 Tf (a) Tj [(b) 5 (c)] TJ ET", 0, MOVE),
            "BT /F1 9 Tf 10 20 Td (a) Tj -10 -20 Td 3 Tr (a) Tj 0 Tr [(b) 5 (c)] TJ ET"
        );
        // `'`：换行在前，位移在后。
        assert_eq!(
            rewrite(b"BT /F1 9 Tf 12 TL (a) ' ET", 0, MOVE),
            "BT /F1 9 Tf 12 TL T* 10 20 Td (a) Tj -10 -20 Td ET"
        );
        assert_eq!(
            rewrite(b"BT /F1 9 Tf 1 2 (a) \" ET", 0, MOVE),
            "BT /F1 9 Tf 1 Tw 2 Tc T* 10 20 Td (a) Tj -10 -20 Td ET"
        );
        // 不在行首。
        assert!(refused(b"BT /F1 9 Tf (a) Tj (b) Tj ET", 1, MOVE).contains("行首"));
        // 不在文字块里、操作数不规整、剪切模式。
        assert!(refused(b"/F1 9 Tf (a) Tj", 0, MOVE).contains("BT"));
        assert!(refused(b"BT /F1 9 Tf /abc Tj ET", 0, MOVE).contains("不规整"));
        assert!(refused(b"BT /F1 9 Tf 4 Tr (a) Tj ET", 0, MOVE).contains("剪切模式"));
        // 移动之后对象数不变（后面有人接着写时多一个不可见的）。
        assert_eq!(
            kinds_after(b"BT /F1 9 Tf (a) Tj ET /Im0 Do", 0, MOVE),
            [ObjKind::Text, ObjKind::Image]
        );
    }

    #[test]
    fn edits_are_split_back_into_their_streams() {
        // 三条流；对象跨在第二、第三条流上。
        let content = Content::from_parts(&[
            (Some(1), b"q 0 0 10 10 re".to_vec()),
            (Some(2), b"f /Im0".to_vec()),
            (Some(3), b"Do Q".to_vec()),
        ]);
        let map = map_content(content, &Fake);
        assert_eq!(map.objects.len(), 2);
        // 移动图像：q 插在第二条流里，Q 插在第三条流里；第一条流不动。
        let out = plan_page(&map, &edit(1, MOVE)).unwrap();
        assert_eq!(out.segments[0], None);
        assert_eq!(
            out.segments[1].as_deref(),
            Some(&b"f  q 1 0 0 1 10 20 cm /Im0"[..])
        );
        assert_eq!(out.segments[2].as_deref(), Some(&b"Do Q  Q"[..]));
        assert_eq!(
            out.data,
            b"q 0 0 10 10 re f  q 1 0 0 1 10 20 cm /Im0 Do Q  Q "
        );
        // 删除路径：跨第一、第二条流，两条流都变，第三条不动。
        let out = plan_page(&map, &edit(0, EditTarget::Delete)).unwrap();
        assert_eq!(out.segments[0].as_deref(), Some(&b"q  "[..]));
        assert_eq!(out.segments[1].as_deref(), Some(&b" /Im0"[..]));
        assert_eq!(out.segments[2], None);
        let again = map_content(
            Content::from_parts(&[
                (Some(1), b"q  ".to_vec()),
                (Some(2), b" /Im0".to_vec()),
                (Some(3), b"Do Q".to_vec()),
            ]),
            &Fake,
        );
        assert_eq!(again.objects.len(), 1);
    }

    #[test]
    fn adjacent_objects_keep_their_patch_order() {
        // 两个对象之间没有空白：前一个的后缀要排在后一个的前缀之前。
        let data = b"BT /F1 9 Tf (a)Tj(b)Tj ET";
        let map = single(data);
        let edits = EditSet {
            edits: BTreeMap::from([(0, RED), (1, RED)]),
        };
        let out = plan_page(&map, &edits).unwrap();
        let text = String::from_utf8(out.data).unwrap();
        let text = text.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(text, "BT /F1 9 Tf 1 0 0 rg (a)Tj 0 g 1 0 0 rg (b)Tj 0 g ET");
    }

    #[test]
    fn errors() {
        let map = single(b"/Im0 Do");
        assert_eq!(
            plan_page(&map, &edit(3, EditTarget::Delete)),
            Err(RewriteError::NoSuchObject(3, 1))
        );
        let blocked = single(b"BI /W 1 /H 1 /BPC 8 /CS /RGB /F /DCT ID x EI");
        assert!(matches!(
            plan_page(&blocked, &EditSet::default()),
            Err(RewriteError::Unmatched(_))
        ));
    }
}
