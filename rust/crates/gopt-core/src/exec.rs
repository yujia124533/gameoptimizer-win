//! 步骤执行原语：一个 [`PlanStep`] → 一组 `gopt-hal::SystemApi` 调用 + 一条审计记录。
//!
//! # 为什么把"预演"与"执行"放在同一个函数里
//!
//! 预演（`gopt plan` / `gopt apply` 不带 `--yes`）必须**看见**真实的前后值才有意义，
//! 因此预演与执行的差别只是"最后那一次写调用要不要发"：两条路径共用同一段读逻辑，
//! 不会出现"预演说的和真做的不一样"。
//!
//! # 幂等
//!
//! 每个动作先读当前状态（能读的话）：已经是目标值 ⇒ 不写系统、不写日志，如实报 `unchanged`。
//! 于是重复执行同一份计划不会污染审计链，也不会给系统制造无意义的写入。
//!
//! # 哪些步骤不可回滚
//!
//! 审计记录的 `before` 就是回滚依据。每个动作在写之前都会尽量把当前状态读出来：
//! 优先级 / 亲和性 / 工作集三条都有官方读路径（工作集用
//! [`gopt_hal::SystemApi::get_working_set`]），因此它们的前值是**真值**、可回滚。
//! 只有"读不到"（进程受保护 / 已退出）或"读到的值无法写回"（工作集 `min = 0`，
//! HAL 写路径不接受）两种情况才会退回 `before = null`，此时回滚计划把它报成
//! `NotActionable` 并说明原因 —— 宁可显式说明做不到，也不写一条假的可回滚记录。
//!
//! [`PlanStep`]: gopt_policy::PlanStep

use gopt_hal::{
    AffinityPlan, PowerSchemeSelector, PriorityClass, RunEntry, RunHive, SystemApi,
    WorkingSetLimits,
};
use gopt_journal::{payload, JournalDraft, JournalKind};
use gopt_policy::{Plan, PlanAction, PlanStep};
use serde_json::Value;

use crate::error::{CoreError, CoreResult};
use crate::i18n::Text;

/// 一次步骤执行（或预演）的实际效果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StepEffect {
    /// 写入前状态载荷（`None` = 不可知 ⇒ 该步骤不可回滚）。
    pub before: Option<Value>,
    /// 写入后状态载荷（预演时是"将会写入的值"）。
    pub after: Option<Value>,
    /// 是否真的改变了状态（`false` = 已经是目标值 / 目标不存在）。
    pub changed: bool,
    /// 目标不存在且策略允许忽略。
    pub missing: bool,
    /// 附加说明。
    pub note: Option<Text>,
}

impl StepEffect {
    fn unchanged(before: Option<Value>, after: Option<Value>) -> Self {
        Self {
            before,
            after,
            changed: false,
            missing: false,
            note: None,
        }
    }

    fn preview(before: Option<Value>, after: Option<Value>, note: Option<Text>) -> Self {
        Self {
            before,
            after,
            changed: true,
            missing: false,
            note,
        }
    }

    fn missing(note: Text) -> Self {
        Self {
            before: None,
            after: None,
            changed: false,
            missing: true,
            note: Some(note),
        }
    }

    /// 该步骤是否可回滚（写入前状态可知）。
    pub fn reversible(&self) -> bool {
        self.before.is_some()
    }
}

/// 执行（或预演）一个步骤。
///
/// * `process_name`：目标进程名（仅用于审计载荷里的 `name` 字段，可传 `None`）；
/// * `dry_run`：`true` 时只读不写。
pub(crate) fn execute(
    api: &dyn SystemApi,
    step: &PlanStep,
    process_name: Option<&str>,
    dry_run: bool,
) -> CoreResult<StepEffect> {
    match &step.action {
        PlanAction::Priority { class } => priority(api, step.pid, process_name, *class, dry_run),
        PlanAction::Affinity { plan, .. } => affinity(api, step, process_name, plan, dry_run),
        PlanAction::WorkingSet { limits } => {
            working_set(api, step.pid, process_name, *limits, dry_run)
        }
        PlanAction::PowerScheme { selector, .. } => power_scheme(api, selector, dry_run),
        PlanAction::RunEntry {
            hive,
            name,
            enabled,
            ignore_missing,
        } => run_entry(api, *hive, name, *enabled, *ignore_missing, dry_run),
    }
}

/// 进程优先级：`get_priority` 读当前值 → `set_priority`（返回写入前的值）。
fn priority(
    api: &dyn SystemApi,
    pid: u32,
    name: Option<&str>,
    class: PriorityClass,
    dry_run: bool,
) -> CoreResult<StepEffect> {
    let current = api.get_priority(pid).map_err(|error| {
        CoreError::from_hal(format!("cannot read the priority of pid {pid}"), error)
    })?;

    if current == class {
        return Ok(StepEffect::unchanged(
            Some(payload::priority(pid, name, current)),
            Some(payload::priority(pid, name, class)),
        ));
    }
    if dry_run {
        return Ok(StepEffect::preview(
            Some(payload::priority(pid, name, current)),
            Some(payload::priority(pid, name, class)),
            None,
        ));
    }

    let previous = api.set_priority(pid, class).map_err(|error| {
        CoreError::from_hal(format!("cannot set the priority of pid {pid}"), error)
    })?;
    // 读回一次：审计记录里的 `after` 应当是**系统上真实的**值，而不是我们请求的值。
    let (after, note) = match api.get_priority(pid) {
        Ok(read_back) if read_back == class => (class, None),
        Ok(read_back) => (
            read_back,
            Some(Text::new(
                format!(
                    "读回不一致：请求 {}，系统报告 {}（可能被其它进程改回去了）",
                    class.as_str(),
                    read_back.as_str()
                ),
                format!(
                    "read-back mismatch: requested {}, the system reports {}",
                    class.as_str(),
                    read_back.as_str()
                ),
            )),
        ),
        Err(error) => (
            class,
            Some(Text::new(
                format!("读回失败（{}），after 记录为请求值", error.kind().as_str()),
                format!(
                    "read-back failed ({}); `after` records the requested value",
                    error.kind().as_str()
                ),
            )),
        ),
    };
    Ok(StepEffect {
        before: Some(payload::priority(pid, name, previous)),
        after: Some(payload::priority(pid, name, after)),
        changed: true,
        missing: false,
        note,
    })
}

/// CPU 亲和性：`get_affinity` 读当前值 → 逐组 `set_affinity`（跨处理器组时按 `per_group` 拆分）。
fn affinity(
    api: &dyn SystemApi,
    step: &PlanStep,
    name: Option<&str>,
    plan: &AffinityPlan,
    dry_run: bool,
) -> CoreResult<StepEffect> {
    let pid = step.pid;
    let info = api.get_affinity(pid).map_err(|error| {
        CoreError::from_hal(format!("cannot read the affinity of pid {pid}"), error)
    })?;
    // 当前亲和性 → 计划形状（掩码非 0 才可表达；读取失败时留空表示不可回滚）。
    let current = AffinityPlan::single(info.total_logical, info.group, info.process_mask).ok();
    let before = current
        .as_ref()
        .map(|plan| payload::affinity(pid, name, plan));
    let after = payload::affinity(pid, name, plan);

    let target_mask = mask_for_group(plan, info.group);
    if plan.is_single_group() && target_mask == Some(info.process_mask) {
        return Ok(StepEffect::unchanged(before, Some(after)));
    }
    if dry_run {
        return Ok(StepEffect::preview(before, Some(after), None));
    }

    let batches = step.affinity_batches();
    if batches.is_empty() {
        return Err(CoreError::unsupported(
            "SetProcessAffinityMask",
            "the affinity plan contains no processor group to apply",
        ));
    }
    let mut threads_updated = 0u32;
    for batch in &batches {
        let applied = api.set_affinity(pid, batch).map_err(|error| {
            CoreError::from_hal(
                format!(
                    "cannot bind pid {pid} to processor group {:?}",
                    batch.primary_group()
                ),
                error,
            )
        })?;
        threads_updated += applied.threads_updated;
    }
    let note = if batches.len() > 1 {
        Some(Text::new(
            format!(
                "跨处理器组：按 {} 组逐组应用（逐线程设置 {} 次）",
                batches.len(),
                threads_updated
            ),
            format!(
                "cross-group affinity: applied per processor group ({} groups, {threads_updated} thread updates)",
                batches.len()
            ),
        ))
    } else if threads_updated > 0 {
        Some(Text::new(
            format!("非主组：逐线程设置亲和性（{threads_updated} 个线程）"),
            format!("non-primary group: set affinity on {threads_updated} threads"),
        ))
    } else {
        None
    };

    Ok(StepEffect {
        before,
        after: Some(after),
        changed: true,
        missing: false,
        note,
    })
}

/// 工作集：`get_working_set` 读当前值 → `set_working_set` → 读回。
///
/// 读到的前值是**系统真值**（官方 `GetProcessWorkingSetSize`），所以这一步可回滚。
/// 只有两种情况会退回 `before = None`（不可回滚），且都会把原因写进 `note`：
///
/// * 读取失败（例如目标进程受保护、已退出）；
/// * 系统报告 `min = 0` —— HAL 的写路径不接受 `min = 0`，这个前值写不回去。
///
/// 两种情况都**不记录假的 `before`**：宁可显式说明做不到，也不让回滚在运行时才发现还原不了。
fn working_set(
    api: &dyn SystemApi,
    pid: u32,
    name: Option<&str>,
    limits: WorkingSetLimits,
    dry_run: bool,
) -> CoreResult<StepEffect> {
    let limits = limits.normalized();
    let after = payload::working_set(pid, name, limits);

    let (before, current, note) = match api.get_working_set(pid) {
        Ok(current) if current.is_restorable() => (
            Some(payload::working_set(pid, name, current)),
            Some(current),
            None,
        ),
        Ok(current) => (
            None,
            Some(current),
            Some(Text::new(
                format!(
                    "系统报告最小工作集为 0（min {} / max {} 字节）：HAL 写路径不接受 min=0，\
                     该前值写不回去 ⇒ 审计记录 before=null、该步骤不可回滚",
                    current.min_bytes, current.max_bytes
                ),
                format!(
                    "the system reports a zero minimum working set (min {} / max {} bytes); the HAL \
                     write path rejects min=0, so this previous value cannot be restored: \
                     before=null and the step is not reversible",
                    current.min_bytes, current.max_bytes
                ),
            )),
        ),
        Err(error) => (
            None,
            None,
            Some(Text::new(
                format!(
                    "读取写入前的工作集失败（{}）：前值不可知 ⇒ 审计记录 before=null、该步骤不可回滚",
                    error.kind().as_str()
                ),
                format!(
                    "cannot read the working-set limits before writing ({}): the previous value is \
                     unknown, so before=null and the step is not reversible",
                    error.kind().as_str()
                ),
            )),
        ),
    };

    // 幂等：系统当前值就是目标值 ⇒ 不写、不记。
    if current == Some(limits) {
        return Ok(StepEffect::unchanged(before, Some(after)));
    }

    if dry_run {
        return Ok(StepEffect::preview(before, Some(after), note));
    }
    api.set_working_set(pid, limits).map_err(|error| {
        CoreError::from_hal(format!("cannot set the working set of pid {pid}"), error)
    })?;

    // 读回一次：审计记录里的 `after` 应当是**系统上真实的**值，而不是我们请求的值。
    let (after, read_back_note) = match api.get_working_set(pid) {
        Ok(read_back) if read_back == limits => (after, None),
        Ok(read_back) => (
            payload::working_set(pid, name, read_back),
            Some(Text::new(
                format!(
                    "读回不一致：请求 min {} / max {} 字节，系统报告 min {} / max {} 字节",
                    limits.min_bytes, limits.max_bytes, read_back.min_bytes, read_back.max_bytes
                ),
                format!(
                    "read-back mismatch: requested min {} / max {} bytes, the system reports \
                     min {} / max {} bytes",
                    limits.min_bytes, limits.max_bytes, read_back.min_bytes, read_back.max_bytes
                ),
            )),
        ),
        Err(error) => (
            after,
            Some(Text::new(
                format!("读回失败（{}），after 记录为请求值", error.kind().as_str()),
                format!(
                    "read-back failed ({}); `after` records the requested value",
                    error.kind().as_str()
                ),
            )),
        ),
    };

    Ok(StepEffect {
        before,
        after: Some(after),
        changed: true,
        missing: false,
        note: read_back_note.or(note),
    })
}

/// 电源方案：`query_power_scheme` 读当前值 → `set_power_scheme`（返回前后方案）。
fn power_scheme(
    api: &dyn SystemApi,
    selector: &PowerSchemeSelector,
    dry_run: bool,
) -> CoreResult<StepEffect> {
    let current = api
        .query_power_scheme()
        .map_err(|error| CoreError::from_hal("cannot query the active power scheme", error))?;
    let before = payload::power_scheme(&current);
    let already = match selector {
        PowerSchemeSelector::HighPerformance => current.is_high_performance,
        PowerSchemeSelector::Explicit(guid) => current.guid == *guid,
    };
    if already {
        return Ok(StepEffect::unchanged(Some(before.clone()), Some(before)));
    }
    if dry_run {
        let note = Text::new(
            "预演：目标方案的实际 GUID/名称由系统在切换时解析，预演阶段不可知",
            "dry-run: the target scheme's GUID/name is resolved by the system on switch, so it is unknown here",
        );
        return Ok(StepEffect::preview(Some(before), None, Some(note)));
    }
    let change = api
        .set_power_scheme(selector)
        .map_err(|error| CoreError::from_hal("cannot switch the active power scheme", error))?;
    Ok(StepEffect {
        before: Some(payload::power_scheme(&change.previous)),
        after: Some(payload::power_scheme(&change.current)),
        changed: true,
        missing: false,
        note: None,
    })
}

/// 开机启动项：`list_run_entries` 定位当前条目 → `set_run_entry_enabled`（改名迁移，可逆）。
fn run_entry(
    api: &dyn SystemApi,
    hive: RunHive,
    name: &str,
    enabled: bool,
    ignore_missing: bool,
    dry_run: bool,
) -> CoreResult<StepEffect> {
    let entries = api
        .list_run_entries()
        .map_err(|error| CoreError::from_hal("cannot list the Run key entries", error))?;
    let Some(current) = entries
        .iter()
        .find(|entry| entry.hive == hive && entry.name == name)
        .cloned()
    else {
        if ignore_missing {
            return Ok(StepEffect::missing(Text::new(
                format!("启动项 {hive}:{name} 不存在，策略要求忽略 ⇒ 跳过（未写入）"),
                format!("the run entry {hive}:{name} does not exist and the policy ignores missing entries"),
            )));
        }
        return Err(CoreError::not_found(
            "RegOpenKeyExW",
            format!("the run entry {hive}:{name} does not exist"),
        ));
    };

    let expected = expected_entry(&current, enabled);
    let before = payload::run_entry(&current);
    if current.enabled == enabled {
        return Ok(StepEffect::unchanged(
            Some(before),
            Some(payload::run_entry(&expected)),
        ));
    }
    if dry_run {
        return Ok(StepEffect::preview(
            Some(before),
            Some(payload::run_entry(&expected)),
            None,
        ));
    }
    let updated = api
        .set_run_entry_enabled(hive, &current.name, enabled)
        .map_err(|error| {
            CoreError::from_hal(
                format!(
                    "cannot {} the run entry {hive}:{name}",
                    if enabled { "enable" } else { "disable" }
                ),
                error,
            )
        })?;
    Ok(StepEffect {
        before: Some(before),
        after: Some(payload::run_entry(&updated)),
        changed: true,
        missing: false,
        note: None,
    })
}

/// 预期中的"启用/禁用之后"的条目（预演输出用，与 HAL 的改名规则逐字节一致）。
fn expected_entry(current: &RunEntry, enabled: bool) -> RunEntry {
    let value_name = if enabled {
        current.name.clone()
    } else {
        RunEntry::disabled_value_name(&current.name)
    };
    RunEntry::from_registry(
        current.hive,
        value_name,
        current.command.clone(),
        current.expandable,
    )
}

/// 计划里针对某个处理器组的掩码。
pub(crate) fn mask_for_group(plan: &AffinityPlan, group: u32) -> Option<u64> {
    plan.requests()
        .iter()
        .find(|request| request.group() == group)
        .map(|request| request.mask())
}

/// 审计记录的作用对象（与 `gopt-journal` 的 target 约定一致）。
pub(crate) fn target_of(step: &PlanStep) -> String {
    match &step.action {
        PlanAction::Priority { .. }
        | PlanAction::Affinity { .. }
        | PlanAction::WorkingSet { .. } => payload::pid_target(step.pid),
        PlanAction::PowerScheme { .. } => payload::POWER_TARGET.to_string(),
        PlanAction::RunEntry { hive, name, .. } => payload::run_target(*hive, name),
    }
}

/// 审计记录的 `rule_id`：`policy:game/<游戏 id>/<规则 id>`（可一路追到 TOML 里的那一行）。
pub(crate) fn rule_id_of(plan: &Plan, step: &PlanStep) -> String {
    format!("policy:game/{}/{}", plan.game_id, step.rule_id)
}

/// 由一个步骤的效果生成 `kind = apply` 的审计草稿。
pub(crate) fn draft_for(
    plan: &Plan,
    step: &PlanStep,
    before: Option<Value>,
    after: Option<Value>,
) -> JournalDraft {
    JournalDraft::now(JournalKind::Apply, target_of(step))
        .with_values(before, after)
        .with_rule_id(rule_id_of(plan, step))
}

/// 由一条撤销动作生成 `kind = rollback` 的审计草稿。
pub(crate) fn rollback_draft(
    target: &str,
    apply_id: u64,
    before: Option<Value>,
    after: Option<Value>,
) -> JournalDraft {
    JournalDraft::now(JournalKind::Rollback, target.to_string())
        .with_values(before, after)
        .with_rule_id(format!("{}{apply_id}", gopt_journal::ROLLBACK_RULE_PREFIX))
}

/// 进程名回填：把 `list_processes` 的结果做成 `pid -> name` 的查询函数。
pub(crate) fn process_name(processes: &[gopt_hal::ProcessInfo], pid: u32) -> Option<String> {
    processes
        .iter()
        .find(|process| process.pid == pid)
        .map(|process| process.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gopt_hal::MockApi;
    use gopt_policy::{PlanStep, Reason};

    fn step(action: PlanAction) -> PlanStep {
        PlanStep {
            order: 1,
            rule_id: "rule".to_string(),
            rule_line: Some(3),
            pid: 1234,
            reason: Reason::new("理由", "reason"),
            action,
            requires_elevation: false,
            is_dangerous: false,
        }
    }

    #[test]
    fn priority_preview_reads_and_does_not_write() {
        let api = MockApi::sample_workstation();
        let step = step(PlanAction::Priority {
            class: PriorityClass::High,
        });
        let effect = execute(&api, &step, Some("cs2.exe"), true).expect("preview");
        assert!(effect.changed);
        assert_eq!(
            effect.before.as_ref().expect("before")["priority"],
            "normal"
        );
        assert_eq!(effect.after.as_ref().expect("after")["priority"], "high");
        assert_eq!(api.call_count(gopt_hal::HalOp::SetPriority), 0);
        assert_eq!(api.priority_of(1234), Some(PriorityClass::Normal));
    }

    #[test]
    fn priority_apply_writes_and_reports_previous_value() {
        let api = MockApi::sample_workstation();
        let step = step(PlanAction::Priority {
            class: PriorityClass::High,
        });
        let effect = execute(&api, &step, Some("cs2.exe"), false).expect("apply");
        assert_eq!(
            effect.before.as_ref().expect("before")["priority"],
            "normal"
        );
        assert_eq!(effect.after.as_ref().expect("after")["priority"], "high");
        assert!(effect.reversible());
        assert_eq!(api.priority_of(1234), Some(PriorityClass::High));

        // 幂等：再执行一次不会写系统。
        let again = execute(&api, &step, Some("cs2.exe"), false).expect("second apply");
        assert!(!again.changed);
        assert_eq!(api.call_count(gopt_hal::HalOp::SetPriority), 1);
    }

    #[test]
    fn working_set_records_the_real_before_and_is_reversible() {
        let api = MockApi::sample_workstation();
        let before_limits = WorkingSetLimits::from_mb(200, 0).expect("limits");
        api.seed_working_set(1234, before_limits);

        let step = step(PlanAction::WorkingSet {
            limits: WorkingSetLimits::from_mb(256, 0).expect("limits"),
        });
        let effect = execute(&api, &step, Some("cs2.exe"), false).expect("apply");
        assert!(effect.changed);
        assert!(effect.reversible(), "the previous limits are known now");
        assert_eq!(
            effect.before.as_ref().expect("before")["min_bytes"],
            before_limits.min_bytes
        );
        assert_eq!(
            effect.after.as_ref().expect("after")["min_bytes"],
            256 * 1024 * 1024
        );
        assert_eq!(api.call_count(gopt_hal::HalOp::SetWorkingSet), 1);
        assert_eq!(
            api.call_count(gopt_hal::HalOp::GetWorkingSet),
            2,
            "read + read-back"
        );
    }

    #[test]
    fn working_set_without_a_readable_before_stays_honest() {
        // (a) 读失败：before=null、不可回滚，但写入照常进行。
        let api = MockApi::sample_workstation();
        api.fail_next(
            gopt_hal::HalOp::GetWorkingSet,
            gopt_hal::HalError::access_denied("GetProcessWorkingSetSize", "denied by the test"),
        );
        let step = step(PlanAction::WorkingSet {
            limits: WorkingSetLimits::from_mb(256, 0).expect("limits"),
        });
        let effect = execute(&api, &step, Some("cs2.exe"), false).expect("apply");
        assert!(effect.changed);
        assert!(
            effect.before.is_none(),
            "an unreadable before is not invented"
        );
        assert!(!effect.reversible());
        let note = effect.note.as_ref().expect("the boundary is explained");
        assert!(note.zh.contains("读取写入前的工作集失败"), "{}", note.zh);

        // (b) 系统报告 min=0：这个前值写不回去 ⇒ 同样如实报"不可回滚"。
        let api = MockApi::sample_workstation();
        api.seed_working_set(
            1234,
            WorkingSetLimits::observed(0, 1_413_120).expect("observed"),
        );
        let effect = execute(&api, &step, Some("cs2.exe"), false).expect("apply");
        assert!(effect.before.is_none());
        assert!(!effect.reversible());
        let note = effect.note.as_ref().expect("the boundary is explained");
        assert!(note.zh.contains("min=0"), "{}", note.zh);
    }

    #[test]
    fn working_set_is_idempotent_once_the_target_is_reached() {
        let api = MockApi::sample_workstation();
        let target = WorkingSetLimits::from_mb(256, 0).expect("limits");
        api.seed_working_set(1234, target);
        let step = step(PlanAction::WorkingSet { limits: target });

        let effect = execute(&api, &step, Some("cs2.exe"), false).expect("apply");
        assert!(!effect.changed, "already at the target value");
        assert!(effect.reversible(), "before is still known");
        assert_eq!(api.call_count(gopt_hal::HalOp::SetWorkingSet), 0);
    }

    #[test]
    fn working_set_preview_reads_but_does_not_write() {
        let api = MockApi::sample_workstation();
        let step = step(PlanAction::WorkingSet {
            limits: WorkingSetLimits::from_mb(256, 0).expect("limits"),
        });
        let effect = execute(&api, &step, Some("cs2.exe"), true).expect("preview");
        assert!(effect.changed);
        assert!(effect.reversible());
        assert_eq!(
            effect.before.as_ref().expect("before")["min_bytes"],
            200 * 1024,
            "the preview shows the mock default"
        );
        assert_eq!(api.call_count(gopt_hal::HalOp::SetWorkingSet), 0);
    }

    #[test]
    fn affinity_preview_and_apply_round_trip() {
        let api = MockApi::sample_workstation();
        let plan = AffinityPlan::reserve_last_n_cores(16, 4).expect("plan");
        let step = step(PlanAction::Affinity {
            plan: plan.clone(),
            spec: gopt_policy::AffinitySpec::new(4, gopt_policy::ReserveSide::Last, false, None),
        });
        let preview = execute(&api, &step, Some("cs2.exe"), true).expect("preview");
        assert_eq!(
            preview.before.as_ref().expect("before")["plan"]["total_logical"],
            16
        );
        assert_eq!(api.call_count(gopt_hal::HalOp::SetAffinity), 0);

        let applied = execute(&api, &step, Some("cs2.exe"), false).expect("apply");
        assert!(applied.changed);
        assert_eq!(api.affinity_of(1234).expect("affinity"), plan);
    }

    #[test]
    fn run_entry_missing_is_skipped_or_an_error() {
        let api = MockApi::sample_workstation();
        let ignore = step(PlanAction::RunEntry {
            hive: RunHive::CurrentUser,
            name: "does-not-exist".to_string(),
            enabled: false,
            ignore_missing: true,
        });
        let effect = execute(&api, &ignore, None, false).expect("ignored");
        assert!(effect.missing);
        assert!(!effect.changed);

        let strict = step(PlanAction::RunEntry {
            hive: RunHive::CurrentUser,
            name: "does-not-exist".to_string(),
            enabled: false,
            ignore_missing: false,
        });
        let error = execute(&api, &strict, None, false).expect_err("missing");
        assert_eq!(error.kind(), crate::CoreErrorKind::NotFound);

        let disable = step(PlanAction::RunEntry {
            hive: RunHive::CurrentUser,
            name: "Steam".to_string(),
            enabled: false,
            ignore_missing: false,
        });
        let effect = execute(&api, &disable, None, false).expect("disable");
        assert!(effect.changed);
        assert_eq!(effect.before.as_ref().expect("before")["enabled"], true);
        assert_eq!(effect.after.as_ref().expect("after")["enabled"], false);
        assert_eq!(
            effect.after.as_ref().expect("after")["value_name"],
            "[disabled] Steam"
        );
    }

    #[test]
    fn power_scheme_reports_the_change_or_the_status_quo() {
        let api = MockApi::sample_workstation();
        let step = step(PlanAction::PowerScheme {
            scheme: gopt_policy::PowerSchemeChoice::High,
            selector: PowerSchemeSelector::HighPerformance,
        });
        let effect = execute(&api, &step, None, false).expect("switch");
        assert!(effect.changed);
        // Mock 的默认活动方案是内置「平衡」；`sample_workstation()` 另外装了「高性能」与「节能」。
        assert_eq!(effect.before.as_ref().expect("before")["name"], "Balanced");
        assert_eq!(
            effect.after.as_ref().expect("after")["is_high_performance"],
            true
        );

        let again = execute(&api, &step, None, false).expect("already high");
        assert!(!again.changed);
        assert_eq!(api.call_count(gopt_hal::HalOp::SetPowerScheme), 1);
    }

    #[test]
    fn targets_and_rule_ids_are_stable() {
        let plan = Plan::new(
            "cs2",
            "CS2",
            "Counter-Strike 2",
            1234,
            gopt_policy::PolicyOrigin::builtin("cs2.toml"),
            Vec::new(),
            Vec::new(),
        );
        let priority = step(PlanAction::Priority {
            class: PriorityClass::High,
        });
        assert_eq!(target_of(&priority), "pid:1234");
        assert_eq!(rule_id_of(&plan, &priority), "policy:game/cs2/rule");

        let run = step(PlanAction::RunEntry {
            hive: RunHive::CurrentUser,
            name: "Steam".to_string(),
            enabled: false,
            ignore_missing: true,
        });
        assert_eq!(target_of(&run), "run:HKCU:Steam");

        let power = step(PlanAction::PowerScheme {
            scheme: gopt_policy::PowerSchemeChoice::High,
            selector: PowerSchemeSelector::HighPerformance,
        });
        assert_eq!(target_of(&power), "power-scheme");

        let draft = draft_for(
            &plan,
            &priority,
            Some(serde_json::json!(1)),
            Some(serde_json::json!(2)),
        );
        let record = draft.into_record(1, gopt_journal::GENESIS_HASH);
        assert!(record.has_valid_hash());
        assert_eq!(record.target, "pid:1234");
        assert_eq!(record.rule_id.as_deref(), Some("policy:game/cs2/rule"));

        let rollback =
            rollback_draft("pid:1234", 7, None, None).into_record(2, "x".repeat(64).as_str());
        assert_eq!(rollback.rule_id.as_deref(), Some("rollback:7"));
        assert_eq!(rollback.kind, JournalKind::Rollback);
    }
}
