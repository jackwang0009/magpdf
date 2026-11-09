//! 文字识别：PP-OCRv5 rec（SVTR + CTC）的预处理、批推理与 CTC 贪心解码。
//!
//! 预处理按官方 `inference.yml`：高固定 48，宽按比例缩放（不超过本批最大宽度）后右侧补零，
//! BGR、`(x/255-0.5)/0.5`。宽度按长宽比排序后分批，每批宽度向上取 32 的倍数（“分桶”），
//! 减少 ONNX Runtime 需要处理的不同输入形状。

use std::path::Path;
use std::time::Instant;

use ort::session::Session;
use ort::value::Tensor;
use serde::{Deserialize, Serialize};

use super::image::{ChannelOrder, Normalize, RgbImage, fill_chw};
use super::runtime::{first_input_name, load_session};
use super::{Millis, OcrError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecParams {
    /// 输入高度（PP-OCRv5 为 48）。
    pub image_height: usize,
    /// 基准宽度：一批的宽度至少为此值（官方 320）。
    pub base_width: usize,
    /// 单批宽度上限，防止极长行占用过多内存；超出的行按比例缩小宽度。
    pub max_width: usize,
    pub batch_size: usize,
}

impl Default for RecParams {
    fn default() -> Self {
        Self {
            image_height: 48,
            base_width: 320,
            max_width: 3200,
            batch_size: 6,
        }
    }
}

/// 一行的识别结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RecResult {
    pub text: String,
    /// 全行置信度：保留字符概率的平均值；无字符时为 0。
    pub confidence: f32,
    /// 每个输出字符对应的概率，与 `text.chars()` 一一对应。
    pub char_confidences: Vec<f32>,
}

pub(crate) struct Recognizer {
    session: Session,
    input_name: String,
    /// 解码表：下标 0 为 blank，其余为字符（末尾可含空格）。
    table: Vec<String>,
    pub(crate) params: RecParams,
    /// 字典来源描述（文件名或 `onnx-metadata`），写入报告用。
    pub(crate) dict_source: String,
}

impl std::fmt::Debug for Recognizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recognizer")
            .field("params", &self.params)
            .field("classes", &self.table.len())
            .field("dict_source", &self.dict_source)
            .finish_non_exhaustive()
    }
}

impl Recognizer {
    pub(crate) fn load(
        env: &ort::environment::Environment,
        path: &Path,
        dict: Option<&Path>,
        params: RecParams,
        intra_threads: usize,
    ) -> Result<Self, OcrError> {
        let session = load_session(env, path, intra_threads)?;
        let input_name = first_input_name(&session, "rec")?;
        let (chars, dict_source) = match dict {
            Some(p) => {
                let text = std::fs::read_to_string(p).map_err(|source| OcrError::ModelFile {
                    path: p.to_path_buf(),
                    source,
                })?;
                (parse_dict(&text), p.display().to_string())
            }
            None => {
                let meta = session.metadata()?;
                let text = meta.custom("character").ok_or(OcrError::DictMissing)?;
                (parse_dict(&text), "onnx-metadata".to_owned())
            }
        };
        let classes = output_classes(&session);
        let table = build_table(chars, classes)?;
        Ok(Self {
            session,
            input_name,
            table,
            params,
            dict_source,
        })
    }

    pub(crate) fn classes(&self) -> usize {
        self.table.len()
    }

    /// 识别一批裁切图，返回值与输入顺序一一对应。
    pub(crate) fn recognize(
        &mut self,
        crops: &[RgbImage],
    ) -> Result<(Vec<RecResult>, Millis), OcrError> {
        let started = Instant::now();
        let h = self.params.image_height.max(8);
        let mut order: Vec<usize> = (0..crops.len()).collect();
        order.sort_by(|&a, &b| {
            let ra = crops[a].width as f32 / crops[a].height.max(1) as f32;
            let rb = crops[b].width as f32 / crops[b].height.max(1) as f32;
            ra.partial_cmp(&rb).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut results: Vec<Option<RecResult>> = vec![None; crops.len()];
        let batch = self.params.batch_size.max(1);
        for chunk in order.chunks(batch) {
            // 本批宽度：最大长宽比 × 高度，向上取 32 的倍数，介于 base_width 与 max_width 之间。
            let max_ratio = chunk
                .iter()
                .map(|&i| crops[i].width as f32 / crops[i].height.max(1) as f32)
                .fold(self.params.base_width as f32 / h as f32, f32::max);
            let want = (h as f32 * max_ratio).ceil() as usize;
            let w = want.div_ceil(32) * 32;
            let w = w.clamp(32, self.params.max_width.max(32));

            let mut input = vec![0f32; chunk.len() * 3 * h * w];
            for (slot, &i) in chunk.iter().enumerate() {
                let crop = &crops[i];
                let ratio = crop.width as f32 / crop.height.max(1) as f32;
                let rw = ((h as f32 * ratio).ceil() as usize).clamp(1, w);
                let resized = crop.resize_bilinear(rw, h);
                fill_chw(
                    &resized,
                    &mut input[slot * 3 * h * w..(slot + 1) * 3 * h * w],
                    w,
                    h,
                    ChannelOrder::Bgr,
                    Normalize::Half,
                );
            }
            let tensor = Tensor::from_array(([chunk.len(), 3, h, w], input))?;
            let outputs = self
                .session
                .run(ort::inputs![self.input_name.as_str() => tensor])?;
            let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
            let dims: Vec<i64> = shape.to_vec();
            let (steps, classes) = match dims.as_slice() {
                [n, t, c] if *n as usize == chunk.len() && *t > 0 && *c > 0 => {
                    (*t as usize, *c as usize)
                }
                _ => {
                    return Err(OcrError::ModelShape {
                        model: "rec",
                        detail: format!("输出形状 {dims:?}，期望 [N, T, C]"),
                    });
                }
            };
            if classes != self.table.len() {
                return Err(OcrError::DictMismatch {
                    dict_len: self.table.len(),
                    classes,
                });
            }
            for (slot, &i) in chunk.iter().enumerate() {
                let logits = &data[slot * steps * classes..(slot + 1) * steps * classes];
                results[i] = Some(ctc_greedy_decode(logits, steps, classes, &self.table));
            }
        }
        let results = results.into_iter().flatten().collect();
        Ok((results, Millis::since(started)))
    }
}

/// 模型输出的类别数（最后一维）；形状动态时返回 `None`。
fn output_classes(session: &Session) -> Option<usize> {
    let output = session.outputs().first()?;
    let dims = output.dtype().tensor_shape()?;
    let last = *dims.last()?;
    (last > 0).then_some(last as usize)
}

/// 按 PaddleOCR 规则读取字典：每行一个字符，只去掉行尾的 `\r`/`\n`，不去掉空格。
pub(crate) fn parse_dict(text: &str) -> Vec<String> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect()
}

/// 组装解码表：`blank` + 字典 + 空格（PaddleOCR `use_space_char=true`）。
/// 已知模型输出类别数时校验：允许“字典+2（含空格）”或“字典+1（不含空格）”。
fn build_table(chars: Vec<String>, classes: Option<usize>) -> Result<Vec<String>, OcrError> {
    let dict_len = chars.len();
    let with_space = match classes {
        Some(c) if c == dict_len + 2 => true,
        Some(c) if c == dict_len + 1 => false,
        Some(c) => {
            return Err(OcrError::DictMismatch {
                dict_len,
                classes: c,
            });
        }
        None => true,
    };
    let mut table = Vec::with_capacity(dict_len + 2);
    table.push(String::new()); // blank
    table.extend(chars);
    if with_space {
        table.push(" ".to_owned());
    }
    Ok(table)
}

/// CTC 贪心解码：逐时间步取最大类别，去重复、去 blank（下标 0）。
pub(crate) fn ctc_greedy_decode(
    logits: &[f32],
    steps: usize,
    classes: usize,
    table: &[String],
) -> RecResult {
    let mut text = String::new();
    let mut confs = Vec::new();
    let mut prev = usize::MAX;
    for t in 0..steps {
        let row = &logits[t * classes..(t + 1) * classes];
        let (idx, &val) =
            row.iter()
                .enumerate()
                .fold((0usize, &f32::NEG_INFINITY), |best, (i, v)| {
                    if v > best.1 { (i, v) } else { best }
                });
        if idx != 0
            && idx != prev
            && let Some(ch) = table.get(idx)
        {
            text.push_str(ch);
            confs.push(val.clamp(0.0, 1.0));
        }
        prev = idx;
    }
    let confidence = if confs.is_empty() {
        0.0
    } else {
        confs.iter().sum::<f32>() / confs.len() as f32
    };
    RecResult {
        text,
        confidence,
        char_confidences: confs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_parsing_keeps_spaces_and_drops_trailing_newline() {
        assert_eq!(parse_dict("a\nb\r\n \n"), vec!["a", "b", " "]);
        assert_eq!(parse_dict("a\nb"), vec!["a", "b"]);
        assert_eq!(parse_dict(""), Vec::<String>::new());
    }

    #[test]
    fn table_validation() {
        let chars = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(build_table(chars.clone(), Some(4)).unwrap().len(), 4);
        assert_eq!(build_table(chars.clone(), Some(3)).unwrap().len(), 3);
        assert_eq!(build_table(chars.clone(), None).unwrap().len(), 4);
        assert!(matches!(
            build_table(chars, Some(9)),
            Err(OcrError::DictMismatch {
                dict_len: 2,
                classes: 9
            })
        ));
    }

    #[test]
    fn ctc_collapses_repeats_and_blanks() {
        // 表：blank, A, B, 空格
        let table: Vec<String> = ["", "A", "B", " "]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        // 时间步：A A blank A B B 空格 blank
        let rows: [[f32; 4]; 8] = [
            [0.1, 0.8, 0.05, 0.05],
            [0.1, 0.7, 0.1, 0.1],
            [0.9, 0.05, 0.03, 0.02],
            [0.1, 0.6, 0.2, 0.1],
            [0.0, 0.1, 0.9, 0.0],
            [0.0, 0.1, 0.85, 0.05],
            [0.1, 0.1, 0.1, 0.7],
            [1.0, 0.0, 0.0, 0.0],
        ];
        let logits: Vec<f32> = rows.iter().flatten().copied().collect();
        let r = ctc_greedy_decode(&logits, 8, 4, &table);
        assert_eq!(r.text, "AAB ");
        assert_eq!(r.char_confidences.len(), 4);
        assert!((r.confidence - (0.8 + 0.6 + 0.9 + 0.7) / 4.0).abs() < 1e-6);
    }

    #[test]
    fn ctc_empty_when_all_blank() {
        let table: Vec<String> = ["", "A"].iter().map(|s| (*s).to_owned()).collect();
        let r = ctc_greedy_decode(&[0.9, 0.1, 0.8, 0.2], 2, 2, &table);
        assert_eq!(r.text, "");
        assert_eq!(r.confidence, 0.0);
    }
}
