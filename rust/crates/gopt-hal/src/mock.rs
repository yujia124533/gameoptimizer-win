//! Mock 后端：记录调用序列、可注入失败、可配置拓扑与权限状态。
//!
//! 用途（团队目标之一："无需管理员即可单测"）：
//!
//! * 让 gopt-core / gopt-cli 的测试完全不碰真实系统（CI 上不需要管理员、不需要游戏在跑）；
//! * 用 [`MockApi::fail_next`] 复现"设置优先级成功但设置亲和性失败"这类真实世界里
//!   必然出现、但难以在真机稳定复现的降级路径；
//! * 用 [`MockApi::calls`] 断言"策略引擎到底调了哪些 API、参数是什么"。
//!
//! 行为对齐：Mock 后端**刻意与 [`crate::Win32Api`] 保持同样的错误分类**，包括跨处理器组
//! 亲和性的 `Unsupported`。差异只有一处并已文档化：Mock 的拓扑由测试指定，不代表真机。

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use crate::affinity::{
    full_group_mask, processor_groups, AffinityApplied, AffinityInfo, AffinityMethod, AffinityPlan,
    MAX_LOGICAL_PER_GROUP,
};
use crate::api::{HalOp, SystemApi};
use crate::error::{HalError, HalResult};
use crate::hardware::{CoreLayout, GpuInfo, GpuVendor, HardwareInfo};
use crate::types::{
    Guid, PowerScheme, PowerSchemeChange, PowerSchemeSelector, PriorityClass, ProcessInfo,
    RunEntry, RunHive, WorkingSetLimits,
};

/// 一次被记录的 HAL 调用（不含返回值；返回值由被测代码直接断言）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Call {
    /// 操作标识。
    pub op: HalOp,
    /// 调用参数。
    pub args: CallArgs,
}

/// 调用参数（与 [`crate::SystemApi`] 方法一一对应，可序列化进测试报告）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum CallArgs {
    /// `get_priority(pid)`
    GetPriority {
        /// 目标进程。
        pid: u32,
    },
    /// `set_priority(pid, class)`
    SetPriority {
        /// 目标进程。
        pid: u32,
        /// 目标优先级。
        class: PriorityClass,
    },
    /// `get_affinity(pid)`
    GetAffinity {
        /// 目标进程。
        pid: u32,
    },
    /// `set_affinity(pid, plan)`
    SetAffinity {
        /// 目标进程。
        pid: u32,
        /// 亲和性计划。
        plan: AffinityPlan,
    },
    /// `get_working_set(pid)`
    GetWorkingSet {
        /// 目标进程。
        pid: u32,
    },
    /// `set_working_set(pid, limits)`
    SetWorkingSet {
        /// 目标进程。
        pid: u32,
        /// 工作集上下限。
        limits: WorkingSetLimits,
    },
    /// `query_power_scheme()`
    QueryPowerScheme,
    /// `set_power_scheme(target)`
    SetPowerScheme {
        /// 目标方案选择器。
        target: PowerSchemeSelector,
    },
    /// `list_run_entries()`
    ListRunEntries,
    /// `set_run_entry_enabled(hive, name, enabled)`
    SetRunEntryEnabled {
        /// 启动项根。
        hive: RunHive,
        /// 启动项展示名。
        name: String,
        /// 目标状态。
        enabled: bool,
    },
    /// `list_processes()`
    ListProcesses,
    /// `is_elevated()`
    IsElevated,
    /// `hardware()`
    Hardware,
}

#[derive(Debug)]
struct MockState {
    calls: Vec<Call>,
    processes: BTreeMap<u32, ProcessInfo>,
    priorities: BTreeMap<u32, PriorityClass>,
    affinities: BTreeMap<u32, AffinityPlan>,
    working_sets: BTreeMap<u32, WorkingSetLimits>,
    run_entries: Vec<RunEntry>,
    power_schemes: Vec<PowerScheme>,
    active_power: Guid,
    elevated: bool,
    hardware: HardwareInfo,
    fail_next: BTreeMap<HalOp, VecDeque<HalError>>,
    fail_always: BTreeMap<HalOp, HalError>,
}

impl MockState {
    fn record(&mut self, op: HalOp, args: CallArgs) {
        self.calls.push(Call { op, args });
    }

    /// 先记录调用，再决定是否注入失败：调用序列永远完整。
    fn check_failure(&mut self, op: HalOp) -> HalResult<()> {
        if let Some(queue) = self.fail_next.get_mut(&op) {
            if let Some(err) = queue.pop_front() {
                return Err(err);
            }
        }
        if let Some(err) = self.fail_always.get(&op) {
            return Err(err.clone());
        }
        Ok(())
    }

    fn require_process(&self, pid: u32) -> HalResult<&ProcessInfo> {
        self.processes.get(&pid).ok_or_else(|| {
            HalError::not_found(
                "MockApi::open_process",
                format!("target process {pid} does not exist"),
            )
        })
    }

    fn active_scheme(&self) -> HalResult<PowerScheme> {
        self.power_schemes
            .iter()
            .find(|scheme| scheme.guid == self.active_power)
            .cloned()
            .ok_or_else(|| {
                HalError::internal(
                    "MockApi::active_scheme",
                    format!(
                        "the configured active power scheme {} is not in the installed list",
                        self.active_power
                    ),
                )
            })
    }
}

/// Mock 后端。
///
/// 内部用 `Mutex` 而不是 `RefCell`：既满足 `Box<dyn SystemApi + Send + Sync>`（前端可能跨线程
/// 持有后端），又能在锁中毒时**恢复而不是 panic**（`unwrap_or_else(|e| e.into_inner())`），
/// 与"禁止以 panic 作为错误路径"的红线一致。
#[derive(Debug)]
pub struct MockApi {
    state: Mutex<MockState>,
}

impl Default for MockApi {
    fn default() -> Self {
        Self::new()
    }
}

impl MockApi {
    /// 未显式安排工作集的进程，其"写入前工作集"（字节）。
    ///
    /// 取 200 KiB / 无上限：与真机的默认最小工作集同量级（本机实测 204800 字节），
    /// 且 `min > 0` ⇒ 可被 [`WorkingSetLimits::is_restorable`] 判定为可还原。
    /// 这是 Mock 的**声明式默认值**，不代表任何具体机器。
    pub const DEFAULT_WORKING_SET: (u64, u64) = (200 * 1024, 0);

    /// 最小可用后端：8 物理核 / 8 逻辑核、无进程、无启动项、仅"平衡"电源方案、未提权。
    pub fn new() -> Self {
        Self::with_topology(8, 8)
    }

    /// 指定逻辑核数（物理核 = 逻辑核，即无 SMT）的后端；`logical > 64` 时自动多处理器组。
    pub fn with_cores(logical_cores: u32) -> Self {
        Self::with_topology(logical_cores, logical_cores)
    }

    /// 指定物理/逻辑核数的后端（逻辑核按 SMT 均匀分摊到物理核）。
    pub fn with_topology(physical_cores: u32, logical_cores: u32) -> Self {
        let physical = physical_cores.max(1);
        let logical = logical_cores.max(1);
        let (core_layout, processor_groups) = build_topology(physical, logical);
        let hardware = HardwareInfo {
            cpu_model: format!("Mock CPU ({physical}C/{logical}T)"),
            physical_cores: physical,
            logical_cores: logical,
            supports_hyper_threading: logical > physical,
            cpu_base_freq_mhz: 3600,
            core_layout,
            processor_groups,
            gpu: None,
            system_ram_mb: 16384,
            available_ram_mb: 8192,
            large_pages_available: false,
            warnings: Vec::new(),
        };
        let state = MockState {
            calls: Vec::new(),
            processes: BTreeMap::new(),
            priorities: BTreeMap::new(),
            affinities: BTreeMap::new(),
            working_sets: BTreeMap::new(),
            run_entries: Vec::new(),
            power_schemes: vec![PowerScheme::new(Guid::BALANCED, "Balanced")],
            active_power: Guid::BALANCED,
            elevated: false,
            hardware,
            fail_next: BTreeMap::new(),
            fail_always: BTreeMap::new(),
        };
        Self {
            state: Mutex::new(state),
        }
    }

    /// 一台"典型游戏 PC"的预置后端：16 逻辑核 / 8 物理核（SMT）、3 个进程、
    /// 3 个启动项（含 1 个已禁用）、3 个电源方案（活动 = 平衡）、未提权。
    ///
    /// CLI / 策略层的集成测试可以直接用它，无需拼接状态。
    pub fn sample_workstation() -> Self {
        let api = Self::with_topology(8, 16);
        api.push_process(4, "explorer.exe", 42);
        api.push_process(1234, "cs2.exe", 24);
        api.push_process(5678, "steam.exe", 8);
        api.push_run_entry(
            RunHive::CurrentUser,
            "Steam",
            r#""C:\Steam\steam.exe" -silent"#,
            true,
        );
        api.push_run_entry(
            RunHive::LocalMachine,
            "SecurityHealth",
            r"C:\Windows\System32\SecurityHealthSystray.exe",
            true,
        );
        api.push_run_entry(
            RunHive::CurrentUser,
            "Discord",
            r#""C:\Discord\Update.exe" --processStart Discord.exe"#,
            false,
        );
        api.push_power_scheme(PowerScheme::new(Guid::HIGH_PERFORMANCE, "High performance"));
        api.push_power_scheme(PowerScheme::new(Guid::POWER_SAVER, "Power saver"));
        {
            let mut state = api.lock();
            state.hardware.gpu = Some(GpuInfo {
                vendor: GpuVendor::Nvidia,
                vendor_id: 0x10de,
                device_id: 0x2786,
                model: "Mock GeForce RTX 4070".to_string(),
                vram_mb: 12282,
                driver_version: Some("31.0.15.3742".to_string()),
                is_hardware: true,
                is_software_adapter: false,
            });
        }
        api
    }

    // ---------------- 测试配置 ----------------

    /// 覆盖硬件画像。
    pub fn set_hardware(&self, hardware: HardwareInfo) {
        self.lock().hardware = hardware;
    }

    /// 追加一个运行中的进程（默认优先级 = `normal`，默认可绑定全部逻辑核）。
    pub fn push_process(&self, pid: u32, name: &str, thread_count: u32) {
        let mut state = self.lock();
        let total = state.hardware.logical_cores;
        state
            .processes
            .insert(pid, ProcessInfo::new(pid, 0, name, thread_count));
        state.priorities.entry(pid).or_insert(PriorityClass::Normal);
        if let Ok(plan) = AffinityPlan::full(total) {
            state.affinities.insert(pid, plan);
        }
    }

    /// 直接安排"写入前的工作集"，**不记录调用**（测试夹具用）。
    ///
    /// 真机上"写入前状态"是系统事实；Mock 里必须显式安排，因此提供一个不污染
    /// [`MockApi::calls`] 的入口（`set_working_set` 会记一次调用）。
    pub fn seed_working_set(&self, pid: u32, limits: WorkingSetLimits) {
        self.lock().working_sets.insert(pid, limits.normalized());
    }

    /// 追加一个启动项（`enabled == false` 时自动带禁用前缀）。
    pub fn push_run_entry(&self, hive: RunHive, name: &str, command: &str, enabled: bool) {
        let value_name = if enabled {
            name.to_string()
        } else {
            RunEntry::disabled_value_name(name)
        };
        self.lock()
            .run_entries
            .push(RunEntry::from_registry(hive, value_name, command, false));
    }

    /// 追加一个已安装电源方案。
    pub fn push_power_scheme(&self, scheme: PowerScheme) {
        self.lock().power_schemes.push(scheme);
    }

    /// 设置当前活动电源方案（必须是已安装的方案）。
    pub fn set_active_power_scheme(&self, guid: Guid) -> HalResult<()> {
        let mut state = self.lock();
        if !state.power_schemes.iter().any(|scheme| scheme.guid == guid) {
            return Err(HalError::not_found(
                "MockApi::set_active_power_scheme",
                format!("power scheme {guid} is not installed"),
            ));
        }
        state.active_power = guid;
        Ok(())
    }

    /// 设置提权状态（模拟"以管理员运行 / 未提权"）。
    pub fn set_elevated(&self, elevated: bool) {
        self.lock().elevated = elevated;
    }

    /// 让某个操作的下一次调用失败（失败发生在记录调用之后）。
    pub fn fail_next(&self, op: HalOp, err: HalError) {
        self.lock().fail_next.entry(op).or_default().push_back(err);
    }

    /// 让某个操作从此总是失败。
    pub fn fail_always(&self, op: HalOp, err: HalError) {
        self.lock().fail_always.insert(op, err);
    }

    /// 清除全部失败注入。
    pub fn clear_failures(&self) {
        let mut state = self.lock();
        state.fail_next.clear();
        state.fail_always.clear();
    }

    // ---------------- 观测 ----------------

    /// 已记录的调用序列（按发生顺序）。
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// 某个操作被调用的次数。
    pub fn call_count(&self, op: HalOp) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|call| call.op == op)
            .count()
    }

    /// 最近一次调用。
    pub fn last_call(&self) -> Option<Call> {
        self.lock().calls.last().cloned()
    }

    /// 清空调用序列。
    pub fn clear_calls(&self) {
        self.lock().calls.clear();
    }

    /// 当前记录的进程优先级。
    pub fn priority_of(&self, pid: u32) -> Option<PriorityClass> {
        self.lock().priorities.get(&pid).copied()
    }

    /// 当前记录的进程亲和性计划。
    pub fn affinity_of(&self, pid: u32) -> Option<AffinityPlan> {
        self.lock().affinities.get(&pid).cloned()
    }

    /// 当前记录的工作集上下限。
    pub fn working_set_of(&self, pid: u32) -> Option<WorkingSetLimits> {
        self.lock().working_sets.get(&pid).copied()
    }

    /// 按根 + 展示名取启动项。
    pub fn run_entry(&self, hive: RunHive, name: &str) -> Option<RunEntry> {
        self.lock()
            .run_entries
            .iter()
            .find(|entry| entry.hive == hive && entry.name == name)
            .cloned()
    }

    /// 当前活动电源方案。
    pub fn active_power_scheme(&self) -> HalResult<PowerScheme> {
        self.lock().active_scheme()
    }

    /// 已安装电源方案列表（Win32 后端有对应方法；此处便于 CLI 测试）。
    pub fn list_power_schemes(&self) -> Vec<PowerScheme> {
        self.lock().power_schemes.clone()
    }

    fn lock(&self) -> MutexGuard<'_, MockState> {
        // 锁中毒时恢复内部状态而不是 panic：HAL 的任何路径都不允许以 panic 收场。
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SystemApi for MockApi {
    fn backend_name(&self) -> &'static str {
        "mock"
    }

    fn get_priority(&self, pid: u32) -> HalResult<PriorityClass> {
        let mut state = self.lock();
        state.record(HalOp::GetPriority, CallArgs::GetPriority { pid });
        state.check_failure(HalOp::GetPriority)?;
        state.require_process(pid)?;
        Ok(state
            .priorities
            .get(&pid)
            .copied()
            .unwrap_or(PriorityClass::Normal))
    }

    fn set_priority(&self, pid: u32, class: PriorityClass) -> HalResult<PriorityClass> {
        let mut state = self.lock();
        state.record(HalOp::SetPriority, CallArgs::SetPriority { pid, class });
        state.check_failure(HalOp::SetPriority)?;
        state.require_process(pid)?;
        let previous = state
            .priorities
            .get(&pid)
            .copied()
            .unwrap_or(PriorityClass::Normal);
        state.priorities.insert(pid, class);
        Ok(previous)
    }

    fn get_affinity(&self, pid: u32) -> HalResult<AffinityInfo> {
        let mut state = self.lock();
        state.record(HalOp::GetAffinity, CallArgs::GetAffinity { pid });
        state.check_failure(HalOp::GetAffinity)?;
        state.require_process(pid)?;
        let total_logical = state.hardware.logical_cores;
        let plan = match state.affinities.get(&pid) {
            Some(plan) => plan.clone(),
            None => AffinityPlan::full(total_logical)?,
        };
        let group = plan.primary_group().unwrap_or(0);
        let process_mask = plan.requests().first().map_or(0, |request| request.mask());
        let system_mask = state
            .hardware
            .processor_groups
            .iter()
            .find(|candidate| candidate.index == group)
            .map_or(0, |candidate| candidate.full_mask());
        Ok(AffinityInfo {
            pid,
            group,
            process_mask,
            system_mask,
            total_logical,
        })
    }

    fn set_affinity(&self, pid: u32, plan: &AffinityPlan) -> HalResult<AffinityApplied> {
        let mut state = self.lock();
        state.record(
            HalOp::SetAffinity,
            CallArgs::SetAffinity {
                pid,
                plan: plan.clone(),
            },
        );
        state.check_failure(HalOp::SetAffinity)?;

        // 与 Win32Api 保持同样的判定顺序：先校验"计划本身能否应用"，再做与目标进程相关的 I/O。
        // 否则"跨组计划 + 不存在的 PID"在两个后端上会给出不同的错误分类。
        for request in plan.requests() {
            let matching = state
                .hardware
                .processor_groups
                .iter()
                .find(|candidate| candidate.index == request.group());
            match matching {
                Some(group) if request.fits(*group) => {}
                Some(group) => {
                    return Err(HalError::invalid_argument(
                        "MockApi::set_affinity",
                        format!(
                            "mask {:#018x} does not fit processor group {} ({} logical processors)",
                            request.mask(),
                            group.index,
                            group.logical_count
                        ),
                    ))
                }
                None => {
                    return Err(HalError::invalid_argument(
                        "MockApi::set_affinity",
                        format!(
                            "processor group {} does not exist on this machine ({} group(s))",
                            request.group(),
                            state.hardware.processor_groups.len()
                        ),
                    ))
                }
            }
        }

        if !plan.is_single_group() {
            return Err(HalError::unsupported(
                "MockApi::set_affinity",
                "cross-group affinity requires applying one group at a time: split the plan with \
                 AffinityPlan::per_group()",
            ));
        }

        let group = plan.primary_group().unwrap_or(0);
        let thread_count = state.require_process(pid)?.thread_count;
        let method = if group == 0 {
            AffinityMethod::ProcessAffinityMask
        } else {
            AffinityMethod::ThreadGroupAffinity
        };
        let threads_updated = if group == 0 { 0 } else { thread_count };
        state.affinities.insert(pid, plan.clone());
        Ok(AffinityApplied {
            plan: plan.clone(),
            method,
            threads_updated,
        })
    }

    fn get_working_set(&self, pid: u32) -> HalResult<WorkingSetLimits> {
        let mut state = self.lock();
        state.record(HalOp::GetWorkingSet, CallArgs::GetWorkingSet { pid });
        state.check_failure(HalOp::GetWorkingSet)?;
        state.require_process(pid)?;
        if let Some(limits) = state.working_sets.get(&pid).copied() {
            return Ok(limits);
        }
        let (min, max) = Self::DEFAULT_WORKING_SET;
        WorkingSetLimits::observed(min, max)
    }

    fn set_working_set(&self, pid: u32, limits: WorkingSetLimits) -> HalResult<()> {
        let mut state = self.lock();
        state.record(
            HalOp::SetWorkingSet,
            CallArgs::SetWorkingSet { pid, limits },
        );
        state.check_failure(HalOp::SetWorkingSet)?;
        state.require_process(pid)?;
        state.working_sets.insert(pid, limits.normalized());
        Ok(())
    }

    fn query_power_scheme(&self) -> HalResult<PowerScheme> {
        let mut state = self.lock();
        state.record(HalOp::QueryPowerScheme, CallArgs::QueryPowerScheme);
        state.check_failure(HalOp::QueryPowerScheme)?;
        state.active_scheme()
    }

    fn set_power_scheme(&self, target: &PowerSchemeSelector) -> HalResult<PowerSchemeChange> {
        let mut state = self.lock();
        state.record(
            HalOp::SetPowerScheme,
            CallArgs::SetPowerScheme {
                target: target.clone(),
            },
        );
        state.check_failure(HalOp::SetPowerScheme)?;

        let desired = match target {
            PowerSchemeSelector::Explicit(guid) => state
                .power_schemes
                .iter()
                .find(|scheme| scheme.guid == *guid)
                .cloned()
                .ok_or_else(|| {
                    HalError::not_found(
                        "MockApi::set_power_scheme",
                        format!("power scheme {guid} is not installed on this system"),
                    )
                })?,
            PowerSchemeSelector::HighPerformance => state
                .power_schemes
                .iter()
                .find(|scheme| scheme.is_high_performance)
                .cloned()
                .ok_or_else(|| {
                    HalError::not_found(
                        "MockApi::set_power_scheme",
                        "the 'High performance' power scheme is not available on this system",
                    )
                })?,
        };

        let previous = state.active_scheme()?;
        state.active_power = desired.guid;
        Ok(PowerSchemeChange {
            previous,
            current: desired,
        })
    }

    fn list_run_entries(&self) -> HalResult<Vec<RunEntry>> {
        let mut state = self.lock();
        state.record(HalOp::ListRunEntries, CallArgs::ListRunEntries);
        state.check_failure(HalOp::ListRunEntries)?;
        let mut entries = state.run_entries.clone();
        entries.sort_by(|a, b| (a.hive, &a.name).cmp(&(b.hive, &b.name)));
        Ok(entries)
    }

    fn set_run_entry_enabled(
        &self,
        hive: RunHive,
        name: &str,
        enabled: bool,
    ) -> HalResult<RunEntry> {
        let mut state = self.lock();
        state.record(
            HalOp::SetRunEntryEnabled,
            CallArgs::SetRunEntryEnabled {
                hive,
                name: RunEntry::display_name(name).to_string(),
                enabled,
            },
        );
        state.check_failure(HalOp::SetRunEntryEnabled)?;

        let display = RunEntry::display_name(name).to_string();
        let position = state
            .run_entries
            .iter()
            .position(|entry| entry.hive == hive && entry.name == display)
            .ok_or_else(|| {
                HalError::not_found(
                    "MockApi::set_run_entry_enabled",
                    format!("startup entry {hive}:{display} does not exist"),
                )
            })?;

        if state.run_entries[position].enabled == enabled {
            return Ok(state.run_entries[position].clone()); // 幂等：目标状态已满足
        }
        let updated = RunEntry::from_registry(
            hive,
            if enabled {
                display.clone()
            } else {
                RunEntry::disabled_value_name(&display)
            },
            state.run_entries[position].command.clone(),
            state.run_entries[position].expandable,
        );
        state.run_entries[position] = updated.clone();
        Ok(updated)
    }

    fn list_processes(&self) -> HalResult<Vec<ProcessInfo>> {
        let mut state = self.lock();
        state.record(HalOp::ListProcesses, CallArgs::ListProcesses);
        state.check_failure(HalOp::ListProcesses)?;
        Ok(state.processes.values().cloned().collect())
    }

    fn is_elevated(&self) -> HalResult<bool> {
        let mut state = self.lock();
        state.record(HalOp::IsElevated, CallArgs::IsElevated);
        state.check_failure(HalOp::IsElevated)?;
        Ok(state.elevated)
    }

    fn hardware(&self) -> HalResult<HardwareInfo> {
        let mut state = self.lock();
        state.record(HalOp::Hardware, CallArgs::Hardware);
        state.check_failure(HalOp::Hardware)?;
        Ok(state.hardware.clone())
    }
}

/// 按"每核 `smt` 个逻辑处理器"分摊，必要时跨处理器组拆分成多条布局。
fn build_topology(
    physical_cores: u32,
    logical_cores: u32,
) -> (Vec<CoreLayout>, Vec<crate::affinity::ProcessorGroup>) {
    let groups = processor_groups(logical_cores);
    let smt = (logical_cores / physical_cores.max(1)).max(1);
    let mut layout = Vec::new();
    let mut logical_index = 0u32;

    for core in 0..physical_cores {
        if logical_index >= logical_cores {
            break;
        }
        let mut remaining = smt;
        while remaining > 0 && logical_index < logical_cores {
            let group = logical_index / MAX_LOGICAL_PER_GROUP;
            let offset = logical_index % MAX_LOGICAL_PER_GROUP;
            let take = remaining
                .min(MAX_LOGICAL_PER_GROUP - offset)
                .min(logical_cores - logical_index);
            let mask = if take >= MAX_LOGICAL_PER_GROUP {
                u64::MAX
            } else {
                full_group_mask(take) << offset
            };
            layout.push(CoreLayout::new(core, group, mask));
            logical_index += take;
            remaining -= take;
        }
    }

    (layout, groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::HalErrorKind;

    #[test]
    fn topology_spreads_logical_cores_across_groups() {
        let (layout, groups) = build_topology(8, 16);
        assert_eq!(groups.len(), 1);
        assert_eq!(layout.len(), 8);
        assert!(layout.iter().all(|core| core.logical_count() == 2));
        assert_eq!(
            layout.iter().map(CoreLayout::logical_count).sum::<u32>(),
            16
        );

        let (layout, groups) = build_topology(48, 96);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].logical_count, 64);
        assert_eq!(groups[1].logical_count, 32);
        assert_eq!(layout.len(), 48);
        assert_eq!(
            layout.iter().map(CoreLayout::logical_count).sum::<u32>(),
            96
        );
        // 前 32 个物理核各占 2 个逻辑核 → 恰好填满组 0；其余进入组 1。
        assert!(layout.iter().take(32).all(|core| core.group == 0));
        assert!(layout.iter().skip(32).all(|core| core.group == 1));
    }

    #[test]
    fn unknown_process_is_not_found_and_still_recorded() {
        let api = MockApi::new();
        let err = api.get_priority(4242).expect_err("no such process");
        assert_eq!(err.kind(), HalErrorKind::NotFound);
        assert_eq!(api.call_count(HalOp::GetPriority), 1);
    }

    #[test]
    fn injected_failure_is_returned_and_recorded() {
        let api = MockApi::sample_workstation();
        api.fail_next(
            HalOp::SetAffinity,
            HalError::win32_from_code("SetProcessAffinityMask", 5),
        );
        let plan = AffinityPlan::full(16).expect("full plan");
        let err = api.set_affinity(1234, &plan).expect_err("injected failure");
        assert_eq!(err.kind(), HalErrorKind::AccessDenied);
        assert_eq!(err.win32_code(), Some(5));
        assert_eq!(api.call_count(HalOp::SetAffinity), 1);
        assert_eq!(api.affinity_of(1234), Some(plan));
    }

    #[test]
    fn call_sequence_records_arguments() {
        let api = MockApi::sample_workstation();
        api.set_priority(1234, PriorityClass::High).expect("set");
        let calls = api.calls();
        let last = calls.last().expect("one call");
        assert_eq!(last.op, HalOp::SetPriority);
        assert_eq!(
            last.args,
            CallArgs::SetPriority {
                pid: 1234,
                class: PriorityClass::High
            }
        );
        assert_eq!(api.priority_of(1234), Some(PriorityClass::High));
    }

    #[test]
    fn set_priority_returns_previous_value_for_rollback() {
        let api = MockApi::sample_workstation();
        let previous = api.set_priority(1234, PriorityClass::High).expect("set");
        assert_eq!(previous, PriorityClass::Normal);
        let previous = api
            .set_priority(1234, PriorityClass::Normal)
            .expect("restore");
        assert_eq!(previous, PriorityClass::High);
    }

    #[test]
    fn working_set_is_stored_normalized() {
        let api = MockApi::sample_workstation();
        api.set_working_set(1234, WorkingSetLimits::new(1024, 0).expect("limits"))
            .expect("set");
        assert_eq!(
            api.working_set_of(1234).map(|limits| limits.max_bytes),
            Some(WorkingSetLimits::NO_UPPER_BOUND)
        );
    }

    #[test]
    fn working_set_read_reports_default_applied_and_observed_values() {
        let api = MockApi::sample_workstation();

        // 未安排过的进程：声明式默认值（200 KiB / 无上限），且可还原。
        let default = api.get_working_set(1234).expect("default read");
        assert_eq!(default.min_bytes, 200 * 1024);
        assert_eq!(default.max_bytes, WorkingSetLimits::NO_UPPER_BOUND);
        assert!(default.is_restorable());

        // 写入之后读到的是写入值（读不是"自说自话"）。
        let target = WorkingSetLimits::from_mb(128, 256).expect("target");
        api.set_working_set(1234, target).expect("set");
        assert_eq!(api.get_working_set(1234).expect("read back"), target);

        // 系统可能报告 min=0：Mock 也能如实扮演这种"不可精确还原"的前值。
        let observed_zero = WorkingSetLimits::observed(0, 1_413_120).expect("observed");
        api.seed_working_set(1234, observed_zero);
        let read = api.get_working_set(1234).expect("read observed");
        assert_eq!(read.min_bytes, 0);
        assert!(!read.is_restorable());
    }

    #[test]
    fn working_set_read_fails_like_the_real_backend() {
        let api = MockApi::sample_workstation();

        // 目标进程不存在 ⇒ NotFound（与 Win32Api 同一分类）。
        let err = api.get_working_set(4242).expect_err("no such process");
        assert_eq!(err.kind(), HalErrorKind::NotFound);

        // 注入失败：调用仍被记录，错误分类原样返回。
        api.fail_next(
            HalOp::GetWorkingSet,
            HalError::access_denied("GetProcessWorkingSetSize", "denied by the test"),
        );
        let err = api.get_working_set(1234).expect_err("injected failure");
        assert_eq!(err.kind(), HalErrorKind::AccessDenied);
        assert_eq!(err.operation(), "GetProcessWorkingSetSize");
        assert_eq!(api.call_count(HalOp::GetWorkingSet), 2);
        // 失败不改状态：下一次读仍然是默认值。
        assert_eq!(
            api.get_working_set(1234)
                .expect("read after failure")
                .min_bytes,
            200 * 1024
        );
    }

    #[test]
    fn multi_group_affinity_matches_win32_behaviour() {
        let api = MockApi::with_topology(48, 96);
        api.push_process(777, "big.exe", 12);
        let plan = AffinityPlan::reserve_last_n_cores(96, 4).expect("plan");
        let err = api
            .set_affinity(777, &plan)
            .expect_err("cross-group is unsupported");
        assert_eq!(err.kind(), HalErrorKind::Unsupported);

        // 拆成单组后可逐组应用；非主组走逐线程组亲和性。
        let mut selected_total = 0;
        for group_plan in plan.per_group() {
            let applied = api.set_affinity(777, &group_plan).expect("per-group apply");
            selected_total += applied.plan.selected_logical();
            match applied.method {
                AffinityMethod::ProcessAffinityMask => assert_eq!(applied.threads_updated, 0),
                AffinityMethod::ThreadGroupAffinity => assert_eq!(applied.threads_updated, 12),
            }
        }
        // 计划本身只保留 96 - 4 = 92 个逻辑核。
        assert_eq!(selected_total, 92);
        // 最后应用的是组 1（28 个逻辑核）。
        assert_eq!(
            api.affinity_of(777).map(|plan| plan.selected_logical()),
            Some(28)
        );
    }

    #[test]
    fn run_entry_toggle_is_idempotent_and_reversible() {
        let api = MockApi::sample_workstation();
        let disabled = api
            .set_run_entry_enabled(RunHive::CurrentUser, "Steam", false)
            .expect("disable");
        assert!(!disabled.enabled);
        assert_eq!(disabled.value_name, "[disabled] Steam");

        // 幂等：再次禁用不报错，也不改内容。
        let again = api
            .set_run_entry_enabled(RunHive::CurrentUser, "Steam", false)
            .expect("disable twice");
        assert_eq!(again, disabled);

        // 用禁用态的名字也能定位并恢复。
        let restored = api
            .set_run_entry_enabled(RunHive::CurrentUser, "[disabled] Steam", true)
            .expect("enable");
        assert!(restored.enabled);
        assert_eq!(restored.value_name, "Steam");
        assert_eq!(restored.command, disabled.command);

        let err = api
            .set_run_entry_enabled(RunHive::CurrentUser, "Nope", false)
            .expect_err("missing entry");
        assert_eq!(err.kind(), HalErrorKind::NotFound);
    }

    #[test]
    fn power_scheme_switch_reports_previous_scheme() {
        let api = MockApi::sample_workstation();
        let change = api
            .set_power_scheme(&PowerSchemeSelector::HighPerformance)
            .expect("switch");
        assert_eq!(change.previous.guid, Guid::BALANCED);
        assert_eq!(change.current.guid, Guid::HIGH_PERFORMANCE);
        assert!(change.current.is_high_performance);
        assert_eq!(
            api.query_power_scheme().expect("query").guid,
            Guid::HIGH_PERFORMANCE
        );

        let err = api
            .set_power_scheme(&PowerSchemeSelector::Explicit(Guid::from_u128(0x1234)))
            .expect_err("unknown scheme");
        assert_eq!(err.kind(), HalErrorKind::NotFound);
    }

    #[test]
    fn elevation_and_hardware_are_reported() {
        let api = MockApi::sample_workstation();
        assert!(!api.is_elevated().expect("read"));
        api.set_elevated(true);
        assert!(api.is_elevated().expect("read"));
        let hardware = api.hardware().expect("hardware");
        assert_eq!(hardware.logical_cores, 16);
        assert!(hardware.supports_hyper_threading);
        assert_eq!(hardware.gpu.map(|gpu| gpu.vendor), Some(GpuVendor::Nvidia));
    }

    #[test]
    fn list_processes_is_sorted_by_pid() {
        let api = MockApi::sample_workstation();
        let pids: Vec<u32> = api
            .list_processes()
            .expect("list")
            .iter()
            .map(|process| process.pid)
            .collect();
        assert_eq!(pids, vec![4, 1234, 5678]);
    }
}
