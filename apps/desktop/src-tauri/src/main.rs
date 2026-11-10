//! MagPDF 桌面主进程（V06 原型入口）。
//!
//! 当前提供命令行基准：`magpdf-desktop --bench`，拉起引擎、测三条通过线与 I1/I2。
//! Tauri WebView 外壳在后续补齐；本二进制先作为主进程监督者。

mod host;

use std::fs;

use anyhow::{Context, Result};
use host::{Host, HostConfig};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MAGPDF_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--bench") {
        args.remove(0);
        return run_bench();
    }
    println!("用法：magpdf-desktop --bench");
    println!("环境变量：PDFIUM_PREBUILT_DIR、MAGPDF_ENGINE_BIN、MAGPDF_BENCH_PDF");
    Ok(())
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
        "I1 json占比={:.3} msgpack/json字节={:.3}  | I2 total_memory_mb={}",
        report.json_share_of_roundtrip, report.msgpack_vs_json_bytes, report.shm_total_mb
    );
    if !(p.tile_fetch_under_5ms && p.ping_p95_under_2ms && p.restart_under_5s) {
        std::process::exit(2);
    }
    Ok(())
}
