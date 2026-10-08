//! 严格校验：非法 TOML / 非法策略必须报"文件名:行号"，并且**永不 panic**。

mod common;

use gopt_hal::HalErrorKind;
use gopt_policy::{
    parse_policy_file, DiagnosticSeverity, PolicyDiagnostic, PolicyLoader, PolicyOrigin,
};

use common::Scratch;

fn parse(text: &str) -> Result<Vec<gopt_policy::GamePolicy>, PolicyDiagnostic> {
    parse_policy_file(text, PolicyOrigin::user(r"C:\tmp\bad.toml"))
}

fn error_at(text: &str, expected_line: u32, expected_fragment: &str) {
    let diagnostic = parse(text).expect_err(&format!("expected an error for:\n{text}"));
    assert_eq!(
        diagnostic.line(),
        Some(expected_line),
        "行号不对（{diagnostic}）:\n{text}"
    );
    assert!(
        diagnostic.message().contains(expected_fragment),
        "消息 `{}` 里没有 `{expected_fragment}`",
        diagnostic.message()
    );
    assert_eq!(diagnostic.severity(), DiagnosticSeverity::Error);
    assert!(diagnostic.to_string().contains("bad.toml:"));
}

#[test]
fn a_valid_file_parses_into_games() {
    let games = parse(
        r#"
schema = 1

[[game]]
id = "demo"
name_zh = "示例"
name_en = "Demo"
match = "demo*.exe"

[[game.rules]]
when = { ram_gb = { gte = 8 } }
action = { priority = { class = "high" } }
"#,
    )
    .expect("valid file");
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].id(), "demo");
    assert_eq!(games[0].rules().len(), 1);
    assert_eq!(games[0].rules()[0].id(), "demo.priority");
    assert_eq!(games[0].rules()[0].line(), Some(10));
}

#[test]
fn syntax_error_carries_the_line() {
    let text = "schema = 1\n\n[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"oops\nmatch = \"a.exe\"\n";
    let diagnostic = parse(text).expect_err("unterminated string");
    assert_eq!(diagnostic.line(), Some(6), "{diagnostic}");
    assert!(
        diagnostic.to_string().contains("bad.toml:6"),
        "{diagnostic}"
    );
}

#[test]
fn unknown_keys_carry_the_line() {
    // 顶层未知键（第 1 行）
    error_at("gamme = []\n", 1, "gamme");
    // 游戏级未知键（第 5 行）
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatchh = \"a.exe\"\n",
        5,
        "matchh",
    );
    // 动作内的未知键（第 3 行）
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\naction = { prioriti = { class = \"high\" } }\n",
        8,
        "prioriti",
    );
    // 条件算子的未知键（第 8 行）
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\nwhen = { ram_gb = { gt3 = 8 } }\naction = { priority = { class = \"high\" } }\n",
        8,
        "gt3",
    );
}

#[test]
fn realtime_priority_is_rejected_by_policy_with_a_line() {
    let text = "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\nid = \"priority\"\naction = { priority = { class = \"realtime\" } }\n";
    let diagnostic = parse(text).expect_err("realtime must be rejected");
    assert_eq!(diagnostic.line(), Some(9), "{diagnostic}");
    assert!(
        diagnostic.message().contains("rejected by policy"),
        "{}",
        diagnostic.message()
    );
    assert!(
        diagnostic.message().to_lowercase().contains("realtime"),
        "{}",
        diagnostic.message()
    );

    // 数值写法 0x100 同样被拦。
    let numeric = text.replace("\"realtime\"", "0x100");
    let diagnostic = parse(&numeric).expect_err("0x100 must be rejected");
    assert_eq!(diagnostic.line(), Some(9), "{diagnostic}");
}

#[test]
fn action_shape_errors_carry_the_line() {
    // 空动作
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\naction = {}\n",
        8,
        "action is empty",
    );
    // 两个动作
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\naction = { priority = { class = \"high\" }, working_set = { min_mb = 128 } }\n",
        8,
        "exactly one of",
    );
    // 工作集缺 min_mb
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\naction = { working_set = { max_mb = 512 } }\n",
        8,
        "requires `min_mb`",
    );
    // min > max
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\naction = { working_set = { min_mb = 512, max_mb = 128 } }\n",
        8,
        "must not be smaller",
    );
}

#[test]
fn affinity_shape_errors_carry_the_line() {
    let head = "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\n";
    // 空 affinity
    error_at(
        &format!("{head}action = {{ affinity = {{}} }}\n"),
        8,
        "at least one of",
    );
    // mask 与 reserve_cores 同时给出
    error_at(
        &format!("{head}action = {{ affinity = {{ mask = \"0xffff\", reserve_cores = 1 }} }}\n"),
        8,
        "must not combine",
    );
    // 掩码为 0
    error_at(
        &format!("{head}action = {{ affinity = {{ mask = 0 }} }}\n"),
        8,
        "must not be zero",
    );
    // 掩码不是数字
    error_at(
        &format!("{head}action = {{ affinity = {{ mask = \"nope\" }} }}\n"),
        8,
        "not a valid mask",
    );
    // reserve_from 取值不在白名单
    error_at(
        &format!(
            "{head}action = {{ affinity = {{ reserve_cores = 1, reserve_from = \"middle\" }} }}\n"
        ),
        8,
        "expected \"first\" or \"last\"",
    );
    // reserve_from 不能与 mask 搭配
    error_at(
        &format!(
            "{head}action = {{ affinity = {{ mask = \"0xff\", reserve_from = \"last\" }} }}\n"
        ),
        8,
        "only applies together with `reserve_cores`",
    );
}

#[test]
fn condition_errors_carry_the_line_and_the_key() {
    let head = "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\n";
    // 区间矛盾（第 8 行的 ram_gb 键）
    error_at(
        &format!("{head}when = {{ ram_gb = {{ gte = 16, lt = 8 }} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"),
        8,
        "empty range",
    );
    // 同时给两个下界
    error_at(
        &format!("{head}when = {{ logical_cores = {{ gt = 4, gte = 8 }} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"),
        8,
        "must not combine `gt` and `gte`",
    );
    // eq 与 ne 同时出现
    error_at(
        &format!("{head}when = {{ physical_cores = {{ eq = 8, ne = 8 }} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"),
        8,
        "must not combine `eq` and `ne`",
    );
    // 类型不匹配：数值字段给了字符串
    error_at(
        &format!("{head}when = {{ ram_gb = {{ gte = \"many\" }} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"),
        8,
        "expects an integer comparison value",
    );
    // gpu_vendor 不支持大小比较
    error_at(
        &format!("{head}when = {{ gpu_vendor = {{ gte = 1 }} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"),
        8,
        "supports only `eq` and `ne`",
    );
    // gpu_vendor 取值不在白名单
    error_at(
        &format!("{head}when = {{ gpu_vendor = {{ eq = \"3dfx\" }} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"),
        8,
        "does not accept",
    );
    // is_elevated 只接受布尔
    error_at(
        &format!("{head}when = {{ is_elevated = {{ eq = 1 }} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"),
        8,
        "expects a boolean comparison value",
    );
    // 空比较表
    error_at(
        &format!(
            "{head}when = {{ ram_gb = {{}} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"
        ),
        8,
        "comparison for `ram_gb` is empty",
    );
}

#[test]
fn game_and_file_level_errors_carry_the_line() {
    // schema 版本
    error_at("schema = 2\n", 1, "unsupported policy schema version 2");
    // 没有任何 game
    error_at("schema = 1\n", 1, "no [[game]] section");
    // 缺少双语名之一
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nmatch = \"a.exe\"\n",
        1,
        "name_en",
    );
    // 空名字
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"  \"\nname_en = \"A\"\nmatch = \"a.exe\"\n",
        3,
        "`name_zh` must not be empty",
    );
    // id 不是 kebab-case
    error_at(
        "[[game]]\nid = \"Bad Id\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n",
        2,
        "kebab-case",
    );
    // match 是路径
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"C:\\\\games\\\\a.exe\"\n",
        5,
        "looks like a path",
    );
    // 裸 *
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"*\"\n",
        5,
        "every running process",
    );
    // 同文件内重复 id（诊断钉在第 8 行的 id 值上）
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game]]\nid = \"a\"\nname_zh = \"乙\"\nname_en = \"B\"\nmatch = \"b.exe\"\n",
        8,
        "duplicate game id",
    );
    // 规则 id 重复（诊断钉在第 11 行的第二条规则上）
    error_at(
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\nid = \"x\"\naction = { priority = { class = \"high\" } }\n\n[[game.rules]]\nid = \"x\"\naction = { priority = { class = \"below-normal\" } }\n",
        11,
        "duplicate rule id",
    );
}

#[test]
fn run_entries_errors_carry_the_line() {
    let head = "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\n";
    error_at(
        &format!(
            "{head}action = {{ run_entries = {{ hive = \"registry\", disable = [\"Steam\"] }} }}\n"
        ),
        8,
        "not supported; expected \"hkcu\"",
    );
    error_at(
        &format!("{head}action = {{ run_entries = {{ disable = [] }} }}\n"),
        8,
        "at least one startup entry name",
    );
    error_at(
        &format!("{head}action = {{ run_entries = {{ disable = [\"\"] }} }}\n"),
        8,
        "must not contain an empty name",
    );
    error_at(
        &format!("{head}action = {{ run_entries = {{ disable = [\"Steam\", \"Steam\"] }} }}\n"),
        8,
        "twice",
    );
}

#[test]
fn skip_action_requires_bilingual_reasons() {
    let head = "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\n";
    error_at(
        &format!("{head}action = {{ skip = {{ reason_zh = \"只有中文\" }} }}\n"),
        8,
        "reason_en",
    );
    error_at(
        &format!("{head}action = {{ skip = {{ reason_zh = \"中文\", reason_en = \"  \" }} }}\n"),
        8,
        "`skip.reason_en` must not be empty",
    );
}

#[test]
fn hostile_inputs_never_panic() {
    let inputs: [&str; 9] = [
        "",
        "not toml at all",
        "[[game]]",
        "[[game]]\n",
        "schema = 999999999999999999999999\n",
        "[[game]]\nid = 1\n",
        "[[game.rules]]\naction = {}\n",
        "\u{feff}schema = 1\n",
        "[[game]]\nid = \"a\"\nname_zh = \"甲\"\nname_en = \"A\"\nmatch = \"a.exe\"\n\n[[game.rules]]\naction = { priority = { class = \"high\" } }\n[[game.rules]]\naction = { priority = { class = \"below-normal\" } }\n",
    ];
    for text in inputs {
        // 只要不 panic、不返回 Ok 之外的东西就算通过；最后一个应当是合法的。
        let result = parse(text);
        match result {
            Ok(games) => {
                assert_eq!(games.len(), 1, "意外的成功:\n{text}");
                // 同种动作的第 2 条用 `-2` 后缀推导 id。
                assert_eq!(games[0].rules()[1].id(), "a.priority-2");
            }
            Err(diagnostic) => {
                assert!(diagnostic.is_error());
                assert!(!diagnostic.message().is_empty());
            }
        }
    }
}

#[test]
fn non_utf8_and_unreadable_files_become_diagnostics() {
    let scratch = Scratch::new("non-utf8");
    std::fs::write(
        scratch.path().join("binary.toml"),
        [0xff_u8, 0xfe, 0x00, 0x41],
    )
    .expect("write bytes");
    let outcome = PolicyLoader::builtin_only()
        .with_user_dir(scratch.path())
        .load();
    assert!(outcome.has_errors());
    let errors = outcome.errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].origin().display_path().ends_with("binary.toml"));
    assert!(!errors[0].message().is_empty());
}

#[test]
fn hal_policy_denial_and_validation_errors_have_distinct_messages() {
    // HAL 的红线错误消息被原样带进诊断（可解释性靠它）。
    let err = gopt_hal::PriorityClass::parse("realtime").expect_err("rejected");
    assert_eq!(err.kind(), HalErrorKind::PolicyDenied);
    assert!(err.message().contains("REALTIME") || err.message().contains("realtime"));
}
