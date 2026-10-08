//! 结构化错误：分类 + 稳定可读消息 + 原始 Win32 错误码。
//!
//! 设计约束（团队红线）：
//!
//! * **禁止以 panic 作为错误路径**：HAL 的每个失败都返回 [`HalError`]；
//!   crate 内部 `deny(clippy::unwrap_used / expect_used / panic / todo / unimplemented)`
//!   （仅测试代码豁免），因此"不会 panic"是可被 CI 检查的工程属性，而不是口头约定。
//! * **可解释**：错误携带 [`HalErrorKind`]（机器可读分类）、`operation`
//!   （失败时正在执行的官方 API/操作名）、`win32_code`（原始错误码，供审计日志与诊断报告）。
//! * **中英双语由前端负责**：[`HalError::message`] 是与日志/审计链绑定的稳定英文诊断文本；
//!   CLI/GUI 按 `kind` + `operation` 本地化出中文，避免在错误类型里塞入会变的文案。

use core::fmt;

/// HAL 错误分类（机器可读，可直接序列化进审计日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HalErrorKind {
    /// 参数非法：调用方修正输入后可重试（例如空的亲和性掩码）。
    InvalidArgument,
    /// 当前系统/平台不支持该操作（例如跨处理器组亲和性、缺失的电源方案）。
    Unsupported,
    /// 目标对象不存在：进程已退出、注册表值已被删除等。
    NotFound,
    /// 权限不足：例如未提权时写 HKLM 启动项、切换电源方案。
    AccessDenied,
    /// 违反安全红线被硬拒绝：目前只有 REALTIME 优先级一类。
    PolicyDenied,
    /// 其它 Win32 调用失败，`win32_code` 携带原始错误码。
    Win32,
    /// 内部不变量被破坏或状态不可用（例如系统返回了自相矛盾的数据）。
    Internal,
}

impl HalErrorKind {
    /// 稳定的蛇形命名，用于 JSON 输出与日志字段。
    pub const fn as_str(self) -> &'static str {
        match self {
            HalErrorKind::InvalidArgument => "invalid_argument",
            HalErrorKind::Unsupported => "unsupported",
            HalErrorKind::NotFound => "not_found",
            HalErrorKind::AccessDenied => "access_denied",
            HalErrorKind::PolicyDenied => "policy_denied",
            HalErrorKind::Win32 => "win32",
            HalErrorKind::Internal => "internal",
        }
    }

    /// 该分类是否属于"安全红线拒绝"（调用方必须停止，不得降级重试）。
    pub const fn is_policy_denial(self) -> bool {
        matches!(self, HalErrorKind::PolicyDenied)
    }

    /// 从 Win32 错误码推导分类。映射表刻意保守：无法识别的一律归为 [`HalErrorKind::Win32`]。
    pub const fn from_win32_code(code: u32) -> Self {
        match code {
            5 => HalErrorKind::AccessDenied,
            2 | 3 | 1168 => HalErrorKind::NotFound,
            87 => HalErrorKind::InvalidArgument,
            50 | 120 => HalErrorKind::Unsupported,
            _ => HalErrorKind::Win32,
        }
    }

    /// 该错误码的默认英文诊断文本（`with_context` 会在此前追加更具体的上下文）。
    const fn default_message(code: u32) -> &'static str {
        match code {
            5 => "access denied; the operation may require administrator privileges",
            2 | 3 | 1168 => "the target does not exist",
            87 => "the target does not exist or a parameter is invalid",
            50 => "the operation is not supported on this system",
            120 => "the operation is not implemented on this system",
            _ => "the Win32 call failed",
        }
    }
}

impl fmt::Display for HalErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// HAL 统一错误类型。
///
/// 字段全部私有，通过访问器读取，保证日志/审计序列化格式稳定。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HalError {
    kind: HalErrorKind,
    operation: &'static str,
    message: String,
    win32_code: Option<u32>,
}

impl HalError {
    /// 构造一个无 Win32 错误码的错误。
    pub fn new(kind: HalErrorKind, operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            operation,
            message: message.into(),
            win32_code: None,
        }
    }

    /// 参数非法。
    pub fn invalid_argument(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(HalErrorKind::InvalidArgument, operation, message)
    }

    /// 当前系统不支持。
    pub fn unsupported(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(HalErrorKind::Unsupported, operation, message)
    }

    /// 目标不存在。
    pub fn not_found(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(HalErrorKind::NotFound, operation, message)
    }

    /// 权限不足。
    pub fn access_denied(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(HalErrorKind::AccessDenied, operation, message)
    }

    /// 安全红线拒绝（REALTIME 优先级等）。
    pub fn policy_denied(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(HalErrorKind::PolicyDenied, operation, message)
    }

    /// 内部错误。
    pub fn internal(operation: &'static str, message: impl Into<String>) -> Self {
        Self::new(HalErrorKind::Internal, operation, message)
    }

    /// 由原始 Win32 错误码构造：分类与默认消息由 [`HalErrorKind::from_win32_code`] 推导。
    pub fn win32_from_code(operation: &'static str, code: u32) -> Self {
        Self {
            kind: HalErrorKind::from_win32_code(code),
            operation,
            message: HalErrorKind::default_message(code).to_string(),
            win32_code: Some(code),
        }
    }

    /// 追加更具体的上下文（放在原消息之前），保留分类与错误码。
    #[must_use]
    pub fn with_context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    /// 覆盖分类（仅用于收紧分类，例如把 OpenProcess 的 87 明确为"进程不存在"）。
    #[must_use]
    pub fn with_kind(mut self, kind: HalErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// 覆盖为明确的"不存在"分类。
    #[must_use]
    pub fn as_not_found(self) -> Self {
        self.with_kind(HalErrorKind::NotFound)
    }

    /// 错误分类。
    pub const fn kind(&self) -> HalErrorKind {
        self.kind
    }

    /// 失败时正在执行的官方 API / 操作名。
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    /// 稳定英文诊断文本（可进审计日志）。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 原始 Win32 错误码（若来自系统调用）。
    pub const fn win32_code(&self) -> Option<u32> {
        self.win32_code
    }

    /// 是否为安全红线拒绝。
    pub const fn is_policy_denial(&self) -> bool {
        self.kind.is_policy_denial()
    }
}

impl fmt::Display for HalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gopt-hal: {} failed [{}]", self.operation, self.kind)?;
        if let Some(code) = self.win32_code {
            write!(f, " (win32={code})")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for HalError {}

/// HAL 操作结果别名。
pub type HalResult<T> = Result<T, HalError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_code_maps_to_kind() {
        assert_eq!(
            HalError::win32_from_code("RegOpenKeyExW", 5).kind(),
            HalErrorKind::AccessDenied
        );
        assert_eq!(
            HalError::win32_from_code("OpenProcess", 87).kind(),
            HalErrorKind::InvalidArgument
        );
        assert_eq!(
            HalError::win32_from_code("PowerSetActiveScheme", 1234).kind(),
            HalErrorKind::Win32
        );
    }

    #[test]
    fn context_prefixes_message_and_keeps_code() {
        let err = HalError::win32_from_code("OpenProcess", 5).with_context("pid 4242");
        assert_eq!(err.win32_code(), Some(5));
        assert!(err.message().starts_with("pid 4242: "));
        assert_eq!(err.operation(), "OpenProcess");
    }

    #[test]
    fn policy_denial_is_flagged() {
        let err = HalError::policy_denied("SetPriorityClass", "REALTIME rejected");
        assert!(err.is_policy_denial());
        assert_eq!(err.kind().as_str(), "policy_denied");
        assert_eq!(err.win32_code(), None);
    }

    #[test]
    fn display_contains_operation_kind_and_code() {
        let text = HalError::win32_from_code("SetProcessAffinityMask", 87).to_string();
        assert!(text.contains("SetProcessAffinityMask"), "{text}");
        assert!(text.contains("invalid_argument"), "{text}");
        assert!(text.contains("win32=87"), "{text}");
    }

    /// 结构化字段必须可直接进 JSON（由 gopt-cli 用 serde_json 输出）。
    /// 这里用编译期断言而不是自造 Serializer：本 crate 只依赖 serde，不引入 serde_json。
    #[test]
    fn hal_error_and_kind_are_serializable() {
        fn assert_serialize<T: serde::Serialize>() {}
        assert_serialize::<HalError>();
        assert_serialize::<HalErrorKind>();

        fn assert_deserialize<'de, T: serde::Deserialize<'de>>() {}
        assert_deserialize::<HalErrorKind>();
    }

    #[test]
    fn into_std_error_is_possible() {
        fn as_std_error(err: HalError) -> Box<dyn std::error::Error> {
            Box::new(err)
        }
        let boxed = as_std_error(HalError::internal("test", "boom"));
        assert!(boxed.to_string().contains("boom"));
    }
}
