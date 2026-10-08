//! 写路径：`apply`（计划 → 系统 + 审计日志）与 `rollback`（日志 → 逆序撤销 → 系统 + 审计日志）。
//!
//! # 执行顺序与 fail-closed
//!
//! ```text
//! 1. 前置检查：pid 有效（游戏在跑）、日志可打开且哈希链可信  ← 任何一条不过 ⇒ 直接失败，系统零改动
//! 2. 逐步骤：读当前值 → 写系统（HAL）→ 立刻追加一条审计记录
//! 3. 日志写不进去 ⇒ 立即停手：后面的步骤不再执行，已执行的步骤如实报出
//! ```
//!
//! 为什么不是"先批量写系统、最后统一写日志"：那样一旦进程被杀，会出现**改了却没记**的窗口，
//! "可回滚"就退化成口号。这里把窗口压缩到"单次 API 调用与它的日志行之间"，
//! 并且任何日志故障都直接停手（`journal_blocked`），不再继续制造无法审计的改动。
//!
//! # 回滚
//!
//! 回滚计划完全来自 `gopt-journal`（纯数据、无系统调用），这里只做两件事：
//! 把 [`RollbackAction`] 翻成 HAL 调用，以及为每条执行过的撤销动作追加一条
//! `kind = rollback` + `rule_id = "rollback:<apply_id>"` 的审计记录。
//! 不可执行的步骤（[`RollbackAction::NotActionable`]）保留在报告里并带上原因，绝不静默跳过。

use gopt_hal::{AffinityPlan, PowerSchemeSelector, PriorityClass};
use gopt_journal::{payload, Journal, JournalOptions, RollbackAction, RollbackPlan};
use gopt_policy::{Plan, PowerSchemeChoice};

use crate::error::{CoreError, CoreResult};
use crate::exec;
use crate::i18n::Text;
use crate::model::{
    AppliedStep, ApplyOptions, ApplyReport, PrioReport, RollbackOptions, RollbackReport,
    RollbackStepReport, StartupReport, StepStatus, TuneReport,
};
use crate::Session;

impl Session {
    /// 打开审计日志。
    ///
    /// * `writable = false`（`status` / `journal` / `verify-journal` / `explain`）：
    ///   只读打开，**不截断**尾部残行、不改任何字节；
    /// * `writable = true`（`apply` / `rollback`）：允许把"崩溃残留的尾部半行"截掉，
    ///   之后的追加才会落在一行干净的链上。
    pub(crate) fn open_journal(&self, writable: bool) -> CoreResult<Journal> {
        let options = if writable {
            JournalOptions::new()
        } else {
            JournalOptions::read_only()
        };
        if writable {
            self.paths().ensure_dir()?;
        }
        Journal::open_with(self.paths().journal(), options).map_err(CoreError::from_journal)
    }

    /// 写路径专用的日志打开：链不可信时**禁止**继续修改系统（fail-closed）。
    fn open_trusted_journal(&self) -> CoreResult<Journal> {
        let journal = self.open_journal(true)?;
        let report = journal.verify_chain();
        if report.is_ok() {
            Ok(journal)
        } else {
            Err(CoreError::journal_kind_broken(
                "Journal::verify_chain",
                format!(
                    "the audit chain in {} does not verify, so gopt will not modify the system: {}",
                    self.paths().journal().display(),
                    report.summary()
                ),
            ))
        }
    }

    /// `gopt apply`：执行一份计划。
    ///
    /// `options.dry_run == true` 时只读系统（不写、不落日志），但会把前后值算出来给用户看。
    pub fn apply(&self, plan: &Plan, options: ApplyOptions) -> CoreResult<ApplyReport> {
        if plan.pid == 0 {
            return Err(CoreError::not_found(
                "Session::apply",
                format!(
                    "`{}` is not running, so there is nothing to apply; start the game first or pass --pid",
                    plan.game_id
                ),
            ));
        }

        let processes = self.processes().unwrap_or_default();
        let process_name = exec::process_name(&processes, plan.pid);
        let mut journal = if options.dry_run {
            None
        } else {
            Some(self.open_trusted_journal()?)
        };

        let mut steps: Vec<AppliedStep> = Vec::with_capacity(plan.steps.len());
        let mut journal_ids: Vec<u64> = Vec::new();
        let mut failed = 0usize;
        let mut applied = 0usize;
        let mut unchanged = 0usize;
        let mut skipped_missing = 0usize;
        let mut blocked = false;
        let mut notices: Vec<Text> = Vec::new();

        for step in &plan.steps {
            let effect = if blocked {
                Err(CoreError::journal_kind_broken(
                    "Session::apply",
                    "skipped because the audit journal could not be written",
                ))
            } else {
                exec::execute(self.api(), step, process_name.as_deref(), options.dry_run)
            };

            match effect {
                Ok(effect) => {
                    let mut journal_id = None;
                    if !options.dry_run && effect.changed {
                        match journal.as_mut() {
                            Some(open) => {
                                match open.append(exec::draft_for(
                                    plan,
                                    step,
                                    effect.before.clone(),
                                    effect.after.clone(),
                                )) {
                                    Ok(record) => {
                                        journal_id = Some(record.id);
                                        journal_ids.push(record.id);
                                    }
                                    Err(error) => {
                                        let error = CoreError::from_journal(error);
                                        blocked = true;
                                        failed += 1;
                                        notices.push(Text::new(
                                            "审计日志写不进去，已停止执行剩余步骤（前面的改动都已入链）",
                                            "the audit journal could not be written; the remaining steps were \
                                             not executed (earlier changes are already on the chain)",
                                        ));
                                        steps.push(AppliedStep {
                                            order: step.order,
                                            rule_id: step.rule_id.clone(),
                                            rule_line: step.rule_line,
                                            hal_op: step.hal_op(),
                                            status: StepStatus::Stopped,
                                            before: effect.before.clone(),
                                            after: effect.after.clone(),
                                            error: Some(error),
                                            journal_id: None,
                                            note: None,
                                            reversible: false,
                                        });
                                        continue;
                                    }
                                }
                            }
                            None => {
                                blocked = true;
                            }
                        }
                    }

                    let status = if options.dry_run {
                        StepStatus::DryRun
                    } else if effect.missing {
                        StepStatus::SkippedMissing
                    } else if effect.changed {
                        StepStatus::Applied
                    } else {
                        StepStatus::Unchanged
                    };
                    match status {
                        StepStatus::Applied => applied += 1,
                        StepStatus::Unchanged => unchanged += 1,
                        StepStatus::SkippedMissing => skipped_missing += 1,
                        _ => {}
                    }
                    let reversible = effect.reversible();
                    steps.push(AppliedStep {
                        order: step.order,
                        rule_id: step.rule_id.clone(),
                        rule_line: step.rule_line,
                        hal_op: step.hal_op(),
                        status,
                        before: effect.before,
                        after: effect.after,
                        error: None,
                        journal_id,
                        note: effect.note,
                        reversible,
                    });
                }
                Err(error) => {
                    failed += 1;
                    steps.push(AppliedStep {
                        order: step.order,
                        rule_id: step.rule_id.clone(),
                        rule_line: step.rule_line,
                        hal_op: step.hal_op(),
                        status: if blocked {
                            StepStatus::Stopped
                        } else {
                            StepStatus::Failed
                        },
                        before: None,
                        after: None,
                        error: Some(error),
                        journal_id: None,
                        note: None,
                        reversible: false,
                    });
                }
            }
        }

        if options.dry_run {
            notices.push(Text::new(
                "预演模式：没有写任何系统状态、也没有写审计日志；确认无误后加 --yes 真正执行",
                "dry-run: nothing was written to the system or the audit journal; re-run with --yes to apply",
            ));
        }
        if plan.requires_elevation() && !self.is_elevated() {
            notices.push(Text::new(
                "计划里有需要管理员权限的步骤，当前未提权：这类步骤会以 access_denied 失败",
                "some steps need administrator rights and this process is not elevated; they fail with access_denied",
            ));
        }
        if steps
            .iter()
            .any(|step| step.status == StepStatus::Applied && !step.reversible)
        {
            notices.push(Text::new(
                "有步骤的写入前状态不可知（读取失败，或系统报告的最小工作集为 0），它们无法回滚：回滚计划会明确报 not_actionable",
                "some steps have an unknown previous state (the read failed, or the system reports a \
                 zero minimum working set) and cannot be rolled back; the rollback plan reports them \
                 as not_actionable",
            ));
        }

        Ok(ApplyReport {
            game_id: plan.game_id.clone(),
            game_name_zh: plan.game_name_zh.clone(),
            game_name_en: plan.game_name_en.clone(),
            pid: plan.pid,
            policy_origin: plan.policy_origin.display_path(),
            dry_run: options.dry_run,
            applied,
            unchanged,
            skipped_missing,
            failed,
            journal_blocked: blocked,
            steps,
            skipped: plan.skipped.clone(),
            journal_ids,
            journal_path: self.paths().journal().display().to_string(),
            requires_elevation: plan.requires_elevation(),
            has_dangerous_steps: plan.has_dangerous_steps(),
            notices,
        })
    }

    /// `gopt rollback`：从审计日志**重新推导**逆序撤销计划并执行。
    pub fn rollback(&self, options: RollbackOptions) -> CoreResult<RollbackReport> {
        // 预演不写日志：只读打开（连"尾部半行修复"这种写也不做）。
        let mut journal = if options.dry_run {
            let journal = self.open_journal(false)?;
            let report = journal.verify_chain();
            if !report.is_ok() {
                return Err(CoreError::journal_kind_broken(
                    "Journal::verify_chain",
                    format!(
                        "the audit chain in {} does not verify, so gopt will not plan a rollback on it: {}",
                        self.paths().journal().display(),
                        report.summary()
                    ),
                ));
            }
            journal
        } else {
            self.open_trusted_journal()?
        };
        let plan: RollbackPlan = if let Some(to_id) = options.to_id {
            journal
                .plan_rollback(to_id)
                .map_err(CoreError::from_journal)?
        } else if options.pending {
            let records = journal.records_owned();
            gopt_journal::plan_rollback_pending(&records).map_err(CoreError::from_journal)?
        } else {
            journal
                .plan_rollback_all()
                .map_err(CoreError::from_journal)?
        };

        let mut steps: Vec<RollbackStepReport> = Vec::with_capacity(plan.len());
        let mut journal_ids: Vec<u64> = Vec::new();
        let mut executed = 0usize;
        let mut failed = 0usize;
        let mut blocked = false;
        let mut notices: Vec<Text> = Vec::new();

        for step in &plan.steps {
            let actionable = step.action.is_executable();
            if !actionable {
                let reason = match &step.action {
                    RollbackAction::NotActionable { reason } => reason.clone(),
                    _ => "the record cannot be undone".to_string(),
                };
                steps.push(RollbackStepReport {
                    journal_id: step.journal_id,
                    kind: step.kind,
                    target: step.target.clone(),
                    action: step.action.clone(),
                    description: step.describe(),
                    actionable: false,
                    status: StepStatus::NotActionable,
                    error: None,
                    rollback_record_id: None,
                });
                notices.push(Text::new(
                    format!(
                        "#{}（{}）无法自动撤销：{reason}",
                        step.journal_id, step.target
                    ),
                    format!(
                        "#{} ({}) cannot be undone automatically: {reason}",
                        step.journal_id, step.target
                    ),
                ));
                continue;
            }

            if options.dry_run {
                steps.push(RollbackStepReport {
                    journal_id: step.journal_id,
                    kind: step.kind,
                    target: step.target.clone(),
                    action: step.action.clone(),
                    description: step.describe(),
                    actionable: true,
                    status: StepStatus::DryRun,
                    error: None,
                    rollback_record_id: None,
                });
                continue;
            }
            if blocked {
                steps.push(RollbackStepReport {
                    journal_id: step.journal_id,
                    kind: step.kind,
                    target: step.target.clone(),
                    action: step.action.clone(),
                    description: step.describe(),
                    actionable: true,
                    status: StepStatus::Stopped,
                    error: Some(CoreError::journal_kind_broken(
                        "Session::rollback",
                        "skipped because the audit journal could not be written",
                    )),
                    rollback_record_id: None,
                });
                continue;
            }

            match self.undo(&step.action) {
                Ok((before, after)) => {
                    let draft = exec::rollback_draft(&step.target, step.journal_id, before, after);
                    match journal.append(draft) {
                        Ok(record) => {
                            journal_ids.push(record.id);
                            executed += 1;
                            steps.push(RollbackStepReport {
                                journal_id: step.journal_id,
                                kind: step.kind,
                                target: step.target.clone(),
                                action: step.action.clone(),
                                description: step.describe(),
                                actionable: true,
                                status: StepStatus::Applied,
                                error: None,
                                rollback_record_id: Some(record.id),
                            });
                        }
                        Err(error) => {
                            blocked = true;
                            failed += 1;
                            notices.push(Text::new(
                                "撤销动作已执行，但审计记录写不进去：后续步骤全部停手（请立刻 verify-journal）",
                                "the undo action ran but its audit record could not be written; the remaining \
                                 steps were not executed (run verify-journal now)",
                            ));
                            steps.push(RollbackStepReport {
                                journal_id: step.journal_id,
                                kind: step.kind,
                                target: step.target.clone(),
                                action: step.action.clone(),
                                description: step.describe(),
                                actionable: true,
                                status: StepStatus::Stopped,
                                error: Some(CoreError::from_journal(error)),
                                rollback_record_id: None,
                            });
                        }
                    }
                }
                Err(error) => {
                    failed += 1;
                    steps.push(RollbackStepReport {
                        journal_id: step.journal_id,
                        kind: step.kind,
                        target: step.target.clone(),
                        action: step.action.clone(),
                        description: step.describe(),
                        actionable: true,
                        status: StepStatus::Failed,
                        error: Some(error),
                        rollback_record_id: None,
                    });
                }
            }
        }

        if options.dry_run {
            notices.push(Text::new(
                "预演模式：只生成了撤销计划，没有改系统、没有写日志；确认后加 --yes 执行",
                "dry-run: the undo plan was generated but nothing was executed; re-run with --yes",
            ));
        }
        let not_actionable = steps.iter().filter(|step| !step.actionable).count();
        if not_actionable > 0 {
            notices.push(Text::new(
                format!("{not_actionable} 步需要人工确认（gopt 没有可用的写入前状态）"),
                format!(
                    "{not_actionable} step(s) need a human: gopt has no previous state to restore"
                ),
            ));
        }

        Ok(RollbackReport {
            to_id: plan.to_id,
            dry_run: options.dry_run,
            planned: plan.len(),
            executable: plan.actionable(),
            executed,
            failed,
            not_actionable,
            steps,
            journal_ids,
            journal_path: self.paths().journal().display().to_string(),
            summary: plan.summary(),
            notices,
        })
    }

    /// 执行一条类型化撤销动作，返回 (before, after) 载荷用于审计记录。
    fn undo(
        &self,
        action: &RollbackAction,
    ) -> CoreResult<(Option<serde_json::Value>, Option<serde_json::Value>)> {
        let api = self.api();
        match action {
            RollbackAction::RestorePriority {
                pid,
                name,
                priority,
            } => {
                let previous = api.set_priority(*pid, *priority).map_err(|error| {
                    CoreError::from_hal(format!("cannot restore the priority of pid {pid}"), error)
                })?;
                Ok((
                    Some(payload::priority(*pid, name.as_deref(), previous)),
                    Some(payload::priority(*pid, name.as_deref(), *priority)),
                ))
            }
            RollbackAction::RestoreAffinity { pid, name, plan } => {
                let info = api.get_affinity(*pid).map_err(|error| {
                    CoreError::from_hal(format!("cannot read the affinity of pid {pid}"), error)
                })?;
                let before =
                    AffinityPlan::single(info.total_logical, info.group, info.process_mask)
                        .ok()
                        .map(|current| payload::affinity(*pid, name.as_deref(), &current));
                for batch in plan.per_group() {
                    api.set_affinity(*pid, &batch).map_err(|error| {
                        CoreError::from_hal(
                            format!("cannot restore the affinity of pid {pid}"),
                            error,
                        )
                    })?;
                }
                Ok((before, Some(payload::affinity(*pid, name.as_deref(), plan))))
            }
            RollbackAction::RestoreWorkingSet { pid, name, limits } => {
                // 撤销记录自己也要可解释：before = 撤销前的真实值，after = 写回后的真实值。
                let before = api
                    .get_working_set(*pid)
                    .ok()
                    .map(|current| payload::working_set(*pid, name.as_deref(), current));
                api.set_working_set(*pid, *limits).map_err(|error| {
                    CoreError::from_hal(
                        format!("cannot restore the working set of pid {pid}"),
                        error,
                    )
                })?;
                let after = api.get_working_set(*pid).ok().map_or_else(
                    || payload::working_set(*pid, name.as_deref(), limits.normalized()),
                    |restored| payload::working_set(*pid, name.as_deref(), restored),
                );
                Ok((before, Some(after)))
            }
            RollbackAction::RestorePowerScheme { guid, .. } => {
                let change = api
                    .set_power_scheme(&PowerSchemeSelector::Explicit(*guid))
                    .map_err(|error| {
                        CoreError::from_hal("cannot restore the active power scheme", error)
                    })?;
                Ok((
                    Some(payload::power_scheme(&change.previous)),
                    Some(payload::power_scheme(&change.current)),
                ))
            }
            RollbackAction::RestoreRunEntry {
                hive,
                value_name,
                enabled,
            } => {
                let display = gopt_hal::RunEntry::display_name(value_name).to_string();
                let before_entry = api.list_run_entries().ok().and_then(|entries| {
                    entries
                        .into_iter()
                        .find(|entry| entry.hive == *hive && entry.name == display)
                });
                let updated = api
                    .set_run_entry_enabled(*hive, &display, *enabled)
                    .map_err(|error| {
                        CoreError::from_hal(
                            format!("cannot restore the run entry {hive}:{display}"),
                            error,
                        )
                    })?;
                Ok((
                    before_entry.as_ref().map(payload::run_entry),
                    Some(payload::run_entry(&updated)),
                ))
            }
            RollbackAction::RestoreLegacySnapshot {
                process_id,
                priority,
                affinity_mask,
                working_set,
                power_scheme_guid,
            } => {
                let mut before = Vec::new();
                let mut after = Vec::new();
                let mut first_error: Option<CoreError> = None;
                let mut attempt =
                    |result: gopt_hal::HalResult<()>,
                     label: &str,
                     before_value: serde_json::Value,
                     after_value: serde_json::Value| {
                        match result {
                            Ok(()) => {
                                before.push(before_value);
                                after.push(after_value);
                            }
                            Err(error) => {
                                if first_error.is_none() {
                                    first_error = Some(CoreError::from_hal(
                                    format!("cannot restore the legacy snapshot field `{label}` for pid {process_id}"),
                                    error,
                                ));
                                }
                            }
                        }
                    };

                // 逐字段尝试：旧快照里的字段可以缺，缺的就不动（宁少勿错）。
                if let Some(priority) = priority {
                    let previous = api.get_priority(*process_id);
                    let result = api.set_priority(*process_id, *priority).map(|_| ());
                    if let Ok(previous) = previous {
                        attempt(
                            result,
                            "priority",
                            payload::priority(*process_id, None, previous),
                            payload::priority(*process_id, None, *priority),
                        );
                    } else {
                        attempt(
                            result,
                            "priority",
                            serde_json::Value::Null,
                            payload::priority(*process_id, None, *priority),
                        );
                    }
                }
                if let Some(mask) = affinity_mask {
                    if let Ok(plan) = AffinityPlan::single(self.hardware().logical_cores, 0, *mask)
                    {
                        attempt(
                            api.set_affinity(*process_id, &plan).map(|_| ()),
                            "affinity",
                            serde_json::Value::Null,
                            payload::affinity(*process_id, None, &plan),
                        );
                    }
                }
                if let Some(limits) = working_set {
                    attempt(
                        api.set_working_set(*process_id, *limits),
                        "working_set",
                        serde_json::Value::Null,
                        payload::working_set(*process_id, None, (*limits).normalized()),
                    );
                }
                if let Some(guid_text) = power_scheme_guid {
                    if let Ok(guid) = guid_text.parse::<gopt_hal::Guid>() {
                        if let Ok(change) =
                            api.set_power_scheme(&PowerSchemeSelector::Explicit(guid))
                        {
                            before.push(payload::power_scheme(&change.previous));
                            after.push(payload::power_scheme(&change.current));
                        }
                    }
                }

                if let Some(error) = first_error {
                    return Err(error);
                }
                Ok((
                    Some(serde_json::Value::Array(before)),
                    Some(serde_json::Value::Array(after)),
                ))
            }
            RollbackAction::NotActionable { reason } => Err(CoreError::new(
                crate::CoreErrorKind::Unsupported,
                "Session::rollback",
                format!("not actionable: {reason}"),
            )),
        }
    }

    /// `gopt prio`：读取或设置某个进程的优先级（设置时写审计日志）。
    pub fn priority(
        &self,
        pid: Option<u32>,
        exe: Option<&str>,
        set: Option<PriorityClass>,
        dry_run: bool,
    ) -> CoreResult<PrioReport> {
        let (pid, name) = self.resolve_target(pid, exe)?;
        let api = self.api();
        let current = api.get_priority(pid).map_err(|error| {
            CoreError::from_hal(format!("cannot read the priority of pid {pid}"), error)
        })?;

        let Some(target) = set else {
            return Ok(PrioReport {
                pid,
                name,
                before: current,
                after: current,
                changed: false,
                dry_run: true,
                journal_id: None,
                journal_path: self.paths().journal().display().to_string(),
            });
        };
        if target == current {
            return Ok(PrioReport {
                pid,
                name,
                before: current,
                after: current,
                changed: false,
                dry_run,
                journal_id: None,
                journal_path: self.paths().journal().display().to_string(),
            });
        }
        if dry_run {
            return Ok(PrioReport {
                pid,
                name,
                before: current,
                after: target,
                changed: true,
                dry_run: true,
                journal_id: None,
                journal_path: self.paths().journal().display().to_string(),
            });
        }

        let mut journal = self.open_trusted_journal()?;
        let previous = api.set_priority(pid, target).map_err(|error| {
            CoreError::from_hal(format!("cannot set the priority of pid {pid}"), error)
        })?;
        let draft = gopt_journal::JournalDraft::now(
            gopt_journal::JournalKind::Apply,
            payload::pid_target(pid),
        )
        .with_values(
            Some(payload::priority(pid, name.as_deref(), previous)),
            Some(payload::priority(pid, name.as_deref(), target)),
        )
        .with_rule_id("cli:prio");
        let record = journal.append(draft).map_err(CoreError::from_journal)?;
        Ok(PrioReport {
            pid,
            name,
            before: previous,
            after: target,
            changed: true,
            dry_run: false,
            journal_id: Some(record.id),
            journal_path: self.paths().journal().display().to_string(),
        })
    }

    /// `gopt tune`：查询或切换电源方案（切换时写审计日志）。
    pub fn tune(&self, target: Option<PowerSchemeChoice>, dry_run: bool) -> CoreResult<TuneReport> {
        let api = self.api();
        let current = api
            .query_power_scheme()
            .map_err(|error| CoreError::from_hal("cannot query the active power scheme", error))?;
        let journal_path = self.paths().journal().display().to_string();

        let Some(choice) = target else {
            return Ok(TuneReport {
                query_only: true,
                target: None,
                before: current.clone(),
                after: current,
                changed: false,
                dry_run: true,
                journal_id: None,
                journal_path,
            });
        };
        let selector = choice.selector();
        let already = match &selector {
            PowerSchemeSelector::HighPerformance => current.is_high_performance,
            PowerSchemeSelector::Explicit(guid) => current.guid == *guid,
        };
        if already {
            return Ok(TuneReport {
                query_only: false,
                target: Some(choice.as_str().to_string()),
                before: current.clone(),
                after: current,
                changed: false,
                dry_run,
                journal_id: None,
                journal_path,
            });
        }
        if dry_run {
            return Ok(TuneReport {
                query_only: false,
                target: Some(choice.as_str().to_string()),
                before: current.clone(),
                after: current,
                changed: true,
                dry_run: true,
                journal_id: None,
                journal_path,
            });
        }
        let mut journal = self.open_trusted_journal()?;
        let change = api
            .set_power_scheme(&selector)
            .map_err(|error| CoreError::from_hal("cannot switch the active power scheme", error))?;
        let draft = gopt_journal::JournalDraft::now(
            gopt_journal::JournalKind::Apply,
            payload::POWER_TARGET,
        )
        .with_values(
            Some(payload::power_scheme(&change.previous)),
            Some(payload::power_scheme(&change.current)),
        )
        .with_rule_id("cli:tune");
        let record = journal.append(draft).map_err(CoreError::from_journal)?;
        Ok(TuneReport {
            query_only: false,
            target: Some(choice.as_str().to_string()),
            before: change.previous,
            after: change.current,
            changed: true,
            dry_run: false,
            journal_id: Some(record.id),
            journal_path,
        })
    }

    /// `gopt startup list`：列全部开机启动项（只读）。
    pub fn startup_list(&self) -> CoreResult<StartupReport> {
        let entries = self
            .api()
            .list_run_entries()
            .map_err(|error| CoreError::from_hal("cannot list the Run key entries", error))?;
        Ok(StartupReport {
            action: "list".to_string(),
            entries,
            before: None,
            after: None,
            changed: false,
            dry_run: true,
            journal_id: None,
            journal_path: self.paths().journal().display().to_string(),
        })
    }

    /// `gopt startup enable|disable <名称>`：改名迁移（可逆），写审计日志。
    pub fn startup_set(
        &self,
        hive: gopt_hal::RunHive,
        name: &str,
        enabled: bool,
        dry_run: bool,
    ) -> CoreResult<StartupReport> {
        let api = self.api();
        let entries = api
            .list_run_entries()
            .map_err(|error| CoreError::from_hal("cannot list the Run key entries", error))?;
        let current = entries
            .iter()
            .find(|entry| entry.hive == hive && entry.name == name)
            .cloned()
            .ok_or_else(|| {
                CoreError::not_found(
                    "set_run_entry_enabled",
                    format!("the run entry {hive}:{name} does not exist"),
                )
            })?;
        let journal_path = self.paths().journal().display().to_string();
        if current.enabled == enabled {
            return Ok(StartupReport {
                action: if enabled { "enable" } else { "disable" }.to_string(),
                entries: Vec::new(),
                before: Some(current.clone()),
                after: Some(current),
                changed: false,
                dry_run,
                journal_id: None,
                journal_path,
            });
        }
        if dry_run {
            return Ok(StartupReport {
                action: if enabled { "enable" } else { "disable" }.to_string(),
                entries: Vec::new(),
                before: Some(current.clone()),
                after: Some(current),
                changed: true,
                dry_run: true,
                journal_id: None,
                journal_path,
            });
        }

        let mut journal = self.open_trusted_journal()?;
        let updated = api
            .set_run_entry_enabled(hive, name, enabled)
            .map_err(|error| {
                CoreError::from_hal(
                    format!(
                        "cannot {} the run entry {hive}:{name}",
                        if enabled { "enable" } else { "disable" }
                    ),
                    error,
                )
            })?;
        let draft = gopt_journal::JournalDraft::now(
            gopt_journal::JournalKind::Apply,
            payload::run_target(hive, name),
        )
        .with_values(
            Some(payload::run_entry(&current)),
            Some(payload::run_entry(&updated)),
        )
        .with_rule_id("cli:startup");
        let record = journal.append(draft).map_err(CoreError::from_journal)?;
        Ok(StartupReport {
            action: if enabled { "enable" } else { "disable" }.to_string(),
            entries: Vec::new(),
            before: Some(current),
            after: Some(updated),
            changed: true,
            dry_run: false,
            journal_id: Some(record.id),
            journal_path,
        })
    }

    /// 解析 `--pid` / `--exe` 目标（二者必给其一；都给了就以 pid 为准）。
    pub(crate) fn resolve_target(
        &self,
        pid: Option<u32>,
        exe: Option<&str>,
    ) -> CoreResult<(u32, Option<String>)> {
        if let Some(pid) = pid {
            let name = self
                .processes()
                .unwrap_or_default()
                .into_iter()
                .find(|process| process.pid == pid)
                .map(|process| process.name);
            return Ok((pid, name));
        }
        let Some(exe) = exe else {
            return Err(CoreError::usage(
                "Session::resolve_target",
                "either --pid <pid> or --exe <name.exe> is required",
            ));
        };
        let processes = self.processes()?;
        let wanted = exe.trim();
        let found = processes.iter().find(|process| {
            process.name.eq_ignore_ascii_case(wanted)
                || process.name.eq_ignore_ascii_case(&format!("{wanted}.exe"))
                || gopt_policy::wildcard_match(wanted, &process.name)
        });
        match found {
            Some(process) => Ok((process.pid, Some(process.name.clone()))),
            None => Err(CoreError::not_found(
                "Session::resolve_target",
                format!("no running process matches `{exe}`"),
            )),
        }
    }
}
