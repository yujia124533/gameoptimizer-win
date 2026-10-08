//! 内置策略的完整性与 C++ v1.1.0 语义对齐验证。
//!
//! 这些断言把"内置策略文件"变成受测对象：磁盘目录 vs 编译进二进制的清单、
//! 8 款 C++ 兼容游戏的 exe / 优先级 / 亲和性口径、双语名与 id 唯一性。

mod common;

use gopt_hal::{HalOp, PriorityClass, SystemApi, WorkingSetLimits};
use gopt_policy::{EvalInput, PlanAction, PolicyLoader, BUILTIN_FILES, CPP_PARITY_FILES};

use common::{eval_input, policies_dir, Scratch};

#[test]
fn embedded_builtin_list_matches_the_repository_directory() {
    let mut on_disk: Vec<String> = std::fs::read_dir(policies_dir())
        .expect("read rust/policies")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("file name")
                .to_string()
        })
        .collect();
    on_disk.sort();

    let mut embedded: Vec<String> = BUILTIN_FILES
        .iter()
        .map(|file| file.name.to_string())
        .collect();
    embedded.sort();

    assert_eq!(
        on_disk, embedded,
        "rust/policies/*.toml 与 src/builtin.rs 的 include_str! 清单必须一一对应"
    );
    for name in CPP_PARITY_FILES {
        assert!(
            embedded.contains(&name.to_string()),
            "缺少 C++ 兼容文件 {name}"
        );
    }
    assert_eq!(CPP_PARITY_FILES.len(), 8);
}

#[test]
fn every_builtin_file_parses_without_diagnostics() {
    let outcome = PolicyLoader::builtin_only().load();
    assert!(
        !outcome.has_errors(),
        "内置策略必须零错误：{:#?}",
        outcome.diagnostics()
    );
    // 用户目录没参与，所以也不该有"覆盖"之类的警告。
    assert!(
        outcome.warnings().is_empty(),
        "内置层不应该产生警告：{:#?}",
        outcome.diagnostics()
    );

    let set = outcome.into_set();
    // 8 款 C++ 兼容游戏 + example-custom.toml 里的 2 款。
    assert_eq!(set.len(), 10, "ids = {:?}", set.game_ids());

    let mut ids = set.game_ids();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 10, "id 必须唯一");

    for game in set.games() {
        assert!(!game.name_zh().is_empty(), "{} 缺少中文名", game.id());
        assert!(!game.name_en().is_empty(), "{} 缺少英文名", game.id());
        assert!(game.origin().to_string().starts_with("<builtin>/"));
        assert!(game.line().is_some(), "{} 缺少来源行号", game.id());
    }
}

#[test]
fn cpp_v1_1_0_roster_and_target_executables_are_preserved() {
    let set = PolicyLoader::builtin_only().load().into_set();

    let expected: [(&str, &str, &str); 8] = [
        (
            "delta-force",
            "三角洲行动",
            "DeltaForceClient-Win64-Shipping.exe",
        ),
        ("league-of-legends", "英雄联盟", "League of Legends.exe"),
        ("cs2", "CS2", "cs2.exe"),
        ("pubg", "绝地求生", "TslGame.exe"),
        ("valorant", "无畏契约", "VALORANT-Win64-Shipping.exe"),
        ("apex-legends", "Apex 英雄", "r5apex.exe"),
        ("dota-2", "Dota 2", "dota2.exe"),
        ("overwatch-2", "守望先锋2", "Overwatch.exe"),
    ];
    for (id, name_zh, exe) in expected {
        let game = set
            .get(id)
            .unwrap_or_else(|| panic!("missing built-in {id}"));
        assert_eq!(game.name_zh(), name_zh);
        assert_eq!(game.exe_match(), exe);
        // exe 名必须能匹配到"正在运行的进程名"（大小写不敏感）。
        assert!(game.matches_exe(&exe.to_uppercase()), "{id} 无法匹配 {exe}");
    }

    // C++ 版 GameExeName() 逐条对应；额外的 exe_aliases 不影响主模式。
    assert!(set.get("cs2").expect("cs2").matches_exe("csgo.exe"));
    // 台服旧译名"瓦罗兰特"作为名称别名可被查到。
    assert_eq!(set.find("瓦罗兰特").map(|game| game.id()), Some("valorant"));
    assert_eq!(set.find("无畏契约").map(|game| game.id()), Some("valorant"));
}

#[test]
fn cpp_priority_and_affinity_semantics_are_preserved_as_data() {
    let set = PolicyLoader::builtin_only().load().into_set();
    // 8 核 16 线程 / 32GB：与 C++ 版在主流机器上的结果一致。
    let input = eval_input(32768, false);

    let expected: [(&str, PriorityClass, bool, u32); 8] = [
        ("delta-force", PriorityClass::High, false, 1),
        ("league-of-legends", PriorityClass::AboveNormal, false, 1),
        ("cs2", PriorityClass::High, true, 1),
        ("pubg", PriorityClass::High, true, 1),
        ("valorant", PriorityClass::High, false, 1),
        ("apex-legends", PriorityClass::AboveNormal, false, 1),
        ("dota-2", PriorityClass::AboveNormal, false, 1),
        ("overwatch-2", PriorityClass::High, false, 1),
    ];

    for (id, class, physical_only, reserve) in expected {
        let game = set
            .get(id)
            .unwrap_or_else(|| panic!("missing built-in {id}"));
        let plan = game.plan(&input, 4242);

        let priority = plan
            .steps
            .iter()
            .find_map(|step| match &step.action {
                PlanAction::Priority { class } => Some(*class),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{id} 缺少优先级步骤"));
        assert_eq!(priority, class, "{id} 优先级口径与 C++ 不一致");

        let (plan_object, spec) = plan
            .steps
            .iter()
            .find_map(|step| match &step.action {
                PlanAction::Affinity { plan, spec } => Some((plan.clone(), spec.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{id} 缺少亲和性步骤"));
        assert_eq!(
            spec.physical_only(),
            physical_only,
            "{id} bindPhysicalOnly 口径不一致"
        );
        assert_eq!(
            spec.reserve_cores(),
            reserve,
            "{id} leaveCoresForSystem 口径不一致"
        );
        assert_eq!(plan_object.total_logical(), 16);

        if physical_only {
            // 仅物理核 + 保留 1 个物理核 → 跳过逻辑核 0、1。
            assert_eq!(
                plan_object.requests()[0].mask(),
                0xfffc,
                "{id} 掩码与 C++ 不一致"
            );
            assert_eq!(plan_object.selected_logical(), 14);
        } else {
            // 全逻辑核 + 保留 1 个逻辑核 → 清掉最低位（C++ 的"清除最低 N 个置位"）。
            assert_eq!(
                plan_object.requests()[0].mask(),
                0xfffe,
                "{id} 掩码与 C++ 不一致"
            );
            assert_eq!(plan_object.selected_logical(), 15);
        }
    }
}

#[test]
fn cpp_working_set_ladder_is_preserved_and_degraded_by_condition() {
    let set = PolicyLoader::builtin_only().load().into_set();

    // (id, 内存 MiB) -> 期望的工作集下限（MiB）；None = 不设置
    let cases: [(&str, u64, Option<u64>); 12] = [
        ("cs2", 32768, Some(256)),
        ("cs2", 12288, Some(128)),
        ("cs2", 4096, None),
        ("pubg", 32768, Some(512)),
        ("pubg", 12288, Some(256)),
        ("pubg", 8192, Some(256)),
        ("valorant", 32768, Some(256)),
        ("valorant", 12288, Some(128)),
        ("overwatch-2", 32768, Some(256)),
        ("delta-force", 32768, None),
        ("league-of-legends", 32768, None),
        ("apex-legends", 32768, None),
    ];

    for (id, ram_mb, expected_mb) in cases {
        let game = set
            .get(id)
            .unwrap_or_else(|| panic!("missing built-in {id}"));
        let plan = game.plan(&eval_input(ram_mb, false), 99);
        let actual = plan.steps.iter().find_map(|step| match &step.action {
            PlanAction::WorkingSet { limits } => Some(limits.min_bytes / (1024 * 1024)),
            _ => None,
        });
        assert_eq!(
            actual, expected_mb,
            "{id} @ {ram_mb}MiB 的工作集口径与 C++ 不一致"
        );
        if expected_mb.is_none() {
            // 该游戏"本来就有工作集规则"时，低内存档位必须留下显式跳过说明：
            // C++ 是静默取消，Rust 版要求可解释（delta-force/LoL/Apex 本就没有这条规则，不在此列）。
            let has_rule = game
                .rules()
                .iter()
                .any(|rule| rule.id().contains("working-set"));
            if has_rule {
                assert!(
                    plan.skipped
                        .iter()
                        .any(|skip| skip.rule_id.contains("working-set")),
                    "{id} @ {ram_mb}MiB 应该有可解释的跳过说明: {:#?}",
                    plan.skipped
                );
            }
        }
    }
}

#[test]
fn example_custom_file_adds_games_as_pure_data() {
    let set = PolicyLoader::builtin_only().load().into_set();

    // 内置 8 款之外的两款：永劫无间、原神（多 exe）。
    let naraka = set.get("naraka-bladepoint").expect("naraka policy");
    assert_eq!(naraka.name_zh(), "永劫无间");
    assert!(naraka.matches_exe("NarakaBladepoint.exe"));
    let genshin = set.get("genshin-impact").expect("genshin policy");
    assert!(genshin.matches_exe("GenshinImpact.exe"));
    assert!(genshin.matches_exe("YuanShen.exe"), "国服 exe 别名必须生效");

    // 原神在国际服/国服两个 exe 上都给出同样的计划。
    let input = eval_input(32768, true);
    let plan_a = genshin.plan(&input, 1);
    let plan_b = genshin.plan(&input, 2);
    assert_eq!(plan_a.hal_ops(), plan_b.hal_ops());
    assert!(
        plan_a.hal_ops().contains(&HalOp::SetPowerScheme),
        "提权时应切换电源方案"
    );
    assert!(plan_a.requires_elevation());
    assert!(plan_a.has_dangerous_steps());

    // 未提权时电源方案规则由条件挡掉，但必须留下可解释的跳过。
    let plan_low = genshin.plan(&eval_input(32768, false), 1);
    assert!(!plan_low.requires_elevation());
    assert!(plan_low
        .skipped
        .iter()
        .any(|skip| skip.rule_id == "power-high-performance"));

    // 永劫无间会禁用用户态启动项（HKCU，不需要管理员）。
    let naraka_plan = naraka.plan(&eval_input(32768, false), 7);
    let run_entries: Vec<&gopt_policy::PlanAction> = naraka_plan
        .steps
        .iter()
        .map(|step| &step.action)
        .filter(|action| matches!(action, PlanAction::RunEntry { .. }))
        .collect();
    assert_eq!(run_entries.len(), 1);
    match run_entries[0] {
        PlanAction::RunEntry {
            hive,
            name,
            enabled,
            ignore_missing,
        } => {
            assert_eq!(*hive, gopt_hal::RunHive::CurrentUser);
            assert_eq!(name, "Discord");
            assert!(!*enabled);
            assert!(*ignore_missing);
        }
        other => panic!("expected run entry, got {other:?}"),
    }
}

#[test]
fn builtin_plans_never_exceed_high_priority() {
    let set = PolicyLoader::builtin_only().load().into_set();
    let input = eval_input(32768, true);
    for game in set.games() {
        let plan = game.plan(&input, 1234);
        for step in &plan.steps {
            if let PlanAction::Priority { class } = step.action {
                assert!(
                    class <= PriorityClass::High,
                    "{} 请求了超过 HIGH 的优先级：{class:?}",
                    game.id()
                );
            }
        }
    }
}

#[test]
fn working_set_steps_stay_reclaimable() {
    // 工作集动作只允许"下限 +（可选）上限"，且上限缺省 = 系统可回收的无上限哨兵值。
    let limits = WorkingSetLimits::from_mb(256, 0).expect("limits");
    assert_eq!(
        limits.normalized().max_bytes,
        WorkingSetLimits::NO_UPPER_BOUND
    );
}

#[test]
fn user_directory_that_does_not_exist_is_not_an_error() {
    let scratch = Scratch::new("missing-dir");
    let missing = scratch.path().join("nope");
    let outcome = PolicyLoader::builtin_only().with_user_dir(missing).load();
    assert!(!outcome.has_errors(), "{:#?}", outcome.diagnostics());
    assert_eq!(outcome.set().len(), 10);
}

/// 用 Mock 后端跑一遍真实加载路径：策略 → 计划，不需要管理员、不需要游戏在跑。
#[test]
fn builtin_policies_produce_plans_from_a_mock_backend() {
    let api = gopt_hal::MockApi::sample_workstation();
    let input = EvalInput::from_api(&api).expect("probe");
    let set = PolicyLoader::builtin_only().load().into_set();
    let processes = api.list_processes().expect("processes");
    let plans = set.plans_for(&input, &processes);

    // MockApi::sample_workstation 里只有 cs2.exe 命中内置策略。
    assert_eq!(plans.len(), 1);
    let cs2 = &plans[0];
    assert_eq!(cs2.game_id, "cs2");
    assert_eq!(cs2.pid, 1234);
    assert_eq!(
        cs2.hal_ops(),
        vec![HalOp::SetPriority, HalOp::SetAffinity, HalOp::SetWorkingSet]
    );

    // 该计划可以直接喂给 HAL（gopt-core 的执行器就做这件事）。
    let class = match &cs2.steps[0].action {
        PlanAction::Priority { class } => *class,
        other => panic!("expected priority step, got {other:?}"),
    };
    let previous = api.set_priority(cs2.pid, class).expect("apply priority");
    assert_eq!(previous, PriorityClass::Normal);
    assert_eq!(api.priority_of(1234), Some(PriorityClass::High));
}
