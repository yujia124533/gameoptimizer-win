//! GameOptimizer-RS 硬件抽象层（HAL）。
//!
//! [SystemApi] 是内核与 CLI 之外**唯一**被允许触碰系统的入口：真实后端 [`Win32Api`] 只用
//! kernel32 / advapi32 / powrprof / dxgi 的公开 API，测试后端 [`MockApi`] 记录调用序列并可
//! 注入失败，两者返回同一套值类型与同一套错误分类。
//!
//! # 红线（在类型与收窄的依赖面上落地，而不是靠注释）
//!
//! * **只用官方 Win32 API**：依赖面只有 `windows` 0.58 与 `serde`；`windows` 的 feature
//!   列表就是本 crate 允许触碰的全部 API 面（见 `Cargo.toml`）。
//! * **不做注入、不做内核 Hook**、不 LoadLibrary 任何第三方 DLL。
//! * **优先级上限 HIGH**：[`PriorityClass`] 里根本没有 REALTIME 这一档，
//!   [`PriorityClass::from_raw`] 对 `0x100` 硬拒绝并返回
//!   [`HalErrorKind::PolicyDenied`]——红线由类型系统保证。
//! * **一切可回滚**：每个写入方法都返回写入前的值（[`SystemApi::set_priority`]、
//!   [`SystemApi::set_power_scheme`]）或回滚所需信息；启动项禁用是改名迁移，天然可逆。
//! * **禁止以 panic 作为错误路径**：crate 内部 `deny(clippy::unwrap_used / expect_used /
//!   panic / todo / unimplemented)`（仅测试代码豁免），所有失败都返回 [`HalError`]。
//! * **不支持就显式报错**：[`HalErrorKind::Unsupported`] 而不是静默跳过。
//!
//! # 最小用法
//!
//! ```
//! use gopt_hal::{MockApi, PriorityClass, SystemApi};
//!
//! let api = MockApi::sample_workstation();
//! // 写入方法返回"写入前的值"，可直接作为回滚依据与审计字段。
//! let previous = api.set_priority(1234, PriorityClass::High)?;
//! assert_eq!(previous, PriorityClass::Normal);
//! assert_eq!(api.priority_of(1234), Some(PriorityClass::High));
//!
//! // REALTIME 无法表达，也无法从原始值混进来。
//! assert!(PriorityClass::from_raw(0x100).is_err());
//! # Ok::<(), gopt_hal::HalError>(())
//! ```
//!
//! # 模块
//!
//! * [`api`]：[`SystemApi`] trait 与 [`HalOp`] 操作标识
//! * [`types`]：与平台无关的值类型（优先级、工作集、启动项、电源方案）
//! * [`affinity`]：处理器组模型与掩码计算（>64 逻辑核分组）
//! * [`hardware`]：硬件画像
//! * [`error`]：[`HalError`] 结构化错误
//! * [`mock`]：[`MockApi`]（无需管理员即可单测）
//!
//! 真机自检入口见 `examples/hal_selfcheck.rs` 与 `README.md`。

#![forbid(unsafe_op_in_unsafe_fn)]
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
pub mod api;
pub mod error;
pub mod hardware;
pub mod mock;
pub mod types;

#[cfg(windows)]
mod win32;

pub use affinity::{
    full_group_mask, processor_groups, reserve_last_n_cores_to_requests, AffinityApplied,
    AffinityInfo, AffinityMethod, AffinityPlan, AffinityRequest, ProcessorGroup,
    MAX_LOGICAL_PER_GROUP,
};
pub use api::{HalOp, SystemApi};
pub use error::{HalError, HalErrorKind, HalResult};
pub use hardware::{CoreLayout, GpuInfo, GpuVendor, HardwareInfo};
pub use mock::{Call, CallArgs, MockApi};
pub use types::{
    Guid, PowerScheme, PowerSchemeChange, PowerSchemeSelector, PriorityClass, ProcessInfo,
    RunEntry, RunHive, WorkingSetLimits,
};

#[cfg(windows)]
pub use win32::Win32Api;
