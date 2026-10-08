//! 策略加载诊断：**文件名 + 行号 + 列号 + 稳定英文消息**，并且**永不 panic**。
//!
//! 设计要点（团队红线）：
//!
//! * 策略文件是用户可编辑的数据，任何一处写错都只能变成一条可读的诊断，不能让进程炸掉，
//!   也不能让其余策略失效——所以加载器收集 [`PolicyDiagnostic`] 而不是 `panic!`/`unwrap`。
//! * 诊断携带 [`PolicyOrigin`]（内置文件名 或 用户文件绝对路径）与 1 基行号/列号，
//!   直接对应编辑器里的位置；错误消息是**稳定英文**（与审计日志绑定），
//!   中文说明由前端按字段本地化（见 crate 根文档）。
//! * [`PolicyDiagnostic`] 结构刻意保持精简（<= 128 字节），使其作为 `Err` 侧不触发
//!   `clippy::result_large_err`，可以直接放进 `Result<T, PolicyDiagnostic>`。

use core::fmt;

use serde::{Deserialize, Serialize};

/// 策略来源层级。查找冲突时**用户层优先于内置层**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyLayer {
    /// 内置策略：编译进二进制，仓库内对应 `rust/policies/*.toml`。
    Builtin,
    /// 用户覆盖：`%LOCALAPPDATA%\GameOptimizer\policies.d\*.toml`。
    User,
}

impl PolicyLayer {
    /// 稳定短名（JSON / 审计日志字段）。
    pub const fn as_str(self) -> &'static str {
        match self {
            PolicyLayer::Builtin => "builtin",
            PolicyLayer::User => "user",
        }
    }
}

impl fmt::Display for PolicyLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条策略的来源。
///
/// 内置策略只有文件名（它是编译进二进制的，磁盘上不一定存在），用户策略是绝对路径；
/// [`PolicyOrigin::display_path`] 把两者统一成可直接展示、可直接粘进编辑器的形式。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolicyOrigin {
    layer: PolicyLayer,
    file: String,
}

impl PolicyOrigin {
    /// 内置策略来源（例如 `cs2.toml`）。
    pub fn builtin(file: impl Into<String>) -> Self {
        Self {
            layer: PolicyLayer::Builtin,
            file: file.into(),
        }
    }

    /// 用户策略来源（建议传绝对路径）。
    pub fn user(path: impl Into<String>) -> Self {
        Self {
            layer: PolicyLayer::User,
            file: path.into(),
        }
    }

    /// 所在层。
    pub const fn layer(&self) -> PolicyLayer {
        self.layer
    }

    /// 文件名（内置）或路径（用户）。
    pub fn file(&self) -> &str {
        &self.file
    }

    /// 是否来自用户覆盖目录。
    pub const fn is_user(&self) -> bool {
        matches!(self.layer, PolicyLayer::User)
    }

    /// 展示用路径：内置为 `<builtin>/cs2.toml`，用户为原路径。
    pub fn display_path(&self) -> String {
        match self.layer {
            PolicyLayer::Builtin => format!("<builtin>/{}", self.file),
            PolicyLayer::User => self.file.clone(),
        }
    }
}

impl fmt::Display for PolicyOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_path())
    }
}

/// 诊断级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// 该文件不可用：策略被跳过（其余文件照常加载）。
    Error,
    /// 可疑但可用：例如用户策略覆盖了内置策略、两个游戏抢同一个 exe 模式。
    Warning,
}

impl DiagnosticSeverity {
    /// 稳定短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
        }
    }
}

impl fmt::Display for DiagnosticSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条加载/校验诊断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDiagnostic {
    severity: DiagnosticSeverity,
    origin: PolicyOrigin,
    line: Option<u32>,
    column: Option<u32>,
    message: String,
}

impl PolicyDiagnostic {
    /// 错误：该策略文件被跳过。
    pub fn error(
        origin: PolicyOrigin,
        line: Option<u32>,
        column: Option<u32>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            origin,
            line,
            column,
            message: message.into(),
        }
    }

    /// 警告：可用但需要提醒（覆盖、模式冲突等）。
    pub fn warning(
        origin: PolicyOrigin,
        line: Option<u32>,
        column: Option<u32>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            origin,
            line,
            column,
            message: message.into(),
        }
    }

    /// 级别。
    pub const fn severity(&self) -> DiagnosticSeverity {
        self.severity
    }

    /// 是否为错误。
    pub const fn is_error(&self) -> bool {
        matches!(self.severity, DiagnosticSeverity::Error)
    }

    /// 来源。
    pub const fn origin(&self) -> &PolicyOrigin {
        &self.origin
    }

    /// 1 基行号（语法/展开错误一定有，纯聚合类诊断可能没有）。
    pub const fn line(&self) -> Option<u32> {
        self.line
    }

    /// 1 基列号。
    pub const fn column(&self) -> Option<u32> {
        self.column
    }

    /// 稳定英文消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PolicyDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.origin.display_path();
        match (self.line, self.column) {
            (Some(line), Some(column)) => write!(
                f,
                "{path}:{line}:{column}: {}: {}",
                self.severity, self.message
            ),
            (Some(line), None) => write!(f, "{path}:{line}: {}: {}", self.severity, self.message),
            _ => write!(f, "{path}: {}: {}", self.severity, self.message),
        }
    }
}

impl std::error::Error for PolicyDiagnostic {}

/// 一批加载错误。实现 [`std::error::Error`] 与 [`fmt::Display`]（每条一行），
/// 便于 `?` 直接冒泡到 CLI 顶层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyLoadErrors {
    diagnostics: Vec<PolicyDiagnostic>,
}

impl PolicyLoadErrors {
    /// 由若干条错误构造（调用方保证只放 `Error` 级别）。
    pub fn new(diagnostics: Vec<PolicyDiagnostic>) -> Self {
        Self { diagnostics }
    }

    /// 全部错误。
    pub fn diagnostics(&self) -> &[PolicyDiagnostic] {
        &self.diagnostics
    }

    /// 错误条数。
    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// 是否没有错误。
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

impl fmt::Display for PolicyLoadErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} policy file(s) failed to load:",
            self.diagnostics.len()
        )?;
        for diagnostic in &self.diagnostics {
            writeln!(f, "  {diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PolicyLoadErrors {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn diagnostic_is_small_enough_for_result_err_side() {
        // clippy::result_large_err 的阈值是 128 字节；这里把它变成可断言的工程属性。
        assert!(
            size_of::<PolicyDiagnostic>() <= 128,
            "{}",
            size_of::<PolicyDiagnostic>()
        );
    }

    #[test]
    fn display_contains_file_and_line() {
        let diagnostic = PolicyDiagnostic::error(
            PolicyOrigin::user(r"C:\Users\me\AppData\Local\GameOptimizer\policies.d\bad.toml"),
            Some(7),
            Some(3),
            "unknown action key `prioriti`",
        );
        let text = diagnostic.to_string();
        assert!(text.contains("bad.toml:7:3"), "{text}");
        assert!(text.contains("error: unknown action key"), "{text}");

        let builtin =
            PolicyDiagnostic::warning(PolicyOrigin::builtin("cs2.toml"), None, None, "overridden");
        assert_eq!(
            builtin.to_string(),
            "<builtin>/cs2.toml: warning: overridden"
        );
        assert!(!builtin.is_error());
        assert!(!builtin.origin().is_user());
    }

    #[test]
    fn load_errors_display_lists_every_diagnostic() {
        let errors = PolicyLoadErrors::new(vec![
            PolicyDiagnostic::error(PolicyOrigin::builtin("a.toml"), Some(1), Some(1), "boom"),
            PolicyDiagnostic::error(PolicyOrigin::builtin("b.toml"), Some(2), Some(1), "bang"),
        ]);
        assert_eq!(errors.len(), 2);
        let text = errors.to_string();
        assert!(text.contains("a.toml:1:1"), "{text}");
        assert!(text.contains("b.toml:2:1"), "{text}");
    }

    #[test]
    fn layer_names_are_stable() {
        assert_eq!(PolicyLayer::Builtin.as_str(), "builtin");
        assert_eq!(PolicyLayer::User.as_str(), "user");
        assert_eq!(
            PolicyOrigin::builtin("x.toml").display_path(),
            "<builtin>/x.toml"
        );
        assert_eq!(
            PolicyOrigin::user("C:/x/y.toml").display_path(),
            "C:/x/y.toml"
        );
    }
}
