//! GameOptimizer-RS 声明式策略引擎：**加游戏 / 改规则不需要重新编译**。
//!
//! 本 crate 是"可解释、可回滚、只用稳定官方 API"三条红线的数据层：
//!
//! * **策略是数据**：内置 8 款游戏预设（与 C++ 版 v1.1.0 的 `GameId` 一一对应）与用户新增/覆盖
//!   的游戏都来自 TOML，解析、校验、求值全在运行期完成；
//! * **可解释**：求值产物是 [`Plan`]——每一步都有 `rule_id`、来源行号、中英双语理由，
//!   以及"需要管理员 / 属于危险动作"标记；没执行的规则进 `Plan::skipped` 并说明原因
//!   （条件不满足 / 硬件降级 / 策略显式跳过），没有"静默跳过"；
//! * **可回滚**：策略只描述动作，执行统一走 `gopt-hal` 的 [`gopt_hal::SystemApi`]，
//!   每个写入方法都返回写入前的值（见 HAL 契约），因此 Plan 的每一步天然带回滚依据；
//! * **只用稳定官方 API**：本 crate **`forbid(unsafe_code)`**，不碰系统；需要触碰系统的
//!   动作全部落在 HAL 已经收窄的 14 个方法里（含只读的 `get_working_set`）。
//!
//! # TOML schema
//!
//! ```toml
//! schema = 1                                  # 可选；本构建只认 1
//!
//! [[game]]
//! id = "cs2"                                  # kebab-case，全局唯一；用户层同 id ⇒ 覆盖内置
//! name_zh = "CS2"                             # 中英双语名都是必填（双语红线）
//! name_en = "Counter-Strike 2"
//! match = "cs2.exe"                           # 必填：exe 名通配（* 与 ?，大小写不敏感，不接受路径）
//! exe_aliases = ["csgo.exe"]                  # 可选：同一款游戏的其它可执行文件
//! name_aliases = ["反恐精英2"]                 # 可选：搜索/展示别名
//! description_zh = "..."                      # 可选
//! description_en = "..."
//!
//! [[game.rules]]                              # 规则按声明顺序求值；id 缺省时按 `游戏id.动作` 推导
//! id = "working-set"                          # 可选但推荐：进 Plan 与审计日志的稳定标识
//! when = { ram_gb = { gte = 8, lt = 16 } }     # 可选：缺省 = 无条件；多个字段之间是 AND
//! action = { working_set = { min_mb = 128 } }  # 必填：恰好一种动作
//! ```
//!
//! 动作（`action = { ... }`，六选一）：
//!
//! | 动作 | 写法 | 落到 HAL |
//! |---|---|---|
//! | `priority` | `{ class = "high" }`（`idle` / `below-normal` / `normal` / `above-normal` / `high`） | `set_priority` |
//! | `affinity` | `{ reserve_cores = 1, reserve_from = "first", physical_only = true }` 或 `{ mask = "0xffff" }` | `set_affinity` |
//! | `working_set` | `{ min_mb = 256, max_mb = 0 }`（`max_mb = 0`/缺省 = 无上限） | `set_working_set` |
//! | `power_scheme` | `{ scheme = "high" }` 或 `{ scheme = "balanced" }` | `set_power_scheme` |
//! | `run_entries` | `{ hive = "hkcu", disable = ["Discord"], ignore_missing = true }` | `set_run_entry_enabled` |
//! | `skip` | `{ reason_zh = "...", reason_en = "..." }` | 不执行（写进 `Plan::skipped` 解释为什么） |
//!
//! 条件字段（`when = { ... }`）与算子：数值字段（`logical_cores` / `physical_cores` /
//! `ram_gb` / `ram_mb`）支持 `gt` `gte` `lt` `lte` `eq` `ne` 与标量简写（`ram_gb = 16` ⇒ `eq`）；
//! `gpu_vendor` 只支持 `eq`/`ne`（`nvidia` `amd` `intel` `microsoft` `unknown` `none`）；
//! `is_elevated` 只支持 `eq`/`ne` 的布尔值。同一字段的多个算子必须同时成立（区间）。
//!
//! # Plan 结构
//!
//! ```text
//! Plan { game_id, game_name_zh, game_name_en, pid, policy_origin, steps, skipped }
//! ├── steps[]:   { order, rule_id, rule_line, pid, reason{zh,en}, action, requires_elevation, is_dangerous }
//! │   └── action: Priority | Affinity{plan,spec} | WorkingSet | PowerScheme | RunEntry
//! └── skipped[]: { rule_id, rule_line, cause: condition_not_met|degraded|explicit_skip, reason{zh,en} }
//! ```
//!
//! 不变量：`order` 从 1 连续递增；`steps` 顺序 = 规则声明顺序（`run_entries` 按 hive × 名称展开）；
//! 每个 `action` 都能经 [`PlanStep::hal_op`] 一对一映射到 HAL 操作；求值不失败（只产生步骤或跳过说明）。
//!
//! # 加载与覆盖
//!
//! ```text
//! 内置层：rust/policies/*.toml          （include_str! 进二进制）
//! 用户层：%LOCALAPPDATA%\GameOptimizer\policies.d\*.toml
//!         └── 同 id 覆盖内置；新 id 新增；查找/匹配都是用户优先
//! ```
//!
//! 解析错误带上 `文件:行:列`（[`PolicyDiagnostic`]），**永不 panic**；
//! 一个文件坏掉不影响其它文件。
//!
//! # 与 C++ 版 v1.1.0 的关系
//!
//! * 8 款内置游戏的 `priority` / `affinity` / `working_set` 语义逐字段对齐
//!   `src/preset/GameOptimizationPreset.cpp`，包括"物理核 ≤ 2 不绑定""内存 < 8GB 不设工作集、
//!   8~16GB 减半"这些降级路径——区别只是它们现在是 `when` 条件而不是 C++ 代码；
//! * 两处**有意**的偏差：① 驱动级帧延迟（`gpuMaxFrames`）不实现——它需要 NVAPI/ADL 等第三方
//!   厂商库，违反"仅官方 Win32 API"红线；② 逻辑处理器 > 64（多处理器组）时不再放弃亲和性，
//!   而是给出逐组计划（HAL 的 `per_group` / 逐线程 `SetThreadGroupAffinity` 支持执行）。
//!
//! # 最小用法
//!
//! ```
//! use gopt_hal::{MockApi, SystemApi, PriorityClass};
//! use gopt_policy::{EvalInput, PolicyLoader};
//!
//! // 1) 加载策略（内置 + 用户覆盖），坏文件只会变成诊断
//! let outcome = PolicyLoader::builtin_only().load();
//! assert!(!outcome.has_errors());
//! let policies = outcome.into_set();
//!
//! // 2) 采集求值输入（硬件画像 + 提权状态），Mock 后端让测试不需要管理员
//! let api = MockApi::sample_workstation();
//! let input = EvalInput::from_api(&api)?;
//!
//! // 3) 对匹配到的进程求值，拿到有序、可解释的执行计划
//! let processes = api.list_processes()?;
//! let plans = policies.plans_for(&input, &processes);
//! let cs2 = plans.iter().find(|plan| plan.game_id == "cs2").expect("cs2 plan");
//! assert_eq!(cs2.steps[0].hal_op(), gopt_hal::HalOp::SetPriority);
//! assert_eq!(
//!     cs2.steps[0].action,
//!     gopt_policy::PlanAction::Priority { class: PriorityClass::High }
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
// 测试代码允许 unwrap/expect/panic：测试失败必须显式炸出来。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod affinity;
pub mod builtin;
pub mod condition;
pub mod error;
pub mod eval;
pub mod loader;
pub mod matching;
pub mod model;
pub mod plan;
mod raw;
mod validate;

pub use affinity::resolve_affinity;
pub use builtin::{builtin_file, BuiltinFile, BUILTIN_FILES, CPP_PARITY_FILES};
pub use condition::{
    parse_gpu_vendor, Comparison, Condition, ConditionField, ConditionTerm, ConditionValue,
    GpuVendorValue, GPU_VENDOR_VALUES,
};
pub use error::{
    DiagnosticSeverity, PolicyDiagnostic, PolicyLayer, PolicyLoadErrors, PolicyOrigin,
};
pub use eval::EvalInput;
pub use loader::{PolicyLoadOutcome, PolicyLoader, PolicySet};
pub use matching::{validate_exe_pattern, wildcard_match};
pub use model::{
    priority_label_en, priority_label_zh, Action, ActionKind, AffinitySpec, GamePolicy,
    PowerSchemeChoice, ReserveSide, Rule, RunHiveSpec,
};
pub use plan::{Plan, PlanAction, PlanSkip, PlanStep, Reason, SkipCause};
pub use validate::{parse_policy_file, SCHEMA_VERSION};
