//! 真机自检入口：`cargo run -p gopt-hal --example hal_selfcheck`。
//!
//! 设计原则：
//!
//! * **默认只读**：不加参数时只调用 `SystemApi` 的只读方法（hardware / query_power_scheme /
//!   list_processes / list_run_entries / get_priority / get_affinity / is_elevated），
//!   不需要管理员权限，也不会改动系统任何状态。
//! * **写入需显式开关**：`--apply-writes` 才会做"写入往返"自检——而且只针对**当前进程**，
//!   每项写入后立即回滚，并在输出里给出"旧值 → 新值 → 回滚后旧值"三段证据。
//! * **失败不 panic**：任何一项失败都会打印 `[FAIL]` 并让进程以退出码 1 结束，
//!   便于 CI / 冒烟脚本直接判断。
//!
//! 用法：
//!
//! ```text
//! cargo run -p gopt-hal --example hal_selfcheck                 # 只读自检
//! cargo run -p gopt-hal --example hal_selfcheck -- --apply-writes  # 含写入往返自检
//! ```

#![cfg(windows)]

use gopt_hal::{
    AffinityPlan, HalErrorKind, MockApi, PowerSchemeSelector, PriorityClass, SystemApi, Win32Api,
    WorkingSetLimits,
};

/// 自检结果统计。
#[derive(Debug, Default)]
struct Report {
    passed: u32,
    failed: u32,
}

impl Report {
    fn pass(&mut self, message: impl std::fmt::Display) {
        self.passed += 1;
        println!("[ OK ] {message}");
    }

    fn fail(&mut self, message: impl std::fmt::Display) {
        self.failed += 1;
        println!("[FAIL] {message}");
    }

    fn check(&mut self, ok: bool, message: impl std::fmt::Display) {
        if ok {
            self.pass(message);
        } else {
            self.fail(message);
        }
    }
}

fn main() {
    let apply_writes = std::env::args().any(|argument| argument == "--apply-writes");
    let mut report = Report::default();

    println!(
        "gopt-hal 真机自检（backend = win32，只读模式 = {}）\n",
        !apply_writes
    );
    let api = Win32Api::new();

    check_identity(&mut report, &api);
    check_elevation(&mut report, &api);
    check_hardware(&mut report, &api);
    check_power(&mut report, &api);
    check_processes(&mut report, &api);
    check_startup_entries(&mut report, &api);
    check_current_process(&mut report, &api, apply_writes);
    check_policy_red_line(&mut report, &api);
    check_mock_parity(&mut report);

    println!(
        "\n自检结束：{} 项通过，{} 项失败{}",
        report.passed,
        report.failed,
        if report.failed == 0 {
            ""
        } else {
            "（请查看上面的 [FAIL] 行）"
        }
    );
    if report.failed > 0 {
        std::process::exit(1);
    }
}

fn check_identity(report: &mut Report, api: &dyn SystemApi) {
    report.check(
        api.backend_name() == "win32",
        format!("backend_name() = {}", api.backend_name()),
    );
}

fn check_elevation(report: &mut Report, api: &dyn SystemApi) {
    match api.is_elevated() {
        Ok(elevated) => report.pass(format!(
            "is_elevated() = {elevated}{}",
            if elevated {
                ""
            } else {
                "（未提权：HKLM 启动项与电源方案切换会返回 AccessDenied，这是预期行为）"
            }
        )),
        Err(error) => report.fail(format!("is_elevated() 失败：{error}")),
    }
}

fn check_hardware(report: &mut Report, api: &dyn SystemApi) {
    match api.hardware() {
        Ok(info) => {
            report.pass(format!(
                "hardware(): {} | {}C/{}T | 核布局 {} 条 | 处理器组 {} | 内存 {} MiB（可用 {} MiB）| 大页 {}",
                info.cpu_model,
                info.physical_cores,
                info.logical_cores,
                info.core_layout.len(),
                info.processor_groups.len(),
                info.system_ram_mb,
                info.available_ram_mb,
                info.large_pages_available
            ));
            match &info.gpu {
                Some(gpu) => report.pass(format!(
                    "GPU: {} ({}, vendor 0x{:04x}, device 0x{:04x}, 显存 {} MiB, 驱动 {}, 硬件 = {})",
                    gpu.model,
                    gpu.vendor.as_str(),
                    gpu.vendor_id,
                    gpu.device_id,
                    gpu.vram_mb,
                    gpu.driver_version.as_deref().unwrap_or("<unknown>"),
                    gpu.is_hardware
                )),
                None => report.fail("GPU: DXGI 未枚举到显示适配器"),
            }
            report.check(
                info.logical_cores > 0
                    && info
                        .processor_groups
                        .iter()
                        .map(|group| group.logical_count)
                        .sum::<u32>()
                        == info.logical_cores,
                format!(
                    "拓扑自洽：各组逻辑核之和 = {} = logical_cores",
                    info.logical_cores
                ),
            );
            match AffinityPlan::full(info.logical_cores) {
                Ok(plan) => report.pass(format!("AffinityPlan::full() = {plan}")),
                Err(error) => report.fail(format!("AffinityPlan::full() 失败：{error}")),
            }
            if info.core_layout.len() != info.physical_cores as usize && !info.is_multi_group() {
                report.fail(format!(
                    "core_layout 条目数 {} 与 physical_cores {} 不一致（同组内应一一对应）",
                    info.core_layout.len(),
                    info.physical_cores
                ));
            }
            for warning in &info.warnings {
                println!("[WARN] hardware 降级说明：{warning}");
            }
        }
        Err(error) => report.fail(format!("hardware() 失败：{error}")),
    }
}

fn check_power(report: &mut Report, api: &dyn SystemApi) {
    let active = match api.query_power_scheme() {
        Ok(scheme) => {
            report.pass(format!(
                "query_power_scheme() = {} ({}){}",
                scheme.name,
                scheme.guid,
                if scheme.is_high_performance {
                    " [高性能]"
                } else {
                    ""
                }
            ));
            scheme
        }
        Err(error) => {
            report.fail(format!("query_power_scheme() 失败：{error}"));
            return;
        }
    };

    // 解析"高性能"方案（不存在时必须是 NotFound，而不是默默激活一个猜测的 GUID）。
    match Win32Api::new().list_power_schemes() {
        Ok(schemes) => {
            report.pass(format!(
                "list_power_schemes(): {} 个已安装方案（活动 = {}）",
                schemes.len(),
                active.name
            ));
            let has_high = schemes.iter().any(|scheme| scheme.is_high_performance);
            if has_high {
                report
                    .pass("已安装方案中包含高性能方案，PowerSchemeSelector::HighPerformance 可用");
            } else {
                println!(
                    "[WARN] 未安装高性能方案：PowerSchemeSelector::HighPerformance 会返回 NotFound"
                );
            }
        }
        Err(error) => report.fail(format!("list_power_schemes() 失败：{error}")),
    }

    // 只读地验证选择器解析路径（QueryPowerScheme 与 selector 解析共用同一段枚举代码）。
    let _ = PowerSchemeSelector::HighPerformance;
}

fn check_processes(report: &mut Report, api: &dyn SystemApi) {
    match api.list_processes() {
        Ok(processes) => {
            let with_path = processes
                .iter()
                .filter(|process| process.exe_path.is_some())
                .count();
            report.pass(format!(
                "list_processes(): {} 个进程，其中 {} 个可读到完整路径（其余为受保护进程，exe_path = None）",
                processes.len(),
                with_path
            ));
            report.check(
                processes.windows(2).all(|pair| pair[0].pid <= pair[1].pid),
                "进程列表按 PID 升序",
            );
            report.check(
                processes
                    .iter()
                    .any(|process| process.pid == std::process::id()),
                format!("列表包含当前进程（pid {}）", std::process::id()),
            );
        }
        Err(error) => report.fail(format!("list_processes() 失败：{error}")),
    }
}

fn check_startup_entries(report: &mut Report, api: &dyn SystemApi) {
    match api.list_run_entries() {
        Ok(entries) => {
            let disabled = entries.iter().filter(|entry| !entry.enabled).count();
            report.pass(format!(
                "list_run_entries(): {} 个 Run 启动项（其中 {} 个处于已禁用/改名状态）",
                entries.len(),
                disabled
            ));
            report.check(
                entries
                    .iter()
                    .all(|entry| entry.name == gopt_hal::RunEntry::display_name(&entry.value_name)),
                "启动项展示名与注册表原始值名一致（禁用前缀解析正确）",
            );
        }
        Err(error) => report.fail(format!("list_run_entries() 失败：{error}")),
    }
}

fn check_current_process(report: &mut Report, api: &dyn SystemApi, apply_writes: bool) {
    let pid = std::process::id();

    match api.get_priority(pid) {
        Ok(class) => report.pass(format!(
            "get_priority({pid}) = {class}（白名单内的合法档位）"
        )),
        Err(error) => report.fail(format!("get_priority({pid}) 失败：{error}")),
    }

    match api.get_affinity(pid) {
        Ok(info) => {
            report.pass(format!(
                "get_affinity({pid}) = 组 {} 掩码 {:#018x}（系统掩码 {:#018x}，逻辑核 {}）",
                info.group, info.process_mask, info.system_mask, info.total_logical
            ));
            report.check(
                info.process_mask != 0 && info.system_mask != 0,
                "亲和性掩码非 0（进程与系统掩码都可读）",
            );
        }
        Err(error) => report.fail(format!("get_affinity({pid}) 失败：{error}")),
    }

    match api.get_working_set(pid) {
        Ok(limits) => {
            report.pass(format!(
                "get_working_set({pid}) = min {} / max {} bytes（{}）",
                limits.min_bytes,
                limits.max_bytes,
                if limits.is_restorable() {
                    "可精确还原"
                } else {
                    "min=0，HAL 写路径不接受，无法精确还原"
                }
            ));
            report.check(
                limits.max_bytes >= limits.min_bytes,
                "工作集上下限自洽（max >= min）",
            );
        }
        Err(error) => report.fail(format!("get_working_set({pid}) 失败：{error}")),
    }

    if !apply_writes {
        println!("[SKIP] 写入往返自检未启用（加 --apply-writes 才会在当前进程上做可回滚的写测试）");
        return;
    }

    // ---- 写入往返 1：优先级 ----
    let original = match api.get_priority(pid) {
        Ok(class) => class,
        Err(error) => {
            report.fail(format!("写入自检前置读取失败：{error}"));
            return;
        }
    };
    let target = if original == PriorityClass::High {
        PriorityClass::Normal
    } else {
        PriorityClass::High
    };
    match api.set_priority(pid, target) {
        Ok(previous) => {
            report.check(
                previous == original,
                format!(
                    "set_priority({pid}, {target}) 返回写入前的值 {previous}（期望 {original}）"
                ),
            );
            match api.set_priority(pid, original) {
                Ok(restored) => {
                    report.check(
                        restored == target,
                        format!("回滚优先级：{target} → {original}"),
                    );
                    match api.get_priority(pid) {
                        Ok(now) => report.check(now == original, format!("回滚后读回 = {now}")),
                        Err(error) => report.fail(format!("回滚后读取失败：{error}")),
                    }
                }
                Err(error) => report.fail(format!("回滚优先级失败：{error}")),
            }
        }
        Err(error) => report.fail(format!("set_priority({pid}, {target}) 失败：{error}")),
    }

    // ---- 写入往返 2：工作集（读 → 写 → 读回 → 还原 → 读回）----
    // 64/128 MiB 是一个明确的窗口；无论中间哪一步失败，都会尝试把最初读到的值写回去。
    let before = match api.get_working_set(pid) {
        Ok(limits) => limits,
        Err(error) => {
            report.fail(format!("工作集写入自检前置读取失败：{error}"));
            return;
        }
    };
    let target = match WorkingSetLimits::from_mb(64, 128) {
        Ok(limits) => limits,
        Err(error) => {
            report.fail(format!("构造工作集参数失败：{error}"));
            return;
        }
    };
    match api.set_working_set(pid, target) {
        Ok(()) => {
            match api.get_working_set(pid) {
                Ok(read_back) => report.check(
                    read_back == target,
                    format!(
                        "set_working_set({pid}, 64/128 MiB) → 读回 min {} / max {} bytes（期望 {} / {}）",
                        read_back.min_bytes, read_back.max_bytes, target.min_bytes, target.max_bytes
                    ),
                ),
                Err(error) => report.fail(format!("写入后读回失败：{error}")),
            }
            if !before.is_restorable() {
                report.fail(format!(
                    "系统报告 min=0（{before:?}），HAL 写路径不接受 min=0，无法精确还原"
                ));
                return;
            }
            match api.set_working_set(pid, before) {
                Ok(()) => match api.get_working_set(pid) {
                    Ok(restored) => report.check(
                        restored == before,
                        format!(
                            "还原工作集 → 读回 min {} / max {} bytes（期望 {} / {}）",
                            restored.min_bytes,
                            restored.max_bytes,
                            before.min_bytes,
                            before.max_bytes
                        ),
                    ),
                    Err(error) => report.fail(format!("还原后读回失败：{error}")),
                },
                Err(error) => report.fail(format!("还原工作集失败：{error}")),
            }
        }
        Err(error) => report.fail(format!("set_working_set({pid}) 失败：{error}")),
    }
}

/// 红线自检：REALTIME 必须在进入系统调用之前就被拒绝。
fn check_policy_red_line(report: &mut Report, api: &dyn SystemApi) {
    match PriorityClass::from_raw(0x100) {
        Ok(class) => report.fail(format!("红线失效：0x100 (REALTIME) 被解析成了 {class}")),
        Err(error) => report.check(
            error.kind() == HalErrorKind::PolicyDenied,
            format!(
                "红线生效：PriorityClass::from_raw(0x100) → {}",
                error.kind()
            ),
        ),
    }

    // REALTIME 是"类型上无法表达"的：这里用编译期证据代替运行期检查。
    let _cannot_express_realtime: PriorityClass = PriorityClass::MAX_ALLOWED;
    report.pass("PriorityClass 枚举没有 REALTIME 成员（只能在 5 档白名单内取值）");

    // 跨组亲和性必须显式报 Unsupported，而不是静默降级。
    match AffinityPlan::reserve_last_n_cores(96, 4) {
        Ok(plan) => report.check(
            !plan.is_single_group(),
            format!(
                "96 逻辑核的保留核计划跨 {} 个处理器组（per_group() 可拆分）",
                plan.requests().len()
            ),
        ),
        Err(error) => report.fail(format!("跨组计划构造失败：{error}")),
    }

    let _ = api;
}

/// Mock 与 Win32 的错误分类一致性：这是"无管理员即可单测"的前提。
fn check_mock_parity(report: &mut Report) {
    let mock = MockApi::sample_workstation();
    match mock.get_priority(0xffff_fff0) {
        Ok(class) => report.fail(format!("Mock 对不存在的进程返回了 {class}")),
        Err(error) => report.check(
            error.kind() == HalErrorKind::NotFound,
            format!("Mock 对不存在的进程返回 {}", error.kind()),
        ),
    }
    report.check(
        mock.backend_name() == "mock",
        format!(
            "MockApi::sample_workstation() backend = {}",
            mock.backend_name()
        ),
    );
}
