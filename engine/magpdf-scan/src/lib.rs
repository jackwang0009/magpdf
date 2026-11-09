//! 扫描件服务（方案 3.1、第 6 节）：识别扫描页（6.1）、OCR 流程（6.2、6.5）、背景修补原型（6.4）。
//! P0 阶段对应验证项 V09（OCR 集成与实测）、V10（GPU 加速评估）、V11（背景修补原型）。
//!
//! 本 crate 不含 `unsafe`：ONNX Runtime 通过 `ort` 的安全 API 调用，动态库在运行时加载。
#![forbid(unsafe_code)]

pub mod ocr;

pub use ocr::{
    ImageView, Lang, Millis, ModelIdentity, OcrConfig, OcrEngine, OcrError, OcrLine, OcrResult,
    PixelFormat, Point, Quad, StageTimings,
};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {}
}
