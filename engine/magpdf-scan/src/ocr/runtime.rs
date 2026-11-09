//! ONNX Runtime 环境与会话。
//!
//! `ort` 以 `load-dynamic` 方式在运行时加载 `onnxruntime.dll`（方案 2.2：原生 ONNX Runtime，默认 CPU），
//! 构建时不下载二进制。`ort` 限制每个进程只能创建一个 `Environment`，所以这里用 `OnceLock` 持有；
//! 第一次调用决定动态库路径，之后的调用沿用同一个环境。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use ort::environment::Environment;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

use super::OcrError;

/// 进程唯一的环境；只缓存成功结果，失败（如动态库路径错误）允许用别的路径重试。
static ENVIRONMENT: OnceLock<Environment> = OnceLock::new();
/// 串行化环境创建：`ort` 对第二次 `build()` 直接报错，所以不能让两个线程同时创建。
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// 解析 ONNX Runtime 动态库路径：显式参数 > 环境变量 `ORT_DYLIB_PATH` > 可执行文件旁的 `onnxruntime.dll`。
pub fn resolve_dylib(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH").filter(|s| !s.is_empty()) {
        return PathBuf::from(p);
    }
    PathBuf::from(format!(
        "{}onnxruntime{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

/// 取得（必要时创建）进程级的 ONNX Runtime 环境。遥测关闭：产品承诺不上传任何数据（方案 13.4）。
pub(crate) fn environment(dylib: Option<&Path>) -> Result<Environment, OcrError> {
    if let Some(env) = ENVIRONMENT.get() {
        return Ok(env.clone());
    }
    let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(env) = ENVIRONMENT.get() {
        return Ok(env.clone());
    }
    let path = resolve_dylib(dylib);
    let started = Instant::now();
    let env = ort::init_from(&path)
        .map_err(|e| OcrError::OrtInit(e.to_string()))?
        .with_name("magpdf")
        .with_telemetry(false)
        .build()
        .map_err(|e| OcrError::OrtInit(e.to_string()))?;
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "ONNX Runtime 环境已创建"
    );
    Ok(ENVIRONMENT.get_or_init(|| env).clone())
}

/// ONNX Runtime 的版本字符串（动态库报告的构建信息）。
pub fn runtime_build_info() -> String {
    ort::info().to_string()
}

/// 会话线程设置：`intra_threads = 0` 表示交给 ONNX Runtime 默认（物理核数）。
pub(crate) fn load_session(
    env: &Environment,
    path: &Path,
    intra_threads: usize,
) -> Result<Session, OcrError> {
    let meta = std::fs::metadata(path).map_err(|source| OcrError::ModelFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !meta.is_file() {
        return Err(OcrError::ModelFile {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "不是文件"),
        });
    }
    let mut builder = Session::builder(env)?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(ort::Error::from)?;
    if intra_threads > 0 {
        builder = builder
            .with_intra_threads(intra_threads)
            .map_err(ort::Error::from)?;
    }
    Ok(builder.commit_from_file(path)?)
}

/// 会话的第一个输入名；模型没有输入时报错。
pub(crate) fn first_input_name(session: &Session, model: &'static str) -> Result<String, OcrError> {
    session
        .inputs()
        .first()
        .map(|i| i.name().to_owned())
        .ok_or_else(|| OcrError::ModelShape {
            model,
            detail: "模型没有输入".to_owned(),
        })
}
