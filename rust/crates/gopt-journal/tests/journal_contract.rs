//! `gopt-journal` 端到端契约测试（真实文件系统，6 类场景）：
//!
//! 1. 正常链：追加 → 校验 → 重开 → 一致性；
//! 2. 篡改检出：报出**第一处**不一致的 id 与原因；
//! 3. 尾部半行恢复：崩溃残留被截断，链继续可用；
//! 4. 逆序回滚：计划顺序、类型化解码、红线拒绝、坏链拒绝；
//! 5. 旧格式解析：`savepoints.txt` / `games.conf`，坏行跳过并计数；
//! 6. 旧格式只读性 + 永不 panic。
//!
//! 这里刻意不引入 `tempfile`：临时目录由下方 `TempDir` 手写实现（唯一测试依赖就是 `std`）。

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use gopt_hal::{
    AffinityPlan, Guid, PowerScheme, PriorityClass, RunEntry, RunHive, WorkingSetLimits,
};
use serde_json::json;

use gopt_journal::chain::ChainProblem;
use gopt_journal::{
    import_legacy, payload, plan_rollback_pending, verify_chain, ChainAnchor, Journal,
    JournalDraft, JournalErrorKind, JournalKind, JournalOptions, JournalRecord, LegacyImport,
    RollbackAction, GENESIS_HASH,
};

// ---------------------------------------------------------------------------
// 测试工具
// ---------------------------------------------------------------------------

/// 唯一的临时目录：进程号 + 纳秒 + 计数器 ⇒ 并发/重名都安全；`Drop` 时清理。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|delta| delta.as_nanos())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "gopt-journal-it-{}-{}-{}-{}",
            std::process::id(),
            nanos,
            COUNTER.fetch_add(1, Ordering::Relaxed),
            label
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).expect("read file as UTF-8")
}

fn read_bytes(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read file")
}

fn write_text(path: &Path, text: &str) {
    fs::write(path, text).expect("write file");
}

/// 追加原始字节（模拟"进程在 write_all 中途被杀"）。
fn append_bytes(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open for append");
    file.write_all(bytes).expect("append bytes");
    file.sync_all().expect("sync");
}

fn lines_of(path: &Path) -> Vec<String> {
    read_text(path).lines().map(str::to_string).collect()
}

/// 改文件里第 `index`（0 起）行。
fn patch_line(path: &Path, index: usize, patch: impl FnOnce(&str) -> String) {
    let mut lines = lines_of(path);
    let patched = patch(&lines[index]);
    lines[index] = patched;
    write_text(path, &format!("{}\n", lines.join("\n")));
}

fn delete_line(path: &Path, index: usize) {
    let mut lines = lines_of(path);
    lines.remove(index);
    write_text(path, &format!("{}\n", lines.join("\n")));
}

/// 第 `index` 条的标准记录草稿（apply + 优先级）。
fn priority_draft(index: u64) -> JournalDraft {
    let pid = 4000 + index as u32;
    JournalDraft::at(
        1_700_000_000_000 + index as i64,
        JournalKind::Apply,
        payload::pid_target(pid),
    )
    .with_before(payload::priority(
        pid,
        Some("game.exe"),
        PriorityClass::Normal,
    ))
    .with_after(payload::priority(
        pid,
        Some("game.exe"),
        PriorityClass::High,
    ))
    .with_rule_id(format!("policy:test/{index}"))
}

/// 建一个含 `count` 条记录的日志，返回 (路径, 记录)。
fn seed_journal(dir: &TempDir, name: &str, count: u64) -> (PathBuf, Vec<JournalRecord>) {
    let path = dir.file(name);
    let mut journal = Journal::open(&path).expect("open journal");
    let mut records = Vec::new();
    for index in 1..=count {
        records.push(journal.append(priority_draft(index)).expect("append"));
    }
    (path, records)
}

/// 恶意但"自洽"的重写：改字段并重算它自己的哈希。
fn self_consistent_rewrite(line: &str, mutate: impl FnOnce(&mut JournalRecord)) -> String {
    let mut record: JournalRecord = serde_json::from_str(line).expect("line parses");
    mutate(&mut record);
    record.hash = record.compute_hash();
    record.to_canonical_line()
}

// ---------------------------------------------------------------------------
// 场景 1：正常链
// ---------------------------------------------------------------------------

#[test]
fn scenario_1_clean_chain_appends_and_verifies() {
    let dir = TempDir::new("clean-chain");
    let path = dir.file("journal.jsonl");

    // 不存在的文件 = 空日志（不创建目录、不创建文件）。
    let mut journal = Journal::open(&path).expect("open missing file");
    assert!(!journal.file_exists());
    assert_eq!(journal.record_count(), 0);
    assert_eq!(journal.next_id(), 1);
    assert_eq!(journal.last_hash(), GENESIS_HASH);
    assert!(journal.verify_chain().is_ok());
    assert!(journal.anchor().is_well_formed());
    assert!(!path.exists(), "open() must not create anything");

    let drafts = vec![
        priority_draft(1),
        JournalDraft::at(
            1_700_000_000_010,
            JournalKind::Apply,
            payload::pid_target(4242),
        )
        .with_before(payload::affinity(4242, Some("cs2.exe"), &sample_plan()))
        .with_rule_id("policy:affinity"),
        JournalDraft::at(1_700_000_000_020, JournalKind::Apply, "power-scheme")
            .with_before(payload::power_scheme(&PowerScheme::new(
                Guid::BALANCED,
                "Balanced",
            )))
            .with_after(payload::power_scheme(&PowerScheme::new(
                Guid::HIGH_PERFORMANCE,
                "High performance",
            )))
            .with_rule_id("policy:power"),
        JournalDraft::at(
            1_700_000_000_030,
            JournalKind::Apply,
            payload::run_target(RunHive::CurrentUser, "Steam"),
        )
        .with_before(payload::run_entry(&RunEntry::from_registry(
            RunHive::CurrentUser,
            "Steam",
            r"C:\steam.exe",
            false,
        )))
        .with_rule_id("policy:run"),
    ];
    let appended = journal.append_all(&drafts).expect("append all");
    assert_eq!(appended.len(), 4);
    assert_eq!(appended[0].id, 1);
    assert_eq!(appended[3].id, 4);
    assert_eq!(appended[0].prev_hash, GENESIS_HASH);
    assert_eq!(appended[1].prev_hash, appended[0].hash);

    let report = journal.verify_chain();
    assert!(report.is_ok(), "{}", report.summary());
    assert_eq!(report.verified, 4);
    assert_eq!(report.total, 4);
    assert!(report.summary().starts_with("hash chain verified"));

    // 文件形态：4 行、每行一个规范行、以 \n 结尾、没有尾部残留。
    let text = read_text(&path);
    assert!(text.ends_with('\n'));
    assert_eq!(lines_of(&path).len(), 4);
    for (record, line) in journal.records().zip(lines_of(&path)) {
        assert_eq!(record.to_canonical_line(), line);
        assert_eq!(record.hash.len(), 64);
        assert!(record.has_valid_hash());
    }
    assert!(journal.tail_repair().is_none());
    assert_eq!(journal.malformed_lines().count(), 0);

    // 重开：哈希逐字节一致（规范形式与实现细节无关）。
    let reopened = Journal::open(&path).expect("reopen");
    assert!(reopened.file_exists());
    assert_eq!(reopened.record_count(), 4);
    assert!(reopened.verify_chain().is_ok());
    assert!(reopened.verification_on_open().is_ok());
    let hashes: Vec<String> = journal.records().map(|r| r.hash.clone()).collect();
    let rehashes: Vec<String> = reopened.records().map(|r| r.hash.clone()).collect();
    assert_eq!(hashes, rehashes);
    let reopened_records = reopened.records_owned();
    assert_eq!(reopened.anchor(), ChainAnchor::of(&reopened_records));
    assert_eq!(reopened.anchor().len, 4);
    assert_eq!(reopened.anchor().last_hash, hashes[3]);
    assert!(reopened
        .verify_chain_with_anchor(&ChainAnchor {
            len: 4,
            last_hash: hashes[3].clone(),
        })
        .is_ok());

    // 重开后继续追加：id 从 5 接着走，链仍然完好（跨进程连续性）。
    let mut reopened = reopened;
    let fifth = reopened
        .append(priority_draft(5))
        .expect("append after reopen");
    assert_eq!(fifth.id, 5);
    assert_eq!(fifth.prev_hash, hashes[3]);
    assert_eq!(reopened.record_count(), 5);
    assert!(Journal::open(&path).expect("reopen").verify_chain().is_ok());

    // 外部锚点：先记下"4 条时的锚点"，之后追加两条，锚点校验仍然通过。
    let anchor = ChainAnchor {
        len: 4,
        last_hash: hashes[3].clone(),
    };
    reopened.append(priority_draft(6)).expect("append 6");
    let report = reopened.verify_chain_with_anchor(&anchor);
    assert!(report.is_ok(), "{}", report.summary());
    assert!(report.anchored);
}

fn sample_plan() -> AffinityPlan {
    AffinityPlan::reserve_last_n_cores(32, 4).expect("plan")
}

// ---------------------------------------------------------------------------
// 场景 2：篡改检出
// ---------------------------------------------------------------------------

#[test]
fn scenario_2_tamper_detection_reports_first_inconsistency() {
    let dir = TempDir::new("tamper");

    // (a) 原地改字段（连哈希一起过时）⇒ record_hash。
    let (path, _records) = seed_journal(&dir, "a.jsonl", 4);
    patch_line(&path, 1, |line| {
        line.replace("\"target\":\"pid:4002\"", "\"target\":\"pid:9999\"")
    });
    let journal = Journal::open_with(&path, JournalOptions::read_only()).expect("open");
    let report = journal.verify_chain();
    let breach = report.first().expect("tampered field must be detected");
    assert_eq!(breach.id, Some(2));
    assert_eq!(breach.line_no, 2);
    assert_eq!(breach.problem, ChainProblem::RecordHash);
    assert_eq!(report.verified, 1);
    assert!(report.summary().contains("#2"));
    assert!(breach.detail.contains("stored hash"));

    // (b) 改 id ⇒ id_sequence。
    let (path, _) = seed_journal(&dir, "b.jsonl", 4);
    patch_line(&path, 2, |line| line.replace("\"id\":3,", "\"id\":4,"));
    let breach = Journal::open_with(&path, JournalOptions::read_only())
        .expect("open")
        .verify_chain()
        .first()
        .cloned()
        .expect("id gap");
    assert_eq!(breach.problem, ChainProblem::IdSequence);
    assert_eq!(breach.id, Some(4));
    assert!(breach.detail.contains("expected id 3"));

    // (c) 删中间一行 ⇒ id_sequence（prev_hash 也断了，但 id 检查在前）。
    let (path, _) = seed_journal(&dir, "c.jsonl", 4);
    delete_line(&path, 1);
    let breach = Journal::open_with(&path, JournalOptions::read_only())
        .expect("open")
        .verify_chain()
        .first()
        .cloned()
        .expect("deleted line");
    assert_eq!(breach.problem, ChainProblem::IdSequence);
    assert_eq!(breach.id, Some(3));

    // (d) 攻击者改字段并重算自己的哈希 ⇒ 下一条的链接断开。
    let (path, _) = seed_journal(&dir, "d.jsonl", 4);
    patch_line(&path, 1, |line| {
        self_consistent_rewrite(line, |record| {
            record.target = "pid:9999".to_string();
        })
    });
    let journal = Journal::open_with(&path, JournalOptions::read_only()).expect("open");
    let breach = journal.verify_chain().first().cloned().expect("link break");
    assert_eq!(breach.problem, ChainProblem::PrevHashLink);
    assert_eq!(breach.id, Some(3));
    assert!(
        journal.plan_rollback(4).is_err(),
        "broken chain must not plan"
    );
    assert!(journal
        .plan_rollback(4)
        .expect_err("refuse")
        .is_chain_broken());

    // (e) 注入未知字段 ⇒ 行不符合 schema。
    let (path, _) = seed_journal(&dir, "e.jsonl", 3);
    patch_line(&path, 0, |line| {
        line.replace(",\"hash\":", ",\"extra\":1,\"hash\":")
    });
    let breach = Journal::open_with(&path, JournalOptions::read_only())
        .expect("open")
        .verify_chain()
        .first()
        .cloned()
        .expect("schema break");
    assert_eq!(breach.problem, ChainProblem::MalformedLine);
    assert_eq!(breach.line_no, 1);

    // (f) 重新格式化（键之间加空格）⇒ 行字节 != 规范形式。
    let (path, _) = seed_journal(&dir, "f.jsonl", 3);
    patch_line(&path, 1, |line| line.replace(',', ", "));
    let breach = Journal::open_with(&path, JournalOptions::read_only())
        .expect("open")
        .verify_chain()
        .first()
        .cloned()
        .expect("canonical break");
    assert_eq!(breach.problem, ChainProblem::LineCanonical);
    assert_eq!(breach.id, Some(2));
    assert!(breach.detail.contains("byte"));

    // (g) 链首 prev_hash 被改（并自洽重算）⇒ genesis_prev_hash。
    let (path, _) = seed_journal(&dir, "g.jsonl", 3);
    patch_line(&path, 0, |line| {
        self_consistent_rewrite(line, |record| {
            record.prev_hash = "f".repeat(64);
        })
    });
    let breach = Journal::open_with(&path, JournalOptions::read_only())
        .expect("open")
        .verify_chain()
        .first()
        .cloned()
        .expect("genesis break");
    assert_eq!(breach.problem, ChainProblem::GenesisPrevHash);

    // (h) 尾部被整行删除：文件内自洽（诚实说明的能力边界），外部锚点检出。
    let (_path, records) = seed_journal(&dir, "h.jsonl", 4);
    let anchor = ChainAnchor::of(&records);
    let truncated = &records[..2];
    assert!(
        verify_chain(truncated).is_ok(),
        "a deleted tail cannot be detected from the file alone"
    );
    let report = gopt_journal::verify_chain_with_anchor(truncated, &anchor);
    let breach = report.first().expect("anchor must catch the removed tail");
    assert_eq!(breach.problem, ChainProblem::AnchorLength);
    assert!(report.anchored);

    // (i) 锚点位置被"自洽重写"（改字段 + 重算哈希 + 截断）：文件内仍然合法，锚点哈希检出。
    let path = dir.file("i.jsonl");
    let mut lines: Vec<String> = records
        .iter()
        .map(JournalRecord::to_canonical_line)
        .collect();
    let anchor = ChainAnchor {
        len: 3,
        last_hash: records[2].hash.clone(),
    };
    lines[2] = self_consistent_rewrite(&lines[2], |record| {
        record.target = "pid:1".to_string();
    });
    lines.truncate(3);
    write_text(&path, &format!("{}\n", lines.join("\n")));
    let journal = Journal::open_with(&path, JournalOptions::read_only()).expect("open");
    assert!(
        journal.verify_chain().is_ok(),
        "a self-consistent replacement is invisible without an anchor"
    );
    let breach = journal
        .verify_chain_with_anchor(&anchor)
        .first()
        .cloned()
        .expect("anchor hash break");
    assert_eq!(breach.problem, ChainProblem::AnchorHash);
    assert_eq!(breach.id, Some(3));
    assert!(breach.detail.contains("anchored hash"));
}

// ---------------------------------------------------------------------------
// 场景 3：尾部半行恢复
// ---------------------------------------------------------------------------

#[test]
fn scenario_3_torn_tail_is_truncated_and_recovered() {
    let dir = TempDir::new("torn-tail");
    let (path, records) = seed_journal(&dir, "journal.jsonl", 3);
    let clean_len = fs::metadata(&path).expect("metadata").len();
    let last_hash = records[2].hash.clone();

    // 模拟崩溃：写入半行（没有换行结尾）。
    let torn = b"{\"id\":4,\"ts_unix_ms\":1700000000";
    append_bytes(&path, torn);
    assert_eq!(
        fs::metadata(&path).expect("metadata").len(),
        clean_len + torn.len() as u64
    );

    let mut journal = Journal::open(&path).expect("open with torn tail");
    let repair = journal.tail_repair().expect("torn tail must be reported");
    assert_eq!(repair.bytes_truncated, torn.len() as u64);
    assert!(repair.raw.starts_with("{\"id\":4"));
    assert_eq!(journal.record_count(), 3);
    assert_eq!(journal.line_count(), 3);
    assert!(journal.verify_chain().is_ok());
    assert_eq!(fs::metadata(&path).expect("metadata").len(), clean_len);
    assert!(read_text(&path).ends_with('\n'));

    // 崩溃的那条记录从未提交 ⇒ 下一个 id 仍然是 4，链继续。
    let next = journal
        .append(priority_draft(4))
        .expect("append after repair");
    assert_eq!(next.id, 4);
    assert_eq!(next.prev_hash, last_hash);
    assert!(Journal::open(&path).expect("reopen").verify_chain().is_ok());
    assert!(Journal::open(&path)
        .expect("reopen")
        .tail_repair()
        .is_none());

    // 半行即使"看起来是完整 JSON"，只要没有换行结尾也一律丢弃（否则下一条会粘在同一行）。
    let path = dir.file("valid-not-terminated.jsonl");
    let (seeded, records) = seed_journal(&dir, "valid-not-terminated.jsonl", 2);
    assert_eq!(seeded, path);
    let extra = records[1].to_canonical_line();
    append_bytes(&path, extra.as_bytes());
    let journal = Journal::open(&path).expect("open");
    assert_eq!(journal.record_count(), 2);
    assert_eq!(
        journal.tail_repair().expect("repair").bytes_truncated,
        extra.len() as u64
    );
    assert!(journal.verify_chain().is_ok());

    // 只读打开：不修改文件、明确报出被截断的行、并且拒绝追加。
    let path = dir.file("readonly.jsonl");
    let (_seeded, _records) = seed_journal(&dir, "readonly.jsonl", 2);
    append_bytes(&path, b"{\"id\":3,\"ts");
    let len_before = fs::metadata(&path).expect("metadata").len();
    let mut read_only = Journal::open_with(&path, JournalOptions::read_only()).expect("open");
    assert_eq!(fs::metadata(&path).expect("metadata").len(), len_before);
    assert!(read_only.tail_repair().is_none());
    let breach = read_only
        .verify_chain()
        .first()
        .cloned()
        .expect("torn tail must be reported, not repaired");
    assert_eq!(breach.problem, ChainProblem::MalformedLine);
    assert!(breach.detail.contains("torn tail"));
    let err = read_only
        .append(priority_draft(3))
        .expect_err("a broken chain must not be extended");
    assert_eq!(err.kind(), JournalErrorKind::ChainBroken);

    // 空行被跳过、不占行号，也不破坏链。
    let path = dir.file("blank-lines.jsonl");
    let (_seeded, _records) = seed_journal(&dir, "blank-lines.jsonl", 2);
    let text = read_text(&path);
    let lines: Vec<&str> = text.lines().collect();
    write_text(&path, &format!("{}\n\n{}\n", lines[0], lines[1]));
    let journal = Journal::open(&path).expect("open");
    assert_eq!(journal.skipped_blank_lines(), 1);
    assert_eq!(journal.record_count(), 2);
    assert!(journal.verify_chain().is_ok());

    // 原子替换：内容不变（逐字节），坏链不允许被"压缩"。
    let (path, rewrite_records) = seed_journal(&dir, "rewrite.jsonl", 3);
    let before = read_bytes(&path);
    let mut journal = Journal::open(&path).expect("open");
    journal.rewrite_atomic().expect("atomic rewrite");
    assert_eq!(read_bytes(&path), before);
    assert!(Journal::open(&path).expect("reopen").verify_chain().is_ok());

    let mut bad = rewrite_records.clone();
    bad[1].target = "pid:1".to_string();
    let err = journal
        .rewrite_atomic_with(&bad)
        .expect_err("a broken chain must not be written back");
    assert_eq!(err.kind(), JournalErrorKind::ChainBroken);
    assert_eq!(read_bytes(&path), before, "file must be left untouched");
    assert!(
        !dir.file("rewrite.jsonl.tmp").exists(),
        "temp file must be cleaned up"
    );

    // 外部改写检测：打开后文件长度变化 ⇒ 追加返回 stale。
    let path = dir.file("stale.jsonl");
    let (_seeded, _records) = seed_journal(&dir, "stale.jsonl", 2);
    let mut journal = Journal::open(&path).expect("open");
    append_bytes(&path, b"# someone else appended without a newline\n");
    let err = journal.append(priority_draft(3)).expect_err("stale");
    assert_eq!(err.kind(), JournalErrorKind::Stale);
    assert!(err.message().contains("changed on disk"));

    // 长链 + 追加批次：一次性写入的多条记录仍然逐行可校验。
    let path = dir.file("batch.jsonl");
    let mut journal = Journal::open(&path).expect("open");
    let drafts: Vec<JournalDraft> = (1..=32).map(priority_draft).collect();
    journal.append_all(&drafts).expect("batch append");
    assert_eq!(lines_of(&path).len(), 32);
    let reopened = Journal::open(&path).expect("reopen");
    let report = reopened.verify_chain();
    assert!(report.is_ok(), "{}", report.summary());
    assert_eq!(report.verified, 32);
    assert_eq!(reopened.record_count(), 32);
}

// ---------------------------------------------------------------------------
// 场景 4：逆序回滚
// ---------------------------------------------------------------------------

#[test]
fn scenario_4_rollback_plan_is_reverse_and_typed() {
    let dir = TempDir::new("rollback");
    let path = dir.file("journal.jsonl");
    let mut journal = Journal::open(&path).expect("open");

    let limits = WorkingSetLimits::from_mb(512, 2048).expect("limits");
    let plan = sample_plan();
    let run_entry = RunEntry::from_registry(RunHive::CurrentUser, "Steam", r"C:\steam.exe", false);

    // 修改前的"真实状态"（= 回滚必须恢复到的值）。
    let expected_priority = PriorityClass::Normal;
    let expected_power = PowerScheme::new(Guid::BALANCED, "Balanced");

    let drafts = vec![
        JournalDraft::at(
            1_700_000_000_001,
            JournalKind::Apply,
            payload::pid_target(4242),
        )
        .with_before(payload::priority(4242, Some("cs2.exe"), expected_priority))
        .with_after(payload::priority(
            4242,
            Some("cs2.exe"),
            PriorityClass::High,
        ))
        .with_rule_id("policy:priority"),
        JournalDraft::at(
            1_700_000_000_002,
            JournalKind::Apply,
            payload::pid_target(4242),
        )
        .with_before(payload::affinity(4242, Some("cs2.exe"), &plan))
        .with_rule_id("policy:affinity"),
        JournalDraft::at(
            1_700_000_000_003,
            JournalKind::Apply,
            payload::pid_target(4242),
        )
        .with_before(payload::working_set(4242, Some("cs2.exe"), limits))
        .with_rule_id("policy:working-set"),
        JournalDraft::at(1_700_000_000_004, JournalKind::Apply, "power-scheme")
            .with_before(payload::power_scheme(&expected_power))
            .with_rule_id("policy:power"),
        JournalDraft::at(
            1_700_000_000_005,
            JournalKind::Apply,
            payload::run_target(RunHive::CurrentUser, "Steam"),
        )
        .with_before(payload::run_entry(&run_entry))
        .with_rule_id("policy:run"),
        // 已执行的回滚条目：只入审计，不进计划。
        JournalDraft::at(
            1_700_000_000_006,
            JournalKind::Rollback,
            payload::pid_target(4242),
        )
        .with_rule_id("rollback:1"),
        // 没有 before：显式列为不可执行。
        JournalDraft::at(1_700_000_000_007, JournalKind::Apply, "pid:7"),
    ];
    let records = journal.append_all(&drafts).expect("append all");
    assert_eq!(records.len(), 7);
    assert_eq!(journal.record_count(), 7);

    let plan = journal.plan_rollback(5).expect("plan");
    assert_eq!(plan.to_id, 5);
    assert_eq!(plan.source_records, 5);
    assert_eq!(
        plan.len(),
        5,
        "rollback markers and out-of-range records stay out"
    );
    let ids: Vec<u64> = plan.steps.iter().map(|step| step.journal_id).collect();
    assert_eq!(ids, vec![5, 4, 3, 2, 1], "steps must be newest-first");

    // 每一步恢复的都是"写入前的值"。
    assert_eq!(
        plan.steps[4].action,
        RollbackAction::RestorePriority {
            pid: 4242,
            name: Some("cs2.exe".to_string()),
            priority: expected_priority,
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
        plan.steps[2].action,
        RollbackAction::RestoreWorkingSet {
            pid: 4242,
            name: Some("cs2.exe".to_string()),
            limits,
        }
    );
    assert_eq!(
        plan.steps[1].action,
        RollbackAction::RestorePowerScheme {
            guid: Guid::BALANCED,
            name: Some("Balanced".to_string()),
        }
    );
    assert_eq!(
        plan.steps[0].action,
        RollbackAction::RestoreRunEntry {
            hive: RunHive::CurrentUser,
            value_name: "Steam".to_string(),
            enabled: true,
        }
    );
    assert!(plan
        .steps
        .iter()
        .all(|step| step.kind == JournalKind::Apply));
    assert_eq!(plan.actionable(), 5);
    assert_eq!(plan.not_actionable(), 0);
    assert!(plan.summary().contains("5 executable"));
    assert!(plan.steps[0].describe().contains("Steam"));

    // 计划可 JSON 往返（CLI 的 --json 输出直接吃这个结构）。
    let text = serde_json::to_string(&plan).expect("plan to json");
    let parsed: gopt_journal::RollbackPlan = serde_json::from_str(&text).expect("plan from json");
    assert_eq!(parsed, plan);

    // 全量计划包含"没有 before"的那条（显式不可执行）。
    let all = journal.plan_rollback_all().expect("plan all");
    assert_eq!(all.to_id, 7);
    assert_eq!(all.len(), 6);
    assert_eq!(all.not_actionable(), 1);
    let blocked = all
        .steps
        .iter()
        .find(|step| step.journal_id == 7)
        .expect("step 7");
    assert!(!blocked.action.is_executable());
    assert!(blocked.action.describe().contains("no `before` payload"));

    // 已撤销的 apply 不再出现在"待撤销"计划里（回滚记录的 rule_id = rollback:1）。
    let pending = plan_rollback_pending(&journal.records_owned()).expect("pending");
    assert!(pending.steps.iter().all(|step| step.journal_id != 1));
    assert_eq!(pending.len(), 5);

    // 不存在的 id ⇒ 参数非法。
    let err = journal.plan_rollback(99).expect_err("no such id");
    assert_eq!(err.kind(), JournalErrorKind::InvalidArgument);

    // 空日志 ⇒ 空计划（不是错误）。
    let empty = Journal::open(dir.file("empty.jsonl")).expect("open");
    let plan = empty.plan_rollback_all().expect("empty plan");
    assert!(plan.is_empty());
    assert_eq!(plan.to_id, 0);
}

// ---------------------------------------------------------------------------
// 场景 5：旧格式解析（含坏行跳过）
// ---------------------------------------------------------------------------

const LEGACY_VALID_1: &str = "4242|32|0x000000000000000f|536870912|2147483648|1|1|1|\
                              381b4222-f694-41f0-9685-ff5bb260df2e|1700000000000|cs2.exe|optimized run";
const LEGACY_VALID_2: &str = "777|128|0x000000000000ffff|0|0|1|0|1|\
                              8c5e7fda-e8bf-4a96-9a85-a6e2638c635c|1700000001000|delta force|high performance";
const LEGACY_VALID_3: &str =
    "888|256|0x00000000ffffffff|1048576|0|1|1|0||1700000002000|realtime game|legacy realtime";

fn write_legacy_files(dir: &TempDir) -> (PathBuf, PathBuf) {
    let savepoints = dir.file("savepoints.txt");
    let games_conf = dir.file("games.conf");
    write_text(
        &savepoints,
        &format!(
            "{LEGACY_VALID_1}\n\
             -5|32|0|0|0|1|1|0|guid|0|bad pid|negative pid\n\
             \n\
             {LEGACY_VALID_2}\n\
             1|2|3|4\n\
             garbage-line-with-no-separators\n\
             {LEGACY_VALID_3}\r\n"
        ),
    );
    write_text(
        &games_conf,
        "0|C:\\games\\cs2.exe|--novid|1001\n\
         abc|C:\\games\\broken.exe||1111\n\
         -3|C:\\games\\ignored.exe||1111\n\
         2|C:\\games\\delta.exe|-dx12|0000\n",
    );
    (savepoints, games_conf)
}

#[test]
fn scenario_5_legacy_import_parses_and_skips_bad_lines() {
    let dir = TempDir::new("legacy");
    let (savepoints, games_conf) = write_legacy_files(&dir);

    let import = import_legacy(&savepoints, Some(&games_conf));
    let report = import.savepoints();
    assert!(report.exists);
    assert_eq!(report.valid_lines, 3);
    assert_eq!(
        report.bad_lines, 3,
        "negative pid, short line, garbage line"
    );
    assert_eq!(report.blank_lines, 1);
    assert!(report.bytes > 0);
    assert!(report.summary().contains("valid=3"));

    let games = import.games_conf();
    assert!(games.exists);
    assert_eq!(games.valid_lines, 2);
    assert_eq!(games.bad_lines, 1, "`abc` index would throw in C++");
    assert_eq!(games.ignored_lines, 1, "negative index is ignored by C++");

    assert_eq!(import.len(), 5);
    assert_eq!(import.policy_filtered(), 1, "REALTIME priority is filtered");
    assert!(import.summary().contains("savepoints.txt"));
    let notes = import.notes();
    assert!(notes.iter().any(|note| note.contains("line 2 skipped")));
    assert!(notes.iter().any(|note| note.contains("0x100")));

    for draft in import.drafts() {
        assert_eq!(draft.kind, JournalKind::Imported);
        assert!(draft.rule_id.is_some());
        assert!(draft.after.is_none(), "imported records have no `after`");
        assert!(draft.before.is_some());
    }

    // 前 3 条来自 savepoints（保留原时间戳与 pid 目标），后 2 条来自 games.conf。
    let draft = &import.drafts()[0];
    assert_eq!(draft.target, "pid:4242");
    assert_eq!(draft.ts_unix_ms, 1_700_000_000_000);
    assert_eq!(
        draft.rule_id.as_deref(),
        Some(gopt_journal::LEGACY_SAVEPOINTS_RULE_ID)
    );
    let draft = &import.drafts()[2];
    assert_eq!(draft.target, "pid:888");
    let draft = &import.drafts()[3];
    assert_eq!(draft.target, "game:0");
    assert_eq!(
        draft.rule_id.as_deref(),
        Some(gopt_journal::LEGACY_GAMES_CONF_RULE_ID)
    );

    // 红线：REALTIME 只留在 `priority_raw_blocked` 里，没有任何可解码成 REALTIME 的载荷。
    let before = import.drafts()[2].before.clone().expect("before");
    assert!(before.get("priority").is_none());
    assert_eq!(before["priority_raw_blocked"].as_u64(), Some(256));
    assert_eq!(before["affinity_mask"], "0x00000000ffffffff");
    let serialized = serde_json::to_string(import.drafts()).expect("serialize drafts");
    assert!(!serialized.contains("\"realtime\""));
    assert!(!serialized.contains("\"priority\":256"));

    // 入链：导入的记录以 kind=imported 追加，链仍然完好，回滚计划覆盖它们。
    let path = dir.file("journal.jsonl");
    let mut journal = Journal::open(&path).expect("open");
    let appended = journal
        .append_legacy_import(&import)
        .expect("append legacy import");
    assert_eq!(appended.len(), 5);
    assert!(appended.iter().all(|record| record.id >= 1));
    let report = journal.verify_chain();
    assert!(report.is_ok(), "{}", report.summary());

    let plan = journal.plan_rollback_all().expect("plan");
    assert_eq!(plan.len(), 5);
    assert_eq!(
        plan.not_actionable(),
        2,
        "games.conf entries carry no state"
    );
    let legacy_step = plan
        .steps
        .iter()
        .find(|step| step.journal_id == 1)
        .expect("step 1");
    match &legacy_step.action {
        RollbackAction::RestoreLegacySnapshot {
            process_id,
            priority,
            affinity_mask,
            working_set,
            power_scheme_guid,
        } => {
            assert_eq!(*process_id, 4242);
            assert_eq!(*priority, Some(PriorityClass::Normal));
            assert_eq!(*affinity_mask, Some(0xf));
            assert_eq!(
                *working_set,
                Some(WorkingSetLimits::from_mb(512, 2048).expect("limits"))
            );
            assert_eq!(
                power_scheme_guid.as_deref(),
                Some("381b4222-f694-41f0-9685-ff5bb260df2e")
            );
        }
        other => panic!("expected a legacy snapshot step, got {other:?}"),
    }
    assert!(legacy_step
        .note
        .as_deref()
        .unwrap_or("")
        .contains("savepoints.txt"));
    let game_step = plan
        .steps
        .iter()
        .find(|step| step.kind == JournalKind::Imported && step.target.starts_with("game:"))
        .expect("games.conf step");
    assert!(!game_step.action.is_executable());
    assert!(game_step.action.describe().contains("launch preferences"));

    // 缺失的来源：降级为 notes，不 panic。
    let missing = import_legacy(
        &dir.file("does-not-exist.txt"),
        Some(&dir.file("also-missing.conf")),
    );
    assert!(missing.is_empty());
    assert!(!missing.savepoints().exists);
    assert!(missing
        .notes()
        .iter()
        .any(|note| note.contains("does not exist")));
    let no_games = import_legacy(&savepoints, None);
    assert_eq!(no_games.len(), 3);
    assert!(no_games
        .notes()
        .iter()
        .any(|note| note.contains("was not requested")));

    // 坏行超过 3 条时只列出前 3 条原因 + 汇总。
    let many_bad = dir.file("many-bad.txt");
    write_text(&many_bad, &"garbage\n".repeat(6));
    let import = import_legacy(&many_bad, None);
    assert!(import.is_empty());
    assert_eq!(import.savepoints().bad_lines, 6);
    assert!(import
        .notes()
        .iter()
        .any(|note| note.contains("3 more malformed line(s)")));
}

// ---------------------------------------------------------------------------
// 场景 6：旧格式只读性 + 永不 panic
// ---------------------------------------------------------------------------

#[test]
fn scenario_6_legacy_import_is_read_only_and_never_panics() {
    let dir = TempDir::new("read-only");
    let (savepoints, games_conf) = write_legacy_files(&dir);

    let before_savepoints = read_bytes(&savepoints);
    let before_games = read_bytes(&games_conf);
    let before_mtime = fs::metadata(&savepoints).expect("metadata").modified().ok();
    let before_listing = listing(dir.path());

    let first: LegacyImport = import_legacy(&savepoints, Some(&games_conf));
    let second: LegacyImport = import_legacy(&savepoints, Some(&games_conf));

    // 逐字节只读：内容、mtime、目录清单都不变。
    assert_eq!(read_bytes(&savepoints), before_savepoints);
    assert_eq!(read_bytes(&games_conf), before_games);
    assert_eq!(
        fs::metadata(&savepoints).expect("metadata").modified().ok(),
        before_mtime
    );
    assert_eq!(listing(dir.path()), before_listing);
    assert!(
        !dir.file("journal.jsonl").exists(),
        "import must not create a journal"
    );

    // 幂等：两次导入的草稿完全一致（顺序稳定）。
    assert_eq!(first.drafts(), second.drafts());
    assert_eq!(first, second);

    // 二进制垃圾：有损解码 + 坏行计数，不 panic。
    let binary = dir.file("binary.txt");
    let mut bytes: Vec<u8> = vec![0xff, 0xfe, 0x00, 0x01];
    bytes.extend_from_slice(b"|junk|line\n4242|32|0|0|0|1|1|1|");
    bytes.extend_from_slice(&[0xc3, 0x28, 0x0a]);
    fs::write(&binary, &bytes).expect("write binary");
    let import = import_legacy(&binary, None);
    assert!(import.savepoints().exists);
    assert!(import.savepoints().bad_lines >= 1);
    assert!(import
        .notes()
        .iter()
        .any(|note| note.contains("not valid UTF-8")));

    // 传入目录路径：降级为 note，不 panic。
    let import = import_legacy(dir.path(), Some(dir.path()));
    assert!(import.is_empty());
    assert_eq!(import.notes().len(), 2);
    assert!(import
        .notes()
        .iter()
        .all(|note| note.contains("cannot be read")));

    // 空文件：既不是坏行也不是有效行。
    let empty = dir.file("empty.txt");
    write_text(&empty, "");
    let import = import_legacy(&empty, None);
    assert!(import.is_empty());
    assert_eq!(import.savepoints().bad_lines, 0);
    assert_eq!(import.savepoints().blank_lines, 0);

    // 默认路径的导入只读有界：不创建 `%LOCALAPPDATA%\GameOptimizer`（未存在时）。
    let default_dir = Journal::default_dir();
    assert!(Journal::default_path().ends_with("journal.jsonl"));
    assert!(Journal::default_path().starts_with(&default_dir));
    let existed_before = default_dir.exists();
    let default_import = gopt_journal::import_legacy_default();
    let default_again = gopt_journal::import_legacy_default();
    assert_eq!(
        default_import, default_again,
        "default import must be stable"
    );
    assert_eq!(
        default_dir.exists(),
        existed_before,
        "the read-only import must not create the data directory"
    );
    let (sp, gc) = gopt_journal::legacy_paths();
    assert!(sp.ends_with("savepoints.txt"));
    assert!(gc.ends_with("games.conf"));
}

/// 目录清单（排序后），用于证明"导入没有写任何文件"。
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// 额外守卫：schema 稳定性（同一输入 ⇒ 同一字节）
// ---------------------------------------------------------------------------

#[test]
fn schema_is_stable_across_object_key_order() {
    let first = JournalDraft::at(1, JournalKind::Apply, "pid:1")
        .with_before(json!({"b": 2, "a": 1, "pid": 1, "priority": "normal"}))
        .into_record(1, GENESIS_HASH);
    let second = JournalDraft::at(1, JournalKind::Apply, "pid:1")
        .with_before(json!({"priority": "normal", "pid": 1, "a": 1, "b": 2}))
        .into_record(1, GENESIS_HASH);
    assert_eq!(first.to_canonical_line(), second.to_canonical_line());
    assert_eq!(first.hash, second.hash);
    assert!(!first.to_canonical_line().contains(", "));
    assert!(first.body_json().starts_with("{\"id\":1,"));
    assert!(!first.body_json().contains("\"hash\""));

    // 记录/草稿/计划/报告都能 JSON 往返（CLI --json 的契约）。
    let text = serde_json::to_string(&first).expect("record json");
    let parsed: JournalRecord = serde_json::from_str(&text).expect("record parse");
    assert_eq!(parsed, first);
    let report = verify_chain(std::slice::from_ref(&first));
    let text = serde_json::to_string(&report).expect("report json");
    let parsed: gopt_journal::ChainReport = serde_json::from_str(&text).expect("report parse");
    assert_eq!(parsed, report);
    let anchor = ChainAnchor::of(std::slice::from_ref(&first));
    let text = serde_json::to_string(&anchor).expect("anchor json");
    let parsed: ChainAnchor = serde_json::from_str(&text).expect("anchor parse");
    assert_eq!(parsed, anchor);
}

// ---------------------------------------------------------------------------
// 额外守卫：错误类型与线程可用性（gopt-core / gopt-cli 的集成前提）
// ---------------------------------------------------------------------------

#[test]
fn api_is_send_and_std_error_compatible() {
    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}
    // 单内核多前端：Journal 会被核心线程持有，记录/报告会在线程间传递。
    assert_send::<Journal>();
    assert_send_sync::<JournalRecord>();
    assert_send::<LegacyImport>();
    assert_send_sync::<gopt_journal::ChainReport>();
    assert_send_sync::<gopt_journal::RollbackPlan>();

    // 日志错误可以塞进 `Box<dyn Error>`（CLI 统一呈现）。
    if let Err(err) =
        Journal::open(std::env::temp_dir().join("gopt-journal-missing-dir/x/journal.jsonl"))
    {
        let boxed: Box<dyn std::error::Error> = Box::new(err);
        let text = boxed.to_string();
        assert!(
            text.starts_with("gopt-journal: "),
            "unexpected message: {text}"
        );
    }

    // 也可以直接当 HAL 错误向上抛（与 HAL 的错误分类对齐）。
    let hal: gopt_hal::HalError =
        gopt_journal::JournalError::invalid_argument("test", "invalid").into();
    assert_eq!(hal.kind(), gopt_hal::HalErrorKind::InvalidArgument);
    assert!(hal.message().contains("invalid"));
}
