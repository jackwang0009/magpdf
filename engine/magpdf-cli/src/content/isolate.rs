//! 在子进程里处理单个文件：PDFium 崩溃或卡死只影响这一个文件。
//!
//! 做法与 `roundtrip --isolate` 相同（那边的代码和它的结果类型绑在一起，这里另写一份）。

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// 子进程的结局。
#[derive(Debug)]
pub enum ChildOutcome {
    /// 正常退出，标准输出的全部内容。
    Finished(Vec<u8>),
    /// 异常退出（PDFium 崩溃等）。
    Crashed {
        code: Option<i32>,
        stderr: String,
    },
    TimedOut,
    SpawnFailed(String),
}

/// 运行子进程并等它结束，超时则终止。标准输出和标准错误边等边读，避免子进程写满管道后阻塞。
pub fn run_child(cmd: &mut Command, timeout: Duration) -> ChildOutcome {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return ChildOutcome::SpawnFailed(e.to_string()),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = thread::spawn(move || read_all(stdout));
    let err_reader = thread::spawn(move || read_all(stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    match status {
        None => ChildOutcome::TimedOut,
        Some(status) if status.success() => ChildOutcome::Finished(stdout),
        Some(status) => ChildOutcome::Crashed {
            code: status.code(),
            stderr: stderr_tail(&stderr),
        },
    }
}

fn read_all(reader: Option<impl Read>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut reader) = reader {
        let _ = reader.read_to_end(&mut buf);
    }
    buf
}

/// 标准错误的最后 300 个字符，压成一行。
pub fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let flat = text.trim().replace(['\r', '\n'], " ");
    let chars: Vec<char> = flat.chars().collect();
    let start = chars.len().saturating_sub(300);
    chars[start..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_keeps_the_end_on_one_line() {
        assert_eq!(stderr_tail(b"  a\r\nb\n"), "a  b");
        let long = "x".repeat(400) + "END";
        let tail = stderr_tail(long.as_bytes());
        assert_eq!(tail.chars().count(), 300);
        assert!(tail.ends_with("END"));
        assert_eq!(stderr_tail(b""), "");
    }

    #[test]
    fn child_that_cannot_start_is_reported() {
        let mut cmd = Command::new("magpdf-no-such-program-for-test");
        assert!(matches!(
            run_child(&mut cmd, Duration::from_secs(5)),
            ChildOutcome::SpawnFailed(_)
        ));
    }
}
