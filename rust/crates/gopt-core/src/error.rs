//! 结构化错误：分类 + 稳定英文诊断 + 失败操作名 + 可执行建议（中英双语）。
//!
//! 设计对齐 `gopt-hal` / `gopt-journal` 的风格，并把它们的错误**原样保留**在
//! [`CoreError::hal`] / [`CoreError::journal`] 里——CLI 因此既能打印一行可读提示，
//! 又能在 `--json` 里给出 `win32_code` / 日志行号这类可自动处理的字段。
//!
//! 退出码映射（任务约定：0 成功 / 1 用法错误 / 2 环境不满足 / 3 审计链校验失败）见
//! [`CoreError::exit_code`]。

use core::fmt;

use serde::Serialize;

use gopt_hal::{HalError, HalErrorKind};
use gopt_journal::{JournalError, JournalErrorKind};

/// 内核错误分类（机器可读，直接进 `--json`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreErrorKind {
    /// 命令行用法错误（未知命令、缺少参数、多余的参数）。
    Usage,
    /// 参数值非法（例如 `--lang de`、`--to` 不是数字）。可修正后重试。
    InvalidArgument,
    /// 目标不存在：游戏没在运行、进程已退出、启动项不存在。
    NotFound,
    /// 权限不足：未提权时切电源方案 / 写 HKLM 启动项。
    AccessDenied,
    /// 安全红线拒绝（REALTIME 优先级一类）。
    PolicyDenied,
    /// 当前系统不支持（例如跨处理器组亲和性未拆分）。
    Unsupported,
    /// 文件系统错误（日志、策略目录、报告输出）。
    Io,
    /// HAL 层的其它失败（保留原始错误与 Win32 错误码）。
    Hal,
    /// 审计日志层的其它失败（保留原始错误与行号）。
    Journal,
    /// **审计链校验失败**：日志被篡改/损坏，或尾部半行未修复。
    AuditChainBroken,
    /// 内部不变量被破坏。
    Internal,
}

impl CoreErrorKind {
    /// 稳定蛇形命名（`--json` 字段值）。
    pub const fn as_str(self) -> &'static str {
        match self {
            CoreErrorKind::Usage => "usage",
            CoreErrorKind::InvalidArgument => "invalid_argument",
            CoreErrorKind::NotFound => "not_found",
            CoreErrorKind::AccessDenied => "access_denied",
            CoreErrorKind::PolicyDenied => "policy_denied",
            CoreErrorKind::Unsupported => "unsupported",
            CoreErrorKind::Io => "io",
            CoreErrorKind::Hal => "hal",
            CoreErrorKind::Journal => "journal",
            CoreErrorKind::AuditChainBroken => "audit_chain_broken",
            CoreErrorKind::Internal => "internal",
        }
    }
}

impl fmt::Display for CoreErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 内核统一错误类型。
///
/// 两个底层错误都装箱存放：`CoreError` 出现在**所有** `Result` 的 `Err` 侧，
/// 保持它 <= 128 字节（clippy 的 `result_large_err` 阈值）比少一次指针跳转重要得多——
/// 否则每个 `CoreResult<T>` 都要携带 200 字节的错误槽。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoreError {
    kind: CoreErrorKind,
    operation: &'static str,
    message: String,
    hal: Option<Box<HalError>>,
    journal: Option<Box<JournalError>>,
}

impl CoreError {
    /// 构造（无底层错误）。
    pub fn new(kind: CoreErrorKind, operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            operation,
            message: message.into(),
            hal: None,
            journal: None,
        }
    }

    /// 命令行用法错误。
    pub fn usage(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::Usage, operation, message)
    }

    /// 参数值非法。
    pub fn invalid_argument(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::InvalidArgument, operation, message)
    }

    /// 目标不存在。
    pub fn not_found(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::NotFound, operation, message)
    }

    /// 权限不足。
    pub fn access_denied(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::AccessDenied, operation, message)
    }

    /// 安全红线拒绝。
    pub fn policy_denied(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::PolicyDenied, operation, message)
    }

    /// 系统不支持。
    pub fn unsupported(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::Unsupported, operation, message)
    }

    /// 文件系统错误。
    pub fn io(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::Io, operation, message)
    }

    /// 内部错误。
    pub fn internal(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::Internal, operation, message)
    }

    /// 由 HAL 错误构造：保留原始错误对象（含分类与 Win32 错误码）。
    pub fn from_hal(message: impl Into<String>, error: HalError) -> Self {
        let kind = match error.kind() {
            HalErrorKind::InvalidArgument => CoreErrorKind::InvalidArgument,
            HalErrorKind::Unsupported => CoreErrorKind::Unsupported,
            HalErrorKind::NotFound => CoreErrorKind::NotFound,
            HalErrorKind::AccessDenied => CoreErrorKind::AccessDenied,
            HalErrorKind::PolicyDenied => CoreErrorKind::PolicyDenied,
            HalErrorKind::Win32 | HalErrorKind::Internal => CoreErrorKind::Hal,
        };
        Self {
            kind,
            operation: error.operation(),
            message: format!("{}: {}", message.into(), error.message()),
            hal: Some(Box::new(error)),
            journal: None,
        }
    }

    /// 由审计日志错误构造：链相关失败统一归类到 [`CoreErrorKind::AuditChainBroken`]。
    pub fn from_journal(error: JournalError) -> Self {
        let kind = match error.kind() {
            JournalErrorKind::ChainBroken | JournalErrorKind::MalformedRecord => {
                CoreErrorKind::AuditChainBroken
            }
            JournalErrorKind::InvalidArgument => CoreErrorKind::InvalidArgument,
            JournalErrorKind::Io | JournalErrorKind::Stale | JournalErrorKind::LegacySource => {
                CoreErrorKind::Journal
            }
        };
        Self {
            kind,
            operation: error.operation(),
            message: error.message().to_string(),
            hal: None,
            journal: Some(Box::new(error)),
        }
    }

    /// `&str` 版 [`json`](Self::from_journal) 辅助（保持调用点紧凑）。
    pub fn journal_kind_broken(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(CoreErrorKind::AuditChainBroken, operation, message)
    }

    /// 错误分类。
    pub const fn kind(&self) -> CoreErrorKind {
        self.kind
    }

    /// 失败时正在执行的操作名（官方 API / 内部步骤名）。
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    /// 稳定英文诊断文本（可进日志、可检索）。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 底层 HAL 错误（若有）。
    pub fn hal(&self) -> Option<&HalError> {
        self.hal.as_deref()
    }

    /// 底层审计日志错误（若有）。
    pub fn journal(&self) -> Option<&JournalError> {
        self.journal.as_deref()
    }

    /// 进程退出码：0 成功 / 1 用法错误 / 2 环境不满足 / 3 审计链校验失败。
    pub const fn exit_code(&self) -> i32 {
        match self.kind {
            CoreErrorKind::Usage | CoreErrorKind::InvalidArgument => 1,
            CoreErrorKind::AuditChainBroken => 3,
            _ => 2,
        }
    }

    /// 中文可执行建议（前端在错误下方打印，替代"报错堆栈"）。
    pub fn hint_zh(&self) -> String {
        match self.kind {
            CoreErrorKind::Usage => "用法：`gopt --help` 查看全部命令；`gopt help <命令>` 查看单个命令。".to_string(),
            CoreErrorKind::InvalidArgument => "参数值不合法，请按 `gopt help <命令>` 里的取值重试。".to_string(),
            CoreErrorKind::NotFound => {
                "目标不存在：游戏可能尚未启动、进程已退出，或启动项名字不匹配（`gopt list` 可查看）。".to_string()
            }
            CoreErrorKind::AccessDenied => {
                "权限不足：请用「以管理员身份运行」的终端重试；只想优化当前用户可改的项时，\
                 可跳过电源方案/HKLM 启动项（优先级、亲和性、工作集不需要提权）。"
                    .to_string()
            }
            CoreErrorKind::PolicyDenied => {
                "被安全红线拒绝：gopt 永不把进程提升到 REALTIME，也不会注入/Hook 任何进程。".to_string()
            }
            CoreErrorKind::Unsupported => "当前系统不支持该操作（例如跨处理器组的亲和性需要逐组应用）。".to_string(),
            CoreErrorKind::Io => "文件读写失败：请检查数据目录是否可写、磁盘是否已满。".to_string(),
            CoreErrorKind::Hal => "系统调用失败：`--json` 里的 win32_code 是原始错误码，可用于查 msdocs。".to_string(),
            CoreErrorKind::Journal => {
                "审计日志操作失败：日志可能被其它进程改写，关闭后重开会重新读取。".to_string()
            }
            CoreErrorKind::AuditChainBroken => {
                "审计链校验失败：日志被篡改或损坏，gopt 拒绝基于它继续修改系统；\
                 请用 `gopt journal` 查看记录、`gopt verify-journal` 定位第一处不一致。"
                    .to_string()
            }
            CoreErrorKind::Internal => "内部错误：请把 `--json` 输出附在问题报告里。".to_string(),
        }
    }

    /// 英文可执行建议。
    pub fn hint_en(&self) -> String {
        match self.kind {
            CoreErrorKind::Usage => "Usage: run `gopt --help` for the command list, `gopt help <cmd>` for one command.".to_string(),
            CoreErrorKind::InvalidArgument => "Invalid value; see `gopt help <cmd>` for accepted values.".to_string(),
            CoreErrorKind::NotFound => {
                "Target not found: the game may not be running, the process may have exited, or the startup entry name differs (`gopt list`).".to_string()
            }
            CoreErrorKind::AccessDenied => {
                "Access denied: retry from an elevated terminal, or skip the steps that need elevation \
                 (power scheme / HKLM startup entries); priority, affinity and working set do not."
                    .to_string()
            }
            CoreErrorKind::PolicyDenied => {
                "Rejected by policy: gopt never raises a process to REALTIME and never injects or hooks.".to_string()
            }
            CoreErrorKind::Unsupported => "Unsupported on this system (e.g. cross-group affinity needs per-group application).".to_string(),
            CoreErrorKind::Io => "File I/O failed: check that the data directory is writable and the disk is not full.".to_string(),
            CoreErrorKind::Hal => "A system call failed; `win32_code` in `--json` is the raw error code.".to_string(),
            CoreErrorKind::Journal => "The audit log operation failed; the file may have been changed by another process.".to_string(),
            CoreErrorKind::AuditChainBroken => {
                "Audit chain verification failed: the journal was tampered with or damaged; gopt refuses to modify \
                 the system based on it. Inspect with `gopt journal` and locate the first inconsistency with `gopt verify-journal`."
                    .to_string()
            }
            CoreErrorKind::Internal => "Internal error: attach the `--json` output to your report.".to_string(),
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gopt-core: {} failed [{}]: {}",
            self.operation, self.kind, self.message
        )?;
        if let Some(error) = &self.journal {
            if let Some(line) = error.line_no() {
                write!(f, " (line {line})")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for CoreError {}

/// 内核操作结果别名。
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_contract() {
        assert_eq!(CoreError::usage("parse", "x").exit_code(), 1);
        assert_eq!(CoreError::invalid_argument("parse", "x").exit_code(), 1);
        assert_eq!(CoreError::not_found("plan", "x").exit_code(), 2);
        assert_eq!(CoreError::access_denied("apply", "x").exit_code(), 2);
        assert_eq!(CoreError::journal_kind_broken("verify", "x").exit_code(), 3);
    }

    #[test]
    fn hal_errors_keep_kind_and_code() {
        let hal = HalError::win32_from_code("SetPriorityClass", 5);
        let error = CoreError::from_hal("cannot raise pid 42", hal);
        assert_eq!(error.kind(), CoreErrorKind::AccessDenied);
        assert_eq!(error.operation(), "SetPriorityClass");
        assert_eq!(error.hal().and_then(HalError::win32_code), Some(5));
        assert!(error.message().contains("cannot raise pid 42"));
        assert!(error.hint_en().contains("elevated"));
        assert!(error.hint_zh().contains("管理员"));
    }

    #[test]
    fn policy_denial_from_hal_stays_a_policy_denial() {
        let hal = HalError::policy_denied("PriorityClass::parse", "realtime rejected");
        let error = CoreError::from_hal("bad priority", hal);
        assert_eq!(error.kind(), CoreErrorKind::PolicyDenied);
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn journal_chain_errors_map_to_exit_code_three() {
        let path = std::path::Path::new("journal.jsonl");
        let report = gopt_journal::ChainReport {
            total: 1,
            verified: 0,
            first_inconsistency: None,
            anchored: false,
        };
        let error = CoreError::from_journal(JournalError::chain_broken(path, &report));
        assert_eq!(error.kind(), CoreErrorKind::AuditChainBroken);
        assert_eq!(error.exit_code(), 3);
        assert!(error.journal().is_some());
        assert!(error.to_string().contains("gopt-core"));
    }

    #[test]
    fn display_and_std_error_work() {
        let error = CoreError::internal("Session::new", "boom");
        let boxed: Box<dyn std::error::Error> = Box::new(error);
        assert!(boxed.to_string().contains("boom"));
    }
}
