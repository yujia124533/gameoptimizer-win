//! 真机自检：一次完整的"追加写 → 校验 → 篡改检出 → 尾部半行修复 → 回滚计划 → 旧格式只读导入"往返。
//!
//! ```text
//! cargo run -p gopt-journal --example journal_selfcheck              # 在 %TEMP% 下用临时文件
//! cargo run -p gopt-journal --example journal_selfcheck -- <file>    # 指定 journal.jsonl 路径
//! ```
//!
//! 说明：
//!
//! * 默认在临时目录里跑，不会碰 `%LOCALAPPDATA%\GameOptimizer\journal.jsonl`；
//!   指定路径时请自行确认那是你想写的位置。
//! * 旧格式导入部分是**只读**的：会报告真机上 `savepoints.txt` / `games.conf` 的解析结果，
//!   不会创建目录、不会改写旧文件。
//! * 任何一步不符合预期都以非零退出码结束，便于 CI / 真机验收直接看退出码。

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gopt_hal::{AffinityPlan, Guid, PowerScheme, PriorityClass, RunEntry, RunHive};
use gopt_journal::{
    import_legacy_default, payload, verify_chain, ChainAnchor, Journal, JournalDraft, JournalKind,
    JournalOptions, JournalRecord,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("journal selfcheck FAILED: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let path = std::env::args()
        .nth(1)
        .map_or_else(scratch_path, PathBuf::from);
    println!("== gopt-journal selfcheck ==");
    println!(
        "default journal path : {}",
        Journal::default_path().display()
    );
    println!("selfcheck journal    : {}", path.display());

    // ---- 1) 追加写 + 校验 -------------------------------------------------
    let mut journal = Journal::open(&path).map_err(show)?;
    println!(
        "\n[1] open: exists={} records={} chain={}",
        journal.file_exists(),
        journal.record_count(),
        journal.verify_chain().summary()
    );

    let plan = AffinityPlan::reserve_last_n_cores(32, 4).map_err(show)?;
    let drafts = vec![
        JournalDraft::now(JournalKind::Apply, payload::pid_target(std::process::id()))
            .with_before(payload::priority(
                std::process::id(),
                Some("selfcheck"),
                PriorityClass::Normal,
            ))
            .with_after(payload::priority(
                std::process::id(),
                Some("selfcheck"),
                PriorityClass::High,
            ))
            .with_rule_id("selfcheck:priority"),
        JournalDraft::now(JournalKind::Apply, payload::pid_target(std::process::id()))
            .with_before(payload::affinity(
                std::process::id(),
                Some("selfcheck"),
                &plan,
            ))
            .with_rule_id("selfcheck:affinity"),
        JournalDraft::now(JournalKind::Apply, "power-scheme")
            .with_before(payload::power_scheme(&PowerScheme::new(
                Guid::BALANCED,
                "Balanced",
            )))
            .with_after(payload::power_scheme(&PowerScheme::new(
                Guid::HIGH_PERFORMANCE,
                "High performance",
            )))
            .with_rule_id("selfcheck:power"),
        JournalDraft::now(
            JournalKind::Apply,
            payload::run_target(RunHive::CurrentUser, "Steam"),
        )
        .with_before(payload::run_entry(&RunEntry::from_registry(
            RunHive::CurrentUser,
            "Steam",
            r"C:\steam.exe",
            false,
        )))
        .with_rule_id("selfcheck:run"),
    ];
    let appended = journal.append_all(&drafts).map_err(show)?;
    for record in &appended {
        println!("    {} hash={}", record.tag(), &record.hash[..16]);
    }
    let report = journal.verify_chain();
    println!("    verify: {}", report.summary());
    if !report.is_ok() {
        return Err("a freshly written chain must verify".to_string());
    }
    let anchor = ChainAnchor::of(&journal.records_owned());
    println!(
        "    anchor: len={} last={}",
        anchor.len,
        &anchor.last_hash[..16]
    );

    // ---- 2) 篡改检出 -----------------------------------------------------
    let tamper_path = path.with_extension("tamper.jsonl");
    fs::copy(&path, &tamper_path).map_err(|err| err.to_string())?;
    let mut lines: Vec<String> = fs::read_to_string(&tamper_path)
        .map_err(|err| err.to_string())?
        .lines()
        .map(str::to_string)
        .collect();
    // 找到那条带 `priority: normal` 的记录（不假设它一定在第几行），把字段改掉。
    let Some(index) = lines
        .iter()
        .position(|line| line.contains("\"priority\":\"normal\""))
    else {
        return Err("no `priority: normal` payload found for the tamper check".to_string());
    };
    let original = lines[index].clone();
    lines[index] = original.replace("\"priority\":\"normal\"", "\"priority\":\"idle\"");
    if lines[index] == original {
        return Err("the tamper patch did not change the line".to_string());
    }
    fs::write(&tamper_path, format!("{}\n", lines.join("\n"))).map_err(|err| err.to_string())?;
    let tampered = Journal::open_with(&tamper_path, JournalOptions::read_only()).map_err(show)?;
    let tamper_report = tampered.verify_chain();
    println!(
        "\n[2] tamper: line {} patched; {}",
        index + 1,
        tamper_report.summary()
    );
    match tamper_report.first() {
        Some(breach) => println!(
            "    first inconsistency: id={:?} line={} problem={} detail={}",
            breach.id, breach.line_no, breach.problem, breach.detail
        ),
        None => return Err("tampering must be detected".to_string()),
    }
    if tampered.plan_rollback(anchor.len).is_ok() {
        return Err("a broken chain must not produce a rollback plan".to_string());
    }
    println!("    rollback refused for the broken chain: yes");

    // ---- 3) 尾部半行修复 -------------------------------------------------
    let torn_path = path.with_extension("torn.jsonl");
    fs::copy(&path, &torn_path).map_err(|err| err.to_string())?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&torn_path)
        .map_err(|err| err.to_string())?;
    use std::io::Write as _;
    file.write_all(b"{\"id\":99,\"ts_unix_ms\":1")
        .map_err(|err| err.to_string())?;
    file.sync_all().map_err(|err| err.to_string())?;
    drop(file);
    let repaired = Journal::open(&torn_path).map_err(show)?;
    match repaired.tail_repair() {
        Some(repair) => println!(
            "\n[3] torn tail: truncated {} byte(s) {:?}; records={} chain={}",
            repair.bytes_truncated,
            repair.raw,
            repaired.record_count(),
            repaired.verify_chain().summary()
        ),
        None => return Err("the torn tail must be reported".to_string()),
    }
    if !repaired.verify_chain().is_ok() {
        return Err("the repaired chain must verify".to_string());
    }

    // ---- 4) 逆序回滚计划 -------------------------------------------------
    let plan = journal.plan_rollback_all().map_err(show)?;
    println!("\n[4] rollback plan: {}", plan.summary());
    for step in plan.steps.iter().take(6) {
        println!("    {}", step.describe());
    }

    // ---- 5) 旧格式只读导入（真机数据） -----------------------------------
    let import = import_legacy_default();
    println!("\n[5] legacy import (read-only): {}", import.summary());
    println!("    savepoints: {}", import.savepoints().summary());
    println!("    games.conf: {}", import.games_conf().summary());
    if import.policy_filtered() > 0 {
        println!(
            "    policy filtered REALTIME priority value(s): {}",
            import.policy_filtered()
        );
    }
    for note in import.notes().iter().take(4) {
        println!("    note: {note}");
    }

    // ---- 6) 锚点 + 记录数复核 -------------------------------------------
    let reopened = Journal::open(&path).map_err(show)?;
    let anchor_report = reopened.verify_chain_with_anchor(&anchor);
    println!("\n[6] anchor: {}", anchor_report.summary());
    if !anchor_report.is_ok() {
        return Err("the anchor of the untouched journal must still match".to_string());
    }
    let records: Vec<JournalRecord> = reopened.records_owned();
    let independent = verify_chain(&records);
    println!(
        "    independent verify of {} record(s): {}",
        records.len(),
        independent.summary()
    );

    println!(
        "\nOK — journal written to {} ({} bytes)",
        path.display(),
        fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
    );
    Ok(())
}

/// 默认自检路径：`%TEMP%\gopt-journal-selfcheck-<pid>.jsonl`。
fn scratch_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "gopt-journal-selfcheck-{}-{stamp}.jsonl",
        std::process::id()
    ))
}

/// 统一错误呈现。
fn show(err: impl std::fmt::Display) -> String {
    err.to_string()
}
