//! `SystemApi` trait：内核/CLI 唯一被允许触碰系统的入口。
//!
//! 契约要点（**新增实现时必须遵守**）：
//!
//! 1. **只读优先、写入可回滚**：每个写入方法都返回"写入前的值"或"回滚所需信息"
//!    （例如 [`SystemApi::set_priority`] 返回旧优先级、[`SystemApi::set_power_scheme`]
//!    返回旧方案），审计链与事件溯源回滚不需要再查询一次即可还原。
//! 2. **失败即 [`HalError`]，永不 panic**：实现内部不得 `unwrap`/`expect`/`panic!`；
//!    参数非法返回 [`crate::HalErrorKind::InvalidArgument`]，权限不足返回
//!    [`crate::HalErrorKind::AccessDenied`]，安全红线返回
//!    [`crate::HalErrorKind::PolicyDenied`]。
//! 3. **降级必须显式**：不支持的能力返回 [`crate::HalErrorKind::Unsupported`]，
//!    不允许"静默跳过"——可解释性优先于"看起来成功"。
//! 4. **不做降级重试**：策略层决定是否重试或跳过；trait 层只报告事实。
//! 5. **对象安全**：全部方法 `&self`、无泛型，可直接 `Box<dyn SystemApi>` /
//!    `Arc<dyn SystemApi + Send + Sync>`；因此写入方法必须是线程安全的。

use crate::affinity::{AffinityApplied, AffinityInfo, AffinityPlan};
use crate::error::HalResult;
use crate::hardware::HardwareInfo;
use crate::types::{
    PowerScheme, PowerSchemeChange, PowerSchemeSelector, PriorityClass, ProcessInfo, RunEntry,
    RunHive, WorkingSetLimits,
};

/// HAL 操作标识：用于 Mock 的失败注入、调用序列断言与审计日志字段。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HalOp {
    /// [`SystemApi::get_priority`]
    GetPriority,
    /// [`SystemApi::set_priority`]
    SetPriority,
    /// [`SystemApi::get_affinity`]
    GetAffinity,
    /// [`SystemApi::set_affinity`]
    SetAffinity,
    /// [`SystemApi::get_working_set`]
    GetWorkingSet,
    /// [`SystemApi::set_working_set`]
    SetWorkingSet,
    /// [`SystemApi::query_power_scheme`]
    QueryPowerScheme,
    /// [`SystemApi::set_power_scheme`]
    SetPowerScheme,
    /// [`SystemApi::list_run_entries`]
    ListRunEntries,
    /// [`SystemApi::set_run_entry_enabled`]
    SetRunEntryEnabled,
    /// [`SystemApi::list_processes`]
    ListProcesses,
    /// [`SystemApi::is_elevated`]
    IsElevated,
    /// [`SystemApi::hardware`]
    Hardware,
}

impl HalOp {
    /// 稳定蛇形命名（JSON 字段值）。
    pub const fn as_str(self) -> &'static str {
        match self {
            HalOp::GetPriority => "get_priority",
            HalOp::SetPriority => "set_priority",
            HalOp::GetAffinity => "get_affinity",
            HalOp::SetAffinity => "set_affinity",
            HalOp::GetWorkingSet => "get_working_set",
            HalOp::SetWorkingSet => "set_working_set",
            HalOp::QueryPowerScheme => "query_power_scheme",
            HalOp::SetPowerScheme => "set_power_scheme",
            HalOp::ListRunEntries => "list_run_entries",
            HalOp::SetRunEntryEnabled => "set_run_entry_enabled",
            HalOp::ListProcesses => "list_processes",
            HalOp::IsElevated => "is_elevated",
            HalOp::Hardware => "hardware",
        }
    }

    /// 该操作是否会修改系统状态（用于审计/权限预检）。
    pub const fn is_write(self) -> bool {
        matches!(
            self,
            HalOp::SetPriority
                | HalOp::SetAffinity
                | HalOp::SetWorkingSet
                | HalOp::SetPowerScheme
                | HalOp::SetRunEntryEnabled
        )
    }

    /// 全部操作（枚举顺序稳定，便于 CLI 展示与测试遍历）。
    pub const ALL: [HalOp; 13] = [
        HalOp::GetPriority,
        HalOp::SetPriority,
        HalOp::GetAffinity,
        HalOp::SetAffinity,
        HalOp::GetWorkingSet,
        HalOp::SetWorkingSet,
        HalOp::QueryPowerScheme,
        HalOp::SetPowerScheme,
        HalOp::ListRunEntries,
        HalOp::SetRunEntryEnabled,
        HalOp::ListProcesses,
        HalOp::IsElevated,
        HalOp::Hardware,
    ];
}

/// 系统操作接口：真实后端 [`crate::Win32Api`]，测试后端 [`crate::MockApi`]。
///
/// 方法语义见各自文档；两个后端必须对同一输入给出同分类的错误
/// （例如目标进程不存在 → [`crate::HalErrorKind::NotFound`]），
/// 这样 CLI / 策略层可以在无管理员、无真机的情况下被完整测试。
pub trait SystemApi {
    /// 后端标识：`win32` / `mock`（进诊断报告与审计日志）。
    fn backend_name(&self) -> &'static str;

    /// 读取目标进程当前优先级类。
    fn get_priority(&self, pid: u32) -> HalResult<PriorityClass>;

    /// 设置目标进程优先级，**返回设置前的优先级**（回滚与审计用）。
    ///
    /// `class` 只能是 [`PriorityClass`] 的 5 档；REALTIME 无法表达，故不可能被请求。
    fn set_priority(&self, pid: u32, class: PriorityClass) -> HalResult<PriorityClass>;

    /// 读取目标进程当前亲和性（含所在处理器组与系统掩码）。
    fn get_affinity(&self, pid: u32) -> HalResult<AffinityInfo>;

    /// 应用亲和性计划，返回实际生效的计划、手段与线程数。
    ///
    /// * 主组（group 0）单组计划 → `SetProcessAffinityMask`；
    /// * 非主组单组计划（>64 逻辑核）→ 逐线程 `SetThreadGroupAffinity`；
    /// * 跨多组计划 → [`crate::HalErrorKind::Unsupported`]，调用方应先
    ///   [`AffinityPlan::per_group`] 拆分。
    fn set_affinity(&self, pid: u32, plan: &AffinityPlan) -> HalResult<AffinityApplied>;

    /// 读取目标进程当前工作集上下限（官方 `GetProcessWorkingSetSize`）。
    ///
    /// 返回值是**系统报告的真值**：`min_bytes` 可能为 0（此时
    /// [`WorkingSetLimits::is_restorable`] 为 `false`，表示这个前值无法经
    /// [`SystemApi::set_working_set`] 精确写回）；`max_bytes` 可能是很大的真实值
    /// （例如 `0x7fff_ffff_ffff_ffff`），不要把它当成 [`WorkingSetLimits::NO_UPPER_BOUND`] 哨兵。
    fn get_working_set(&self, pid: u32) -> HalResult<WorkingSetLimits>;

    /// 设置进程工作集上下限（替代"内存池预分配"，可被系统回收）。
    fn set_working_set(&self, pid: u32, limits: WorkingSetLimits) -> HalResult<()>;

    /// 查询当前活动电源方案。
    fn query_power_scheme(&self) -> HalResult<PowerScheme>;

    /// 切换电源方案，**返回切换前/后的方案**（切换前方案即回滚依据）。
    fn set_power_scheme(&self, target: &PowerSchemeSelector) -> HalResult<PowerSchemeChange>;

    /// 列出全部开机启动项（HKCU + HKLM 的 Run 键，含已禁用项）。
    fn list_run_entries(&self) -> HalResult<Vec<RunEntry>>;

    /// 启用/禁用指定启动项（按展示名定位），返回变更后的条目。
    ///
    /// 禁用是"改名迁移"：`Foo` → `[disabled] Foo`，值内容原样保留，可逆。
    fn set_run_entry_enabled(
        &self,
        hive: RunHive,
        name: &str,
        enabled: bool,
    ) -> HalResult<RunEntry>;

    /// 列出运行中的进程（按 PID 升序）。
    fn list_processes(&self) -> HalResult<Vec<ProcessInfo>>;

    /// 当前进程是否已提权（管理员）。
    fn is_elevated(&self) -> HalResult<bool>;

    /// 采集硬件画像。
    fn hardware(&self) -> HalResult<HardwareInfo>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_names_and_write_flags_are_stable() {
        assert_eq!(HalOp::SetPriority.as_str(), "set_priority");
        assert!(HalOp::SetPriority.is_write());
        assert!(!HalOp::GetPriority.is_write());
        assert!(!HalOp::GetWorkingSet.is_write(), "reading is not a write");
        assert!(HalOp::SetWorkingSet.is_write());
        assert_eq!(HalOp::GetWorkingSet.as_str(), "get_working_set");
        assert!(!HalOp::Hardware.is_write());
        assert_eq!(HalOp::ALL.len(), 13);
        let mut names: Vec<&str> = HalOp::ALL.iter().map(|op| op.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 13, "operation names must be unique");
    }

    /// 对象安全：单内核多前端要求 `dyn SystemApi` 可用。
    #[test]
    fn trait_is_object_safe() {
        fn assert_object_safe(_: &dyn SystemApi) {}
        let mock = crate::MockApi::sample_workstation();
        assert_object_safe(&mock);
        let boxed: Box<dyn SystemApi> = Box::new(mock);
        assert_eq!(boxed.backend_name(), "mock");
    }

    /// 前端可能把后端放进 `Arc` 跨线程使用。
    #[test]
    fn mock_backend_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::MockApi>();
    }
}
