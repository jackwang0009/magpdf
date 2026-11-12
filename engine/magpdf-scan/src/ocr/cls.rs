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
    /// 复核和同页多数行方向不一样的行（见 [`needs_verify`]）：正着、倒着各识别一次再定方向。
    /// 方向分类模型会把一小部分正着的行判成 180°，判错的那一行整行读反或者被当成低置信度丢掉
    /// （实测的比例和复核的效果见 V09 报告 4.4）。
    #[serde(default = "default_verify")]
    pub verify: bool,
    /// 复核时，和多数行方向不一样的读法要比多数方向的读法置信度高出这么多才采用。
    #[serde(default = "default_verify_margin")]
    pub verify_margin: f32,
}

fn default_verify() -> bool {
    true
}

fn default_verify_margin() -> f32 {
    0.1
}

impl Default for ClsParams {
    fn default() -> Self {
        Self {
            kind: ClsKind::TextLineOri,
            thresh: 0.5,
            batch_size: 6,
            verify: default_verify(),
            verify_margin: default_verify_margin(),
        }
    }
}

impl ClsParams {
    pub fn legacy() -> Self {
        Self {
            kind: ClsKind::LegacyV2,
            thresh: 0.9,
            ..Self::default()
        }
    }
}

/// 一行的方向要不要复核，按什么规则定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// 和同组多数行的方向一致，不复核。
    No,
    /// 和同组多数行的方向不一致：除非这个方向的读法置信度明显更高，否则改用多数行的方向。
    AgainstMajority,
    /// 同组里看不出多数（行太少，或两种方向都不少）：两种读法取置信度高的。
    NoMajority,
}

/// 一组里至少要有这么多行才谈得上“多数”。
const MAJORITY_MIN_LINES: usize = 3;

/// 按同页其他行的方向，定出每一行要不要复核。
///
/// 横排的行和竖排的行（裁切时转过 90° 的）各算一组：一页纸上的字基本朝同一个方向，
/// 一组里三分之二以上的行是同一个方向时，剩下的少数行很可能是方向分类判错了。
/// `flipped[i]` 是方向分类对第 i 行的判断（是否 180°），`vertical[i]` 是这一行是否按竖排处理。
pub fn needs_verify(flipped: &[bool], vertical: &[bool]) -> Vec<Verify> {
    let mut out = vec![Verify::No; flipped.len()];
    for group in [false, true] {
        let members: Vec<usize> = (0..flipped.len())
            .filter(|&i| vertical.get(i).copied().unwrap_or(false) == group)
            .collect();
        let flips = members.iter().filter(|&&i| flipped[i]).count();
        let majority = if members.len() < MAJORITY_MIN_LINES {
            None
        } else if flips * 3 >= members.len() * 2 {
            Some(true)
        } else if (members.len() - flips) * 3 >= members.len() * 2 {
            Some(false)
        } else {
            None
        };
        for &i in &members {
            out[i] = match majority {
                Some(m) if flipped[i] == m => Verify::No,
                Some(_) => Verify::AgainstMajority,
                None => Verify::NoMajority,
            };
        }
    }
    out
}

/// 复核之后用不用反过来的读法。`confidence` 是按方向分类的判断识别出来的置信度，
/// `reversed_confidence` 是反过来识别的置信度。
pub fn prefer_reversed(
    verify: Verify,
    confidence: f32,
    reversed_confidence: f32,
    margin: f32,
) -> bool {
    match verify {
        Verify::No => false,
        // 反过来就是多数行的方向：分类给的方向没有明显更好就用它。
        Verify::AgainstMajority => confidence <= reversed_confidence + margin,
        Verify::NoMajority => reversed_confidence > confidence,
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

    #[test]
    fn lone_flip_on_an_upright_page_is_verified() {
        // 正着的一页 30 行，方向分类把第 7 行判成了 180°。
        let mut flipped = vec![false; 30];
        flipped[7] = true;
        let v = needs_verify(&flipped, &[false; 30]);
        assert_eq!(v[7], Verify::AgainstMajority);
        assert_eq!(v.iter().filter(|&&x| x == Verify::No).count(), 29);
    }

    #[test]
    fn lone_unflipped_line_on_an_upside_down_page_is_verified() {
        // 倒着的一页：只有一行被判成了正的。
        let mut flipped = vec![true; 30];
        flipped[3] = false;
        let v = needs_verify(&flipped, &[false; 30]);
        assert_eq!(v[3], Verify::AgainstMajority);
        assert_eq!(v.iter().filter(|&&x| x == Verify::No).count(), 29);
    }

    #[test]
    fn horizontal_and_vertical_lines_vote_separately() {
        // 10 行横排都是正的；4 行竖排里 3 行判成 180°、1 行判成正的；另有一行竖排单独成不了组。
        let flipped = [
            false, false, false, false, false, false, false, false, false, false, true, true, true,
            false,
        ];
        let mut vertical = vec![false; 10];
        vertical.extend([true; 4]);
        let v = needs_verify(&flipped, &vertical);
        assert!(v[..10].iter().all(|&x| x == Verify::No));
        assert_eq!(&v[10..13], &[Verify::No; 3]);
        assert_eq!(v[13], Verify::AgainstMajority);

        // 只有一行竖排：没有多数可言，两种读法都试。
        let v = needs_verify(&[false, false, false, true], &[false, false, false, true]);
        assert_eq!(v, [Verify::No, Verify::No, Verify::No, Verify::NoMajority]);
    }

    #[test]
    fn no_clear_majority_verifies_everything() {
        // 6 行里 3 正 3 反：全都复核。行数不到 3 也一样。
        let v = needs_verify(&[true, false, true, false, true, false], &[false; 6]);
        assert!(v.iter().all(|&x| x == Verify::NoMajority));
        assert_eq!(
            needs_verify(&[true, true], &[false, false]),
            [Verify::NoMajority; 2]
        );
        assert!(needs_verify(&[], &[]).is_empty());
    }

    #[test]
    fn reversed_reading_is_preferred_by_rule() {
        // 不复核的行不换。
        assert!(!prefer_reversed(Verify::No, 0.2, 0.99, 0.1));
        // 和多数行不一致：两种读法差不多（比如单独一个 6 和 9），用多数行的方向。
        assert!(prefer_reversed(Verify::AgainstMajority, 0.98, 0.97, 0.1));
        // 分类给的方向读得明显更好（这一行确实是倒着印的），保留。
        assert!(!prefer_reversed(Verify::AgainstMajority, 0.95, 0.60, 0.1));
        // 分类判错的典型情况：倒着读出一串低置信度的字，正着读是高置信度。
        assert!(prefer_reversed(Verify::AgainstMajority, 0.62, 0.97, 0.1));
        // 没有多数：谁高用谁，一样高时保留分类的判断。
        assert!(prefer_reversed(Verify::NoMajority, 0.7, 0.9, 0.1));
        assert!(!prefer_reversed(Verify::NoMajority, 0.9, 0.9, 0.1));
    }
}
