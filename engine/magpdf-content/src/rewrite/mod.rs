//! 重写层：只改被修改的对象的字节，其余字节原样保留；改过的内容流用增量更新写回文件。
//!
//! - [`plan_page`]：纯函数。输入一页的对应结果和“要改哪些对象”，输出每一段内容流的新字节。
//!   做法是在原字节上打补丁，所以没有修改时输出就是输入；写不出和 PDFium 等价的结果时明确拒绝
//!   （[`RewriteError::Unsupported`]），调用方把这一页交给 PDFium 的生成器。
//! - [`save_incremental`]：把新字节写进文件。内容流被多页共用时另建一条流并重写页字典；
//!   重写不了的页原样留着并报告出来。

mod plan;
mod save;

pub use plan::{EditSet, EditTarget, PageRewrite, RewriteError, plan_page};
pub use save::{PageUpdate, SavedDocument, save_full_rewrite, save_incremental};
