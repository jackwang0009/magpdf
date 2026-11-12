//! 内容流解释器：`CPDF_StreamContentParser` 里决定“建不建对象”的那部分的移植。
//!
//! 出处：PDFium `core/fpdfapi/page/cpdf_streamcontentparser.cpp`（BSD-3-Clause，版权声明见本 crate 的
//! `PDFIUM-NOTICE.md`）。照搬的行为，按和规范不一样的程度排：
//!
//! - 操作数放在 16 项的环形缓冲里，多出来的把最早的挤掉；每个操作符执行完清空。操作符按下标从后往前
//!   取操作数，取不到的数字当 0、取不到的字符串当空串；
//! - `m`、`l` 的操作数不是恰好 2 个时整个操作符被忽略；`rg`/`RG` 要恰好 3 个，`k`/`K` 恰好 4 个，
//!   `Tz` 恰好 1 个；
//! - `m` 之后进入“快速路径”（`ParsePathObject`）：只认数字和 `m l c v y h re`，不检查操作数个数，
//!   数字存在一个 6 项的数组里且**不清零**（上一个操作符留下的值会被下一个用到）；遇到别的元素时
//!   退回到最后一个处理过的路径操作符之后，交还给主循环；
//! - 路径的点列不随 `q`/`Q` 保存恢复；绘制操作符取走点列时：没有点就不建对象；只有一个点时，挂着
//!   `W` 就加一个空矩形剪切，否则只有“闭合的起点 + 圆线帽”才建对象；末尾落单的 `m` 丢掉；
//!   `n` 不建对象；挂着的 `W`/`W*` 在这时生效；
//! - `Tj`/`'`/`"` 的字符串为空时不建对象；`TJ` 的数组里没有非空字符串时不建对象；当前字体为空
//!   （还没有成功的 `Tf`）时不建对象；`Tf` 引用的字体在资源里找不到时换成备用字体（不为空），
//!   找到了但加载失败时保持原来的字体；
//! - `Do` 的 XObject 得是流，`/Subtype` 是 `Form` 或 `Image` 才建对象；`sh` 的渐变得加载得了；
//! - `BI` 的字典里遇到不是 `ID` 的关键字时退回 `BI` 之后，当作什么都没发生；
//! - `q`/`Q` 连文字矩阵、文字位置一起保存恢复；`Q` 多了不出错；
//! - 不认识的操作符忽略。
//!
//! 除了对象本身，还记下写回层要用的东西：对象的字节区间、当时的变换矩阵、填充色的来历、文字的
//! 行内位置关系、渐变所在的 `q … Q` 组。

use std::collections::VecDeque;
use std::ops::Range;

use super::resources::{FontLookup, Resources, XObjectKind};
use super::{
    CLIP_SLACK, Detail, FillHistory, FillRule, ImageDetail, MappedObject, Matrix, ObjKind,
    PathDetail, Rect, ShadingGroup, ShowKind, TextDetail, Unmatched,
};
use crate::content::inline::{InlineOutcome, dict_int, handle_begin_image};
use crate::content::parser::{Element, Number, Obj, StreamParser, name_decode};

/// `kParamBufSize`。
const PARAM_BUF: usize = 16;
/// 填充色的来历最多记这么多个操作符。
const FILL_HISTORY_LIMIT: usize = 16;

/// 解释的结果。
#[derive(Debug)]
pub(super) struct Outcome {
    pub objects: Vec<MappedObject>,
    pub operators: usize,
    pub blocked: Option<Unmatched>,
}

/// 一个操作数。
#[derive(Debug, Clone)]
enum ParamValue {
    Number(Number),
    /// 解码之后的名字。
    Name(Vec<u8>),
    /// 字符串、数组、字典、布尔、空；落单的分隔符没有对象。
    Object(Option<Obj>),
}

#[derive(Debug, Clone)]
struct Param {
    value: ParamValue,
    range: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointKind {
    Move,
    Line,
    Bezier,
}

#[derive(Debug, Clone, Copy)]
struct PathPoint {
    x: f32,
    y: f32,
    kind: PointKind,
    close: bool,
}

/// 路径的状态。不随 `q`/`Q` 保存恢复。
#[derive(Debug, Default)]
struct PathState {
    points: Vec<PathPoint>,
    current: (f32, f32),
    start: (f32, f32),
    /// 挂着的 `W`/`W*`（最近的一个）。
    clip: Option<Range<usize>>,
    /// 点列从空变成非空的那个操作符的操作数起点。
    span_start: Option<usize>,
    /// 见 [`PathDetail::simple`]。
    simple: bool,
    /// 点列非空之后见到的 `W`/`W*` 个数。
    clips_in_span: usize,
}

/// PDFium 的剪切路径列表里和 `CheckClip` 有关的那一点信息（见 [`MappedObject::droppable_clip`]）。
///
/// 出处：`cpdf_clippath.cpp` 的 `AppendPathWithAutoMerge`、`cpdf_contentparser.cpp` 的 `CheckClip`。
/// PDFium 往列表里加路径时会“自动合并”：列表里最后一条是矩形、又包得住新路径的包围盒时，先把那
/// 一条去掉。所以列表可能只剩最后加的那一条，`CheckClip` 只对这种情况动手。
///
/// “是不是矩形”PDFium 是在 `cfx_path.cpp` 里判断的（4 或 5 个点，全是直线，相邻两点同横或同纵，
/// 点多的还要先归一化），这里不重现，只取一个不会漏的条件：至少 4 个点、除了起点全是直线。
#[derive(Debug, Clone, Copy, Default)]
struct ClipTrack {
    /// 最后加进来的那条路径：包围盒（页面空间），可不可能被当成矩形。
    last: Option<(Rect, bool)>,
    /// 列表里可能只剩 `last` 这一条。
    maybe_single: bool,
}

impl ClipTrack {
    fn append(&mut self, bbox: Rect, maybe_rect: bool) {
        self.maybe_single = match self.last {
            None => true,
            Some((last, last_maybe_rect)) => {
                self.maybe_single && last_maybe_rect && last.contains(&bbox, CLIP_SLACK)
            }
        };
        self.last = Some((bbox, maybe_rect));
    }

    /// `CheckClip` 可能去掉的那条剪切。
    fn droppable(&self) -> Option<Rect> {
        match self.last {
            Some((bbox, true)) if self.maybe_single => Some(bbox),
            _ => None,
        }
    }
}

/// 随 `q`/`Q` 保存恢复的状态。
#[derive(Debug, Clone)]
struct State {
    ctm: Matrix,
    tm: Matrix,
    /// 当前字体：`None` 是空，`Some(type3)`。
    font: Option<bool>,
    font_size: f32,
    render_mode: i32,
    line_cap: i32,
    /// 生效的剪切个数（路径和文字都算）。
    clips: usize,
    /// 生效的剪切里有文字剪切。
    text_clip: bool,
    /// 剪切路径列表的近似。文字剪切不算在里面：有文字剪切时 PDFium 不会去掉剪切，但文字剪切
    /// 超过 1024 个时 PDFium 会整批丢掉，这里不跟着数，一律当作可能去掉。
    clip_paths: ClipTrack,
    fill: FillHistory,
    at_line_start: bool,
    /// 上一个定位操作符之后最近一次产生了对象的显示。
    last_show: Option<usize>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            tm: Matrix::IDENTITY,
            font: None,
            font_size: 0.0,
            render_mode: 0,
            line_cap: 0,
            clips: 0,
            text_clip: false,
            clip_paths: ClipTrack::default(),
            fill: FillHistory {
                ops: Vec::new(),
                from_default: true,
                overflow: false,
            },
            at_line_start: true,
            last_show: None,
        }
    }
}

/// `q` 压栈的一层。
#[derive(Debug)]
struct Frame {
    saved: State,
    after_q: usize,
    /// 这一层里唯一的对象是一个渐变时，它的下标。
    sole_shading: Option<usize>,
    /// 这一层里已经建过对象。
    has_objects: bool,
}

struct Interp<'r> {
    resources: &'r dyn Resources,
    params: VecDeque<Param>,
    /// 上一个操作符之后第一个操作数的位置（不受 16 项上限影响）。
    run_start: Option<usize>,
    /// 上一个操作符之后的操作数个数（不受 16 项上限影响）。
    run_count: usize,
    state: State,
    stack: Vec<Frame>,
    path: PathState,
    in_text: bool,
    /// 这个文字块里有剪切模式的文字，`ET` 时并入剪切。
    clip_texts: bool,
    objects: Vec<MappedObject>,
    operators: usize,
}

pub(super) fn interpret(data: &[u8], resources: &dyn Resources) -> Outcome {
    let mut interp = Interp {
        resources,
        params: VecDeque::with_capacity(PARAM_BUF),
        run_start: None,
        run_count: 0,
        state: State::default(),
        stack: Vec::new(),
        path: PathState::default(),
        in_text: false,
        clip_texts: false,
        objects: Vec::new(),
        operators: 0,
    };
    let blocked = interp.run(data);
    Outcome {
        objects: interp.objects,
        operators: interp.operators,
        blocked,
    }
}

fn is_path_construction(op: &[u8]) -> bool {
    matches!(op, b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re")
}

fn is_path_end(op: &[u8]) -> bool {
    matches!(
        op,
        b"B" | b"B*" | b"b" | b"b*" | b"f" | b"F" | b"f*" | b"S" | b"s" | b"n"
    )
}

/// 点列乘上矩阵之后的包围盒（`CFX_Path::GetBoundingBox`：控制点也算）。`points` 不为空。
fn bounding_box(points: &[PathPoint], ctm: &Matrix) -> Rect {
    let mut rect = Rect {
        left: f64::INFINITY,
        bottom: f64::INFINITY,
        right: f64::NEG_INFINITY,
        top: f64::NEG_INFINITY,
    };
    for point in points {
        let (x, y) = (f64::from(point.x), f64::from(point.y));
        let px = ctm.a * x + ctm.c * y + ctm.e;
        let py = ctm.b * x + ctm.d * y + ctm.f;
        rect.left = rect.left.min(px);
        rect.right = rect.right.max(px);
        rect.bottom = rect.bottom.min(py);
        rect.top = rect.top.max(py);
    }
    rect
}

impl Interp<'_> {
    /// `CPDF_StreamContentParser::Parse` 的主循环。
    fn run(&mut self, data: &[u8]) -> Option<Unmatched> {
        let mut parser = StreamParser::new(data);
        loop {
            let element = parser.next_element();
            let range = parser.start()..parser.pos();
            match element {
                Element::EndOfData => return None,
                Element::Keyword => {
                    let op = parser.word().to_vec();
                    self.operators += 1;
                    if let Err(blocked) = self.on_operator(&op, range, &mut parser) {
                        return Some(blocked);
                    }
                    self.params.clear();
                    self.run_start = None;
                    self.run_count = 0;
                }
                Element::Number => {
                    let value = ParamValue::Number(Number::parse(parser.word()));
                    self.push_param(value, range);
                }
                Element::Name => {
                    let value = ParamValue::Name(name_decode(&parser.word()[1..]));
                    self.path_disturbed();
                    self.push_param(value, range);
                }
                Element::Other => {
                    let value = ParamValue::Object(parser.take_object());
                    self.path_disturbed();
                    self.push_param(value, range);
                }
            }
        }
    }

    fn push_param(&mut self, value: ParamValue, range: Range<usize>) {
        if self.run_start.is_none() {
            self.run_start = Some(range.start);
        }
        self.run_count += 1;
        if self.params.len() == PARAM_BUF {
            self.params.pop_front();
        }
        self.params.push_back(Param { value, range });
    }

    /// 路径构造到一半时出现了不属于路径的东西：这条路径的字节不能整体搬动。
    fn path_disturbed(&mut self) {
        if !self.path.points.is_empty() {
            self.path.simple = false;
        }
    }

    /// 从后往前数第 `index` 个操作数。
    fn param(&self, index: usize) -> Option<&Param> {
        let len = self.params.len();
        (index < len).then(|| &self.params[len - 1 - index])
    }

    /// `GetNumber`。
    fn number(&self, index: usize) -> f32 {
        match self.param(index).map(|p| &p.value) {
            Some(ParamValue::Number(n)) => n.to_f32(),
            Some(ParamValue::Object(Some(o))) => o.get_number(),
            _ => 0.0,
        }
    }

    /// `GetString`。
    fn string(&self, index: usize) -> Vec<u8> {
        match self.param(index).map(|p| &p.value) {
            Some(ParamValue::Name(n)) => n.clone(),
            Some(ParamValue::Object(Some(o))) => o.get_string(),
            _ => Vec::new(),
        }
    }

    /// `GetInteger`：数值超出 32 位整数范围时没有值。
    fn integer(&self, index: usize) -> Option<i32> {
        let num = self.number(index);
        (-2_147_483_648.0..2_147_483_648.0)
            .contains(&num)
            .then_some(num as i32)
    }

    /// `GetMatrix`。
    fn matrix(&self) -> Matrix {
        Matrix {
            a: f64::from(self.number(5)),
            b: f64::from(self.number(4)),
            c: f64::from(self.number(3)),
            d: f64::from(self.number(2)),
            e: f64::from(self.number(1)),
            f: f64::from(self.number(0)),
        }
    }

    /// `GetPoint(index)`。
    fn point(&self, index: usize) -> (f32, f32) {
        (self.number(index + 1), self.number(index))
    }

    fn on_operator(
        &mut self,
        op: &[u8],
        range: Range<usize>,
        parser: &mut StreamParser<'_>,
    ) -> Result<(), Unmatched> {
        let count = self.params.len();
        let operands = self.run_start.unwrap_or(range.start);
        if !is_path_construction(op) && !is_path_end(op) && !matches!(op, b"W" | b"W*") {
            self.path_disturbed();
        }
        match op {
            // ---- 路径构造 ----
            b"m" => {
                if count == 2 {
                    let (x, y) = self.point(0);
                    self.add_path_point(x, y, PointKind::Move, operands);
                    self.parse_path_object(parser);
                }
            }
            b"l" => {
                if count == 2 {
                    let (x, y) = self.point(0);
                    self.add_path_point(x, y, PointKind::Line, operands);
                }
            }
            b"c" => {
                for index in [4, 2, 0] {
                    let (x, y) = self.point(index);
                    self.add_path_point(x, y, PointKind::Bezier, operands);
                }
            }
            b"v" => {
                let (cx, cy) = self.path.current;
                self.add_path_point(cx, cy, PointKind::Bezier, operands);
                for index in [2, 0] {
                    let (x, y) = self.point(index);
                    self.add_path_point(x, y, PointKind::Bezier, operands);
                }
            }
            b"y" => {
                for index in [2, 0, 0] {
                    let (x, y) = self.point(index);
                    self.add_path_point(x, y, PointKind::Bezier, operands);
                }
            }
            b"h" => self.close_path(),
            b"re" => {
                let (x, y, w, h) = (
                    self.number(3),
                    self.number(2),
                    self.number(1),
                    self.number(0),
                );
                self.add_path_rect(x, y, w, h, operands);
            }
            b"W" | b"W*" => {
                self.path.clip = Some(range);
                if !self.path.points.is_empty() {
                    self.path.clips_in_span += 1;
                }
            }

            // ---- 路径绘制 ----
            b"b" => {
                self.close_path();
                self.add_path_object(FillRule::NonZero, true, range, operands);
            }
            b"B" => self.add_path_object(FillRule::NonZero, true, range, operands),
            b"b*" => {
                let (sx, sy) = self.path.start;
                self.add_path_point_and_close(sx, sy, PointKind::Line);
                self.add_path_object(FillRule::EvenOdd, true, range, operands);
            }
            b"B*" => self.add_path_object(FillRule::EvenOdd, true, range, operands),
            b"f" | b"F" => self.add_path_object(FillRule::NonZero, false, range, operands),
            b"f*" => self.add_path_object(FillRule::EvenOdd, false, range, operands),
            b"s" => {
                self.close_path();
                self.add_path_object(FillRule::None, true, range, operands);
            }
            b"S" => self.add_path_object(FillRule::None, true, range, operands),
            b"n" => self.add_path_object(FillRule::None, false, range, operands),

            // ---- 图形状态 ----
            b"q" => self.stack.push(Frame {
                saved: self.state.clone(),
                after_q: range.end,
                sole_shading: None,
                has_objects: false,
            }),
            b"Q" => {
                if let Some(frame) = self.stack.pop() {
                    self.state = frame.saved;
                }
            }
            b"cm" => self.state.ctm = self.matrix().then(self.state.ctm),
            b"J" => self.state.line_cap = self.integer(0).unwrap_or(0),
            b"gs" => {
                if let Some(gs) = self.resources.ext_gstate(&self.string(0)) {
                    if let Some((name, size)) = gs.font {
                        self.state.font_size = size;
                        self.state.font = match self.resources.font(&name) {
                            FontLookup::Stock => Some(false),
                            FontLookup::Loaded { type3 } => Some(type3),
                            FontLookup::Failed => None,
                        };
                    }
                    if let Some(cap) = gs.line_cap {
                        self.state.line_cap = cap;
                    }
                }
            }

            // ---- 填充色（只记来历，改色之后要设回去） ----
            b"g" => self.fill_absolute(operands..range.end),
            b"rg" => {
                if count == 3 {
                    self.fill_absolute(operands..range.end);
                }
            }
            b"k" => {
                if count == 4 {
                    self.fill_absolute(operands..range.end);
                }
            }
            b"cs" => {
                if self.colorspace_resolves(&self.string(0)) {
                    self.fill_absolute(operands..range.end);
                } else {
                    self.fill_relative(operands..range.end);
                }
            }
            b"sc" | b"scn" => self.fill_relative(operands..range.end),

            // ---- 文字 ----
            b"BT" => {
                self.state.tm = Matrix::IDENTITY;
                self.line_reset();
                self.in_text = true;
            }
            b"ET" => {
                if self.clip_texts {
                    if self.state.render_mode >= 4 {
                        self.state.clips += 1;
                        self.state.text_clip = true;
                    }
                    self.clip_texts = false;
                }
                self.in_text = false;
            }
            b"Tm" => {
                self.state.tm = self.matrix();
                self.line_reset();
            }
            b"Td" | b"TD" | b"T*" => self.line_reset(),
            b"Tf" => {
                self.state.font_size = self.number(0);
                match self.resources.font(&self.string(1)) {
                    FontLookup::Stock => self.state.font = Some(false),
                    FontLookup::Loaded { type3 } => self.state.font = Some(type3),
                    FontLookup::Failed => {}
                }
            }
            b"Tr" => {
                if let Some(mode @ 0..=7) = Some(self.integer(0).unwrap_or(0)) {
                    self.state.render_mode = mode;
                }
            }
            b"Tj" => self.show_string(ShowKind::Show, range, operands),
            b"'" => {
                self.line_reset();
                self.show_string(ShowKind::NextLineShow, range, operands);
            }
            b"\"" => {
                self.line_reset();
                self.show_string(ShowKind::NextLineShowSpaced, range, operands);
            }
            b"TJ" => self.show_array(range, operands),

            // ---- 外部对象、渐变、内联图像 ----
            b"Do" => match self.resources.xobject(&self.string(0)) {
                XObjectKind::Form => {
                    self.push_object(ObjKind::Form, operands..range.end, range, Detail::Form);
                }
                XObjectKind::Image => {
                    let detail = Detail::Image(ImageDetail {
                        inline: false,
                        width: 0,
                        height: 0,
                    });
                    self.push_object(ObjKind::Image, operands..range.end, range, detail);
                }
                XObjectKind::Missing | XObjectKind::Other => {}
            },
            b"sh" => {
                if self.resources.shading(&self.string(0)) {
                    self.add_shading(operands..range.end, range);
                }
            }
            b"BI" => {
                let resources = self.resources;
                let outcome = handle_begin_image(parser, &|name| resources.colorspace(name))
                    .map_err(|e| Unmatched::InlineImageFilter(e.filter))?;
                if let InlineOutcome::Done { object: true, dict } = outcome {
                    let detail = Detail::Image(ImageDetail {
                        inline: true,
                        width: dict_int(&dict, b"Width"),
                        height: dict_int(&dict, b"Height"),
                    });
                    let whole = operands..parser.pos();
                    self.push_object(
                        ObjKind::Image,
                        whole.clone(),
                        range.start..whole.end,
                        detail,
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ---- 路径 ----

    /// 点列刚从空变成非空：记下这条路径从哪里开始。
    fn note_path_start(&mut self, operands: usize) {
        if self.path.span_start.is_none() && !self.path.points.is_empty() {
            self.path.span_start = Some(operands);
            // 起点之前就挂着 `W` 的话，那个 `W` 不在这一段字节里。
            self.path.simple = self.path.clip.is_none();
            self.path.clips_in_span = 0;
        }
    }

    /// `AddPathPoint`。
    fn add_path_point(&mut self, x: f32, y: f32, kind: PointKind, operands: usize) {
        let path = &mut self.path;
        // 和上一个没有闭合的起点是同一个点的 `m`：跳过。
        if kind == PointKind::Move
            && let Some(last) = path.points.last()
            && !last.close
            && last.kind == PointKind::Move
            && path.current == (x, y)
        {
            return;
        }
        path.current = (x, y);
        if kind == PointKind::Move {
            path.start = (x, y);
            if let Some(last) = path.points.last_mut()
                && last.kind == PointKind::Move
                && !last.close
            {
                last.x = x;
                last.y = y;
                return;
            }
        } else if path.points.is_empty() {
            return;
        }
        path.points.push(PathPoint {
            x,
            y,
            kind,
            close: false,
        });
        self.note_path_start(operands);
    }

    /// `AddPathPointAndClose`。
    fn add_path_point_and_close(&mut self, x: f32, y: f32, kind: PointKind) {
        self.path.current = (x, y);
        if self.path.points.is_empty() {
            return;
        }
        self.path.points.push(PathPoint {
            x,
            y,
            kind,
            close: true,
        });
    }

    /// `AddPathRect`。
    fn add_path_rect(&mut self, x: f32, y: f32, w: f32, h: f32, operands: usize) {
        self.add_path_point(x, y, PointKind::Move, operands);
        self.add_path_point(x + w, y, PointKind::Line, operands);
        self.add_path_point(x + w, y + h, PointKind::Line, operands);
        self.add_path_point(x, y + h, PointKind::Line, operands);
        self.add_path_point_and_close(x, y, PointKind::Line);
    }

    /// `Handle_ClosePath`。
    fn close_path(&mut self) {
        if self.path.points.is_empty() {
            return;
        }
        if self.path.start != self.path.current {
            let (sx, sy) = self.path.start;
            self.add_path_point_and_close(sx, sy, PointKind::Line);
        } else if let Some(last) = self.path.points.last_mut() {
            last.close = true;
        }
    }

    /// `ParsePathObject`：`m` 之后的快速路径。
    fn parse_path_object(&mut self, parser: &mut StreamParser<'_>) {
        let mut params = [0f32; 6];
        let mut count = 0usize;
        let mut last_pos = parser.pos();
        let mut run_start: Option<usize> = None;
        loop {
            let element = parser.next_element();
            let processed = match element {
                Element::EndOfData => return,
                Element::Number => {
                    if run_start.is_none() {
                        run_start = Some(parser.start());
                    }
                    if count < 6 {
                        params[count] = Number::parse(parser.word()).to_f32();
                        count += 1;
                    }
                    continue;
                }
                Element::Keyword => {
                    let operands = run_start.unwrap_or(parser.start());
                    let [p0, p1, p2, p3, p4, p5] = params;
                    match parser.word() {
                        b"m" => {
                            self.add_path_point(p0, p1, PointKind::Move, operands);
                            true
                        }
                        b"l" => {
                            self.add_path_point(p0, p1, PointKind::Line, operands);
                            true
                        }
                        b"c" => {
                            self.add_path_point(p0, p1, PointKind::Bezier, operands);
                            self.add_path_point(p2, p3, PointKind::Bezier, operands);
                            self.add_path_point(p4, p5, PointKind::Bezier, operands);
                            true
                        }
                        b"v" => {
                            let (cx, cy) = self.path.current;
                            self.add_path_point(cx, cy, PointKind::Bezier, operands);
                            self.add_path_point(p0, p1, PointKind::Bezier, operands);
                            self.add_path_point(p2, p3, PointKind::Bezier, operands);
                            true
                        }
                        b"y" => {
                            self.add_path_point(p0, p1, PointKind::Bezier, operands);
                            self.add_path_point(p2, p3, PointKind::Bezier, operands);
                            self.add_path_point(p2, p3, PointKind::Bezier, operands);
                            true
                        }
                        b"h" => {
                            self.close_path();
                            true
                        }
                        b"re" => {
                            self.add_path_rect(p0, p1, p2, p3, operands);
                            true
                        }
                        _ => false,
                    }
                }
                Element::Name | Element::Other => false,
            };
            if !processed {
                parser.set_pos(last_pos);
                return;
            }
            self.operators += 1;
            count = 0;
            run_start = None;
            last_pos = parser.pos();
        }
    }

    /// `AddPathObject`：绘制操作符和 `n` 都走这里。
    fn add_path_object(&mut self, fill: FillRule, stroke: bool, op: Range<usize>, operands: usize) {
        let mut points = std::mem::take(&mut self.path.points);
        let clip = self.path.clip.take();
        let span_start = self.path.span_start.take();
        let simple = self.path.simple && self.path.clips_in_span <= 1;
        self.path.simple = false;
        self.path.clips_in_span = 0;

        if points.is_empty() {
            return;
        }
        if points.len() == 1 {
            if clip.is_some() {
                // 加一个空矩形剪切（`AppendRect(0, 0, 0, 0)`，不乘矩阵）。
                self.state.clips += 1;
                let empty = Rect {
                    left: 0.0,
                    bottom: 0.0,
                    right: 0.0,
                    top: 0.0,
                };
                self.state.clip_paths.append(empty, true);
                return;
            }
            let point = points[0];
            if point.kind != PointKind::Move || !point.close || self.state.line_cap != 1 {
                return;
            }
            // 圆线帽：移到一个点马上闭合，当作从这个点画到它自己。
            points[0].close = false;
            points.push(PathPoint {
                x: point.x,
                y: point.y,
                kind: PointKind::Line,
                close: true,
            });
        }
        if let Some(last) = points.last()
            && last.kind == PointKind::Move
            && !last.close
        {
            points.pop();
        }
        if stroke || fill != FillRule::None {
            let detail = Detail::Path(PathDetail {
                clip: clip.clone(),
                simple,
                points: points.len(),
                stroke,
                fill,
            });
            let span = span_start.unwrap_or(operands)..op.end;
            self.push_object_with(ObjKind::Path, span, op, operands, detail);
        }
        if clip.is_some() {
            // 对象（如果有）用的是加这条剪切之前的状态；剪切路径是点列乘上此刻的矩阵。
            self.state.clips += 1;
            let maybe_rect =
                points.len() >= 4 && points[1..].iter().all(|p| p.kind == PointKind::Line);
            self.state
                .clip_paths
                .append(bounding_box(&points, &self.state.ctm), maybe_rect);
        }
    }

    // ---- 填充色 ----

    /// 一步到位的设色：之前的来历作废。
    fn fill_absolute(&mut self, op: Range<usize>) {
        self.state.fill = FillHistory {
            ops: vec![op],
            from_default: false,
            overflow: false,
        };
    }

    /// 依赖当前颜色空间的设色（或者一个 PDFium 可能不认的 `cs`）：接在来历后面。
    fn fill_relative(&mut self, op: Range<usize>) {
        let fill = &mut self.state.fill;
        if fill.ops.len() >= FILL_HISTORY_LIMIT {
            fill.overflow = true;
        } else {
            fill.ops.push(op);
        }
    }

    /// `FindColorSpace` 找不找得到。找不到时 PDFium 忽略整个 `cs`。
    fn colorspace_resolves(&self, name: &[u8]) -> bool {
        match name {
            b"Pattern" => true,
            b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" => {
                // 资源里有 `DefaultGray` 之类时用它（加载不了就算找不到）。
                let mut default = b"Default".to_vec();
                default.extend_from_slice(&name[6..]);
                match self.resources.colorspace(&default) {
                    None => true,
                    Some(loaded) => loaded.is_some(),
                }
            }
            other => matches!(self.resources.colorspace(other), Some(Some(_))),
        }
    }

    // ---- 文字 ----

    /// 重新定位：笔回到行首。
    fn line_reset(&mut self) {
        self.state.at_line_start = true;
        self.state.last_show = None;
    }

    /// 操作数的字节区间（写法规整时）。
    fn regular_operands(&self, kind: ShowKind) -> Vec<Range<usize>> {
        let is_string = |p: &Param| matches!(p.value, ParamValue::Object(Some(Obj::Str(_))));
        let is_number = |p: &Param| matches!(p.value, ParamValue::Number(_));
        let params: Vec<&Param> = self.params.iter().collect();
        let regular = match (kind, params.as_slice()) {
            (ShowKind::Show | ShowKind::NextLineShow, [s]) => is_string(s),
            (ShowKind::ShowArray, [a]) => {
                matches!(a.value, ParamValue::Object(Some(Obj::Array(_))))
            }
            (ShowKind::NextLineShowSpaced, [aw, ac, s]) => {
                is_number(aw) && is_number(ac) && is_string(s)
            }
            _ => false,
        };
        if regular && self.run_count == params.len() {
            params.iter().map(|p| p.range.clone()).collect()
        } else {
            Vec::new()
        }
    }

    /// `Handle_ShowText`（`'`、`"` 先换行再走这里）。
    fn show_string(&mut self, kind: ShowKind, op: Range<usize>, operands: usize) {
        let non_empty = !self.string(0).is_empty();
        self.show(kind, non_empty, op, operands);
    }

    /// `Handle_ShowText_Positioning`。
    fn show_array(&mut self, op: Range<usize>, operands: usize) {
        let Some(ParamValue::Object(Some(Obj::Array(items)))) = self.param(0).map(|p| &p.value)
        else {
            return;
        };
        let non_empty = items
            .iter()
            .any(|item| matches!(item, Obj::Str(s) if !s.is_empty()));
        self.show(ShowKind::ShowArray, non_empty, op, operands);
    }

    fn show(&mut self, kind: ShowKind, non_empty: bool, op: Range<usize>, operands: usize) {
        let at_line_start = self.state.at_line_start;
        if matches!(kind, ShowKind::Show | ShowKind::ShowArray)
            && let Some(previous) = self.state.last_show
            && let Some(MappedObject {
                detail: Detail::Text(text),
                ..
            }) = self.objects.get_mut(previous)
        {
            text.continued = true;
        }
        self.state.at_line_start = false;
        let Some(type3) = self.state.font else {
            return;
        };
        if !non_empty {
            return;
        }
        let detail = Detail::Text(TextDetail {
            show: kind,
            operands: self.regular_operands(kind),
            at_line_start,
            continued: false,
            in_text_block: self.in_text,
            tm: self.state.tm,
            render_mode: self.state.render_mode,
            type3,
            font_size: self.state.font_size,
        });
        let index = self.push_object(ObjKind::Text, operands..op.end, op, detail);
        self.state.last_show = Some(index);
        if !type3 && self.state.render_mode >= 4 {
            self.clip_texts = true;
        }
    }

    // ---- 对象 ----

    fn push_object(
        &mut self,
        kind: ObjKind,
        span: Range<usize>,
        op: Range<usize>,
        detail: Detail,
    ) -> usize {
        let operands = span.start;
        self.push_object_with(kind, span, op, operands, detail)
    }

    fn push_object_with(
        &mut self,
        kind: ObjKind,
        span: Range<usize>,
        op: Range<usize>,
        op_operands: usize,
        detail: Detail,
    ) -> usize {
        // 这个对象所在的每一层 `q`：层里不再只有一个渐变。
        for frame in &mut self.stack {
            if let Some(shading) = frame.sole_shading.take()
                && let Some(MappedObject {
                    detail: Detail::Shading(group),
                    ..
                }) = self.objects.get_mut(shading)
            {
                *group = None;
            }
            frame.has_objects = true;
        }
        let index = self.objects.len();
        // `CheckClip` 跳过渐变。
        let droppable_clip = if kind == ObjKind::Shading {
            None
        } else {
            self.state.clip_paths.droppable()
        };
        self.objects.push(MappedObject {
            kind,
            span,
            op,
            op_operands,
            ctm: self.state.ctm,
            fill: self.state.fill.clone(),
            droppable_clip,
            detail,
        });
        index
    }

    /// 渐变对象。看它是不是独占一层 `q`、剪切是不是都在这一层里建立的。
    fn add_shading(&mut self, span: Range<usize>, op: Range<usize>) {
        let group = self.stack.last().and_then(|frame| {
            let own_clip = frame.saved.clips == 0
                && !frame.saved.text_clip
                && self.state.clips > 0
                && !self.state.text_clip;
            (own_clip && !frame.has_objects).then_some(ShadingGroup {
                after_q: frame.after_q,
                ctm: frame.saved.ctm,
            })
        });
        let grouped = group.is_some();
        let index = self.push_object(ObjKind::Shading, span, op, Detail::Shading(group));
        if grouped && let Some(frame) = self.stack.last_mut() {
            frame.sole_shading = Some(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::resources::ExtGState;
    use super::*;

    /// 测试用的资源表。
    #[derive(Default)]
    struct Fake {
        xobjects: BTreeMap<Vec<u8>, XObjectKind>,
        shadings: Vec<Vec<u8>>,
        fonts: BTreeMap<Vec<u8>, FontLookup>,
        gstates: BTreeMap<Vec<u8>, ExtGState>,
        colorspaces: BTreeMap<Vec<u8>, Option<u32>>,
    }

    impl Resources for Fake {
        fn xobject(&self, name: &[u8]) -> XObjectKind {
            self.xobjects
                .get(name)
                .copied()
                .unwrap_or(XObjectKind::Missing)
        }
        fn shading(&self, name: &[u8]) -> bool {
            self.shadings.iter().any(|n| n == name)
        }
        fn font(&self, name: &[u8]) -> FontLookup {
            self.fonts.get(name).copied().unwrap_or(FontLookup::Stock)
        }
        fn ext_gstate(&self, name: &[u8]) -> Option<ExtGState> {
            self.gstates.get(name).cloned()
        }
        fn colorspace(&self, name: &[u8]) -> Option<Option<u32>> {
            self.colorspaces.get(name).copied()
        }
        fn pattern(&self, _: &[u8]) -> bool {
            false
        }
    }

    fn fake() -> Fake {
        let mut f = Fake::default();
        f.xobjects.insert(b"Im0".to_vec(), XObjectKind::Image);
        f.xobjects.insert(b"Fm0".to_vec(), XObjectKind::Form);
        f.xobjects.insert(b"Odd".to_vec(), XObjectKind::Other);
        f.shadings.push(b"Sh0".to_vec());
        f.fonts
            .insert(b"F1".to_vec(), FontLookup::Loaded { type3: false });
        f.fonts
            .insert(b"T3".to_vec(), FontLookup::Loaded { type3: true });
        f.fonts.insert(b"Bad".to_vec(), FontLookup::Failed);
        f.colorspaces.insert(b"CS0".to_vec(), Some(3));
        f
    }

    fn run(data: &[u8]) -> Outcome {
        interpret(data, &fake())
    }

    fn kinds(data: &[u8]) -> Vec<ObjKind> {
        run(data).objects.iter().map(|o| o.kind).collect()
    }

    fn path(o: &MappedObject) -> &PathDetail {
        match &o.detail {
            Detail::Path(p) => p,
            other => panic!("不是路径：{other:?}"),
        }
    }

    fn text(o: &MappedObject) -> &TextDetail {
        match &o.detail {
            Detail::Text(t) => t,
            other => panic!("不是文字：{other:?}"),
        }
    }

    fn bytes<'a>(data: &'a [u8], range: &Range<usize>) -> &'a str {
        std::str::from_utf8(&data[range.clone()]).unwrap()
    }

    #[test]
    fn empty_and_comment_only_streams_have_no_objects() {
        assert!(kinds(b"").is_empty());
        assert!(kinds(b"   \n").is_empty());
        assert!(kinds(b"% just a comment").is_empty());
        assert!(kinds(b"% comment\n q Q % another\n").is_empty());
        assert_eq!(run(b"q Q").operators, 2);
    }

    #[test]
    fn simple_paths_and_their_spans() {
        let data = b"1 0 0 rg 10 10 100 50 re f 0 0 m 10 0 l 10 10 l h S";
        let out = run(data);
        assert_eq!(out.objects.len(), 2);
        let first = &out.objects[0];
        assert_eq!(bytes(data, &first.span), "10 10 100 50 re f");
        assert_eq!(bytes(data, &first.op), "f");
        assert!(path(first).simple);
        assert_eq!(path(first).points, 5);
        assert_eq!(path(first).fill, FillRule::NonZero);
        assert!(!path(first).stroke);
        assert_eq!(first.fill.ops.len(), 1);
        assert_eq!(bytes(data, &first.fill.ops[0]), "1 0 0 rg");
        let second = &out.objects[1];
        assert_eq!(bytes(data, &second.span), "0 0 m 10 0 l 10 10 l h S");
        assert!(path(second).simple && path(second).stroke);
        assert_eq!(path(second).points, 4, "起点、两条线、闭合补的一条线");
    }

    #[test]
    fn operand_count_rules() {
        // `m`、`l` 要恰好 2 个操作数：多了少了都被忽略。
        assert!(
            kinds(b"1 2 3 m 10 10 l S").is_empty(),
            "m 被忽略，l 没有起点"
        );
        assert!(kinds(b"5 m 10 10 l S").is_empty());
        assert_eq!(kinds(b"0 0 m 1 10 10 l 20 20 l S"), [ObjKind::Path]);
        // 上面那条：快速路径里 `l` 不检查个数，取数组的前两项（1、10）。
        let out = run(b"0 0 m 1 10 10 l 20 20 l S");
        assert_eq!(path(&out.objects[0]).points, 3);
        // 主循环里的 `l`（前面隔了别的操作符）要检查个数。
        let out = run(b"0 0 m 5 5 l 0 g 1 10 10 l S");
        assert_eq!(
            path(&out.objects[0]).points,
            2,
            "第二个 l 有 3 个操作数，被忽略"
        );
        assert!(!path(&out.objects[0]).simple, "中间夹了 `0 g`");
        // `re` 不检查个数，缺的当 0。
        assert_eq!(kinds(b"5 5 re f"), [ObjKind::Path]);
    }

    #[test]
    fn fast_path_keeps_stale_numbers_and_rewinds() {
        // 第二个 `l` 只给了一个数：数组第二项还是上一个 `l` 留下的 7，所以它画到 (9, 7)，
        // 和上一个点不同；要是清零了就是 (9, 0)。两种情况点数一样，这里只确认照常建对象。
        let out = run(b"1 2 m 5 7 l 9 l S");
        assert_eq!(out.objects.len(), 1);
        assert_eq!(path(&out.objects[0]).points, 3);
        // `m` 自己的操作数由主循环取走，不进快速路径的数组：`3 l` 画到 (3, 0)。
        let out = run(b"1 2 m 3 l S");
        assert_eq!(path(&out.objects[0]).points, 2);
        // 快速路径遇到不认识的东西就退回主循环，那里的数字重新当操作数读。
        let data = b"0 0 m 10 0 l 1 0 0 RG S";
        let out = run(data);
        assert_eq!(out.objects.len(), 1);
        assert!(!path(&out.objects[0]).simple);
        assert_eq!(bytes(data, &out.objects[0].span), "0 0 m 10 0 l 1 0 0 RG S");
        // 多于 6 个数字：只留前 6 个。
        let out = run(b"0 0 m 1 2 3 4 5 6 7 8 c S");
        assert_eq!(path(&out.objects[0]).points, 4);
    }

    #[test]
    fn single_point_paths() {
        // 只有一个点：不建对象。
        assert!(kinds(b"10 10 m S").is_empty());
        assert!(kinds(b"10 10 m h S").is_empty(), "闭合的起点，线帽不是圆的");
        // 圆线帽 + 闭合的起点：建对象（两个点）。
        let out = run(b"1 J 10 10 m h S");
        assert_eq!(out.objects.len(), 1);
        assert_eq!(path(&out.objects[0]).points, 2);
        // ExtGState 里的 /LC 也算。
        let mut f = fake();
        f.gstates.insert(
            b"GS0".to_vec(),
            ExtGState {
                line_cap: Some(1),
                font: None,
            },
        );
        assert_eq!(interpret(b"/GS0 gs 10 10 m h S", &f).objects.len(), 1);
        // 末尾落单的 `m` 丢掉。
        let out = run(b"0 0 10 10 re 50 50 m f");
        assert_eq!(path(&out.objects[0]).points, 5);
        // 同一个点的重复 `m` 跳过；不同点的 `m` 替换上一个没用过的 `m`。
        let out = run(b"0 0 m 0 0 m 5 5 m 9 9 l S");
        assert_eq!(path(&out.objects[0]).points, 2);
    }

    #[test]
    fn end_path_and_clipping() {
        // `n` 不建对象；`W n` 只建立剪切。
        assert!(kinds(b"0 0 10 10 re n").is_empty());
        assert!(kinds(b"0 0 10 10 re W n").is_empty());
        // 既剪切又绘制。
        let data = b"0 0 10 10 re W* f 0 0 5 5 re f";
        let out = run(data);
        assert_eq!(out.objects.len(), 2);
        let clip = path(&out.objects[0]).clip.clone().expect("挂着 W*");
        assert_eq!(bytes(data, &clip), "W*");
        assert!(path(&out.objects[0]).simple);
        assert!(path(&out.objects[1]).clip.is_none(), "W 只管一次");
        // `W` 写在路径开始之前：也生效，但不在这条路径的字节里。
        let out = run(b"W 0 0 10 10 re f");
        assert!(path(&out.objects[0]).clip.is_some());
        assert!(!path(&out.objects[0]).simple);
        // 没有路径时的 `W n`：`W` 被这次 `n` 用掉，不会留给后面的路径。
        let out = run(b"W n 0 0 10 10 re f");
        assert!(path(&out.objects[0]).clip.is_none());
    }

    #[test]
    fn path_survives_q_and_cm_but_is_not_simple() {
        // 路径的点列不随 q/Q 保存恢复；绘制时用当时的矩阵。
        let data = b"0 0 10 10 re q 2 0 0 2 0 0 cm f Q";
        let out = run(data);
        assert_eq!(out.objects.len(), 1);
        assert!(!path(&out.objects[0]).simple);
        assert_eq!(out.objects[0].ctm.a, 2.0);
        assert_eq!(
            bytes(data, &out.objects[0].span),
            "0 0 10 10 re q 2 0 0 2 0 0 cm f"
        );
    }

    #[test]
    fn close_and_paint_operators() {
        for op in ["b", "b*", "s", "B", "B*", "f", "F", "f*", "S"] {
            let data = format!("0 0 m 10 0 l 10 10 l {op}");
            assert_eq!(kinds(data.as_bytes()), [ObjKind::Path], "{op}");
        }
        // 不认识的操作符被忽略。
        assert_eq!(kinds(b"0 0 m 10 0 l foo S"), [ObjKind::Path]);
    }

    #[test]
    fn matrices_compose_and_restore() {
        let out = run(b"2 0 0 2 0 0 cm q 1 0 0 1 10 20 cm /Im0 Do Q /Im0 Do Q Q /Fm0 Do");
        assert_eq!(
            out.objects.iter().map(|o| o.kind).collect::<Vec<_>>(),
            [ObjKind::Image, ObjKind::Image, ObjKind::Form]
        );
        let first = out.objects[0].ctm;
        assert_eq!((first.a, first.d, first.e, first.f), (2.0, 2.0, 20.0, 40.0));
        let second = out.objects[1].ctm;
        assert_eq!((second.a, second.e), (2.0, 0.0), "Q 恢复了矩阵");
        assert_eq!(out.objects[2].ctm.a, 2.0, "多出来的 Q 不出错也不改状态");
    }

    #[test]
    fn missing_resources_create_no_objects() {
        assert!(kinds(b"/Nope Do").is_empty(), "资源里没有");
        assert!(kinds(b"/Odd Do").is_empty(), "Subtype 不是 Form/Image");
        assert!(kinds(b"/NoShading sh").is_empty());
        assert_eq!(
            kinds(b"/Sh0 sh /Im0 Do /Fm0 Do"),
            [ObjKind::Shading, ObjKind::Image, ObjKind::Form]
        );
        // `Do` 取最后一个操作数；前面多出来的操作数属于同一个操作符。
        let data = b"1 2 /Im0 Do";
        let out = run(data);
        assert_eq!(bytes(data, &out.objects[0].span), "1 2 /Im0 Do");
        assert_eq!(bytes(data, &out.objects[0].op), "Do");
    }

    #[test]
    fn text_needs_a_font_and_a_non_empty_string() {
        // 还没有成功的 Tf：字体为空，不建对象。
        assert!(kinds(b"BT (abc) Tj ET").is_empty());
        // 资源里没有的字体名：换成备用字体，照样建对象。
        assert_eq!(kinds(b"BT /Missing 12 Tf (abc) Tj ET"), [ObjKind::Text]);
        // 加载失败的字体：保持原来的（这里原来是空）。
        assert!(kinds(b"BT /Bad 12 Tf (abc) Tj ET").is_empty());
        assert_eq!(
            kinds(b"BT /F1 12 Tf /Bad 9 Tf (abc) Tj ET"),
            [ObjKind::Text]
        );
        // 空串不建对象。
        assert!(kinds(b"BT /F1 12 Tf () Tj <> Tj ET").is_empty());
        // TJ：有非空字符串才建对象；只有数字、全是空串、不是数组都不建。
        assert_eq!(kinds(b"BT /F1 12 Tf [(a) -100 (b)] TJ ET"), [ObjKind::Text]);
        assert!(kinds(b"BT /F1 12 Tf [-100 20] TJ [() ()] TJ (x) TJ ET").is_empty());
        // 名字当操作数：PDFium 把名字的内容当成字符串显示。
        assert_eq!(kinds(b"BT /F1 12 Tf /abc Tj ET"), [ObjKind::Text]);
        // 数字当操作数：取不到字符串，不建对象。
        assert!(kinds(b"BT /F1 12 Tf 5 Tj ET").is_empty());
        // BT/ET 不是必需的。
        assert_eq!(kinds(b"/F1 12 Tf (abc) Tj"), [ObjKind::Text]);
        // ExtGState 的 /Font 也能设字体。
        let mut f = fake();
        f.gstates.insert(
            b"GS0".to_vec(),
            ExtGState {
                line_cap: None,
                font: Some((Vec::new(), 10.0)),
            },
        );
        let out = interpret(b"/GS0 gs (abc) Tj", &f);
        assert_eq!(out.objects.len(), 1);
        assert_eq!(text(&out.objects[0]).font_size, 10.0);
    }

    #[test]
    fn text_positioning_operators() {
        // 每个显示操作符一个对象；`'`、`"` 自带换行。
        let data = b"BT /F1 12 Tf 10 TL 72 700 Td (a) Tj (b) Tj T* (c) Tj (d) ' 1 2 (e) \" ET";
        let out = run(data);
        assert_eq!(out.objects.len(), 5);
        let t: Vec<&TextDetail> = out.objects.iter().map(text).collect();
        assert!(t[0].at_line_start && t[0].continued, "a 后面紧跟着 b");
        assert!(!t[1].at_line_start && !t[1].continued, "b 之后是 T*");
        assert!(t[2].at_line_start && !t[2].continued, "c 之后的 ' 自带换行");
        assert!(t[3].at_line_start && t[3].show == ShowKind::NextLineShow);
        assert!(t[4].at_line_start && t[4].show == ShowKind::NextLineShowSpaced);
        assert_eq!(bytes(data, &out.objects[4].span), "1 2 (e) \"");
        assert_eq!(t[4].operands.len(), 3);
        assert_eq!(bytes(data, &t[4].operands[2]), "(e)");
        assert!(t.iter().all(|t| t.in_text_block && t.font_size == 12.0));
        // Tm 设文字矩阵；BT 把它还原。
        let out = run(b"BT /F1 12 Tf 2 0 0 2 10 10 Tm (a) Tj ET BT (b) Tj ET");
        assert_eq!(text(&out.objects[0]).tm.a, 2.0);
        assert_eq!(text(&out.objects[1]).tm.a, 1.0);
        assert!(text(&out.objects[1]).at_line_start);
        assert!(!text(&out.objects[0]).continued, "BT 重新定位");
        // 只有字距的 TJ 也会挪笔：后面的显示不在行首，前面的显示算“有人接着写”。
        let out = run(b"BT /F1 12 Tf (a) Tj [-500] TJ (b) Tj ET");
        assert_eq!(out.objects.len(), 2);
        assert!(text(&out.objects[0]).continued);
        assert!(!text(&out.objects[1]).at_line_start);
    }

    #[test]
    fn q_restores_text_position_state() {
        // PDFium 的 q/Q 连文字位置一起保存恢复：Q 之后的显示不依赖 q 里面那次显示。
        let out = run(b"BT /F1 12 Tf (a) Tj q (b) Tj Q (c) Tj ET");
        assert_eq!(out.objects.len(), 3);
        assert!(text(&out.objects[0]).continued, "b、c 都接着 a 写");
        assert!(!text(&out.objects[1]).continued, "c 不接着 b");
        assert!(!text(&out.objects[2]).at_line_start);
    }

    #[test]
    fn irregular_operands_are_flagged() {
        let out = run(b"BT /F1 12 Tf 5 (a) Tj /abc Tj (b) Tj ET");
        assert_eq!(out.objects.len(), 3);
        assert!(text(&out.objects[0]).operands.is_empty(), "多了一个操作数");
        assert!(text(&out.objects[1]).operands.is_empty(), "名字不是字符串");
        assert_eq!(text(&out.objects[2]).operands.len(), 1);
    }

    #[test]
    fn type3_and_render_modes() {
        let out = run(b"BT /T3 1 Tf 7 Tr (a) Tj /F1 9 Tf (b) Tj 9 Tr (c) Tj ET");
        assert_eq!(out.objects.len(), 3);
        assert!(text(&out.objects[0]).type3);
        assert_eq!(text(&out.objects[1]).render_mode, 7);
        assert_eq!(text(&out.objects[2]).render_mode, 7, "无效的模式被忽略");
        // 剪切模式的文字照样是对象。
        assert_eq!(
            kinds(b"BT /F1 12 Tf 4 Tr (a) Tj 5 Tr (b) Tj 6 Tr (c) Tj 7 Tr (d) Tj ET"),
            [ObjKind::Text; 4]
        );
    }

    #[test]
    fn tokens_may_span_streams() {
        // 多条流拼接之后：q 与 Q、BT 与 ET、操作数与操作符分在不同的流里。
        let c = crate::content::Content::from_parts(&[
            (Some(1), b"q 1 0 0 1 5 5 cm BT /F1 12".to_vec()),
            (Some(2), b"Tf (a) Tj".to_vec()),
            (Some(3), b"ET 0 0 10 10".to_vec()),
            (Some(4), b"re f Q /Im0 Do".to_vec()),
        ]);
        let map = super::super::map_content(c, &fake());
        assert_eq!(
            map.objects.iter().map(|o| o.kind).collect::<Vec<_>>(),
            [ObjKind::Text, ObjKind::Path, ObjKind::Image]
        );
        assert_eq!(map.objects[1].ctm.e, 5.0);
        assert_eq!(map.objects[2].ctm.e, 0.0);
        let span = map.objects[1].span.clone();
        assert_eq!(&map.content.data[span], b"0 0 10 10 re f");
    }

    #[test]
    fn inline_images() {
        let data = b"q 10 0 0 10 0 0 cm BI /W 2 /H 2 /BPC 8 /CS /G ID abcd EI Q 0 0 1 1 re f";
        let out = run(data);
        assert_eq!(
            out.objects.iter().map(|o| o.kind).collect::<Vec<_>>(),
            [ObjKind::Image, ObjKind::Path]
        );
        assert_eq!(
            bytes(data, &out.objects[0].span),
            "BI /W 2 /H 2 /BPC 8 /CS /G ID abcd EI"
        );
        assert_eq!(
            out.objects[0].detail,
            Detail::Image(ImageDetail {
                inline: true,
                width: 2,
                height: 2
            })
        );
        // 数据里出现 `EI`：按宽高算出的长度跳过，不会提前结束。
        let data = b"BI /W 2 /H 2 /BPC 8 /CS /G ID  EI  EI 0 0 1 1 re f";
        assert_eq!(kinds(data), [ObjKind::Image, ObjKind::Path]);
        // 字典里出现不是 ID 的关键字：`BI` 作废，后面照常解释。
        assert_eq!(kinds(b"BI /W 2 q 0 0 1 1 re f Q"), [ObjKind::Path]);
        // 定不了界的过滤器。
        let out = run(b"BI /W 2 /H 2 /BPC 8 /CS /RGB /F /DCT ID xxxx EI");
        assert_eq!(
            out.blocked,
            Some(Unmatched::InlineImageFilter("DCTDecode".to_owned()))
        );
    }

    #[test]
    fn fill_history_follows_pdfium_rules() {
        let data = b"0.5 g 1 0 rg 0 0 1 1 re f /CS0 cs 1 0 0 sc 0 0 1 1 re f \
                     /Nope cs 0 1 0 scn 0 0 1 1 re f q 0 g Q 0 0 1 1 re f";
        let out = run(data);
        let history = |i: usize| -> Vec<&str> {
            out.objects[i]
                .fill
                .ops
                .iter()
                .map(|r| bytes(data, r))
                .collect()
        };
        assert_eq!(history(0), ["0.5 g"], "操作数个数不对的 rg 被忽略");
        assert_eq!(history(1), ["/CS0 cs", "1 0 0 sc"]);
        assert_eq!(
            history(2),
            ["/CS0 cs", "1 0 0 sc", "/Nope cs", "0 1 0 scn"],
            "找不到的颜色空间：cs 可能被忽略，接在后面重放"
        );
        assert_eq!(history(3), history(2), "Q 恢复了填充色");
        assert!(!out.objects[3].fill.from_default);
        let out = run(b"0 0 1 1 re f 0.5 sc 0 0 1 1 re f");
        assert!(out.objects[0].fill.ops.is_empty() && out.objects[0].fill.from_default);
        assert!(out.objects[1].fill.from_default);
        assert_eq!(out.objects[1].fill.ops.len(), 1);
    }

    #[test]
    fn shading_groups() {
        // 独占一层 q、剪切是这一层里建立的：可以连剪切一起移动。
        let data = b"q 0 0 100 100 re W n 2 0 0 2 0 0 cm /Sh0 sh Q";
        let out = run(data);
        let Detail::Shading(Some(group)) = &out.objects[0].detail else {
            panic!("应当认出所在的组");
        };
        assert_eq!(group.after_q, 1);
        assert_eq!(group.ctm.a, 1.0);
        assert_eq!(out.objects[0].ctm.a, 2.0);
        // 没有剪切。
        let out = run(b"q /Sh0 sh Q");
        assert_eq!(out.objects[0].detail, Detail::Shading(None));
        // 剪切是外面建立的。
        let out = run(b"0 0 9 9 re W n q 0 0 5 5 re W n /Sh0 sh Q");
        assert_eq!(out.objects[0].detail, Detail::Shading(None));
        // 组里还有别的对象（之前或之后）。
        let out = run(b"q 0 0 9 9 re W n /Sh0 sh 0 0 1 1 re f Q");
        assert_eq!(out.objects[0].detail, Detail::Shading(None));
        let out = run(b"q 0 0 1 1 re f 0 0 9 9 re W n /Sh0 sh Q");
        assert_eq!(out.objects[1].detail, Detail::Shading(None));
        // 不在任何 q 里。
        let out = run(b"0 0 9 9 re W n /Sh0 sh");
        assert_eq!(out.objects[0].detail, Detail::Shading(None));
        // 组关闭之后的对象不影响它。
        let out = run(b"q 0 0 9 9 re W n /Sh0 sh Q 0 0 1 1 re f");
        assert!(matches!(out.objects[0].detail, Detail::Shading(Some(_))));
    }

    #[test]
    fn clips_pdfium_may_drop() {
        let clip_of = |data: &[u8], index: usize| run(data).objects[index].droppable_clip;
        let rect = |left: f64, bottom: f64, right: f64, top: f64| {
            Some(Rect {
                left,
                bottom,
                right,
                top,
            })
        };
        // 没有剪切。
        assert_eq!(clip_of(b"/Im0 Do", 0), None);
        // 一条矩形剪切；范围乘过当时的矩阵。
        assert_eq!(
            clip_of(b"0 0 100 50 re W n /Im0 Do", 0),
            rect(0.0, 0.0, 100.0, 50.0)
        );
        assert_eq!(
            clip_of(b"2 0 0 2 10 10 cm 0 0 100 50 re W n /Im0 Do", 0),
            rect(10.0, 10.0, 210.0, 110.0)
        );
        // 负的宽高、翻转的矩阵：范围照样是规整的。
        assert_eq!(
            clip_of(b"1 0 0 -1 0 100 cm 10 10 20 -5 re W n /Im0 Do", 0),
            rect(10.0, 90.0, 30.0, 95.0)
        );
        // 用 m/l/h 画的矩形也算；三角形（闭合之后 4 个点，全是直线）宁可多报。
        assert_eq!(
            clip_of(b"0 0 m 10 0 l 10 10 l 0 10 l h W n /Im0 Do", 0),
            rect(0.0, 0.0, 10.0, 10.0)
        );
        assert_eq!(
            clip_of(b"0 0 m 10 0 l 5 5 l h W n /Im0 Do", 0),
            rect(0.0, 0.0, 10.0, 5.0)
        );
        // 不到 4 个点、带曲线：肯定不是矩形。
        assert_eq!(clip_of(b"0 0 m 10 0 l 5 5 l W n /Im0 Do", 0), None);
        assert_eq!(
            clip_of(b"0 0 m 10 0 l 10 10 l 5 15 0 10 0 5 c h W n /Im0 Do", 0),
            None
        );
        // 后一条在前一条里面：前一条被合并掉，只剩后一条。
        assert_eq!(
            clip_of(b"0 0 100 100 re W n 10 10 50 50 re W n /Im0 Do", 0),
            rect(10.0, 10.0, 60.0, 60.0)
        );
        // 后一条伸出前一条：两条都在，不会被去掉。
        assert_eq!(
            clip_of(b"0 0 100 100 re W n 50 50 100 100 re W n /Im0 Do", 0),
            None
        );
        // 前一条带曲线（合并不掉），后一条是矩形：两条都在。
        assert_eq!(
            clip_of(
                b"0 0 m 99 0 l 99 99 l 50 150 0 99 0 50 c h W n 10 10 20 20 re W n /Im0 Do",
                0
            ),
            None
        );
        // 路径自己挂着 W：它用的是加这条剪切之前的状态，后面的对象才受它管。
        let out = run(b"0 0 10 10 re W f /Im0 Do");
        assert_eq!(out.objects[0].droppable_clip, None);
        assert_eq!(out.objects[1].droppable_clip, rect(0.0, 0.0, 10.0, 10.0));
        // Q 恢复剪切。
        let out = run(b"q 0 0 10 10 re W n /Im0 Do Q /Im0 Do");
        assert_eq!(out.objects[0].droppable_clip, rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(out.objects[1].droppable_clip, None);
        // 渐变不检查。
        assert_eq!(clip_of(b"0 0 10 10 re W n /Sh0 sh", 0), None);
        // 只有一个点的路径挂着 W：加的是一个空矩形。
        assert_eq!(clip_of(b"5 5 m W n /Im0 Do", 0), rect(0.0, 0.0, 0.0, 0.0));
        // 文字、表单、路径都带着这条信息。
        let out = run(b"0 0 50 50 re W n BT /F1 9 Tf (a) Tj ET /Fm0 Do 1 1 2 2 re f");
        assert!(out.objects.iter().all(|o| o.droppable_clip.is_some()));
    }

    #[test]
    fn more_than_sixteen_operands() {
        // 操作数缓冲只有 16 项，多出来的把最早的挤掉；这一串仍然算同一个操作符的操作数。
        let data = b"1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 /Im0 Do";
        let out = run(data);
        assert_eq!(out.objects.len(), 1);
        assert_eq!(out.objects[0].span, 0..data.len());
        // 18 个数字之后的 `m`：缓冲里是 16 项，不是 2 项，被忽略。
        assert!(kinds(b"1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 m 5 5 l S").is_empty());
    }

    #[test]
    fn broken_input_terminates() {
        for data in [
            &b"("[..],
            b"[ (a",
            b"<< /A",
            b"BI",
            b"BI /W",
            b"BI /W 1 /H 1 ID",
            b"q q q q",
            b"Q Q Q",
            b"1 2 3",
            b"\xff\xfe\x00",
            b"/F1 Tf ) ] > } {",
        ] {
            let _ = run(data);
        }
    }
}
