//! 重写层：把一页的内容流按对象拆分，只替换改过的片段，其余字节原样保留。
//!
//! 片段模型：
//! - 一个对象占的字节从它的第一个记号**之前**的空白开始（`leading_start`），到最后一个记号**之后**
//!   的空白开始为止；对象之间的状态操作符原样留在原地，所以"删除"就是删掉这一段。
//! - 移动/新增：`q <cm> … Q` 包住**绘制段**（路径构造到绘制），状态设置留在 `q` 外面，
//!   颜色、线宽、`Tf` 等不会被包进 `q … Q` 而丢掉。
//! - 改色：在绘制段前插入颜色操作符，后接原颜色操作符还原；下一对象若自己设色则跳过还原。
//! - 删除文字：用 `Tr 3`（不画）替代显示，字形仍推进文字矩阵，后续字形位置不变；
//!   同一 `BT` 里的其它显示操作不受影响。
//! - 每个 `Q` 前补一个 `BT/ET` 或什么都不补取决于当前是否在文字块里；插入的内容始终在
//!   `BT … ET` 之外，避免 `q/Q` 落进文字块（规范禁止、PDFium 也不认）。
//!
//! 字节切分是纯函数（`plan`），写增量更新见 `save`。

mod plan;
mod save;

pub use plan::{
    EditSet, EditTarget, PageRewrite, RewriteError, RewriteInput, plan_page, verify_tokens,
};
pub use save::{SaveMode, SavedDocument, StreamReplacement, save_full_rewrite, save_incremental};
