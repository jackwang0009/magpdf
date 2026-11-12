//! OCR 流水线（方案 6.2、6.5；P0 验证项 V09）。
//!
//! 输入一页位图（RGB、BGRA 或灰度）→ 文字检测（PP-OCRv5 det，DB 后处理）→ 按四点框透视裁切 →
//! 文字行方向分类（0°/180°）→ 文字识别（PP-OCRv5 rec，CTC 解码）→ 输出每行的四点框、文字、置信度，
//! 以及各阶段耗时。模型通过 ONNX Runtime 推理，CPU 执行；ONNX Runtime 自带线程池，调用方按页串行调用即可。
//!
//! 隐私：本模块的日志只记录数量与耗时，不记录任何识别文字（工程规范 §5）。

pub mod cls;
pub mod det;
pub mod eval;
pub mod format;
pub mod geometry;
pub mod image;
pub mod models;
pub mod rec;
pub mod runtime;

use std::ops::{Add, AddAssign};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub use self::cls::{ClsKind, ClsParams};
pub use self::det::{DetParams, DetTimings, LimitType};
pub use self::geometry::{Point, Quad};
pub use self::image::{ImageView, PixelFormat};
pub use self::models::{Manifest, ModelSelection};
pub use self::rec::RecParams;

use self::cls::{Classifier, Verify, needs_verify, prefer_reversed};
use self::det::Detector;
use self::geometry::quad_sides;
use self::image::RgbImage;
use self::rec::Recognizer;

/// OCR 模块的错误。
#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("ONNX Runtime 初始化失败：{0}")]
    OrtInit(String),
    #[error("ONNX Runtime 错误：{0}")]
    Ort(#[from] ort::Error),
    #[error("无法读取模型文件 {path}：{source}")]
    ModelFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("识别字典与模型不匹配：字典 {dict_len} 类，模型输出 {classes} 类")]
    DictMismatch { dict_len: usize, classes: usize },
    #[error("缺少识别字典：模型元数据里没有 character，也未提供字典文件")]
    DictMissing,
    #[error("模型 {model} 的输入输出不符合预期：{detail}")]
    ModelShape { model: &'static str, detail: String },
    #[error("图像参数无效：{0}")]
    InvalidImage(String),
    #[error("模型清单错误：{0}")]
    Manifest(String),
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),
}

/// 毫秒计时值（f64），便于序列化到 JSON 与相加。
#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Millis(pub f64);

impl Millis {
    pub fn since(start: Instant) -> Self {
        Self(start.elapsed().as_secs_f64() * 1000.0)
    }
}

impl Add for Millis {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for Millis {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl std::fmt::Display for Millis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.1} ms", self.0)
    }
}

/// 识别语言偏好，决定选用哪个识别模型（见 `models::Manifest::engine_config`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// 中英混合（PP-OCRv5 中文模型本身覆盖中英文、日文与拼音）。
    Zh,
    /// 纯英文（`en_PP-OCRv5_mobile_rec`，字典小、速度快）。
    En,
    /// 自动：P0 阶段等同于 `Zh`；方案 6.2 的“按页语言选模型”待正式评测后再定。
    #[default]
    Auto,
}

impl FromStr for Lang {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "zh" | "ch" | "cn" => Ok(Self::Zh),
            "en" => Ok(Self::En),
            "auto" => Ok(Self::Auto),
            other => Err(format!("不支持的语言 {other:?}，可选 zh、en、auto")),
        }
    }
}

/// 模型身份信息，写入结果与 OCR 缓存（文件格式规范 9.5：记录模型名与版本）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub det: String,
    pub rec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cls: Option<String>,
    /// 模型集版本（清单里的 `version`）。
    pub version: String,
    /// 推理运行时（如 `onnxruntime 1.31.1`）。
    #[serde(default)]
    pub runtime: String,
}

/// 引擎配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrConfig {
    /// `onnxruntime.dll` 路径；`None` 时依次尝试环境变量 `ORT_DYLIB_PATH`、可执行文件旁。
    pub ort_dylib: Option<PathBuf>,
    pub det_model: PathBuf,
    pub rec_model: PathBuf,
    /// 识别字典；`None` 时从 ONNX 元数据 `character` 读取。
    pub rec_dict: Option<PathBuf>,
    /// 方向分类模型；`None` 跳过方向分类。
    pub cls_model: Option<PathBuf>,
    /// 每个会话的算子内线程数；0 交给 ONNX Runtime 默认。
    pub intra_threads: usize,
    pub det: DetParams,
    pub cls: ClsParams,
    pub rec: RecParams,
    /// 行置信度低于此值时标记 `low_confidence`（方案 6.2：低置信度字段提示人工复核）。
    pub low_confidence: f32,
    /// 行置信度低于此值直接丢弃（PaddleOCR `drop_score`）。
    pub drop_score: f32,
    pub identity: ModelIdentity,
}

impl OcrConfig {
    /// 用默认参数构造，三个模型路径显式给出。
    pub fn new(det_model: PathBuf, rec_model: PathBuf, cls_model: Option<PathBuf>) -> Self {
        Self {
            ort_dylib: None,
            det_model,
            rec_model,
            rec_dict: None,
            cls_model,
            intra_threads: 0,
            det: DetParams::default(),
            cls: ClsParams::default(),
            rec: RecParams::default(),
            low_confidence: 0.8,
            drop_score: 0.5,
            identity: ModelIdentity::default(),
        }
    }
}

/// 一行识别结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrLine {
    /// 原图坐标的四点框：左上、右上、右下、左下。
    pub quad: Quad,
    pub text: String,
    /// 行置信度（字符概率平均）。
    pub confidence: f32,
    /// 低于 `OcrConfig::low_confidence`。
    pub low_confidence: bool,
    /// 每个字符的概率，与 `text.chars()` 对应。
    pub char_confidences: Vec<f32>,
    /// 检测打分（DB 框内平均概率）。
    pub det_score: f32,
    /// 方向分类判定为 180° 并已翻转后识别。
    pub rotated_180: bool,
    /// 裁切后高宽比 ≥ 1.5、按竖排处理（逆时针旋转 90° 后识别）。
    pub vertical: bool,
}

/// 各阶段耗时。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct StageTimings {
    /// 输入位图转内部 RGB。
    pub convert: Millis,
    pub det: DetTimings,
    /// 透视裁切。
    pub crop: Millis,
    pub cls: Millis,
    pub rec: Millis,
    pub total: Millis,
}

/// 模型加载耗时。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct LoadTimings {
    pub runtime: Millis,
    pub det: Millis,
    pub cls: Millis,
    pub rec: Millis,
    pub total: Millis,
}

/// 一页的识别结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrResult {
    pub width: u32,
    pub height: u32,
    pub lines: Vec<OcrLine>,
    /// 检测到但识别置信度低于 `drop_score` 而丢弃的行数。
    pub dropped: usize,
    /// 方向复核了几行（正着、倒着各识别一次；见 `cls::needs_verify`）。
    #[serde(default)]
    pub orientation_checked: usize,
    /// 复核之后没有采用方向分类的判断的行数。
    #[serde(default)]
    pub orientation_overruled: usize,
    pub timings: StageTimings,
    pub models: ModelIdentity,
}

/// OCR 引擎：一次加载模型，可反复识别。`Send`，可移动到工作线程；内部持有 ONNX Runtime 会话。
pub struct OcrEngine {
    config: OcrConfig,
    detector: Detector,
    classifier: Option<Classifier>,
    recognizer: Recognizer,
    load_timings: LoadTimings,
}

impl std::fmt::Debug for OcrEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OcrEngine")
            .field("config", &self.config)
            .field("detector", &self.detector)
            .field("classifier", &self.classifier)
            .field("recognizer", &self.recognizer)
            .field("load_timings", &self.load_timings)
            .finish()
    }
}

impl OcrEngine {
    /// 加载 ONNX Runtime 与全部模型。
    pub fn new(config: OcrConfig) -> Result<Self, OcrError> {
        let total = Instant::now();
        let t = Instant::now();
        let env = runtime::environment(config.ort_dylib.as_deref())?;
        let runtime_ms = Millis::since(t);

        let t = Instant::now();
        let detector = Detector::load(&env, &config.det_model, config.det, config.intra_threads)?;
        let det_ms = Millis::since(t);

        let t = Instant::now();
        let classifier = match &config.cls_model {
            Some(p) => Some(Classifier::load(&env, p, config.cls, config.intra_threads)?),
            None => None,
        };
        let cls_ms = Millis::since(t);

        let t = Instant::now();
        let recognizer = Recognizer::load(
            &env,
            &config.rec_model,
            config.rec_dict.as_deref(),
            config.rec,
            config.intra_threads,
        )?;
        let rec_ms = Millis::since(t);

        let load_timings = LoadTimings {
            runtime: runtime_ms,
            det: det_ms,
            cls: cls_ms,
            rec: rec_ms,
            total: Millis::since(total),
        };
        tracing::info!(
            runtime_ms = load_timings.runtime.0,
            det_ms = load_timings.det.0,
            cls_ms = load_timings.cls.0,
            rec_ms = load_timings.rec.0,
            classes = recognizer.classes(),
            "OCR 模型加载完成"
        );
        Ok(Self {
            config,
            detector,
            classifier,
            recognizer,
            load_timings,
        })
    }

    pub fn config(&self) -> &OcrConfig {
        &self.config
    }

    pub fn load_timings(&self) -> LoadTimings {
        self.load_timings
    }

    /// 识别字典来源（文件路径或 `onnx-metadata`）。
    pub fn dict_source(&self) -> &str {
        &self.recognizer.dict_source
    }

    /// ONNX Runtime 构建信息（版本、提交号）。
    pub fn runtime_build_info(&self) -> String {
        runtime::runtime_build_info()
    }

    /// 识别一页。调用方保证 `view` 的像素属于同一页；返回的坐标为 `view` 的像素坐标。
    pub fn recognize(&mut self, view: &ImageView<'_>) -> Result<OcrResult, OcrError> {
        let total = Instant::now();
        let mut timings = StageTimings::default();

        let t = Instant::now();
        let img = RgbImage::from_view(view);
        timings.convert = Millis::since(t);

        let (boxes, det_timings) = self.detector.detect(&img)?;
        timings.det = det_timings;

        let t = Instant::now();
        let mut crops = Vec::with_capacity(boxes.len());
        let mut vertical = Vec::with_capacity(boxes.len());
        for b in &boxes {
            let (crop, is_vertical) = crop_line(&img, &b.quad);
            crops.push(crop);
            vertical.push(is_vertical);
        }
        timings.crop = Millis::since(t);

        let mut rotated = vec![false; crops.len()];
        let mut verify = vec![Verify::No; crops.len()];
        if let Some(cls) = &mut self.classifier {
            let (results, ms) = cls.classify(&crops)?;
            timings.cls = ms;
            for (i, r) in results.iter().enumerate() {
                if r.flipped {
                    crops[i] = crops[i].rotate_180();
                    rotated[i] = true;
                }
            }
            if self.config.cls.verify {
                verify = needs_verify(&rotated, &vertical);
            }
        }

        let (mut rec_results, rec_ms) = self.recognizer.recognize(&crops)?;
        timings.rec = rec_ms;

        // 复核方向：和同页多数行方向不一样的行，反过来再识别一次，按规则取其中一种读法。
        let to_check: Vec<usize> = (0..crops.len())
            .filter(|&i| verify[i] != Verify::No)
            .collect();
        let mut overruled = 0usize;
        if !to_check.is_empty() {
            let reversed: Vec<RgbImage> = to_check.iter().map(|&i| crops[i].rotate_180()).collect();
            let (reversed_results, ms) = self.recognizer.recognize(&reversed)?;
            timings.rec += ms;
            for (&i, alt) in to_check.iter().zip(reversed_results) {
                if prefer_reversed(
                    verify[i],
                    rec_results[i].confidence,
                    alt.confidence,
                    self.config.cls.verify_margin,
                ) {
                    rec_results[i] = alt;
                    rotated[i] = !rotated[i];
                    overruled += 1;
                }
            }
        }

        let mut lines = Vec::with_capacity(boxes.len());
        let mut dropped = 0usize;
        for (i, (b, r)) in boxes.into_iter().zip(rec_results).enumerate() {
            if r.text.is_empty() || r.confidence < self.config.drop_score {
                dropped += 1;
                continue;
            }
            lines.push(OcrLine {
                quad: b.quad,
                low_confidence: r.confidence < self.config.low_confidence,
                text: r.text,
                confidence: r.confidence,
                char_confidences: r.char_confidences,
                det_score: b.score,
                rotated_180: rotated[i],
                vertical: vertical[i],
            });
        }
        timings.total = Millis::since(total);
        tracing::debug!(
            lines = lines.len(),
            dropped,
            orientation_checked = to_check.len(),
            orientation_overruled = overruled,
            det_ms = timings.det.infer.0,
            cls_ms = timings.cls.0,
            rec_ms = timings.rec.0,
            total_ms = timings.total.0,
            "OCR 一页完成"
        );
        Ok(OcrResult {
            width: view.width(),
            height: view.height(),
            lines,
            dropped,
            orientation_checked: to_check.len(),
            orientation_overruled: overruled,
            timings,
            models: self.config.identity.clone(),
        })
    }
}

/// 按 PaddleOCR `get_rotate_crop_image` 裁切：宽取上下边较长者，高取左右边较长者，透视变换；
/// 高宽比 ≥ 1.5 视为竖排，逆时针旋转 90°。
fn crop_line(img: &RgbImage, quad: &Quad) -> (RgbImage, bool) {
    let (w, h) = quad_sides(quad);
    let w = (w.round() as usize).max(1);
    let h = (h.round() as usize).max(1);
    let crop = img.warp_quad(quad, w, h);
    if h as f32 / w as f32 >= 1.5 {
        (crop.rotate_90_ccw(), true)
    } else {
        (crop, false)
    }
}

/// 让调用方可以把引擎交给工作线程：编译期断言 `OcrEngine: Send`。
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<OcrEngine>();
};

/// 模型目录里 ONNX Runtime 动态库的约定位置（与 `models/fetch-models.ps1` 一致）。
pub fn default_dylib_in(models_dir: &Path) -> PathBuf {
    models_dir.join("onnxruntime").join(format!(
        "{}onnxruntime{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_parsing() {
        assert_eq!("zh".parse::<Lang>(), Ok(Lang::Zh));
        assert_eq!("EN".parse::<Lang>(), Ok(Lang::En));
        assert_eq!("auto".parse::<Lang>(), Ok(Lang::Auto));
        assert!("fr".parse::<Lang>().is_err());
    }

    #[test]
    fn crop_line_handles_vertical_text() {
        let img = RgbImage::new(100, 100);
        let quad: Quad = [
            Point::new(10.0, 10.0),
            Point::new(20.0, 10.0),
            Point::new(20.0, 60.0),
            Point::new(10.0, 60.0),
        ];
        let (crop, vertical) = crop_line(&img, &quad);
        assert!(vertical);
        assert_eq!((crop.width, crop.height), (50, 10));

        let quad: Quad = [
            Point::new(10.0, 10.0),
            Point::new(90.0, 10.0),
            Point::new(90.0, 30.0),
            Point::new(10.0, 30.0),
        ];
        let (crop, vertical) = crop_line(&img, &quad);
        assert!(!vertical);
        assert_eq!((crop.width, crop.height), (80, 20));
    }

    #[test]
    fn millis_formatting_and_sum() {
        let mut a = Millis(1.25);
        a += Millis(2.0);
        assert_eq!((a + Millis(0.75)).0, 4.0);
        assert_eq!(Millis(12.34).to_string(), "12.3 ms");
    }

    #[test]
    fn engine_fails_cleanly_without_models() {
        // 没有动态库/模型文件时必须返回错误而不是 panic。
        let cfg = OcrConfig {
            ort_dylib: Some(PathBuf::from("Z:/definitely/missing/onnxruntime.dll")),
            ..OcrConfig::new(
                PathBuf::from("Z:/missing/det.onnx"),
                PathBuf::from("Z:/missing/rec.onnx"),
                None,
            )
        };
        let err = OcrEngine::new(cfg).map(|_| ()).expect_err("应当失败");
        // 同一进程里若别的测试已成功创建环境，则在模型文件这一步失败。
        assert!(
            matches!(err, OcrError::OrtInit(_) | OcrError::ModelFile { .. }),
            "{err}"
        );
    }
}
