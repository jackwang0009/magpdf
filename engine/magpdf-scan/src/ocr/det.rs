//! 文字检测：PP-OCRv5 det（DB 算法）的预处理、推理与后处理。
//!
//! 后处理按 PaddleOCR `DBPostProcess`（`box_type=quad`、`score_mode=fast`）实现：
//! 概率图二值化 → 8 连通区域 → 边界点凸包的最小外接矩形 → 框内平均概率打分 → unclip 扩展 → 映射回原图。
//! 用纯 Rust 实现，不引入 OpenCV 绑定；与 OpenCV 实现的差异见 V09 报告 1.2
//! （`docs/p0/V09-OCR集成与实测.md`）。

use std::time::Instant;

use ort::session::Session;
use ort::value::Tensor;
use serde::{Deserialize, Serialize};

use super::geometry::{
    Point, Quad, bounding_box, expand_quad, min_area_rect, point_in_polygon, polygon_area,
    polygon_perimeter, quad_sides,
};
use super::image::{ChannelOrder, Normalize, RgbImage, fill_chw};
use super::runtime::{first_input_name, load_session};
use super::{Millis, OcrError};

/// 检测前缩放规则（PaddleOCR `DetResizeForTest` 的 `limit_type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LimitType {
    /// 长边超过 `limit_side_len` 时缩小到该长度；小图不放大。适合 300 dpi 扫描件控制耗时。
    #[default]
    Max,
    /// 短边小于 `limit_side_len` 时放大到该长度；大图不缩小（PaddleOCR 3.x 对 PP-OCRv5 的默认行为，
    /// 配合 `max_side_limit` 封顶）。
    Min,
}

/// 检测参数。默认阈值与官方 `inference.yml` 一致（thresh 0.3、box_thresh 0.6、unclip_ratio 1.5）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DetParams {
    pub limit_type: LimitType,
    /// 配合 `limit_type` 的边长；最终输入尺寸取 32 的倍数。
    pub limit_side_len: u32,
    /// 无论如何长边不超过这个值（PaddleX 的 `max_side_limit`）。
    pub max_side_limit: u32,
    /// 概率图二值化阈值。
    pub thresh: f32,
    /// 框内平均概率低于此值的候选丢弃。
    pub box_thresh: f32,
    /// unclip 扩展系数：偏移距离 = 面积 × 系数 ÷ 周长。
    pub unclip_ratio: f32,
    /// 最多处理的连通区数量。
    pub max_candidates: usize,
    /// 最小外接矩形短边（检测图像素）低于此值丢弃。
    pub min_size: f32,
}

impl Default for DetParams {
    fn default() -> Self {
        Self {
            limit_type: LimitType::Max,
            limit_side_len: 1600,
            max_side_limit: 4000,
            thresh: 0.3,
            box_thresh: 0.6,
            unclip_ratio: 1.5,
            max_candidates: 1000,
            min_size: 3.0,
        }
    }
}

impl DetParams {
    /// 按规则算出检测输入尺寸 `(宽, 高)`，均为 32 的倍数且不小于 32。
    pub fn input_size(&self, width: usize, height: usize) -> (usize, usize) {
        let (w, h) = (width as f32, height as f32);
        let limit = self.limit_side_len.max(1) as f32;
        let mut ratio = match self.limit_type {
            LimitType::Max => {
                if w.max(h) > limit {
                    limit / w.max(h)
                } else {
                    1.0
                }
            }
            LimitType::Min => {
                if w.min(h) < limit {
                    limit / w.min(h)
                } else {
                    1.0
                }
            }
        };
        let max_side = self.max_side_limit.max(32) as f32;
        if w.max(h) * ratio > max_side {
            ratio = max_side / w.max(h);
        }
        let round32 = |v: f32| ((v / 32.0).round() * 32.0).max(32.0) as usize;
        (round32((w * ratio).floor()), round32((h * ratio).floor()))
    }
}

/// 一个检测框：原图坐标的四点（左上、右上、右下、左下）与 DB 打分。
#[derive(Debug, Clone, PartialEq)]
pub struct DetBox {
    pub quad: Quad,
    pub score: f32,
}

/// 检测阶段各步耗时。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct DetTimings {
    pub resize: Millis,
    pub infer: Millis,
    pub postprocess: Millis,
    /// 实际送入模型的尺寸 `(宽, 高)`。
    pub input_size: (usize, usize),
}

pub(crate) struct Detector {
    session: Session,
    input_name: String,
    pub(crate) params: DetParams,
}

impl std::fmt::Debug for Detector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Detector")
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

impl Detector {
    pub(crate) fn load(
        env: &ort::environment::Environment,
        path: &std::path::Path,
        params: DetParams,
        intra_threads: usize,
    ) -> Result<Self, OcrError> {
        let session = load_session(env, path, intra_threads)?;
        let input_name = first_input_name(&session, "det")?;
        Ok(Self {
            session,
            input_name,
            params,
        })
    }

    pub(crate) fn detect(&mut self, img: &RgbImage) -> Result<(Vec<DetBox>, DetTimings), OcrError> {
        let mut timings = DetTimings::default();
        let t0 = Instant::now();
        let (in_w, in_h) = self.params.input_size(img.width, img.height);
        let resized = img.resize_bilinear(in_w, in_h);
        let mut input = vec![0f32; 3 * in_w * in_h];
        fill_chw(
            &resized,
            &mut input,
            in_w,
            in_h,
            ChannelOrder::Bgr,
            Normalize::ImageNet,
        );
        timings.resize = Millis::since(t0);
        timings.input_size = (in_w, in_h);

        let t1 = Instant::now();
        let tensor = Tensor::from_array(([1usize, 3, in_h, in_w], input))?;
        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => tensor])?;
        let (shape, prob) = outputs[0].try_extract_tensor::<f32>()?;
        let dims: Vec<i64> = shape.to_vec();
        let (map_h, map_w) = match dims.as_slice() {
            [_, _, h, w] if *h > 0 && *w > 0 => (*h as usize, *w as usize),
            _ => {
                return Err(OcrError::ModelShape {
                    model: "det",
                    detail: format!("输出形状 {dims:?}，期望 [1, 1, H, W]"),
                });
            }
        };
        if prob.len() < map_h * map_w {
            return Err(OcrError::ModelShape {
                model: "det",
                detail: format!("输出元素数 {} 少于 {map_h}×{map_w}", prob.len()),
            });
        }
        timings.infer = Millis::since(t1);

        let t2 = Instant::now();
        let scale_x = img.width as f32 / map_w as f32;
        let scale_y = img.height as f32 / map_h as f32;
        let boxes = db_postprocess(
            &prob[..map_h * map_w],
            map_w,
            map_h,
            &self.params,
            scale_x,
            scale_y,
            img.width as f32,
            img.height as f32,
        );
        timings.postprocess = Millis::since(t2);
        Ok((boxes, timings))
    }
}

/// DB 后处理。`prob` 为 `map_h × map_w` 的概率图；返回原图坐标的框，已按阅读顺序排序。
#[allow(clippy::too_many_arguments)]
pub(crate) fn db_postprocess(
    prob: &[f32],
    map_w: usize,
    map_h: usize,
    params: &DetParams,
    scale_x: f32,
    scale_y: f32,
    dest_w: f32,
    dest_h: f32,
) -> Vec<DetBox> {
    let mask: Vec<bool> = prob.iter().map(|&p| p > params.thresh).collect();
    let components = connected_components(&mask, map_w, map_h, params.max_candidates);

    let mut boxes = Vec::new();
    for comp in components {
        let boundary: Vec<Point> = comp
            .iter()
            .filter(|&&(x, y)| is_boundary(&mask, map_w, map_h, x, y))
            .map(|&(x, y)| Point::new(x as f32, y as f32))
            .collect();
        let Some(rect) = min_area_rect(&boundary) else {
            continue;
        };
        let (a, b) = quad_sides(&rect);
        if a.min(b) < params.min_size {
            continue;
        }
        let score = box_score(prob, map_w, map_h, &rect);
        if score < params.box_thresh {
            continue;
        }
        let area = polygon_area(&rect);
        let perimeter = polygon_perimeter(&rect);
        if perimeter <= f32::EPSILON {
            continue;
        }
        let distance = area * params.unclip_ratio / perimeter;
        let expanded = expand_quad(&rect, distance);
        let (ea, eb) = quad_sides(&expanded);
        if ea.min(eb) < params.min_size + 2.0 {
            continue;
        }
        let mut quad = expanded;
        for p in &mut quad {
            p.x = (p.x * scale_x).round().clamp(0.0, dest_w);
            p.y = (p.y * scale_y).round().clamp(0.0, dest_h);
        }
        // 与 PaddleOCR `filter_tag_det_res` 一致：丢弃宽或高不超过 3 像素的框。
        let (bx0, by0, bx1, by1) = bounding_box(&quad);
        if bx1 - bx0 <= 3.0 || by1 - by0 <= 3.0 {
            continue;
        }
        boxes.push(DetBox { quad, score });
    }
    sort_reading_order(&mut boxes);
    boxes
}

/// 8 连通区域标记（迭代式洪泛）。返回每个区域的像素坐标列表，最多 `max_candidates` 个。
fn connected_components(
    mask: &[bool],
    w: usize,
    h: usize,
    max_candidates: usize,
) -> Vec<Vec<(usize, usize)>> {
    let mut visited = vec![false; mask.len()];
    let mut components = Vec::new();
    let mut stack = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || visited[start] {
            continue;
        }
        if components.len() >= max_candidates {
            break;
        }
        visited[start] = true;
        stack.push(start);
        let mut pixels = Vec::new();
        while let Some(idx) = stack.pop() {
            let (x, y) = (idx % w, idx / w);
            pixels.push((x, y));
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let n = ny as usize * w + nx as usize;
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        components.push(pixels);
    }
    components
}

/// 像素是否在区域边界上（4 邻域有背景或触到图像边缘）。
fn is_boundary(mask: &[bool], w: usize, h: usize, x: usize, y: usize) -> bool {
    x == 0
        || y == 0
        || x + 1 == w
        || y + 1 == h
        || !mask[y * w + x - 1]
        || !mask[y * w + x + 1]
        || !mask[(y - 1) * w + x]
        || !mask[(y + 1) * w + x]
}

/// 框内概率均值（`box_score_fast`：在包围盒范围内按多边形填充取平均）。
fn box_score(prob: &[f32], w: usize, h: usize, quad: &Quad) -> f32 {
    let (x0, y0, x1, y1) = bounding_box(quad);
    let xs = (x0.floor().max(0.0) as usize).min(w - 1);
    let xe = (x1.ceil().max(0.0) as usize).min(w - 1);
    let ys = (y0.floor().max(0.0) as usize).min(h - 1);
    let ye = (y1.ceil().max(0.0) as usize).min(h - 1);
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for y in ys..=ye {
        for x in xs..=xe {
            if point_in_polygon(Point::new(x as f32, y as f32), quad) {
                sum += prob[y * w + x] as f64;
                count += 1;
            }
        }
    }
    if count == 0 {
        // 极小的框：退化为包围盒内的均值。
        for y in ys..=ye {
            for x in xs..=xe {
                sum += prob[y * w + x] as f64;
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        (sum / count as f64) as f32
    }
}

/// 阅读顺序：先按上边 y 再按 x；上边相差不到 10 像素的相邻框按 x 调整（PaddleOCR `sorted_boxes`）。
pub(crate) fn sort_reading_order(boxes: &mut [DetBox]) {
    boxes.sort_by(|a, b| {
        a.quad[0]
            .y
            .partial_cmp(&b.quad[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.quad[0]
                    .x
                    .partial_cmp(&b.quad[0].x)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    for i in 0..boxes.len().saturating_sub(1) {
        let mut j = i;
        while j < boxes.len() - 1 {
            let (a, b) = (&boxes[j], &boxes[j + 1]);
            if (b.quad[0].y - a.quad[0].y).abs() < 10.0 && b.quad[0].x < a.quad[0].x {
                boxes.swap(j, j + 1);
                j += 1;
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_size_rules() {
        let p = DetParams {
            limit_type: LimitType::Max,
            limit_side_len: 960,
            ..DetParams::default()
        };
        // A4 300 dpi：长边 3508 → 960，短边按比例 678 → 672。
        assert_eq!(p.input_size(2480, 3508), (672, 960));
        // 小图不放大，只取 32 的倍数。
        assert_eq!(p.input_size(100, 50), (96, 64));

        let p = DetParams {
            limit_type: LimitType::Min,
            limit_side_len: 736,
            max_side_limit: 4000,
            ..DetParams::default()
        };
        // 短边已超过 736：不缩放，只取 32 的倍数。
        assert_eq!(p.input_size(2480, 3508), (2496, 3520));
        // 短边不足：放大。
        assert_eq!(p.input_size(368, 200), (1344, 736));
        // 长边封顶 4000。
        assert_eq!(p.input_size(5000, 1000).0, 4000);
    }

    #[test]
    fn postprocess_finds_rectangles_and_sorts() {
        // 60×40 的概率图里两块高概率矩形：上面一行两块（右边那块先出现在扫描顺序之外），要求按阅读顺序排序。
        let (w, h) = (60usize, 40usize);
        let mut prob = vec![0.05f32; w * h];
        let fill = |prob: &mut Vec<f32>, x0: usize, x1: usize, y0: usize, y1: usize| {
            for y in y0..y1 {
                for x in x0..x1 {
                    prob[y * w + x] = 0.9;
                }
            }
        };
        fill(&mut prob, 35, 55, 5, 12); // 右上
        fill(&mut prob, 5, 25, 6, 13); // 左上（上边 y 略大但在 10 像素容差内）
        fill(&mut prob, 5, 50, 25, 33); // 下方长条
        let params = DetParams::default();
        let boxes = db_postprocess(&prob, w, h, &params, 2.0, 2.0, 120.0, 80.0);
        assert_eq!(boxes.len(), 3, "{boxes:?}");
        // 阅读顺序：左上、右上、下。
        assert!(boxes[0].quad[0].x < boxes[1].quad[0].x);
        assert!(boxes[2].quad[0].y > boxes[1].quad[0].y);
        // 坐标按 2 倍映射回原图，且框比原矩形大（unclip）。
        let (bw, bh) = quad_sides(&boxes[2].quad);
        assert!(bw > 90.0 && bw < 120.0, "{bw}");
        assert!(bh > 16.0 && bh < 40.0, "{bh}");
        assert!(boxes.iter().all(|b| b.score > 0.8));
    }

    #[test]
    fn postprocess_drops_low_score_and_tiny_regions() {
        let (w, h) = (40usize, 20usize);
        let mut prob = vec![0.0f32; w * h];
        // 分数刚过二值化阈值但低于 box_thresh。
        for y in 5..10 {
            for x in 5..30 {
                prob[y * w + x] = 0.4;
            }
        }
        // 1×1 的小点。
        prob[15 * w + 35] = 0.99;
        let boxes = db_postprocess(&prob, w, h, &DetParams::default(), 1.0, 1.0, 40.0, 20.0);
        assert!(boxes.is_empty(), "{boxes:?}");
    }
}
