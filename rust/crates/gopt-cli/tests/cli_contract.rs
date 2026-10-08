//! 二进制级契约测试：真的把 `gopt.exe` 拉起来，验证**退出码**与 **`--json` 契约**。
//!
//! 为什么要在二进制层测（而不是只测库）：任务书的三条硬要求只有在进程边界上才成立——
//!
//! 1. `--json` 能被 `serde_json` 解析（含 `schema_version`）；
//! 2. 退出码 0/1/2/3 的语义（成功 / 用法错误 / 环境不满足 / 审计链校验失败）；
//! 3. **默认安全**：`apply` 没有 `--yes` 时只打印计划，不写系统、不建日志。
//!
//! 每个用例用自己的临时数据目录（`GOPT_DATA_DIR`），并用 `GOPT_BACKEND=mock` 让 CLI 走 Mock 后端：
//! 这既保证测试不需要管理员、不碰真实配置，也顺带证明了"单内核多前端"里内核是可替换的。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 被测二进制（cargo 在集成测试里提供）。
const BIN: &str = env!("CARGO_BIN_EXE_gopt");

/// 一次性临时目录。
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("gopt-cli-contract-{name}-{}", std::process::id()));
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

/// 一次调用结果。
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// stdout 解析成 JSON（解析失败即 panic，因为"`--json` 必须可解析"就是被测契约）。
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|error| panic!("stdout is not JSON ({error}):\n{}", self.stdout))
    }
}

/// 跑一次 `gopt`（数据目录指向临时目录、后端 = mock）。
fn gopt(dir: &Path, args: &[&str]) -> Run {
    let output = Command::new(BIN)
        .args(args)
        .env("GOPT_DATA_DIR", dir)
        .env("GOPT_BACKEND", "mock")
        .env_remove("GOPT_LANG")
        .output()
        .expect("spawn gopt");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn status_json_is_parseable_and_carries_the_schema_version() {
    let dir = TempDir::new("status");
    let run = gopt(dir.path(), &["status", "--json"]);
    assert_eq!(run.code, 0, "stderr={}", run.stderr);
    let json = run.json();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["command"], "status");
    assert_eq!(json["ok"], true);
    assert_eq!(json["lang"], "zh");
    assert_eq!(json["data"]["backend"], "mock");
    assert_eq!(json["data"]["hardware"]["logical_cores"], 16);
    assert_eq!(json["data"]["journal_records"], 0);
    assert_eq!(json["data"]["chain_ok"], true);
    assert!(json["data"]["policies_total"].as_u64().unwrap_or(0) >= 9);
    assert!(json["error"].is_null());
}

#[test]
fn plan_is_read_only_and_parseable() {
    let dir = TempDir::new("plan");
    let run = gopt(dir.path(), &["plan", "cs2", "--json"]);
    assert_eq!(run.code, 0, "stderr={}", run.stderr);
    let json = run.json();
    assert_eq!(json["data"]["plan"]["game_id"], "cs2");
    assert_eq!(json["data"]["running"], true);
    assert_eq!(json["data"]["plan"]["pid"], 1234);
    let steps = json["data"]["plan"]["steps"].as_array().expect("steps");
    assert!(steps.len() >= 3);
    assert_eq!(steps[0]["rule_id"], "priority");
    assert_eq!(steps[0]["pid"], 1234);
    assert_eq!(steps[0]["requires_elevation"], false);
    assert!(steps[0]["reason"]["zh"].as_str().unwrap_or("").len() > 4);
    // 只读：没有建日志、没有写系统。
    assert!(!dir.journal().exists(), "plan must not create the journal");
}

#[test]
fn apply_without_yes_only_previews() {
    let dir = TempDir::new("apply-default-safe");
    let run = gopt(dir.path(), &["apply", "cs2", "--json"]);
    assert_eq!(run.code, 0, "stderr={}", run.stderr);
    let json = run.json();
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["applied"], 0);
    assert_eq!(json["data"]["failed"], 0);
    assert_eq!(
        json["data"]["journal_ids"].as_array().expect("ids").len(),
        0
    );
    assert!(json["notices"]
        .as_array()
        .expect("notices")
        .iter()
        .any(|notice| notice["en"].as_str().unwrap_or("").contains("dry-run")));
    assert!(
        !dir.journal().exists(),
        "a dry run must not create the journal"
    );
}

#[test]
fn apply_with_yes_writes_the_journal_and_rollback_undoes_it() {
    let dir = TempDir::new("apply-rollback");
    let run = gopt(dir.path(), &["apply", "cs2", "--yes", "--json"]);
    assert_eq!(run.code, 0, "stderr={}", run.stderr);
    let json = run.json();
    assert_eq!(json["data"]["dry_run"], false);
    assert!(json["data"]["applied"].as_u64().expect("applied") >= 3);
    assert_eq!(json["data"]["failed"], 0);
    assert!(
        dir.journal().exists(),
        "apply --yes must write the audit journal"
    );

    // verify-journal：链完好。
    let verify = gopt(dir.path(), &["verify-journal", "--json"]);
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr);
    let json = verify.json();
    assert_eq!(json["data"]["status"], "ok");
    assert_eq!(json["data"]["exists"], true);
    assert!(json["data"]["records"].as_u64().expect("records") >= 3);

    // journal：记录齐全且带规则来源。
    let journal = gopt(dir.path(), &["journal", "--json"]);
    assert_eq!(journal.code, 0);
    let journal_json = journal.json();
    assert_eq!(
        journal_json["data"]["chain"]["first_inconsistency"],
        serde_json::Value::Null
    );
    let entries = journal_json["data"]["entries"].as_array().expect("entries");
    assert_eq!(
        entries.len(),
        journal_json["data"]["records"].as_u64().expect("records") as usize
    );
    assert!(entries[0]["rule_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("policy:game/cs2/"));
    assert!(entries[0]["reversible"].as_bool().expect("reversible"));

    // journal --limit：只留最近 N 条。
    let limited = gopt(dir.path(), &["journal", "--limit", "1", "--json"]);
    assert_eq!(limited.code, 0);
    assert_eq!(
        limited.json()["data"]["entries"]
            .as_array()
            .expect("entries")
            .len(),
        1
    );

    // explain：规则解释与记录解释都可用。
    let rule = gopt(dir.path(), &["explain", "--rule", "priority", "--json"]);
    assert_eq!(rule.code, 0, "stderr={}", rule.stderr);
    assert!(!rule.json()["data"]["rules"]
        .as_array()
        .expect("rules")
        .is_empty());
    let record = gopt(dir.path(), &["explain", "--journal-id", "1", "--json"]);
    assert_eq!(record.code, 0, "stderr={}", record.stderr);
    let record_json = record.json();
    assert_eq!(record_json["data"]["journal"]["id"], 1);
    assert_eq!(record_json["data"]["journal"]["hash_ok"], true);

    // rollback --all（无 --yes：只预演）。
    let preview = gopt(dir.path(), &["rollback", "--all", "--json"]);
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr);
    assert_eq!(preview.json()["data"]["dry_run"], true);
    assert_eq!(preview.json()["data"]["executed"], 0);

    // rollback --all --yes：真的撤销，并追加 rollback 记录。
    let rolled = gopt(dir.path(), &["rollback", "--all", "--yes", "--json"]);
    assert_eq!(rolled.code, 0, "stderr={}", rolled.stderr);
    let rolled_json = rolled.json();
    assert!(rolled_json["data"]["executed"].as_u64().expect("executed") >= 2);
    assert_eq!(rolled_json["data"]["failed"], 0);

    let after = gopt(dir.path(), &["journal", "--kind", "rollback", "--json"]);
    assert_eq!(after.code, 0);
    assert!(
        after.json()["data"]["entries"]
            .as_array()
            .expect("entries")
            .len()
            >= 2
    );
}

#[test]
fn exit_code_3_when_the_audit_chain_does_not_verify() {
    let dir = TempDir::new("tamper");
    assert_eq!(gopt(dir.path(), &["apply", "cs2", "--yes"]).code, 0);

    // 篡改一行（插入一个空格 ⇒ 不再是规范形式）。
    let text = std::fs::read_to_string(dir.journal()).expect("read journal");
    let tampered = text.replacen(",\"kind\":\"apply\"", ", \"kind\":\"apply\"", 1);
    assert_ne!(text, tampered);
    std::fs::write(dir.journal(), tampered).expect("write journal");

    let verify = gopt(dir.path(), &["verify-journal", "--json"]);
    assert_eq!(verify.code, 3, "a broken chain must exit with 3");
    let json = verify.json();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["kind"], "audit_chain_broken");
    assert_eq!(json["data"]["status"], "broken");
    assert!(json["data"]["first_break"]["problem"].is_string());

    // 写路径 fail-closed：退出码同样是 3。
    let apply = gopt(dir.path(), &["apply", "cs2", "--yes", "--json"]);
    assert_eq!(apply.code, 3, "stderr={}", apply.stderr);
    assert_eq!(apply.json()["error"]["kind"], "audit_chain_broken");

    let rollback = gopt(dir.path(), &["rollback", "--all", "--yes", "--json"]);
    assert_eq!(rollback.code, 3);
}

#[test]
fn exit_code_1_for_usage_errors() {
    let dir = TempDir::new("usage");

    let unknown = gopt(dir.path(), &["--nope", "status"]);
    assert_eq!(unknown.code, 1);
    assert!(
        unknown.stderr.contains("unknown"),
        "stderr={}",
        unknown.stderr
    );

    let command = gopt(dir.path(), &["no-such-command"]);
    assert_eq!(command.code, 1);
    assert!(command.stderr.contains("unknown command"));

    let missing = gopt(dir.path(), &["plan"]);
    assert_eq!(missing.code, 1);
    assert!(missing.stderr.contains("gopt plan"));

    let realtime = gopt(dir.path(), &["prio", "--pid", "1", "--set", "realtime"]);
    assert_eq!(
        realtime.code, 1,
        "REALTIME must be rejected before anything runs"
    );
    assert!(realtime.stderr.contains("realtime"));

    let lang = gopt(dir.path(), &["status", "--lang", "de"]);
    assert_eq!(lang.code, 1);
    assert!(lang.stderr.contains("zh or en"));

    // 用法错误在 --json 下也有结构化输出（脚本能统一处理）。
    let json = gopt(dir.path(), &["status", "--nope", "--json"]);
    assert_eq!(json.code, 1);
    let parsed = json.json();
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["kind"], "usage");
}

#[test]
fn exit_code_2_when_the_environment_is_not_satisfied() {
    let dir = TempDir::new("environment");

    let unknown_game = gopt(dir.path(), &["plan", "not-a-game", "--json"]);
    assert_eq!(unknown_game.code, 2, "stderr={}", unknown_game.stderr);
    let json = unknown_game.json();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["kind"], "not_found");
    assert!(json["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("known ids"));

    let unknown_pid = gopt(dir.path(), &["prio", "--pid", "999999", "--json"]);
    assert_eq!(unknown_pid.code, 2);

    // 游戏没在运行时 `apply` 不能"假装成功"。
    let not_running = gopt(
        dir.path(),
        &["apply", "1234", "--pid", "999999", "--yes", "--json"],
    );
    assert_eq!(not_running.code, 2);
    assert_eq!(not_running.json()["error"]["kind"], "not_found");
}

#[test]
fn version_help_and_language_switching() {
    let dir = TempDir::new("meta");
    let version = gopt(dir.path(), &["--version"]);
    assert_eq!(version.code, 0);
    assert!(version.stdout.contains("GameOptimizer-RS"));
    assert!(
        version.stdout.contains("1.1.0"),
        "the C++ release version must be visible"
    );

    let help = gopt(dir.path(), &["--help"]);
    assert_eq!(help.code, 0);
    // 语言跟随 GOPT_LANG（测试里已清空 ⇒ 默认中文）；两种语言的用法都必须列出退出码与全部命令。
    assert!(
        help.stdout.contains("退出码") || help.stdout.contains("exit codes"),
        "{}",
        help.stdout
    );
    assert!(help.stdout.contains("import-legacy"));

    let topic = gopt(dir.path(), &["help", "apply"]);
    assert_eq!(topic.code, 0);
    assert!(topic.stdout.contains("gopt apply"));

    let zh = gopt(dir.path(), &["status"]);
    assert!(zh.stdout.contains("HAL 后端"), "{}", zh.stdout);
    let en = gopt(dir.path(), &["status", "--lang=en"]);
    assert!(en.stdout.contains("HAL backend"), "{}", en.stdout);
    assert!(en.stdout.contains("Data directory"));
}

#[test]
fn list_variants_and_report_work() {
    let dir = TempDir::new("list");
    let games = gopt(dir.path(), &["list", "games", "--json"]);
    assert_eq!(games.code, 0);
    assert!(games.json()["data"].as_array().expect("games").len() >= 9);

    let processes = gopt(dir.path(), &["list", "processes", "--json"]);
    assert_eq!(processes.code, 0);
    assert_eq!(
        processes.json()["data"]
            .as_array()
            .expect("processes")
            .len(),
        3
    );

    let startup = gopt(dir.path(), &["list", "startup", "--json"]);
    assert_eq!(startup.code, 0);
    let startup_json = startup.json();
    let entries = startup_json["data"]["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 3);
    assert!(entries.iter().any(|entry| entry["enabled"] == false));

    // report --out：文本报告落盘。
    let out = dir.path().join("report.txt");
    let report = gopt(
        dir.path(),
        &["report", "--out", out.to_str().expect("utf8")],
    );
    assert_eq!(report.code, 0, "stderr={}", report.stderr);
    let text = std::fs::read_to_string(&out).expect("report file");
    assert!(text.contains("GameOptimizer-RS"));
    assert!(text.contains("策略清单"), "{text}");
}

#[test]
fn tune_startup_and_watch_are_covered() {
    let dir = TempDir::new("tune");
    let tune = gopt(dir.path(), &["tune", "--json"]);
    assert_eq!(tune.code, 0);
    let tune_json = tune.json();
    assert_eq!(tune_json["data"]["query_only"], true);
    assert_eq!(tune_json["data"]["changed"], false);
    assert!(tune_json["data"]["before"]["guid"].is_string());

    // 切换（Mock 上装了「高性能」）→ 写一条审计记录。
    let switched = gopt(
        dir.path(),
        &["tune", "--power-scheme", "high", "--yes", "--json"],
    );
    assert_eq!(switched.code, 0, "stderr={}", switched.stderr);
    assert_eq!(switched.json()["data"]["changed"], true);

    let disable = gopt(
        dir.path(),
        &["startup", "disable", "Steam", "--yes", "--json"],
    );
    assert_eq!(disable.code, 0, "stderr={}", disable.stderr);
    let disable_json = disable.json();
    assert_eq!(disable_json["data"]["changed"], true);
    assert_eq!(
        disable_json["data"]["after"]["value_name"],
        "[disabled] Steam"
    );
    assert_eq!(disable_json["data"]["after"]["enabled"], false);
    assert!(
        disable_json["data"]["journal_id"].is_number(),
        "a disable is an audited change"
    );

    let list = gopt(dir.path(), &["startup", "list", "--json"]);
    assert_eq!(list.code, 0);

    // watch --once --json：JSONL（每轮一行 Outcome）。
    let watch = gopt(dir.path(), &["watch", "--once", "--json"]);
    assert_eq!(watch.code, 0, "stderr={}", watch.stderr);
    let first_line = watch.stdout.lines().next().expect("one tick line");
    let parsed: serde_json::Value = serde_json::from_str(first_line).expect("tick JSON");
    assert_eq!(parsed["command"], "watch");
    assert_eq!(parsed["data"]["tick"], 1);
}

#[test]
fn import_legacy_is_read_only_until_yes() {
    let dir = TempDir::new("import");
    std::fs::write(
        dir.path().join("savepoints.txt"),
        "38760|32|65535|204800|1413120|1|1|1|8c5e7fda-e8bf-4a96-9a85-a6e2638c635c|1700000000000|三角洲行动|before\n",
    )
    .expect("write savepoints");

    let dry = gopt(dir.path(), &["import-legacy", "--json"]);
    assert_eq!(dry.code, 0, "stderr={}", dry.stderr);
    let dry_json = dry.json();
    assert_eq!(dry_json["data"]["dry_run"], true);
    assert_eq!(dry_json["data"]["imported"], 0);
    assert!(dry_json["data"]["drafts"].as_u64().expect("drafts") >= 1);
    assert!(
        !dir.journal().exists(),
        "a dry run must not create the journal"
    );

    let yes = gopt(dir.path(), &["import-legacy", "--yes", "--json"]);
    assert_eq!(yes.code, 0, "stderr={}", yes.stderr);
    let yes_json = yes.json();
    assert!(yes_json["data"]["imported"].as_u64().expect("imported") >= 1);
    assert_eq!(yes_json["data"]["savepoints"]["valid_lines"], 1);

    let verify = gopt(dir.path(), &["verify-journal", "--json"]);
    assert_eq!(verify.code, 0);
    assert_eq!(verify.json()["data"]["status"], "ok");
}
