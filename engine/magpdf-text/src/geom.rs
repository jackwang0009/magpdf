//! 文字块识别用的平面几何类型。与 `pdfium-sys` 解耦：核心算法只认这些类型，
//! 单元测试可以手工构造；从 PDFium 类型的转换放在本文件末尾。

use serde::{Deserialize, Serialize};

/// 页面坐标系里的点，单位为点（1/72 英寸），原点在页面左下角。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 轴对齐矩形，构造后保证 `left <= right`、`bottom <= top`。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

impl Rect {
    /// 由任意两个对角点构造，自动规范化。
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            left: x0.min(x1),
            bottom: y0.min(y1),
            right: x0.max(x1),
            top: y0.max(y1),
        }
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.top - self.bottom
    }

    pub fn center_x(&self) -> f32 {
        (self.left + self.right) * 0.5
    }

    pub fn center_y(&self) -> f32 {
        (self.bottom + self.top) * 0.5
    }

    /// 宽或高小于 `eps` 视为退化（PDFium 给空格之类无轮廓字形的就是这种盒子）。
    pub fn is_degenerate(&self, eps: f32) -> bool {
        self.width() < eps || self.height() < eps
    }

    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            left: self.left.min(other.left),
            bottom: self.bottom.min(other.bottom),
            right: self.right.max(other.right),
            top: self.top.max(other.top),
        }
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.left < other.right
            && other.left < self.right
            && self.bottom < other.top
            && other.bottom < self.top
    }
}

/// 文字基线方向所在的象限：0°、90°、180°、270°。
///
/// 这四种方向之间只差 90° 的整数倍旋转，矩形转过去还是矩形，所以行、段聚类统一在
/// "基线向右、字头向上"的规范坐标系（u 沿基线，v 垂直基线）里做，结果再转回页面坐标。
/// 其他角度的文字不进这个流程（方案 5.5：竖排与任意旋转在 P4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Quadrant {
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl Quadrant {
    /// 角度（度，逆时针，任意范围）离某个象限方向不超过 `tol` 时归入该象限。
    pub fn from_angle(angle_deg: f32, tol: f32) -> Option<Self> {
        let a = angle_deg.rem_euclid(360.0);
        let near = |target: f32| {
            let d = (a - target).abs();
            d <= tol || (360.0 - d) <= tol
        };
        if near(0.0) {
            Some(Self::Deg0)
        } else if near(90.0) {
            Some(Self::Deg90)
        } else if near(180.0) {
            Some(Self::Deg180)
        } else if near(270.0) {
            Some(Self::Deg270)
        } else {
            None
        }
    }

    pub fn degrees(self) -> f32 {
        match self {
            Self::Deg0 => 0.0,
            Self::Deg90 => 90.0,
            Self::Deg180 => 180.0,
            Self::Deg270 => 270.0,
        }
    }

    /// 页面坐标 → 规范坐标（绕原点旋转 −θ）。
    pub fn to_canonical(self, p: Point) -> Point {
        match self {
            Self::Deg0 => p,
            Self::Deg90 => Point::new(p.y, -p.x),
            Self::Deg180 => Point::new(-p.x, -p.y),
            Self::Deg270 => Point::new(-p.y, p.x),
        }
    }

    /// 规范坐标 → 页面坐标（[`to_canonical`](Self::to_canonical) 的逆）。
    pub fn from_canonical(self, p: Point) -> Point {
        match self {
            Self::Deg0 => p,
            Self::Deg90 => Point::new(-p.y, p.x),
            Self::Deg180 => Point::new(-p.x, -p.y),
            Self::Deg270 => Point::new(p.y, -p.x),
        }
    }

    pub fn rect_to_canonical(self, r: &Rect) -> Rect {
        let a = self.to_canonical(Point::new(r.left, r.bottom));
        let b = self.to_canonical(Point::new(r.right, r.top));
        Rect::new(a.x, a.y, b.x, b.y)
    }

    pub fn rect_from_canonical(self, r: &Rect) -> Rect {
        let a = self.from_canonical(Point::new(r.left, r.bottom));
        let b = self.from_canonical(Point::new(r.right, r.top));
        Rect::new(a.x, a.y, b.x, b.y)
    }
}

impl From<pdfium_sys::Rect> for Rect {
    fn from(r: pdfium_sys::Rect) -> Self {
        Rect::new(r.left, r.bottom, r.right, r.top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_normalizes_and_unions() {
        let r = Rect::new(10.0, 20.0, 0.0, 5.0);
        assert_eq!((r.left, r.bottom, r.right, r.top), (0.0, 5.0, 10.0, 20.0));
        let u = r.union(&Rect::new(-1.0, 0.0, 3.0, 30.0));
        assert_eq!((u.left, u.bottom, u.right, u.top), (-1.0, 0.0, 10.0, 30.0));
        assert!(Rect::new(0.0, 0.0, 5.0, 0.001).is_degenerate(0.01));
        assert!(r.intersects(&Rect::new(5.0, 10.0, 50.0, 50.0)));
        assert!(
            !r.intersects(&Rect::new(10.0, 10.0, 50.0, 50.0)),
            "只碰到边不算相交"
        );
    }

    #[test]
    fn quadrant_from_angle_with_tolerance() {
        assert_eq!(Quadrant::from_angle(0.4, 1.0), Some(Quadrant::Deg0));
        assert_eq!(Quadrant::from_angle(-0.4, 1.0), Some(Quadrant::Deg0));
        assert_eq!(Quadrant::from_angle(359.5, 1.0), Some(Quadrant::Deg0));
        assert_eq!(Quadrant::from_angle(90.0, 1.0), Some(Quadrant::Deg90));
        assert_eq!(Quadrant::from_angle(-90.0, 1.0), Some(Quadrant::Deg270));
        assert_eq!(Quadrant::from_angle(180.9, 1.0), Some(Quadrant::Deg180));
        assert_eq!(Quadrant::from_angle(30.0, 1.0), None);
        assert_eq!(Quadrant::from_angle(2.0, 1.0), None);
    }

    #[test]
    fn canonical_round_trip_and_direction() {
        for q in [
            Quadrant::Deg0,
            Quadrant::Deg90,
            Quadrant::Deg180,
            Quadrant::Deg270,
        ] {
            let p = Point::new(12.5, -7.25);
            let back = q.from_canonical(q.to_canonical(p));
            assert!(
                (back.x - p.x).abs() < 1e-5 && (back.y - p.y).abs() < 1e-5,
                "{q:?}"
            );
            // 文字方向向量转到规范坐标后应指向 +u。
            let (dx, dy) = q.degrees().to_radians().sin_cos();
            let dir = q.to_canonical(Point::new(dy, dx));
            assert!(
                (dir.x - 1.0).abs() < 1e-5 && dir.y.abs() < 1e-5,
                "{q:?}: {dir:?}"
            );
            let r = Rect::new(1.0, 2.0, 4.0, 8.0);
            let rr = q.rect_from_canonical(&q.rect_to_canonical(&r));
            assert!((rr.left - r.left).abs() < 1e-5 && (rr.top - r.top).abs() < 1e-5);
        }
    }
}
