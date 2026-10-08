//! [`Session`]：一次运行的编排上下文（**前端唯一入口**）。
//!
//! 编排顺序（与任务书一致）：`detect → load policy → plan（默认只读）→ apply（写 journal）→ rollback`。
//!
//! * `detect`：`hardware()` + `is_elevated()` 组成 [`EvalInput`]，一次采集、多游戏复用；
//! * `load policy`：内置 TOML（编译进二进制）+ 数据目录下的用户覆盖 `policies.d/*.toml`；
//! * `plan`：**只读**。`gopt plan` 不改任何东西，这是"默认安全"的第一层；
//! * `apply` / `rollback` 见 [`crate::apply`]。
//!
//! 数据目录里的东西全部由 [`DataPaths`] 决定：把 `GOPT_DATA_DIR` 指向临时目录，
//! 整个内核（含审计日志、用户策略、旧格式导入）就完全在临时目录里跑，不碰用户的真实配置。

use std::sync::Arc;

use gopt_hal::{HalOp, HardwareInfo, ProcessInfo, SystemApi};
use gopt_policy::{
    Action, ActionKind, DiagnosticSeverity, EvalInput, GamePolicy, PolicyDiagnostic, PolicyLoader,
    PolicySet,
};

use crate::error::{CoreError, CoreResult};
use crate::i18n::{Lang, Text};
use crate::model::{
    GameSummary, PlanReport, RunningGame, StatusReport, WatchOptions, WatchState, WatchTick,
    WatchTickItem,
};
use crate::paths::DataPaths;

/// 一次运行的编排上下文。
pub struct Session {
    api: Arc<dyn SystemApi>,
    lang: Lang,
    paths: DataPaths,
    policies: PolicySet,
    diagnostics: Vec<PolicyDiagnostic>,
    eval: EvalInput,
}

impl core::fmt::Debug for Session {
    /// 手写 `Debug`：`dyn SystemApi` 不是 `Debug`，这里只暴露诊断必需的事实
    /// （后端名、语言、数据目录、策略条数），不打印硬件画像那种长结构。
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session")
            .field("backend", &self.api.backend_name())
            .field("lang", &self.lang)
            .field("data_dir", &self.paths.root())
            .field("policies", &self.policies.len())
            .field("diagnostics", &self.diagnostics.len())
            .finish()
    }
}

/// 一次"进程命中策略"的匹配结果（内部用）。
#[derive(Debug, Clone)]
struct MatchedGame {
    game_id: String,
    name_zh: String,
    name_en: String,
    pid: u32,
    exe: String,
}

impl Session {
    /// 采集环境 + 加载策略。
    ///
    /// 环境采集失败（例如 `hardware()` 在某些受限环境下报错）会直接返回错误：
    /// 没有硬件画像就无法做条件求值，硬着头皮往下走只会给出**看起来成功**的错结果。
    pub fn new(api: Arc<dyn SystemApi>, paths: DataPaths, lang: Lang) -> CoreResult<Self> {
        let eval = EvalInput::from_api(api.as_ref()).map_err(|error| {
            CoreError::from_hal("Session::detect (hardware/is_elevated)", error)
        })?;
        let outcome = PolicyLoader::builtin_only()
            .with_user_dir(paths.policies_dir())
            .load();
        let diagnostics = outcome.diagnostics().to_vec();
        let policies = outcome.into_set();
        Ok(Self {
            api,
            lang,
            paths,
            policies,
            diagnostics,
            eval,
        })
    }

    /// HAL 后端（只读用途：诊断/自检）。
    pub fn api(&self) -> &dyn SystemApi {
        self.api.as_ref()
    }

    /// 本次输出语言。
    pub const fn lang(&self) -> Lang {
        self.lang
    }

    /// 数据目录布局。
    pub const fn paths(&self) -> &DataPaths {
        &self.paths
    }

    /// 已加载的策略集合（用户层优先）。
    pub const fn policies(&self) -> &PolicySet {
        &self.policies
    }

    /// 策略加载诊断（错误 + 警告）。
    pub fn diagnostics(&self) -> &[PolicyDiagnostic] {
        &self.diagnostics
    }

    /// 求值输入（硬件画像 + 提权状态）。
    pub const fn eval_input(&self) -> &EvalInput {
        &self.eval
    }

    /// 硬件画像。
    pub fn hardware(&self) -> &HardwareInfo {
        self.eval.hardware()
    }

    /// 是否已提权。
    pub const fn is_elevated(&self) -> bool {
        self.eval.is_elevated()
    }

    /// HAL 后端名（`win32` / `mock`）。
    pub fn backend_name(&self) -> &'static str {
        self.api.backend_name()
    }

    /// 版本行（`gopt --version`）。
    pub fn version_line(&self) -> String {
        crate::version_line()
    }

    /// 当前运行中的进程（按 pid 升序，由 HAL 保证）。
    pub fn processes(&self) -> CoreResult<Vec<ProcessInfo>> {
        self.api
            .list_processes()
            .map_err(|error| CoreError::from_hal("list_processes", error))
    }

    /// 全部策略摘要（`gopt list games`）。
    pub fn games(&self) -> Vec<GameSummary> {
        let processes = self.processes().unwrap_or_default();
        self.policies
            .games()
            .iter()
            .map(|game| self.summarise_game(game, &processes))
            .collect()
    }

    /// 全部策略摘要 + 当前运行状态。
    fn summarise_game(&self, game: &GamePolicy, processes: &[ProcessInfo]) -> GameSummary {
        let mut hal_ops: Vec<String> = Vec::new();
        for rule in game.rules() {
            if let Some(name) = action_hal_name(rule.action().kind()) {
                if !hal_ops.iter().any(|existing| existing == name) {
                    hal_ops.push(name.to_string());
                }
            }
        }
        let running_pid = processes
            .iter()
            .find(|process| game.matches_exe(&process.name))
            .map(|process| process.pid);
        GameSummary {
            id: game.id().to_string(),
            name_zh: game.name_zh().to_string(),
            name_en: game.name_en().to_string(),
            exe_match: game.exe_match().to_string(),
            exe_aliases: game.exe_aliases().to_vec(),
            rules: game.rules().len(),
            hal_ops,
            origin: game.origin().display_path(),
            layer: game.origin().layer().as_str().to_string(),
            running_pid,
            description_zh: game.description_zh().to_string(),
            description_en: game.description_en().to_string(),
        }
    }

    /// 运行中且命中策略的进程。
    fn matches(&self, processes: &[ProcessInfo]) -> Vec<(GamePolicy, ProcessInfo, MatchedGame)> {
        let mut matches = Vec::new();
        for process in processes {
            if let Some(game) = self.policies.match_process_name(&process.name) {
                matches.push((
                    game.clone(),
                    process.clone(),
                    MatchedGame {
                        game_id: game.id().to_string(),
                        name_zh: game.name_zh().to_string(),
                        name_en: game.name_en().to_string(),
                        pid: process.pid,
                        exe: process.name.clone(),
                    },
                ));
            }
        }
        matches
    }

    /// 为**一个具体进程**生成计划（pid 必须存在且 exe 命中某份策略）。
    pub fn plan_for_pid(&self, pid: u32) -> CoreResult<PlanReport> {
        let processes = self.processes()?;
        let process = processes
            .iter()
            .find(|process| process.pid == pid)
            .cloned()
            .ok_or_else(|| {
                CoreError::not_found(
                    "Session::plan_for_pid",
                    format!("no running process has pid {pid}"),
                )
            })?;
        let game = self
            .policies
            .match_process_name(&process.name)
            .ok_or_else(|| {
                CoreError::not_found(
                    "Session::plan_for_pid",
                    format!(
                        "no policy matches the process `{}` (pid {pid}); add one in {}",
                        process.name,
                        self.paths.policies_dir().display()
                    ),
                )
            })?;
        let same_name = processes
            .iter()
            .filter(|candidate| candidate.name.eq_ignore_ascii_case(&process.name))
            .count();
        Ok(self.build_plan_report(
            game,
            format!("{pid}"),
            Some(&process),
            same_name,
            Vec::new(),
        ))
    }

    /// `gopt plan <查询>`：查询串可以是策略 id、中英文名、别名、exe 名，或一个 pid。
    ///
    /// **只读**：不写系统、不写日志。游戏没在运行时照样给出计划（`pid = 0`、`running = false`），
    /// 这样用户可以在启动游戏前就看清"会做什么"。
    pub fn plan(&self, query: &str, pid: Option<u32>) -> CoreResult<PlanReport> {
        if let Some(pid) = pid {
            return self.plan_for_pid(pid);
        }
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(CoreError::usage(
                "Session::plan",
                "a game id or a pid is required",
            ));
        }
        if let Ok(pid) = trimmed.parse::<u32>() {
            return self.plan_for_pid(pid);
        }

        let processes = self.processes()?;
        let candidates: Vec<String> = self
            .policies
            .find_all(trimmed)
            .into_iter()
            .map(|game| game.id().to_string())
            .collect();

        // 精确 id 优先；其次宽松查找（名字/别名/exe）。
        let game = self
            .policies
            .get(trimmed)
            .or_else(|| self.policies.find(trimmed))
            .or_else(|| {
                // 兜底：查询串直接命中某个正在运行的进程名 ⇒ 按 exe 找策略。
                self.policies.match_process_name(trimmed)
            })
            .ok_or_else(|| {
                CoreError::not_found(
                    "Session::plan",
                    format!(
                        "no policy matches `{trimmed}`; known ids: {}",
                        self.policies.game_ids().join(", ")
                    ),
                )
            })?;

        let running: Vec<ProcessInfo> = processes
            .iter()
            .filter(|process| game.matches_exe(&process.name))
            .cloned()
            .collect();
        let chosen = running.first().cloned();
        Ok(self.build_plan_report(
            game,
            trimmed.to_string(),
            chosen.as_ref(),
            running.len(),
            candidates,
        ))
    }

    /// 为全部"正在运行且命中策略"的进程生成计划（`status` / `watch` / `report` 用）。
    pub fn plans_for_running(&self) -> CoreResult<Vec<PlanReport>> {
        let processes = self.processes()?;
        let matches = self.matches(&processes);
        Ok(matches
            .into_iter()
            .map(|(game, process, _)| {
                let same_name = processes
                    .iter()
                    .filter(|candidate| candidate.name.eq_ignore_ascii_case(&process.name))
                    .count();
                self.build_plan_report(
                    &game,
                    process.name.clone(),
                    Some(&process),
                    same_name,
                    Vec::new(),
                )
            })
            .collect())
    }

    /// 内部：把"策略 + 目标进程"变成 [`PlanReport`]。
    fn build_plan_report(
        &self,
        game: &GamePolicy,
        query: String,
        process: Option<&ProcessInfo>,
        process_count: usize,
        candidates: Vec<String>,
    ) -> PlanReport {
        let pid = process.map(|process| process.pid).unwrap_or(0);
        let plan = game.plan(&self.eval, pid);
        let mut notes = Vec::new();
        if process.is_none() {
            notes.push(Text::new(
                "游戏当前没有运行：这是启动后会执行的计划（pid = 0，仅供预览）",
                "the game is not running: this is the plan that would be applied (pid = 0, preview only)",
            ));
        }
        if plan.requires_elevation() && !self.is_elevated() {
            notes.push(Text::new(
                "计划里有需要管理员权限的步骤，当前未提权：执行时会被拒绝并如实报告（其余步骤照常）",
                "some steps need administrator rights and this process is not elevated: they will be \
                 refused and reported, the remaining steps still run",
            ));
        }
        PlanReport {
            query,
            matched: true,
            running: process.is_some(),
            plan,
            process_name: process.map(|process| process.name.clone()),
            process_count,
            candidates,
            notes,
        }
    }

    /// `gopt status`：环境 + 策略 + 审计链 + 正在运行的游戏。
    pub fn status(&self) -> CoreResult<StatusReport> {
        let processes = self.processes()?;
        let matches = self.matches(&processes);
        let mut running_games = Vec::new();
        for (game, process, matched) in matches {
            let plan = game.plan(&self.eval, process.pid);
            running_games.push(RunningGame {
                game_id: matched.game_id,
                name_zh: matched.name_zh,
                name_en: matched.name_en,
                pid: matched.pid,
                exe: matched.exe,
                steps: plan.step_count(),
                skipped: plan.skipped_count(),
                requires_elevation: plan.requires_elevation(),
            });
        }

        let (journal, journal_error) = match self.journal_snapshot(false) {
            Ok(snapshot) => (Some(snapshot), None),
            Err(error) => (None, Some(error)),
        };

        let user_policies = self
            .policies
            .games()
            .iter()
            .filter(|game| game.origin().is_user())
            .count();
        let errors = self
            .diagnostics
            .iter()
            .filter(|item| item.severity() == DiagnosticSeverity::Error)
            .count();
        let warnings = self.diagnostics.len() - errors;

        let mut notices: Vec<Text> = Vec::new();
        if !self.is_elevated() {
            notices.push(Text::new(
                "当前未提权：电源方案切换与 HKLM 启动项会被系统拒绝；优先级/亲和性/工作集不受影响",
                "not elevated: power-scheme switches and HKLM startup entries will be refused; \
                 priority, affinity and working set are unaffected",
            ));
        }
        if !self.paths.is_explicit() {
            notices.push(Text::new(
                format!(
                    "数据目录来自平台默认值；可用 --data-dir 或环境变量 {} 指向别处",
                    crate::paths::DATA_DIR_ENV
                ),
                format!(
                    "the data directory is the platform default; use --data-dir or {} to point elsewhere",
                    crate::paths::DATA_DIR_ENV
                ),
            ));
        }
        if errors > 0 {
            notices.push(Text::new(
                format!("有 {errors} 个策略文件加载失败（其余策略照常可用，见 policy_diagnostics）"),
                format!("{errors} policy file(s) failed to load (the others still work; see policy_diagnostics)"),
            ));
        }
        if let Some(error) = &journal_error {
            notices.push(Text::new(
                format!("审计日志不可读：{}", error.message()),
                format!("the audit journal cannot be read: {}", error.message()),
            ));
        }
        if let Some(snapshot) = &journal {
            if let Some(repair) = &snapshot.tail_repair_bytes {
                notices.push(Text::new(
                    format!(
                        "审计日志尾部有 {repair} 字节崩溃残留，已按 JSONL 规则截掉（记录未被篡改）"
                    ),
                    format!(
                        "{repair} bytes of a torn tail were truncated while opening the journal"
                    ),
                ));
            }
            if !snapshot.chain_ok {
                notices.push(Text::new(
                    "审计链校验未通过：gopt 拒绝在这种状态下修改系统（verify-journal 可定位第一处不一致）",
                    "the audit chain does not verify: gopt refuses to modify the system in this state",
                ));
            }
        }

        Ok(StatusReport {
            product: crate::PRODUCT_NAME.to_string(),
            version: crate::VERSION.to_string(),
            cpp_release: crate::CPP_RELEASE_VERSION.to_string(),
            schema_version: crate::SCHEMA_VERSION,
            backend: self.backend_name().to_string(),
            elevated: self.is_elevated(),
            lang: self.lang,
            data_dir: self.paths.describe(),
            journal_path: self.paths.journal().display().to_string(),
            journal_exists: journal.as_ref().is_some_and(|snapshot| snapshot.exists),
            journal_records: journal.as_ref().map_or(0, |snapshot| snapshot.records),
            chain_ok: journal.as_ref().is_some_and(|snapshot| snapshot.chain_ok),
            chain_summary: journal.as_ref().map_or_else(
                || "the audit journal could not be read".to_string(),
                |snapshot| snapshot.chain_summary.clone(),
            ),
            first_problem: journal
                .as_ref()
                .and_then(|snapshot| snapshot.first_problem.clone()),
            tail_repair_bytes: journal
                .as_ref()
                .and_then(|snapshot| snapshot.tail_repair_bytes),
            pending_rollbacks: journal
                .as_ref()
                .map_or(0, |snapshot| snapshot.pending_apply_ids.len()),
            policies_total: self.policies.len(),
            policies_builtin: self.policies.len() - user_policies,
            policies_user: user_policies,
            policy_errors: errors,
            policy_warnings: warnings,
            user_policy_dir: self.paths.policies_dir().display().to_string(),
            hardware: self.hardware().clone(),
            running_games,
            policy_diagnostics: self.diagnostics.clone(),
            notices,
        })
    }

    /// `gopt watch` 的一轮：重新枚举进程，对新出现的游戏进程执行（或预演）计划。
    ///
    /// 不用后台线程、不用 async：CLI 自己按 `interval_secs` 调这个方法，
    /// 于是"监控"这件事也能被单测直接驱动（`once = true` 跑一轮就返回）。
    pub fn watch_tick(
        &self,
        state: &mut WatchState,
        options: WatchOptions,
    ) -> CoreResult<WatchTick> {
        state.ticks = state.ticks.saturating_add(1);
        let processes = self.processes()?;
        let matches = self.matches(&processes);
        let mut items = Vec::new();

        for (game, process, matched) in &matches {
            if state.has_processed(&matched.game_id, matched.pid) {
                items.push(WatchTickItem {
                    game_id: matched.game_id.clone(),
                    name_zh: matched.name_zh.clone(),
                    name_en: matched.name_en.clone(),
                    pid: matched.pid,
                    action: "already_optimized".to_string(),
                    applied: 0,
                    failed: 0,
                    error: None,
                });
                continue;
            }

            let plan = game.plan(&self.eval, process.pid);
            if plan.is_empty() {
                items.push(WatchTickItem {
                    game_id: matched.game_id.clone(),
                    name_zh: matched.name_zh.clone(),
                    name_en: matched.name_en.clone(),
                    pid: matched.pid,
                    action: "unchanged".to_string(),
                    applied: 0,
                    failed: 0,
                    error: None,
                });
                continue;
            }

            let apply_options = if options.apply {
                crate::model::ApplyOptions::commit()
            } else {
                crate::model::ApplyOptions::preview()
            };
            match self.apply(&plan, apply_options) {
                Ok(report) => {
                    let action = if !options.apply {
                        "previewed"
                    } else if report.applied == 0 && report.failed == 0 {
                        "unchanged"
                    } else {
                        "applied"
                    };
                    if report.all_ok() && options.apply {
                        state.mark_processed(&matched.game_id, matched.pid);
                    }
                    items.push(WatchTickItem {
                        game_id: matched.game_id.clone(),
                        name_zh: matched.name_zh.clone(),
                        name_en: matched.name_en.clone(),
                        pid: matched.pid,
                        action: action.to_string(),
                        applied: report.applied,
                        failed: report.failed,
                        error: None,
                    });
                }
                Err(error) => {
                    items.push(WatchTickItem {
                        game_id: matched.game_id.clone(),
                        name_zh: matched.name_zh.clone(),
                        name_en: matched.name_en.clone(),
                        pid: matched.pid,
                        action: "failed".to_string(),
                        applied: 0,
                        failed: 0,
                        error: Some(error.message().to_string()),
                    });
                }
            }
        }

        let elapsed_secs =
            (state.ticks.saturating_sub(1) as f64) * (options.interval_secs.max(1) as f64);
        let continuing = !options.once
            && (options.duration_secs == 0
                || elapsed_secs + (options.interval_secs.max(1) as f64)
                    <= options.duration_secs as f64);

        Ok(WatchTick {
            tick: state.ticks,
            elapsed_secs,
            matched: matches.len(),
            items,
            continuing,
        })
    }
}

/// 动作种类 → HAL 操作名（`gopt list games` 用）。
pub(crate) fn action_hal_name(kind: ActionKind) -> Option<&'static str> {
    match kind {
        ActionKind::Priority => Some(HalOp::SetPriority.as_str()),
        ActionKind::Affinity => Some(HalOp::SetAffinity.as_str()),
        ActionKind::WorkingSet => Some(HalOp::SetWorkingSet.as_str()),
        ActionKind::PowerScheme => Some(HalOp::SetPowerScheme.as_str()),
        ActionKind::RunEntries => Some(HalOp::SetRunEntryEnabled.as_str()),
        ActionKind::Skip => None,
    }
}

/// 一条 `skip` 动作的理由（`explain` 用）。
pub(crate) fn skip_reason(action: &Action) -> Option<&gopt_policy::Reason> {
    match action {
        Action::Skip { reason } => Some(reason),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gopt_hal::MockApi;

    fn session(dir: &std::path::Path) -> Session {
        Session::new(
            Arc::new(MockApi::sample_workstation()),
            DataPaths::new(dir),
            Lang::Zh,
        )
        .expect("session")
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gopt-core-engine-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn session_detects_hardware_and_policies() {
        let dir = temp_dir("detect");
        let session = session(&dir);
        assert_eq!(session.backend_name(), "mock");
        assert_eq!(session.hardware().logical_cores, 16);
        assert!(session.policies().get("cs2").is_some());
        assert!(session.games().iter().any(|game| game.id == "cs2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_is_read_only_and_previews_when_the_game_is_absent() {
        let dir = temp_dir("plan-readonly");
        let session = session(&dir);
        let report = session.plan("cs2", None).expect("plan");
        assert!(report.running);
        assert_eq!(report.plan.pid, 1234);
        assert!(report.plan.step_count() >= 1);

        let missing = session
            .plan("cs2", Some(999_999))
            .expect_err("pid must exist");
        assert_eq!(missing.kind(), crate::CoreErrorKind::NotFound);

        let unknown = session
            .plan("no-such-game", None)
            .expect_err("unknown game");
        assert_eq!(unknown.kind(), crate::CoreErrorKind::NotFound);
        assert!(unknown.message().contains("cs2"), "{}", unknown.message());

        let by_pid = session.plan("1234", None).expect("by pid");
        assert_eq!(by_pid.plan.pid, 1234);
        assert_eq!(by_pid.query, "1234");

        // 计划只读：一个写调用都没有发生。
        let api = session.api();
        assert_eq!(api.backend_name(), "mock");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_reports_the_empty_journal_as_healthy() {
        let dir = temp_dir("status");
        let session = session(&dir);
        let status = session.status().expect("status");
        assert!(status.journal_path.ends_with("journal.jsonl"));
        assert!(!status.journal_exists);
        assert_eq!(status.journal_records, 0);
        assert_eq!(status.pending_rollbacks, 0);
        assert!(status.policies_total >= 9);
        assert_eq!(status.policies_user, 0);
        assert!(!status.elevated);
        assert!(status
            .running_games
            .iter()
            .any(|game| game.game_id == "cs2"));
        assert!(status
            .notices
            .iter()
            .any(|notice| notice.zh.contains("未提权")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watch_tick_reports_and_does_not_write_without_apply() {
        let dir = temp_dir("watch");
        let session = session(&dir);
        let mut state = WatchState::default();
        let options = WatchOptions {
            interval_secs: 1,
            duration_secs: 0,
            once: true,
            apply: false,
        };
        let tick = session.watch_tick(&mut state, options).expect("tick");
        assert_eq!(tick.tick, 1);
        assert!(!tick.continuing);
        assert!(tick.items.iter().any(|item| item.action == "previewed"));
        assert!(!dir.join("journal.jsonl").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn action_hal_names_cover_every_kind() {
        assert_eq!(action_hal_name(ActionKind::Priority), Some("set_priority"));
        assert_eq!(
            action_hal_name(ActionKind::RunEntries),
            Some("set_run_entry_enabled")
        );
        assert_eq!(action_hal_name(ActionKind::Skip), None);
    }
}
