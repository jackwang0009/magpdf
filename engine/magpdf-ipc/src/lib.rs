//! 主进程与引擎进程之间的 IPC 公共代码（方案 3.1、3.3；《接口协议》第 3 节）。
//!
//! 三块内容，主进程和引擎进程共用：
//! - [`frame`]：命名管道上的帧格式（4 字节小端长度 + UTF-8 JSON），以及 [`codec`] 里的 JSON 与
//!   MessagePack 对照编解码（《接口协议》待定项 I1）；
//! - [`messages`]：JSON-RPC 2.0 的消息、方法参数、事件、错误码（《接口协议》3.1、第 8 节），由 `ts-rs`
//!   生成 TypeScript 类型；
//! - [`pipe`]：管道名与连接；[`shm`]：命名共享内存的瓦片槽位。
//!
//! 库代码不使用 `unsafe`；调用 Windows 接口的 `unsafe` 只集中在 [`shm::win32`] 一个模块里。

pub mod codec;
pub mod frame;
pub mod messages;
pub mod pipe;
pub mod pump;
pub mod shm;

pub use messages::PROTOCOL_VERSION;

/// PDFium 动态库的文件名。引擎可执行文件由主进程启动时用它定位动态库目录，两个 crate 都引用这个常量。
pub const PDFIUM_DLL: &str = "pdfium.dll";
