//! 命令实现：**只做三件事**——造 [`Session`]、调内核、选一种渲染。
//!
//! 这里刻意不出现任何 `gopt-hal` / `gopt-journal` 的调用：前端不碰系统、不写日志，
//! 于是"CLI 与 GUI 行为会不一致"这类问题在结构上就不可能出现。
//!
//! # 默认安全
//!
//! `apply` / `rollback` / `tune` / `startup` / `prio --set` / `import-legacy` 在没有 `--yes` 时
//! 一律走**预演**（只读系统、不写日志），退出码仍是 0（"我成功地把计划算给你看了"），
//! `--json` 里的 `dry_run: true` / `applied: 0` 才是机器可判定的信号。

use std::path::PathBuf;
use std::sync::Arc;

use gopt_core::report::{
    render_apply, render_error, render_explain, render_games, render_import, render_journal,
    render_plan, render_prio, render_report, render_rollback, render_startup, render_status,
    render_tune, render_verify, render_watch,
};
use gopt_core::{
    pick, ApplyOptions, ApplyReport, ChainAnchor, CoreError, CoreErrorKind, DataPaths,
    JournalFilter, JournalKind, Lang, LegacyImportOptions, MockApi, Notice, Outcome,
    PowerSchemeChoice, PriorityClass, RollbackOptions, RollbackReport, RunHive, Session,
    StartupReport, StepStatus, SystemApi, VerifyOptions, VerifyReport, VerifyStatus, WatchOptions,
    WatchState, WatchTick, EXIT_OK,
};

use crate::args::{Command, Invocation, ListKind, StartupAction, WatchArgs};

/// 数据目录后端选择：`GOPT_BACKEND=mock` 时用 Mock（测试/CI 用，默认永远是真机后端）。
const BACKEND_ENV: &str = "GOPT_BACKEND";

/// 一次命令的输出（文本 + JSON + 退出码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// 终端文本。
    pub text: String,
    /// `--json` 输出（缩进 JSON，含 `schema_version`）。
    pub json: String,
    /// 进程退出码。
    pub code: i32,
}

impl CommandOutput {
    /// 由 `Outcome` + 文本构造。
    fn finish<T: serde::Serialize>(outcome: Outcome<T>, text: String) -> Self {
        Self {
            code: outcome.exit_code(),
            json: outcome.to_json_pretty(),
            text,
        }
    }

    /// 失败输出（文本 = 错误块 + 建议）。
    pub fn failed(command: &str, lang: Lang, error: CoreError) -> Self {
        let text = render_error(&error, lang);
        let outcome: Outcome<()> = Outcome::failed(command, lang, error);
        Self::finish(outcome, text)
    }

    /// 用法错误输出（退出码 1）。
    pub fn usage(lang: Lang, message: &str, topic: Option<&str>) -> Self {
        let text = format!(
            "{}: {message}\n\n{}",
            pick(lang, "用法错误", "usage error"),
            crate::args::usage(lang, topic)
        );
        let error = CoreError::usage("gopt::parse_args", message);
        let outcome: Outcome<()> = Outcome::failed("usage", lang, error);
        Self::finish(outcome, text)
    }
}

/// 执行一次调用。
pub fn execute(invocation: Invocation) -> CommandOutput {
    let lang = invocation.lang;
    let json = invocation.json;
    match &invocation.command {
        Command::Help { topic } => {
            let text = crate::args::usage(lang, topic.as_deref());
            let outcome = Outcome::ok("help", lang, serde_json::json!({ "help": text }));
            CommandOutput::finish(outcome, text)
        }
        Command::Version => {
            let text = gopt_core::version_line();
            let outcome = Outcome::ok(
                "version",
                lang,
                serde_json::json!({
                    "product": gopt_core::PRODUCT_NAME,
                    "version": gopt_core::VERSION,
                    "phase": "Phase 1",
                    "cpp_release": gopt_core::CPP_RELEASE_VERSION,
                    "schema_version": gopt_core::SCHEMA_VERSION,
                    "line": text,
                }),
            );
            CommandOutput::finish(outcome, text)
        }
        _ => run_with_session(&invocation, json),
    }
}

/// 造 `Session` 并分发（除 help/version 之外的全部命令）。
fn run_with_session(invocation: &Invocation, json: bool) -> CommandOutput {
    let lang = invocation.lang;
    let command_name = invocation.command.name();
    let paths = DataPaths::resolve(invocation.data_dir.clone());
    let api = make_backend();
    let session = match Session::new(api, paths, lang) {
        Ok(session) => session,
        Err(error) => return CommandOutput::failed(command_name, lang, error),
    };
    let mut output = match &invocation.command {
        Command::Status => status(&session),
        Command::List(kind) => list(&session, *kind),
        Command::Plan { query, pid } => plan(&session, query, *pid),
        Command::Apply { query, pid, yes } => apply(&session, query, *pid, *yes),
        Command::Rollback {
            to,
            all,
            pending,
            yes,
        } => rollback(&session, *to, *all, *pending, *yes),
        Command::Journal { kind, limit } => journal(&session, kind.as_deref(), *limit),
        Command::Explain {
            game,
            rule,
            journal_id,
        } => explain(&session, game.as_deref(), rule.as_deref(), *journal_id),
        Command::VerifyJournal {
            anchor_len,
            anchor_hash,
            strict,
        } => verify_journal(&session, *anchor_len, anchor_hash.as_deref(), *strict),
        Command::Watch {
            watch: watch_args,
            yes,
        } => watch(&session, *watch_args, *yes, json),
        Command::Prio { pid, exe, set, yes } => prio(&session, *pid, exe.as_deref(), *set, *yes),
        Command::Tune { scheme, yes } => tune(&session, *scheme, *yes),
        Command::Startup { action, hive, yes } => startup(&session, action, *hive, *yes),
        Command::Report { out } => report(&session, out.as_deref()),
        Command::ImportLegacy {
            savepoints,
            games_conf,
            yes,
        } => import_legacy(&session, savepoints.as_deref(), games_conf.as_deref(), *yes),
        Command::Help { .. } | Command::Version => CommandOutput::failed(
            command_name,
            lang,
            CoreError::internal(
                "gopt::run",
                "help and version are handled before the session is built",
            ),
        ),
    };

    // 策略加载错误对所有命令都可见（坏文件绝不静默）。
    for diagnostic in session.diagnostics() {
        if diagnostic.is_error() {
            output = prepend_notice(
                output,
                &format!(
                    "{} {}",
                    pick(lang, "策略文件加载失败:", "policy file failed to load:"),
                    diagnostic
                ),
            );
        }
    }
    output
}

/// 在文本输出之前插一行提示（`--json` 的 notices 由各命令自己维护）。
fn prepend_notice(output: CommandOutput, message: &str) -> CommandOutput {
    CommandOutput {
        text: format!("! {message}\n{}", output.text),
        ..output
    }
}

/// 真实后端（Windows）/ Mock 后端（其它平台或测试开关）。
fn make_backend() -> Arc<dyn SystemApi> {
    let use_mock = std::env::var(BACKEND_ENV)
        .map(|value| value.eq_ignore_ascii_case("mock"))
        .unwrap_or(false);
    if use_mock {
        return Arc::new(MockApi::sample_workstation());
    }
    #[cfg(windows)]
    {
        Arc::new(gopt_core::Win32Api::new())
    }
    #[cfg(not(windows))]
    {
        // 非 Windows 平台没有 Win32 后端：给一个 Mock，让 CLI 仍然可跑（并在提示里说明）。
        Arc::new(MockApi::sample_workstation())
    }
}

// ---------------------------------------------------------------------------
// 各命令
// ---------------------------------------------------------------------------

fn status(session: &Session) -> CommandOutput {
    match session.status() {
        Ok(report) => {
            let text = render_status(&report, session.lang());
            let mut outcome = Outcome::ok("status", session.lang(), report.clone());
            for notice in &report.notices {
                outcome = outcome.with_notice(Notice::info(notice.zh.clone(), notice.en.clone()));
            }
            CommandOutput::finish(outcome, text)
        }
        Err(error) => CommandOutput::failed("status", session.lang(), error),
    }
}

fn list(session: &Session, kind: ListKind) -> CommandOutput {
    match kind {
        ListKind::Games => {
            let games = session.games();
            let text = render_games(&games, session.lang());
            CommandOutput::finish(Outcome::ok("list", session.lang(), games), text)
        }
        ListKind::Processes => match session.processes() {
            Ok(processes) => {
                let mut text = format!(
                    "{}: {}\n",
                    pick(session.lang(), "运行中的进程", "running processes"),
                    processes.len()
                );
                for process in &processes {
                    text.push_str(&format!(
                        "  {:<8} {:<28} {:<6} {}\n",
                        process.pid,
                        process.name,
                        process.thread_count,
                        process.exe_path.as_deref().unwrap_or("-")
                    ));
                }
                CommandOutput::finish(Outcome::ok("list", session.lang(), processes), text)
            }
            Err(error) => CommandOutput::failed("list", session.lang(), error),
        },
        ListKind::Startup => match session.startup_list() {
            Ok(report) => {
                let text = render_startup(&report, session.lang());
                CommandOutput::finish(Outcome::ok("list", session.lang(), report), text)
            }
            Err(error) => CommandOutput::failed("list", session.lang(), error),
        },
    }
}

fn plan(session: &Session, query: &str, pid: Option<u32>) -> CommandOutput {
    match session.plan(query, pid) {
        Ok(report) => {
            let text = render_plan(&report, session.lang());
            CommandOutput::finish(Outcome::ok("plan", session.lang(), report), text)
        }
        Err(error) => CommandOutput::failed("plan", session.lang(), error),
    }
}

fn apply(session: &Session, query: &str, pid: Option<u32>, yes: bool) -> CommandOutput {
    let lang = session.lang();
    let report = match session.plan(query, pid) {
        Ok(report) => report,
        Err(error) => return CommandOutput::failed("apply", lang, error),
    };
    let options = if yes {
        ApplyOptions::commit()
    } else {
        ApplyOptions::preview()
    };
    match session.apply(&report.plan, options) {
        Ok(applied) => {
            let text = render_apply(&applied, lang);
            match apply_outcome(&applied, lang) {
                Ok(outcome) => CommandOutput::finish(outcome, text),
                Err(error) => {
                    let text = format!("{text}\n{}", render_error(&error, lang));
                    let outcome: Outcome<ApplyReport> =
                        Outcome::failed_with("apply", lang, error, applied);
                    CommandOutput::finish(outcome, text)
                }
            }
        }
        Err(error) => CommandOutput::failed("apply", lang, error),
    }
}

/// 执行结果的成败判定（全部失败路径都保留报告数据，脚本看得见已发生的改动）。
fn apply_outcome(applied: &ApplyReport, lang: Lang) -> Result<Outcome<ApplyReport>, CoreError> {
    if applied.journal_blocked {
        return Err(CoreError::journal_kind_broken(
            "Session::apply",
            "the audit journal could not be written; the remaining steps were stopped",
        ));
    }
    if applied.failed > 0 {
        return Err(CoreError::new(
            CoreErrorKind::Hal,
            "Session::apply",
            format!(
                "{} of {} plan step(s) failed",
                applied.failed,
                applied.steps.len()
            ),
        ));
    }
    let mut outcome = Outcome::ok("apply", lang, applied.clone());
    if applied.dry_run {
        outcome = outcome.with_notice(Notice::info(
            "预演：没有写入任何系统状态，也没有写审计日志（加 --yes 真正执行）",
            "dry-run: nothing was written to the system or the journal (add --yes to apply)",
        ));
    }
    if applied.applied == 0 && !applied.dry_run {
        outcome = outcome.with_notice(Notice::info(
            "没有需要改的东西：所有步骤都已经是目标值",
            "nothing to change: every step is already at its target value",
        ));
    }
    Ok(outcome)
}

fn rollback(
    session: &Session,
    to: Option<u64>,
    all: bool,
    pending: bool,
    yes: bool,
) -> CommandOutput {
    let lang = session.lang();
    let mut options = match (to, all, pending) {
        (Some(to), _, _) => RollbackOptions::to(to),
        (None, _, true) => RollbackOptions::pending(),
        _ => RollbackOptions::all(),
    };
    if !yes {
        options = options.preview();
    }
    match session.rollback(options) {
        Ok(report) => {
            let text = render_rollback(&report, lang);
            if report.failed > 0
                || report
                    .steps
                    .iter()
                    .any(|step| step.status == StepStatus::Stopped)
            {
                let error = if report
                    .steps
                    .iter()
                    .any(|step| step.status == StepStatus::Stopped)
                {
                    CoreError::journal_kind_broken(
                        "Session::rollback",
                        "the audit journal could not be written during the rollback; the remaining steps were stopped",
                    )
                } else {
                    CoreError::new(
                        CoreErrorKind::Hal,
                        "Session::rollback",
                        format!("{} rollback step(s) failed", report.failed),
                    )
                };
                let text = format!("{text}\n{}", render_error(&error, lang));
                let outcome: Outcome<RollbackReport> =
                    Outcome::failed_with("rollback", lang, error, report);
                return CommandOutput::finish(outcome, text);
            }
            let mut outcome = Outcome::ok("rollback", lang, report.clone());
            if report.dry_run {
                outcome = outcome.with_notice(Notice::info(
                    "预演：只生成了撤销计划（加 --yes 执行）",
                    "dry-run: the undo plan was generated only (add --yes to execute)",
                ));
            }
            if report.not_actionable > 0 {
                outcome = outcome.with_notice(Notice::warning(
                    "有步骤无法自动撤销（需要人工确认）",
                    "some steps cannot be undone automatically and need a human",
                ));
            }
            CommandOutput::finish(outcome, text)
        }
        Err(error) => CommandOutput::failed("rollback", lang, error),
    }
}

fn journal(session: &Session, kind: Option<&str>, limit: Option<usize>) -> CommandOutput {
    let lang = session.lang();
    let kind = match kind {
        None => None,
        Some(text) => match JournalKind::parse(text) {
            Some(kind) => Some(kind),
            None => {
                return CommandOutput::usage(
                    lang,
                    &format!("`{text}` is not a record kind; use apply, rollback or imported"),
                    Some("journal"),
                )
            }
        },
    };
    match session.journal_view(JournalFilter { kind, limit }) {
        Ok(view) => {
            let text = render_journal(&view, lang);
            CommandOutput::finish(Outcome::ok("journal", lang, view), text)
        }
        Err(error) => CommandOutput::failed("journal", lang, error),
    }
}

fn explain(
    session: &Session,
    game: Option<&str>,
    rule: Option<&str>,
    journal_id: Option<u64>,
) -> CommandOutput {
    let lang = session.lang();
    let queries = usize::from(game.is_some())
        + usize::from(rule.is_some())
        + usize::from(journal_id.is_some());
    if queries != 1 {
        return CommandOutput::usage(
            lang,
            "explain needs exactly one of --game <id>, --rule <id> or --journal-id <n>",
            Some("explain"),
        );
    }
    let result = if let Some(game) = game {
        session.explain_game(game)
    } else if let Some(rule) = rule {
        session.explain_rule(rule)
    } else if let Some(id) = journal_id {
        session.explain_journal(id)
    } else {
        return CommandOutput::failed(
            "explain",
            lang,
            CoreError::internal("gopt::explain", "no query was selected"),
        );
    };
    match result {
        Ok(report) => {
            let text = render_explain(&report, lang);
            CommandOutput::finish(Outcome::ok("explain", lang, report), text)
        }
        Err(error) => CommandOutput::failed("explain", lang, error),
    }
}

fn verify_journal(
    session: &Session,
    anchor_len: Option<u64>,
    anchor_hash: Option<&str>,
    strict: bool,
) -> CommandOutput {
    let lang = session.lang();
    let anchor = match (anchor_len, anchor_hash) {
        (None, None) => None,
        (Some(len), Some(hash)) => Some(ChainAnchor {
            len,
            last_hash: hash.to_string(),
        }),
        _ => {
            return CommandOutput::usage(
                lang,
                "--anchor-len and --anchor-hash must be given together",
                Some("verify-journal"),
            )
        }
    };
    match session.verify_journal(VerifyOptions { anchor, strict }) {
        Ok(report) => {
            let text = render_verify(&report, lang);
            if report.exit_code() == EXIT_OK {
                let outcome = Outcome::ok("verify-journal", lang, report.clone()).with_notice(
                    if report.status == VerifyStatus::Recoverable {
                    Notice::warning(
                        "尾部有崩溃残留的半行（可修复）；--strict 时会按失败处理",
                        "the file ends with a torn half-line (repairable); --strict treats it as a failure",
                    )
                } else {
                    Notice::info("审计链完好", "the audit chain verifies")
                });
                CommandOutput::finish(outcome, text)
            } else {
                let error = CoreError::journal_kind_broken(
                    "Journal::verify_chain",
                    report
                        .first_break
                        .as_ref()
                        .map_or_else(|| report.summary.clone(), |breakage| breakage.summary()),
                );
                let text = format!("{text}\n{}", render_error(&error, lang));
                let outcome: Outcome<VerifyReport> =
                    Outcome::failed_with("verify-journal", lang, error, report);
                CommandOutput::finish(outcome, text)
            }
        }
        Err(error) => CommandOutput::failed("verify-journal", lang, error),
    }
}

/// `watch` 是唯一会循环的命令；`--json` 时每轮输出一行 JSON（JSONL），便于 `jq` 流式处理。
fn watch(session: &Session, args: WatchArgs, yes: bool, json: bool) -> CommandOutput {
    let lang = session.lang();
    let options = WatchOptions {
        interval_secs: args.interval,
        duration_secs: args.duration,
        once: args.once,
        apply: yes,
    };
    let mut state = WatchState::default();
    let mut ticks: Vec<WatchTick> = Vec::new();
    let mut text = String::new();
    if !json {
        text.push_str(&format!(
            "{}: {}s / {}: {}{}\n",
            pick(lang, "开始监控，间隔", "watching every"),
            options.interval_secs,
            pick(lang, "时长", "duration"),
            if options.duration_secs == 0 {
                pick(lang, "直到 Ctrl+C", "until Ctrl+C")
            } else {
                "?"
            },
            if args.once {
                pick(lang, "（只跑一轮）", " (single tick)")
            } else {
                ""
            }
        ));
    }
    loop {
        let tick = match session.watch_tick(&mut state, options) {
            Ok(tick) => tick,
            Err(error) => return CommandOutput::failed("watch", lang, error),
        };
        if json {
            text.push_str(&format!(
                "{}\n",
                Outcome::ok("watch", lang, tick.clone()).to_json()
            ));
        } else {
            text.push_str(&render_watch(&tick, lang));
        }
        let continuing = tick.continuing;
        ticks.push(tick);
        if !continuing {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(options.interval_secs.max(1)));
    }

    if json {
        // JSONL：文本与 JSON 完全一致（每行一个 Outcome），退出码仍由是否出现错误决定。
        return CommandOutput {
            code: EXIT_OK,
            json: text.clone(),
            text,
        };
    }
    let outcome = Outcome::ok("watch", lang, ticks);
    CommandOutput::finish(outcome, text)
}

fn prio(
    session: &Session,
    pid: Option<u32>,
    exe: Option<&str>,
    set: Option<PriorityClass>,
    yes: bool,
) -> CommandOutput {
    let lang = session.lang();
    if pid.is_none() && exe.is_none() {
        return CommandOutput::usage(
            lang,
            "prio needs --pid <pid> or --exe <name.exe>",
            Some("prio"),
        );
    }
    match session.priority(pid, exe, set, !yes) {
        Ok(report) => {
            let text = render_prio(&report, lang);
            let mut outcome = Outcome::ok("prio", lang, report.clone());
            if report.changed && report.dry_run {
                outcome = outcome.with_notice(Notice::info(
                    "预演：没有写入（加 --yes 真正设置优先级）",
                    "dry-run: nothing written (add --yes to set the priority)",
                ));
            }
            CommandOutput::finish(outcome, text)
        }
        Err(error) => CommandOutput::failed("prio", lang, error),
    }
}

fn tune(session: &Session, scheme: Option<PowerSchemeChoice>, yes: bool) -> CommandOutput {
    let lang = session.lang();
    match session.tune(scheme, !yes) {
        Ok(report) => {
            let text = render_tune(&report, lang);
            let mut outcome = Outcome::ok("tune", lang, report.clone());
            if report.changed && report.dry_run {
                outcome = outcome.with_notice(Notice::info(
                    "预演：没有切换电源方案（加 --yes 执行）",
                    "dry-run: the power scheme was not switched (add --yes)",
                ));
            }
            CommandOutput::finish(outcome, text)
        }
        Err(error) => CommandOutput::failed("tune", lang, error),
    }
}

fn startup(session: &Session, action: &StartupAction, hive: RunHive, yes: bool) -> CommandOutput {
    let lang = session.lang();
    let result: Result<StartupReport, CoreError> = match action {
        StartupAction::List => session.startup_list(),
        StartupAction::Enable(name) => session.startup_set(hive, name, true, !yes),
        StartupAction::Disable(name) => session.startup_set(hive, name, false, !yes),
    };
    match result {
        Ok(report) => {
            let text = render_startup(&report, lang);
            let mut outcome = Outcome::ok("startup", lang, report.clone());
            if report.changed && report.dry_run && report.action != "list" {
                outcome = outcome.with_notice(Notice::info(
                    "预演：没有改注册表（加 --yes 执行）",
                    "dry-run: the registry was not changed (add --yes)",
                ));
            }
            CommandOutput::finish(outcome, text)
        }
        Err(error) => CommandOutput::failed("startup", lang, error),
    }
}

fn report(session: &Session, out: Option<&std::path::Path>) -> CommandOutput {
    let lang = session.lang();
    match session.report() {
        Ok(report) => {
            let mut text = render_report(&report, lang, session.paths());
            if let Some(path) = out {
                if let Err(error) = std::fs::write(path, &text) {
                    return CommandOutput::failed(
                        "report",
                        lang,
                        CoreError::io(
                            "report --out",
                            format!("cannot write {}: {error}", path.display()),
                        ),
                    );
                }
                text.push_str(&format!(
                    "\n{}: {}\n",
                    pick(lang, "报告已写入", "report written to"),
                    path.display()
                ));
            }
            CommandOutput::finish(Outcome::ok("report", lang, report), text)
        }
        Err(error) => CommandOutput::failed("report", lang, error),
    }
}

fn import_legacy(
    session: &Session,
    savepoints: Option<&std::path::Path>,
    games_conf: Option<&std::path::Path>,
    yes: bool,
) -> CommandOutput {
    let lang = session.lang();
    let options = LegacyImportOptions {
        savepoints: savepoints.map(PathBuf::from),
        games_conf: games_conf.map(PathBuf::from),
        dry_run: !yes,
    };
    match session.import_legacy(options) {
        Ok(report) => {
            let text = render_import(&report, lang);
            let mut outcome = Outcome::ok("import-legacy", lang, report.clone());
            if report.dry_run {
                outcome = outcome.with_notice(Notice::info(
                    "预演：只解析了旧文件（加 --yes 把历史条目写进审计链）",
                    "dry-run: the legacy files were parsed only (add --yes to append them to the chain)",
                ));
            }
            if report.policy_filtered > 0 {
                outcome = outcome.with_notice(Notice::warning(
                    "旧文件里有 REALTIME 优先级条目，已按红线过滤（不会进入可回滚字段）",
                    "the legacy file contained REALTIME priorities; they were filtered out by policy",
                ));
            }
            CommandOutput::finish(outcome, text)
        }
        Err(error) => CommandOutput::failed("import-legacy", lang, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::parse;

    fn run(args: &[&str]) -> CommandOutput {
        let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let invocation = parse(&owned).expect("parse");
        execute(invocation)
    }

    #[test]
    fn version_and_help_do_not_need_a_session() {
        let version = run(&["--version"]);
        assert_eq!(version.code, 0);
        assert!(version.text.contains("GameOptimizer-RS"));
        assert!(version.json.contains("cpp_release"));

        let help = run(&["help"]);
        assert_eq!(help.code, 0);
        // 语言取决于 GOPT_LANG：两种语言的用法里都必须列出退出码与 `--json`。
        assert!(
            help.text.contains("退出码") || help.text.contains("exit codes"),
            "{}",
            help.text
        );
        assert!(help.text.contains("--json"));
    }

    #[test]
    fn usage_errors_exit_one() {
        let output = CommandOutput::usage(Lang::Zh, "boom", Some("plan"));
        assert_eq!(output.code, 1);
        assert!(output.text.contains("用法错误"));
        assert!(output.json.contains("\"kind\": \"usage\""));
    }

    #[test]
    fn status_render_includes_notices_in_json() {
        // 用 Mock 后端 + 临时数据目录，保证测试不碰真机。
        std::env::set_var(BACKEND_ENV, "mock");
        let dir = std::env::temp_dir().join(format!("gopt-cli-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let output = run(&[
            "status",
            "--json",
            "--data-dir",
            dir.to_str().expect("utf8"),
        ]);
        std::env::remove_var(BACKEND_ENV);
        assert_eq!(output.code, 0);
        let parsed: serde_json::Value = serde_json::from_str(&output.json).expect("json");
        assert_eq!(parsed["schema_version"], gopt_core::SCHEMA_VERSION);
        assert_eq!(parsed["command"], "status");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["data"]["backend"], "mock");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
