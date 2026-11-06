//! bindgen 生成的原始 FFI 绑定，一比一对应 PDFium 公共头文件。
//!
//! 重新生成：`engine/pdfium-sys/regen-bindings.ps1`。切换到自家 PDFium 分支后，
//! `MAG_*` 扩展接口也从这里导出。工作区其他 crate 不应直接使用本模块，
//! 应通过 [`crate`] 顶层的安全封装调用（《工程规范》4：`unsafe` 只允许出现在 `pdfium-sys`）。
#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    improper_ctypes,
    unused_qualifications,
    missing_debug_implementations,
    clippy::all,
    clippy::pedantic,
    rustdoc::all
)]

include!("bindings.rs");
