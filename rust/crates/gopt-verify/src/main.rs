//! `gopt-verify` —— **真机往返自检**：在不改动机器长期状态的前提下，逐项证明
//! "写进去 → 读回来 → 还原"这条链在真机上是通的。
//!
//! 检查项（每项都打印**前后值**与 PASS/FAIL，最后一行是 `RESULT: PASS`）：
//!
//! | # | 项 | 说明 |
//! | --- | --- | --- |
//! | 1 | `hardware` | 只读：CPU/内存/GPU/处理器组（策略条件求值的输入） |
//! | 2 | `priority_round_trip` | 自身进程 HIGH → 读回 → 还原（红线：永不 REALTIME） |
//! | 3 | `affinity_round_trip` | 自身进程改掩码 → 读回 → 还原 |
//! | 4 | `working_set_round_trip` | 自身进程设上下限 → 读回 → 还原 → 读回（HAL 的工作集读） |
//! | 5 | `power_scheme_query` | 只读：查询当前电源方案 |
//! | 6 | `run_entries_read` | 只读：读取 HKCU/HKLM 启动项 |
//! | 7 | `journal_round_trip` | 在 %TEMP% 里建链 → 校验 → 篡改检出 → 还原 |
//!
//! 只碰**自己的进程**（`GetCurrentProcess` / `std::process::id()`）与 `%TEMP%` 下的临时目录：
//! 不需要管理员，跑完不会留下任何系统改动（优先级/亲和性/工作集都还原成原值）。
//!
//! 退出码：`0` 全部 PASS / `1` 有 FAIL / `2` 环境不满足（参数错误、无法建立会话等）。
//!
//! 用法：
//!
//! ```text
//! gopt-verify [--lang zh|en] [--json] [--keep-temp]
//! ```
//!
//! `--json` 时输出一份结构化报告（不再打印终端的 `RESULT:` 行，改成 JSON 里的 `result` 字段）。

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented
)]
// 测试代码允许 unwrap/expect/panic：测试失败必须显式炸出来。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// 本 crate **零 unsafe**：所有系统调用都经 `gopt-hal::SystemApi`（包括工作集读回），
// 因此这里 `forbid(unsafe_code)` —— 它不是承诺，是编译器保证。

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gopt_hal::{
    AffinityPlan, AffinityRequest, Guid, HalError, PowerSchemeSelector, PriorityClass, RunEntry,
    RunHive, SystemApi, Win32Api, WorkingSetLimits,
};
use gopt_journal::{payload, Journal, JournalDraft, JournalKind, JournalOptions, GENESIS_HASH};

/// 语言（与 CLI 的 `--lang` 取值一致；这里独立实现，避免为了两个字符串依赖 gopt-core）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Zh,
    En,
}

impl Lang {
    fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "zh" | "zh-cn" | "cn" | "中文" => Some(Lang::Zh),
            "en" | "en-us" | "english" => Some(Lang::En),
            _ => None,
        }
    }

    fn pick<'a>(self, zh: &'a str, en: &'a str) -> &'a str {
        match self {
            Lang::Zh => zh,
            Lang::En => en,
        }
    }
}

/// 单项检查结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Skip,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }
}

/// 一项检查的报告。
#[derive(Debug, Clone)]
struct Check {
    id: &'static str,
    title_zh: &'static str,
    title_en: &'static str,
    status: Status,
    before: String,
    after: String,
    detail: String,
    millis: u128,
}

impl Check {
    fn new(id: &'static str, title_zh: &'static str, title_en: &'static str) -> Self {
        Self {
            id,
            title_zh,
            title_en,
            status: Status::Pass,
            before: "-".to_string(),
            after: "-".to_string(),
            detail: String::new(),
            millis: 0,
        }
    }

    fn pass(mut self, before: impl Into<String>, after: impl Into<String>) -> Self {
        self.status = Status::Pass;
        self.before = before.into();
        self.after = after.into();
        self
    }

    fn fail(
        mut self,
        before: impl Into<String>,
        after: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        self.status = Status::Fail;
        self.before = before.into();
        self.after = after.into();
        self.detail = detail.into();
        self
    }

    fn skip(mut self, detail: impl Into<String>) -> Self {
        self.status = Status::Skip;
        self.detail = detail.into();
        self
    }

    fn note(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    fn title(&self, lang: Lang) -> &'static str {
        lang.pick(self.title_zh, self.title_en)
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "title_zh": self.title_zh,
            "title_en": self.title_en,
            "status": self.status.as_str(),
            "before": self.before,
            "after": self.after,
            "detail": self.detail,
            "millis": self.millis,
        })
    }
}

/// 命令行开关。
#[derive(Debug, Clone, Copy)]
struct Options {
    lang: Lang,
    json: bool,
    keep_temp: bool,
}

fn parse(argv: &[String]) -> Result<Options, String> {
    let mut options = Options {
        lang: match std::env::var("GOPT_LANG") {
            Ok(value) => Lang::parse(&value).unwrap_or(Lang::Zh),
            Err(_) => Lang::Zh,
        },
        json: false,
        keep_temp: false,
    };
    let mut index = 0usize;
    while index < argv.len() {
        let token = argv[index].as_str();
        match token {
            "--json" | "-j" => options.json = true,
            "--keep-temp" => options.keep_temp = true,
            "--help" | "-h" => return Err("help".to_string()),
            "--lang" => {
                index += 1;
                let value = argv.get(index).ok_or("--lang needs a value (zh|en)")?;
                options.lang = Lang::parse(value).ok_or("--lang accepts zh or en only")?;
            }
            other => {
                if let Some(value) = other.strip_prefix("--lang=") {
                    options.lang = Lang::parse(value).ok_or("--lang accepts zh or en only")?;
                } else {
                    return Err(format!("unknown argument `{other}` (try --help)"));
                }
            }
        }
        index += 1;
    }
    Ok(options)
}

fn usage(lang: Lang) -> String {
    lang.pick(
        "gopt-verify —— 真机往返自检\n\n用法: gopt-verify [--lang zh|en] [--json] [--keep-temp]\n\n\
         逐项检查: hardware / priority / affinity / working_set / power_scheme / run_entries / journal\n\
         最后一行输出 RESULT: PASS 或 RESULT: FAIL",
        "gopt-verify — real-machine round-trip self-check\n\nusage: gopt-verify [--lang zh|en] [--json] [--keep-temp]\n\n\
         checks: hardware / priority / affinity / working_set / power_scheme / run_entries / journal\n\
         the last line is RESULT: PASS or RESULT: FAIL",
    )
    .to_string()
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse(&argv) {
        Ok(options) => options,
        Err(message) => {
            let lang = Lang::parse(
                argv.iter()
                    .position(|arg| arg == "--lang")
                    .and_then(|index| argv.get(index + 1).map(String::as_str))
                    .unwrap_or("zh"),
            )
            .unwrap_or(Lang::Zh);
            if message == "help" {
                println!("{}", usage(lang));
                std::process::exit(0);
            }
            println!(
                "{}: {message}\n\n{}",
                lang.pick("参数错误", "argument error"),
                usage(lang)
            );
            std::process::exit(2);
        }
    };

    let api = Win32Api::new();
    let pid = std::process::id();
    let started = Instant::now();
    let mut checks: Vec<Check> = Vec::new();

    let backend = api.backend_name().to_string();
    let elevated = api.is_elevated().unwrap_or(false);

    checks.push(check_hardware(&api, options.lang));
    checks.push(check_priority(&api, pid, options.lang));
    checks.push(check_affinity(&api, pid, options.lang));
    checks.push(check_working_set(&api, pid, options.lang));
    checks.push(check_power_scheme(&api, options.lang));
    checks.push(check_run_entries(&api, options.lang));
    checks.push(check_journal(options.lang, options.keep_temp));

    let failed = checks
        .iter()
        .filter(|check| check.status == Status::Fail)
        .count();
    let skipped = checks
        .iter()
        .filter(|check| check.status == Status::Skip)
        .count();
    let passed = checks
        .iter()
        .filter(|check| check.status == Status::Pass)
        .count();
    let result = if failed == 0 { "PASS" } else { "FAIL" };

    if options.json {
        let report = serde_json::json!({
            "schema_version": 1,
            "ok": failed == 0,
            "product": "GameOptimizer-RS",
            "tool": "gopt-verify",
            "version": env!("CARGO_PKG_VERSION"),
            "phase": "Phase 1",
            "backend": backend,
            "pid": pid,
            "elevated": elevated,
            "result": result,
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "millis": started.elapsed().as_millis(),
            "checks": checks.iter().map(Check::json).collect::<Vec<serde_json::Value>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!(
            "gopt-verify {} (GameOptimizer-RS {}) — {}",
            env!("CARGO_PKG_VERSION"),
            options.lang.pick("Phase 1", "Phase 1"),
            options.lang.pick(
                "真机往返自检（只碰自己的进程与 %TEMP%）",
                "real-machine round-trip self-check (own process and %TEMP% only)"
            )
        );
        println!(
            "pid {pid} / backend {backend} / {}{}",
            options.lang.pick("提权: ", "elevated: "),
            if elevated { "yes" } else { "no" }
        );
        println!();
        for (index, check) in checks.iter().enumerate() {
            println!(
                "{:>2}. [{}] {:<22} ({:>4} ms) {}",
                index + 1,
                check.status.as_str(),
                check.id,
                check.millis,
                check.title(options.lang)
            );
            println!(
                "      {} {}",
                options.lang.pick("前:", "before:"),
                check.before
            );
            println!(
                "      {} {}",
                options.lang.pick("后:", "after :"),
                check.after
            );
            if !check.detail.is_empty() {
                println!(
                    "      {} {}",
                    options.lang.pick("说明:", "note  :"),
                    check.detail
                );
            }
        }
        println!();
        println!(
            "{}: {} {} / {} {} / {} {} / {} ms",
            options.lang.pick("汇总", "summary"),
            passed,
            options.lang.pick("通过", "passed"),
            failed,
            options.lang.pick("失败", "failed"),
            skipped,
            options.lang.pick("跳过", "skipped"),
            started.elapsed().as_millis()
        );
        // 契约：最后一行必须是 `RESULT: PASS` / `RESULT: FAIL`（脚本可 grep）。
        println!("RESULT: {result}");
    }

    std::process::exit(if failed == 0 { 0 } else { 1 });
}

// ---------------------------------------------------------------------------
// 各检查项
// ---------------------------------------------------------------------------

/// 1) 硬件画像（只读）。
fn check_hardware(api: &Win32Api, lang: Lang) -> Check {
    let started = Instant::now();
    let mut check = Check::new("hardware", "硬件画像（只读）", "hardware (read-only)");
    match api.hardware() {
        Ok(hardware) => {
            let gpu = hardware
                .gpu
                .as_ref()
                .map(|gpu| {
                    format!(
                        "{} ({}, {} MiB)",
                        gpu.model,
                        gpu.vendor.as_str(),
                        gpu.vram_mb
                    )
                })
                .unwrap_or_else(|| "-".to_string());
            let before = format!(
                "{}C/{}T, {} MiB RAM",
                hardware.physical_cores, hardware.logical_cores, hardware.system_ram_mb
            );
            let after = format!(
                "{} / {} = {} / {} groups",
                hardware.cpu_model.trim(),
                gpu,
                hardware.processor_groups.len(),
                hardware.core_layout.len()
            );
            check = if hardware.logical_cores == 0 || hardware.physical_cores == 0 {
                check.fail(before, after, "the OS reported zero processors")
            } else {
                check.pass(before, after)
            };
            if !hardware.warnings.is_empty() {
                check = check.note(format!("warnings: {}", hardware.warnings.join(" | ")));
            }
        }
        Err(error) => {
            check = check.fail("-", "-", format!("hardware() failed: {}", describe(&error)));
        }
    }
    check.millis = started.elapsed().as_millis();
    let _ = lang;
    check
}

/// 2) 优先级往返：HIGH → 读回 → 还原。
fn check_priority(api: &Win32Api, pid: u32, lang: Lang) -> Check {
    let started = Instant::now();
    let mut check = Check::new(
        "priority_round_trip",
        "优先级 HIGH → 读回 → 还原",
        "priority HIGH → read back → restore",
    );
    let read = api.get_priority(pid);
    let before = match &read {
        Ok(class) => *class,
        Err(error) => {
            check = check.fail(
                "-",
                "-",
                format!(
                    "{} {}",
                    lang.pick("读取自身优先级失败:", "cannot read the own priority:"),
                    describe(error)
                ),
            );
            check.millis = started.elapsed().as_millis();
            return check;
        }
    };

    // HIGH 是本工具允许的最高档；REALTIME 在类型层面不可表达。
    let raised = api.set_priority(pid, PriorityClass::High);
    let read_back = api.get_priority(pid);
    let restored = api.set_priority(pid, before);
    let restored_read_back = api.get_priority(pid);

    let after_text = format!(
        "raise={:?} read_back={:?} restore={:?} read_back={:?}",
        raised.as_ref().map(|_| PriorityClass::High.as_str()),
        read_back.as_ref().map(|class| class.as_str()),
        restored.as_ref().map(|class| class.as_str()),
        restored_read_back.as_ref().map(|class| class.as_str())
    );
    match (raised, read_back, restored, restored_read_back) {
        (Ok(previous), Ok(high), Ok(_), Ok(back)) => {
            let mut ok = high == PriorityClass::High && back == before;
            let mut detail = format!(
                "{} (set_priority returned the previous class: {})",
                lang.pick(
                    "自身进程上完成了一次完整往返",
                    "a full round trip on the own process"
                ),
                previous.as_str()
            );
            if high != PriorityClass::High {
                detail = format!(
                    "{}: {}",
                    lang.pick("读回不是 HIGH", "read-back is not HIGH"),
                    high.as_str()
                );
                ok = false;
            }
            if back != before {
                detail = format!(
                    "{}: {} -> {}",
                    lang.pick(
                        "还原后读回与写入前不一致",
                        "the restored value differs from the original"
                    ),
                    before.as_str(),
                    back.as_str()
                );
                ok = false;
            }
            check = if ok {
                check.pass(before.as_str(), after_text).note(detail)
            } else {
                check.fail(before.as_str(), after_text, detail)
            };
        }
        (Err(error), _, _, _) => {
            check = check.fail(
                before.as_str(),
                after_text,
                format!("set HIGH failed: {}", describe(&error)),
            )
        }
        (_, Err(error), _, _) => {
            check = check.fail(
                before.as_str(),
                after_text,
                format!("read-back failed: {}", describe(&error)),
            )
        }
        (_, _, Err(error), _) => {
            check = check.fail(
                before.as_str(),
                after_text,
                format!(
                    "{} {}",
                    lang.pick(
                        "还原失败（进程会保持 HIGH）:",
                        "restore failed (the process stays at HIGH):"
                    ),
                    describe(&error)
                ),
            )
        }
        (_, _, _, Err(error)) => {
            check = check.fail(
                before.as_str(),
                after_text,
                format!(
                    "{} {}",
                    lang.pick("还原后读取失败:", "read-back after restore failed:"),
                    describe(&error)
                ),
            )
        }
    }
    check.millis = started.elapsed().as_millis();
    check
}

/// 3) 亲和性往返：改掩码 → 读回 → 还原。
fn check_affinity(api: &Win32Api, pid: u32, lang: Lang) -> Check {
    let started = Instant::now();
    let mut check = Check::new(
        "affinity_round_trip",
        "亲和性掩码 → 读回 → 还原",
        "affinity mask → read back → restore",
    );
    let info = match api.get_affinity(pid) {
        Ok(info) => info,
        Err(error) => {
            check = check.fail(
                "-",
                "-",
                format!(
                    "{} {}",
                    lang.pick("读取自身亲和性失败:", "cannot read the own affinity:"),
                    describe(&error)
                ),
            );
            check.millis = started.elapsed().as_millis();
            return check;
        }
    };
    let original_mask = info.process_mask;
    let original_group = info.group;
    let before_text = format!(
        "group {} mask {:#018x} ({} of {} logical)",
        original_group,
        original_mask,
        original_mask.count_ones(),
        info.total_logical
    );

    // 目标掩码：优先"去掉一个最高位"（严格子集，绝不扩大进程可用的核）；
    // 只有一个核时改成"补上系统掩码里最低的一个未用位"，同样不越界。
    let target_mask = if original_mask.count_ones() > 1 {
        let highest = 63 - original_mask.leading_zeros();
        original_mask & !(1u64 << highest)
    } else {
        let spare = info.system_mask & !original_mask;
        if spare == 0 {
            original_mask
        } else {
            original_mask | (1u64 << spare.trailing_zeros())
        }
    };

    let single = |mask: u64| -> Result<AffinityPlan, HalError> {
        AffinityPlan::from_requests(
            info.total_logical,
            vec![AffinityRequest::new(original_group, mask)?],
        )
    };
    let plan = match single(target_mask) {
        Ok(plan) => plan,
        Err(error) => {
            check = check.fail(
                before_text,
                "-",
                format!(
                    "{} {}",
                    lang.pick("构造亲和性计划失败:", "cannot build the affinity plan:"),
                    describe(&error)
                ),
            );
            check.millis = started.elapsed().as_millis();
            return check;
        }
    };
    let restore_plan = match single(original_mask) {
        Ok(plan) => plan,
        Err(error) => {
            check = check.fail(
                before_text,
                "-",
                format!(
                    "{} {}",
                    lang.pick("构造还原计划失败:", "cannot build the restore plan:"),
                    describe(&error)
                ),
            );
            check.millis = started.elapsed().as_millis();
            return check;
        }
    };

    let mut problems: Vec<String> = Vec::new();

    if let Err(error) = api.set_affinity(pid, &plan) {
        problems.push(format!("set_affinity failed: {}", describe(&error)));
    }
    let read_back = api.get_affinity(pid);
    match &read_back {
        Ok(now) => {
            if now.group != original_group || now.process_mask != target_mask {
                problems.push(format!(
                    "read-back group {} mask {:#018x} != target group {} mask {target_mask:#018x}",
                    now.group, now.process_mask, original_group
                ));
            }
        }
        Err(error) => problems.push(format!("read-back failed: {}", describe(error))),
    }

    if let Err(error) = api.set_affinity(pid, &restore_plan) {
        problems.push(format!("restore failed: {}", describe(&error)));
    }
    let restored_read_back = api.get_affinity(pid);
    match &restored_read_back {
        Ok(now) => {
            if now.process_mask != original_mask || now.group != original_group {
                problems.push(format!(
                    "the restored mask {:#018x} (group {}) != the original {original_mask:#018x} (group {original_group})",
                    now.process_mask, now.group
                ));
            }
        }
        Err(error) => problems.push(format!(
            "read-back after restore failed: {}",
            describe(error)
        )),
    }

    let after_text = format!(
        "target {target_mask:#018x} -> read back {:#018x} -> restored {:#018x}",
        read_back
            .as_ref()
            .map(|info| info.process_mask)
            .unwrap_or(0),
        restored_read_back
            .as_ref()
            .map(|info| info.process_mask)
            .unwrap_or(0)
    );

    check = if problems.is_empty() {
        check.pass(before_text, after_text)
    } else {
        check.fail(before_text, after_text, problems.join("; "))
    };
    let _ = lang;
    check.millis = started.elapsed().as_millis();
    check
}

/// 4) 工作集往返：设上下限 → 读回（HAL 的 `get_working_set`）→ 还原 → 读回。
///
/// 读回**只经 `SystemApi`**：官方 `GetProcessWorkingSetSize` 现在由 `gopt-hal` 暴露，
/// 所以本 crate 里既没有 `unsafe`、也没有 `windows` 依赖。
/// 比较用的是**精确相等**（不是容忍区间）：写入 64/128 MiB 后系统就报这两个数，
/// 还原原值后也必须逐字节回到原值——任何"差不多"都说明读路径在骗人。
fn check_working_set(api: &Win32Api, pid: u32, lang: Lang) -> Check {
    let started = Instant::now();
    let mut check = Check::new(
        "working_set_round_trip",
        "工作集设上下限 → 读回 → 还原",
        "working set limits → read back → restore",
    );

    let before = match api.get_working_set(pid) {
        Ok(limits) => limits,
        Err(error) => {
            check = check.fail(
                "-",
                "-",
                format!("get_working_set failed: {}", describe(&error)),
            );
            check.millis = started.elapsed().as_millis();
            return check;
        }
    };
    let before_text = format!("min {} / max {} bytes", before.min_bytes, before.max_bytes);

    // 系统报告 min=0：HAL 写路径不接受 min=0，这个前值无法精确写回 ⇒ 明确报 SKIP 并说明。
    if !before.is_restorable() {
        check = check.skip(format!(
            "{} min 0 / max {} bytes",
            lang.pick(
                "系统报告的最小工作集为 0，HAL 写路径不接受 min=0，无法精确还原：",
                "the system reports a zero minimum working set and the HAL write path rejects min=0, \
                 so it cannot be restored exactly:"
            ),
            before.max_bytes
        ));
        check.millis = started.elapsed().as_millis();
        return check;
    }

    // 写入一个明确的窗口（64 MiB / 128 MiB），随后读回核对。
    let target = match WorkingSetLimits::from_mb(64, 128) {
        Ok(limits) => limits,
        Err(error) => {
            check = check.fail(
                before_text,
                "-",
                format!("cannot build limits: {}", describe(&error)),
            );
            check.millis = started.elapsed().as_millis();
            return check;
        }
    };
    let set = api.set_working_set(pid, target);
    let read_back = api.get_working_set(pid);
    let restored = api.set_working_set(pid, before);
    let restored_read_back = api.get_working_set(pid);

    let mut problems: Vec<String> = Vec::new();
    if let Err(error) = &set {
        problems.push(format!("set_working_set failed: {}", describe(error)));
    }
    match &read_back {
        Ok(limits) => {
            if *limits != target {
                problems.push(format!(
                    "read-back min {} / max {} does not match the requested min {} / max {}",
                    limits.min_bytes, limits.max_bytes, target.min_bytes, target.max_bytes
                ));
            }
        }
        Err(error) => problems.push(format!("read-back failed: {}", describe(error))),
    }
    if let Err(error) = &restored {
        problems.push(format!("restore failed: {}", describe(error)));
    }
    match &restored_read_back {
        Ok(limits) => {
            if *limits != before {
                problems.push(format!(
                    "the restored limits min {} / max {} do not match the original min {} / max {}",
                    limits.min_bytes, limits.max_bytes, before.min_bytes, before.max_bytes
                ));
            }
        }
        Err(error) => problems.push(format!(
            "read-back after restore failed: {}",
            describe(error)
        )),
    }

    let after_text = format!(
        "set {} MiB/{} MiB -> read back {} / {} bytes -> restored {} / {} bytes",
        target.min_bytes / (1024 * 1024),
        target.max_bytes / (1024 * 1024),
        read_back.as_ref().map_or(0, |limits| limits.min_bytes),
        read_back.as_ref().map_or(0, |limits| limits.max_bytes),
        restored_read_back
            .as_ref()
            .map_or(0, |limits| limits.min_bytes),
        restored_read_back
            .as_ref()
            .map_or(0, |limits| limits.max_bytes),
    );

    check = if problems.is_empty() {
        check.pass(before_text, after_text).note(format!(
            "{} {}",
            lang.pick(
                "读回用 HAL 的 SystemApi::get_working_set（官方 GetProcessWorkingSetSize），逐字节精确比对:",
                "read-back uses the HAL's SystemApi::get_working_set (official GetProcessWorkingSetSize), compared byte for byte:"
            ),
            describe_limits(restored_read_back.as_ref().ok())
        ))
    } else {
        check.fail(before_text, after_text, problems.join("; "))
    };
    check.millis = started.elapsed().as_millis();
    check
}

fn describe_limits(limits: Option<&WorkingSetLimits>) -> String {
    match limits {
        Some(limits) => format!("{} / {} bytes", limits.min_bytes, limits.max_bytes),
        None => "-".to_string(),
    }
}

/// 5) 电源方案查询（只读）。
fn check_power_scheme(api: &Win32Api, lang: Lang) -> Check {
    let started = Instant::now();
    let mut check = Check::new(
        "power_scheme_query",
        "电源方案查询（只读）",
        "power scheme query (read-only)",
    );
    match api.query_power_scheme() {
        Ok(scheme) => {
            check = check
                .pass("(read-only)", format!("{} ({})", scheme.name, scheme.guid))
                .note(format!(
                    "{} {}",
                    lang.pick("是否高性能方案:", "is high performance:"),
                    if scheme.is_high_performance {
                        "yes"
                    } else {
                        "no"
                    }
                ));
            // 只读项：确认查询不改变活动方案。
            if let Ok(again) = api.query_power_scheme() {
                if again.guid != scheme.guid {
                    check = check.fail(
                        "read-only",
                        format!("{} ({})", scheme.name, scheme.guid),
                        "two consecutive queries returned different schemes".to_string(),
                    );
                }
            }
        }
        Err(error) => {
            check = check.fail(
                "(read-only)",
                "-",
                format!("query failed: {}", describe(&error)),
            )
        }
    }
    let _ = lang;
    check.millis = started.elapsed().as_millis();
    check
}

/// 6) 启动项读取（只读）。
fn check_run_entries(api: &Win32Api, lang: Lang) -> Check {
    let started = Instant::now();
    let mut check = Check::new(
        "run_entries_read",
        "开机启动项读取（只读）",
        "startup entries read (read-only)",
    );
    match api.list_run_entries() {
        Ok(entries) => {
            let enabled = entries.iter().filter(|entry| entry.enabled).count();
            let sample: Vec<String> = entries
                .iter()
                .take(3)
                .map(|entry| format!("{}:{}", entry.hive, entry.name))
                .collect();
            check = check
                .pass(
                    "(read-only)",
                    format!("{} entries ({enabled} enabled)", entries.len()),
                )
                .note(format!(
                    "{} {}",
                    lang.pick("示例:", "sample:"),
                    if sample.is_empty() {
                        "-".to_string()
                    } else {
                        sample.join(", ")
                    }
                ));
            // 展示名唯一性（禁用前缀迁移依赖它）：同根同显示名不应重复。
            let mut names: Vec<(RunHive, &str)> = entries
                .iter()
                .map(|entry| (entry.hive, entry.name.as_str()))
                .collect();
            names.sort_unstable();
            let before_len = names.len();
            names.dedup();
            if names.len() != before_len {
                check = check.fail(
                    "(read-only)",
                    format!("{before_len} entries"),
                    "duplicate display names in the same hive".to_string(),
                );
            }
        }
        Err(error) => {
            check = check.fail(
                "(read-only)",
                "-",
                format!("list failed: {}", describe(&error)),
            )
        }
    }
    let _ = lang;
    check.millis = started.elapsed().as_millis();
    check
}

/// 7) 审计链往返：建链 → 校验 → 篡改检出 → 还原（全程在 %TEMP% 下的临时目录）。
fn check_journal(lang: Lang, keep_temp: bool) -> Check {
    let started = Instant::now();
    let mut check = Check::new(
        "journal_round_trip",
        "审计链建链 → 校验 → 篡改检出 → 还原",
        "audit chain build → verify → tamper detection → restore",
    );
    let dir = std::env::temp_dir().join(format!("gopt-verify-journal-{}", std::process::id()));
    if let Err(error) = std::fs::create_dir_all(&dir) {
        check = check.fail(
            "-",
            "-",
            format!("cannot create {}: {error}", dir.display()),
        );
        check.millis = started.elapsed().as_millis();
        return check;
    }
    let path = dir.join("journal.jsonl");
    let _ = std::fs::remove_file(&path);

    let mut problems: Vec<String> = Vec::new();
    let mut steps: Vec<String> = Vec::new();

    // 1) 建链：两条 apply 记录（优先级 + 亲和性）。
    let mut journal = match Journal::open_with(&path, JournalOptions::new()) {
        Ok(journal) => journal,
        Err(error) => {
            check = check.fail("-", "-", format!("cannot open {}: {error}", path.display()));
            check.millis = started.elapsed().as_millis();
            return check;
        }
    };
    let affinity = AffinityPlan::reserve_last_n_cores(16, 4);
    let drafts = [
        JournalDraft::now(JournalKind::Apply, payload::pid_target(std::process::id()))
            .with_before(payload::priority(
                std::process::id(),
                Some("gopt-verify.exe"),
                PriorityClass::Normal,
            ))
            .with_after(payload::priority(
                std::process::id(),
                Some("gopt-verify.exe"),
                PriorityClass::High,
            ))
            .with_rule_id("verify:priority"),
        JournalDraft::now(JournalKind::Apply, payload::pid_target(std::process::id()))
            .with_before(match &affinity {
                Ok(plan) => payload::affinity(std::process::id(), Some("gopt-verify.exe"), plan),
                Err(_) => serde_json::json!({"pid": std::process::id()}),
            })
            .with_after(match &affinity {
                Ok(plan) => payload::affinity(std::process::id(), Some("gopt-verify.exe"), plan),
                Err(_) => serde_json::json!({"pid": std::process::id()}),
            })
            .with_rule_id("verify:affinity"),
    ];
    let appended = journal.append_all(&drafts);
    match &appended {
        Ok(records) => steps.push(format!("appended {} records", records.len())),
        Err(error) => problems.push(format!("append failed: {error}")),
    }

    // 2) 校验。
    let verified = journal.verify_chain();
    if verified.is_ok() {
        steps.push(format!("{} records verified", verified.verified));
    } else {
        problems.push(format!(
            "the fresh chain does not verify: {}",
            verified.summary()
        ));
    }

    // 3) 回滚计划（逆序、可执行）。
    match journal.plan_rollback_all() {
        Ok(plan) if plan.len() == 2 && plan.actionable() == 2 => {
            steps.push(format!("rollback plan: {}", plan.summary()));
        }
        Ok(plan) => problems.push(format!(
            "the rollback plan has {} step(s) ({} actionable), expected 2 of 2",
            plan.len(),
            plan.actionable()
        )),
        Err(error) => problems.push(format!("plan_rollback failed: {error}")),
    }
    drop(journal);

    // 4) 篡改检出：改一个字段、保留原哈希。
    let original = std::fs::read_to_string(&path).unwrap_or_default();
    let tampered = original.replacen("\"priority\":\"high\"", "\"priority\":\"idle\"", 1);
    if tampered != original {
        if std::fs::write(&path, &tampered).is_ok() {
            if let Ok(reopened) = Journal::open_with(&path, JournalOptions::read_only()) {
                let report = reopened.verify_chain();
                if !report.is_ok() {
                    steps.push(format!(
                        "tampering detected: {}",
                        report
                            .first()
                            .map(gopt_journal::ChainBreak::summary)
                            .unwrap_or_default()
                    ));
                } else {
                    problems.push("tampering was NOT detected".to_string());
                }
            }
        }
        // 5) 还原原始字节，链应当重新通过。
        if std::fs::write(&path, &original).is_ok() {
            if let Ok(reopened) = Journal::open_with(&path, JournalOptions::read_only()) {
                if reopened.verify_chain().is_ok() {
                    steps.push("restored bytes verify again".to_string());
                } else {
                    problems.push("the restored file does not verify".to_string());
                }
            }
        }
    } else {
        problems.push("the tamper step could not find the payload to modify".to_string());
    }

    // 6) 锚点：命中位置的哈希应为 last_hash。
    if let Ok(reopened) = Journal::open_with(&path, JournalOptions::read_only()) {
        let anchor = reopened.anchor();
        let report = reopened.verify_chain_with_anchor(&anchor);
        if report.is_ok() {
            steps.push(format!("anchor ok (len {})", anchor.len));
        } else {
            problems.push(format!("the anchor does not match: {}", report.summary()));
        }
        let _ = GENESIS_HASH;
    }

    if !keep_temp {
        let _ = std::fs::remove_dir_all(&dir);
    }

    check = if problems.is_empty() {
        check.pass(
            format!(
                "{} ({})",
                path.display(),
                if keep_temp { "kept" } else { "temp" }
            ),
            steps.join(" | "),
        )
    } else {
        check.fail(
            path.display().to_string(),
            steps.join(" | "),
            problems.join("; "),
        )
    };
    let _ = lang;
    check.millis = started.elapsed().as_millis();
    check
}

/// HAL 错误 → 一行诊断（含原始 Win32 错误码）。
fn describe(error: &HalError) -> String {
    match error.win32_code() {
        Some(code) => format!(
            "{} [{}] win32={code}: {}",
            error.operation(),
            error.kind(),
            error.message()
        ),
        None => format!(
            "{} [{}]: {}",
            error.operation(),
            error.kind(),
            error.message()
        ),
    }
}

/// 未使用但保留的类型引用（`Guid` / `PowerSchemeSelector` 在扩展检查项时会用到）。
#[allow(dead_code)]
fn _type_anchors() -> (&'static str, Guid, PowerSchemeSelector, SystemTime) {
    (
        RunEntry::DISABLED_PREFIX,
        Guid::HIGH_PERFORMANCE,
        PowerSchemeSelector::HighPerformance,
        UNIX_EPOCH,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_language_and_json() {
        let options =
            parse(&["--json".to_string(), "--lang".to_string(), "en".to_string()]).expect("parse");
        assert!(options.json);
        assert_eq!(options.lang, Lang::En);
        assert!(!options.keep_temp);

        let options = parse(&["--lang=zh".to_string()]).expect("parse");
        assert_eq!(options.lang, Lang::Zh);

        assert!(parse(&["--nope".to_string()]).is_err());
        assert!(parse(&["--lang".to_string()]).is_err());
        assert_eq!(parse(&["--help".to_string()]).expect_err("help"), "help");
    }

    #[test]
    fn descriptions_are_bilingual() {
        assert_eq!(Lang::Zh.pick("中文", "english"), "中文");
        assert_eq!(Lang::En.pick("中文", "english"), "english");
        assert_eq!(Lang::parse("EN"), Some(Lang::En));
        assert_eq!(Lang::parse("de"), None);
        assert!(usage(Lang::Zh).contains("RESULT"));
        assert!(usage(Lang::En).contains("usage"));
    }

    #[test]
    fn check_json_shape_is_stable() {
        let check = Check::new("unit", "单元", "unit").pass("a", "b");
        let json = check.json();
        assert_eq!(json["id"], "unit");
        assert_eq!(json["status"], "PASS");
        assert_eq!(json["before"], "a");
        assert_eq!(check.title(Lang::Zh), "单元");
    }
}
