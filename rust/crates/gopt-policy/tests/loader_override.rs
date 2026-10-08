//! 加载与覆盖：用户层优先、同 id 覆盖内置、新 id 新增、错误不影响其它文件。

mod common;

use gopt_policy::{DiagnosticSeverity, PolicyLayer, PolicyLoader};

use common::{eval_input, Scratch};

const OVERRIDE_CS2: &str = r#"
schema = 1

# 与内置同 id：覆盖内置策略，而不是新增一款游戏。
[[game]]
id = "cs2"
name_zh = "CS2（保守档）"
name_en = "Counter-Strike 2 (conservative)"
match = "cs2.exe"
description_zh = "保守档：AboveNormal"
description_en = "conservative"

[[game.rules]]
id = "priority"
action = { priority = { class = "above-normal" } }
"#;

const NEW_GAME: &str = r#"
schema = 1

# 全新游戏：不需要改代码，只需要放一个 TOML。
[[game]]
id = "my-indie-game"
name_zh = "我的独立游戏"
name_en = "My Indie Game"
match = "indie-*.exe"
name_aliases = ["indie"]
description_zh = "示例"
description_en = "example"

[[game.rules]]
id = "priority"
action = { priority = { class = "high" } }
"#;

#[test]
fn user_policy_overrides_the_builtin_with_the_same_id() {
    let scratch = Scratch::new("override");
    scratch.write("override.toml", OVERRIDE_CS2);

    let outcome = PolicyLoader::builtin_only()
        .with_user_dir(scratch.path())
        .load();
    assert!(!outcome.has_errors(), "{:#?}", outcome.diagnostics());

    let set = outcome.set();
    // 总款数不变（覆盖，不是并存）。
    assert_eq!(set.len(), 10);
    let cs2 = set.get("cs2").expect("cs2");
    assert_eq!(cs2.name_zh(), "CS2（保守档）");
    assert_eq!(cs2.origin().layer(), PolicyLayer::User);
    assert!(cs2.origin().is_user());
    assert!(cs2.origin().display_path().ends_with("override.toml"));
    assert_eq!(cs2.rules().len(), 1);

    // 规则真的变了：只有 AboveNormal，没有亲和性。
    let plan = cs2.plan(&eval_input(32768, false), 1234);
    assert_eq!(plan.step_count(), 1);
    match &plan.steps[0].action {
        gopt_policy::PlanAction::Priority { class } => assert_eq!(class.as_str(), "above-normal"),
        other => panic!("expected priority, got {other:?}"),
    }
    assert_eq!(&plan.policy_origin, cs2.origin());

    // 覆盖行为必须留下可见的警告（用户要知道自己盖掉了内置策略）。
    let warnings = outcome.warnings();
    assert_eq!(warnings.len(), 1, "{:#?}", outcome.diagnostics());
    assert_eq!(warnings[0].severity(), DiagnosticSeverity::Warning);
    assert!(
        warnings[0]
            .message()
            .contains("overrides the built-in policy"),
        "{}",
        warnings[0].message()
    );

    // 其它内置策略不受影响。
    let dota = set.get("dota-2").expect("dota-2");
    assert_eq!(dota.origin().layer(), PolicyLayer::Builtin);
}

#[test]
fn user_policy_can_add_a_brand_new_game_without_recompiling() {
    let scratch = Scratch::new("new-game");
    scratch.write("indie.toml", NEW_GAME);

    let outcome = PolicyLoader::builtin_only()
        .with_user_dir(scratch.path())
        .load();
    assert!(!outcome.has_errors(), "{:#?}", outcome.diagnostics());
    let set = outcome.set();

    assert_eq!(set.len(), 11);
    let game = set.get("my-indie-game").expect("new game");
    assert_eq!(game.origin().layer(), PolicyLayer::User);
    // 通配模式生效。
    assert!(game.matches_exe("indie-platformer.exe"));
    assert!(!game.matches_exe("indie-platformer.dll"));
    // exe 匹配（而不是 id 查找）也能命中。
    assert_eq!(
        set.match_process_name("INDIE-GAME.EXE")
            .map(|game| game.id()),
        Some("my-indie-game")
    );
    // 用户层在列表里排在内置层之前（查找/匹配都是用户优先）。
    assert_eq!(set.games()[0].id(), "my-indie-game");
    assert_eq!(set.game_ids().last(), Some(&"genshin-impact"));

    let plan = game.plan(&eval_input(16384, false), 555);
    assert_eq!(plan.hal_ops(), vec![gopt_hal::HalOp::SetPriority]);
}

#[test]
fn later_user_files_win_inside_the_user_layer_and_warn() {
    let scratch = Scratch::new("user-dup");
    scratch.write(
        "10-first.toml",
        r#"
[[game]]
id = "same-id"
name_zh = "先写的"
name_en = "first"
match = "first.exe"

[[game.rules]]
action = { priority = { class = "idle" } }
"#,
    );
    scratch.write(
        "20-second.toml",
        r#"
[[game]]
id = "same-id"
name_zh = "后写的"
name_en = "second"
match = "second.exe"

[[game.rules]]
action = { priority = { class = "high" } }
"#,
    );

    let outcome = PolicyLoader::user_only()
        .with_user_dir(scratch.path())
        .load();
    assert!(!outcome.has_errors(), "{:#?}", outcome.diagnostics());
    let set = outcome.set();
    assert_eq!(set.len(), 1);
    let game = set.get("same-id").expect("same-id");
    assert_eq!(game.name_zh(), "后写的");
    assert!(game.origin().display_path().ends_with("20-second.toml"));
    assert!(outcome.warnings().iter().any(|warning| warning
        .message()
        .contains("defined twice in the user layer")));
}

#[test]
fn a_broken_user_file_is_reported_and_the_rest_still_loads() {
    let scratch = Scratch::new("broken-user");
    scratch.write("broken.toml", "[[game]]\nid = \n");
    scratch.write("good.toml", NEW_GAME);

    let outcome = PolicyLoader::builtin_only()
        .with_user_dir(scratch.path())
        .load();
    assert!(outcome.has_errors());
    let errors = outcome.errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].origin().display_path().ends_with("broken.toml"));
    assert!(errors[0].line().is_some(), "语法错误必须带行号");

    // 坏文件被跳过，好文件与内置层照常可用。
    let set = outcome.set();
    assert!(set.get("my-indie-game").is_some());
    assert!(set.get("cs2").is_some());

    // 严格模式（给 CLI 用）会把错误冒泡出来。
    let errors = outcome.into_result().expect_err("strict load must fail");
    assert_eq!(errors.len(), 1);
    assert!(errors.to_string().contains("broken.toml"));
}

#[test]
fn default_user_directory_follows_localappdata() {
    let expected = std::env::var_os("LOCALAPPDATA").map(|base| {
        let mut path = std::path::PathBuf::from(base);
        path.push("GameOptimizer");
        path.push("policies.d");
        path
    });
    assert_eq!(PolicyLoader::default_user_dir(), expected);

    // `new()` = 内置 + 默认用户目录（目录是否存在不影响加载）。
    let loader = PolicyLoader::new();
    assert!(loader.loads_builtin());
    assert!(loader.user_dirs().len() <= 1);
}

#[test]
fn user_layer_adds_or_overrides_any_builtin_field() {
    let scratch = Scratch::new("override-alias");
    // 覆盖内置 cs2 的 exe 别名 + 增加条件规则，验证"用户层完整替换"语义。
    scratch.write(
        "alias.toml",
        r#"
[[game]]
id = "cs2"
name_zh = "CS2 自定义"
name_en = "CS2 custom"
match = "cs2.exe"
exe_aliases = ["cs2-beta.exe"]
name_aliases = ["自定义CS2"]

[[game.rules]]
id = "affinity"
when = { logical_cores = { gte = 8 } }
action = { affinity = { reserve_cores = 1 } }
"#,
    );

    let outcome = PolicyLoader::builtin_only()
        .with_user_dir(scratch.path())
        .load();
    assert!(!outcome.has_errors(), "{:#?}", outcome.diagnostics());
    let set = outcome.set();
    let cs2 = set.get("cs2").expect("cs2");
    assert!(cs2.matches_exe("cs2-beta.exe"));
    assert_eq!(set.find("自定义CS2").map(|game| game.id()), Some("cs2"));
    let plan = cs2.plan(&eval_input(32768, false), 1);
    assert_eq!(plan.step_count(), 1);
    assert_eq!(plan.steps[0].rule_id, "affinity");
    assert_eq!(plan.steps[0].hal_op(), gopt_hal::HalOp::SetAffinity);
}

#[test]
fn duplicate_exe_patterns_across_games_are_warned_about() {
    let scratch = Scratch::new("pattern-clash");
    scratch.write(
        "clash.toml",
        r#"
[[game]]
id = "my-cs2"
name_zh = "我的 CS2"
name_en = "my cs2"
match = "cs2.exe"

[[game.rules]]
action = { priority = { class = "normal" } }
"#,
    );

    let outcome = PolicyLoader::builtin_only()
        .with_user_dir(scratch.path())
        .load();
    assert!(!outcome.has_errors(), "{:#?}", outcome.diagnostics());
    // 内置 cs2 与用户 my-cs2 抢同一个模式：用户层优先，并且必须警告。
    assert!(
        outcome
            .warnings()
            .iter()
            .any(|warning| warning.message().contains("claimed by both")),
        "{:#?}",
        outcome.diagnostics()
    );
    assert_eq!(
        outcome
            .set()
            .match_process_name("cs2.exe")
            .map(|game| game.id()),
        Some("my-cs2")
    );
}
