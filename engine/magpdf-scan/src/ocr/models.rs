//! 模型清单（`models/manifest.json`）：解析、按语言/模型集挑选模型、生成 [`OcrConfig`]、校验文件哈希。
//!
//! 清单格式见 `models/README.md`。模型文件本身不入库，由 `models/fetch-models.ps1` 下载。

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{ClsKind, ClsParams, DetParams, Lang, ModelIdentity, OcrConfig, OcrError, RecParams};

/// 模型类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Det,
    Rec,
    Cls,
    Dict,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractEntry {
    pub from: String,
    pub to: String,
}

/// ONNX Runtime 条目。
#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeEntry {
    pub id: String,
    pub version: String,
    pub license: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub api_version: u32,
    #[serde(default)]
    pub extract: Vec<ExtractEntry>,
}

/// 一个模型或字典文件。
#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub kind: ModelKind,
    #[serde(default)]
    pub purpose: String,
    /// 相对 `models/` 目录。
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub license: String,
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub sets: Vec<String>,
    /// 识别模型的字典文件（相对 `models/`）。
    #[serde(default)]
    pub dict: Option<String>,
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl ModelEntry {
    fn in_set(&self, set: &str) -> bool {
        self.sets.iter().any(|s| s == set)
    }

    fn param_f32(&self, key: &str) -> Option<f32> {
        self.params.get(key)?.as_f64().map(|v| v as f32)
    }

    fn param_usize(&self, key: &str) -> Option<usize> {
        self.params.get(key)?.as_u64().map(|v| v as usize)
    }

    fn param_str(&self, key: &str) -> Option<&str> {
        self.params.get(key)?.as_str()
    }
}

/// 模型清单。
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub fmt: u32,
    pub runtime: RuntimeEntry,
    pub models: Vec<ModelEntry>,
    /// 清单所在目录（`models/`），文件路径以此为基准。
    #[serde(skip)]
    pub base_dir: PathBuf,
}

/// 选择哪些模型组成流水线。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelSelection {
    pub lang: Lang,
    /// 用 `server` 集的检测/识别模型（高精度、体积大，方案 6.5 的可选组件）。
    pub server: bool,
    /// 显式指定模型 id，覆盖自动选择。
    pub det: Option<String>,
    pub rec: Option<String>,
    pub cls: Option<String>,
    /// 不做方向分类。
    pub no_cls: bool,
}

impl Manifest {
    /// 读取清单；`base_dir` 取清单所在目录。
    pub fn load(path: &Path) -> Result<Self, OcrError> {
        let text = std::fs::read_to_string(path).map_err(|source| OcrError::ModelFile {
            path: path.to_path_buf(),
            source,
        })?;
        let mut manifest: Manifest = serde_json::from_str(&text)?;
        if manifest.fmt != 1 {
            return Err(OcrError::Manifest(format!(
                "不支持的清单格式 fmt={}",
                manifest.fmt
            )));
        }
        manifest.base_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(manifest)
    }

    /// 从 `models/` 目录读取 `manifest.json`。
    pub fn load_dir(models_dir: &Path) -> Result<Self, OcrError> {
        Self::load(&models_dir.join("manifest.json"))
    }

    pub fn find(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn path_of(&self, entry: &ModelEntry) -> PathBuf {
        self.base_dir.join(&entry.file)
    }

    /// ONNX Runtime 动态库位置：`runtime.extract` 里以本平台动态库名结尾的条目。
    pub fn dylib_path(&self) -> PathBuf {
        let name = format!(
            "{}onnxruntime{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        );
        self.runtime
            .extract
            .iter()
            .find(|e| e.to.ends_with(&name))
            .map(|e| self.base_dir.join(&e.to))
            .unwrap_or_else(|| self.base_dir.join("onnxruntime").join(&name))
    }

    /// 按选择规则挑出 det / rec / cls 条目。
    pub fn select(
        &self,
        sel: &ModelSelection,
    ) -> Result<(&ModelEntry, &ModelEntry, Option<&ModelEntry>), OcrError> {
        let set = if sel.server { "server" } else { "default" };
        let lang_tag = match sel.lang {
            Lang::En => "en",
            Lang::Zh | Lang::Auto => "zh",
        };

        let det = match &sel.det {
            Some(id) => self.find_kind(id, ModelKind::Det)?,
            None => self
                .models
                .iter()
                .find(|m| m.kind == ModelKind::Det && m.in_set(set))
                .ok_or_else(|| OcrError::Manifest(format!("清单里没有 {set} 集的检测模型")))?,
        };

        let rec = match &sel.rec {
            Some(id) => self.find_kind(id, ModelKind::Rec)?,
            None => {
                // 同一集里支持该语言的识别模型，优先语言列表最短（最专用）的。
                let candidates = || {
                    self.models.iter().filter(|m| {
                        m.kind == ModelKind::Rec && m.langs.iter().any(|l| l == lang_tag)
                    })
                };
                candidates()
                    .filter(|m| m.in_set(set))
                    .min_by_key(|m| m.langs.len())
                    .or_else(|| candidates().min_by_key(|m| m.langs.len()))
                    .ok_or_else(|| {
                        OcrError::Manifest(format!("清单里没有支持 {lang_tag} 的识别模型"))
                    })?
            }
        };

        let cls = if sel.no_cls {
            None
        } else {
            match &sel.cls {
                Some(id) => Some(self.find_kind(id, ModelKind::Cls)?),
                None => self
                    .models
                    .iter()
                    .find(|m| m.kind == ModelKind::Cls && m.in_set("default")),
            }
        };
        Ok((det, rec, cls))
    }

    fn find_kind(&self, id: &str, kind: ModelKind) -> Result<&ModelEntry, OcrError> {
        let entry = self
            .find(id)
            .ok_or_else(|| OcrError::Manifest(format!("清单里没有模型 {id}")))?;
        if entry.kind != kind {
            return Err(OcrError::Manifest(format!(
                "模型 {id} 的类别是 {:?}，不是 {kind:?}",
                entry.kind
            )));
        }
        Ok(entry)
    }

    /// 生成引擎配置：路径、清单里的模型参数、模型身份。不检查文件是否存在（引擎加载时报错）。
    pub fn engine_config(&self, sel: &ModelSelection) -> Result<OcrConfig, OcrError> {
        let (det, rec, cls) = self.select(sel)?;
        let mut config = OcrConfig::new(
            self.path_of(det),
            self.path_of(rec),
            cls.map(|c| self.path_of(c)),
        );
        config.ort_dylib = Some(self.dylib_path());
        config.rec_dict = rec.dict.as_ref().map(|d| self.base_dir.join(d));

        let mut det_params = DetParams::default();
        if let Some(v) = det.param_f32("thresh") {
            det_params.thresh = v;
        }
        if let Some(v) = det.param_f32("box_thresh") {
            det_params.box_thresh = v;
        }
        if let Some(v) = det.param_f32("unclip_ratio") {
            det_params.unclip_ratio = v;
        }
        if let Some(v) = det.param_usize("max_candidates") {
            det_params.max_candidates = v;
        }
        config.det = det_params;

        let mut rec_params = RecParams::default();
        if let Some(v) = rec.param_usize("image_height") {
            rec_params.image_height = v;
        }
        if let Some(v) = rec.param_usize("base_width") {
            rec_params.base_width = v;
        }
        config.rec = rec_params;

        if let Some(c) = cls {
            config.cls = match c.param_str("normalize") {
                Some("half") => ClsParams::legacy(),
                _ => ClsParams {
                    kind: ClsKind::TextLineOri,
                    ..ClsParams::default()
                },
            };
            if let Some(v) = c.param_f32("thresh") {
                config.cls.thresh = v;
            }
        }

        config.identity = ModelIdentity {
            det: det.id.clone(),
            rec: rec.id.clone(),
            cls: cls.map(|c| c.id.clone()),
            version: rec.version.clone(),
            runtime: format!("{} {}", self.runtime.id, self.runtime.version),
        };
        Ok(config)
    }

    /// 校验一个条目的文件大小与 SHA-256。
    pub fn verify(&self, entry: &ModelEntry) -> Result<(), OcrError> {
        let path = self.path_of(entry);
        verify_file(&path, &entry.sha256, entry.size)
    }

    /// 配置用到的全部文件（模型 + 字典）是否都存在；返回缺失的路径。
    pub fn missing_files(&self, sel: &ModelSelection) -> Result<Vec<PathBuf>, OcrError> {
        let (det, rec, cls) = self.select(sel)?;
        let mut paths = vec![self.dylib_path(), self.path_of(det), self.path_of(rec)];
        if let Some(d) = &rec.dict {
            paths.push(self.base_dir.join(d));
        }
        if let Some(c) = cls {
            paths.push(self.path_of(c));
        }
        Ok(paths.into_iter().filter(|p| !p.is_file()).collect())
    }
}

/// 计算文件的 SHA-256（小写十六进制）。
pub fn sha256_file(path: &Path) -> Result<(String, u64), OcrError> {
    let mut file = std::fs::File::open(path).map_err(|source| OcrError::ModelFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    Ok((hex, total))
}

/// 校验文件大小与 SHA-256（不区分大小写）。
pub fn verify_file(path: &Path, expected_sha256: &str, expected_size: u64) -> Result<(), OcrError> {
    let (actual, size) = sha256_file(path)?;
    if size != expected_size {
        return Err(OcrError::Manifest(format!(
            "{} 大小 {size} 字节，清单为 {expected_size}",
            path.display()
        )));
    }
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(OcrError::Manifest(format!(
            "{} 的 SHA-256 与清单不符",
            path.display()
        )));
    }
    Ok(())
}

/// 寻找模型目录：环境变量 `MAGPDF_MODELS_DIR`，否则从当前目录与可执行文件目录向上找含 `models/manifest.json` 的目录。
pub fn find_models_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MAGPDF_MODELS_DIR").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        starts.push(exe_dir);
    }
    for start in starts {
        for dir in start.ancestors() {
            let candidate = dir.join("models");
            if candidate.join("manifest.json").is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "fmt": 1,
      "runtime": { "id": "onnxruntime", "version": "1.31.1", "license": "MIT", "url": "u", "sha256": "x", "size": 1,
                   "extract": [ { "from": "a/lib/onnxruntime.dll", "to": "onnxruntime/onnxruntime.dll" } ] },
      "models": [
        { "id": "det_m", "kind": "det", "file": "ocr/det.onnx", "url": "u", "sha256": "x", "size": 1, "license": "Apache-2.0",
          "sets": ["default"], "params": { "thresh": 0.25, "box_thresh": 0.55, "unclip_ratio": 1.6, "max_candidates": 500 } },
        { "id": "det_s", "kind": "det", "file": "ocr/det_s.onnx", "url": "u", "sha256": "x", "size": 1, "license": "Apache-2.0", "sets": ["server"] },
        { "id": "rec_zh", "kind": "rec", "file": "ocr/rec.onnx", "url": "u", "sha256": "x", "size": 1, "license": "Apache-2.0",
          "version": "v5", "sets": ["default"], "dict": "ocr/d.txt", "langs": ["zh", "en"], "params": { "image_height": 48, "base_width": 320 } },
        { "id": "rec_en", "kind": "rec", "file": "ocr/rec_en.onnx", "url": "u", "sha256": "x", "size": 1, "license": "Apache-2.0",
          "sets": ["default"], "dict": "ocr/en.txt", "langs": ["en"] },
        { "id": "rec_s", "kind": "rec", "file": "ocr/rec_s.onnx", "url": "u", "sha256": "x", "size": 1, "license": "Apache-2.0",
          "sets": ["server"], "dict": "ocr/d.txt", "langs": ["zh", "en"] },
        { "id": "cls_new", "kind": "cls", "file": "ocr/cls.onnx", "url": "u", "sha256": "x", "size": 1, "license": "Apache-2.0",
          "sets": ["default"], "params": { "normalize": "imagenet" } },
        { "id": "cls_old", "kind": "cls", "file": "ocr/cls_old.onnx", "url": "u", "sha256": "x", "size": 1, "license": "Apache-2.0",
          "sets": ["legacy"], "params": { "normalize": "half" } }
      ]
    }"#;

    fn sample() -> Manifest {
        let mut m: Manifest = serde_json::from_str(SAMPLE).unwrap();
        m.base_dir = PathBuf::from("M");
        m
    }

    #[test]
    fn selects_by_lang_and_set() {
        let m = sample();
        let (d, r, c) = m.select(&ModelSelection::default()).unwrap();
        assert_eq!((d.id.as_str(), r.id.as_str()), ("det_m", "rec_zh"));
        assert_eq!(c.map(|c| c.id.as_str()), Some("cls_new"));

        let (_, r, _) = m
            .select(&ModelSelection {
                lang: Lang::En,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(r.id, "rec_en", "英文优先选专用模型");

        let (d, r, c) = m
            .select(&ModelSelection {
                lang: Lang::En,
                server: true,
                no_cls: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            (d.id.as_str(), r.id.as_str()),
            ("det_s", "rec_s"),
            "server 集没有英文专用模型时退回同集通用模型"
        );
        assert!(c.is_none());

        let err = m
            .select(&ModelSelection {
                det: Some("rec_zh".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, OcrError::Manifest(_)));
    }

    #[test]
    fn config_applies_params_and_paths() {
        let m = sample();
        let cfg = m.engine_config(&ModelSelection::default()).unwrap();
        assert_eq!(cfg.det_model, PathBuf::from("M").join("ocr/det.onnx"));
        assert_eq!(cfg.rec_dict, Some(PathBuf::from("M").join("ocr/d.txt")));
        assert_eq!(
            cfg.ort_dylib,
            Some(PathBuf::from("M").join("onnxruntime/onnxruntime.dll"))
        );
        assert_eq!(cfg.det.thresh, 0.25);
        assert_eq!(cfg.det.max_candidates, 500);
        assert_eq!(cfg.cls.kind, ClsKind::TextLineOri);
        assert_eq!(cfg.identity.rec, "rec_zh");
        assert_eq!(cfg.identity.version, "v5");
        assert_eq!(cfg.identity.runtime, "onnxruntime 1.31.1");

        let cfg = m
            .engine_config(&ModelSelection {
                cls: Some("cls_old".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(cfg.cls.kind, ClsKind::LegacyV2);
        assert_eq!(cfg.cls.thresh, 0.9);
    }

    #[test]
    fn sha256_of_temp_file() {
        let dir = std::env::temp_dir().join(format!("magpdf-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        let (hex, size) = sha256_file(&path).unwrap();
        assert_eq!(size, 3);
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(verify_file(&path, &hex.to_uppercase(), 3).is_ok());
        assert!(verify_file(&path, &hex, 4).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
