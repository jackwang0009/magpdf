//! 操作符分类。对照 PDFium `CPDF_StreamContentParser::AddTextObject` 等建对象的调用点，
//! 把内容流操作符分成"路径构造 / 路径绘制 / 文字显示 / XObject / 渐变 / 状态 / 标记内容"几类，
//! 其余的（`d0`、`d1`、`BX`、`EX` 等）归入 `Other`，在页面层不产生对象。

/// 操作符类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfiumOp {
    /// 路径构造：`m l c v y re h`（ClosePath 在构造段内，PDFium 不会单独为它建对象）。
    PathConstruct,
    /// 路径绘制：`S s f F f* B B* b b*`。
    PathPaint(PaintKind),
    /// 只设剪切、不画：`n`（有 `W` 时 PDFium 记录剪切，不建对象）。
    PathEnd,
    /// `W`/`W*`：标记下一个绘制/`n` 是剪切。
    Clip,
    /// 文字显示：`Tj TJ ' "`。
    TextShow,
    /// `BT`/`ET`。
    TextBegin,
    TextEnd,
    TextState,
    /// 文字定位：`Td TD Tm T*`。
    TextPos,
    /// 图形状态：`q Q cm g G rg RG k K cs CS sc SC scn SCN gs w J j M d ri i`。
    GState,
    /// `Do`。
    XObject,
    /// `sh`。
    Shading,
    /// `BI`（内联图像整体是一个记号）。
    InlineImage,
    /// 标记内容：`BMC BDC EMC MP DP`。
    Marked,
    /// 其他（`d0 d1 BX EX` 等），不建对象。
    Other,
}

/// 填充/描边的细分，写回层用它决定改色时改哪个操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintKind {
    Stroke,
    Fill,
    FillStroke,
    /// `b b*`：闭合且填充描边。
    CloseFillStroke,
}

impl PdfiumOp {
    /// 由操作符原文（已解码）分类。不认识的操作符返回 `Other`。
    pub fn classify(op: &[u8]) -> Self {
        match op {
            b"m" | b"l" | b"c" | b"v" | b"y" | b"re" | b"h" => Self::PathConstruct,
            b"S" | b"s" => Self::PathPaint(PaintKind::Stroke),
            b"f" | b"F" | b"f*" => Self::PathPaint(PaintKind::Fill),
            b"B" | b"B*" => Self::PathPaint(PaintKind::FillStroke),
            b"b" | b"b*" => Self::PathPaint(PaintKind::CloseFillStroke),
            b"n" => Self::PathEnd,
            b"W" | b"W*" => Self::Clip,
            b"Tj" | b"TJ" | b"'" | b"\"" => Self::TextShow,
            b"BT" => Self::TextBegin,
            b"ET" => Self::TextEnd,
            b"Tc" | b"Tw" | b"Tz" | b"TL" | b"Tf" | b"Tr" | b"Ts" => Self::TextState,
            b"Td" | b"TD" | b"Tm" | b"T*" => Self::TextPos,
            b"q" | b"Q" | b"cm" | b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" | b"cs" | b"CS"
            | b"sc" | b"SC" | b"scn" | b"SCN" | b"gs" | b"w" | b"J" | b"j" | b"M" | b"d"
            | b"ri" | b"i" => Self::GState,
            b"Do" => Self::XObject,
            b"sh" => Self::Shading,
            b"BI" => Self::InlineImage,
            b"BMC" | b"BDC" | b"EMC" | b"MP" | b"DP" => Self::Marked,
            _ => Self::Other,
        }
    }

    /// 是填充类绘制。
    pub fn fills(self) -> bool {
        matches!(
            self,
            Self::PathPaint(PaintKind::Fill | PaintKind::FillStroke | PaintKind::CloseFillStroke)
        )
    }
}
