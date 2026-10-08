//! GameOptimizer-RS **编排内核**：`detect → load policy → plan（默认只读）→ apply（写审计日志）→ rollback`。
//!
//! 本 crate 是"单内核、多前端"里的内核：CLI（`gopt-cli`）与真机自检（`gopt-verify`）都只是
//! 前端，它们不自己拼 HAL 调用、不自己写审计日志——所有状态修改都必须经过 [`Session`]，
//! 于是"改了什么、为什么改、怎么撤销"在任何前端上都只有一份实现。
//!
//! # 数据流
//!
//! ```text
//!                       ┌──────────────── gopt-policy（TOML 声明式策略）
//! detect ──► EvalInput ─┤
//!   │                   └──► Plan（有序、可解释：rule_id/行号/中英双语理由/降级说明）
//!   │
//!   └──► apply ──► gopt-hal::SystemApi（唯一被允许触碰系统的入口）
//!            │
//!            └──► gopt-journal（每条修改写一行"写入前状态"，SHA-256 哈希链绑定）
//!                     │
//!                     └──► rollback：从日志**重新推导**逆序撤销计划，再经 HAL 落地
//! ```
//!
//! # 默认安全
//!
//! * **plan 默认只读**：`Session::plan` 只读系统状态，不改任何东西；
//! * **apply 需要显式确认**：[`ApplyOptions::dry_run`] 为 `true` 时只读取当前值、不写、不落日志，
//!   CLI 只有在看到 `--yes` 时才把它设为 `false`；
//! * **fail-closed**：真正执行前先打开审计日志并校验哈希链，链不可信 ⇒ 拒绝修改（[`CoreErrorKind::AuditChainBroken`]）；
//!   执行过程中如果日志写不进去（链坏、文件被外部改写），立即停手并把已发生的修改如实报出
//!   （绝不"改了却没记"）。
//!
//! # 可解释
//!
//! 每个步骤都携带策略里的 `rule_id`、来源行号、中英双语理由；没有执行的规则进 `Plan::skipped`
//! 并说明原因。执行结果 [`AppliedStep`] 记录每一步的**前后值**（`before` / `after` JSON 载荷，
//! 与审计日志同一套 schema）与是否真的改变了状态（已是目标值 ⇒ 不写、不记、如实说明）。
//!
//! # 可回滚
//!
//! [`Session::rollback`] 不靠内存里的临时变量：它从审计日志生成逆序计划，把
//! [`gopt_journal::RollbackAction`] 逐条翻成 HAL 调用，并为每条执行过的撤销动作追加一条
//! `kind = rollback`、`rule_id = "rollback:<apply_id>"` 的记录。
//!
//! **能力边界（诚实说明）**：`gopt-hal` 的 [`SystemApi`] 有 14 个方法，优先级 / 亲和性 /
//! 工作集三条都有官方读路径（工作集用 `get_working_set`，底层是官方
//! `GetProcessWorkingSetSize`），所以这三类步骤的 `before` 是**真值**、可回滚。
//! 只有在"读不到前值"（进程受保护 / 已退出）或"前值写不回去"（系统报告的最小工作集为 0，
//! HAL 写路径不接受 `min = 0`）时，审计记录才会是 `before = null`，回滚计划会把它报成
//! `NotActionable` 并说明原因，而不是假装能还原。详见 `README.md` 的"能力边界"一节。
//!
//! # 红线
//!
//! * `forbid(unsafe_code)`：本 crate 不碰系统，系统调用全在 `gopt-hal` 里；
//! * 不以 panic 作为错误路径：crate 内 `deny(clippy::unwrap_used / expect_used / panic / todo / unimplemented)`
//!   （仅测试豁免），所有失败都返回 [`CoreError`]；
//! * 优先级上限仍是 HIGH：内核拿到的目标类型是 `gopt_hal::PriorityClass`，REALTIME 无法表达。
//!
//! # 最小用法
//!
//! ```
//! use std::sync::Arc;
//!
//! use gopt_core::{ApplyOptions, DataPaths, Lang, Session};
//! use gopt_hal::MockApi;
//! use tempdir::TempDir;
//!
//! # mod tempdir {
//! #     pub struct TempDir(std::path::PathBuf);
//! #     impl TempDir {
//! #         pub fn new(name: &str) -> Self {
//! #             let dir = std::env::temp_dir().join(name);
//! #             std::fs::create_dir_all(&dir).expect("temp dir");
//! #             Self(dir)
//! #         }
//! #         pub fn path(&self) -> &std::path::Path { &self.0 }
//! #     }
//! #     impl Drop for TempDir {
//! #         fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
//! #     }
//! # }
//! let api = Arc::new(MockApi::sample_workstation());
//! let dir = TempDir::new("gopt-core-doc");
//! let session = Session::new(api, DataPaths::new(dir.path()), Lang::Zh)?;
//!
//! // 1) 只读计划：命中 cs2.exe（pid 1234），三步动作都带理由与来源行号
//! let report = session.plan("cs2", None)?;
//! assert_eq!(report.plan.game_id, "cs2");
//! assert!(report.plan.step_count() >= 3);
//!
//! // 2) 真实执行：写系统 + 写审计日志
//! let applied = session.apply(&report.plan, ApplyOptions::commit())?;
//! assert!(applied.failed == 0);
//! assert_eq!(applied.journal_ids.len(), applied.applied);
//!
//! // 3) 回滚：从日志逆序还原
//! let rolled = session.rollback(gopt_core::RollbackOptions::all())?;
//! assert!(rolled.executed >= 1);
//! # Ok::<(), gopt_core::CoreError>(())
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

pub mod apply;
pub mod engine;
pub mod error;
pub mod exec;
pub mod i18n;
pub mod inspect;
pub mod model;
pub mod outcome;
pub mod paths;
pub mod report;

/// `--json` 输出的 schema 版本（前端/脚本据此判断字段是否兼容）。
pub const SCHEMA_VERSION: u32 = 1;

/// 产品名（`gopt --json` 的 `product` 字段）。
pub const PRODUCT_NAME: &str = "GameOptimizer-RS";

/// 本构建的版本（取自 `Cargo.toml`）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 仍在提供正式发布的 C++ 版版本号（Phase 1 期间它是发布版，Rust 版是并行验证版）。
pub const CPP_RELEASE_VERSION: &str = "1.1.0";

/// `gopt --version` 的单行文本（中英各一行由前端选择，这里给稳定的英文行）。
pub fn version_line() -> String {
    format!(
        "gopt {VERSION} ({PRODUCT_NAME} Phase 1) — C++ v{CPP_RELEASE_VERSION} remains the official release"
    )
}

pub use engine::Session;
pub use error::{CoreError, CoreErrorKind, CoreResult};
pub use i18n::{pick, Lang, Text};
pub use model::{
    AppliedStep, ApplyOptions, ApplyReport, ExplainReport, GameSummary, ImportReport,
    JournalEntryView, JournalFilter, JournalView, LegacyImportOptions, PlanReport, PrioReport,
    RollbackOptions, RollbackReport, RollbackStepReport, SourceSummary, StartupReport,
    StatusReport, StepStatus, TuneReport, VerifyOptions, VerifyReport, VerifyStatus, WatchOptions,
    WatchState, WatchTick, WatchTickItem,
};
pub use outcome::{Notice, NoticeLevel, Outcome, EXIT_AUDIT, EXIT_ENV, EXIT_OK, EXIT_USAGE};
pub use paths::DataPaths;

// 前端（CLI / 将来的 GUI）只需要依赖 gopt-core：这里把三个底层 crate 的公开类型重新导出，
// 保证"单内核多前端"不会退化成"每个前端各自拼一遍底层 API"。
pub use gopt_hal::{
    AffinityApplied, AffinityInfo, AffinityPlan, AffinityRequest, Guid, HalError, HalErrorKind,
    HalOp, HardwareInfo, MockApi, PowerScheme, PowerSchemeChange, PowerSchemeSelector,
    PriorityClass, ProcessInfo, RunEntry, RunHive, SystemApi, WorkingSetLimits,
};
pub use gopt_journal::{
    plan_rollback_pending, undone_apply_ids, ChainAnchor, ChainBreak, ChainProblem, ChainReport,
    Journal, JournalDraft, JournalError, JournalErrorKind, JournalKind, JournalOptions,
    JournalRecord, LegacyImport, RollbackAction, RollbackPlan, RollbackStep, SourceReport,
    TailRepair,
};
pub use gopt_policy::{
    AffinitySpec, DiagnosticSeverity, EvalInput, GamePolicy, Plan, PlanAction, PlanSkip, PlanStep,
    PolicyDiagnostic, PolicyLayer, PolicyLoadOutcome, PolicyLoader, PolicyOrigin, PolicySet,
    PowerSchemeChoice, Reason, Rule, SkipCause,
};

#[cfg(windows)]
pub use gopt_hal::Win32Api;
