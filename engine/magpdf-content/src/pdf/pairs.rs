//! 页树里的页与 PDFium 的同号页是不是同一页（V17 任务书：逐页验证两边读到的是同一页）。
//!
//! [`crate::pdf::pages`] 照 PDFium 的规则数页、找页，按理两边的页序一致。为了不靠“按理”，这里再用
//! PDFium 报的页面宽高和旋转逐页核对：尺寸按 PDFium 的 `CPDF_Page::UpdateDimensions` 算（MediaBox
//! 为空时按 612×792，CropBox 与 MediaBox 取交集，旋转 90°/270° 时宽高对调）。对不上的页整页回退。

use crate::pdf::pages::{PageInfo, page_geometry};
use crate::pdf::reader::PdfDocument;

/// 几何容差（点）。两边都是单精度浮点，算法相同，留一点余量给取整顺序的差别。
pub const GEOMETRY_TOLERANCE: f32 = 0.05;

/// 核对页树里的一页与 PDFium 的同序号页是不是同一页（旋转、尺寸）。不一致时返回原因。
pub fn check_page(
    doc: &PdfDocument,
    info: &PageInfo,
    g: &pdfium_sys::PageGeometry,
) -> Result<(), String> {
    let ours = page_geometry(doc, &info.dict);
    let theirs = g.rotation.rem_euclid(4);
    if ours.rotation != theirs {
        return Err(format!(
            "旋转 {}° ≠ PDFium {}°",
            ours.rotation * 90,
            theirs * 90
        ));
    }
    let close = |a: f32, b: f32| (a - b).abs() <= GEOMETRY_TOLERANCE;
    if !close(ours.width, g.width) || !close(ours.height, g.height) {
        return Err(format!(
            "尺寸 {}×{} ≠ PDFium {}×{}",
            ours.width, ours.height, g.width, g.height
        ));
    }
    Ok(())
}
