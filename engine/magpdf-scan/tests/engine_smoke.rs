//! 依赖模型文件的冒烟测试：模型或 ONNX Runtime 缺失时跳过（打印原因），不算失败。
//! 本地运行前先执行 `pwsh models/fetch-models.ps1`；CI 不下载模型，这些测试在 CI 上全部跳过。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use magpdf_scan::ocr::models::{Manifest, ModelSelection, find_models_dir};
use magpdf_scan::{ImageView, OcrConfig, OcrEngine};

/// 找到模型并生成配置；缺东西就返回 `None` 并打印原因。
fn config_or_skip(sel: &ModelSelection) -> Option<OcrConfig> {
    let Some(dir) = find_models_dir() else {
        eprintln!("skip: 找不到 models/manifest.json（设置 MAGPDF_MODELS_DIR 或在仓库内运行）");
        return None;
    };
    let manifest = match Manifest::load_dir(&dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skip: 清单无法读取：{e}");
            return None;
        }
    };
    let missing = manifest.missing_files(sel).expect("清单选择失败");
    if !missing.is_empty() {
        eprintln!(
            "skip: 缺少模型文件（运行 models/fetch-models.ps1）：{}",
            missing
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return None;
    }
    Some(manifest.engine_config(sel).expect("生成配置失败"))
}

#[test]
fn blank_page_has_no_lines() {
    let Some(cfg) = config_or_skip(&ModelSelection::default()) else {
        return;
    };
    let mut engine = OcrEngine::new(cfg).expect("加载引擎");
    let load = engine.load_timings();
    assert!(load.total.0 > 0.0);

    // 640×480 的纯白页：不应检出任何文字行，也不应 panic。
    let (w, h) = (640u32, 480u32);
    let data = vec![255u8; (w * h * 3) as usize];
    let view = ImageView::rgb(w, h, &data).unwrap();
    let result = engine.recognize(&view).expect("识别");
    assert_eq!(result.width, w);
    assert_eq!(result.height, h);
    assert!(result.lines.is_empty(), "{:?}", result.lines);
    assert!(result.timings.det.infer.0 > 0.0);
    assert!(!result.models.rec.is_empty());
}

#[test]
fn gray_and_bgra_inputs_are_accepted() {
    let Some(cfg) = config_or_skip(&ModelSelection::default()) else {
        return;
    };
    let mut engine = OcrEngine::new(cfg).expect("加载引擎");
    let (w, h) = (320u32, 200u32);
    let gray = vec![200u8; (w * h) as usize];
    let r = engine
        .recognize(&ImageView::gray(w, h, &gray).unwrap())
        .expect("灰度识别");
    assert!(r.lines.is_empty());

    // 带行填充的 BGRA。
    let stride = (w as usize) * 4 + 16;
    let bgra = vec![255u8; stride * h as usize];
    let r = engine
        .recognize(&ImageView::bgra(w, h, stride, &bgra).unwrap())
        .expect("BGRA 识别");
    assert!(r.lines.is_empty());
}

#[test]
fn engine_is_send() {
    let Some(cfg) = config_or_skip(&ModelSelection::default()) else {
        return;
    };
    let engine = OcrEngine::new(cfg).expect("加载引擎");
    let handle = std::thread::spawn(move || {
        let mut engine = engine;
        let data = vec![255u8; 64 * 64 * 3];
        let view = ImageView::rgb(64, 64, &data).unwrap();
        engine.recognize(&view).map(|r| r.lines.len())
    });
    assert_eq!(handle.join().unwrap().unwrap(), 0);
}
