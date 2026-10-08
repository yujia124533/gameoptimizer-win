//! 条件求值矩阵：通过真实的 TOML 解析路径验证 `when` 的算子、类型与边界。
//!
//! 每个用例都是"一份策略文件 + 一份硬件画像 → 计划里有没有那一步"，
//! 因此同时覆盖了 解析 → 校验 → 求值 三层的接口一致性。

mod common;

use gopt_hal::{GpuVendor, HalOp, PriorityClass};
use gopt_policy::{parse_policy_file, EvalInput, PlanAction, PolicyOrigin};

use common::hardware;

/// 生成一份"条件 + 高优先级动作"的文件内容。
fn file_with_when(when: &str) -> String {
    format!(
        "[[game]]\nid = \"probe\"\nname_zh = \"探针\"\nname_en = \"Probe\"\nmatch = \"probe.exe\"\n\n[[game.rules]]\nid = \"r\"\nwhen = {{ {when} }}\naction = {{ priority = {{ class = \"high\" }} }}\n"
    )
}

fn plan_for(when: &str, input: &EvalInput) -> bool {
    let games = parse_policy_file(&file_with_when(when), PolicyOrigin::builtin("probe.toml"))
        .expect("valid policy");
    let plan = games[0].plan(input, 42);
    assert!(plan.is_empty() || plan.step_count() == 1);
    plan.step_count() == 1
}

#[test]
fn numeric_operator_matrix() {
    // 8 核 16 线程 / 32GB
    let input = EvalInput::new(hardware(8, 2, 32768, Some(GpuVendor::Nvidia)), false);

    let cases: [(&str, bool); 20] = [
        ("logical_cores = 16", true),
        ("logical_cores = { eq = 16 }", true),
        ("logical_cores = { ne = 16 }", false),
        ("logical_cores = { gt = 15 }", true),
        ("logical_cores = { gt = 16 }", false),
        ("logical_cores = { gte = 16 }", true),
        ("logical_cores = { lt = 17 }", true),
        ("logical_cores = { lt = 16 }", false),
        ("logical_cores = { lte = 16 }", true),
        ("physical_cores = 8", true),
        ("physical_cores = { gte = 9 }", false),
        ("ram_gb = 32", true),
        ("ram_gb = { gte = 16 }", true),
        ("ram_gb = { gte = 8, lt = 16 }", false),
        ("ram_gb = { gt = 8, lte = 32 }", true),
        ("ram_mb = 32768", true),
        ("ram_mb = { gte = 32768 }", true),
        ("ram_gb = { lt = 64 }", true),
        ("ram_gb = { gte = 33 }", false),
        ("logical_cores = { gte = 8, lte = 16 }", true),
    ];
    for (when, expected) in cases {
        assert_eq!(plan_for(when, &input), expected, "when = {{ {when} }}");
    }
}

#[test]
fn ram_gb_boundaries_follow_the_cpp_thresholds() {
    for (ram_mb, gte16, gte8_lt16, lt8) in [
        (32768_u64, true, false, false),
        (16384, true, false, false),
        (16383, false, true, false),
        (8192, false, true, false),
        (8191, false, false, true),
    ] {
        let input = EvalInput::new(hardware(8, 2, ram_mb, None), false);
        assert_eq!(
            plan_for("ram_gb = { gte = 16 }", &input),
            gte16,
            "ram {ram_mb}MiB gte16"
        );
        assert_eq!(
            plan_for("ram_gb = { gte = 8, lt = 16 }", &input),
            gte8_lt16,
            "ram {ram_mb}MiB 8..16"
        );
        assert_eq!(
            plan_for("ram_gb = { lt = 8 }", &input),
            lt8,
            "ram {ram_mb}MiB lt8"
        );
    }
}

#[test]
fn gpu_vendor_and_elevation_matrix() {
    for (vendor, expected_eq, expected_ne) in [
        (Some(GpuVendor::Nvidia), true, false),
        (Some(GpuVendor::Amd), false, true),
        (Some(GpuVendor::Intel), false, true),
        (Some(GpuVendor::Unknown), false, true),
        (None, false, true),
    ] {
        let input = EvalInput::new(hardware(8, 2, 16384, vendor), false);
        let label = format!("{vendor:?}");
        assert_eq!(
            plan_for("gpu_vendor = \"nvidia\"", &input),
            expected_eq,
            "{label} == nvidia"
        );
        assert_eq!(
            plan_for("gpu_vendor = { ne = \"nvidia\" }", &input),
            expected_ne,
            "{label} != nvidia"
        );
        // `none` 只匹配"没有适配器"，`unknown` 是有适配器但认不出厂商。
        assert_eq!(
            plan_for("gpu_vendor = { eq = \"none\" }", &input),
            vendor.is_none(),
            "{label} == none"
        );
        assert_eq!(
            plan_for("gpu_vendor = { ne = \"none\" }", &input),
            vendor.is_some(),
            "{label} != none"
        );
    }

    for elevated in [false, true] {
        let input = EvalInput::new(hardware(8, 2, 16384, Some(GpuVendor::Nvidia)), elevated);
        assert_eq!(plan_for("is_elevated = true", &input), elevated);
        assert_eq!(plan_for("is_elevated = { eq = false }", &input), !elevated);
        assert_eq!(plan_for("is_elevated = { ne = true }", &input), !elevated);
    }
}

#[test]
fn multiple_terms_are_combined_with_and() {
    let high_end = EvalInput::new(hardware(8, 2, 32768, Some(GpuVendor::Nvidia)), true);
    let low_end = EvalInput::new(hardware(4, 2, 8192, Some(GpuVendor::Intel)), false);

    let when = "logical_cores = { gte = 8 }, ram_gb = { gte = 16 }, gpu_vendor = \"nvidia\"";
    assert!(plan_for(when, &high_end));
    assert!(!plan_for(when, &low_end));

    // 只要有一项不满足，整条规则就不命中（AND 语义）。
    assert!(!plan_for(
        "physical_cores = { gte = 4 }, is_elevated = true",
        &low_end
    ));
}

#[test]
fn empty_when_means_unconditional_and_skips_are_explained() {
    let input = EvalInput::new(hardware(8, 2, 16384, Some(GpuVendor::Nvidia)), false);

    // when = {} 等价于无条件。
    let unconditional = parse_policy_file(
        "[[game]]\nid = \"probe\"\nname_zh = \"探针\"\nname_en = \"Probe\"\nmatch = \"probe.exe\"\n\n[[game.rules]]\nwhen = {}\naction = { priority = { class = \"high\" } }\n",
        PolicyOrigin::builtin("probe.toml"),
    )
    .expect("valid");
    assert!(unconditional[0].rules()[0].when().is_none());

    // 条件不满足时：没有步骤，但有一条中英双语的解释。
    let games = parse_policy_file(
        &file_with_when("physical_cores = { gte = 16 }"),
        PolicyOrigin::builtin("probe.toml"),
    )
    .expect("valid");
    let plan = games[0].plan(&input, 7);
    assert!(plan.is_empty());
    assert_eq!(plan.skipped_count(), 1);
    let skip = &plan.skipped[0];
    assert_eq!(skip.rule_id, "r");
    assert_eq!(skip.cause, gopt_policy::SkipCause::ConditionNotMet);
    assert!(
        skip.reason.zh.contains("条件不满足：物理核 ≥ 16"),
        "{}",
        skip.reason.zh
    );
    assert!(
        skip.reason
            .en
            .contains("condition not met: physical cores ≥ 16"),
        "{}",
        skip.reason.en
    );
    // 来源行号可用于"跳到 TOML 那一行去改"。
    assert_eq!(skip.rule_line, Some(7));
}

#[test]
fn conditions_drive_the_whole_builtin_ladder() {
    // 同一款内置游戏在三档内存下给出不同的工作集策略（条件即降级路径）。
    let set = gopt_policy::PolicyLoader::builtin_only().load().into_set();
    let cs2 = set.get("cs2").expect("cs2");

    let high = cs2.plan(&EvalInput::new(hardware(8, 2, 32768, None), false), 1);
    let mid = cs2.plan(&EvalInput::new(hardware(8, 2, 12288, None), false), 1);
    let low = cs2.plan(&EvalInput::new(hardware(8, 2, 4096, None), false), 1);

    let min_of = |plan: &gopt_policy::Plan| -> Option<u64> {
        plan.steps.iter().find_map(|step| match &step.action {
            PlanAction::WorkingSet { limits } => Some(limits.min_bytes / (1024 * 1024)),
            _ => None,
        })
    };
    assert_eq!(min_of(&high), Some(256));
    assert_eq!(min_of(&mid), Some(128));
    assert_eq!(min_of(&low), None);

    // 三档都必须有优先级与亲和性（内存只影响工作集）。
    for plan in [&high, &mid, &low] {
        assert!(
            plan.hal_ops().contains(&HalOp::SetPriority),
            "{:?}",
            plan.hal_ops()
        );
        assert!(
            plan.hal_ops().contains(&HalOp::SetAffinity),
            "{:?}",
            plan.hal_ops()
        );
        assert!(plan
            .steps
            .iter()
            .any(|step| matches!(&step.action, PlanAction::Priority { class } if *class == PriorityClass::High)));
    }
    // 低内存档位要解释"为什么没有工作集动作"。
    assert!(low
        .skipped
        .iter()
        .any(|skip| skip.cause == gopt_policy::SkipCause::ExplicitSkip
            && skip.reason.zh.contains("内存不足 8GB")));
}

#[test]
fn physical_core_guard_disables_affinity_on_tiny_machines() {
    let set = gopt_policy::PolicyLoader::builtin_only().load().into_set();
    let plan = set
        .get("cs2")
        .expect("cs2")
        .plan(&EvalInput::new(hardware(2, 2, 16384, None), false), 1);

    // 物理核 <= 2：不绑定亲和性，并且给出解释（与 C++ 降级一致）。
    assert!(!plan
        .steps
        .iter()
        .any(|step| matches!(&step.action, PlanAction::Affinity { .. })));
    assert!(plan
        .skipped
        .iter()
        .any(|skip| skip.rule_id == "affinity-skipped-too-few-cores"));
    assert!(plan
        .skipped
        .iter()
        .any(|skip| skip.reason.zh.contains("物理核不足 3 个")));
}

#[test]
fn multi_group_machines_get_a_group_split_affinity_plan() {
    // 96 逻辑核（2 个处理器组）+ 仅物理核：计划变成"每组一段掩码"。
    let mut layout = Vec::new();
    for core in 0..48u32 {
        let mask = if core < 32 {
            0b11u64 << (core * 2)
        } else {
            0b11u64 << ((core - 32) * 2)
        };
        layout.push(gopt_hal::CoreLayout::new(core, core / 32, mask));
    }
    let big = gopt_hal::HardwareInfo {
        cpu_model: "Threadripper".to_string(),
        physical_cores: 48,
        logical_cores: 96,
        supports_hyper_threading: true,
        cpu_base_freq_mhz: 3500,
        core_layout: layout,
        processor_groups: gopt_hal::processor_groups(96),
        gpu: None,
        system_ram_mb: 65536,
        available_ram_mb: 32768,
        large_pages_available: false,
        warnings: Vec::new(),
    };
    let input = EvalInput::new(big, false);
    let set = gopt_policy::PolicyLoader::builtin_only().load().into_set();
    let plan = set.get("cs2").expect("cs2").plan(&input, 999);

    let step = plan
        .steps
        .iter()
        .find(|step| matches!(&step.action, PlanAction::Affinity { .. }))
        .expect("affinity step");
    let batches = step.affinity_batches();
    assert_eq!(batches.len(), 2, "跨组计划必须能拆成逐组批次");
    assert!(batches.iter().all(gopt_hal::AffinityPlan::is_single_group));
    assert_eq!(
        batches
            .iter()
            .map(gopt_hal::AffinityPlan::selected_logical)
            .sum::<u32>(),
        94
    );
    assert!(
        step.reason.zh.contains("跨 2 个处理器组"),
        "{}",
        step.reason.zh
    );
    assert!(
        step.reason.en.contains("2 processor groups"),
        "{}",
        step.reason.en
    );
}
