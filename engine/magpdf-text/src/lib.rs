//! 文字服务（方案 3.1、第 5 节）：文字块识别（5.1）、修改已有文字的三级回退（5.2）、
//! 排版（5.3）、字体来源与子集化（5.4）。P0 阶段对应验证项 V07（聚类原型）和 V08（"改一行字"端到端）。
//!
//! 输入来自 [`pdfium_sys::TextPage`] 的字符级信息；本 crate 不直接调用 PDFium 的 FFI。
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {}
}
