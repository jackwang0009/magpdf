//! 页面树与 PDFium 页序的配对（V17 第 2 条"逐页验证两边读到的是同一页、同样的流"）。
//!
//! 我们的 `pdf::pages` 按 PDF 规范的页树顺序列页；PDFium 的 `FPDF_GetPageCount`/`FPDF_LoadPage`
//! 顺序与之一致（同样深度优先遍历 `/Kids`）。为了不靠"应该一致"来保证，这里用 PDFium 给出的
//! 页面尺寸、旋转与边界框逐个核对：不一致的页不计入任何统计，单列"页序不一致"。

use crate::pdf::object::PdfObject;
use crate::pdf::pages::{PageInfo, walk_pages};
use crate::pdf::reader::PdfDocument;

/// 几何容差（点）。PDFium 计算页面边界时用定点/浮点混算，个别文件差 1e-3 量级。
pub const GEOMETRY_TOLERANCE: f32 = 0.05;

/// 一页的配对结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PagePair {
    /// 页树里的页（`/Contents`、`/Resources`）。
    pub info: PageInfo,
    /// 是否通过了与 PDFium 的几何核对。
    pub consistent: bool,
    /// 不一致时写明原因。
    pub mismatch: Option<String>,
}

/// 批量配对：`pdfium` 必须与 `pages` 等长（由调用方从 `pdfium_sys::Document` 取几何）。
pub fn pair_pages(doc: &PdfDocument, pdfium: &[pdfium_sys::PageGeometry]) -> Vec<PagePair> {
    let pages = walk_pages(doc, pdfium.len().max(1).saturating_mul(2));
    let mut out = Vec::with_capacity(pdfium.len());
    for g in pdfium {
        match pages.get(g.index) {
            None => out.push(PagePair {
                info: PageInfo {
                    index: g.index,
                    ..PageInfo::default()
                },
                consistent: false,
                mismatch: Some("页树里的页数少于 PDFium".to_owned()),
            }),
            Some(info) => {
                let (consistent, mismatch) = check(doc, info, g);
                out.push(PagePair {
                    info: info.clone(),
                    consistent,
                    mismatch,
                });
            }
        }
    }
    for info in pages.iter().skip(pdfium.len()) {
        out.push(PagePair {
            info: info.clone(),
            consistent: false,
            mismatch: Some("页树里的页数多于 PDFium".to_owned()),
        });
    }
    out
}

/// 核对页树里的一页与 PDFium 的同序号页是不是同一页（旋转、尺寸）。不一致时返回原因。
pub fn check_page(
    doc: &PdfDocument,
    info: &PageInfo,
    g: &pdfium_sys::PageGeometry,
) -> Result<(), String> {
    match check(doc, info, g) {
        (true, _) => Ok(()),
        (false, why) => Err(why.unwrap_or_else(|| "页面几何不一致".to_owned())),
    }
}

fn check(
    doc: &PdfDocument,
    info: &PageInfo,
    g: &pdfium_sys::PageGeometry,
) -> (bool, Option<String>) {
    // 页对象的 `/Rotate`（可能继承）与 PDFium 的旋转值。
    let rotate = inherited_rotate(doc, info);
    let rotate_norm = ((rotate % 360) + 360) % 360;
    let pdfium_deg = g.rotation.rem_euclid(4) * 90;
    if rotate_norm != i64::from(pdfium_deg) {
        return (
            false,
            Some(format!("旋转 {rotate_norm}° ≠ PDFium {pdfium_deg}°")),
        );
    }
    // 尺寸：MediaBox 尺寸（旋转 90/270 时交换）与 PDFium 的宽高对比。
    let Some((mw, mh)) = media_size(doc, info) else {
        return (true, None);
    };
    let (w, h) = if pdfium_deg == 90 || pdfium_deg == 270 {
        (mh, mw)
    } else {
        (mw, mh)
    };
    // 有 CropBox 时 PDFium 的宽高取自 CropBox，允许"MediaBox 或 CropBox 任一匹配"。
    let crop = crop_size(doc, info);
    let ok = |a: f32, b: f32| (a - b).abs() <= GEOMETRY_TOLERANCE;
    let size_ok = (ok(w, g.width) && ok(h, g.height))
        || crop.is_some_and(|(cw, ch)| {
            let (cw, ch) = if pdfium_deg == 90 || pdfium_deg == 270 {
                (ch, cw)
            } else {
                (cw, ch)
            };
            ok(cw, g.width) && ok(ch, g.height)
        });
    if !size_ok {
        return (
            false,
            Some(format!("尺寸 {w}×{h} ≠ PDFium {}×{}", g.width, g.height)),
        );
    }
    (true, None)
}

fn page_dict(doc: &PdfDocument, info: &PageInfo) -> Option<crate::pdf::PdfDict> {
    doc.object(info.number).and_then(|o| match o {
        PdfObject::Dict(d) => Some(d),
        _ => None,
    })
}

/// 沿 `/Parent` 链找继承的 `/Rotate`。
fn inherited_rotate(doc: &PdfDocument, info: &PageInfo) -> i64 {
    let mut node = page_dict(doc, info);
    for _ in 0..64 {
        let Some(dict) = node else { break };
        if let Some(v) = dict.get(b"Rotate").and_then(PdfObject::as_int) {
            return v;
        }
        node = dict
            .get(b"Parent")
            .and_then(|p| doc.deref(p))
            .and_then(|o| o.as_dict().cloned());
    }
    0
}

fn box_size(doc: &PdfDocument, info: &PageInfo, key: &[u8]) -> Option<(f32, f32)> {
    let mut node = page_dict(doc, info);
    for _ in 0..64 {
        let dict = node?;
        if let Some(v) = dict.get(key).and_then(|v| doc.resolve_or_none(Some(v))) {
            let arr = v.as_array()?;
            let nums: Vec<f32> = arr
                .iter()
                .filter_map(|o| doc.resolve_or_none(Some(o)).and_then(|o| o.as_f64()))
                .map(|v| v as f32)
                .collect();
            if nums.len() == 4 {
                return Some(((nums[2] - nums[0]).abs(), (nums[3] - nums[1]).abs()));
            }
            return None;
        }
        node = dict
            .get(b"Parent")
            .and_then(|p| doc.deref(p))
            .and_then(|o| o.as_dict().cloned());
    }
    None
}

fn media_size(doc: &PdfDocument, info: &PageInfo) -> Option<(f32, f32)> {
    box_size(doc, info, b"MediaBox")
}

fn crop_size(doc: &PdfDocument, info: &PageInfo) -> Option<(f32, f32)> {
    box_size(doc, info, b"CropBox")
}
