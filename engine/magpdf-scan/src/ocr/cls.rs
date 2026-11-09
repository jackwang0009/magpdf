//! 文字行方向分类（0° / 180°）。
//!
//! 支持两种模型：
//! - [`ClsKind::TextLineOri`]：PP-OCRv5 流水线的 `PP-LCNet_x0_25_textline_ori`，输入固定 160×80，
//!   ImageNet 归一化、RGB（官方 `inference.yml`）；
//! - [`ClsKind::LegacyV2`]：`ch_ppocr_mobile_v2.0_cls`，输入高 48、宽至多 192 按比例缩放后右侧补零，
//!   `(x/255-0.5)/0.5`、BGR。

use std::path::Path;
use std::time::Instant;

use ort::session::Session;
use ort::value::Tensor;
use serde::{Deserialize, Serialize};

use super::image::{ChannelOrder, Normalize, RgbImage, fill_chw};
use super::runtime::{first_input_name, load_session};
use super::{Millis, OcrError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClsKind {
    #[default]
    TextLineOri,
    LegacyV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClsParams {
    pub kind: ClsKind,
    /// 判为 180° 的最低置信度；低于它保持原方向。
    pub thresh: f32,
    pub batch_size: usize,
}

impl Default for ClsParams {
    fn default() -> Self {
        Self {
            kind: ClsKind::TextLineOri,
            thresh: 0.5,
            batch_size: 6,
        }
    }
}

impl ClsParams {
    pub fn legacy() -> Self {
        Self {
            kind: ClsKind::LegacyV2,
            thresh: 0.9,
            batch_size: 6,
        }
    }
}

/// 单行的方向判断结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClsResult {
    /// `true` 表示判为 180°（置信度达到阈值），调用方应把裁切图旋转后再识别。
    pub flipped: bool,
    /// 180° 类别的概率。
    pub prob_180: f32,
}

pub(crate) struct Classifier {
    session: Session,
    input_name: String,
    pub(crate) params: ClsParams,
}

impl std::fmt::Debug for Classifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Classifier")
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

impl Classifier {
    pub(crate) fn load(
        env: &ort::environment::Environment,
        path: &Path,
        params: ClsParams,
        intra_threads: usize,
    ) -> Result<Self, OcrError> {
        let session = load_session(env, path, intra_threads)?;
        let input_name = first_input_name(&session, "cls")?;
        Ok(Self {
            session,
            input_name,
            params,
        })
    }

    /// 对一批裁切图判断方向。返回值与输入一一对应。
    pub(crate) fn classify(
        &mut self,
        crops: &[RgbImage],
    ) -> Result<(Vec<ClsResult>, Millis), OcrError> {
        let started = Instant::now();
        let mut results = Vec::with_capacity(crops.len());
        let batch = self.params.batch_size.max(1);
        for chunk in crops.chunks(batch) {
            let (input, w, h) = self.prepare_batch(chunk);
            let tensor = Tensor::from_array(([chunk.len(), 3, h, w], input))?;
            let outputs = self
                .session
                .run(ort::inputs![self.input_name.as_str() => tensor])?;
            let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
            let dims: Vec<i64> = shape.to_vec();
            let classes = match dims.as_slice() {
                [n, c] if *n as usize == chunk.len() && *c >= 2 => *c as usize,
                _ => {
                    return Err(OcrError::ModelShape {
                        model: "cls",
                        detail: format!("输出形状 {dims:?}，期望 [N, 2]"),
                    });
                }
            };
            for i in 0..chunk.len() {
                let logits = &data[i * classes..(i + 1) * classes];
                let probs = softmax_if_needed(logits);
                let prob_180 = probs.get(1).copied().unwrap_or(0.0);
                let flipped = prob_180 >= probs[0] && prob_180 >= self.params.thresh;
                results.push(ClsResult { flipped, prob_180 });
            }
        }
        Ok((results, Millis::since(started)))
    }

    /// 返回 `(张量数据, 宽, 高)`。
    fn prepare_batch(&self, crops: &[RgbImage]) -> (Vec<f32>, usize, usize) {
        match self.params.kind {
            ClsKind::TextLineOri => {
                let (w, h) = (160usize, 80usize);
                let mut input = vec![0f32; crops.len() * 3 * w * h];
                for (i, crop) in crops.iter().enumerate() {
                    let resized = crop.resize_bilinear(w, h);
                    fill_chw(
                        &resized,
                        &mut input[i * 3 * w * h..(i + 1) * 3 * w * h],
                        w,
                        h,
                        ChannelOrder::Rgb,
                        Normalize::ImageNet,
                    );
                }
                (input, w, h)
            }
            ClsKind::LegacyV2 => {
                let (w, h) = (192usize, 48usize);
                let mut input = vec![0f32; crops.len() * 3 * w * h];
                for (i, crop) in crops.iter().enumerate() {
                    let ratio = crop.width as f32 / crop.height.max(1) as f32;
                    let rw = ((h as f32 * ratio).ceil() as usize).clamp(1, w);
                    let resized = crop.resize_bilinear(rw, h);
                    fill_chw(
                        &resized,
                        &mut input[i * 3 * w * h..(i + 1) * 3 * w * h],
                        w,
                        h,
                        ChannelOrder::Bgr,
                        Normalize::Half,
                    );
                }
                (input, w, h)
            }
        }
    }
}

/// 模型输出通常已是 softmax 概率；若不是（有负值或和不为 1），这里补一次 softmax。
fn softmax_if_needed(v: &[f32]) -> Vec<f32> {
    let sum: f32 = v.iter().sum();
    if v.iter().all(|&x| (0.0..=1.0).contains(&x)) && (sum - 1.0).abs() < 1e-3 {
        return v.to_vec();
    }
    let max = v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = v.iter().map(|&x| (x - max).exp()).collect();
    let total: f32 = exps.iter().sum();
    exps.iter().map(|&e| e / total).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softmax_only_when_needed() {
        assert_eq!(softmax_if_needed(&[0.3, 0.7]), vec![0.3, 0.7]);
        let p = softmax_if_needed(&[2.0, 0.0]);
        assert!((p[0] + p[1] - 1.0).abs() < 1e-6 && p[0] > 0.8);
    }
}
