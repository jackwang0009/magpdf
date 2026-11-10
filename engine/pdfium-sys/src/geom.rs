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

    /// 点是否落在矩形内（含边界）。左右或上下颠倒时按规范化后的范围判断。
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let (left, right) = ordered(self.left, self.right);
        let (bottom, top) = ordered(self.bottom, self.top);
        x >= left && x <= right && y >= bottom && y <= top
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

    /// 仿射逆。行列式接近 0 时返回 `None`。
    pub fn inverse(self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < 1e-8 {
            return None;
        }
        let inv = 1.0 / det;
        Some(Self {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
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

fn ordered(a: f32, b: f32) -> (f32, f32) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::{Matrix, Rect};

    #[test]
    fn rect_contains_inclusive_and_normalizes() {
        let r = Rect {
            left: 10.0,
            bottom: 20.0,
            right: 0.0,
            top: 5.0,
        };
        assert!(r.contains(0.0, 5.0));
        assert!(r.contains(10.0, 20.0));
        assert!(!r.contains(10.1, 12.0));
    }

    #[test]
    fn translation_inverse_restores_origin() {
        let m = Matrix::translation(12.0, -4.0);
        let inv = m.inverse().expect("平移可逆");
        assert!((inv.e + 12.0).abs() < 1e-5);
        assert!((inv.f - 4.0).abs() < 1e-5);
        let back = m.inverse().map(|i| {
            // 平移再逆平移应回到单位矩阵的平移分量。
            let composed_e = m.a * i.e + m.c * i.f + m.e;
            let composed_f = m.b * i.e + m.d * i.f + m.f;
            (composed_e, composed_f)
        });
        let (e, f) = back.expect("可逆");
        assert!(e.abs() < 1e-4 && f.abs() < 1e-4);
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let m = Matrix {
            a: 0.0,
            d: 0.0,
            ..Matrix::IDENTITY
        };
        assert!(m.inverse().is_none());
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
