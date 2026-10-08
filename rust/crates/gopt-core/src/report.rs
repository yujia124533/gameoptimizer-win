//! 文本渲染：把内核的 DTO 变成给人看的多行文本（`--json` 走 `serde`，本模块只管终端）。
//!
//! 渲染放在内核而不是 CLI，是为了让"将来再多一个前端"（GUI/托盘程序）复用同一套措辞与信息层次；
//! CLI 只负责：解析参数 → 调内核 → 选一种渲染 → 打印。
//!
//! 语言由 [`Lang`] 决定；两种语言的信息密度刻意保持一致（中文不省略细节，英文不堆术语）。

use serde_json::Value;

use gopt_hal::HardwareInfo;
use gopt_policy::{Plan, PlanStep, SkipCause};

use crate::i18n::{pick, Lang};
use crate::model::{
    ApplyReport, ExplainReport, GameSummary, ImportReport, JournalView, PlanReport, PrioReport,
    ReportData, RollbackReport, StartupReport, StatusReport, StepStatus, TuneReport, VerifyReport,
    VerifyStatus, WatchTick,
};
use crate::paths::DataPaths;

/// JSON 值 → 一行紧凑文本（长值截断，保证终端里一行放得下）。
pub fn compact(value: &Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "<unserialisable>".to_string());
    if text.chars().count() > 120 {
        let head: String = text.chars().take(117).collect();
        format!("{head}...")
    } else {
        text
    }
}

/// 可选 JSON 值 → 文本（`null` 显示成 `-`）。
fn compact_opt(value: Option<&Value>) -> String {
    match value {
        Some(value) => compact(value),
        None => "-".to_string(),
    }
}

/// 一行标题。
fn header(text: &str) -> String {
    format!("== {text} ==")
}

/// MiB 展示（保留整数）。
fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// 硬件画像。
pub fn render_hardware(hardware: &HardwareInfo, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        pick(
            lang,
            &format!(
                "CPU: {} ({}C/{}T{})",
                hardware.cpu_model,
                hardware.physical_cores,
                hardware.logical_cores,
                if hardware.supports_hyper_threading {
                    ", SMT"
                } else {
                    ""
                }
            ),
            &format!(
                "CPU: {} ({}C/{}T{})",
                hardware.cpu_model,
                hardware.physical_cores,
                hardware.logical_cores,
                if hardware.supports_hyper_threading {
                    ", SMT"
                } else {
                    ""
                }
            )
        )
    ));
    out.push_str(&format!(
        "{}\n",
        pick(
            lang,
            &format!(
                "内存: {} MiB 总量 / {} MiB 可用",
                hardware.system_ram_mb, hardware.available_ram_mb
            ),
            &format!(
                "RAM: {} MiB total / {} MiB available",
                hardware.system_ram_mb, hardware.available_ram_mb
            )
        )
    ));
    match &hardware.gpu {
        Some(gpu) => out.push_str(&format!(
            "{}\n",
            pick(
                lang,
                &format!(
                    "GPU: {} ({} {:#06x}:{:#06x}, {} MiB 显存{})",
                    gpu.model,
                    gpu.vendor.as_str(),
                    gpu.vendor_id,
                    gpu.device_id,
                    gpu.vram_mb,
                    gpu.driver_version
                        .as_ref()
                        .map(|driver| format!(", 驱动 {driver}"))
                        .unwrap_or_default()
                ),
                &format!(
                    "GPU: {} ({} {:#06x}:{:#06x}, {} MiB VRAM{})",
                    gpu.model,
                    gpu.vendor.as_str(),
                    gpu.vendor_id,
                    gpu.device_id,
                    gpu.vram_mb,
                    gpu.driver_version
                        .as_ref()
                        .map(|driver| format!(", driver {driver}"))
                        .unwrap_or_default()
                )
            )
        )),
        None => out.push_str(&format!(
            "{}\n",
            pick(
                lang,
                "GPU: 未探测到独立适配器",
                "GPU: no discrete adapter detected"
            )
        )),
    }
    out.push_str(&format!(
        "{}\n",
        pick(
            lang,
            &format!(
                "处理器组: {} 组 / 大页: {}",
                hardware.processor_groups.len(),
                if hardware.large_pages_available {
                    "可用"
                } else {
                    "不可用"
                }
            ),
            &format!(
                "Processor groups: {} / large pages: {}",
                hardware.processor_groups.len(),
                if hardware.large_pages_available {
                    "available"
                } else {
                    "unavailable"
                }
            )
        )
    ));
    for warning in &hardware.warnings {
        out.push_str(&format!("  ! {warning}\n"));
    }
    out
}

/// `gopt status`。
pub fn render_status(status: &StatusReport, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {} — {} / {}\n",
        status.product,
        status.version,
        pick(lang, "Phase 1", "Phase 1"),
        pick(
            lang,
            &format!("C++ v{} 仍是正式发布版", status.cpp_release),
            &format!("C++ v{} is still the official release", status.cpp_release)
        )
    ));
    out.push_str(&header(pick(lang, "环境", "Environment")));
    out.push('\n');
    out.push_str(&format!(
        "{}: {}\n",
        pick(lang, "数据目录", "Data directory"),
        status.data_dir
    ));
    out.push_str(&format!(
        "{}: {} ({})\n",
        pick(lang, "HAL 后端", "HAL backend"),
        status.backend,
        pick(
            lang,
            if status.elevated {
                "已提权"
            } else {
                "未提权"
            },
            if status.elevated {
                "elevated"
            } else {
                "not elevated"
            }
        )
    ));
    out.push_str(&format!(
        "{}: {}\n",
        pick(lang, "语言", "Language"),
        status.lang
    ));
    out.push_str(&render_hardware(&status.hardware, lang));

    out.push_str(&header(pick(lang, "审计日志", "Audit journal")));
    out.push('\n');
    out.push_str(&format!("{}\n", status.journal_path));
    out.push_str(&format!(
        "{}: {} / {}: {}\n",
        pick(lang, "记录", "records"),
        status.journal_records,
        pick(lang, "链状态", "chain"),
        if status.chain_ok {
            pick(lang, "完好", "verified")
        } else {
            pick(lang, "校验未通过", "does not verify")
        }
    ));
    out.push_str(&format!("  {}\n", status.chain_summary));
    if let Some(repair) = status.tail_repair_bytes {
        out.push_str(&format!(
            "  {}\n",
            pick(
                lang,
                &format!("尾部崩溃残留已截断：{repair} 字节"),
                &format!("torn tail truncated: {repair} bytes")
            )
        ));
    }
    out.push_str(&format!(
        "{}: {}\n",
        pick(lang, "待撤销的记录", "records pending rollback"),
        status.pending_rollbacks
    ));

    out.push_str(&header(pick(lang, "策略", "Policies")));
    out.push('\n');
    out.push_str(&format!(
        "{}: {} ({} {} + {} {}) / {}: {} / {}: {}\n",
        pick(lang, "总数", "total"),
        status.policies_total,
        pick(lang, "内置", "builtin"),
        status.policies_builtin,
        pick(lang, "用户", "user"),
        status.policies_user,
        pick(lang, "错误", "errors"),
        status.policy_errors,
        pick(lang, "警告", "warnings"),
        status.policy_warnings
    ));
    out.push_str(&format!(
        "{}: {}\n",
        pick(lang, "用户覆盖目录", "user override directory"),
        status.user_policy_dir
    ));

    out.push_str(&header(pick(lang, "正在运行的游戏", "Running games")));
    out.push('\n');
    if status.running_games.is_empty() {
        out.push_str(&format!(
            "{}\n",
            pick(
                lang,
                "没有匹配到策略的游戏进程",
                "no running process matches a policy"
            )
        ));
    } else {
        for game in &status.running_games {
            out.push_str(&format!(
                "  {} {} (pid {}) — {} {}{}\n",
                game.game_id,
                game.exe,
                game.pid,
                game.steps,
                pick(lang, "步", "steps"),
                if game.requires_elevation {
                    pick(lang, "（含需提权步骤）", " (needs elevation)")
                } else {
                    ""
                }
            ));
        }
    }

    for diagnostic in &status.policy_diagnostics {
        out.push_str(&format!("  {diagnostic}\n"));
    }
    for notice in &status.notices {
        out.push_str(&format!("  ! {}\n", notice.pick(lang)));
    }
    out
}

/// `gopt list games`。
pub fn render_games(games: &[GameSummary], lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}: {}\n",
        pick(lang, "可用策略", "available policies"),
        games.len()
    ));
    for game in games {
        let running = match game.running_pid {
            Some(pid) => match lang {
                Lang::Zh => format!("运行中 pid {pid}"),
                Lang::En => format!("running pid {pid}"),
            },
            None => pick(lang, "未运行", "not running").to_string(),
        };
        out.push_str(&format!(
            "  {:<22} {:<30} {:<16} {} {} / {}\n",
            game.id,
            format!("{} / {}", game.name_zh, game.name_en),
            game.exe_match,
            game.rules,
            pick(lang, "条规则", "rules"),
            running
        ));
        out.push_str(&format!(
            "    {}: {} — {}\n",
            pick(lang, "来源", "origin"),
            game.origin,
            if game.hal_ops.is_empty() {
                pick(lang, "无需系统调用", "no system calls").to_string()
            } else {
                game.hal_ops.join(", ")
            }
        ));
    }
    out
}

/// 一条计划步骤（不含前后值：那是 `render_apply` 的事）。
fn render_step(step: &PlanStep, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {}. [{}] {}\n",
        step.order,
        step.hal_op().as_str(),
        pick(lang, &step.reason.zh, &step.reason.en)
    ));
    out.push_str(&format!(
        "     {}: {}:{} — {}\n",
        pick(lang, "规则", "rule"),
        step.rule_id,
        step.rule_line
            .map(|line| line.to_string())
            .unwrap_or_else(|| "-".to_string()),
        step.action.hal_op().as_str()
    ));
    out
}

/// `gopt plan`（只读预演的计划文本）。
pub fn render_plan(report: &PlanReport, lang: Lang) -> String {
    let plan: &Plan = &report.plan;
    let mut out = String::new();
    let state = if report.running {
        match lang {
            Lang::Zh => format!("运行中 pid {}", plan.pid),
            Lang::En => format!("running pid {}", plan.pid),
        }
    } else {
        pick(lang, "未运行：预览", "not running: preview").to_string()
    };
    out.push_str(&format!(
        "{} {} — {} / {} ({state})\n",
        pick(lang, "计划", "plan"),
        plan.game_id,
        plan.game_name_zh,
        plan.game_name_en
    ));
    out.push_str(&format!(
        "{}: {} | {}: {} | {}: {} {}\n",
        pick(lang, "策略来源", "policy origin"),
        plan.policy_origin.display_path(),
        pick(lang, "步骤", "steps"),
        plan.step_count(),
        pick(lang, "跳过", "skipped"),
        plan.skipped_count(),
        pick(lang, "(只读，不改系统)", "(read-only, nothing is changed)")
    ));
    if !report.candidates.is_empty() {
        out.push_str(&format!(
            "{}: {}\n",
            pick(lang, "候选", "candidates"),
            report.candidates.join(", ")
        ));
    }
    for step in &plan.steps {
        out.push_str(&render_step(step, lang));
    }
    if plan.steps.is_empty() {
        out.push_str(&format!(
            "  {}\n",
            pick(
                lang,
                "没有可执行步骤（见下方跳过原因）",
                "no executable steps (see the skip reasons below)"
            )
        ));
    }
    for skip in &plan.skipped {
        out.push_str(&format!(
            "  - {} [{}]: {}\n",
            skip.rule_id,
            match skip.cause {
                SkipCause::ConditionNotMet => pick(lang, "条件不满足", "condition not met"),
                SkipCause::Degraded => pick(lang, "降级", "degraded"),
                SkipCause::ExplicitSkip => pick(lang, "策略显式跳过", "explicitly skipped"),
            },
            pick(lang, &skip.reason.zh, &skip.reason.en)
        ));
    }
    for note in &report.notes {
        out.push_str(&format!("  ! {}\n", note.pick(lang)));
    }
    out
}

/// `gopt apply`（含预演）。
pub fn render_apply(report: &ApplyReport, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {} {} / {} — pid {} ({})\n",
        if report.dry_run {
            pick(lang, "预演（未写入）", "dry-run (nothing written)")
        } else {
            pick(lang, "执行", "apply")
        },
        report.game_id,
        report.game_name_zh,
        report.game_name_en,
        report.pid,
        report.policy_origin
    ));
    for step in &report.steps {
        out.push_str(&format!(
            "  {}. [{}/{}] {} {}\n",
            step.order,
            step.hal_op.as_str(),
            step.status.as_str(),
            pick(lang, step.status.label_zh(), step.status.label_en()),
            step.rule_id
        ));
        out.push_str(&format!(
            "     before: {}\n     after : {}\n",
            compact_opt(step.before.as_ref()),
            compact_opt(step.after.as_ref())
        ));
        if let Some(id) = step.journal_id {
            out.push_str(&format!(
                "     {} #{id}\n",
                pick(lang, "审计记录", "journal record")
            ));
        }
        if !step.reversible && matches!(step.status, StepStatus::Applied | StepStatus::DryRun) {
            out.push_str(&format!(
                "     ! {}\n",
                pick(
                    lang,
                    "不可回滚（写入前状态不可知）",
                    "not reversible (previous state unknown)"
                )
            ));
        }
        if let Some(note) = &step.note {
            out.push_str(&format!("     ! {}\n", note.pick(lang)));
        }
        if let Some(error) = &step.error {
            out.push_str(&format!(
                "     X {}: {}\n        {}\n",
                error.kind().as_str(),
                error.message(),
                pick(lang, &error.hint_zh(), &error.hint_en())
            ));
        }
    }
    for skip in &report.skipped {
        out.push_str(&format!(
            "  - {}: {}\n",
            skip.rule_id,
            pick(lang, &skip.reason.zh, &skip.reason.en)
        ));
    }
    out.push_str(&format!(
        "{}: {} {} / {} {} / {} {} / {} {} / {} {}\n",
        pick(lang, "汇总", "summary"),
        report.applied,
        pick(lang, "已应用", "applied"),
        report.unchanged,
        pick(lang, "已是目标值", "unchanged"),
        report.skipped_missing,
        pick(lang, "目标不存在", "missing"),
        report.failed,
        pick(lang, "失败", "failed"),
        report.journal_ids.len(),
        pick(lang, "条审计记录", "journal records")
    ));
    for notice in &report.notices {
        out.push_str(&format!("  ! {}\n", notice.pick(lang)));
    }
    out
}

/// `gopt rollback`。
pub fn render_rollback(report: &RollbackReport, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} (to_id={}){}\n",
        pick(lang, "回滚", "rollback"),
        report.to_id,
        if report.dry_run {
            pick(lang, " — 预演，未执行", " — dry-run, nothing executed")
        } else {
            ""
        }
    ));
    out.push_str(&format!("  {}\n", report.summary));
    for step in &report.steps {
        out.push_str(&format!(
            "  #{} [{}] {} — {}\n",
            step.journal_id,
            step.status.as_str(),
            step.target,
            step.description
        ));
        if let Some(id) = step.rollback_record_id {
            out.push_str(&format!(
                "     {} #{id}\n",
                pick(lang, "撤销记录", "rollback record")
            ));
        }
        if let Some(error) = &step.error {
            out.push_str(&format!(
                "     X {}: {}\n        {}\n",
                error.kind().as_str(),
                error.message(),
                pick(lang, &error.hint_zh(), &error.hint_en())
            ));
        }
    }
    out.push_str(&format!(
        "{}: {} {} / {} {} / {} {} / {} {}\n",
        pick(lang, "汇总", "summary"),
        report.planned,
        pick(lang, "计划", "planned"),
        report.executable,
        pick(lang, "可执行", "executable"),
        report.executed,
        pick(lang, "已执行", "executed"),
        report.failed,
        pick(lang, "失败", "failed")
    ));
    for notice in &report.notices {
        out.push_str(&format!("  ! {}\n", notice.pick(lang)));
    }
    out
}

/// `gopt journal`。
pub fn render_journal(view: &JournalView, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", view.path));
    out.push_str(&format!(
        "{}: {} / {}: {} / {}: {}\n",
        pick(lang, "记录", "records"),
        view.records,
        pick(lang, "链", "chain"),
        if view.chain.is_ok() {
            pick(lang, "完好", "verified")
        } else {
            pick(lang, "异常", "broken")
        },
        pick(lang, "待撤销", "pending rollback"),
        view.pending_apply_ids.len()
    ));
    out.push_str(&format!("  {}\n", view.chain.summary()));
    if view.malformed_lines > 0 {
        out.push_str(&format!(
            "  ! {}: {}\n",
            pick(lang, "无法解析的行", "malformed lines"),
            view.malformed_lines
        ));
    }
    for entry in &view.entries {
        out.push_str(&format!(
            "  #{:<4} {:<9} {:<34} {} {}\n",
            entry.id,
            entry.kind.as_str(),
            entry.target,
            entry.rule_id.as_deref().unwrap_or("-"),
            if entry.undone {
                pick(lang, "[已撤销]", "[undone]")
            } else {
                ""
            }
        ));
        out.push_str(&format!(
            "        before {}\n        after  {}\n",
            compact_opt(entry.before.as_ref()),
            compact_opt(entry.after.as_ref())
        ));
        if !entry.hash_ok {
            out.push_str(&format!(
                "        ! {}\n",
                pick(
                    lang,
                    "自哈希不自洽：该行被改动过",
                    "self-hash mismatch: this line was modified"
                )
            ));
        }
    }
    out
}

/// `gopt verify-journal`。
pub fn render_verify(report: &VerifyReport, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", report.path));
    let verdict = match report.status {
        VerifyStatus::Ok => pick(lang, "审计链完好", "the audit chain verifies"),
        VerifyStatus::Recoverable => pick(
            lang,
            "审计链完好，但尾部有崩溃残留的半行（可修复）",
            "the audit chain verifies; the file ends with a torn half-line from a crash (repairable)",
        ),
        VerifyStatus::Broken => pick(lang, "审计链校验失败", "the audit chain does NOT verify"),
    };
    out.push_str(&format!("{verdict}\n"));
    out.push_str(&format!(
        "{}: {} / {}: {} / {}: {}\n",
        pick(lang, "文件存在", "file exists"),
        if report.exists { "yes" } else { "no" },
        pick(lang, "记录", "records"),
        report.records,
        pick(lang, "锚点", "anchor"),
        if report.anchored { "yes" } else { "no" }
    ));
    out.push_str(&format!("  {}\n", report.summary));
    if let Some(breakage) = &report.first_break {
        out.push_str(&format!(
            "  {}: line {} — {} ({})\n     {}\n",
            pick(lang, "第一处不一致", "first inconsistency"),
            breakage.line_no,
            breakage.problem.as_str(),
            breakage.problem.explanation(),
            breakage.detail
        ));
    }
    if report.status == VerifyStatus::Recoverable {
        out.push_str(&format!(
            "  {}\n",
            pick(
                lang,
                "修复：任何写命令（如 apply）都会在打开日志时截掉这半行；只想查看时加 --strict 可把它当失败。",
                "fix: any writing command truncates the torn tail when opening the journal; use --strict to treat it as a failure."
            )
        ));
    }
    out.push_str(&format!(
        "{}: {}\n",
        pick(lang, "待撤销记录", "records pending rollback"),
        report.pending_rollbacks
    ));
    out
}

/// `gopt explain`。
pub fn render_explain(report: &ExplainReport, lang: Lang) -> String {
    let mut out = String::new();
    if let Some(game) = &report.game {
        out.push_str(&format!(
            "{} {} — {} / {} ({})\n",
            pick(lang, "游戏", "game"),
            game.id,
            game.name_zh,
            game.name_en,
            game.origin
        ));
        if !game.description_zh.is_empty() || !game.description_en.is_empty() {
            out.push_str(&format!(
                "  {}\n",
                pick(lang, &game.description_zh, &game.description_en)
            ));
        }
        out.push_str(&format!(
            "  {}: {} / {}: {}\n",
            pick(lang, "exe 匹配", "exe match"),
            game.exe_match,
            pick(lang, "名称别名", "name aliases"),
            if game.name_aliases.is_empty() {
                "-".to_string()
            } else {
                game.name_aliases.join(", ")
            }
        ));
        out.push_str(&format!("{}\n", header(pick(lang, "规则", "rules"))));
        for rule in &game.rules {
            out.push_str(&render_rule(rule, lang));
        }
    }
    if !report.rules.is_empty() {
        out.push_str(&format!(
            "{}: {}\n",
            pick(lang, "命中规则", "matching rules"),
            report.rules.len()
        ));
        for rule in &report.rules {
            out.push_str(&format!("  [{}] {}\n", rule.game_id, rule.rule_id));
            out.push_str(&render_rule(rule, lang));
        }
    }
    if let Some(journal) = &report.journal {
        out.push_str(&format!(
            "{} #{} {} — {}\n",
            pick(lang, "审计记录", "journal record"),
            journal.id,
            journal.kind.as_str(),
            journal.target
        ));
        out.push_str(&format!(
            "  {}: {}\n  {}: {}\n  {}: {}\n  {}: {}\n",
            pick(lang, "规则", "rule"),
            journal.rule_id.as_deref().unwrap_or("-"),
            pick(lang, "自哈希", "self hash"),
            if journal.hash_ok { "ok" } else { "MISMATCH" },
            pick(lang, "已撤销", "undone"),
            if journal.undone { "yes" } else { "no" },
            pick(lang, "撤销方式", "undo"),
            journal.rollback_description
        ));
        out.push_str(&format!(
            "  before {}\n  after  {}\n",
            compact_opt(journal.before.as_ref()),
            compact_opt(journal.after.as_ref())
        ));
        if !journal.actionable {
            out.push_str(&format!(
                "  ! {}\n",
                pick(
                    lang,
                    "该记录不能自动撤销",
                    "this record cannot be undone automatically"
                )
            ));
        }
    }
    for note in &report.notes {
        out.push_str(&format!("  ! {}\n", note.pick(lang)));
    }
    out
}

/// 一条规则的解释块。
fn render_rule(rule: &crate::model::RuleExplanation, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  - {} ({}:{}): {}\n",
        rule.rule_id,
        pick(lang, "行", "line"),
        rule.rule_line
            .map(|line| line.to_string())
            .unwrap_or_else(|| "-".to_string()),
        pick(lang, &rule.condition_zh, &rule.condition_en)
    ));
    out.push_str(&format!(
        "    {}: {}\n    {}: {} {}{}\n",
        pick(lang, "动作", "action"),
        pick(lang, &rule.action_zh, &rule.action_en),
        pick(lang, "调用", "calls"),
        rule.hal_op
            .as_deref()
            .unwrap_or(pick(lang, "（无系统调用）", "(no system call)")),
        match rule.condition_met {
            Some(true) => pick(lang, " [条件满足]", " [condition met]"),
            Some(false) => pick(lang, " [条件不满足]", " [condition not met]"),
            None => "",
        },
        if rule.requires_elevation {
            pick(lang, " [需要管理员]", " [needs elevation]")
        } else {
            ""
        }
    ));
    out
}

/// `gopt prio`。
pub fn render_prio(report: &PrioReport, lang: Lang) -> String {
    let suffix = if report.changed {
        if report.dry_run {
            pick(lang, "（预演，未写入）", " (dry-run, nothing written)").to_string()
        } else {
            match report.journal_id {
                Some(id) => format!(" [written #{id}]"),
                None => String::new(),
            }
        }
    } else {
        pick(lang, "（未变化）", " (unchanged)").to_string()
    };
    format!(
        "pid {}{}: {} {} -> {}{suffix}\n",
        report.pid,
        report
            .name
            .as_ref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default(),
        pick(lang, "优先级", "priority"),
        report.before.as_str(),
        report.after.as_str()
    )
}

/// `gopt tune`。
pub fn render_tune(report: &TuneReport, lang: Lang) -> String {
    let mut out = format!(
        "{}: {} ({})\n",
        pick(lang, "活动电源方案", "active power scheme"),
        report.before.name,
        report.before.guid
    );
    if report.changed {
        out.push_str(&format!(
            "{}: {} -> {}{}\n",
            pick(lang, "切换到", "switch to"),
            report.before.name,
            match (&report.target, report.dry_run) {
                (Some(target), true) => format!("{target} (dry-run)"),
                _ => report.after.name.clone(),
            },
            match report.journal_id {
                Some(id) => format!(" [journal #{id}]"),
                None => String::new(),
            }
        ));
    } else if !report.query_only {
        out.push_str(&format!(
            "{}\n",
            pick(lang, "已经是目标方案", "already at the target scheme")
        ));
    }
    out
}

/// `gopt startup`。
pub fn render_startup(report: &StartupReport, lang: Lang) -> String {
    let mut out = String::new();
    if report.action == "list" {
        out.push_str(&format!(
            "{}: {}\n",
            pick(lang, "开机启动项", "startup entries"),
            report.entries.len()
        ));
        for entry in &report.entries {
            out.push_str(&format!(
                "  [{}] {:<34} {} {}\n",
                entry.hive,
                entry.name,
                if entry.enabled {
                    pick(lang, "启用", "enabled")
                } else {
                    pick(lang, "禁用", "disabled")
                },
                if entry.expandable {
                    "(REG_EXPAND_SZ)"
                } else {
                    ""
                }
            ));
            out.push_str(&format!("      {}\n", entry.command));
        }
        return out;
    }
    out.push_str(&format!(
        "{} {}: {}{}\n",
        pick(lang, "启动项", "run entry"),
        report.action,
        report
            .after
            .as_ref()
            .map(|entry| format!("[{}] {}", entry.hive, entry.name))
            .unwrap_or_else(|| "-".to_string()),
        match (report.changed, report.dry_run, report.journal_id) {
            (false, _, _) => pick(lang, "（未变化）", " (unchanged)").to_string(),
            (true, true, _) =>
                pick(lang, "（预演，未写入）", " (dry-run, nothing written)").to_string(),
            (true, false, Some(id)) => format!(" [journal #{id}]"),
            _ => String::new(),
        }
    ));
    if let (Some(before), Some(after)) = (&report.before, &report.after) {
        out.push_str(&format!(
            "  value_name: {} -> {}\n  enabled: {} -> {}\n",
            before.value_name, after.value_name, before.enabled, after.enabled
        ));
    }
    out
}

/// `gopt import-legacy`。
pub fn render_import(report: &ImportReport, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}{}\n",
        pick(
            lang,
            "旧格式导入（只读解析）",
            "legacy import (read-only parsing)"
        ),
        if report.dry_run {
            pick(lang, " — 预演，未写日志", " — dry-run, nothing written")
        } else {
            ""
        }
    ));
    out.push_str(&format!("  {}\n", report.summary));
    out.push_str(&format!(
        "  savepoints.txt: {} / games.conf: {}\n",
        report.savepoints.summary, report.games_conf.summary
    ));
    out.push_str(&format!(
        "{}: {} / {}: {} / {}: {} / {}: {}\n",
        pick(lang, "解析条目", "parsed drafts"),
        report.drafts,
        pick(lang, "已入链", "imported"),
        report.imported,
        pick(lang, "红线过滤", "policy-filtered"),
        report.policy_filtered,
        pick(lang, "入链 id", "journal ids"),
        if report.journal_ids.is_empty() {
            "-".to_string()
        } else {
            report
                .journal_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<String>>()
                .join(", ")
        }
    ));
    for note in &report.notes {
        out.push_str(&format!("  - {note}\n"));
    }
    out
}

/// `gopt watch` 的一轮。
pub fn render_watch(tick: &WatchTick, lang: Lang) -> String {
    let mut out = format!(
        "{} #{} ({:.0}s) — {} {}\n",
        pick(lang, "监控轮次", "watch tick"),
        tick.tick,
        tick.elapsed_secs,
        tick.matched,
        pick(lang, "个命中进程", "matching processes")
    );
    for item in &tick.items {
        out.push_str(&format!(
            "  {} {} (pid {}) — {} {}{}\n",
            item.game_id,
            item.name_en,
            item.pid,
            item.action,
            if item.applied > 0 {
                format!("({} applied)", item.applied)
            } else {
                String::new()
            },
            match &item.error {
                Some(error) => format!(" ! {error}"),
                None => String::new(),
            }
        ));
    }
    if !tick.continuing {
        out.push_str(&format!(
            "{}\n",
            pick(lang, "（本轮后结束）", "(ends after this tick)")
        ));
    }
    out
}

/// `gopt report`：一整份体检报告。
pub fn render_report(report: &ReportData, lang: Lang, paths: &DataPaths) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} — {}\n\n",
        pick(
            lang,
            "GameOptimizer-RS 体检报告",
            "GameOptimizer-RS health report"
        ),
        paths.describe()
    ));
    out.push_str(&render_status(&report.status, lang));
    out.push_str(&format!("\n{}\n", header(pick(lang, "进程", "Processes"))));
    out.push_str(&format!(
        "{}: {} / {}: {}\n",
        pick(lang, "运行中的进程", "running processes"),
        report.processes,
        pick(lang, "命中策略", "matching a policy"),
        report.running.len()
    ));
    out.push_str(&format!(
        "\n{}\n",
        header(pick(lang, "策略清单", "Policy list"))
    ));
    out.push_str(&render_games(&report.games, lang));
    out.push_str(&format!(
        "\n{}\n",
        header(pick(
            lang,
            "审计日志（最近 20 条）",
            "Audit journal (last 20)"
        ))
    ));
    if report.journal_tail.is_empty() {
        out.push_str(&format!(
            "{}\n",
            pick(lang, "还没有任何审计记录", "no audit records yet")
        ));
    }
    for entry in &report.journal_tail {
        out.push_str(&format!(
            "  #{:<4} {:<9} {:<34} {}\n",
            entry.id,
            entry.kind.as_str(),
            entry.target,
            entry.rule_id.as_deref().unwrap_or("-")
        ));
    }
    out.push_str(&format!(
        "\n{}: {}\n",
        pick(lang, "待撤销记录", "records pending rollback"),
        report.pending_rollbacks
    ));
    out
}

/// 错误块：`kind + message + 可执行建议`（CLI 打印失败时用，绝不打堆栈）。
pub fn render_error(error: &crate::error::CoreError, lang: Lang) -> String {
    let mut out = format!(
        "{} [{}]: {}\n",
        pick(lang, "错误", "error"),
        error.kind().as_str(),
        error.message()
    );
    out.push_str(&format!(
        "  {}: {}\n",
        pick(lang, "建议", "hint"),
        pick(lang, &error.hint_zh(), &error.hint_en())
    ));
    if let Some(hal) = error.hal() {
        if let Some(code) = hal.win32_code() {
            out.push_str(&format!(
                "  win32_code: {code} ({}: {})\n",
                hal.operation(),
                hal.kind()
            ));
        }
    }
    if let Some(journal) = error.journal() {
        if let Some(line) = journal.line_no() {
            out.push_str(&format!("  journal_line: {line}\n"));
        }
    }
    out
}

/// 未使用但保留的语义提示：`mib` 帮助其它渲染函数统一单位。
#[allow(dead_code)]
fn bytes_as_mib(bytes: u64) -> u64 {
    mib(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gopt_hal::{MockApi, SystemApi};

    #[test]
    fn compact_truncates_long_values() {
        let value = serde_json::json!({"a": "x".repeat(300)});
        let text = compact(&value);
        assert!(text.ends_with("..."));
        assert_eq!(compact_opt(None), "-");
        assert_eq!(compact(&serde_json::json!(1)), "1");
    }

    #[test]
    fn hardware_renders_both_languages() {
        let api = MockApi::sample_workstation();
        let hardware = api.hardware().expect("hardware");
        let zh = render_hardware(&hardware, Lang::Zh);
        let en = render_hardware(&hardware, Lang::En);
        assert!(zh.contains("内存:"));
        assert!(en.contains("RAM:"));
        assert!(zh.contains("处理器组"));
        assert!(en.contains("Processor groups"));
    }
}
