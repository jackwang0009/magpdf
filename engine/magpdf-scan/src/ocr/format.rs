//! 结果文件与标注文件的格式（`docs/p0/V09-标注格式.md`）。
//!
//! - 预测结果：`magpdf-ocr-result/1`，一页一个 JSON，由 `magpdf-cli ocr --json` 写出；
//! - 标注：PPOCRLabel 的 `Label.txt`（`图像路径<TAB>JSON 数组`），元素 `{"transcription", "points", "key_cls"?}`。

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::geometry::Point;
use super::{LoadTimings, Millis, ModelIdentity, OcrError, OcrResult, StageTimings};

pub const RESULT_FMT: &str = "magpdf-ocr-result/1";

/// 一行预测结果（文件格式）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineRecord {
    /// 四点 `[x, y]`，左上起顺时针。
    pub points: Vec<[f32; 2]>,
    pub text: String,
    pub confidence: f32,
    #[serde(default)]
    pub low_confidence: bool,
    #[serde(default)]
    pub det_score: f32,
    #[serde(default)]
    pub rotated_180: bool,
    #[serde(default)]
    pub vertical: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub char_confidences: Vec<f32>,
}

/// 一页（一张图）的预测结果文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageResultFile {
    pub fmt: String,
    /// 输入文件路径（图片或 PDF）。
    pub source: String,
    /// PDF 页码（从 1 起）；图片为 1。
    #[serde(default = "one")]
    pub page: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f32>,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub models: ModelIdentity,
    pub lines: Vec<LineRecord>,
    #[serde(default)]
    pub dropped: usize,
    /// 方向复核了几行、其中几行没有采用方向分类的判断（`OcrResult` 的同名字段）。
    #[serde(default)]
    pub orientation_checked: usize,
    #[serde(default)]
    pub orientation_overruled: usize,
    #[serde(default)]
    pub timings: StageTimings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_timings: Option<LoadTimings>,
    /// 取得位图花的时间，不含在 `timings` 里：图片是读文件加解码；PDF 是打开文档、加载页面、按 `dpi` 渲染。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquire: Option<Millis>,
    /// `--repeat` 多次运行时每次的耗时（含第一次）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<StageTimings>,
}

fn one() -> usize {
    1
}

impl PageResultFile {
    pub fn from_result(result: &OcrResult, source: &Path, page: usize, dpi: Option<f32>) -> Self {
        Self {
            fmt: RESULT_FMT.to_owned(),
            source: source.display().to_string(),
            page,
            dpi,
            width: result.width,
            height: result.height,
            models: result.models.clone(),
            lines: result
                .lines
                .iter()
                .map(|l| LineRecord {
                    points: l.quad.iter().map(|p| [p.x, p.y]).collect(),
                    text: l.text.clone(),
                    confidence: l.confidence,
                    low_confidence: l.low_confidence,
                    det_score: l.det_score,
                    rotated_180: l.rotated_180,
                    vertical: l.vertical,
                    char_confidences: l.char_confidences.clone(),
                })
                .collect(),
            dropped: result.dropped,
            orientation_checked: result.orientation_checked,
            orientation_overruled: result.orientation_overruled,
            timings: result.timings,
            load_timings: None,
            acquire: None,
            runs: Vec::new(),
        }
    }

    pub fn read(path: &Path) -> Result<Self, OcrError> {
        let text = std::fs::read_to_string(path)?;
        let file: Self = serde_json::from_str(&text)?;
        if file.fmt != RESULT_FMT {
            return Err(OcrError::Manifest(format!(
                "{} 不是 {RESULT_FMT} 文件（fmt={:?}）",
                path.display(),
                file.fmt
            )));
        }
        Ok(file)
    }

    pub fn write(&self, path: &Path) -> Result<(), OcrError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// 文件名主干（不含扩展名），用于与标注对应。
    pub fn source_stem(&self) -> String {
        stem_of(&self.source)
    }
}

/// 标注行的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RegionKind {
    /// 普通文字行，参与全部统计。
    #[default]
    Text,
    /// 印章区域：不转写，只用来统计落在其中的识别结果。
    Seal,
    /// 手写区域：同上（方案 6.6：手写不在 v1 范围）。
    Handwriting,
    /// 标注为 `###` 的忽略区域（PaddleOCR 惯例）。
    Ignore,
}

/// 一条标注。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub transcription: String,
    pub points: Vec<[f32; 2]>,
    #[serde(default)]
    pub kind: RegionKind,
}

/// 一张图的标注。
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotatedImage {
    /// `Label.txt` 里的图像路径（原样）。
    pub image: String,
    pub annotations: Vec<Annotation>,
}

impl AnnotatedImage {
    pub fn stem(&self) -> String {
        stem_of(&self.image)
    }
}

/// PPOCRLabel 一行里的 JSON 元素。
#[derive(Debug, Deserialize)]
struct RawLabel {
    transcription: String,
    points: Vec<[f32; 2]>,
    #[serde(default)]
    key_cls: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

/// 解析 PPOCRLabel `Label.txt` 文本。类别判定：`key_cls`/`kind` 为 `seal`/`handwriting`，
/// 或转写为 `###`（忽略）。
pub fn parse_label_file(text: &str) -> Result<Vec<AnnotatedImage>, OcrError> {
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let (image, json) = line
            .split_once('\t')
            .ok_or_else(|| OcrError::Manifest(format!("标注第 {} 行缺少制表符分隔", lineno + 1)))?;
        let raw: Vec<RawLabel> = serde_json::from_str(json.trim()).map_err(|e| {
            OcrError::Manifest(format!("标注第 {} 行 JSON 无法解析：{e}", lineno + 1))
        })?;
        let annotations = raw
            .into_iter()
            .map(|r| {
                let tag = r
                    .key_cls
                    .as_deref()
                    .or(r.kind.as_deref())
                    .map(|s| s.trim().to_ascii_lowercase());
                let kind = match tag.as_deref() {
                    Some("seal") => RegionKind::Seal,
                    Some("handwriting") => RegionKind::Handwriting,
                    _ if r.transcription == "###" => RegionKind::Ignore,
                    _ => RegionKind::Text,
                };
                Annotation {
                    transcription: r.transcription,
                    points: r.points,
                    kind,
                }
            })
            .collect();
        out.push(AnnotatedImage {
            image: image.trim().to_owned(),
            annotations,
        });
    }
    Ok(out)
}

/// 读取 `Label.txt`。
pub fn read_label_file(path: &Path) -> Result<Vec<AnnotatedImage>, OcrError> {
    let text = std::fs::read_to_string(path)?;
    parse_label_file(&text)
}

/// 把 `[x, y]` 列表转成几何点。
pub fn to_points(points: &[[f32; 2]]) -> Vec<Point> {
    points.iter().map(|p| Point::new(p[0], p[1])).collect()
}

/// 路径的文件名主干（统一 `/` 与 `\`，去扩展名）。
pub fn stem_of(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_owned(),
        _ => name.to_owned(),
    }
}

/// 从文件名推断页面语言：按 `_`、`-`、`.`、`/` 切开后含 `zh`/`en`/`mixed` 片段。
pub fn infer_lang_from_path(path: &str) -> Option<&'static str> {
    for part in path.split(['_', '-', '.', '/', '\\', ' ']) {
        match part.to_ascii_lowercase().as_str() {
            "zh" | "cn" | "ch" => return Some("zh"),
            "en" => return Some("en"),
            "mixed" | "mix" => return Some("mixed"),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ppocrlabel_lines() {
        let text = "imgs/zh_001.png\t[{\"transcription\": \"提单号\", \"points\": [[1,2],[30,2],[30,10],[1,10]]}, {\"transcription\": \"###\", \"points\": [[0,0],[1,0],[1,1],[0,1]]}, {\"transcription\": \"\", \"points\": [[5,5],[9,5],[9,9],[5,9]], \"key_cls\": \"seal\"}]\r\n\nen-002.jpg\t[]\n";
        let imgs = parse_label_file(text).unwrap();
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].stem(), "zh_001");
        assert_eq!(imgs[0].annotations.len(), 3);
        assert_eq!(imgs[0].annotations[0].kind, RegionKind::Text);
        assert_eq!(imgs[0].annotations[1].kind, RegionKind::Ignore);
        assert_eq!(imgs[0].annotations[2].kind, RegionKind::Seal);
        assert_eq!(imgs[1].stem(), "en-002");
        assert!(parse_label_file("no-tab-here").is_err());
    }

    #[test]
    fn stems_and_langs() {
        assert_eq!(stem_of("a/b\\c.tar.png"), "c.tar");
        assert_eq!(stem_of(".hidden"), ".hidden");
        assert_eq!(infer_lang_from_path("out/zh_001.png"), Some("zh"));
        assert_eq!(infer_lang_from_path("EN-rotated-3.png"), Some("en"));
        assert_eq!(infer_lang_from_path("mixed_07.png"), Some("mixed"));
        assert_eq!(infer_lang_from_path("page7.png"), None);
    }

    #[test]
    fn result_file_roundtrip() {
        let json = serde_json::json!({
            "fmt": RESULT_FMT,
            "source": "x/zh_1.png",
            "width": 10, "height": 20,
            "lines": [{"points": [[0,0],[5,0],[5,2],[0,2]], "text": "甲", "confidence": 0.9}]
        });
        let f: PageResultFile = serde_json::from_value(json).unwrap();
        assert_eq!(f.page, 1);
        assert_eq!(f.source_stem(), "zh_1");
        assert!(!f.lines[0].low_confidence);
        let back = serde_json::to_string(&f).unwrap();
        let f2: PageResultFile = serde_json::from_str(&back).unwrap();
        assert_eq!(f, f2);
    }
}
