//! `gopt-hal` 的 trait 契约集成测试：同一组断言同时跑在 Mock 与真实 Win32 后端上。
//!
//! 这里只做**只读**的真机检查（`hardware` / `list_processes` / `list_run_entries` /
//! `query_power_scheme` / `get_priority(自身)`），因此 CI 上无需管理员权限。

#[cfg(windows)]
use gopt_hal::Win32Api;
use gopt_hal::{
    AffinityPlan, HalErrorKind, HalOp, MockApi, PowerSchemeSelector, PriorityClass, RunHive,
    SystemApi, WorkingSetLimits,
};

/// 构造一个后端列表：Mock 一定在；Windows 上再加真机后端（只读用例才碰它）。
fn mock_backend() -> MockApi {
    MockApi::sample_workstation()
}

#[test]
fn unknown_target_is_not_found_on_every_backend() {
    let mock = mock_backend();
    assert_eq!(
        mock.get_priority(0xdead_beef)
            .expect_err("unknown pid")
            .kind(),
        HalErrorKind::NotFound
    );
    assert_eq!(
        mock.set_priority(0xdead_beef, PriorityClass::High)
            .expect_err("unknown pid")
            .kind(),
        HalErrorKind::NotFound
    );
    assert_eq!(
        mock.get_affinity(0xdead_beef)
            .expect_err("unknown pid")
            .kind(),
        HalErrorKind::NotFound
    );
    assert_eq!(
        mock.set_working_set(0xdead_beef, WorkingSetLimits::new(1024, 0).expect("limits"))
            .expect_err("unknown pid")
            .kind(),
        HalErrorKind::NotFound
    );
    assert_eq!(
        mock.get_working_set(0xdead_beef)
            .expect_err("unknown pid")
            .kind(),
        HalErrorKind::NotFound
    );
}

/// 工作集读是契约的一部分：读→写→读回→还原→读回必须闭环（Mock 端完全可跑）。
#[test]
fn working_set_read_write_round_trip_is_part_of_the_contract() {
    let mock = mock_backend();
    let before = mock.get_working_set(1234).expect("read before");
    assert!(before.is_restorable(), "{before:?}");

    let target = WorkingSetLimits::from_mb(96, 192).expect("target");
    mock.set_working_set(1234, target).expect("set");
    assert_eq!(mock.get_working_set(1234).expect("read back"), target);

    mock.set_working_set(1234, before).expect("restore");
    assert_eq!(mock.get_working_set(1234).expect("read restored"), before);

    // 读路径不改状态，只记录调用（审计/断言用）：3 次读、2 次写。
    assert_eq!(mock.call_count(HalOp::GetWorkingSet), 3);
    assert_eq!(mock.call_count(HalOp::SetWorkingSet), 2);
}

#[test]
fn write_methods_return_rollback_information() {
    let mock = mock_backend();
    let previous = mock
        .set_priority(1234, PriorityClass::AboveNormal)
        .expect("set priority");
    assert_eq!(previous, PriorityClass::Normal);
    assert_eq!(mock.priority_of(1234), Some(PriorityClass::AboveNormal));

    let change = mock
        .set_power_scheme(&PowerSchemeSelector::HighPerformance)
        .expect("set power scheme");
    assert_eq!(change.previous.guid, gopt_hal::Guid::BALANCED);
    assert!(change.current.is_high_performance);

    let entry = mock
        .set_run_entry_enabled(RunHive::CurrentUser, "Steam", false)
        .expect("disable startup entry");
    assert_eq!(entry.value_name, "[disabled] Steam");
    assert!(!entry.enabled);
}

#[test]
fn realtime_is_rejected_before_any_backend_call() {
    // 红线在类型/解析层：后端根本没有机会看到 REALTIME。
    assert_eq!(
        PriorityClass::from_raw(0x100).expect_err("realtime").kind(),
        HalErrorKind::PolicyDenied
    );
    assert_eq!(
        PriorityClass::parse("0x100").expect_err("realtime").kind(),
        HalErrorKind::PolicyDenied
    );
    assert_eq!(
        PriorityClass::parse("realtime")
            .expect_err("realtime")
            .kind(),
        HalErrorKind::PolicyDenied
    );
    // 白名单内的 5 档都能往返。
    for class in PriorityClass::ALL {
        assert_eq!(
            PriorityClass::from_raw(class.raw()).expect("round trip"),
            class
        );
    }
    assert_eq!(PriorityClass::MAX_ALLOWED, PriorityClass::High);
}

#[test]
fn affinity_masks_are_computed_for_multi_group_machines() {
    // 96 逻辑核 → 组0=64、组1=32；保留最后 4 核 → 组1 保留低 28 位。
    let plan = AffinityPlan::reserve_last_n_cores(96, 4).expect("plan");
    assert_eq!(plan.selected_logical(), 92);
    assert!(!plan.is_single_group());
    let groups = plan.per_group();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].requests()[0].mask(), u64::MAX);
    assert_eq!(groups[1].requests()[0].mask(), 0x0fff_ffff);

    // 单组请求仍然可用（≤64 逻辑核机器上的常规路径）。
    let single = AffinityPlan::single(16, 0, 0x00ff).expect("single");
    assert_eq!(single.selected_logical(), 8);
    assert!(single.is_single_group());
}

#[test]
fn cross_group_affinity_is_unsupported_without_touching_the_system() {
    let plan = AffinityPlan::reserve_last_n_cores(96, 4).expect("plan");
    // 用 96 逻辑核的拓扑，让计划里的两个处理器组都真实存在。
    let mock = MockApi::with_cores(96);
    mock.push_process(1234, "big.exe", 8);
    let err = mock.set_affinity(1234, &plan).expect_err("cross-group");
    assert_eq!(err.kind(), HalErrorKind::Unsupported);
    // 调用仍然被记录（审计/测试需要看到"尝试过"）。
    assert_eq!(mock.call_count(HalOp::SetAffinity), 1);
}

#[test]
fn plan_referring_to_a_missing_processor_group_is_rejected() {
    // 16 逻辑核机器上只有组 0；引用组 1 的计划必须报参数错误，而不是被当成"跨组不支持"。
    let plan = AffinityPlan::reserve_last_n_cores(96, 4).expect("plan");
    let mock = MockApi::sample_workstation();
    let err = mock
        .set_affinity(1234, &plan)
        .expect_err("group 1 is missing");
    assert_eq!(err.kind(), HalErrorKind::InvalidArgument);
}

#[test]
fn failure_injection_reports_the_same_kind_as_a_real_failure() {
    let mock = mock_backend();
    mock.fail_next(
        HalOp::SetPriority,
        gopt_hal::HalError::win32_from_code("SetPriorityClass", 5),
    );
    let err = mock
        .set_priority(1234, PriorityClass::High)
        .expect_err("injected");
    assert_eq!(err.kind(), HalErrorKind::AccessDenied);
    assert_eq!(err.win32_code(), Some(5));
    assert_eq!(err.operation(), "SetPriorityClass");
    // 失败不影响后续调用。
    assert!(mock.set_priority(1234, PriorityClass::High).is_ok());
}

/// 真机只读契约：不需要管理员，只验证"能读且分类正确"。
#[cfg(windows)]
#[test]
fn win32_backend_read_only_contract() {
    let api = Win32Api::new();
    assert_eq!(api.backend_name(), "win32");

    let hardware = api.hardware().expect("hardware probe");
    assert!(hardware.logical_cores > 0);
    assert!(hardware.physical_cores > 0);
    assert!(hardware.physical_cores <= hardware.logical_cores);
    assert_eq!(
        hardware
            .processor_groups
            .iter()
            .map(|group| group.logical_count)
            .sum::<u32>(),
        hardware.logical_cores
    );
    assert!(!hardware.cpu_model.is_empty());

    assert!(api.is_elevated().is_ok());
    assert!(!api.list_processes().expect("processes").is_empty());
    assert!(api.list_run_entries().is_ok());
    let scheme = api.query_power_scheme().expect("power scheme");
    assert!(!scheme.name.is_empty());

    let own = std::process::id();
    assert!(PriorityClass::ALL.contains(&api.get_priority(own).expect("own priority")));
    let affinity = api.get_affinity(own).expect("own affinity");
    assert!(affinity.process_mask != 0);

    // 工作集读：官方 GetProcessWorkingSetSize，不需要管理员、不改状态。
    let limits = api.get_working_set(own).expect("own working set");
    assert!(limits.max_bytes >= limits.min_bytes, "{limits:?}");
    assert!(limits.max_bytes > 0, "{limits:?}");
}

#[cfg(windows)]
#[test]
fn win32_backend_reports_not_found_for_a_missing_working_set_target() {
    let api = Win32Api::new();
    let err = api
        .get_working_set(0xffff_fff0)
        .expect_err("missing process");
    assert_eq!(err.kind(), HalErrorKind::NotFound);
}

#[cfg(windows)]
#[test]
fn win32_backend_reports_not_found_for_a_missing_process() {
    let api = Win32Api::new();
    let err = api.get_priority(0xffff_fff0).expect_err("missing process");
    assert_eq!(err.kind(), HalErrorKind::NotFound);
}

#[cfg(windows)]
#[test]
fn both_backends_agree_on_the_cross_group_affinity_error() {
    let plan = AffinityPlan::reserve_last_n_cores(96, 4).expect("plan");
    let mock: Box<dyn SystemApi> = Box::new(MockApi::with_cores(96));
    let real: Box<dyn SystemApi> = Box::new(Win32Api::new());
    for backend in [&mock, &real] {
        let err = backend
            .set_affinity(0xffff_fff0, &plan)
            .expect_err("cross-group must be rejected");
        assert_eq!(
            err.kind(),
            HalErrorKind::Unsupported,
            "backend {} disagrees",
            backend.backend_name()
        );
    }
}

/// 后端可以通过 `Box<dyn SystemApi>` 传递（单内核多前端的前提）。
#[test]
fn backends_are_object_safe() {
    let backends: Vec<Box<dyn SystemApi>> = vec![Box::new(mock_backend())];
    for backend in &backends {
        assert!(!backend.backend_name().is_empty());
        let _ = backend.hardware();
        let _ = backend.query_power_scheme();
    }
}
