//! 共享内存的安全描述符（《接口协议》3.2、3.3；P0 报告 V06 的"Windows 安全现状与差距"）。
//!
//! 规范要求只有当前用户能访问。P0 原型给出的是：用户数据目录下本机上所有认证用户可打开、当前用户
//! 完全控制的描述符——与 `Local\` 命名空间组合后，只有本登录会话内的进程能看到这块内存。显式 ACL
//! （只允许当前用户的 SID）留到 P1 与引擎进程沙箱一起做，见报告。
//!
//! 这里为了构造 SECURITY_ATTRIBUTES 需要几处 `unsafe`；与本 crate 的约定一致，unsafe 只出现在
//! Windows 模块里（本文件属于 [`crate::shm`] 的 Windows 实现部分）。

use std::mem::size_of;
use std::sync::OnceLock;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::core::{HSTRING, PCWSTR};

/// 描述符：`D:` DACL，`(A;;GA;;;AU)` 允许认证用户全部访问（P0 原型的取舍），`(A;;GA;;;SY)` 允许本地系统。
///
/// 试用过的更窄的 DACL（只给认证用户 `GRGW`，或只给 `0x1F0003`）在开发机上会让另一进程打开映射或
/// `MapViewOfFile` 失败；P1 改用当前用户 SID 时一并复核。不显式给描述符（用默认 DACL）时，另一进程
/// 只读打开超过 4 MB 的映射也会失败，所以必须显式给描述符。
const SDDL: &str = "D:(A;;GA;;;AU)(A;;GA;;;SY)";

struct Holder {
    /// 自描述指针，指向由 `LocalAlloc` 分配的描述符，进程结束时不必释放（与进程同寿命）。
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
    _guard: &'static (),
}

// SAFETY: Holder 只含自描述指针与 SECURITY_ATTRIBUTES（原始指针字段）；描述符一经构造不再修改，
// 只读地交给 CreateFileMappingW，跨线程共享只产生只读访问。
unsafe impl Send for Holder {}
// SAFETY: 同 Send：构造之后不再修改，共享只产生只读访问。
unsafe impl Sync for Holder {}

fn holder() -> &'static Holder {
    static HOLDER: OnceLock<Holder> = OnceLock::new();
    HOLDER.get_or_init(|| {
        let sddl = HSTRING::from(SDDL);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: sddl 在调用期间有效；descriptor 是有效的输出位置。成功后描述符由 LocalAlloc 分配，
        // 这里故意不释放，与进程同寿命。
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        };
        if ok.is_err() {
            // 描述符构造失败时退回到默认安全属性（此时 `descriptor` 为空指针，调用方按 None 处理）。
            return Holder {
                descriptor: PSECURITY_DESCRIPTOR::default(),
                attributes: SECURITY_ATTRIBUTES::default(),
                _guard: &(),
            };
        }
        Holder {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            },
            _guard: &(),
        }
    })
}

/// 创建共享内存时使用的安全属性。
pub fn security_attributes() -> SECURITY_ATTRIBUTES {
    let h = holder();
    if h.descriptor.is_invalid() {
        return SECURITY_ATTRIBUTES::default();
    }
    h.attributes
}

/// 释放构造描述符时分配的内存（只在测试里调用，正常进程退出时由系统回收）。
pub fn free_descriptor() {
    let h = holder();
    let ptr = h.descriptor;
    if !ptr.is_invalid() {
        // SAFETY: descriptor 由 ConvertStringSecurityDescriptorToSecurityDescriptorW 分配；调用方保证只调一次。
        unsafe {
            let _ = LocalFree(Some(HLOCAL(ptr.0)));
        }
    }
}
