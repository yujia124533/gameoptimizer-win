//! **Plan**：策略求值的输出——一组有序、可解释、可回滚的执行步骤。
//!
//! 与 [`crate::model`]（受约束保护的输入模型）相反，本模块是**输出记录**：字段公开、
//! 可直接读取与序列化（CLI `--json`、审计日志、事件溯源回滚都吃这个结构）。
//!
//! ```text
//! Plan
//! ├── game_id / game_name_zh / game_name_en / pid / policy_origin   —— 这是给谁、按哪份策略算的
//! ├── steps:   Vec<PlanStep>   —— 真正要执行的动作，按顺序（order 从 1 开始）
//! │   └── PlanStep { order, rule_id, rule_line, pid, reason{zh,en}, action, requires_elevation, is_dangerous }
//! └── skipped: Vec<PlanSkip>   —— 没执行的原因（条件不满足 / 硬件降级 / 显式跳过），也是可解释性的一部分
//! ```
//!
//! 关键不变量（由 `gopt-policy` 的测试守住）：
//!
//! 1. `steps` 的顺序 = 规则在 TOML 中的声明顺序；`run_entries` 规则按 (hive, name) 展开；
//! 2. `order` 从 1 连续递增，等于在 `steps` 中的下标 + 1；
//! 3. 每个步骤的 `action` 都能一对一映射到 HAL 的操作（[`PlanStep::hal_op`]），
//!    因此"策略说了什么"与"系统上真的会调什么"之间没有解释鸿沟；
//! 4. 求值不会失败：所有可失败点都在加载期校验，运行期只会产生步骤或跳过说明。

use serde::{Deserialize, Serialize};

use gopt_hal::{
    AffinityPlan, HalOp, PowerSchemeSelector, PriorityClass, RunHive, WorkingSetLimits,
};

use crate::error::PolicyOrigin;
use crate::model::{AffinitySpec, PowerSchemeChoice};

/// 中英双语理由（红线：全部功能中英双语，且两种语言的文案都进审计日志）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    /// 中文理由。
    pub zh: String,
    /// 英文理由。
    pub en: String,
}

impl Reason {
    /// 构造。
    pub fn new(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self {
            zh: zh.into(),
            en: en.into(),
        }
    }
}

/// 步骤被跳过的原因分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipCause {
    /// `when` 条件不满足（例如内存不足、未提权）。
    ConditionNotMet,
    /// 条件满足，但按当前硬件画像无法执行，降级跳过（例如掩码超出处理器组容量）。
    Degraded,
    /// 策略里写了显式空动作（`action = { skip = { ... } }`），把"为什么不做"写进数据。
    ExplicitSkip,
}

impl SkipCause {
    /// 稳定短名（JSON 字段）。
    pub const fn as_str(self) -> &'static str {
        match self {
            SkipCause::ConditionNotMet => "condition_not_met",
            SkipCause::Degraded => "degraded",
            SkipCause::ExplicitSkip => "explicit_skip",
        }
    }

    /// 中文标签。
    pub const fn label_zh(self) -> &'static str {
        match self {
            SkipCause::ConditionNotMet => "条件不满足",
            SkipCause::Degraded => "硬件降级",
            SkipCause::ExplicitSkip => "策略显式跳过",
        }
    }

    /// 英文标签。
    pub const fn label_en(self) -> &'static str {
        match self {
            SkipCause::ConditionNotMet => "condition not met",
            SkipCause::Degraded => "degraded by hardware",
            SkipCause::ExplicitSkip => "explicitly skipped",
        }
    }
}

/// 一条规则没有产生步骤的原因说明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSkip {
    /// 规则 id。
    pub rule_id: String,
    /// 规则在来源文件中的 1 基行号（便于直接跳到 TOML 里改）。
    pub rule_line: Option<u32>,
    /// 跳过原因分类。
    pub cause: SkipCause,
    /// 中英双语理由。
    pub reason: Reason,
}

/// 一个具体动作（已经落到 HAL 类型上）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    /// 设置进程优先级。
    Priority {
        /// 目标优先级类（枚举里没有 REALTIME）。
        class: PriorityClass,
    },
    /// 绑定 CPU 亲和性。
    Affinity {
        /// 解析后的亲和性计划（含每组掩码）。
        plan: AffinityPlan,
        /// 原始声明参数（用于 explain 与审计复现）。
        spec: AffinitySpec,
    },
    /// 设置工作集上下限。
    WorkingSet {
        /// 已校验的上下限。
        limits: WorkingSetLimits,
    },
    /// 切换电源方案。
    PowerScheme {
        /// 策略里的目标（中文/英文标签用）。
        scheme: PowerSchemeChoice,
        /// 交给 HAL 的选择器（`high` → 按名称解析；`balanced` → 内置平衡 GUID）。
        selector: PowerSchemeSelector,
    },
    /// 启用/禁用某个启动项（`run_entries` 规则按 hive 展开成多个步骤）。
    RunEntry {
        /// 注册表根。
        hive: RunHive,
        /// 启动项展示名。
        name: String,
        /// `true` = 启用，`false` = 禁用。
        enabled: bool,
        /// 条目不存在时按"跳过"处理而不是报错。
        ignore_missing: bool,
    },
}

impl PlanAction {
    /// 该动作对应的 HAL 操作（策略与执行之间的"翻译表"）。
    pub const fn hal_op(&self) -> HalOp {
        match self {
            PlanAction::Priority { .. } => HalOp::SetPriority,
            PlanAction::Affinity { .. } => HalOp::SetAffinity,
            PlanAction::WorkingSet { .. } => HalOp::SetWorkingSet,
            PlanAction::PowerScheme { .. } => HalOp::SetPowerScheme,
            PlanAction::RunEntry { .. } => HalOp::SetRunEntryEnabled,
        }
    }

    /// 亲和性动作的执行批次：单组计划返回 1 个，跨处理器组（>64 逻辑核）返回逐组拆分结果，
    /// 非亲和性动作返回空。
    ///
    /// HAL 契约：`set_affinity` 对跨组计划返回 `Unsupported`，执行方必须按
    /// [`AffinityPlan::per_group`] 逐组应用（非主组走逐线程 `SetThreadGroupAffinity`）。
    pub fn affinity_batches(&self) -> Vec<AffinityPlan> {
        match self {
            PlanAction::Affinity { plan, .. } => {
                if plan.is_single_group() {
                    vec![plan.clone()]
                } else {
                    plan.per_group()
                }
            }
            _ => Vec::new(),
        }
    }
}

/// 一个有顺序的执行步骤。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    /// 执行顺序（1 基，等于在 `Plan::steps` 中的下标 + 1）。
    pub order: u32,
    /// 触发本步骤的规则 id。
    pub rule_id: String,
    /// 规则在来源文件中的 1 基行号。
    pub rule_line: Option<u32>,
    /// 目标进程。
    pub pid: u32,
    /// 中英双语理由。
    pub reason: Reason,
    /// 具体动作。
    pub action: PlanAction,
    /// 是否（可能）需要管理员权限。
    pub requires_elevation: bool,
    /// 是否属于危险动作（改整机状态、用户可见：电源方案 / 启动项）。
    pub is_dangerous: bool,
}

impl PlanStep {
    /// 该步骤对应的 HAL 操作。
    pub const fn hal_op(&self) -> HalOp {
        self.action.hal_op()
    }

    /// 亲和性动作的执行批次（见 [`PlanAction::affinity_batches`]）。
    pub fn affinity_batches(&self) -> Vec<AffinityPlan> {
        self.action.affinity_batches()
    }
}

/// 一次求值的完整结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// 游戏策略 id。
    pub game_id: String,
    /// 中文名。
    pub game_name_zh: String,
    /// 英文名。
    pub game_name_en: String,
    /// 目标进程。
    pub pid: u32,
    /// 这份计划来自哪份策略文件（内置 / 用户覆盖）。
    pub policy_origin: PolicyOrigin,
    /// 有序执行步骤。
    pub steps: Vec<PlanStep>,
    /// 没有执行的规则及原因。
    pub skipped: Vec<PlanSkip>,
}

impl Plan {
    /// 构造（`steps` / `skipped` 由求值器按顺序给出）。
    pub fn new(
        game_id: impl Into<String>,
        game_name_zh: impl Into<String>,
        game_name_en: impl Into<String>,
        pid: u32,
        policy_origin: PolicyOrigin,
        steps: Vec<PlanStep>,
        skipped: Vec<PlanSkip>,
    ) -> Self {
        Self {
            game_id: game_id.into(),
            game_name_zh: game_name_zh.into(),
            game_name_en: game_name_en.into(),
            pid,
            policy_origin,
            steps,
            skipped,
        }
    }

    /// 步骤数。
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// 跳过的规则数。
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    /// 是否没有任何可执行步骤。
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// 是否有步骤需要管理员权限。
    pub fn requires_elevation(&self) -> bool {
        self.steps.iter().any(|step| step.requires_elevation)
    }

    /// 是否含危险动作。
    pub fn has_dangerous_steps(&self) -> bool {
        self.steps.iter().any(|step| step.is_dangerous)
    }

    /// 计划里会用到的全部 HAL 操作（按步骤顺序，可重复）。
    pub fn hal_ops(&self) -> Vec<HalOp> {
        self.steps.iter().map(PlanStep::hal_op).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gopt_hal::{HalOp, PriorityClass};

    fn step(order: u32, action: PlanAction) -> PlanStep {
        PlanStep {
            order,
            rule_id: format!("rule-{order}"),
            rule_line: Some(order + 1),
            pid: 4242,
            reason: Reason::new("中文理由", "english reason"),
            requires_elevation: action.hal_op() == HalOp::SetPowerScheme,
            is_dangerous: action.hal_op() == HalOp::SetRunEntryEnabled,
            action,
        }
    }

    #[test]
    fn skip_cause_names_are_stable() {
        assert_eq!(SkipCause::ConditionNotMet.as_str(), "condition_not_met");
        assert_eq!(SkipCause::Degraded.as_str(), "degraded");
        assert_eq!(SkipCause::ExplicitSkip.as_str(), "explicit_skip");
        for cause in [
            SkipCause::ConditionNotMet,
            SkipCause::Degraded,
            SkipCause::ExplicitSkip,
        ] {
            assert!(!cause.label_zh().is_empty());
            assert!(!cause.label_en().is_empty());
        }
    }

    #[test]
    fn plan_summarises_steps_and_permissions() {
        let plan = Plan::new(
            "cs2",
            "CS2",
            "Counter-Strike 2",
            1234,
            PolicyOrigin::builtin("cs2.toml"),
            vec![
                step(
                    1,
                    PlanAction::Priority {
                        class: PriorityClass::High,
                    },
                ),
                step(
                    2,
                    PlanAction::WorkingSet {
                        limits: WorkingSetLimits::from_mb(256, 0).expect("limits"),
                    },
                ),
            ],
            vec![PlanSkip {
                rule_id: "affinity".to_string(),
                rule_line: Some(20),
                cause: SkipCause::ConditionNotMet,
                reason: Reason::new(
                    "条件不满足：物理核 ≥ 3",
                    "condition not met: physical cores ≥ 3",
                ),
            }],
        );
        assert_eq!(plan.step_count(), 2);
        assert_eq!(plan.skipped_count(), 1);
        assert!(!plan.is_empty());
        assert!(!plan.requires_elevation());
        assert!(!plan.has_dangerous_steps());
        assert_eq!(
            plan.hal_ops(),
            vec![HalOp::SetPriority, HalOp::SetWorkingSet]
        );
        assert_eq!(plan.steps[0].reason.zh, "中文理由");
        assert!(plan.steps[1].affinity_batches().is_empty());

        let elevated = Plan::new(
            "genshin",
            "原神",
            "Genshin Impact",
            1,
            PolicyOrigin::user(r"C:\x\y.toml"),
            vec![step(
                1,
                PlanAction::PowerScheme {
                    scheme: PowerSchemeChoice::High,
                    selector: PowerSchemeChoice::High.selector(),
                },
            )],
            Vec::new(),
        );
        assert!(elevated.requires_elevation());
        assert!(!elevated.has_dangerous_steps());
        assert_eq!(elevated.steps[0].rule_line, Some(2));
    }

    #[test]
    fn affinity_batches_split_cross_group_plans() {
        let plan = AffinityPlan::full(16).expect("plan");
        let action = PlanAction::Affinity {
            plan,
            spec: AffinitySpec::new(1, crate::model::ReserveSide::First, false, None),
        };
        let batches = action.affinity_batches();
        assert_eq!(batches.len(), 1);
        assert!(batches[0].is_single_group());

        let plan = AffinityPlan::reserve_last_n_cores(96, 4).expect("plan");
        let action = PlanAction::Affinity {
            plan,
            spec: AffinitySpec::new(4, crate::model::ReserveSide::Last, false, None),
        };
        let batches = action.affinity_batches();
        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches
                .iter()
                .map(AffinityPlan::selected_logical)
                .sum::<u32>(),
            92
        );
    }
}
