//! 块（段落）聚类：行 → 块（规范坐标系），并推断对齐方式与行距。

use super::{
    Alignment, Block, Layout, LayoutParams, Line, LineBuild, LinePitch, median_f32, mode_f32,
    mode_u32, spread,
};
use crate::geom::{Point, Quadrant, Rect};
use crate::glyph::PageText;

/// 把行聚成块并生成最终输出。
pub(super) fn cluster_blocks(
    page: &PageText,
    lines: Vec<LineBuild>,
    params: &LayoutParams,
) -> Layout {
    let mut groups: [Vec<LineBuild>; 4] = Default::default();
    let mut others: Vec<LineBuild> = Vec::new();
    for line in lines {
        match line.other_angle {
            Some(_) => others.push(line),
            None => groups[line.quadrant as usize].push(line),
        }
    }

    // (块内各行, 象限, 非象限角度)
    let mut raw_blocks: Vec<(Vec<LineBuild>, Quadrant, Option<f32>)> = Vec::new();
    for (qi, mut group) in groups.into_iter().enumerate() {
        if group.is_empty() {
            continue;
        }
        let quadrant = [
            Quadrant::Deg0,
            Quadrant::Deg90,
            Quadrant::Deg180,
            Quadrant::Deg270,
        ][qi];
        group.sort_by(|a, b| b.v.total_cmp(&a.v).then(a.u0.total_cmp(&b.u0)));
        let rulings = horizontal_rulings(page, quadrant, params);
        let page_pitch = estimate_page_pitch_em(&group, params);
        let grouped = pass1(page, &group, &rulings, page_pitch, params);
        for members in grouped {
            let lines: Vec<LineBuild> = members.iter().map(|&i| group[i].clone()).collect();
            for part in pass2_split(lines, page, params) {
                raw_blocks.push((part, quadrant, None));
            }
        }
    }
    for line in others {
        let angle = line.other_angle;
        raw_blocks.push((vec![line], Quadrant::Deg0, angle));
    }

    let mut layout = Layout::default();
    let mut built: Vec<(Block, Vec<Line>)> = raw_blocks
        .into_iter()
        .map(|(lines, q, angle)| build_block(page, lines, q, angle, params))
        .collect();
    // 阅读顺序（按显示方向）：端正的文字在前，再按自上而下、自左而右。
    let display = display_quadrant(page);
    let display_angle = |b: &Block| (b.angle - display.degrees()).rem_euclid(360.0);
    built.sort_by(|(a, _), (b, _)| {
        let (ka, kb) = (
            display.rect_to_canonical(&a.bbox),
            display.rect_to_canonical(&b.bbox),
        );
        quadrant_rank(display_angle(a))
            .cmp(&quadrant_rank(display_angle(b)))
            .then(kb.top.total_cmp(&ka.top))
            .then(ka.left.total_cmp(&kb.left))
    });
    for (mut block, lines) in built {
        let start = layout.lines.len();
        block.lines = (start..start + lines.len()).collect();
        layout.lines.extend(lines);
        layout.blocks.push(block);
    }
    layout
}

/// 显示时端正的文字在内容流坐标系里的象限（由页面 `/Rotate` 决定）。
fn display_quadrant(page: &PageText) -> Quadrant {
    match page.rotation % 4 {
        1 => Quadrant::Deg90,
        2 => Quadrant::Deg180,
        3 => Quadrant::Deg270,
        _ => Quadrant::Deg0,
    }
}

fn quadrant_rank(angle: f32) -> u8 {
    match Quadrant::from_angle(angle, 0.01) {
        Some(Quadrant::Deg0) => 0,
        Some(Quadrant::Deg180) => 1,
        Some(Quadrant::Deg90) => 2,
        Some(Quadrant::Deg270) => 3,
        None => 4,
    }
}

/// 规范坐标系里"水平"（与基线平行）的细线，按 v 中心排序。
fn horizontal_rulings(page: &PageText, q: Quadrant, params: &LayoutParams) -> Vec<Rect> {
    let mut v: Vec<Rect> = page
        .rulings
        .iter()
        .map(|r| q.rect_to_canonical(r))
        .filter(|r| r.height() <= params.ruling_max_thickness && r.width() > r.height())
        .collect();
    v.sort_by(|a, b| a.center_y().total_cmp(&b.center_y()));
    v
}

/// 上行 `upper` 与下行 `lower` 之间是否有水平细线把它们隔开。
///
/// 细线必须在上行下缘以下（排除下划线：下划线紧贴基线下方约 0.1em）、下行字头以上，
/// 且横向覆盖两行重叠区域的一半以上。
fn ruling_separates(rulings: &[Rect], upper: &LineBuild, lower: &LineBuild) -> bool {
    if rulings.is_empty() {
        return false;
    }
    let lo = lower.v + 0.7 * lower.size;
    let hi = upper.v - 0.3 * upper.size;
    if lo >= hi {
        return false;
    }
    let o_lo = upper.u0.max(lower.u0);
    let o_hi = upper.u1.min(lower.u1);
    if o_hi <= o_lo {
        return false;
    }
    let start = rulings.partition_point(|r| r.center_y() <= lo);
    rulings[start..]
        .iter()
        .take_while(|r| r.center_y() < hi)
        .any(|r| r.right.min(o_hi) - r.left.max(o_lo) >= 0.5 * (o_hi - o_lo))
}

fn overlap_ratio(a: &LineBuild, b_u0: f32, b_u1: f32) -> f32 {
    let ov = a.u1.min(b_u1) - a.u0.max(b_u0);
    let narrow = a.width().min(b_u1 - b_u0);
    if narrow <= 1e-3 {
        // 退化宽度（单个空格之类）：只要确实落在范围内就算完全重叠。
        if ov >= -1e-3 { 1.0 } else { 0.0 }
    } else {
        ov / narrow
    }
}

/// 页面行距估计：对每行找正下方最近、水平重叠、字号相近的行，取基线距（em）的众数。
fn estimate_page_pitch_em(group: &[LineBuild], params: &LayoutParams) -> Option<f32> {
    let mut samples: Vec<f32> = Vec::new();
    for (i, a) in group.iter().enumerate() {
        let mut best: Option<f32> = None;
        for b in group.iter().skip(i + 1) {
            let d = a.v - b.v;
            let size = a.size.max(b.size);
            if d > params.max_pitch_em * size {
                break;
            }
            if d < params.min_line_gap_em * a.size.min(b.size) {
                continue;
            }
            if a.size.max(b.size) / a.size.min(b.size) > params.block_size_ratio {
                continue;
            }
            if overlap_ratio(a, b.u0, b.u1) < params.block_overlap_ratio {
                continue;
            }
            best = Some(d / size);
            break;
        }
        if let Some(em) = best {
            samples.push(em);
        }
    }
    if samples.len() < 3 {
        return None;
    }
    Some(mode_f32(&mut samples, 0.05))
}

struct OpenBlock {
    members: Vec<usize>,
    u0: f32,
    u1: f32,
    anchor_size: f32,
    gaps: Vec<f32>,
}

/// 第一遍：按重叠、基线距、字号、字体、细线把行归入开放的块。返回每块的行下标（v 降序）。
fn pass1(
    page: &PageText,
    group: &[LineBuild],
    rulings: &[Rect],
    page_pitch_em: Option<f32>,
    params: &LayoutParams,
) -> Vec<Vec<usize>> {
    let font_key = |id: u32| page.font(id).map(|f| f.family_key()).unwrap_or_default();
    let max_size = group.iter().map(|l| l.size).fold(0.0f32, f32::max);
    let first_limit_em = page_pitch_em.map_or(params.first_pitch_em, |p| {
        (p * (1.0 + params.pitch_tolerance)).clamp(1.2, params.max_pitch_em)
    });
    let mut open: Vec<OpenBlock> = Vec::new();
    let mut done: Vec<Vec<usize>> = Vec::new();

    for (li, line) in group.iter().enumerate() {
        // 关闭够不着的块。
        let mut i = 0;
        while i < open.len() {
            let last = &group[open[i].members[open[i].members.len() - 1]];
            if last.v - line.v > params.max_pitch_em * last.size.max(max_size) {
                let closed = open.swap_remove(i);
                done.push(closed.members);
            } else {
                i += 1;
            }
        }

        let mut best: Option<(usize, f32, f32)> = None;
        for (bi, block) in open.iter().enumerate() {
            let last = &group[block.members[block.members.len() - 1]];
            let d = last.v - line.v;
            let size_ref = last.size.max(line.size);
            if d < params.min_line_gap_em * last.size.min(line.size) {
                continue;
            }
            let ov = overlap_ratio(line, block.u0, block.u1);
            if ov < params.block_overlap_ratio {
                continue;
            }
            let size_ratio = block.anchor_size.max(line.size) / block.anchor_size.min(line.size);
            if size_ratio > params.block_size_ratio {
                continue;
            }
            if line.uniform_font
                && last.uniform_font
                && font_key(line.font_id) != font_key(last.font_id)
            {
                continue;
            }
            let limit = if block.gaps.is_empty() {
                first_limit_em * size_ref
            } else {
                let mut gaps = block.gaps.clone();
                let p = median_f32(&mut gaps);
                (p * (1.0 + params.pitch_tolerance)).min(params.max_pitch_em * size_ref)
            };
            if d > limit {
                continue;
            }
            if line.width() > params.width_growth_ratio * (block.u1 - block.u0).max(line.size) {
                continue;
            }
            if ruling_separates(rulings, last, line) {
                continue;
            }
            let better = match best {
                None => true,
                Some((_, bd, bov)) => d < bd - 1e-3 || ((d - bd).abs() <= 1e-3 && ov > bov),
            };
            if better {
                best = Some((bi, d, ov));
            }
        }
        match best {
            Some((bi, d, _)) => {
                let block = &mut open[bi];
                block.members.push(li);
                block.u0 = block.u0.min(line.u0);
                block.u1 = block.u1.max(line.u1);
                block.gaps.push(d);
            }
            None => open.push(OpenBlock {
                members: vec![li],
                u0: line.u0,
                u1: line.u1,
                anchor_size: line.size,
                gaps: Vec::new(),
            }),
        }
    }
    done.extend(open.into_iter().map(|b| b.members));
    done
}

/// 第二遍：在块内按行距跳变、首行缩进、两端对齐的段末短行、列表项标记切分段落。
fn pass2_split(
    lines: Vec<LineBuild>,
    page: &PageText,
    params: &LayoutParams,
) -> Vec<Vec<LineBuild>> {
    let n = lines.len();
    if n < 2 {
        return vec![lines];
    }
    let mut gaps: Vec<f32> = lines.windows(2).map(|w| w[0].v - w[1].v).collect();
    let pitch = median_f32(&mut gaps);
    let left = lines.iter().map(|l| l.u0).fold(f32::MAX, f32::min);
    let right = lines.iter().map(|l| l.u1).fold(f32::MIN, f32::max);
    let mut sizes: Vec<f32> = lines.iter().map(|l| l.size).collect();
    let size = mode_f32(&mut sizes, 0.1).max(1e-3);
    let tol = params.align_tol_em * size;

    let left_frac = lines.iter().filter(|l| l.u0 <= left + tol).count() as f32 / n as f32;
    let right_frac_nonlast = lines[..n - 1]
        .iter()
        .filter(|l| l.u1 >= right - tol)
        .count() as f32
        / (n - 1) as f32;
    let justified = n >= 3 && left_frac >= 0.75 && right_frac_nonlast >= 0.75;
    let centers: Vec<f32> = lines.iter().map(|l| (l.u0 + l.u1) * 0.5).collect();
    let lefts_spread = spread(&lines.iter().map(|l| l.u0).collect::<Vec<_>>());
    let rights_spread = spread(&lines.iter().map(|l| l.u1).collect::<Vec<_>>());
    let centered = spread(&centers) <= tol && lefts_spread > tol;
    let right_aligned = rights_spread <= tol && lefts_spread > tol && !centered;

    let mut breaks: Vec<bool> = vec![false; n]; // breaks[i]：第 i 行开始新段
    for i in 1..n {
        let (prev, cur) = (&lines[i - 1], &lines[i]);
        let d = prev.v - cur.v;
        let s = prev.size.max(cur.size);
        if d > pitch * (1.0 + params.pitch_tolerance) && d - pitch > 0.15 * s {
            breaks[i] = true;
            continue;
        }
        let prev_ragged = prev.u1 < right - params.ragged_em * s;
        if !centered && !right_aligned {
            let indented = cur.u0 - left > params.indent_em * s;
            if (indented && prev_ragged) || (justified && prev_ragged) {
                breaks[i] = true;
                continue;
            }
        }
        if starts_with_list_marker(cur, page) {
            breaks[i] = true;
        }
    }

    let mut out: Vec<Vec<LineBuild>> = Vec::new();
    let mut current: Vec<LineBuild> = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        if breaks[i] && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        current.push(line);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// 行首是否是列表项标记：项目符号，或不超过 3 个字母数字加 `.`/`)`/`、`/`．`，后面跟着空格或间隙。
fn starts_with_list_marker(line: &LineBuild, page: &PageText) -> bool {
    let chars: Vec<(u32, &super::CanonGlyph)> = line
        .glyphs
        .iter()
        .take(6)
        .map(|g| (page.glyphs[g.index].unicode, g))
        .collect();
    let Some(&(first, _)) = chars.first() else {
        return false;
    };
    let gap_after = |k: usize| -> bool {
        match (chars.get(k), chars.get(k + 1)) {
            (Some((_, a)), Some((u, b))) => {
                *u == 0x20 || *u == 0x3000 || b.u0 - a.u1 >= 0.3 * line.size
            }
            _ => false,
        }
    };
    const BULLETS: [u32; 16] = [
        0x2022, 0x00B7, 0x25E6, 0x25AA, 0x25A0, 0x25A1, 0x2013, 0x2014, 0x25BA, 0x25B6, 0x2713,
        0x2610, 0x2611, 0x27A2, 0x002D, 0x002A,
    ];
    if BULLETS.contains(&first) {
        return gap_after(0);
    }
    // 编号：1. / 12) / a. / 一、 / (1)
    let is_alnum = |u: u32| char::from_u32(u).is_some_and(|c| c.is_alphanumeric());
    let is_cjk_numeral = |u: u32| "一二三四五六七八九十".chars().any(|c| u32::from(c) == u);
    let mut k = 0;
    let paren = first == u32::from('(') || first == 0xFF08;
    if paren {
        k = 1;
    }
    let start = k;
    while k < chars.len() && k - start < 3 && (is_alnum(chars[k].0) || is_cjk_numeral(chars[k].0)) {
        k += 1;
    }
    if k == start || k >= chars.len() {
        return false;
    }
    let term = chars[k].0;
    let closer = term == u32::from(')') || term == 0xFF09;
    let dot = term == u32::from('.') || term == 0x3001 || term == 0xFF0E || term == 0xFF0C;
    if (paren && closer) || (!paren && (dot || closer)) {
        return gap_after(k) || (term == 0x3001);
    }
    false
}

fn build_block(
    page: &PageText,
    mut lines: Vec<LineBuild>,
    q: Quadrant,
    other_angle: Option<f32>,
    params: &LayoutParams,
) -> (Block, Vec<Line>) {
    lines.sort_by(|a, b| b.v.total_cmp(&a.v).then(a.u0.total_cmp(&b.u0)));
    let mut all_fonts: Vec<u32> = Vec::new();
    let mut all_sizes: Vec<f32> = Vec::new();
    let mut editable = true;
    let mut invisible = true;
    let mut out_lines: Vec<Line> = Vec::with_capacity(lines.len());
    let mut bbox: Option<Rect> = None;
    for l in &lines {
        let (line, lb) = build_line(page, l, q, other_angle);
        bbox = Some(bbox.map_or(lb, |b| b.union(&lb)));
        editable &= line.editable;
        invisible &= line.invisible;
        for g in &l.glyphs {
            all_fonts.push(g.font_id);
            all_sizes.push(g.size);
        }
        out_lines.push(line);
    }
    let font_id = mode_u32(&all_fonts);
    let font_size = mode_f32(&mut all_sizes, 0.1).max(1e-3);
    let line_pitch = if lines.len() >= 2 {
        let mut gaps: Vec<f32> = lines.windows(2).map(|w| w[0].v - w[1].v).collect();
        let points = median_f32(&mut gaps);
        Some(LinePitch {
            points,
            em: points / font_size,
        })
    } else {
        None
    };
    let page_center_u =
        (page.width > 0.0 && page.height > 0.0 && other_angle.is_none()).then(|| {
            q.to_canonical(Point::new(page.width * 0.5, page.height * 0.5))
                .x
        });
    let alignment = infer_alignment(&lines, params.align_tol_em * font_size, page_center_u);
    let angle = other_angle.unwrap_or_else(|| q.degrees());
    // 相对显示方向：页面 /Rotate 90 的页上 90° 的文字看起来是端正的。
    let display_angle = (angle - display_quadrant(page).degrees()).rem_euclid(360.0);
    let horizontal = other_angle.is_none()
        && matches!(
            Quadrant::from_angle(display_angle, 0.01),
            Some(Quadrant::Deg0 | Quadrant::Deg180)
        );
    let block = Block {
        lines: Vec::new(),
        bbox: bbox.unwrap_or_default(),
        alignment,
        line_pitch,
        font_id,
        font_name: page
            .font(font_id)
            .map(|f| f.name.clone())
            .unwrap_or_default(),
        font_size,
        editable,
        horizontal,
        angle,
        invisible,
    };
    (block, out_lines)
}

fn build_line(
    page: &PageText,
    l: &LineBuild,
    q: Quadrant,
    other_angle: Option<f32>,
) -> (Line, Rect) {
    let glyphs: Vec<usize> = l.glyphs.iter().map(|g| g.index).collect();
    let (bbox, baseline) = if other_angle.is_none() {
        let canon = Rect::new(l.u0, l.v0, l.u1, l.v1);
        (q.rect_from_canonical(&canon), l.v)
    } else {
        let mut b: Option<Rect> = None;
        for &i in &glyphs {
            let r = page.glyphs[i].bbox;
            b = Some(b.map_or(r, |acc| acc.union(&r)));
        }
        let baseline = glyphs.first().map_or(0.0, |&i| page.glyphs[i].origin.y);
        (b.unwrap_or_default(), baseline)
    };
    let editable = glyphs.iter().all(|&i| !page.glyphs[i].blocks_editing());
    let invisible = glyphs.iter().all(|&i| page.glyphs[i].invisible);
    let line = Line {
        glyphs,
        baseline,
        bbox,
        font_id: l.font_id,
        font_name: page
            .font(l.font_id)
            .map(|f| f.name.clone())
            .unwrap_or_default(),
        font_size: l.size,
        angle: other_angle.unwrap_or_else(|| q.degrees()),
        editable,
        invisible,
    };
    (line, bbox)
}

/// 推断对齐方式。`tol` 为绝对容差（点）。
fn infer_alignment(lines: &[LineBuild], tol: f32, page_center_u: Option<f32>) -> Alignment {
    let n = lines.len();
    if n == 0 {
        return Alignment::Left;
    }
    if n == 1 {
        let l = &lines[0];
        return match page_center_u {
            Some(c) if ((l.u0 + l.u1) * 0.5 - c).abs() <= tol && l.u0 < c - tol => {
                Alignment::Center
            }
            _ => Alignment::Left,
        };
    }
    // 首行可以缩进或悬挂，末行可以不满：n ≥ 3 时左边缘不看首行、右边缘不看末行。
    let body = n >= 3;
    let lefts: Vec<f32> = lines.iter().skip(usize::from(body)).map(|l| l.u0).collect();
    let rights: Vec<f32> = lines
        .iter()
        .take(if body { n - 1 } else { n })
        .map(|l| l.u1)
        .collect();
    let centers: Vec<f32> = lines.iter().map(|l| (l.u0 + l.u1) * 0.5).collect();
    let la = spread(&lefts) <= tol;
    let ra = spread(&rights) <= tol;
    let ca = spread(&centers) <= tol;
    if la && ra {
        if !body {
            // 两行等宽：无法分辨，按左对齐处理不会改变呈现。
            return Alignment::Left;
        }
        let left = lefts.iter().copied().fold(f32::MAX, f32::min);
        let right = rights.iter().copied().fold(f32::MIN, f32::max);
        let first = &lines[0];
        let last = &lines[n - 1];
        let first_outdented = first.u0 < left - tol;
        let first_indented = first.u0 > left + tol;
        let last_full = last.u1 >= right - tol;
        if first_outdented {
            // 首行比正文更靠左：右对齐块（首行最宽）或悬挂缩进；由末行是否顶到右边缘决定。
            return if last_full {
                Alignment::Right
            } else {
                Alignment::Left
            };
        }
        if !last_full {
            return Alignment::Justify;
        }
        if first_indented {
            // 首行缩进、末行满行：正文 ≥ 3 行时三条左边缘重合不太可能是巧合。
            return if n >= 4 {
                Alignment::Justify
            } else {
                Alignment::Right
            };
        }
        // 所有行等宽且两边都对齐：区分不了，取左对齐。
        return Alignment::Left;
    }
    match (la, ra, ca) {
        (true, _, _) => Alignment::Left,
        (false, true, _) => Alignment::Right,
        (false, false, true) => Alignment::Center,
        _ => Alignment::Left,
    }
}
