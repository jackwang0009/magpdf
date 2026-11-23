//! 恢复文件（方案 9.3；V06 演示级实现）。
//!
//! 引擎在打开文档、翻页、改变视图时把"最近一次请求渲染的页面和视图"写进会话的恢复文件（JSON，写临时
//! 文件后替换）。崩溃或被杀后重启，引擎读回它，重新打开文档，并通过 `session.hello` 的返回值告诉主进程
//! 恢复了哪些文档；界面收到 `engine.restored` 后重新取瓦片。
//!
//! 有没保存的修改的文档，PDFium 线程空闲时每隔一段时间把它完整写一份自动备份（`snapshot`，见
//! `pdfium_thread::SnapshotConfig`），记在这里；重启时从备份打开。方案 9.3 说的是只写相对原文件新增的
//! 增量部分，原型用的是完整重写（增量保存还没有做，V06 报告第 15、17 节）。

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
    /// 没保存的修改的自动备份（完整的 PDF 文件，方案 9.3）。重新打开时读它，路径仍记作 `path`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
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

    /// 记录（或更新）一个文档。只更新视图时（`doc.snapshot` 为空）原来记的自动备份留着。
    pub fn upsert(&mut self, mut doc: DocRecovery) {
        if let Some(existing) = self.state.docs.iter_mut().find(|d| d.path == doc.path) {
            if doc.snapshot.is_none() {
                doc.snapshot = existing.snapshot.take();
            }
            *existing = doc;
        } else {
            self.state.docs.push(doc);
        }
    }

    /// 记下这份文档新的自动备份，返回换下来的旧备份（调用方删掉它）。文档不在记录里时什么也不做。
    pub fn set_snapshot(&mut self, path: &str, snapshot: String) -> Option<String> {
        let doc = self.state.docs.iter_mut().find(|d| d.path == path)?;
        doc.snapshot.replace(snapshot)
    }

    /// 去掉这份文档的自动备份（存好了、关掉了），返回原来记的（调用方删掉它）。
    pub fn take_snapshot(&mut self, path: &str) -> Option<String> {
        self.state
            .docs
            .iter_mut()
            .find(|d| d.path == path)?
            .snapshot
            .take()
    }

    /// 文档关闭后不再需要恢复它。
    pub fn remove(&mut self, path: &str) {
        self.state.docs.retain(|d| d.path != path);
    }

    /// 文档另存到了 `to`：以后恢复的是新文件，视图照旧。`keep_old` 为真时（同一个文件还开着别的 docId）
    /// 原来那一条也留着。
    pub fn rename(&mut self, from: &str, to: &str, keep_old: bool) {
        let Some(old) = self.state.docs.iter().find(|d| d.path == from).cloned() else {
            return;
        };
        if !keep_old {
            self.remove(from);
        }
        self.upsert(DocRecovery {
            path: to.to_owned(),
            ..old
        });
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
            snapshot: None,
        });
        file.save().unwrap();
        assert_eq!(
            file.set_snapshot(r"C:\samples\a.pdf", "b1.pdf".into()),
            None
        );
        file.upsert(DocRecovery {
            path: r"C:\samples\a.pdf".into(),
            page_id: 1,
            scale: 2.0,
            origin_x: 512,
            origin_y: 0,
            snapshot: None,
        });
        file.save().unwrap();
        let mut reloaded = RecoveryFile::load(&path);
        assert_eq!(reloaded.state().docs.len(), 1);
        assert_eq!(reloaded.state().docs[0].page_id, 1);
        assert_eq!(
            reloaded.state().docs[0].snapshot.as_deref(),
            Some("b1.pdf"),
            "只换视图时自动备份留着"
        );
        assert_eq!(reloaded.state().revision, 2);
        assert_eq!(
            reloaded.set_snapshot(r"C:\samples\a.pdf", "b2.pdf".into()),
            Some("b1.pdf".into())
        );
        assert_eq!(
            reloaded.take_snapshot(r"C:\samples\a.pdf"),
            Some("b2.pdf".into())
        );
        assert_eq!(reloaded.state().docs[0].snapshot, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
