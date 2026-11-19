//! 上次没有正常退出时留下的恢复文件（方案 9.3 第 3 条，《交互设计》6.9“崩溃恢复”）。
//!
//! 每次运行的恢复文件按主进程的 PID 分开（`recovery/session-<PID>.json`），正常退出时删掉。主进程被强制
//! 结束、崩溃或者断电时它会留下来。下次启动时把这些文件找出来，列出当时开着的文档，用户选了之后重新打开，
//! 不管选不选都把文件删掉。还在运行的 MagPDF（另一个窗口）的恢复文件不算。
//!
//! 现在的恢复文件只记了开着哪些文档、看到哪一页（引擎还不把没保存的修改写进去），所以“恢复”是重新打开
//! 磁盘上的文件，没保存的修改恢复不了。界面上要照实说。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde_json::Value;

/// 一份可以恢复的文档。`id` 只在这一次运行里有效。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverableDocument {
    pub id: String,
    /// 文件名（不带目录），对话框里显示
    pub name: String,
    /// 恢复文件最后一次写入的时间，Unix 毫秒
    pub modified_ms: u64,
    #[serde(skip)]
    pub path: PathBuf,
}

/// 启动时找到的、上次留下的恢复文件和里面的文档。
#[derive(Debug, Default)]
pub struct Leftover {
    pub files: Vec<PathBuf>,
    pub documents: Vec<RecoverableDocument>,
}

/// 找 `dir` 里别的会话留下的恢复文件。`current` 是本次运行自己的那一份；`alive` 判断某个 PID 是不是一个
/// 还在运行的 MagPDF。同一份文档出现在好几个文件里时只列一次，用最近的时间；磁盘上已经没有的文件不列。
pub fn scan(dir: &Path, current: &Path, alive: impl Fn(u32) -> bool) -> Leftover {
    let mut leftover = Leftover::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return leftover;
    };
    let mut found: Vec<(PathBuf, u64)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current {
            continue;
        }
        let Some(pid) = session_pid(&path) else {
            continue;
        };
        if alive(pid) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        found.push((path, modified));
    }
    found.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    for (file, modified) in found {
        for doc in documents_in(&file) {
            let known = leftover
                .documents
                .iter()
                .any(|d| d.path.as_os_str().eq_ignore_ascii_case(doc.as_os_str()));
            if known || !doc.is_file() {
                continue;
            }
            let name = doc
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            leftover.documents.push(RecoverableDocument {
                id: leftover.documents.len().to_string(),
                name,
                modified_ms: modified,
                path: doc,
            });
        }
        leftover.files.push(file);
    }
    leftover
}

/// `session-<PID>.json` 里的 PID。
fn session_pid(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("session-")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

/// 恢复文件里记的文档路径。读不了、格式不对的当作没有文档（文件照样删）。
fn documents_in(file: &Path) -> Vec<PathBuf> {
    let Some(state) = std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return Vec::new();
    };
    state["docs"]
        .as_array()
        .map(|docs| {
            docs.iter()
                .filter_map(|doc| doc["path"].as_str().map(PathBuf::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 这个 PID 现在是不是一个 MagPDF 主进程（可执行文件名和本程序的一样）。PID 被别的程序复用了不算。
pub fn is_running_magpdf(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    let Some(own) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_os_string()))
    else {
        return false;
    };
    // SAFETY: 只查询进程的可执行文件名；句柄用完就关。
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut buf = [0u16; 1024];
        let mut len = u32::try_from(buf.len()).unwrap_or(0);
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return false;
        }
        let image = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
        image
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(&own))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, docs: &[&Path]) -> PathBuf {
        let path = dir.join(name);
        let docs: Vec<Value> = docs
            .iter()
            .map(|d| serde_json::json!({ "path": d.to_string_lossy(), "pageId": 0 }))
            .collect();
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({ "docs": docs })).unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn leftover_sessions_list_existing_documents_once() {
        let dir = std::env::temp_dir().join(format!("magpdf-leftover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        std::fs::write(&a, b"%PDF").unwrap();
        std::fs::write(&b, b"%PDF").unwrap();
        let gone = dir.join("gone.pdf");

        let current = write(&dir, "session-1.json", &[&a]);
        let crashed = write(&dir, "session-2.json", &[&a, &gone]);
        let older = write(&dir, "session-3.json", &[&b, &a]);
        let running = write(&dir, "session-4.json", &[&b]);
        std::fs::write(dir.join("notes.json"), b"{}").unwrap();
        std::fs::write(dir.join("session-5.json"), b"not json").unwrap();

        let found = scan(&dir, &current, |pid| pid == 4);
        let names: Vec<&str> = found.documents.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"a.pdf") && names.contains(&"b.pdf"));
        assert!(!found.files.contains(&current), "本次运行自己的不算");
        assert!(!found.files.contains(&running), "还在运行的 MagPDF 的不算");
        assert!(found.files.contains(&crashed) && found.files.contains(&older));
        assert!(
            found.files.contains(&dir.join("session-5.json")),
            "坏掉的文件也要删"
        );
        assert!(!found.files.contains(&dir.join("notes.json")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn this_process_counts_as_a_running_magpdf_only_by_its_own_name() {
        assert!(is_running_magpdf(std::process::id()));
        assert!(!is_running_magpdf(u32::MAX));
    }
}
