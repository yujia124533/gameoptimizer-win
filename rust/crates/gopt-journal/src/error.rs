//! 结构化错误：与 `gopt-hal` 的错误风格一致（机器可读分类 + 稳定英文消息 + 出错位置）。
//!
//! * **禁止以 panic 作为错误路径**：crate 内 `deny(clippy::unwrap_used / expect_used /
//!   panic / todo / unimplemented)`（仅测试豁免），所有失败都返回 [`JournalError`]。
//! * **可解释**：错误携带 [`JournalErrorKind`]、`operation`（失败的操作名）、
//!   `path`（涉及的日志文件）、`line_no`（涉及的日志行号，1 起）与 `win32_code`
//!   （底层文件系统错误码，若有）。
//! * **可与 HAL 错误统一呈现**：`impl From<JournalError> for HalError`，CLI 只需处理一种错误。

use core::fmt;
use std::path::Path;

use gopt_hal::{HalError, HalErrorKind};

use crate::chain::ChainReport;

/// 日志错误分类（机器可读，可直接序列化进 `--json` 输出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalErrorKind {
    /// 文件系统层失败：读、写、截断、原子改名。
    Io,
    /// 日志行不符合 schema（JSON 解析失败或字段缺失/多余）。
    MalformedRecord,
    /// 哈希链已被破坏（篡改、损坏，或尾部记录被删除后与锚点不符）。
    ChainBroken,
    /// 参数非法，例如 `to_id` 在日志中不存在。
    InvalidArgument,
    /// 日志文件在本次打开之后被外部改写（长度变化）——必须重新打开才能继续追加。
    Stale,
    /// 旧格式来源不可读或路径类型不对（降级说明，不视为致命错误）。
    LegacySource,
}

impl JournalErrorKind {
    /// 稳定的蛇形命名，用于 JSON 输出与日志字段。
    pub const fn as_str(self) -> &'static str {
        match self {
            JournalErrorKind::Io => "io",
            JournalErrorKind::MalformedRecord => "malformed_record",
            JournalErrorKind::ChainBroken => "chain_broken",
            JournalErrorKind::InvalidArgument => "invalid_argument",
            JournalErrorKind::Stale => "stale",
            JournalErrorKind::LegacySource => "legacy_source",
        }
    }

    /// 该分类是否表示"审计链不可信"——调用方必须停止，不得继续写入或回滚。
    pub const fn is_chain_broken(self) -> bool {
        matches!(self, JournalErrorKind::ChainBroken)
    }
}

impl fmt::Display for JournalErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 日志统一错误类型。
///
/// 字段私有、通过访问器读取，保证 `--json` 与日志序列化格式稳定。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JournalError {
    kind: JournalErrorKind,
    operation: &'static str,
    message: String,
    path: Option<String>,
    line_no: Option<usize>,
    win32_code: Option<u32>,
}

impl JournalError {
    /// 构造一个不关联文件位置的错误。
    pub fn new(
        kind: JournalErrorKind,
        operation: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation,
            message: message.into(),
            path: None,
            line_no: None,
            win32_code: None,
        }
    }

    /// 由 `std::io::Error` 构造：保留原始 Windows 错误码（`raw_os_error`）。
    pub fn io(operation: &'static str, path: &Path, error: &std::io::Error) -> Self {
        Self {
            kind: JournalErrorKind::Io,
            operation,
            message: error.to_string(),
            path: Some(path.display().to_string()),
            line_no: None,
            win32_code: error
                .raw_os_error()
                .and_then(|code| u32::try_from(code).ok()),
        }
    }

    /// 日志行不符合 schema。
    pub fn malformed(
        operation: &'static str,
        path: &Path,
        line_no: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: JournalErrorKind::MalformedRecord,
            operation,
            message: message.into(),
            path: Some(path.display().to_string()),
            line_no: Some(line_no),
            win32_code: None,
        }
    }

    /// 哈希链被破坏：消息与行号取自校验报告的第一处不一致。
    pub fn chain_broken(path: &Path, report: &ChainReport) -> Self {
        Self {
            kind: JournalErrorKind::ChainBroken,
            operation: "Journal::verify_chain",
            message: report.summary(),
            path: Some(path.display().to_string()),
            line_no: report.first_inconsistency.as_ref().map(|item| item.line_no),
            win32_code: None,
        }
    }

    /// 参数非法。
    pub fn invalid_argument(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::InvalidArgument, operation, message)
    }

    /// 日志文件被外部改写（长度变化）。
    pub fn stale(path: &Path, expected_len: u64, actual_len: u64) -> Self {
        Self {
            kind: JournalErrorKind::Stale,
            operation: "Journal::append",
            message: format!(
                "the journal file changed on disk after it was opened \
                 (expected {expected_len} bytes, found {actual_len}); reopen it before appending"
            ),
            path: Some(path.display().to_string()),
            line_no: None,
            win32_code: None,
        }
    }

    /// 旧格式来源降级说明。
    pub fn legacy_source(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::LegacySource, operation, message)
    }

    /// 错误分类。
    pub const fn kind(&self) -> JournalErrorKind {
        self.kind
    }

    /// 失败时正在执行的操作名。
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    /// 稳定英文诊断文本（可进审计日志）。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 涉及的日志文件路径。
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// 涉及的日志行号（1 起）。
    pub const fn line_no(&self) -> Option<usize> {
        self.line_no
    }

    /// 底层文件系统错误码（若来自系统调用）。
    pub const fn win32_code(&self) -> Option<u32> {
        self.win32_code
    }

    /// 是否为"审计链不可信"。
    pub const fn is_chain_broken(&self) -> bool {
        self.kind.is_chain_broken()
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gopt-journal: {} failed [{}]", self.operation, self.kind)?;
        if let Some(path) = &self.path {
            write!(f, " (path={path})")?;
        }
        if let Some(line_no) = self.line_no {
            write!(f, " (line={line_no})")?;
        }
        if let Some(code) = self.win32_code {
            write!(f, " (win32={code})")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for JournalError {}

impl From<JournalError> for HalError {
    /// 统一错误呈现：CLI 只需处理 [`HalError`] 一种类型。
    ///
    /// 分类映射刻意保守（`io` / `stale` → [`HalErrorKind::Internal`]，`chain_broken` 也是
    /// [`HalErrorKind::Internal`]：它表示"审计链不可信，必须停止"，而不是安全红线拒绝）；
    /// 精确分类仍然保留在 [`JournalError::kind`] 里。原始 Win32 错误码降级写进消息文本，
    /// 因为 [`HalError`] 只有 `win32_from_code` 一个带错误码的构造入口。
    fn from(err: JournalError) -> Self {
        let hal_kind = match err.kind {
            JournalErrorKind::Io => HalErrorKind::Internal,
            JournalErrorKind::MalformedRecord => HalErrorKind::InvalidArgument,
            JournalErrorKind::ChainBroken => HalErrorKind::Internal,
            JournalErrorKind::InvalidArgument => HalErrorKind::InvalidArgument,
            JournalErrorKind::Stale => HalErrorKind::Internal,
            JournalErrorKind::LegacySource => HalErrorKind::NotFound,
        };
        let mut message = err.message;
        if let Some(path) = &err.path {
            message = format!("{path}: {message}");
        }
        if let Some(line_no) = err.line_no {
            message = format!("{message} (line {line_no})");
        }
        if let Some(code) = err.win32_code {
            message = format!("{message} (win32={code})");
        }
        HalError::new(hal_kind, err.operation, message)
    }
}

/// 日志操作结果别名。
pub type JournalResult<T> = Result<T, JournalError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_keeps_raw_os_code() {
        let raw = std::io::Error::from_raw_os_error(5);
        let err = JournalError::io("Journal::open", Path::new("C:/x/journal.jsonl"), &raw);
        assert_eq!(err.kind(), JournalErrorKind::Io);
        assert_eq!(err.win32_code(), Some(5));
        assert_eq!(err.path(), Some("C:/x/journal.jsonl"));
        assert!(err.to_string().contains("win32=5"));
    }

    #[test]
    fn chain_broken_is_flagged_and_maps_to_hal() {
        let report = ChainReport {
            total: 3,
            verified: 1,
            first_inconsistency: Some(crate::chain::ChainBreak {
                index: 1,
                line_no: 2,
                id: Some(2),
                problem: crate::chain::ChainProblem::RecordHash,
                detail: "stored hash does not match the record content".to_string(),
            }),
            anchored: false,
        };
        let err = JournalError::chain_broken(Path::new("journal.jsonl"), &report);
        assert!(err.is_chain_broken());
        assert_eq!(err.line_no(), Some(2));
        let hal: HalError = err.clone().into();
        assert_eq!(hal.kind(), HalErrorKind::Internal);
        assert!(hal.message().contains("#2") || hal.message().contains("line 2"));
        assert!(!hal.is_policy_denial());
    }

    #[test]
    fn kinds_have_stable_names() {
        assert_eq!(JournalErrorKind::Io.as_str(), "io");
        assert_eq!(JournalErrorKind::ChainBroken.as_str(), "chain_broken");
        assert_eq!(JournalErrorKind::Stale.as_str(), "stale");
    }
}
