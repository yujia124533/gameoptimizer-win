//! 官方 Win32 后端：`SystemApi` 的真实实现。
//!
//! 只使用 kernel32 / advapi32 / powrprof / dxgi 的公开 API（见 crate 依赖的 `windows`
//! feature 列表），不做注入、不做内核 Hook、不使用未文档化调用。
//!
//! 子模块划分：
//!
//! * [`process`]：优先级 / 亲和性 / 工作集 / 进程枚举（kernel32 + ToolHelp）
//! * [`power`]：电源方案查询与切换（powrprof）
//! * [`startup`]：开机启动项（HKCU/HKLM Run 键，改名迁移，可回滚）
//! * [`system`]：提权检测与硬件画像（advapi32 令牌 + DXGI + 注册表）
//! * [`handle`]：RAII 句柄封装

mod handle;
mod power;
mod process;
mod startup;
mod system;

use windows::Win32::Foundation::GetLastError;

use crate::affinity::{AffinityApplied, AffinityInfo, AffinityPlan};
use crate::api::SystemApi;
use crate::error::{HalError, HalResult};
use crate::hardware::HardwareInfo;
use crate::types::{
    PowerScheme, PowerSchemeChange, PowerSchemeSelector, PriorityClass, ProcessInfo, RunEntry,
    RunHive, WorkingSetLimits,
};

/// 真实系统后端（仅官方 Win32 API）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Win32Api;

impl Win32Api {
    /// 构造后端（无状态，可自由复制、跨线程共享）。
    pub const fn new() -> Self {
        Self
    }

    /// 枚举系统上已安装的电源方案。
    ///
    /// 不在 [`SystemApi`] trait 里（trait 只暴露 query/set），但 CLI 的
    /// `power list --json` 需要它，且实现与"高性能方案解析"共用同一段代码。
    pub fn list_power_schemes(&self) -> HalResult<Vec<PowerScheme>> {
        power::list_schemes()
    }
}

impl SystemApi for Win32Api {
    fn backend_name(&self) -> &'static str {
        "win32"
    }

    fn get_priority(&self, pid: u32) -> HalResult<PriorityClass> {
        process::get_priority(pid)
    }

    fn set_priority(&self, pid: u32, class: PriorityClass) -> HalResult<PriorityClass> {
        process::set_priority(pid, class)
    }

    fn get_affinity(&self, pid: u32) -> HalResult<AffinityInfo> {
        process::get_affinity(pid)
    }

    fn set_affinity(&self, pid: u32, plan: &AffinityPlan) -> HalResult<AffinityApplied> {
        process::set_affinity(pid, plan)
    }

    fn get_working_set(&self, pid: u32) -> HalResult<WorkingSetLimits> {
        process::get_working_set(pid)
    }

    fn set_working_set(&self, pid: u32, limits: WorkingSetLimits) -> HalResult<()> {
        process::set_working_set(pid, limits)
    }

    fn query_power_scheme(&self) -> HalResult<PowerScheme> {
        power::query_active()
    }

    fn set_power_scheme(&self, target: &PowerSchemeSelector) -> HalResult<PowerSchemeChange> {
        power::set_active(target)
    }

    fn list_run_entries(&self) -> HalResult<Vec<RunEntry>> {
        startup::list_run_entries()
    }

    fn set_run_entry_enabled(
        &self,
        hive: RunHive,
        name: &str,
        enabled: bool,
    ) -> HalResult<RunEntry> {
        startup::set_run_entry_enabled(hive, name, enabled)
    }

    fn list_processes(&self) -> HalResult<Vec<ProcessInfo>> {
        process::list_processes()
    }

    fn is_elevated(&self) -> HalResult<bool> {
        system::is_elevated()
    }

    fn hardware(&self) -> HalResult<HardwareInfo> {
        system::hardware()
    }
}

/// UTF-8 → 以 NUL 结尾的宽字符串。
pub(crate) fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

impl crate::types::Guid {
    /// HAL GUID → Win32 GUID（`windows` 类型只在本模块内部出现）。
    pub(crate) fn to_win32(self) -> windows::core::GUID {
        let (data1, data2, data3, data4) = self.parts();
        windows::core::GUID {
            data1,
            data2,
            data3,
            data4,
        }
    }

    /// Win32 GUID → HAL GUID。
    pub(crate) fn from_win32(guid: windows::core::GUID) -> Self {
        crate::types::Guid::new(guid.data1, guid.data2, guid.data3, guid.data4)
    }
}

/// 以 NUL 结尾的宽字符串 → `String`（去掉尾部 NUL 之后的全部内容）。
pub(crate) fn from_wide(units: &[u16]) -> String {
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// UTF-16LE 字节缓冲区 → `String`（注册表 `REG_SZ` / powrprof 名称输出）。
pub(crate) fn from_wide_bytes(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    from_wide(&units)
}

/// 读取线程本地 `GetLastError()`（用于返回 `BOOL` 而 windows-rs 未包装成 `Result` 的 API）。
pub(crate) fn last_error(operation: &'static str) -> HalError {
    let code = unsafe { GetLastError().0 };
    HalError::win32_from_code(operation, code)
}

/// 从 `windows::core::Error` 里还原**原始 Win32 错误码**。
///
/// windows-rs 把 `BOOL` 失败包装成 `HRESULT_FROM_WIN32(code)`（0x8007xxxx），
/// 直接把 HRESULT 写进审计日志会让人看不懂，所以这里取出低 16 位。
pub(crate) fn win32_code_of(error: &windows::core::Error) -> u32 {
    let raw = error.code().0 as u32;
    if raw & 0xffff_0000 == 0x8007_0000 {
        raw & 0xffff
    } else {
        raw
    }
}

/// `BOOL` 失败 → [`HalError`]（保留原始 Win32 错误码）。
pub(crate) fn error_from(
    operation: &'static str,
    error: &windows::core::Error,
    context: impl core::fmt::Display,
) -> HalError {
    HalError::win32_from_code(operation, win32_code_of(error)).with_context(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
        IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, REALTIME_PRIORITY_CLASS,
    };

    #[test]
    fn hal_priority_constants_match_win32() {
        // HAL 是唯一允许出现原始数值的地方：这些断言把它们钉死在官方常量上。
        assert_eq!(PriorityClass::Idle.raw(), IDLE_PRIORITY_CLASS.0);
        assert_eq!(
            PriorityClass::BelowNormal.raw(),
            BELOW_NORMAL_PRIORITY_CLASS.0
        );
        assert_eq!(PriorityClass::Normal.raw(), NORMAL_PRIORITY_CLASS.0);
        assert_eq!(
            PriorityClass::AboveNormal.raw(),
            ABOVE_NORMAL_PRIORITY_CLASS.0
        );
        assert_eq!(PriorityClass::High.raw(), HIGH_PRIORITY_CLASS.0);
        assert_ne!(PriorityClass::High.raw(), REALTIME_PRIORITY_CLASS.0);
    }

    #[test]
    fn realtime_priority_is_rejected_by_the_only_raw_entry_point() {
        let err = PriorityClass::from_raw(REALTIME_PRIORITY_CLASS.0).expect_err("policy denial");
        assert!(err.is_policy_denial());
    }

    #[test]
    fn wide_helpers_round_trip() {
        let wide = to_wide("cs2.exe");
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(from_wide(&wide), "cs2.exe");
        let bytes: Vec<u8> = "高性能"
            .encode_utf16()
            .chain(core::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(from_wide_bytes(&bytes), "高性能");
    }

    #[test]
    fn win32_code_is_extracted_from_hresult() {
        // HRESULT_FROM_WIN32(5) == 0x80070005，其中 5 = ERROR_ACCESS_DENIED
        let error =
            windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0005u32 as i32));
        assert_eq!(win32_code_of(&error), 5);
        assert_eq!(
            error_from("RegOpenKeyExW", &error, "hive HKLM").kind(),
            crate::HalErrorKind::AccessDenied
        );
        // 非 HRESULT_FROM_WIN32 系的 HRESULT（例如 DXGI_ERROR_NOT_FOUND）原样保留。
        let dxgi =
            windows::core::Error::from_hresult(windows::core::HRESULT(0x887a_0002u32 as i32));
        assert_eq!(win32_code_of(&dxgi), 0x887a_0002);
    }

    #[test]
    fn win32_backend_is_send_and_sync_and_object_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Win32Api>();
        let backend: Box<dyn SystemApi> = Box::new(Win32Api::new());
        assert_eq!(backend.backend_name(), "win32");
    }
}
