//! Plan 的结构、可解释性与"能不能真的执行"。
//!
//! 这里用一个**执行器**把 Plan 逐步喂给 `MockApi`：如果计划里的动作无法映射到 HAL，
//! 或者顺序/回滚信息不对，测试就会挂——这正是 gopt-core 将来要做的事，
//! 提前在策略层锁死契约。

mod common;

use gopt_hal::{
    AffinityPlan, Guid, HalErrorKind, HalOp, HalResult, MockApi, PowerSchemeSelector,
    PriorityClass, RunHive, SystemApi,
};
use gopt_policy::{EvalInput, Plan, PlanAction, PolicyLoader, SkipCause};

use common::{eval_input, hardware};

/// 把一份计划按顺序应用到 HAL；返回每一步的中英双语说明（模拟 gopt-core 的执行器）。
fn execute(api: &dyn SystemApi, plan: &Plan) -> HalResult<Vec<String>> {
    let mut executed = Vec::new();
    for step in &plan.steps {
        let note = match &step.action {
            PlanAction::Priority { class } => {
                let previous = api.set_priority(step.pid, *class)?;
                format!("{} {previous:?} -> {class:?}", step.rule_id)
            }
            PlanAction::Affinity { .. } => {
                // 跨处理器组时必须逐组应用（HAL 对跨组计划返回 Unsupported）。
                let mut applied = 0;
                for batch in step.affinity_batches() {
                    api.set_affinity(step.pid, &batch)?;
                    applied += 1;
                }
                format!("{} {applied} batch(es)", step.rule_id)
            }
            PlanAction::WorkingSet { limits } => {
                api.set_working_set(step.pid, *limits)?;
                format!("{} {} MiB", step.rule_id, limits.min_bytes / (1024 * 1024))
            }
            PlanAction::PowerScheme { selector, .. } => {
                let change = api.set_power_scheme(selector)?;
                format!(
                    "{} {} -> {}",
                    step.rule_id, change.previous.guid, change.current.guid
                )
            }
            PlanAction::RunEntry {
                hive,
                name,
                enabled,
                ignore_missing,
            } => match api.set_run_entry_enabled(*hive, name, *enabled) {
                Ok(entry) => format!("{} {} enabled={}", step.rule_id, entry.id(), entry.enabled),
                // `ignore_missing = true` 时"条目不存在"是软跳过而不是失败。
                Err(err) if *ignore_missing && err.kind() == HalErrorKind::NotFound => {
                    format!("{} {} missing (skipped)", step.rule_id, name)
                }
                Err(err) => return Err(err),
            },
        };
        executed.push(note);
    }
    Ok(executed)
}

#[test]
fn cs2_plan_is_ordered_explained_and_executable() {
    let api = MockApi::sample_workstation();
    let input = EvalInput::from_api(&api).expect("probe");
    let set = PolicyLoader::builtin_only().load().into_set();
    let plan = set.get("cs2").expect("cs2").plan(&input, 1234);

    // 顺序 = TOML 声明顺序；order 从 1 连续递增。
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.rule_id.as_str())
            .collect::<Vec<_>>(),
        vec!["priority", "affinity", "working-set"]
    );
    for (index, step) in plan.steps.iter().enumerate() {
        assert_eq!(step.order as usize, index + 1);
        assert_eq!(step.pid, 1234);
        assert!(step.rule_line.is_some());
        assert!(!step.reason.zh.is_empty());
        assert!(!step.reason.en.is_empty());
        assert!(!step.is_dangerous, "进程级动作都不是危险动作");
        assert!(!step.requires_elevation, "进程级动作不需要管理员");
    }
    assert_eq!(
        plan.hal_ops(),
        vec![HalOp::SetPriority, HalOp::SetAffinity, HalOp::SetWorkingSet]
    );

    // 亲和性掩码：16 逻辑核、仅物理核、保留 1 个物理核 → 跳过逻辑核 0、1。
    match &plan.steps[1].action {
        PlanAction::Affinity { plan, spec } => {
            assert_eq!(plan.requests()[0].mask(), 0xfffc);
            assert_eq!(plan.selected_logical(), 14);
            assert!(spec.physical_only());
            assert_eq!(spec.reserve_cores(), 1);
        }
        other => panic!("expected affinity, got {other:?}"),
    }
    assert!(
        plan.steps[1].reason.zh.contains("仅物理核"),
        "{}",
        plan.steps[1].reason.zh
    );
    assert!(!plan.steps[1].reason.en.is_empty());

    // 执行 + 回滚信息。
    let notes = execute(&api, &plan).expect("plan executes against the mock backend");
    assert_eq!(notes.len(), 3);
    assert_eq!(api.priority_of(1234), Some(PriorityClass::High));
    assert_eq!(
        api.affinity_of(1234).map(|plan| plan.requests()[0].mask()),
        Some(0xfffc)
    );
    assert_eq!(
        api.working_set_of(1234).map(|limits| limits.min_bytes),
        Some(256 * 1024 * 1024)
    );
    // 顺序与调用序列一致。
    assert_eq!(
        api.calls().iter().map(|call| call.op).collect::<Vec<_>>(),
        vec![
            HalOp::Hardware,
            HalOp::IsElevated,
            HalOp::SetPriority,
            HalOp::SetAffinity,
            HalOp::SetWorkingSet
        ]
    );
}

#[test]
fn cross_group_plans_are_executed_one_group_at_a_time() {
    let api = MockApi::with_topology(48, 96);
    api.push_process(999, "cs2.exe", 12);
    let input = EvalInput::from_api(&api).expect("probe");
    let set = PolicyLoader::builtin_only().load().into_set();
    let plan = set.get("cs2").expect("cs2").plan(&input, 999);

    let notes = execute(&api, &plan).expect("cross-group plan executes per group");
    assert!(
        notes
            .iter()
            .any(|note| note.contains("affinity 2 batch(es)")),
        "{notes:?}"
    );
    assert_eq!(api.call_count(HalOp::SetAffinity), 2);
    // 最后一次应用的是组 1（该组 32 个逻辑核全部保留）。
    assert_eq!(
        api.affinity_of(999).map(|plan| plan.selected_logical()),
        Some(32)
    );
}

#[test]
fn elevated_actions_are_flagged_and_reversible() {
    let api = MockApi::sample_workstation();
    api.push_process(4321, "GenshinImpact.exe", 8);
    let input = EvalInput::from_api(&api).expect("probe");
    assert!(!input.is_elevated());

    let set = PolicyLoader::builtin_only().load().into_set();
    let genshin = set.get("genshin-impact").expect("genshin");

    // 未提权：电源方案规则被条件挡掉，但留下解释；其余动作照做。
    let plan = genshin.plan(&input, 4321);
    assert!(!plan.requires_elevation());
    assert!(plan
        .skipped
        .iter()
        .any(|skip| skip.rule_id == "power-high-performance"));
    let notes = execute(&api, &plan).expect("execute without admin");
    assert!(
        notes
            .iter()
            .any(|note| note.contains("working-set-with-gpu")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .all(|note| !note.contains("power-high-performance")),
        "{notes:?}"
    );
    assert_eq!(
        api.query_power_scheme().expect("scheme").guid,
        Guid::BALANCED
    );
    assert_eq!(api.priority_of(4321), Some(PriorityClass::AboveNormal));

    // 提权后：多出电源方案步骤，且带"危险动作 + 需要管理员"标记。
    api.set_elevated(true);
    let input = EvalInput::from_api(&api).expect("probe");
    let plan = genshin.plan(&input, 4321);
    let power = plan
        .steps
        .iter()
        .find(|step| matches!(&step.action, PlanAction::PowerScheme { .. }))
        .expect("power step");
    assert!(power.requires_elevation);
    assert!(power.is_dangerous);

    let notes = execute(&api, &plan).expect("execute as admin");
    // 回滚依据：切换前后的方案都从 HAL 拿到。
    assert!(
        notes.iter().any(|note| note.contains("-> 8c5e7fda")),
        "{notes:?}"
    );
    assert_eq!(
        api.query_power_scheme().expect("scheme").guid,
        Guid::HIGH_PERFORMANCE
    );

    // 启动项禁用是改名迁移，HAL 返回的条目可以直接用于还原（永劫无间策略）。
    let naraka_plan = set
        .get("naraka-bladepoint")
        .expect("naraka")
        .plan(&input, 4321);
    let _ = execute(&api, &naraka_plan).expect("disable startup entry");
    let discord = api
        .run_entry(RunHive::CurrentUser, "Discord")
        .expect("entry");
    assert!(!discord.enabled);
    assert_eq!(discord.value_name, "[disabled] Discord");
}

#[test]
fn plans_are_serialisable_for_audit_and_json_frontends() {
    fn assert_serialize<T: serde::Serialize>() {}
    assert_serialize::<Plan>();
    assert_serialize::<PlanAction>();
    assert_serialize::<gopt_policy::PlanStep>();
    assert_serialize::<gopt_policy::PlanSkip>();
    assert_serialize::<gopt_policy::Reason>();
    assert_serialize::<gopt_policy::SkipCause>();
    assert_serialize::<gopt_policy::PolicyOrigin>();
    assert_serialize::<gopt_policy::PolicyDiagnostic>();

    fn assert_deserialize<'de, T: serde::Deserialize<'de>>() {}
    assert_deserialize::<Plan>();
    assert_deserialize::<PlanAction>();
}

#[test]
fn every_builtin_plan_holds_the_invariants_on_every_hardware_tier() {
    let set = PolicyLoader::builtin_only().load().into_set();
    let profiles = [
        hardware(8, 2, 32768, Some(gopt_hal::GpuVendor::Nvidia)),
        hardware(4, 1, 8192, Some(gopt_hal::GpuVendor::Amd)),
        hardware(2, 2, 4096, None),
        hardware(48, 2, 65536, Some(gopt_hal::GpuVendor::Intel)),
    ];

    for profile in profiles {
        for elevated in [false, true] {
            let input = EvalInput::new(profile.clone(), elevated);
            for game in set.games() {
                let plan = game.plan(&input, 777);
                assert_eq!(plan.game_id, game.id());
                assert_eq!(plan.pid, 777);
                assert_eq!(plan.policy_origin, game.origin().clone());
                for (index, step) in plan.steps.iter().enumerate() {
                    assert_eq!(
                        step.order as usize,
                        index + 1,
                        "{} 的步骤编号不连续",
                        game.id()
                    );
                    assert_eq!(step.pid, 777);
                    assert!(!step.rule_id.is_empty());
                    assert!(
                        !step.reason.zh.is_empty(),
                        "{} 的步骤缺少中文理由",
                        game.id()
                    );
                    assert!(
                        !step.reason.en.is_empty(),
                        "{} 的步骤缺少英文理由",
                        game.id()
                    );
                    // 每个步骤都必须能翻译成 HAL 操作。
                    assert!(matches!(
                        step.hal_op(),
                        HalOp::SetPriority
                            | HalOp::SetAffinity
                            | HalOp::SetWorkingSet
                            | HalOp::SetPowerScheme
                            | HalOp::SetRunEntryEnabled
                    ));
                    // 危险动作与提权标记只可能出现在电源方案 / 启动项上。
                    if step.is_dangerous {
                        assert!(matches!(
                            step.action,
                            PlanAction::PowerScheme { .. } | PlanAction::RunEntry { .. }
                        ));
                    }
                }
                for skip in &plan.skipped {
                    assert!(!skip.rule_id.is_empty());
                    assert!(!skip.reason.zh.is_empty());
                    assert!(!skip.reason.en.is_empty());
                    assert!(matches!(
                        skip.cause,
                        SkipCause::ConditionNotMet | SkipCause::Degraded | SkipCause::ExplicitSkip
                    ));
                }
            }
        }
    }
}

#[test]
fn cancellation_and_rollback_information_is_available_for_every_write() {
    // 策略层本身不写系统，但它产出的每一步都落在 HAL 的"返回旧值"契约上：
    // 这里验证三类写入的回滚依据都能拿到。
    let api = MockApi::sample_workstation();
    api.push_process(2468, "cs2.exe", 16);

    // 优先级：返回旧值。
    let previous = api
        .set_priority(2468, PriorityClass::High)
        .expect("priority");
    assert_eq!(previous, PriorityClass::Normal);
    assert_eq!(
        api.set_priority(2468, previous).expect("restore"),
        PriorityClass::High
    );

    // 亲和性：应用前先读一次原计划（策略执行器的回滚依据）。
    let original = api.get_affinity(2468).expect("affinity");
    assert_eq!(original.process_mask, 0xffff);
    let plan = AffinityPlan::single(16, 0, 0xfffc).expect("plan");
    api.set_affinity(2468, &plan).expect("apply");
    let restore = AffinityPlan::single(
        original.total_logical,
        original.group,
        original.process_mask,
    )
    .expect("restore");
    assert_eq!(restore.selected_logical(), 16);

    // 电源方案：返回切换前后的方案。
    let change = api
        .set_power_scheme(&PowerSchemeSelector::Explicit(Guid::HIGH_PERFORMANCE))
        .expect("power scheme");
    assert_eq!(change.previous.guid, Guid::BALANCED);
    assert_eq!(change.current.guid, Guid::HIGH_PERFORMANCE);
}

#[test]
fn empty_rules_produce_an_empty_plan_without_panicking() {
    let api = MockApi::sample_workstation();
    let input = EvalInput::from_api(&api).expect("probe");
    let games = gopt_policy::parse_policy_file(
        "[[game]]\nid = \"empty\"\nname_zh = \"空策略\"\nname_en = \"Empty\"\nmatch = \"empty.exe\"\n",
        gopt_policy::PolicyOrigin::builtin("empty.toml"),
    )
    .expect("a game without rules is legal (it only registers the game)");
    assert!(games[0].rules().is_empty());

    let plan = games[0].plan(&input, 1);
    assert!(plan.is_empty());
    assert_eq!(plan.skipped_count(), 0);
    assert_eq!(
        execute(&api, &plan).expect("no-op execution"),
        Vec::<String>::new()
    );
    assert_eq!(api.call_count(HalOp::SetPriority), 0);
}

#[test]
fn plans_never_request_realtime_priority() {
    let set = PolicyLoader::builtin_only().load().into_set();
    let input = eval_input(32768, true);
    for game in set.games() {
        for step in game.plan(&input, 1).steps {
            if let PlanAction::Priority { class } = step.action {
                assert!(
                    class <= PriorityClass::High,
                    "{} 请求了超过 HIGH 的优先级",
                    game.id()
                );
                assert_ne!(class.as_str(), "realtime");
            }
        }
    }
}
