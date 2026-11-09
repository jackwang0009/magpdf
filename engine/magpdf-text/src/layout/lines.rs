//! 行聚类：字形 → 行（规范坐标系）。

use super::{CanonGlyph, LayoutParams, LineBuild, mode_f32, mode_u32};
use crate::geom::{Point, Quadrant, Rect};
use crate::glyph::{Glyph, PageText};

/// 把一页字形聚成行。返回的行按象限、再按 v 降序排列；非象限方向的字形按文字对象各成一行。
pub(super) fn cluster_lines(page: &PageText, params: &LayoutParams) -> Vec<LineBuild> {
    let mut by_quadrant: [Vec<CanonGlyph>; 4] = Default::default();
    let mut others: Vec<(usize, f32)> = Vec::new();

    for (index, g) in page.glyphs.iter().enumerate() {
        if g.generated {
            continue;
        }
        match Quadrant::from_angle(g.angle, params.angle_tol_deg) {
            Some(q) => by_quadrant[q as usize].push(canon_glyph(index, g, q)),
            None => others.push((index, g.angle)),
        }
    }

    let mut out = Vec::new();
    for (qi, glyphs) in by_quadrant.into_iter().enumerate() {
        if glyphs.is_empty() {
            continue;
        }
        let quadrant = [
            Quadrant::Deg0,
            Quadrant::Deg90,
            Quadrant::Deg180,
            Quadrant::Deg270,
        ][qi];
        let vertical_rulings = vertical_rulings(page, quadrant, params);
        let mut lines = sweep(glyphs, &vertical_rulings, params);
        merge_satellites(&mut lines, params);
        lines.sort_by(|a, b| b.v.total_cmp(&a.v).then(a.u0.total_cmp(&b.u0)));
        out.extend(lines);
    }
    out.extend(other_angle_lines(page, others));
    out
}

fn canon_glyph(index: usize, g: &Glyph, q: Quadrant) -> CanonGlyph {
    let bbox = q.rect_to_canonical(&g.bbox);
    let origin = q.to_canonical(g.origin);
    let (vb, vt) = g.vertical_extent();
    // vertical_extent 是按页面 y 算的，对非 0° 象限要转到规范坐标：
    // 用字号在基线两侧重新估计，再并上紧包围盒的 v 范围。
    let (v0, v1) = if q == Quadrant::Deg0 {
        (vb, vt)
    } else {
        let est = (origin.y - 0.25 * g.font_size, origin.y + 0.75 * g.font_size);
        if g.bbox.is_degenerate(1e-3) {
            est
        } else {
            (est.0.min(bbox.bottom), est.1.max(bbox.top))
        }
    };
    // 退化的紧包围盒（空格）宽度仍可信（PDFium 用字符进距补足），高度不用。
    let (u0, u1) = if bbox.width() > 1e-3 {
        (bbox.left, bbox.right)
    } else {
        (origin.x, origin.x)
    };
    CanonGlyph {
        index,
        quadrant: q,
        u0,
        u1,
        v: origin.y,
        v0,
        v1,
        size: g.font_size.max(1e-3),
        font_id: g.font_id,
        text_object: g.text_object,
    }
}

/// 规范坐标系里"竖直"（与基线垂直）的细线，按 u 中心排序。
fn vertical_rulings(page: &PageText, q: Quadrant, params: &LayoutParams) -> Vec<Rect> {
    let mut v: Vec<Rect> = page
        .rulings
        .iter()
        .map(|r| q.rect_to_canonical(r))
        .filter(|r| r.width() <= params.ruling_max_thickness && r.height() > r.width())
        .collect();
    v.sort_by(|a, b| a.center_x().total_cmp(&b.center_x()));
    v
}

/// 是否有竖直细线穿过基线 `v`、且位于开区间 `(lo, hi)` 内。
fn ruling_between(rulings: &[Rect], lo: f32, hi: f32, v: f32) -> bool {
    if lo >= hi || rulings.is_empty() {
        return false;
    }
    let start = rulings.partition_point(|r| r.center_x() <= lo);
    rulings[start..]
        .iter()
        .take_while(|r| r.center_x() < hi)
        .any(|r| r.bottom <= v && v <= r.top)
}

struct OpenLine {
    glyphs: Vec<CanonGlyph>,
    v_sum: f64,
    u0: f32,
    u1: f32,
    size_min: f32,
    size_max: f32,
    objects: Vec<usize>,
}

impl OpenLine {
    fn new(g: CanonGlyph) -> Self {
        Self {
            v_sum: f64::from(g.v),
            u0: g.u0,
            u1: g.u1,
            size_min: g.size,
            size_max: g.size,
            objects: g.text_object.into_iter().collect(),
            glyphs: vec![g],
        }
    }

    fn v(&self) -> f32 {
        (self.v_sum / self.glyphs.len() as f64) as f32
    }

    fn push(&mut self, g: CanonGlyph) {
        self.v_sum += f64::from(g.v);
        self.u0 = self.u0.min(g.u0);
        self.u1 = self.u1.max(g.u1);
        self.size_min = self.size_min.min(g.size);
        self.size_max = self.size_max.max(g.size);
        if let Some(o) = g.text_object
            && !self.objects.contains(&o)
        {
            self.objects.push(o);
        }
        self.glyphs.push(g);
    }

    /// 字形到行现有范围的水平间距；重叠为 0。
    fn gap_to(&self, g: &CanonGlyph) -> f32 {
        if g.u0 >= self.u1 {
            g.u0 - self.u1
        } else if g.u1 <= self.u0 {
            self.u0 - g.u1
        } else {
            0.0
        }
    }

    fn same_object(&self, g: &CanonGlyph) -> bool {
        g.text_object.is_some_and(|o| self.objects.contains(&o))
    }
}

/// 按 v 降序扫描：每个字形归入最近的一条兼容的开放行，否则新开一行。
/// 开放行只保留基线仍可能被后续字形触及的那些，所以每个字形只和少数几行比较。
fn sweep(mut glyphs: Vec<CanonGlyph>, rulings: &[Rect], params: &LayoutParams) -> Vec<LineBuild> {
    glyphs.sort_by(|a, b| b.v.total_cmp(&a.v).then(a.u0.total_cmp(&b.u0)));
    let mut open: Vec<OpenLine> = Vec::new();
    let mut done: Vec<LineBuild> = Vec::new();

    for g in glyphs {
        // 关闭再也够不着的行：后面字形的 v 只会更小。
        let mut i = 0;
        while i < open.len() {
            let line = &open[i];
            if line.v() - g.v > params.baseline_tol_em * line.size_max {
                let closed = open.swap_remove(i);
                done.push(finish(closed));
            } else {
                i += 1;
            }
        }

        let mut best: Option<(usize, f32)> = None;
        for (i, line) in open.iter().enumerate() {
            let size_ref = line.size_max.min(g.size);
            if (line.v() - g.v).abs() > params.baseline_tol_em * size_ref {
                continue;
            }
            let ratio = line.size_max.max(g.size) / line.size_min.min(g.size);
            if ratio > params.max_size_ratio_in_line {
                continue;
            }
            let gap = line.gap_to(&g);
            let limit_em = if line.same_object(&g) {
                params.same_object_gap_em
            } else {
                params.max_word_gap_em
            };
            if gap > limit_em * size_ref {
                continue;
            }
            let (lo, hi) = if g.u0 >= line.u1 {
                (line.u1, g.u0)
            } else {
                (g.u1, line.u0)
            };
            if ruling_between(rulings, lo, hi, g.v) {
                continue;
            }
            if best.is_none_or(|(_, best_gap)| gap < best_gap) {
                best = Some((i, gap));
            }
        }
        match best {
            Some((i, _)) => open[i].push(g),
            None => open.push(OpenLine::new(g)),
        }
    }
    done.extend(open.into_iter().map(finish));
    done
}

fn finish(mut line: OpenLine) -> LineBuild {
    line.glyphs
        .sort_by(|a, b| a.u0.total_cmp(&b.u0).then(a.u1.total_cmp(&b.u1)));
    build_line(line.glyphs, None)
}

fn build_line(glyphs: Vec<CanonGlyph>, other_angle: Option<f32>) -> LineBuild {
    debug_assert!(!glyphs.is_empty());
    let quadrant = glyphs.first().map_or(Quadrant::Deg0, |g| g.quadrant);
    let mut sizes: Vec<f32> = glyphs.iter().map(|g| g.size).collect();
    let size = mode_f32(&mut sizes, 0.1);
    let fonts: Vec<u32> = glyphs.iter().map(|g| g.font_id).collect();
    let font_id = mode_u32(&fonts);
    let uniform_font = fonts.iter().all(|&f| f == font_id);
    let mut vs: Vec<f32> = glyphs.iter().map(|g| g.v).collect();
    let v = super::median_f32(&mut vs);
    let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for g in &glyphs {
        u0 = u0.min(g.u0);
        u1 = u1.max(g.u1);
        v0 = v0.min(g.v0);
        v1 = v1.max(g.v1);
    }
    LineBuild {
        quadrant,
        other_angle,
        glyphs,
        u0,
        u1,
        v,
        v0,
        v1,
        size,
        font_id,
        uniform_font,
    }
}

/// 把上下标之类的小片段并入主行。
fn merge_satellites(lines: &mut Vec<LineBuild>, params: &LayoutParams) {
    if lines.len() < 2 {
        return;
    }
    lines.sort_by(|a, b| b.v.total_cmp(&a.v));
    let mut merged_into: Vec<Option<usize>> = vec![None; lines.len()];
    for s in 0..lines.len() {
        if lines[s].glyphs.len() > params.satellite_max_glyphs {
            continue;
        }
        let sat = &lines[s];
        let sat_h = (sat.v1 - sat.v0).max(1e-3);
        let mut best: Option<(usize, f32)> = None;
        let mut consider = |h: usize, lines: &[LineBuild]| {
            if h == s || merged_into[h].is_some() {
                return;
            }
            let host = &lines[h];
            if sat.size > params.satellite_size_ratio * host.size {
                return;
            }
            let dist = (sat.v - host.v).abs();
            if dist > host.size {
                return;
            }
            let overlap = sat.v1.min(host.v1) - sat.v0.max(host.v0);
            if overlap < 0.5 * sat_h {
                return;
            }
            let slack = 0.5 * host.size;
            let inside = sat.u0 >= host.u0 - slack && sat.u1 <= host.u1 + slack;
            if !inside {
                return;
            }
            if best.is_none_or(|(_, d)| dist < d) {
                best = Some((h, dist));
            }
        };
        // 宿主字号至少是卫星的 1/satellite_size_ratio 倍，基线距不超过宿主字号：
        // 行已按 v 降序，向两侧扫到基线距超出 3 倍卫星字号为止即可覆盖所有候选。
        let window = 3.0 * sat.size.max(1.0);
        let mut h = s;
        while h > 0 && lines[h - 1].v - sat.v <= window {
            h -= 1;
            consider(h, lines);
        }
        let mut h = s + 1;
        while h < lines.len() && sat.v - lines[h].v <= window {
            consider(h, lines);
            h += 1;
        }
        if let Some((h, _)) = best {
            merged_into[s] = Some(h);
        }
    }
    if merged_into.iter().all(Option::is_none) {
        return;
    }
    // 先把卫星的字形挪到最终宿主（宿主自己也可能被并入别的行，沿链找到底），再重建受影响的行。
    let mut moved: Vec<Vec<CanonGlyph>> = vec![Vec::new(); lines.len()];
    for s in 0..lines.len() {
        let Some(mut target) = merged_into[s] else {
            continue;
        };
        let mut hops = 0;
        while let Some(next) = merged_into[target]
            && hops < lines.len()
        {
            target = next;
            hops += 1;
        }
        let gl = std::mem::take(&mut lines[s].glyphs);
        moved[target].extend(gl);
    }
    let old = std::mem::take(lines);
    for (i, mut line) in old.into_iter().enumerate() {
        if merged_into[i].is_some() {
            continue;
        }
        if !moved[i].is_empty() {
            line.glyphs.append(&mut moved[i]);
            line.glyphs
                .sort_by(|a, b| a.u0.total_cmp(&b.u0).then(a.u1.total_cmp(&b.u1)));
            line = build_line(line.glyphs, line.other_angle);
        }
        lines.push(line);
    }
}

/// 非象限方向的字形：同一文字对象、角度相近的连续字形为一行，各自成块（标为非水平）。
fn other_angle_lines(page: &PageText, others: Vec<(usize, f32)>) -> Vec<LineBuild> {
    let mut out = Vec::new();
    let mut current: Vec<(usize, f32)> = Vec::new();
    let flush = |current: &mut Vec<(usize, f32)>, out: &mut Vec<LineBuild>| {
        if current.is_empty() {
            return;
        }
        let angle = current[0].1;
        // 用角度 θ 把字形转到自己的坐标系里算范围：沿基线为 u，垂直为 v。
        let (sin, cos) = (-angle).to_radians().sin_cos();
        let rot = |p: Point| Point::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos);
        let glyphs: Vec<CanonGlyph> = current
            .iter()
            .map(|&(index, _)| {
                let g = &page.glyphs[index];
                let o = rot(g.origin);
                let corners = [
                    rot(Point::new(g.bbox.left, g.bbox.bottom)),
                    rot(Point::new(g.bbox.right, g.bbox.bottom)),
                    rot(Point::new(g.bbox.left, g.bbox.top)),
                    rot(Point::new(g.bbox.right, g.bbox.top)),
                ];
                let u0 = corners.iter().map(|p| p.x).fold(f32::MAX, f32::min);
                let u1 = corners.iter().map(|p| p.x).fold(f32::MIN, f32::max);
                CanonGlyph {
                    index,
                    quadrant: Quadrant::Deg0,
                    u0,
                    u1,
                    v: o.y,
                    v0: o.y - 0.25 * g.font_size,
                    v1: o.y + 0.75 * g.font_size,
                    size: g.font_size.max(1e-3),
                    font_id: g.font_id,
                    text_object: g.text_object,
                }
            })
            .collect();
        let mut glyphs = glyphs;
        glyphs.sort_by(|a, b| a.u0.total_cmp(&b.u0));
        out.push(build_line(glyphs, Some(angle)));
        current.clear();
    };
    for (index, angle) in others {
        let g = &page.glyphs[index];
        let same = current.last().is_some_and(|&(last, a)| {
            (a - angle).abs() <= 1.0
                && page.glyphs[last].text_object.is_some()
                && page.glyphs[last].text_object == g.text_object
        });
        if !same {
            flush(&mut current, &mut out);
        }
        current.push((index, angle));
    }
    flush(&mut current, &mut out);
    out
}
