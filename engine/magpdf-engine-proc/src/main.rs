//! 引擎进程可执行程序（方案 3.1、3.3；P0 计划 V06）。
//!
//! 由主进程启动，命令行参数：
//! ```text
//! magpdf-engine --pipe <管道名> --session <会话 ID>
//!              [--recovery <恢复文件路径>] [--slots <槽位数>]
//! ```
//!
//! 结构：
//! - 主线程跑 tokio 运行时，负责管道通信：读帧、JSON 解码、派发、编码、写帧；
//! - PDFium 任务投递给一个专用线程（[`pdfium_thread`]），PDFium 忙时通信线程仍能应答 `session.ping`；
//! - 瓦片由 PDFium 线程渲染后写进命名共享内存（[`slots`] 记账），再发 `render.tileReady` 事件。
#![forbid(unsafe_code)]

mod bench;
mod pdfium_thread;
mod recovery;
mod server;
mod slots;

use anyhow::{Context, Result};

use crate::server::EngineConfig;

fn main() -> Result<()> {
    // 日志写到标准错误，主进程负责收集。默认只看 info 及以上，可用 MAGPDF_LOG 覆盖。
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MAGPDF_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let config = EngineConfig::from_args(std::env::args().skip(1))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?;
    let code = runtime.block_on(server::run(config))?;
    std::process::exit(code);
}
