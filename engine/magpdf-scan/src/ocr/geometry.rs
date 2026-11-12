//! 几何工具：凸包、最小外接矩形、多边形面积与交并比。
//!
//! DB 后处理（`det`）和评测（`eval`）共用。只处理像素坐标系（x 向右、y 向下）的小多边形，
//! 全部用 `f32`，不引入 OpenCV 之类的绑定（方案 3.4 的 OpenCV 用于其他图像处理，见 V09 报告 1.2）。

use serde::{Deserialize, Serialize};

/// 二维点，像素坐标。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }

    fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y
    }

    fn norm(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn distance(self, o: Self) -> f32 {
        self.sub(o).norm()
    }
}

/// 四点框，顺序为左上、右上、右下、左下（与 PaddleOCR 的 `get_mini_boxes` 一致）。
pub type Quad = [Point; 4];

fn cross(o: Point, a: Point, b: Point) -> f32 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

/// 凸包（Andrew 单调链），返回逆时针（在 y 向下的坐标系里看是顺时针）的顶点，不含重复点。
/// 点数少于 3 时原样返回去重结果。
pub fn convex_hull(points: &[Point]) -> Vec<Point> {
    let mut pts: Vec<Point> = points.to_vec();
    pts.sort_by(|a, b| {
        a.x.partial_cmp(&b.x)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
    });
    pts.dedup_by(|a, b| a.x == b.x && a.y == b.y);
    if pts.len() < 3 {
        return pts;
    }
    let mut hull: Vec<Point> = Vec::with_capacity(pts.len() * 2);
    for &p in &pts {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    let lower_len = hull.len() + 1;
    for &p in pts.iter().rev().skip(1) {
        while hull.len() >= lower_len && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
        {
            hull.pop();
        }
        hull.push(p);
    }
    hull.pop();
    hull
}

/// 最小面积外接矩形（旋转卡壳：最小矩形必有一边与凸包某边共线）。
/// 返回的四点已按 [`order_quad`] 排序。点数不足或退化（共线）时返回 `None`。
pub fn min_area_rect(points: &[Point]) -> Option<Quad> {
    let hull = convex_hull(points);
    if hull.len() < 3 {
        return None;
    }
    let mut best: Option<(f32, Quad)> = None;
    for i in 0..hull.len() {
        let a = hull[i];
        let b = hull[(i + 1) % hull.len()];
        let edge = b.sub(a);
        let len = edge.norm();
        if len <= f32::EPSILON {
            continue;
        }
        let u = Point::new(edge.x / len, edge.y / len);
        let v = Point::new(-u.y, u.x);
        let (mut min_u, mut max_u, mut min_v, mut max_v) = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );
        for &p in &hull {
            let pu = p.dot(u);
            let pv = p.dot(v);
            min_u = min_u.min(pu);
            max_u = max_u.max(pu);
            min_v = min_v.min(pv);
            max_v = max_v.max(pv);
        }
        let area = (max_u - min_u) * (max_v - min_v);
        if best.as_ref().is_none_or(|(best_area, _)| area < *best_area) {
            let corner = |cu: f32, cv: f32| Point::new(u.x * cu + v.x * cv, u.y * cu + v.y * cv);
            let quad = [
                corner(min_u, min_v),
                corner(max_u, min_v),
                corner(max_u, max_v),
                corner(min_u, max_v),
            ];
            best = Some((area, quad));
        }
    }
    best.map(|(_, q)| order_quad(q))
}

/// 按 PaddleOCR `get_mini_boxes` 的规则排序：先按 x 排序，左边两点中 y 小的是左上，右边两点中 y 小的是右上。
pub fn order_quad(quad: Quad) -> Quad {
    let mut pts = quad;
    pts.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    let (tl, bl) = if pts[1].y > pts[0].y {
        (pts[0], pts[1])
    } else {
        (pts[1], pts[0])
    };
    let (tr, br) = if pts[3].y > pts[2].y {
        (pts[2], pts[3])
    } else {
        (pts[3], pts[2])
    };
    [tl, tr, br, bl]
}

/// 多边形面积（鞋带公式，取绝对值）。
pub fn polygon_area(poly: &[Point]) -> f32 {
    if poly.len() < 3 {
        return 0.0;
    }
    let mut s = 0.0;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        s += a.x * b.y - b.x * a.y;
    }
    (s * 0.5).abs()
}

/// 多边形周长。
pub fn polygon_perimeter(poly: &[Point]) -> f32 {
    let mut s = 0.0;
    for i in 0..poly.len() {
        s += poly[i].distance(poly[(i + 1) % poly.len()]);
    }
    s
}

/// 矩形四点框的两条边长 `(宽, 高)`：宽取左上→右上，高取左上→左下。
pub fn quad_sides(quad: &Quad) -> (f32, f32) {
    (quad[0].distance(quad[1]), quad[0].distance(quad[3]))
}

/// 把矩形框向外扩 `d` 像素（DB 的 unclip 对矩形做偏移再取最小外接矩形，结果等价于长宽各加 `2d`）。
pub fn expand_quad(quad: &Quad, d: f32) -> Quad {
    let (a, b) = quad_sides(quad);
    let center = Point::new((quad[0].x + quad[2].x) * 0.5, (quad[0].y + quad[2].y) * 0.5);
    let unit = |from: Point, to: Point, len: f32| {
        if len <= f32::EPSILON {
            Point::new(0.0, 0.0)
        } else {
            Point::new((to.x - from.x) / len, (to.y - from.y) / len)
        }
    };
    let u = unit(quad[0], quad[1], a);
    let v = unit(quad[0], quad[3], b);
    let (ha, hb) = (a * 0.5 + d, b * 0.5 + d);
    let corner = |su: f32, sv: f32| {
        Point::new(
            center.x + u.x * su * ha + v.x * sv * hb,
            center.y + u.y * su * ha + v.y * sv * hb,
        )
    };
    order_quad([
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ])
}

/// 包围盒 `(min_x, min_y, max_x, max_y)`。
pub fn bounding_box(poly: &[Point]) -> (f32, f32, f32, f32) {
    let mut b = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for p in poly {
        b.0 = b.0.min(p.x);
        b.1 = b.1.min(p.y);
        b.2 = b.2.max(p.x);
        b.3 = b.3.max(p.y);
    }
    b
}

/// 射线法判断点是否在多边形内（边界上的点按内处理的近似）。
pub fn point_in_polygon(p: Point, poly: &[Point]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (pi, pj) = (poly[i], poly[j]);
        if (pi.y > p.y) != (pj.y > p.y) {
            let x = (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y) + pi.x;
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Sutherland–Hodgman：用凸多边形 `clip` 裁剪多边形 `subject`，返回交集多边形。
pub fn clip_polygon(subject: &[Point], clip: &[Point]) -> Vec<Point> {
    if subject.len() < 3 || clip.len() < 3 {
        return Vec::new();
    }
    // clip 的方向决定“内侧”的符号，先算一次有向面积。
    let mut signed = 0.0;
    for i in 0..clip.len() {
        let a = clip[i];
        let b = clip[(i + 1) % clip.len()];
        signed += a.x * b.y - b.x * a.y;
    }
    let sign = if signed >= 0.0 { 1.0 } else { -1.0 };
    let inside = |p: Point, a: Point, b: Point| cross(a, b, p) * sign >= 0.0;
    let intersect = |s: Point, e: Point, a: Point, b: Point| {
        let d1 = e.sub(s);
        let d2 = b.sub(a);
        let denom = d1.x * d2.y - d1.y * d2.x;
        if denom.abs() <= f32::EPSILON {
            return e;
        }
        let t = ((a.x - s.x) * d2.y - (a.y - s.y) * d2.x) / denom;
        Point::new(s.x + d1.x * t, s.y + d1.y * t)
    };

    let mut output: Vec<Point> = subject.to_vec();
    for i in 0..clip.len() {
        let a = clip[i];
        let b = clip[(i + 1) % clip.len()];
        let input = std::mem::take(&mut output);
        if input.is_empty() {
            break;
        }
        let mut s = input[input.len() - 1];
        for &e in &input {
            if inside(e, a, b) {
                if !inside(s, a, b) {
                    output.push(intersect(s, e, a, b));
                }
                output.push(e);
            } else if inside(s, a, b) {
                output.push(intersect(s, e, a, b));
            }
            s = e;
        }
    }
    output
}

/// 两个多边形的交并比。为了稳健，先各取凸包再求交（文字行的标注和检测框基本都是凸四边形）。
pub fn polygon_iou(a: &[Point], b: &[Point]) -> f32 {
    let ha = convex_hull(a);
    let hb = convex_hull(b);
    let (area_a, area_b) = (polygon_area(&ha), polygon_area(&hb));
    if area_a <= 0.0 || area_b <= 0.0 {
        return 0.0;
    }
    let inter = polygon_area(&clip_polygon(&ha, &hb));
    let union = area_a + area_b - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn hull_of_square_with_interior_points() {
        let pts = [
            p(0., 0.),
            p(4., 0.),
            p(4., 4.),
            p(0., 4.),
            p(2., 2.),
            p(1., 3.),
        ];
        let hull = convex_hull(&pts);
        assert_eq!(hull.len(), 4);
        assert!((polygon_area(&hull) - 16.0).abs() < 1e-4);
    }

    #[test]
    fn min_area_rect_of_axis_aligned_points() {
        let pts = [p(1., 1.), p(9., 1.), p(9., 3.), p(1., 3.), p(5., 2.)];
        let q = min_area_rect(&pts).unwrap();
        assert!((polygon_area(&q) - 16.0).abs() < 1e-3, "{q:?}");
        assert!(
            (q[0].x - 1.0).abs() < 1e-3 && (q[0].y - 1.0).abs() < 1e-3,
            "{q:?}"
        );
        assert!(
            (q[2].x - 9.0).abs() < 1e-3 && (q[2].y - 3.0).abs() < 1e-3,
            "{q:?}"
        );
    }

    #[test]
    fn min_area_rect_of_rotated_rectangle() {
        // 边长 10×2，旋转 30°。
        let (c, s) = (30f32.to_radians().cos(), 30f32.to_radians().sin());
        let rot = |x: f32, y: f32| p(x * c - y * s + 50.0, x * s + y * c + 50.0);
        let pts: Vec<Point> = (0..=10)
            .flat_map(|i| [rot(i as f32, 0.0), rot(i as f32, 2.0)])
            .collect();
        let q = min_area_rect(&pts).unwrap();
        assert!((polygon_area(&q) - 20.0).abs() < 1e-2, "{q:?}");
        let (a, b) = quad_sides(&q);
        assert!((a.max(b) - 10.0).abs() < 1e-2 && (a.min(b) - 2.0).abs() < 1e-2);
    }

    #[test]
    fn expand_grows_each_side_by_2d() {
        let q = [p(0., 0.), p(10., 0.), p(10., 4.), p(0., 4.)];
        let e = expand_quad(&q, 1.5);
        let (a, b) = quad_sides(&e);
        assert!((a - 13.0).abs() < 1e-4 && (b - 7.0).abs() < 1e-4, "{e:?}");
        assert!((e[0].x + 1.5).abs() < 1e-4 && (e[0].y + 1.5).abs() < 1e-4);
    }

    #[test]
    fn iou_of_overlapping_squares() {
        let a = [p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
        let b = [p(5., 0.), p(15., 0.), p(15., 10.), p(5., 10.)];
        let iou = polygon_iou(&a, &b);
        assert!((iou - 50.0 / 150.0).abs() < 1e-4, "{iou}");
        assert!((polygon_iou(&a, &a) - 1.0).abs() < 1e-5);
        let far = [p(20., 20.), p(30., 20.), p(30., 30.), p(20., 30.)];
        assert_eq!(polygon_iou(&a, &far), 0.0);
    }

    #[test]
    fn iou_is_orientation_independent() {
        let a = [p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
        let b_cw = [p(0., 0.), p(0., 10.), p(10., 10.), p(10., 0.)];
        assert!((polygon_iou(&a, &b_cw) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn point_in_polygon_basic() {
        let a = [p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
        assert!(point_in_polygon(p(5., 5.), &a));
        assert!(!point_in_polygon(p(15., 5.), &a));
    }

    #[test]
    fn order_quad_matches_paddleocr_rule() {
        let q = order_quad([p(10., 0.), p(0., 5.), p(10., 5.), p(0., 0.)]);
        assert_eq!(q, [p(0., 0.), p(10., 0.), p(10., 5.), p(0., 5.)]);
    }
}
