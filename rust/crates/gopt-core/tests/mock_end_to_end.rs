//! 端到端集成测试（**MockApi，无需管理员**）：`detect → load policy → plan → apply → journal → rollback`。
//!
//! 这些用例回答的是同一类问题："内核声称的能力，在一条真实链路上是否真的成立？"
//!
//! | 用例 | 证明什么 |
//! | --- | --- |
//! | `plan_apply_journal_rollback_round_trip` | 计划 → 系统 → 审计链 → 逆序撤销，全链路闭环 |
//! | `dry_run_writes_nothing` | 默认安全：预演既不写系统也不写日志 |
//! | `apply_fails_closed_on_a_broken_chain` | 审计链不可信时**拒绝修改系统**（fail-closed） |
//! | `apply_keeps_going_after_a_step_failure` | 单步失败不拖垮整份计划，且**只有成功的步骤入链** |
//! | `working_set_round_trips_back_to_the_previous_limits` | 工作集可回滚：before 是真值、回滚按字节还原 |
//! | `working_set_without_a_readable_before_is_still_honest` | 读不到前值时不写假的 `before`，如实报不可回滚 |
//! | `legacy_import_then_rollback` | C++ v1.1.0 旧格式导入 → 逆序恢复旧快照 |
//! | `watch_applies_once_per_process` | 监控模式下同一进程只优化一次 |
//!
//! 临时目录手写在 `support` 里（不引入 tempfile/rand），每个用例一个独立目录，
//! 通过 `DataPaths` 让内核只在临时目录里读写——真实配置与真实游戏进程全程不被触碰。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gopt_core::{
    ApplyOptions, DataPaths, HalError, HalOp, JournalFilter, JournalKind, Lang,
    LegacyImportOptions, RollbackOptions, Session, StepStatus, SystemApi, WatchOptions, WatchState,
};
use gopt_hal::{MockApi, PriorityClass, WorkingSetLimits};

/// 一次性临时目录（`Drop` 时删除）。
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gopt-core-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.jsonl")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 建一个 Mock 会话（返回 mock 句柄用于断言系统侧状态）。
fn session(dir: &Path) -> (Arc<MockApi>, Session) {
    let mock = Arc::new(MockApi::sample_workstation());
    let api: Arc<dyn SystemApi> = mock.clone();
    let session = Session::new(api, DataPaths::new(dir), Lang::Zh).expect("session");
    (mock, session)
}

#[test]
fn plan_apply_journal_rollback_round_trip() {
    let dir = TempDir::new("round-trip");
    let (mock, session) = session(dir.path());

    // 1) plan：只读。cs2 策略在 16 逻辑核 / 16GB 上有 3 个步骤（优先级 + 亲和性 + 工作集）。
    let report = session.plan("cs2", None).expect("plan");
    assert!(report.running);
    assert_eq!(report.plan.pid, 1234);
    assert_eq!(
        report.plan.step_count(),
        3,
        "cs2 policy has three applicable rules"
    );
    assert_eq!(
        mock.call_count(HalOp::SetPriority),
        0,
        "plan must not write"
    );
    assert!(!dir.journal().exists(), "plan must not create the journal");

    // 2) apply：写系统 + 写审计链。
    let applied = session
        .apply(&report.plan, ApplyOptions::commit())
        .expect("apply");
    assert!(applied.all_ok(), "apply failed: {applied:?}");
    assert_eq!(applied.applied, 3);
    assert_eq!(applied.journal_ids.len(), 3);
    assert_eq!(applied.failed, 0);
    assert_eq!(mock.priority_of(1234), Some(PriorityClass::High));
    assert!(dir.journal().exists());

    // 3) journal：链完好、记录字段齐全。
    let view = session
        .journal_view(JournalFilter {
            kind: None,
            limit: None,
        })
        .expect("journal view");
    assert_eq!(view.records, 3);
    assert!(view.chain.is_ok(), "{}", view.chain.summary());
    assert!(view.entries.iter().all(|entry| entry.hash_ok));
    assert_eq!(view.entries[0].target, "pid:1234");
    assert_eq!(
        view.entries[0].rule_id.as_deref(),
        Some("policy:game/cs2/priority"),
        "the audit trail points back at the exact rule"
    );
    assert!(
        view.entries[0].reversible,
        "priority records carry the previous class"
    );
    assert!(
        view.entries[2].reversible,
        "the working-set record carries the real previous limits"
    );
    let working_set_before = view.entries[2]
        .before
        .as_ref()
        .expect("the working-set record has a before payload");
    assert_eq!(
        working_set_before["min_bytes"],
        200 * 1024,
        "{working_set_before}"
    );
    assert_eq!(view.pending_apply_ids.len(), 3);

    // 4) rollback：从日志逆序撤销，系统回到原状（工作集也是了）。
    let rolled = session
        .rollback(RollbackOptions::all().preview())
        .expect("rollback preview");
    assert!(rolled.dry_run);
    assert_eq!(rolled.planned, 3);
    assert_eq!(rolled.executable, 3, "every applied step is actionable now");
    assert_eq!(rolled.not_actionable, 0);
    assert_eq!(rolled.executed, 0);
    assert_eq!(
        mock.priority_of(1234),
        Some(PriorityClass::High),
        "preview must not write"
    );

    let rolled = session.rollback(RollbackOptions::all()).expect("rollback");
    assert!(rolled.all_ok(), "rollback failed: {rolled:?}");
    assert_eq!(rolled.executed, 3);
    assert_eq!(rolled.journal_ids.len(), 3);
    assert_eq!(rolled.not_actionable, 0);
    assert_eq!(
        mock.priority_of(1234),
        Some(PriorityClass::Normal),
        "priority restored"
    );
    assert_eq!(
        mock.affinity_of(1234).expect("affinity").selected_logical(),
        16,
        "affinity restored to the full mask"
    );
    assert_eq!(
        mock.working_set_of(1234),
        Some(
            WorkingSetLimits::observed(200 * 1024, 0)
                .expect("observed")
                .normalized()
        ),
        "working set restored to the exact previous limits"
    );

    // 5) 撤销记录自己也是一条审计记录（rule_id = rollback:<apply_id>）。
    let view = session
        .journal_view(JournalFilter {
            kind: Some(JournalKind::Rollback),
            limit: None,
        })
        .expect("journal view");
    assert_eq!(view.entries.len(), 3);
    assert!(view.entries.iter().all(|entry| entry
        .rule_id
        .as_deref()
        .unwrap_or("")
        .starts_with("rollback:")));
    assert_eq!(
        view.pending_apply_ids.len(),
        0,
        "every applied record has been undone"
    );
    assert_eq!(view.undone_apply_ids.len(), 3);

    // 6) 再回滚一次：没有新的可撤销动作（幂等）。
    let again = session
        .rollback(RollbackOptions::pending())
        .expect("pending");
    assert_eq!(again.planned, 0, "nothing is left to undo");
    assert_eq!(again.executed, 0);
}

#[test]
fn dry_run_writes_nothing() {
    let dir = TempDir::new("dry-run");
    let (mock, session) = session(dir.path());
    let plan = session.plan("cs2", None).expect("plan").plan;

    let preview = session
        .apply(&plan, ApplyOptions::preview())
        .expect("preview");
    assert!(preview.dry_run);
    assert_eq!(preview.applied, 0);
    assert_eq!(preview.failed, 0);
    assert!(preview
        .steps
        .iter()
        .all(|step| step.status == StepStatus::DryRun));
    assert!(
        preview.steps[0].before.is_some(),
        "a preview must show the current value"
    );
    assert!(
        preview.steps[0].after.is_some(),
        "and the value it would write"
    );
    assert_eq!(preview.journal_ids.len(), 0);
    assert_eq!(mock.priority_of(1234), Some(PriorityClass::Normal));
    assert_eq!(mock.call_count(HalOp::SetPriority), 0);
    assert_eq!(mock.call_count(HalOp::SetAffinity), 0);
    assert_eq!(mock.call_count(HalOp::SetWorkingSet), 0);
    assert_eq!(
        mock.call_count(HalOp::GetWorkingSet),
        1,
        "the preview reads the current working-set limits"
    );
    assert!(
        !dir.journal().exists(),
        "a dry run must not create the journal"
    );
}

#[test]
fn apply_fails_closed_on_a_broken_chain() {
    let dir = TempDir::new("broken-chain");
    let (mock, session) = session(dir.path());
    let plan = session.plan("cs2", None).expect("plan").plan;
    session
        .apply(&plan, ApplyOptions::commit())
        .expect("first apply");

    // 把优先级改回 normal，然后篡改日志（加一个空格 ⇒ 不再是规范行）。
    mock.set_priority(1234, PriorityClass::Normal)
        .expect("reset");
    let text = std::fs::read_to_string(dir.journal()).expect("read journal");
    let tampered = text.replacen(",\"kind\":\"apply\"", ", \"kind\":\"apply\"", 1);
    assert_ne!(text, tampered);
    std::fs::write(dir.journal(), tampered).expect("write journal");

    // 从这一刻起不允许再有任何写调用（fail-closed）。
    let writes_before = mock.call_count(HalOp::SetPriority);
    let error = session
        .apply(&plan, ApplyOptions::commit())
        .expect_err("a broken chain must block the write path");
    assert_eq!(error.kind(), gopt_core::CoreErrorKind::AuditChainBroken);
    assert_eq!(error.exit_code(), 3);
    assert_eq!(
        mock.priority_of(1234),
        Some(PriorityClass::Normal),
        "fail-closed: the system must not be modified"
    );
    assert_eq!(
        mock.call_count(HalOp::SetPriority),
        writes_before,
        "fail-closed: not a single write call may happen on a broken chain"
    );

    // 回滚同样被拒绝（不能基于被篡改的日志改系统）。
    let rollback = session
        .rollback(RollbackOptions::all())
        .expect_err("rollback on a broken chain must be refused");
    assert_eq!(rollback.kind(), gopt_core::CoreErrorKind::AuditChainBroken);

    // verify-journal 报出第一处不一致；只读路径仍然可用。
    let report = session
        .verify_journal(gopt_core::VerifyOptions::default())
        .expect("verify");
    assert_eq!(report.status, gopt_core::VerifyStatus::Broken);
    let breakage = report.first_break.expect("a break is reported");
    assert_eq!(breakage.problem.as_str(), "line_canonical");
    assert_eq!(breakage.line_no, 1);
}

#[test]
fn apply_keeps_going_after_a_step_failure() {
    let dir = TempDir::new("step-failure");
    let (mock, session) = session(dir.path());
    let plan = session.plan("cs2", None).expect("plan").plan;

    mock.fail_next(
        HalOp::SetAffinity,
        HalError::access_denied("SetProcessAffinityMask", "denied by the test"),
    );
    let applied = session.apply(&plan, ApplyOptions::commit()).expect("apply");
    assert_eq!(applied.failed, 1);
    assert_eq!(
        applied.applied, 2,
        "priority and working set still went through"
    );
    assert!(!applied.journal_blocked);
    let failed = applied
        .steps
        .iter()
        .find(|step| step.status == StepStatus::Failed)
        .expect("a failed step");
    assert_eq!(failed.hal_op, HalOp::SetAffinity);
    assert_eq!(
        failed.error.as_ref().map(|error| error.kind()),
        Some(gopt_core::CoreErrorKind::AccessDenied)
    );
    assert!(failed
        .error
        .as_ref()
        .expect("error")
        .hint_zh()
        .contains("管理员"));

    // 只有成功的步骤进审计链：失败的步骤没有留下"假的已应用"记录。
    let view = session
        .journal_view(JournalFilter {
            kind: None,
            limit: None,
        })
        .expect("view");
    assert_eq!(view.records, 2);
    assert!(view.chain.is_ok());
    assert!(view.entries.iter().all(|entry| entry.target == "pid:1234"));
}

#[test]
fn working_set_round_trips_back_to_the_previous_limits() {
    let dir = TempDir::new("working-set-round-trip");
    let (mock, session) = session(dir.path());

    // 明确安排"写入前"的工作集：断言的是具体数值，而不是某个碰巧的默认值。
    let before = WorkingSetLimits::from_mb(200, 512).expect("before");
    mock.seed_working_set(1234, before);

    let plan = session.plan("cs2", None).expect("plan").plan;
    let applied = session.apply(&plan, ApplyOptions::commit()).expect("apply");
    let working_set = applied
        .steps
        .iter()
        .find(|step| step.hal_op == HalOp::SetWorkingSet)
        .expect("a working-set step");
    assert!(
        working_set.reversible,
        "the previous limits are read from the system now"
    );
    let recorded = working_set.before.as_ref().expect("before payload");
    assert_eq!(recorded["min_bytes"], before.min_bytes, "{recorded}");
    assert_eq!(recorded["max_bytes"], before.max_bytes, "{recorded}");

    // 系统上确实是策略目标（256 MiB / 无上限），而不是"我们以为写了"。
    let applied_limits = mock.working_set_of(1234).expect("applied limits");
    assert_eq!(applied_limits.min_bytes, 256 * 1024 * 1024);
    assert_eq!(applied_limits.max_bytes, WorkingSetLimits::NO_UPPER_BOUND);

    // 回滚：工作集步骤现在是可执行动作，而不是 not_actionable。
    let rolled = session.rollback(RollbackOptions::all()).expect("rollback");
    assert_eq!(rolled.not_actionable, 0);
    let step = rolled
        .steps
        .iter()
        .find(|step| {
            matches!(
                step.action,
                gopt_journal::RollbackAction::RestoreWorkingSet { .. }
            )
        })
        .expect("the working-set record is in the plan");
    assert!(step.actionable, "the previous limits are known");
    assert_eq!(
        mock.working_set_of(1234),
        Some(before),
        "byte for byte back to the previous limits"
    );
    assert!(rolled.all_ok(), "rollback failed: {rolled:?}");
}

#[test]
fn working_set_without_a_readable_before_is_still_honest() {
    let dir = TempDir::new("working-set-no-before");
    let (mock, session) = session(dir.path());
    mock.fail_next(
        HalOp::GetWorkingSet,
        HalError::access_denied("GetProcessWorkingSetSize", "denied by the test"),
    );

    let plan = session.plan("cs2", None).expect("plan").plan;
    let applied = session.apply(&plan, ApplyOptions::commit()).expect("apply");
    let working_set = applied
        .steps
        .iter()
        .find(|step| step.hal_op == HalOp::SetWorkingSet)
        .expect("a working-set step");

    // 读不到前值：不写假的 before、不假装可回滚，但写入照常进行（降级哲学）。
    assert_eq!(working_set.status, StepStatus::Applied);
    assert!(!working_set.reversible);
    assert!(working_set.before.is_none());
    let note = working_set
        .note
        .as_ref()
        .expect("the boundary is explained");
    assert!(note.zh.contains("读取写入前的工作集失败"), "{}", note.zh);

    // 回滚计划把它明确报成 not_actionable，并给出"需要人工确认"的提示。
    let rolled = session.rollback(RollbackOptions::all()).expect("rollback");
    assert_eq!(rolled.not_actionable, 1);
    let step = rolled
        .steps
        .iter()
        .find(|step| step.status == StepStatus::NotActionable)
        .expect("the working-set record is reported");
    assert!(matches!(
        step.action,
        gopt_journal::RollbackAction::NotActionable { .. }
    ));
    assert!(rolled
        .notices
        .iter()
        .any(|notice| notice.zh.contains("人工确认")));
}

#[test]
fn legacy_import_then_rollback() {
    let dir = TempDir::new("legacy");
    // C++ v1.1.0 的 savepoints.txt 行：12 个 `|` 分隔字段（见 gopt-journal/README.md §6）。
    std::fs::write(
        dir.path().join("savepoints.txt"),
        "38760|32|65535|204800|1413120|1|1|1|8c5e7fda-e8bf-4a96-9a85-a6e2638c635c|1700000000000|三角洲行动|before optimization\n",
    )
    .expect("write savepoints");

    let mock = Arc::new(MockApi::sample_workstation());
    mock.push_process(38760, "三角洲行动.exe", 12);
    let api: Arc<dyn SystemApi> = mock.clone();
    let session = Session::new(api, DataPaths::new(dir.path()), Lang::Zh).expect("session");

    let dry = session
        .import_legacy(LegacyImportOptions {
            dry_run: true,
            ..LegacyImportOptions::default()
        })
        .expect("dry import");
    assert!(dry.drafts >= 1);
    assert_eq!(dry.imported, 0);
    assert!(!dir.journal().exists());

    let imported = session
        .import_legacy(LegacyImportOptions {
            dry_run: false,
            ..LegacyImportOptions::default()
        })
        .expect("import");
    assert_eq!(imported.imported, dry.drafts);
    assert!(imported.savepoints.exists);
    assert_eq!(imported.savepoints.valid_lines, 1);
    assert_eq!(imported.savepoints.bad_lines, 0);

    // 导入的记录是 `imported`，且带上"这是旧文件、需人工确认"的说明。
    let view = session
        .journal_view(JournalFilter {
            kind: Some(JournalKind::Imported),
            limit: None,
        })
        .expect("view");
    assert_eq!(view.entries.len(), imported.imported);
    assert!(view.chain.is_ok());

    // 撤销：旧快照被翻成 RestoreLegacySnapshot 并真的作用到系统上。
    mock.set_priority(38760, PriorityClass::High)
        .expect("simulate the C++ optimization");
    let rolled = session.rollback(RollbackOptions::all()).expect("rollback");
    assert!(rolled.all_ok(), "{rolled:?}");
    assert!(rolled.executed >= 1);
    assert_eq!(
        mock.priority_of(38760),
        Some(PriorityClass::Normal),
        "the legacy snapshot's priorityClass 32 = NORMAL was restored"
    );

    // 旧文件全程只读：内容没被改写。
    let text = std::fs::read_to_string(dir.path().join("savepoints.txt")).expect("read back");
    assert!(text.starts_with("38760|32|65535|"));
}

#[test]
fn watch_applies_once_per_process() {
    let dir = TempDir::new("watch");
    let (mock, session) = session(dir.path());
    let mut state = WatchState::default();
    let options = WatchOptions {
        interval_secs: 1,
        duration_secs: 0,
        once: true,
        apply: true,
    };

    let first = session.watch_tick(&mut state, options).expect("tick");
    let cs2 = first
        .items
        .iter()
        .find(|item| item.game_id == "cs2")
        .expect("cs2 was matched");
    assert_eq!(cs2.action, "applied");
    assert_eq!(cs2.applied, 3);

    let second = session
        .watch_tick(&mut state, options)
        .expect("second tick");
    let cs2 = second
        .items
        .iter()
        .find(|item| item.game_id == "cs2")
        .expect("cs2 was matched");
    assert_eq!(cs2.action, "already_optimized", "no repeated optimization");
    assert_eq!(second.tick, 2);
    assert_eq!(mock.call_count(HalOp::SetPriority), 1);
}

#[test]
fn json_output_is_parseable_for_every_report() {
    // `--json` 可被 serde_json 解析：这里逐类报告断言"能序列化成对象且 schema 版本正确"。
    let dir = TempDir::new("json");
    let (_mock, session) = session(dir.path());
    let plan = session.plan("cs2", None).expect("plan");
    let applied = session
        .apply(&plan.plan, ApplyOptions::commit())
        .expect("apply");
    let view = session
        .journal_view(JournalFilter {
            kind: None,
            limit: None,
        })
        .expect("view");
    let verified = session
        .verify_journal(gopt_core::VerifyOptions::default())
        .expect("verify");
    let status = session.status().expect("status");
    let report = session.report().expect("report");
    let explain = session.explain_game("cs2").expect("explain");

    let values = [
        serde_json::to_value(&plan).expect("plan json"),
        serde_json::to_value(&applied).expect("apply json"),
        serde_json::to_value(&view).expect("journal json"),
        serde_json::to_value(&verified).expect("verify json"),
        serde_json::to_value(&status).expect("status json"),
        serde_json::to_value(&report).expect("report json"),
        serde_json::to_value(&explain).expect("explain json"),
    ];
    for value in values {
        assert!(
            value.is_object(),
            "every report must serialise to a JSON object"
        );
    }

    let outcome = gopt_core::Outcome::ok("status", Lang::En, status);
    let parsed: serde_json::Value =
        serde_json::from_str(&outcome.to_json_pretty()).expect("the outcome must be valid JSON");
    assert_eq!(parsed["schema_version"], gopt_core::SCHEMA_VERSION);
    assert_eq!(parsed["command"], "status");
    assert_eq!(parsed["ok"], true);
}
