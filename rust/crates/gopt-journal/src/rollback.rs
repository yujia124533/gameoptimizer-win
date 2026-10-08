//! 事件溯源回滚：把 `apply` / `imported` 记录按 id **逆序**翻成结构化撤销步骤。
//!
//! * **不含任何系统调用**（本 crate 的职责边界）：计划只是数据；
//!   真正执行的是 `gopt-core`，它把每一步翻成 `gopt-hal::SystemApi` 调用。
//! * **逆序**：先撤销最新的修改，避免"后来者依赖前者"的状态倒灌。
//! * **不静默跳过**：任何无法解读成动作的步骤都会保留为 [`RollbackAction::NotActionable`]
//!   并带上原因（红线：显式报错而不是静默跳过）；调用方用
//!   [`RollbackPlan::executable_steps`] 拿可执行子集、用 [`RollbackPlan::not_actionable`]
//!   统计需要人工确认的步骤数。
//! * `kind = rollback` 的记录**不进计划**：它们是"某次回滚已执行"的审计条目，
//!   不是待撤销的修改。
//! * 要区分"已经被撤销过的 apply"，用 [`undone_apply_ids`] +
//!   [`plan_rollback_pending`]（约定：回滚记录的 `rule_id` 写成 `rollback:<apply_id>`）。

use gopt_hal::{AffinityPlan, Guid, PriorityClass, RunHive, WorkingSetLimits};
use serde_json::Value;

use crate::canonical::brief;
use crate::error::{JournalError, JournalResult};
use crate::payload;
use crate::record::{JournalKind, JournalRecord};

/// 回滚记录用来标记"撤销了哪条 apply"的 `rule_id` 前缀。
pub const ROLLBACK_RULE_PREFIX: &str = "rollback:";

/// 一条撤销动作（结构化，可直接 `--json` 输出）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RollbackAction {
    /// 把进程优先级恢复成写入前的值。
    RestorePriority {
        /// 目标进程。
        pid: u32,
        /// 进程名（可选，仅用于展示/审计）。
        name: Option<String>,
        /// 恢复到的优先级（类型层面不可能出现 REALTIME）。
        priority: PriorityClass,
    },
    /// 把进程亲和性恢复成写入前的计划。
    RestoreAffinity {
        /// 目标进程。
        pid: u32,
        /// 进程名（可选）。
        name: Option<String>,
        /// 恢复到的亲和性计划。
        plan: AffinityPlan,
    },
    /// 把工作集上下限恢复成写入前的值。
    RestoreWorkingSet {
        /// 目标进程。
        pid: u32,
        /// 进程名（可选）。
        name: Option<String>,
        /// 恢复到的上下限。
        limits: WorkingSetLimits,
    },
    /// 把活动电源方案恢复成写入前的方案。
    RestorePowerScheme {
        /// 恢复到的电源方案 GUID。
        guid: Guid,
        /// 方案名（可选，仅用于展示）。
        name: Option<String>,
    },
    /// 把开机启动项恢复成写入前的名字/启用状态。
    RestoreRunEntry {
        /// 注册表根。
        hive: RunHive,
        /// 恢复到的注册表值名（可能带 `"[disabled] "` 前缀）。
        value_name: String,
        /// 恢复到的启用状态。
        enabled: bool,
    },
    /// 由 C++ 旧格式导入的快照：一次恢复进程态/工作集/电源（字段可空 = 旧文件里没有或不可用）。
    ///
    /// 注意：旧 `savepoints.txt` 只记录"优化前的状态"，不记录当时实际应用了哪些项，
    /// 因此执行方需要自行确认（`note` 里会带上来源与不确定说明）。
    RestoreLegacySnapshot {
        /// 目标进程。
        process_id: u32,
        /// 优化前优先级（旧文件里的 REALTIME 已在导入时被红线拦下 ⇒ `None`）。
        priority: Option<PriorityClass>,
        /// 优化前亲和性掩码（处理器组 0；`None` = 旧文件未记录）。
        affinity_mask: Option<u64>,
        /// 优化前工作集上下限。
        working_set: Option<WorkingSetLimits>,
        /// 优化前电源方案 GUID（文本，未校验时保留原文）。
        power_scheme_guid: Option<String>,
    },
    /// 不可执行：`before` 缺失、形状无法识别，或被安全红线拒绝。
    NotActionable {
        /// 稳定英文原因。
        reason: String,
    },
}

impl RollbackAction {
    /// 该动作是否可执行（`NotActionable` 之外全部可执行）。
    pub const fn is_executable(&self) -> bool {
        !matches!(self, RollbackAction::NotActionable { .. })
    }

    /// 一行英文描述（CLI 的 `explain` 用；`--json` 输出结构化字段）。
    pub fn describe(&self) -> String {
        match self {
            RollbackAction::RestorePriority {
                pid,
                priority,
                name,
            } => match name {
                Some(name) => format!("restore priority of pid {pid} ({name}) to {priority}"),
                None => format!("restore priority of pid {pid} to {priority}"),
            },
            RollbackAction::RestoreAffinity { pid, plan, .. } => {
                format!("restore affinity of pid {pid} to {plan}")
            }
            RollbackAction::RestoreWorkingSet { pid, limits, .. } => format!(
                "restore working set of pid {pid} to min {} bytes / max {} bytes",
                limits.min_bytes, limits.max_bytes
            ),
            RollbackAction::RestorePowerScheme { guid, name } => match name {
                Some(name) => format!("restore the active power scheme to {name} ({guid})"),
                None => format!("restore the active power scheme to {guid}"),
            },
            RollbackAction::RestoreRunEntry {
                hive,
                value_name,
                enabled,
            } => format!(
                "restore the run entry {hive}:{value_name} to {}",
                if *enabled { "enabled" } else { "disabled" }
            ),
            RollbackAction::RestoreLegacySnapshot {
                process_id,
                priority,
                affinity_mask,
                working_set,
                power_scheme_guid,
            } => {
                let mut parts: Vec<String> = Vec::new();
                if let Some(priority) = priority {
                    parts.push(format!("priority {priority}"));
                }
                if let Some(mask) = affinity_mask {
                    parts.push(format!("affinity mask 0x{mask:016x}"));
                }
                if let Some(limits) = working_set {
                    parts.push(format!(
                        "working set {}..{} bytes",
                        limits.min_bytes, limits.max_bytes
                    ));
                }
                if let Some(guid) = power_scheme_guid {
                    parts.push(format!("power scheme {guid}"));
                }
                if parts.is_empty() {
                    format!("restore the legacy snapshot of pid {process_id} (no usable fields)")
                } else {
                    format!(
                        "restore the legacy snapshot of pid {process_id} ({})",
                        parts.join(", ")
                    )
                }
            }
            RollbackAction::NotActionable { reason } => format!("not actionable: {reason}"),
        }
    }
}

/// 一条撤销步骤（对应链上一条记录）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RollbackStep {
    /// 被撤销的记录 id。
    pub journal_id: u64,
    /// 被撤销记录的类型（`apply` / `imported`）。
    pub kind: JournalKind,
    /// 被撤销记录的作用对象。
    pub target: String,
    /// 结构化撤销动作。
    pub action: RollbackAction,
    /// 额外说明（旧格式来源、不确定项等）。
    pub note: Option<String>,
}

impl RollbackStep {
    /// 由一条链上记录生成撤销步骤（纯函数，不碰系统）。
    pub fn from_record(record: &JournalRecord) -> Self {
        let (action, note) = match record.before.as_ref() {
            None => (
                RollbackAction::NotActionable {
                    reason:
                        "the record carries no `before` payload, so the previous state is unknown"
                            .to_string(),
                },
                Some("written state cannot be undone from this record alone".to_string()),
            ),
            Some(before) => decode_action(before),
        };
        Self {
            journal_id: record.id,
            kind: record.kind,
            target: record.target.clone(),
            action,
            note,
        }
    }

    /// 一行英文描述（`#3 restore priority of pid 1234 to normal`）。
    pub fn describe(&self) -> String {
        format!(
            "#{} {} → {}",
            self.journal_id,
            self.target,
            self.action.describe()
        )
    }
}

/// 逆序回滚计划（纯数据）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RollbackPlan {
    /// 计划覆盖到哪条记录（含）。
    pub to_id: u64,
    /// `id <= to_id` 的记录条数（含不进计划的 `rollback` 记录）。
    pub source_records: usize,
    /// 撤销步骤：按 `journal_id` **降序**。
    pub steps: Vec<RollbackStep>,
}

impl RollbackPlan {
    /// 步骤数。
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// 是否没有步骤（空日志或该区间内没有可逆记录）。
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// 可执行步骤数。
    pub fn actionable(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.action.is_executable())
            .count()
    }

    /// 不可执行（需要人工确认）的步骤数。
    pub fn not_actionable(&self) -> usize {
        self.len() - self.actionable()
    }

    /// 可执行步骤的子集（顺序不变）。
    pub fn executable_steps(&self) -> impl Iterator<Item = &RollbackStep> {
        self.steps.iter().filter(|step| step.action.is_executable())
    }

    /// 稳定英文摘要。
    pub fn summary(&self) -> String {
        format!(
            "rollback plan to #{}: {} steps ({} executable, {} not actionable) over {} records",
            self.to_id,
            self.len(),
            self.actionable(),
            self.not_actionable(),
            self.source_records
        )
    }
}

/// 生成"回滚到 `to_id`（含）"的逆序计划。
///
/// 步骤覆盖 `id <= to_id` 且 [`JournalKind::is_reversible`] 的全部记录（`apply` / `imported`），
/// 按 `id` 降序。`to_id` 必须是链上真实存在的 id，否则返回
/// [`crate::JournalErrorKind::InvalidArgument`]。
pub fn plan_rollback(records: &[JournalRecord], to_id: u64) -> JournalResult<RollbackPlan> {
    if !records.is_empty() && !records.iter().any(|record| record.id == to_id) {
        return Err(JournalError::invalid_argument(
            "plan_rollback",
            format!("the journal has no record with id {to_id}"),
        ));
    }
    let source_records = records.iter().filter(|record| record.id <= to_id).count();
    let mut steps: Vec<RollbackStep> = records
        .iter()
        .filter(|record| record.id <= to_id && record.kind.is_reversible())
        .map(RollbackStep::from_record)
        .collect();
    // 显式按 id 降序排序：不依赖输入顺序（调用方可能传入未排序的子集）。
    steps.sort_by_key(|step| std::cmp::Reverse(step.journal_id));
    Ok(RollbackPlan {
        to_id,
        source_records,
        steps,
    })
}

/// 生成"撤销全部可逆记录"的逆序计划（空日志 ⇒ 空计划，不是错误）。
pub fn plan_rollback_all(records: &[JournalRecord]) -> JournalResult<RollbackPlan> {
    match records.iter().map(|record| record.id).max() {
        Some(to_id) => plan_rollback(records, to_id),
        None => Ok(RollbackPlan {
            to_id: 0,
            source_records: 0,
            steps: Vec::new(),
        }),
    }
}

/// 已经被回滚记录覆盖过的 apply id。
///
/// 约定：`kind = rollback` 的记录把 `rule_id` 写成 `rollback:<apply_id>`
/// （见 [`ROLLBACK_RULE_PREFIX`]）；无法解析的记录忽略。返回升序去重列表。
pub fn undone_apply_ids(records: &[JournalRecord]) -> Vec<u64> {
    let mut ids: Vec<u64> = records
        .iter()
        .filter(|record| record.kind == JournalKind::Rollback)
        .filter_map(|record| record.rule_id.as_deref())
        .filter_map(|rule_id| rule_id.strip_prefix(ROLLBACK_RULE_PREFIX))
        .filter_map(|text| text.trim().parse::<u64>().ok())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// 生成"仍然待撤销"的逆序计划：跳过 [`undone_apply_ids`] 里已撤销的 apply。
pub fn plan_rollback_pending(records: &[JournalRecord]) -> JournalResult<RollbackPlan> {
    let undone = undone_apply_ids(records);
    let mut plan = plan_rollback_all(records)?;
    if !undone.is_empty() {
        plan.steps.retain(|step| !undone.contains(&step.journal_id));
        plan.source_records = plan.steps.len();
    }
    Ok(plan)
}

/// 按 `before` 载荷的**形状**派发到对应解码器（形状优先于 `target`，避免 target 写错时张冠李戴）。
fn decode_action(before: &Value) -> (RollbackAction, Option<String>) {
    let name = before
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    // 1) 旧格式导入：带 `legacy` 标记 ⇒ 一次恢复多项，不能拆成单域动作。
    if let Some(legacy) = before.get("legacy") {
        let process_id = payload::decode_pid(before).unwrap_or(0);
        let priority = before
            .get("priority")
            .and_then(|value| payload::decode_priority(value).ok());
        let affinity_mask = before
            .get("affinity_mask")
            .and_then(payload::as_u64_lenient);
        let working_set = payload::decode_working_set(before)
            .ok()
            .map(|(_, limits)| limits);
        let power_scheme_guid = before
            .get("guid")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let source = legacy
            .get("source")
            .and_then(|value| value.as_str())
            .unwrap_or("legacy file");
        let legacy_note = legacy.get("note").and_then(|value| value.as_str());
        if priority.is_none()
            && affinity_mask.is_none()
            && working_set.is_none()
            && power_scheme_guid.is_none()
        {
            // 旧条目里没有任何可恢复字段（例如 `games.conf` 的启动偏好）：
            // 显式标成不可执行，而不是生成一个空动作。
            let reason = legacy_note
                .map(str::to_string)
                .unwrap_or_else(|| format!("the {source} entry carries no restorable state"));
            return (
                RollbackAction::NotActionable { reason },
                Some(format!("imported from {source}")),
            );
        }
        let note = legacy_note.map(str::to_string).unwrap_or_else(|| {
            format!(
                "imported from {source}: the legacy file records the state before the C++ \
                 optimization, not which entries were actually applied — confirm before restoring"
            )
        });
        return (
            RollbackAction::RestoreLegacySnapshot {
                process_id,
                priority,
                affinity_mask,
                working_set,
                power_scheme_guid,
            },
            Some(note),
        );
    }

    // 2) 进程优先级。
    if before.get("priority").is_some() {
        return match payload::decode_priority(before) {
            Ok(priority) => {
                let pid = payload::decode_pid(before).unwrap_or(0);
                (
                    RollbackAction::RestorePriority {
                        pid,
                        name,
                        priority,
                    },
                    None,
                )
            }
            Err(reason) => (RollbackAction::NotActionable { reason }, None),
        };
    }

    // 3) 亲和性。
    if before.get("plan").is_some()
        || before.get("affinity_plan").is_some()
        || before.get("mask").is_some()
        || before.get("affinity_mask").is_some()
    {
        return match payload::decode_affinity(before) {
            Ok((pid, plan)) => (RollbackAction::RestoreAffinity { pid, name, plan }, None),
            Err(reason) => (RollbackAction::NotActionable { reason }, None),
        };
    }

    // 4) 工作集。
    if before.get("min_bytes").is_some() || before.get("working_set_min").is_some() {
        return match payload::decode_working_set(before) {
            Ok((pid, limits)) => (
                RollbackAction::RestoreWorkingSet { pid, name, limits },
                None,
            ),
            Err(reason) => (RollbackAction::NotActionable { reason }, None),
        };
    }

    // 5) 电源方案。
    if before.get("guid").is_some() {
        return match payload::decode_power_scheme(before) {
            Ok((guid, name)) => (RollbackAction::RestorePowerScheme { guid, name }, None),
            Err(reason) => (RollbackAction::NotActionable { reason }, None),
        };
    }

    // 6) 开机启动项。
    if before.get("hive").is_some() {
        return match payload::decode_run_entry(before) {
            Ok((hive, value_name, enabled)) => (
                RollbackAction::RestoreRunEntry {
                    hive,
                    value_name,
                    enabled,
                },
                None,
            ),
            Err(reason) => (RollbackAction::NotActionable { reason }, None),
        };
    }

    // 7) 裸字符串：GUID 按电源方案处理，其余按优先级处理（两者都是"值即状态"的域）。
    if let Some(text) = before.as_str() {
        if text.parse::<Guid>().is_ok() {
            return match payload::decode_power_scheme(before) {
                Ok((guid, name)) => (RollbackAction::RestorePowerScheme { guid, name }, None),
                Err(reason) => (RollbackAction::NotActionable { reason }, None),
            };
        }
        return match payload::decode_priority(before) {
            Ok(priority) => (
                RollbackAction::RestorePriority {
                    pid: 0,
                    name: None,
                    priority,
                },
                Some("the payload carries a bare priority without a pid".to_string()),
            ),
            Err(reason) => (RollbackAction::NotActionable { reason }, None),
        };
    }

    (
        RollbackAction::NotActionable {
            reason: format!("unrecognised `before` payload shape: {}", brief(before)),
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::GENESIS_HASH;
    use crate::payload as pl;
    use crate::record::JournalDraft;
    use gopt_hal::RunEntry;
    use serde_json::json;

    fn apply(kind: JournalKind, target: &str, before: Value) -> JournalDraft {
        JournalDraft::at(1_700_000_000_000, kind, target).with_before(before)
    }

    /// 构造一条 4 环 32 逻辑核、保留最后 4 核的亲和性计划。
    fn sample_plan() -> AffinityPlan {
        AffinityPlan::reserve_last_n_cores(32, 4).expect("plan")
    }

    fn chain() -> Vec<JournalRecord> {
        let drafts = vec![
            apply(
                JournalKind::Apply,
                &pl::pid_target(4242),
                pl::priority(4242, Some("cs2.exe"), PriorityClass::Normal),
            )
            .with_after(pl::priority(4242, Some("cs2.exe"), PriorityClass::High)),
            apply(
                JournalKind::Apply,
                &pl::pid_target(4242),
                pl::affinity(4242, Some("cs2.exe"), &sample_plan()),
            ),
            apply(
                JournalKind::Apply,
                &pl::pid_target(4242),
                pl::working_set(
                    4242,
                    None,
                    WorkingSetLimits::from_mb(512, 2048).expect("limits"),
                ),
            ),
            apply(
                JournalKind::Apply,
                pl::POWER_TARGET,
                pl::power_scheme(&gopt_hal::PowerScheme::new(
                    gopt_hal::Guid::BALANCED,
                    "Balanced",
                )),
            ),
            apply(
                JournalKind::Apply,
                &pl::run_target(RunHive::CurrentUser, "Steam"),
                pl::run_entry(&RunEntry::from_registry(
                    RunHive::CurrentUser,
                    "Steam",
                    r"C:\steam.exe",
                    false,
                )),
            ),
            // 无 before：不可执行（显式列出而不是静默跳过）。
            JournalDraft::at(1_700_000_000_000, JournalKind::Apply, "pid:7"),
            // 回滚记录：不进计划。
            JournalDraft::at(1_700_000_000_000, JournalKind::Rollback, "pid:4242")
                .with_rule_id("rollback:1"),
        ];
        let mut records = Vec::new();
        let mut prev = GENESIS_HASH.to_string();
        for (index, draft) in drafts.into_iter().enumerate() {
            let record = draft.into_record(index as u64 + 1, prev);
            prev = record.hash.clone();
            records.push(record);
        }
        records
    }

    #[test]
    fn plan_is_reverse_ordered_and_typed() {
        let records = chain();
        let plan = plan_rollback(&records, 5).expect("plan");
        assert_eq!(plan.to_id, 5);
        assert_eq!(plan.source_records, 5);
        assert_eq!(plan.len(), 5);
        // 逆序：最新在前。
        let ids: Vec<u64> = plan.steps.iter().map(|step| step.journal_id).collect();
        assert_eq!(ids, vec![5, 4, 3, 2, 1]);
        assert_eq!(plan.not_actionable(), 0);

        assert_eq!(
            plan.steps[0].action,
            RollbackAction::RestoreRunEntry {
                hive: RunHive::CurrentUser,
                value_name: "Steam".to_string(),
                enabled: true,
            }
        );
        assert_eq!(
            plan.steps[1].action,
            RollbackAction::RestorePowerScheme {
                guid: gopt_hal::Guid::BALANCED,
                name: Some("Balanced".to_string()),
            }
        );
        assert_eq!(
            plan.steps[2].action,
            RollbackAction::RestoreWorkingSet {
                pid: 4242,
                name: None,
                limits: WorkingSetLimits::from_mb(512, 2048).expect("limits"),
            }
        );
        assert_eq!(
            plan.steps[3].action,
            RollbackAction::RestoreAffinity {
                pid: 4242,
                name: Some("cs2.exe".to_string()),
                plan: sample_plan(),
            }
        );
        assert_eq!(
            plan.steps[4].action,
            RollbackAction::RestorePriority {
                pid: 4242,
                name: Some("cs2.exe".to_string()),
                priority: PriorityClass::Normal,
            }
        );
    }

    #[test]
    fn plan_skips_rollback_records_and_later_applies() {
        let records = chain();
        let plan = plan_rollback(&records, 5).expect("plan");
        assert!(plan.steps.iter().all(|step| step.journal_id != 7));
        assert!(plan
            .steps
            .iter()
            .all(|step| step.kind != JournalKind::Rollback));

        let partial = plan_rollback(&records, 2).expect("partial plan");
        assert_eq!(partial.len(), 2);
        assert_eq!(partial.source_records, 2);
        assert_eq!(partial.steps[0].journal_id, 2);

        assert!(plan_rollback(&records, 99).is_err());
    }

    #[test]
    fn missing_before_becomes_not_actionable() {
        let records = chain();
        let plan = plan_rollback_all(&records).expect("plan");
        // #6（无 before）与 #7（rollback 记录）之外的 5 条都在计划里；#6 保留为 NotActionable。
        assert_eq!(plan.steps.len(), 6);
        let step = plan
            .steps
            .iter()
            .find(|step| step.journal_id == 6)
            .expect("step 6");
        assert!(!step.action.is_executable());
        assert!(step.note.is_some());
        assert_eq!(plan.not_actionable(), 1);
        assert_eq!(plan.actionable(), 5);
        assert_eq!(plan.executable_steps().count(), 5);
        assert!(plan.summary().contains("5 executable"));
        assert!(step.describe().contains("not actionable"));
    }

    #[test]
    fn realtime_before_value_is_rejected_not_restored() {
        let draft = apply(
            JournalKind::Apply,
            &pl::pid_target(1),
            json!({"pid": 1, "priority": 0x100}),
        );
        let record = draft.into_record(1, GENESIS_HASH);
        let step = RollbackStep::from_record(&record);
        match step.action {
            RollbackAction::NotActionable { reason } => assert!(reason.contains("rejected")),
            other => panic!("REALTIME must not become a restore step: {other:?}"),
        }
    }

    #[test]
    fn garbage_and_bare_values_are_handled() {
        let unknown = apply(JournalKind::Apply, "pid:1", json!({"something": "else"}))
            .into_record(1, GENESIS_HASH);
        assert!(!RollbackStep::from_record(&unknown).action.is_executable());

        let bare =
            apply(JournalKind::Apply, "pid:1", json!("above-normal")).into_record(2, GENESIS_HASH);
        assert_eq!(
            RollbackStep::from_record(&bare).action,
            RollbackAction::RestorePriority {
                pid: 0,
                name: None,
                priority: PriorityClass::AboveNormal,
            }
        );

        let bare_guid = apply(
            JournalKind::Apply,
            pl::POWER_TARGET,
            json!("381b4222-f694-41f0-9685-ff5bb260df2e"),
        )
        .into_record(3, GENESIS_HASH);
        assert_eq!(
            RollbackStep::from_record(&bare_guid).action,
            RollbackAction::RestorePowerScheme {
                guid: gopt_hal::Guid::BALANCED,
                name: None,
            }
        );

        let bad_limits = apply(
            JournalKind::Apply,
            "pid:1",
            json!({"pid": 1, "min_bytes": 0, "max_bytes": 10}),
        )
        .into_record(4, GENESIS_HASH);
        assert!(!RollbackStep::from_record(&bad_limits)
            .action
            .is_executable());
    }

    #[test]
    fn legacy_snapshot_is_one_step_with_note() {
        let before = json!({
            "pid": 4242,
            "priority": "normal",
            "affinity_mask": "0x000000000000000f",
            "min_bytes": 536870912,
            "max_bytes": 2147483648_u64,
            "guid": "381b4222-f694-41f0-9685-ff5bb260df2e",
            "legacy": {"source": "savepoints.txt", "index": 1},
        });
        let record = apply(JournalKind::Imported, &pl::pid_target(4242), before)
            .into_record(1, GENESIS_HASH);
        let step = RollbackStep::from_record(&record);
        assert_eq!(
            step.action,
            RollbackAction::RestoreLegacySnapshot {
                process_id: 4242,
                priority: Some(PriorityClass::Normal),
                affinity_mask: Some(0xf),
                working_set: Some(WorkingSetLimits::from_mb(512, 2048).expect("limits")),
                power_scheme_guid: Some("381b4222-f694-41f0-9685-ff5bb260df2e".to_string()),
            }
        );
        let note = step.note.expect("note");
        assert!(note.contains("savepoints.txt"));
        assert!(step.action.describe().contains("0x000000000000000f"));
    }

    #[test]
    fn undone_ids_and_pending_plan() {
        let records = chain();
        assert_eq!(undone_apply_ids(&records), vec![1]);

        let pending = plan_rollback_pending(&records).expect("pending plan");
        assert!(pending.steps.iter().all(|step| step.journal_id != 1));
        assert_eq!(pending.len(), 5);

        let plain = plan_rollback_all(&records).expect("plan");
        assert_eq!(plain.len(), 6);
        assert!(plain.steps.iter().any(|step| step.journal_id == 1));
    }

    #[test]
    fn empty_journal_plans_are_empty_not_errors() {
        let plan = plan_rollback_all(&[]).expect("empty plan");
        assert!(plan.is_empty());
        assert_eq!(plan.to_id, 0);
        assert!(plan.summary().contains("0 steps"));
        assert!(plan_rollback(&[], 1).is_ok());
    }
}
