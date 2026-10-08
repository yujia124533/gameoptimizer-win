//! 策略引擎真机自检（**只读**：采集硬件画像 + 枚举进程 + 打印计划，不做任何写入）。
//!
//! ```powershell
//! cd rust
//! cargo run -p gopt-policy --example policy_selfcheck
//! cargo run -p gopt-policy --example policy_selfcheck -- --verbose   # 打印跳过说明与规则行号
//! ```
//!
//! 它刻意不需要管理员：硬件画像与进程枚举都是只读官方 API；
//! 目的有两个——(1) 用真机画像验证"策略 → 计划"链路，(2) 让人一眼看懂
//! "当前这台机器上、这些正在运行的游戏会得到什么计划、为什么"。

#[cfg(windows)]
fn main() {
    use gopt_hal::{HalError, SystemApi, Win32Api};
    use gopt_policy::{EvalInput, Plan, PolicyLoader};

    let verbose = std::env::args().any(|arg| arg == "--verbose");
    let mut failures = 0u32;

    println!("== 策略引擎自检（只读，不需要管理员）==");

    // --- 1) 加载策略：内置 + 用户覆盖 ---
    let loader = PolicyLoader::new();
    let default_user_dir = PolicyLoader::default_user_dir();
    println!(
        "内置策略：{} 个文件；用户覆盖目录：{}",
        gopt_policy::BUILTIN_FILES.len(),
        default_user_dir.as_ref().map_or_else(
            || "<未设置 LOCALAPPDATA>".to_string(),
            |dir| dir.display().to_string()
        )
    );

    let outcome = loader.load();
    println!(
        "已加载 {} 款游戏策略（错误 {} / 警告 {}）",
        outcome.set().len(),
        outcome.errors().len(),
        outcome.warnings().len()
    );
    for diagnostic in outcome.diagnostics() {
        println!("  [诊断] {diagnostic}");
        failures += u32::from(diagnostic.is_error());
    }
    let set = outcome.into_set();
    if set.is_empty() {
        println!("没有任何可用策略，自检结束");
        std::process::exit(1);
    }

    // --- 2) 真机画像 + 进程枚举（全部只读）---
    let api = Win32Api::new();
    let hardware = match api.hardware() {
        Ok(hardware) => hardware,
        Err(err) => {
            report("hardware()", &err);
            std::process::exit(1);
        }
    };
    println!(
        "\n硬件画像：{} / {} 物理核 / {} 逻辑核{} / {} MiB 内存 / GPU {}",
        hardware.cpu_model,
        hardware.physical_cores,
        hardware.logical_cores,
        if hardware.supports_hyper_threading {
            "（SMT）"
        } else {
            ""
        },
        hardware.system_ram_mb,
        hardware.gpu.as_ref().map_or_else(
            || "未探测到".to_string(),
            |gpu| format!("{} ({} MiB)", gpu.model, gpu.vram_mb)
        )
    );
    for warning in &hardware.warnings {
        println!("  [降级] {warning}");
    }

    let input = match EvalInput::from_api(&api) {
        Ok(input) => input,
        Err(err) => {
            report("EvalInput::from_api", &err);
            std::process::exit(1);
        }
    };
    println!(
        "求值输入：{} 逻辑核 / {} 物理核 / {} GiB 内存 / 显卡厂商 {:?} / 已提权 = {}",
        input.hardware().logical_cores,
        input.hardware().physical_cores,
        input.ram_gb(),
        input.gpu_vendor(),
        input.is_elevated()
    );

    // --- 3) 策略清单 ---
    println!("\n== 策略清单（用户层在前）==");
    for game in set.games() {
        println!(
            "  {:<18} {:<10} {:<28} 规则 {} 条  来源 {}",
            game.id(),
            game.name_zh(),
            game.patterns().collect::<Vec<&str>>().join(" | "),
            game.rules().len(),
            game.origin()
        );
    }

    // --- 4) 正在运行的游戏 → 计划 ---
    println!("\n== 正在运行的游戏进程 → 计划 ==");
    let processes = match api.list_processes() {
        Ok(processes) => processes,
        Err(err) => {
            report("list_processes()", &err);
            std::process::exit(1);
        }
    };
    let mut planned = 0usize;
    for process in &processes {
        let Some(game) = set.match_process_name(&process.name) else {
            continue;
        };
        planned += 1;
        let plan = game.plan(&input, process.pid);
        print_plan(&plan, verbose);
    }
    if planned == 0 {
        println!(
            "  （当前没有命中策略的进程；内置目录里共 {} 款游戏可匹配）",
            set.len()
        );
    }

    // --- 5) 用户策略演示：把 example-custom.toml 当成用户覆盖再加载一次 ---
    println!("\n== 校验“用户覆盖”路径（用内置 example-custom.toml 当作用户文件）==");
    let demo_dir =
        std::env::temp_dir().join(format!("gopt-policy-selfcheck-{}", std::process::id()));
    if std::fs::create_dir_all(&demo_dir).is_ok() {
        if let Some(file) = gopt_policy::builtin_file("example-custom.toml") {
            let path = demo_dir.join(file.name);
            if std::fs::write(&path, file.text).is_ok() {
                let override_outcome = PolicyLoader::builtin_only().with_user_dir(&demo_dir).load();
                println!(
                    "  用户层加载：{} 款策略，错误 {} 警告 {}（同一份 TOML 在用户层同样生效）",
                    override_outcome.set().len(),
                    override_outcome.errors().len(),
                    override_outcome.warnings().len()
                );
                if override_outcome.set().get("naraka-bladepoint").is_none() {
                    println!("  [失败] 用户层没有加载到 naraka-bladepoint");
                    failures += 1;
                }
            }
        }
        let _ = std::fs::remove_dir_all(&demo_dir);
    }

    println!("\n== 结论 ==");
    if failures == 0 {
        println!(
            "策略引擎自检通过：策略加载、真机画像、进程匹配、计划生成全部正常（未做任何写入）"
        );
    } else {
        println!("策略引擎自检发现 {failures} 个问题，见上面的 [诊断]/[失败] 行");
        std::process::exit(1);
    }

    fn print_plan(plan: &Plan, verbose: bool) {
        println!(
            "\n  {} (pid {}) ← {}",
            plan.game_name_zh, plan.pid, plan.policy_origin
        );
        if plan.steps.is_empty() {
            println!("    没有任何可执行步骤");
        }
        for step in &plan.steps {
            let flags = match (step.requires_elevation, step.is_dangerous) {
                (false, false) => String::new(),
                (true, false) => "  [需要管理员]".to_string(),
                (false, true) => "  [危险动作]".to_string(),
                (true, true) => "  [需要管理员][危险动作]".to_string(),
            };
            println!("    {}. [{}]{flags}", step.order, step.rule_id);
            println!("       zh: {}", step.reason.zh);
            println!("       en: {}", step.reason.en);
            if verbose {
                if let Some(line) = step.rule_line {
                    println!("       rule: {}:{}", plan.policy_origin, line);
                }
                println!("       hal: {:?}", step.hal_op());
            }
        }
        for skip in &plan.skipped {
            println!("    - [{}] {}", skip.rule_id, skip.reason.zh);
            if verbose {
                println!("       en: {} ({})", skip.reason.en, skip.cause.as_str());
                if let Some(line) = skip.rule_line {
                    println!("       rule: {}:{}", plan.policy_origin, line);
                }
            }
        }
    }

    fn report(operation: &str, error: &HalError) {
        eprintln!("[失败] {operation}: {error}");
    }
}

#[cfg(not(windows))]
fn main() {
    println!("策略引擎自检目前只在 Windows 上运行（Win32 后端 + 官方 API）");
}
