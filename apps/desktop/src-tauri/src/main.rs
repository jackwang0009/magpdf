//! MagPDF 桌面主进程。
//!
//! - `magpdf-desktop [文档.pdf ...]`：打开窗口（Tauri 2 + WebView2），启动并监督引擎进程，
//!   把界面的请求转给引擎（[`app`]）。命令行给了 PDF 就直接打开。
//! - `magpdf-desktop --bench`：不开窗口，拉起引擎测 V06 的三条通过线与 I1、I2（[`host`]）。

// 发布版是窗口程序，不带控制台；调试版留着控制台，方便看日志。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod console;
mod host;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use host::{Host, HostConfig};

const USAGE: &str = "\
用法：
  magpdf-desktop [文档.pdf ...]   打开窗口；给了文档就直接打开
  magpdf-desktop --bench          V06 基准，不开窗口
环境变量：
  MAGPDF_ENGINE_BIN     引擎可执行文件（默认和本程序在同一个目录）
  MAGPDF_DATA_DIR       数据目录（默认 %LOCALAPPDATA%\\MagPDF 和 %APPDATA%\\MagPDF）
  MAGPDF_LOG            日志级别（默认 info）
  MAGPDF_WEBVIEW_DEBUG_PORT   打开 WebView2 的远程调试端口，界面端到端测试用
  PDFIUM_PREBUILT_DIR、MAGPDF_BENCH_PDF   只有 --bench 用";

/// 命令行的意思。
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    /// 打开窗口，并打开这些文档。
    Window(Vec<PathBuf>),
    Bench,
    Help,
    /// 不认识的开关。
    Unknown(String),
}

fn parse_args(args: impl Iterator<Item = String>) -> Invocation {
    let mut documents = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--bench" => return Invocation::Bench,
            "--help" | "-h" => return Invocation::Help,
            flag if flag.starts_with("--") => return Invocation::Unknown(arg),
            _ => documents.push(PathBuf::from(arg)),
        }
    }
    Invocation::Window(documents)
}

fn main() -> ExitCode {
    console::attach_to_parent();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MAGPDF_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let outcome = match parse_args(std::env::args().skip(1)) {
        Invocation::Window(documents) => app::run(documents),
        Invocation::Bench => run_bench(),
        Invocation::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Invocation::Unknown(flag) => {
            eprintln!("不认识的参数 {flag}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_bench() -> Result<()> {
    let cfg = HostConfig::from_env_or_defaults()?;
    if let Some(parent) = cfg.out_json.parent() {
        fs::create_dir_all(parent).ok();
    }
    let out = cfg.out_json.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?;
    let report = runtime.block_on(async {
        let mut host = Host::new(cfg);
        host.run_bench().await
    })?;
    let json = serde_json::to_string_pretty(&report)?;
    fs::write(&out, &json).with_context(|| format!("写 {}", out.display()))?;
    println!("{json}");
    let p = &report.pass;
    println!(
        "通过线：tile_fetch_p95<5ms={} ping_p95<2ms={} restart<5s={}",
        p.tile_fetch_under_5ms, p.ping_p95_under_2ms, p.restart_under_5s
    );
    println!(
        "render.tiles 到 render.tileReady：中位数 {:.3} ms，P95 {:.3} ms；重启后恢复的文档 {}",
        report.tile_event_median_ms,
        report.tile_event_p95_ms,
        report.restored_docs.unwrap_or(0)
    );
    println!(
        "I1 json占比={:.3} msgpack/json字节={:.3}  | I2 total_memory_mb={}",
        report.json_share_of_roundtrip, report.msgpack_vs_json_bytes, report.shm_total_mb
    );
    if !(p.tile_fetch_under_5ms && p.ping_p95_under_2ms && p.restart_under_5s) {
        std::process::exit(2);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Invocation {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn command_line_is_understood() {
        assert_eq!(parse(&[]), Invocation::Window(Vec::new()));
        assert_eq!(
            parse(&["a.pdf", r"C:\dir\b.pdf"]),
            Invocation::Window(vec![PathBuf::from("a.pdf"), PathBuf::from(r"C:\dir\b.pdf")])
        );
        assert_eq!(parse(&["--bench"]), Invocation::Bench);
        assert_eq!(parse(&["-h"]), Invocation::Help);
        assert_eq!(
            parse(&["--frobnicate"]),
            Invocation::Unknown("--frobnicate".to_owned())
        );
    }
}
