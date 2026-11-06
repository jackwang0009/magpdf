use crate::raw;

/// PDF 用户空间里的矩形，单位为点（1/72 英寸），原点在页面左下角。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

impl Rect {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.top - self.bottom
    }

    pub(crate) fn from_fs(r: raw::FS_RECTF) -> Self {
        Self {
            left: r.left,
            bottom: r.bottom,
            right: r.right,
            top: r.top,
        }
    }
}

/// 2×3 仿射矩阵，与 PDF 的 `cm` 操作数顺序一致：`[a b c d e f]`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Matrix {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translation(tx: f32, ty: f32) -> Self {
        Self {
            e: tx,
            f: ty,
            ..Self::IDENTITY
        }
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    pub(crate) fn from_fs(m: raw::FS_MATRIX) -> Self {
        Self {
            a: m.a,
            b: m.b,
            c: m.c,
            d: m.d,
            e: m.e,
            f: m.f,
        }
    }

    pub(crate) fn to_fs(self) -> raw::FS_MATRIX {
        raw::FS_MATRIX {
            a: self.a,
            b: self.b,
            c: self.c,
            d: self.d,
            e: self.e,
            f: self.f,
        }
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// 颜色分量 0–255。PDFium 的公共接口只暴露 RGB，这正是方案 2.1 列出的写回缺陷之一。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}
