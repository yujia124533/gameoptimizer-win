//! 求值：`硬件画像 + 提权状态 + 目标 pid` → [`Plan`]。
//!
//! 求值**不会失败**：
//!
//! * 加载期已经把"语法/类型/语义"问题全部挡掉（带文件名与行号）；
//! * 运行期唯一可能失败的环节是"按当前硬件画像算不出来的亲和性"（例如显式掩码超出组容量），
//!   它按**降级**处理：写进 `Plan::skipped` 的 [`SkipCause::Degraded`]，附带中英双语原因。
//!
//! 因此调用方拿到的永远是一份可以直接展示、直接执行的计划：要么有一个步骤，
//! 要么有一条"为什么没有步骤"的说明。
//!
//! ```
//! use gopt_hal::{MockApi, SystemApi};
//! use gopt_policy::{EvalInput, PolicyLoader};
//!
//! let api = MockApi::sample_workstation();
//! let input = EvalInput::from_api(&api)?;
//! let set = PolicyLoader::builtin_only().load().into_result()?;
//! let cs2 = set.get("cs2").expect("built-in cs2 policy");
//! let plan = cs2.plan(&input, 1234);
//! assert_eq!(plan.steps[0].rule_id, "priority");
//! assert!(plan.steps.iter().all(|step| step.pid == 1234));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use gopt_hal::{
    AffinityPlan, GpuInfo, GpuVendor, HalResult, HardwareInfo, PriorityClass, RunHive, SystemApi,
    WorkingSetLimits,
};

use crate::affinity::resolve_affinity;
use crate::model::{
    priority_label_en, priority_label_zh, Action, AffinitySpec, GamePolicy, PowerSchemeChoice,
};
use crate::plan::{Plan, PlanAction, PlanSkip, PlanStep, Reason, SkipCause};

/// 一次求值的输入：硬件画像 + 提权状态。
///
/// 刻意只包含"与具体进程无关"的部分：`pid` 是 [`GamePolicy::plan`] 的参数，
/// 这样同一份画像可以对多个游戏进程复用（也便于无管理员单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalInput {
    hardware: HardwareInfo,
    is_elevated: bool,
}

impl EvalInput {
    /// 由硬件画像与提权状态构造。
    pub const fn new(hardware: HardwareInfo, is_elevated: bool) -> Self {
        Self {
            hardware,
            is_elevated,
        }
    }

    /// 通过 HAL 采集（真实后端 = Win32，测试后端 = Mock，二者同一套类型）。
    pub fn from_api(api: &dyn SystemApi) -> HalResult<Self> {
        let hardware = api.hardware()?;
        let is_elevated = api.is_elevated()?;
        Ok(Self::new(hardware, is_elevated))
    }

    /// 硬件画像。
    pub const fn hardware(&self) -> &HardwareInfo {
        &self.hardware
    }

    /// 是否已提权。
    pub const fn is_elevated(&self) -> bool {
        self.is_elevated
    }

    /// 系统内存（MiB）。
    pub const fn ram_mb(&self) -> u64 {
        self.hardware.system_ram_mb
    }

    /// 系统内存（GiB，向下取整；与 C++ 版 `systemRamMB` 阈值同口径）。
    pub const fn ram_gb(&self) -> u32 {
        // 16384 MiB → 16 GiB；15872 MiB（15.5 GiB）→ 15 GiB（仍落在"8~16GB 减半"档）。
        (self.hardware.system_ram_mb / 1024) as u32
    }

    /// 首选显示适配器。
    pub fn gpu(&self) -> Option<&GpuInfo> {
        self.hardware.gpu.as_ref()
    }

    /// 首选显示适配器厂商（没有适配器时为 `None`）。
    pub fn gpu_vendor(&self) -> Option<GpuVendor> {
        self.hardware.gpu.as_ref().map(|gpu| gpu.vendor)
    }
}

impl GamePolicy {
    /// 对某个 pid 求值，得到这份策略的可执行计划。
    ///
    /// 步骤顺序 = 规则声明顺序；`run_entries` 规则按 (hive, name) 展开：
    /// `hive = "both"` + 2 个名字 = 4 个步骤（HKCU 在前）。
    pub fn plan(&self, input: &EvalInput, pid: u32) -> Plan {
        let mut steps: Vec<PlanStep> = Vec::new();
        let mut skipped: Vec<PlanSkip> = Vec::new();

        for rule in self.rules() {
            if let Some(condition) = rule.when() {
                if !condition.evaluate(input) {
                    skipped.push(PlanSkip {
                        rule_id: rule.id().to_string(),
                        rule_line: rule.line(),
                        cause: SkipCause::ConditionNotMet,
                        reason: Reason::new(
                            format!("条件不满足：{}", condition.describe_zh()),
                            format!("condition not met: {}", condition.describe_en()),
                        ),
                    });
                    continue;
                }
            }

            match rule.action() {
                Action::Skip { reason } => skipped.push(PlanSkip {
                    rule_id: rule.id().to_string(),
                    rule_line: rule.line(),
                    cause: SkipCause::ExplicitSkip,
                    reason: reason.clone(),
                }),

                Action::Priority { class } => steps.push(make_step(
                    rule,
                    pid,
                    PlanAction::Priority { class: *class },
                    priority_reason(self, *class),
                    false,
                    false,
                )),

                Action::Affinity(spec) => match resolve_affinity(input.hardware(), spec) {
                    Ok(plan) => steps.push(make_step(
                        rule,
                        pid,
                        PlanAction::Affinity {
                            plan: plan.clone(),
                            spec: spec.clone(),
                        },
                        affinity_reason(spec, &plan),
                        false,
                        false,
                    )),
                    // 算不出来不是错误，而是"这台机器上不适用"——降级并解释。
                    Err(reason) => skipped.push(PlanSkip {
                        rule_id: rule.id().to_string(),
                        rule_line: rule.line(),
                        cause: SkipCause::Degraded,
                        reason,
                    }),
                },

                Action::WorkingSet { limits } => steps.push(make_step(
                    rule,
                    pid,
                    PlanAction::WorkingSet { limits: *limits },
                    working_set_reason(*limits),
                    false,
                    false,
                )),

                Action::PowerScheme { scheme } => steps.push(make_step(
                    rule,
                    pid,
                    PlanAction::PowerScheme {
                        scheme: *scheme,
                        selector: scheme.selector(),
                    },
                    power_scheme_reason(*scheme),
                    true,
                    true,
                )),

                Action::RunEntries {
                    hive,
                    names,
                    enabled,
                    ignore_missing,
                } => {
                    for target_hive in hive.hives() {
                        for name in names {
                            steps.push(make_step(
                                rule,
                                pid,
                                PlanAction::RunEntry {
                                    hive: *target_hive,
                                    name: name.clone(),
                                    enabled: *enabled,
                                    ignore_missing: *ignore_missing,
                                },
                                run_entry_reason(*target_hive, name, *enabled),
                                matches!(target_hive, RunHive::LocalMachine),
                                true,
                            ));
                        }
                    }
                }
            }
        }

        for (index, step) in steps.iter_mut().enumerate() {
            step.order = index as u32 + 1;
        }

        Plan::new(
            self.id(),
            self.name_zh(),
            self.name_en(),
            pid,
            self.origin().clone(),
            steps,
            skipped,
        )
    }
}

fn make_step(
    rule: &crate::model::Rule,
    pid: u32,
    action: PlanAction,
    reason: Reason,
    requires_elevation: bool,
    is_dangerous: bool,
) -> PlanStep {
    PlanStep {
        order: 0, // 由 plan() 统一编号
        rule_id: rule.id().to_string(),
        rule_line: rule.line(),
        pid,
        reason,
        action,
        requires_elevation,
        is_dangerous,
    }
}

fn priority_reason(policy: &GamePolicy, class: PriorityClass) -> Reason {
    Reason::new(
        format!(
            "把 {} 的进程优先级设为「{}」（本工具上限 HIGH，绝不使用 REALTIME）",
            policy.name_zh(),
            priority_label_zh(class)
        ),
        format!(
            "set the {} process priority to {} (this tool caps at high and never uses realtime)",
            policy.name_en(),
            priority_label_en(class)
        ),
    )
}

fn affinity_reason(spec: &AffinitySpec, plan: &AffinityPlan) -> Reason {
    if plan.is_single_group() {
        Reason::new(
            format!(
                "绑定 CPU 亲和性：{}；实际选中 {} / {} 个逻辑处理器",
                spec.describe_zh(),
                plan.selected_logical(),
                plan.total_logical()
            ),
            format!(
                "bind CPU affinity: {}; {} of {} logical processors selected",
                spec.describe_en(),
                plan.selected_logical(),
                plan.total_logical()
            ),
        )
    } else {
        let groups = plan.requests().len();
        Reason::new(
            format!(
                "绑定 CPU 亲和性：{}；实际选中 {} / {} 个逻辑处理器，跨 {groups} 个处理器组（需逐组应用）",
                spec.describe_zh(),
                plan.selected_logical(),
                plan.total_logical()
            ),
            format!(
                "bind CPU affinity: {}; {} of {} logical processors selected across {groups} processor groups (apply one group at a time)",
                spec.describe_en(),
                plan.selected_logical(),
                plan.total_logical()
            ),
        )
    }
}

fn working_set_reason(limits: WorkingSetLimits) -> Reason {
    let min_mb = limits.min_bytes / (1024 * 1024);
    let upper_zh = if limits.max_bytes >= WorkingSetLimits::NO_UPPER_BOUND {
        "无上限".to_string()
    } else {
        format!("上限 {} MB", limits.max_bytes / (1024 * 1024))
    };
    let upper_en = if limits.max_bytes >= WorkingSetLimits::NO_UPPER_BOUND {
        "no upper bound".to_string()
    } else {
        format!("upper bound {} MiB", limits.max_bytes / (1024 * 1024))
    };
    Reason::new(
        format!("设置工作集下限 {min_mb} MB（{upper_zh}；可被系统随时回收，不是内存预分配）"),
        format!(
            "set the working-set floor to {min_mb} MiB ({upper_en}; reclaimable at any time, not a pre-allocation)"
        ),
    )
}

fn power_scheme_reason(scheme: PowerSchemeChoice) -> Reason {
    Reason::new(
        format!(
            "切换到「{}」电源方案（系统级设置、需要管理员、可一键还原）",
            scheme.label_zh()
        ),
        format!(
            "switch the active power scheme to {} (system-wide, requires administrator, reversible)",
            scheme.label_en()
        ),
    )
}

fn run_entry_reason(hive: RunHive, name: &str, enabled: bool) -> Reason {
    let qualified = format!("{hive}:{name}");
    let admin_zh = if matches!(hive, RunHive::LocalMachine) {
        "（HKLM 需要管理员权限）"
    } else {
        ""
    };
    let admin_en = if matches!(hive, RunHive::LocalMachine) {
        " (HKLM requires administrator)"
    } else {
        ""
    };
    if enabled {
        Reason::new(
            format!("启用开机启动项 {qualified}：把「[disabled] {name}」的名字改回去，内容原样保留{admin_zh}"),
            format!(
                "enable the startup entry {qualified}: revert the rename migration, the value is kept intact{admin_en}"
            ),
        )
    } else {
        Reason::new(
            format!("禁用开机启动项 {qualified}：改名迁移为「[disabled] {name}」，可一键还原{admin_zh}"),
            format!(
                "disable the startup entry {qualified}: rename migration to `[disabled] {name}`, fully reversible{admin_en}"
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PolicyOrigin;
    use crate::model::{ReserveSide, Rule};
    use crate::{Comparison, Condition, ConditionField, ConditionTerm};
    use gopt_hal::{CoreLayout, MockApi, ProcessorGroup};

    fn hardware(logical: u32, physical: u32, ram_mb: u64) -> HardwareInfo {
        let smt = (logical / physical.max(1)).max(1);
        let mut layout = Vec::new();
        let mut cursor = 0u32;
        for core in 0..physical {
            let mut mask = 0u64;
            for _ in 0..smt {
                if cursor < 64 {
                    mask |= 1u64 << cursor;
                }
                cursor += 1;
            }
            layout.push(CoreLayout::new(core, 0, mask));
        }
        HardwareInfo {
            cpu_model: "Test CPU".to_string(),
            physical_cores: physical,
            logical_cores: logical,
            supports_hyper_threading: logical > physical,
            cpu_base_freq_mhz: 3600,
            core_layout: layout,
            processor_groups: vec![ProcessorGroup::new(0, logical.min(64))],
            gpu: Some(GpuInfo {
                vendor: GpuVendor::Nvidia,
                vendor_id: 0x10de,
                device_id: 0x1,
                model: "Test GPU".to_string(),
                vram_mb: 8192,
                driver_version: None,
                is_hardware: true,
                is_software_adapter: false,
            }),
            system_ram_mb: ram_mb,
            available_ram_mb: ram_mb / 2,
            large_pages_available: false,
            warnings: Vec::new(),
        }
    }

    fn make_policy(rules: Vec<Rule>) -> GamePolicy {
        GamePolicy::new(
            "test-game",
            "测试游戏",
            "Test Game",
            "test.exe",
            rules,
            PolicyOrigin::builtin("test.toml"),
        )
    }

    #[test]
    fn steps_keep_the_declaration_order_and_are_numbered_from_one() {
        let policy = make_policy(vec![
            Rule::new(
                "priority",
                None,
                Action::Priority {
                    class: PriorityClass::High,
                },
            )
            .with_line(Some(10)),
            Rule::new(
                "affinity",
                None,
                Action::Affinity(AffinitySpec::new(1, ReserveSide::First, false, None)),
            )
            .with_line(Some(14)),
            Rule::new(
                "working-set",
                None,
                Action::WorkingSet {
                    limits: WorkingSetLimits::from_mb(256, 0).expect("limits"),
                },
            ),
        ]);
        let input = EvalInput::new(hardware(16, 8, 32768), false);
        let plan = policy.plan(&input, 4321);

        assert_eq!(plan.step_count(), 3);
        assert_eq!(plan.skipped_count(), 0);
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.rule_id.as_str())
                .collect::<Vec<_>>(),
            vec!["priority", "affinity", "working-set"]
        );
        assert_eq!(plan.steps[0].order, 1);
        assert_eq!(plan.steps[1].order, 2);
        assert_eq!(plan.steps[2].order, 3);
        assert_eq!(plan.steps[0].pid, 4321);
        assert_eq!(plan.steps[0].rule_line, Some(10));
        assert!(plan.steps[0].reason.zh.contains("优先级"));
        assert!(plan.steps[0].reason.en.contains("priority"));
        assert!(!plan.requires_elevation());
        assert!(!plan.has_dangerous_steps());
        assert_eq!(plan.policy_origin, PolicyOrigin::builtin("test.toml"));

        match &plan.steps[1].action {
            PlanAction::Affinity { plan, spec } => {
                assert_eq!(plan.selected_logical(), 15);
                assert_eq!(spec.reserve_cores(), 1);
            }
            other => panic!("expected affinity, got {other:?}"),
        }
        assert_eq!(plan.steps[1].affinity_batches().len(), 1);
        assert!(plan.steps[1].reason.zh.contains("实际选中 15 / 16"));
    }

    #[test]
    fn unmet_conditions_and_degradation_land_in_the_skipped_section() {
        let policy = make_policy(vec![
            Rule::new(
                "affinity-high-end",
                Some(Condition::new(vec![ConditionTerm::new(
                    ConditionField::PhysicalCores,
                    Comparison::at_least(16),
                    None,
                )])),
                Action::Affinity(AffinitySpec::new(1, ReserveSide::First, false, None)),
            )
            .with_line(Some(4)),
            Rule::new(
                "affinity-broken-mask",
                None,
                Action::Affinity(AffinitySpec::new(
                    0,
                    ReserveSide::First,
                    false,
                    Some(1 << 40),
                )),
            ),
            Rule::new(
                "explicit-skip",
                None,
                Action::Skip {
                    reason: Reason::new("内存太小，不做任何设置", "too little RAM, nothing to do"),
                },
            ),
        ]);
        let input = EvalInput::new(hardware(8, 4, 8192), false);
        let plan = policy.plan(&input, 999);

        assert!(plan.is_empty());
        assert_eq!(plan.skipped_count(), 3);
        assert_eq!(plan.skipped[0].cause, SkipCause::ConditionNotMet);
        assert_eq!(plan.skipped[0].rule_id, "affinity-high-end");
        assert_eq!(plan.skipped[0].rule_line, Some(4));
        assert!(
            plan.skipped[0].reason.zh.contains("条件不满足"),
            "{}",
            plan.skipped[0].reason.zh
        );
        assert!(plan.skipped[0].reason.en.contains("condition not met"));
        assert_eq!(plan.skipped[1].cause, SkipCause::Degraded);
        assert_eq!(plan.skipped[2].cause, SkipCause::ExplicitSkip);
        assert_eq!(plan.skipped[2].reason.zh, "内存太小，不做任何设置");
    }

    #[test]
    fn run_entries_expand_per_hive_and_name_in_a_stable_order() {
        let policy = make_policy(vec![Rule::new(
            "release-startup",
            None,
            Action::RunEntries {
                hive: crate::model::RunHiveSpec::Both,
                names: vec!["Discord".to_string(), "Steam".to_string()],
                enabled: false,
                ignore_missing: true,
            },
        )
        .with_line(Some(7))]);
        let input = EvalInput::new(hardware(8, 4, 16384), false);
        let plan = policy.plan(&input, 55);

        assert_eq!(plan.step_count(), 4);
        let entries: Vec<(RunHive, &str)> = plan
            .steps
            .iter()
            .map(|step| match &step.action {
                PlanAction::RunEntry { hive, name, .. } => (*hive, name.as_str()),
                other => panic!("expected run entry, got {other:?}"),
            })
            .collect();
        assert_eq!(
            entries,
            vec![
                (RunHive::CurrentUser, "Discord"),
                (RunHive::CurrentUser, "Steam"),
                (RunHive::LocalMachine, "Discord"),
                (RunHive::LocalMachine, "Steam"),
            ]
        );
        assert!(!plan.steps[0].requires_elevation);
        assert!(plan.steps[2].requires_elevation);
        assert!(plan.has_dangerous_steps());
        assert!(plan.steps[0].reason.zh.contains("HKCU:Discord"));
        assert!(plan.steps[2].reason.zh.contains("需要管理员权限"));
    }

    #[test]
    fn power_scheme_steps_are_flagged_as_elevated_and_dangerous() {
        let policy = make_policy(vec![Rule::new(
            "power",
            None,
            Action::PowerScheme {
                scheme: PowerSchemeChoice::High,
            },
        )]);
        let input = EvalInput::new(hardware(8, 4, 16384), true);
        let plan = policy.plan(&input, 77);
        assert!(plan.requires_elevation());
        assert!(plan.has_dangerous_steps());
        match &plan.steps[0].action {
            PlanAction::PowerScheme { selector, .. } => {
                assert_eq!(selector, &gopt_hal::PowerSchemeSelector::HighPerformance);
            }
            other => panic!("expected power scheme, got {other:?}"),
        }
        assert_eq!(plan.steps[0].hal_op(), gopt_hal::HalOp::SetPowerScheme);
    }

    #[test]
    fn empty_rule_set_yields_an_empty_plan_without_panicking() {
        let policy = make_policy(Vec::new());
        let input = EvalInput::new(hardware(8, 4, 16384), false);
        let plan = policy.plan(&input, 1);
        assert!(plan.is_empty());
        assert_eq!(plan.skipped_count(), 0);
        assert!(plan.hal_ops().is_empty());

        // 完全没有硬件信息（0 核 / 0 内存）也不 panic：只产出可解释的跳过。
        let zero = EvalInput::new(
            HardwareInfo {
                cpu_model: String::new(),
                physical_cores: 0,
                logical_cores: 0,
                supports_hyper_threading: false,
                cpu_base_freq_mhz: 0,
                core_layout: Vec::new(),
                processor_groups: Vec::new(),
                gpu: None,
                system_ram_mb: 0,
                available_ram_mb: 0,
                large_pages_available: false,
                warnings: Vec::new(),
            },
            false,
        );
        let policy = make_policy(vec![Rule::new(
            "affinity",
            None,
            Action::Affinity(AffinitySpec::new(1, ReserveSide::First, false, None)),
        )]);
        let plan = policy.plan(&zero, 1);
        assert!(plan.is_empty());
        assert_eq!(plan.skipped_count(), 1);
        assert_eq!(plan.skipped[0].cause, SkipCause::Degraded);
    }

    #[test]
    fn eval_input_probes_the_mock_backend_without_admin_rights() {
        let api = MockApi::sample_workstation();
        let input = EvalInput::from_api(&api).expect("probe");
        assert!(!input.is_elevated());
        assert_eq!(input.hardware().logical_cores, 16);
        assert_eq!(input.ram_gb(), 16);
        assert_eq!(input.ram_mb(), 16384);
        assert_eq!(input.gpu_vendor(), Some(GpuVendor::Nvidia));
        assert!(api
            .calls()
            .iter()
            .any(|call| call.op == gopt_hal::HalOp::Hardware));

        // 提权状态同样来自 HAL。
        api.set_elevated(true);
        let input = EvalInput::from_api(&api).expect("probe");
        assert!(input.is_elevated());
    }
}
