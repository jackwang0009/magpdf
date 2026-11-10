//! 恢复文件（方案 9.3；V06 演示级实现）。
//!
//! 引擎在打开文档、翻页、改变视图时把"最近一次请求渲染的页面和视图"写进会话的恢复文件（JSON，写临时
//! 文件后替换）。崩溃或被杀后重启，引擎读回它，重新打开文档，并通过 `session.hello` 的返回值告诉主进程
//! 恢复了哪些文档；界面收到 `engine.restored` 后重新取瓦片。
//!
//! 真实实现只写相对原文件新增的增量部分（`FPDF_SaveAsCopy` + `FPDF_INCREMENTAL`）。P0 原型没有编辑
//! 命令，所以只需记住文件路径与视图。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("读写恢复文件失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("恢复文件格式错误：{0}")]
    Json(#[from] serde_json::Error),
}

/// 一个文档的恢复信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocRecovery {
    pub path: String,
    pub page_id: u32,
    pub scale: f32,
    /// 视口的左上角在整页像素网格上的位置（与最后一次 `render.tiles` 一致）。
    pub origin_x: u32,
    pub origin_y: u32,
}

/// 恢复文件的内容。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryState {
    #[serde(default)]
    pub docs: Vec<DocRecovery>,
    /// 写入次数，演示里用来确认引擎确实定期落盘。
    #[serde(default)]
    pub revision: u64,
}

/// 恢复文件。
#[derive(Debug, Clone)]
pub struct RecoveryFile {
    path: PathBuf,
    state: RecoveryState,
}

impl RecoveryFile {
    /// 从文件读回；文件不存在或损坏时视为空。
    pub fn load(path: &Path) -> Self {
        let state = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path: path.to_owned(),
            state,
        }
    }

    pub fn state(&self) -> &RecoveryState {
        &self.state
    }

    /// 记录（或更新）一个文档。
    pub fn upsert(&mut self, doc: DocRecovery) {
        if let Some(existing) = self.state.docs.iter_mut().find(|d| d.path == doc.path) {
            *existing = doc;
        } else {
            self.state.docs.push(doc);
        }
    }

    #[expect(dead_code, reason = "V06 原型还没有 doc.close，文档只开不关")]
    pub fn remove(&mut self, path: &str) {
        self.state.docs.retain(|d| d.path != path);
    }

    /// 落盘：写临时文件后替换，避免写到一半被杀留下半个文件。
    pub fn save(&mut self) -> Result<(), RecoveryError> {
        self.state.revision += 1;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.state)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_disk() {
        let dir = std::env::temp_dir().join(format!("magpdf-recovery-{}", std::process::id()));
        let path = dir.join("session.json");
        let mut file = RecoveryFile::load(&path);
        assert!(file.state().docs.is_empty());
        file.upsert(DocRecovery {
            path: r"C:\samples\a.pdf".into(),
            page_id: 0,
            scale: 1.0,
            origin_x: 0,
            origin_y: 0,
        });
        file.save().unwrap();
        file.upsert(DocRecovery {
            path: r"C:\samples\a.pdf".into(),
            page_id: 1,
            scale: 2.0,
            origin_x: 512,
            origin_y: 0,
        });
        file.save().unwrap();
        let reloaded = RecoveryFile::load(&path);
        assert_eq!(reloaded.state().docs.len(), 1);
        assert_eq!(reloaded.state().docs[0].page_id, 1);
        assert_eq!(reloaded.state().revision, 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
