//! 发布版是窗口程序，启动时没有控制台。从终端里启动的时候把自己接到那个终端上，
//! `--bench` 的输出和日志才看得见。

/// 接到父进程的控制台上。双击启动（没有父控制台）或者调试版（本来就有控制台）时什么也不做。
pub fn attach_to_parent() {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        // SAFETY: AttachConsole 只收一个整数参数，没有指针；失败（没有父控制台，或者已经有控制台）
        // 只返回错误，不改变进程的其他状态。
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
}
