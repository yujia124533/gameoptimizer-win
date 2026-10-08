//! GameOptimizer-RS 审计日志（`gopt-journal`）：**SHA-256 哈希链** + **事件溯源回滚** +
//! **旧格式兼容导入**。
//!
//! 这是"可回滚 / 可解释"从口号变成可验证工程属性的地方：每一次修改都先写一行"写入前状态"，
//! 每一行都与前一行用哈希链绑定；回滚不是"反向执行一堆临时变量"，而是从日志里**重新推导**
//! 出来的逆序计划。
//!
//! # 记录 schema（`journal.jsonl`，每行一个对象）
//!
//! ```text
//! {"id":1,"ts_unix_ms":1735689600123,"kind":"apply","target":"pid:4242",
//!  "before":{...}|null,"after":{...}|null,"rule_id":"policy:game/cs2"|null,
//!  "prev_hash":"…64 hex…","hash":"…64 hex…"}
//! ```
//!
//! `before` / `after` 的字段约定见 [`payload`]；`kind ∈ {apply, rollback, imported}`。
//!
//! # 哈希链（我们的"规范化"方案）
//!
//! ```text
//! hash = SHA256( canonical(record 去掉 hash 字段) ‖ prev_hash )
//! ```
//!
//! * `canonical` 是**自己实现**的规范 JSON 写入器（[`canonical`]）：对象键按 UTF-8 字节序排序、
//!   无空白、控制字符统一写成 `\u00xx`、非 ASCII 原样输出。字段序与键序都由本 crate 写死，
//!   **不依赖 `serde` 的字段序，也不依赖任何 map 的迭代顺序**。
//! * `‖ prev_hash` 是把定长 64 字节的十六进制文本直接拼在规范正文之后——
//!   于是"位置"同时被编码进字段与哈希输入，记录无法被搬到链上的另一处。
//! * 写盘的行 == 规范形式 + 一个 `\n`；读取时重算规范形式做**逐字节**比对，
//!   因此"把行重新格式化、加键、改键序"也会被检出。
//!
//! # 校验：[`verify_chain`] 报出**第一处**不一致
//!
//! 校验顺序：行能否解析 → `id` 连续 → 链首创世哈希 → `prev_hash` 链接 → 记录自哈希 →
//! 行字节==规范形式。第一处失败以 [`ChainBreak`]（`id` + [`ChainProblem`] + 数值证据）返回。
//!
//! **能力边界**：单靠文件自身，"尾部整行被删除"不可检出（剩下的前缀仍然自洽）。
//! 因此提供 [`ChainAnchor`]（`len` + 该位置哈希）作为外部锚点：
//! [`Journal::anchor`] 生成、[`Journal::verify_chain_with_anchor`] 校验。锚点要存到日志之外的
//! 地方（配置、CI 产物、另一台机器）才有意义。
//!
//! # 崩溃安全
//!
//! 追加 = 一次 `write_all` + `flush` + `fsync`（`FlushFileBuffers`）；
//! 打开时把"最后一个换行符之后的残余字节"截掉（[`JournalOptions::repair_torn_tail`]，
//! 默认开启，见 [`Journal::tail_repair`]）。只想读不想改的调用方用
//! [`JournalOptions::read_only`]，此时尾部残行保留成 `terminated = false` 的行，
//! 校验会明确报出，且**拒绝追加**。日志被外部改动过（长度变化）时追加返回
//! [`JournalErrorKind::Stale`]，要求重新打开。
//!
//! # 旧格式兼容（只读）
//!
//! [`import_legacy`] / [`import_legacy_default`] 只读解析 C++ 版 v1.1.0 的
//! `savepoints.txt`（`SecurityRollback::Serialize` 的 12 字段格式）与 `games.conf`，
//! 转成 `kind = imported` 的草稿：坏行跳过并计数、不 panic、不修改旧文件。
//! 旧文件里的 `REALTIME_PRIORITY_CLASS (0x100)` 会被红线拦下，只在记录里留
//! `priority_raw_blocked` 痕迹，绝不进入可回滚字段。
//!
//! # 红线（本 crate 的职责边界）
//!
//! * **不做任何系统调用**：回滚计划是纯数据（[`RollbackPlan`]），执行交给 `gopt-core` 经
//!   `gopt-hal::SystemApi` 落地。
//! * **不 panic**：crate 内 `deny(clippy::unwrap_used / expect_used / panic / todo / unimplemented)`
//!   （仅测试豁免），`forbid(unsafe_code)`。
//! * **依赖面只有 4 个**：`gopt-hal` + `serde` + `serde_json` + `sha2`，无 async、无第三方日志框架。
//! * **只读旧文件**：导入路径只 `fs::read` / `fs::metadata`，不创建目录、不改 mtime。
//!
//! # 最小用法
//!
//! ```
//! use gopt_hal::PriorityClass;
//! use gopt_journal::{payload, rollback, verify_chain, JournalDraft, JournalKind, GENESIS_HASH};
//!
//! let draft = JournalDraft::now(JournalKind::Apply, payload::pid_target(4242))
//!     .with_before(payload::priority(4242, Some("cs2.exe"), PriorityClass::Normal))
//!     .with_after(payload::priority(4242, Some("cs2.exe"), PriorityClass::High));
//! let record = draft.into_record(1, GENESIS_HASH);
//!
//! let report = verify_chain(std::slice::from_ref(&record));
//! assert!(report.is_ok());
//! assert_eq!(report.verified, 1);
//!
//! // 回滚计划：逆序、结构化、不含系统调用。
//! let plan = rollback::plan_rollback(std::slice::from_ref(&record), 1)?;
//! assert_eq!(plan.len(), 1);
//! assert!(plan.summary().contains("1 steps"));
//! # Ok::<(), gopt_journal::JournalError>(())
//! ```
//!
//! 真实用法（写盘）见 `README.md` 与 `tests/journal_contract.rs`：
//! `Journal::open(path)` → `append(draft)` → `verify_chain()` → `plan_rollback(to_id)`。

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

pub mod canonical;
pub mod chain;
pub mod error;
pub mod legacy;
pub mod payload;
pub mod record;
pub mod rollback;
pub mod store;

pub use canonical::{canonical_json, chain_hash, is_well_formed_hash, sha256_hex, GENESIS_HASH};
pub use chain::{
    verify_chain, verify_chain_with_anchor, verify_lines, ChainAnchor, ChainBreak, ChainProblem,
    ChainReport, JournalLine,
};
pub use error::{JournalError, JournalErrorKind, JournalResult};
pub use legacy::{
    import_legacy, import_legacy_default, legacy_paths, parse_game_conf_line, parse_savepoint_line,
    LegacyGameConfig, LegacyImport, LegacySavepoint, SourceReport, GAMES_CONF_FILE_NAME,
    LEGACY_GAMES_CONF_RULE_ID, LEGACY_SAVEPOINTS_RULE_ID, SAVEPOINTS_FILE_NAME,
};
pub use record::{now_unix_ms, JournalDraft, JournalKind, JournalRecord};
pub use rollback::{
    plan_rollback, plan_rollback_all, plan_rollback_pending, undone_apply_ids, RollbackAction,
    RollbackPlan, RollbackStep, ROLLBACK_RULE_PREFIX,
};
pub use store::{Journal, JournalOptions, TailRepair, DATA_DIR_NAME, JOURNAL_FILE_NAME};
