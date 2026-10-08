//! 只读视图：`journal` / `verify-journal` / `explain` / `report` / `import-legacy`。
//!
//! 这一整个模块**不修改系统、也不修改日志**（`import-legacy` 例外：它只在 `--yes` 时把旧格式
//! 的历史条目追加进审计链，且旧文件永远只读）。因此它可以安全地用在诊断、CI 和用户排查里。
//!
//! # 审计链校验的诚实边界
//!
//! `verify-journal` 会区分三种结论：
//!
//! * [`VerifyStatus::Ok`]：链完好；
//! * [`VerifyStatus::Recoverable`]：**只有**最后一行是"崩溃残留的半行"（进程在 `write_all`
//!   中途被杀），已提交的记录一条没被改 —— 退出码仍是 0，但会给出明确的修复提示；
//!   `--strict` 可以把它按失败处理（CI 里更安全）；
//! * [`VerifyStatus::Broken`]：其它任何不一致（改字段、删中间行、改链首、重新格式化、锚点不符）
//!   ⇒ 退出码 3，并且**拒绝**在这个状态下修改系统。

use gopt_journal::{
    plan_rollback_pending, undone_apply_ids, ChainProblem, ChainReport, Journal, JournalRecord,
    RollbackStep,
};
use gopt_policy::{Action, GamePolicy, Rule};

use crate::engine::{action_hal_name, skip_reason};
use crate::error::{CoreError, CoreResult};
use crate::i18n::Text;
use crate::model::{
    ExplainReport, GameExplanation, ImportReport, JournalEntryView, JournalExplanation,
    JournalFilter, JournalView, LegacyImportOptions, ReportData, RuleExplanation, SourceSummary,
    StatusReport, VerifyOptions, VerifyReport, VerifyStatus,
};
use crate::Session;

/// 审计日志的一次快照（内部用；`status` / `journal` / `report` 共用）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JournalSnapshot {
    /// 日志路径。
    pub path: String,
    /// 文件是否存在。
    pub exists: bool,
    /// 已提交记录数。
    pub records: usize,
    /// 无法解析的行数。
    pub malformed_lines: usize,
    /// 跳过的空行数。
    pub skipped_blank_lines: usize,
    /// 链是否完好。
    pub chain_ok: bool,
    /// 链校验摘要。
    pub chain_summary: String,
    /// 第一处不一致。
    pub first_problem: Option<gopt_journal::ChainBreak>,
    /// 打开时修复的尾部残字节数。
    pub tail_repair_bytes: Option<u64>,
    /// 待撤销的 apply 记录 id（升序）。
    pub pending_apply_ids: Vec<u64>,
    /// 已撤销的 apply 记录 id（升序）。
    pub undone_apply_ids: Vec<u64>,
    /// 全部记录的展示视图（按 id 升序）。
    pub entries: Vec<JournalEntryView>,
    /// 链校验报告。
    pub chain: ChainReport,
}

impl Session {
    /// 读取审计日志快照（`writable = false` 时不改日志一个字节）。
    pub(crate) fn journal_snapshot(&self, writable: bool) -> CoreResult<JournalSnapshot> {
        let journal = self.open_journal(writable)?;
        Ok(snapshot_of(&journal))
    }

    /// `gopt journal`：按过滤条件列出审计记录。
    pub fn journal_view(&self, filter: JournalFilter) -> CoreResult<JournalView> {
        let journal = self.open_journal(false)?;
        let snapshot = snapshot_of(&journal);
        let mut entries = snapshot.entries.clone();
        if let Some(kind) = filter.kind {
            entries.retain(|entry| entry.kind == kind);
        }
        let matched = entries.len();
        if let Some(limit) = filter.limit {
            if limit < matched {
                entries.drain(..matched - limit);
            }
        }
        Ok(JournalView {
            path: snapshot.path,
            exists: snapshot.exists,
            records: snapshot.records,
            malformed_lines: snapshot.malformed_lines,
            skipped_blank_lines: snapshot.skipped_blank_lines,
            entries,
            chain: snapshot.chain,
            tail_repair: journal.tail_repair().cloned(),
            pending_apply_ids: snapshot.pending_apply_ids,
            undone_apply_ids: snapshot.undone_apply_ids,
        })
    }

    /// `gopt verify-journal`：校验哈希链（可带外部锚点）。
    pub fn verify_journal(&self, options: VerifyOptions) -> CoreResult<VerifyReport> {
        let journal = self.open_journal(false)?;
        let report = match &options.anchor {
            Some(anchor) => journal.verify_chain_with_anchor(anchor),
            None => journal.verify_chain(),
        };
        let repairable = is_repairable_torn_tail(&journal, &report);
        let status = if report.is_ok() {
            VerifyStatus::Ok
        } else if repairable && !options.strict {
            VerifyStatus::Recoverable
        } else {
            VerifyStatus::Broken
        };
        let records = journal.records_owned();
        let pending_rollbacks = plan_rollback_pending(&records).map_or(0, |plan| plan.len());

        Ok(VerifyReport {
            path: journal.path().display().to_string(),
            exists: journal.file_exists(),
            status,
            total: report.total,
            verified: report.verified,
            records: records.len(),
            malformed_lines: journal.malformed_lines().count(),
            anchored: report.anchored,
            first_break: report.first_inconsistency.clone(),
            tail_repair: journal.tail_repair().cloned(),
            repairable,
            summary: report.summary(),
            pending_rollbacks,
        })
    }

    /// `gopt explain --game <id>`：这款游戏为什么这么优化（逐规则的条件与动作）。
    pub fn explain_game(&self, query: &str) -> CoreResult<ExplainReport> {
        let game = self
            .policies()
            .get(query)
            .or_else(|| self.policies().find(query))
            .ok_or_else(|| {
                CoreError::not_found(
                    "Session::explain_game",
                    format!(
                        "no policy matches `{query}`; known ids: {}",
                        self.policies().game_ids().join(", ")
                    ),
                )
            })?;
        let rules = game
            .rules()
            .iter()
            .map(|rule| self.explain_rule_of(game, rule))
            .collect();
        Ok(ExplainReport {
            query_kind: "game".to_string(),
            query: query.to_string(),
            game: Some(GameExplanation {
                id: game.id().to_string(),
                name_zh: game.name_zh().to_string(),
                name_en: game.name_en().to_string(),
                origin: game.origin().display_path(),
                layer: game.origin().layer().as_str().to_string(),
                exe_match: game.exe_match().to_string(),
                exe_aliases: game.exe_aliases().to_vec(),
                name_aliases: game.name_aliases().to_vec(),
                description_zh: game.description_zh().to_string(),
                description_en: game.description_en().to_string(),
                rules,
            }),
            rules: Vec::new(),
            journal: None,
            notes: Vec::new(),
        })
    }

    /// `gopt explain --rule <rule_id>`：这条规则在哪些游戏里、什么时候生效。
    pub fn explain_rule(&self, rule_id: &str) -> CoreResult<ExplainReport> {
        let mut rules = Vec::new();
        for game in self.policies().games() {
            for rule in game.rules() {
                if rule.id().eq_ignore_ascii_case(rule_id.trim()) {
                    rules.push(self.explain_rule_of(game, rule));
                }
            }
        }
        if rules.is_empty() {
            return Err(CoreError::not_found(
                "Session::explain_rule",
                format!("no rule has the id `{rule_id}`"),
            ));
        }
        Ok(ExplainReport {
            query_kind: "rule".to_string(),
            query: rule_id.to_string(),
            game: None,
            rules,
            journal: None,
            notes: Vec::new(),
        })
    }

    /// `gopt explain --journal-id <id>`：这条审计记录做了什么、能不能撤、怎么撤。
    pub fn explain_journal(&self, id: u64) -> CoreResult<ExplainReport> {
        let journal = self.open_journal(false)?;
        let snapshot = snapshot_of(&journal);
        let records = journal.records_owned();
        let record: &JournalRecord =
            records
                .iter()
                .find(|record| record.id == id)
                .ok_or_else(|| {
                    CoreError::not_found(
                        "Session::explain_journal",
                        format!("the journal has no record with id {id}"),
                    )
                })?;
        let step = RollbackStep::from_record(record);
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| {
                CoreError::invalid_argument(
                    "Session::explain_journal",
                    format!("record {id} is not a full line"),
                )
            })?;
        Ok(ExplainReport {
            query_kind: "journal".to_string(),
            query: id.to_string(),
            game: None,
            rules: Vec::new(),
            journal: Some(JournalExplanation {
                id: record.id,
                target: record.target.clone(),
                kind: record.kind,
                rule_id: record.rule_id.clone(),
                rollback_description: step.action.describe(),
                actionable: step.action.is_executable(),
                hash_ok: record.has_valid_hash(),
                undone: entry.undone,
                before: record.before.clone(),
                after: record.after.clone(),
                line_no: entry.line_no,
            }),
            notes: Vec::new(),
        })
    }

    /// `gopt report`：把"体检"需要的事实打包（前端负责排版）。
    pub fn report(&self) -> CoreResult<ReportData> {
        let status: StatusReport = self.status()?;
        let games = self.games();
        let processes = self.processes()?;
        let running = status.running_games.clone();
        let (journal_tail, pending) = match self.journal_snapshot(false) {
            Ok(snapshot) => {
                let mut entries = snapshot.entries.clone();
                let keep = 20usize;
                if entries.len() > keep {
                    let drop = entries.len() - keep;
                    entries.drain(..drop);
                }
                (entries, snapshot.pending_apply_ids.len())
            }
            Err(_) => (Vec::new(), 0),
        };
        Ok(ReportData {
            generated_at_unix_ms: gopt_journal::now_unix_ms(),
            status,
            games,
            processes: processes.len(),
            running,
            journal_tail,
            pending_rollbacks: pending,
        })
    }

    /// `gopt import-legacy`：只读解析 C++ v1.1.0 的 `savepoints.txt` / `games.conf`，
    /// `--yes` 时把历史条目以 `kind = imported` 追加进审计链。
    pub fn import_legacy(&self, options: LegacyImportOptions) -> CoreResult<ImportReport> {
        let savepoints = options
            .savepoints
            .clone()
            .unwrap_or_else(|| self.paths().savepoints());
        let games_conf = options
            .games_conf
            .clone()
            .unwrap_or_else(|| self.paths().games_conf());
        let import = gopt_journal::import_legacy(savepoints.as_path(), Some(games_conf.as_path()));
        let drafts = import.drafts().len();

        let mut journal_ids: Vec<u64> = Vec::new();
        if !options.dry_run && drafts > 0 {
            let mut journal = self.open_journal(true)?;
            let report = journal.verify_chain();
            if !report.is_ok() {
                return Err(CoreError::journal_kind_broken(
                    "Journal::verify_chain",
                    format!(
                        "the audit chain in {} does not verify, so the legacy import was not written: {}",
                        self.paths().journal().display(),
                        report.summary()
                    ),
                ));
            }
            let records = journal
                .append_legacy_import(&import)
                .map_err(CoreError::from_journal)?;
            journal_ids = records.iter().map(|record| record.id).collect();
        }

        Ok(ImportReport {
            savepoints: SourceSummary::from_source_report(import.savepoints()),
            games_conf: SourceSummary::from_source_report(import.games_conf()),
            drafts,
            imported: journal_ids.len(),
            policy_filtered: import.policy_filtered(),
            dry_run: options.dry_run,
            journal_ids,
            notes: import.notes(),
            summary: import.summary(),
        })
    }

    /// 内部：把一条规则翻成可解释的三元组（条件 / 动作 / HAL 操作）。
    fn explain_rule_of(&self, game: &GamePolicy, rule: &Rule) -> RuleExplanation {
        let kind = rule.action().kind();
        let (condition_zh, condition_en, met) = match rule.when() {
            Some(condition) => (
                condition.describe_zh(),
                condition.describe_en(),
                Some(condition.evaluate(self.eval_input())),
            ),
            None => (
                "无条件（总是生效）".to_string(),
                "unconditional".to_string(),
                Some(true),
            ),
        };
        let skip_cause = match rule.action() {
            Action::Skip { .. } => Some("explicit_skip".to_string()),
            _ if met == Some(false) => Some("condition_not_met".to_string()),
            _ => None,
        };
        let note = skip_reason(rule.action())
            .map(|reason| Text::new(reason.zh.clone(), reason.en.clone()));
        RuleExplanation {
            game_id: game.id().to_string(),
            rule_id: rule.id().to_string(),
            rule_line: rule.line(),
            condition_zh: match &note {
                Some(note) => format!("{condition_zh}；策略显式跳过：{}", note.zh),
                None => condition_zh,
            },
            condition_en: match &note {
                Some(note) => format!("{condition_en}; explicitly skipped: {}", note.en),
                None => condition_en,
            },
            condition_met: met,
            action_kind: kind.as_str().to_string(),
            action_zh: rule.action().describe_zh(),
            action_en: rule.action().describe_en(),
            hal_op: action_hal_name(kind).map(str::to_string),
            requires_elevation: rule.action().requires_elevation(),
            is_dangerous: rule.action().is_dangerous(),
            skip_cause,
        }
    }
}

/// 由已打开的日志生成快照。
fn snapshot_of(journal: &Journal) -> JournalSnapshot {
    let records = journal.records_owned();
    let undone = undone_apply_ids(&records);
    let pending = plan_rollback_pending(&records)
        .map(|plan| {
            plan.steps
                .iter()
                .map(|step| step.journal_id)
                .collect::<Vec<u64>>()
        })
        .unwrap_or_default();
    let entries = journal
        .lines()
        .iter()
        .filter_map(|line| {
            line.parsed.as_ref().map(|record| {
                JournalEntryView::from_record(record, line.line_no, undone.contains(&record.id))
            })
        })
        .collect();
    let chain = journal.verify_chain();
    JournalSnapshot {
        path: journal.path().display().to_string(),
        exists: journal.file_exists(),
        records: records.len(),
        malformed_lines: journal.malformed_lines().count(),
        skipped_blank_lines: journal.skipped_blank_lines(),
        chain_ok: chain.is_ok(),
        chain_summary: chain.summary(),
        first_problem: chain.first_inconsistency.clone(),
        tail_repair_bytes: journal.tail_repair().map(|repair| repair.bytes_truncated),
        pending_apply_ids: pending,
        undone_apply_ids: undone,
        entries,
        chain,
    }
}

/// 该不一致是否只是"最后一行是未终止的半行"（崩溃残留，可修复）。
fn is_repairable_torn_tail(journal: &Journal, report: &ChainReport) -> bool {
    let Some(first) = report.first() else {
        return false;
    };
    if first.problem != ChainProblem::MalformedLine {
        return false;
    }
    let Some(line) = journal
        .lines()
        .iter()
        .find(|line| line.line_no == first.line_no)
    else {
        return false;
    };
    // 必须是最后一行、且未以换行结尾。
    let is_last = journal.lines().last().map(|last| last.line_no) == Some(line.line_no);
    is_last && !line.terminated && line.parsed.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::model::ApplyOptions;
    use crate::paths::DataPaths;
    use gopt_hal::MockApi;
    use std::sync::Arc;

    fn dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gopt-core-inspect-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn session(dir: &std::path::Path) -> Session {
        Session::new(
            Arc::new(MockApi::sample_workstation()),
            DataPaths::new(dir),
            Lang::Zh,
        )
        .expect("session")
    }

    #[test]
    fn journal_view_lists_records_after_apply() {
        let dir = dir("view");
        let session = session(&dir);
        let plan = session.plan("cs2", None).expect("plan").plan;
        let applied = session.apply(&plan, ApplyOptions::commit()).expect("apply");
        assert!(applied.applied >= 3);

        let view = session
            .journal_view(JournalFilter {
                kind: None,
                limit: None,
            })
            .expect("journal view");
        assert_eq!(view.records, applied.journal_ids.len());
        assert!(view.chain.is_ok());
        assert_eq!(view.pending_apply_ids.len(), applied.journal_ids.len());
        assert!(view.entries.iter().all(|entry| entry.hash_ok));
        assert!(view
            .entries
            .iter()
            .any(|entry| entry.rule_id.as_deref() == Some("policy:game/cs2/priority")));

        let limited = session
            .journal_view(JournalFilter {
                kind: None,
                limit: Some(1),
            })
            .expect("limited");
        assert_eq!(limited.entries.len(), 1);
        assert_eq!(limited.entries[0].id, view.entries.last().expect("last").id);

        let only_apply = session
            .journal_view(JournalFilter {
                kind: Some(gopt_journal::JournalKind::Apply),
                limit: None,
            })
            .expect("apply only");
        assert_eq!(only_apply.entries.len(), view.entries.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_journal_reports_ok_then_broken() {
        let dir = dir("verify");
        let session = session(&dir);
        let plan = session.plan("cs2", None).expect("plan").plan;
        session.apply(&plan, ApplyOptions::commit()).expect("apply");

        let ok = session
            .verify_journal(VerifyOptions::default())
            .expect("verify");
        assert_eq!(ok.status, VerifyStatus::Ok);
        assert_eq!(ok.exit_code(), 0);
        assert!(ok.summary.contains("verified"));

        // 篡改一行：把 priority 改成 realtime 之外的另一个值并保留原哈希 ⇒ record_hash 不一致。
        let path = session.paths().journal();
        let text = std::fs::read_to_string(&path).expect("read");
        let tampered = text.replacen("\"priority\":\"high\"", "\"priority\":\"normal\"", 1);
        assert_ne!(text, tampered);
        std::fs::write(&path, tampered).expect("write");

        let broken = session
            .verify_journal(VerifyOptions::default())
            .expect("verify");
        assert_eq!(broken.status, VerifyStatus::Broken);
        assert_eq!(broken.exit_code(), 3);
        assert!(broken.first_break.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn torn_tail_is_recoverable_unless_strict() {
        let dir = dir("torn");
        let session = session(&dir);
        let plan = session.plan("cs2", None).expect("plan").plan;
        session.apply(&plan, ApplyOptions::commit()).expect("apply");

        let path = session.paths().journal();
        let mut text = std::fs::read_to_string(&path).expect("read");
        text.push_str("{\"id\":99,\"ts_unix_ms\":1,\"kind\":\"apply\"");
        std::fs::write(&path, text).expect("write");

        let lenient = session
            .verify_journal(VerifyOptions::default())
            .expect("verify");
        assert_eq!(lenient.status, VerifyStatus::Recoverable);
        assert!(lenient.repairable);
        assert_eq!(lenient.exit_code(), 0);
        assert_eq!(lenient.malformed_lines, 1);

        let strict = session
            .verify_journal(VerifyOptions {
                anchor: None,
                strict: true,
            })
            .expect("verify");
        assert_eq!(strict.status, VerifyStatus::Broken);
        assert_eq!(strict.exit_code(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explain_answers_game_rule_and_journal_questions() {
        let dir = dir("explain");
        let session = session(&dir);

        let game = session.explain_game("cs2").expect("game");
        let explanation = game.game.expect("game explanation");
        assert_eq!(explanation.id, "cs2");
        assert!(!explanation.rules.is_empty());
        assert!(explanation
            .rules
            .iter()
            .all(|rule| rule.action_kind == "priority"
                || rule.action_kind == "affinity"
                || rule.action_kind == "working-set"
                || rule.action_kind == "power-scheme"
                || rule.action_kind == "run-entries"
                || rule.action_kind == "skip"));
        assert!(explanation.rules.iter().any(|rule| rule.hal_op.is_some()));

        let rule = session.explain_rule("priority").expect("rule");
        assert!(rule.rules.iter().all(|rule| rule.rule_id == "priority"));

        let missing = session.explain_rule("does-not-exist").expect_err("missing");
        assert_eq!(missing.kind(), crate::CoreErrorKind::NotFound);

        let plan = session.plan("cs2", None).expect("plan").plan;
        let applied = session.apply(&plan, ApplyOptions::commit()).expect("apply");
        let id = *applied.journal_ids.first().expect("first journal id");
        let journal = session.explain_journal(id).expect("journal");
        let explanation = journal.journal.expect("journal explanation");
        assert_eq!(explanation.id, id);
        assert!(explanation.hash_ok);
        assert!(
            explanation.actionable,
            "priority records must be reversible"
        );
        assert!(explanation.rollback_description.contains("restore"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_and_import_legacy_are_safe_by_default() {
        let dir = dir("report");
        // 造一个旧的 savepoints.txt（C++ v1.1.0 的 12 字段格式）。
        std::fs::write(
            dir.join("savepoints.txt"),
            "38760|32|65535|204800|1413120|1|1|1|8c5e7fda-e8bf-4a96-9a85-a6e2638c635c|1700000000000|三角洲行动|test\n",
        )
        .expect("write legacy");
        let session = session(&dir);

        let report = session.report().expect("report");
        assert_eq!(report.status.product, "GameOptimizer-RS");
        assert!(report.games.len() >= 9);
        assert!(report.processes >= 3);

        let dry = session
            .import_legacy(LegacyImportOptions {
                dry_run: true,
                ..LegacyImportOptions::default()
            })
            .expect("dry run");
        assert_eq!(dry.imported, 0);
        assert!(dry.drafts >= 1);
        assert!(
            !session.paths().journal().exists(),
            "dry-run must not create the journal"
        );

        let yes = session
            .import_legacy(LegacyImportOptions {
                dry_run: false,
                ..LegacyImportOptions::default()
            })
            .expect("import");
        assert_eq!(yes.imported, dry.drafts);
        let view = session
            .journal_view(JournalFilter {
                kind: Some(gopt_journal::JournalKind::Imported),
                limit: None,
            })
            .expect("view");
        assert_eq!(view.entries.len(), yes.imported);
        assert!(view.chain.is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
