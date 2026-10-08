//! 统一结构化结果 [`Outcome`]：`--json` 的顶层形状 + 退出码的唯一来源。
//!
//! 前端（CLI / 将来的 GUI）不再各自决定"什么算失败、返回几"：
//!
//! ```text
//! { "schema_version": 1, "ok": true, "command": "status", "lang": "zh",
//!   "data": { ... }, "error": null, "notices": [ { "level": "warning", "zh": "...", "en": "..." } ] }
//! ```
//!
//! * `schema_version` 让脚本能判断字段是否兼容（[`crate::SCHEMA_VERSION`]）；
//! * `ok=false` 时 `data` 仍然保留（例如"计划执行到一半失败"要能看到已完成的步骤）；
//! * [`Outcome::exit_code`] 与 [`crate::CoreError::exit_code`] 一致：0/1/2/3。

use serde::Serialize;

use crate::error::CoreError;
use crate::i18n::Lang;

/// 成功退出码。
pub const EXIT_OK: i32 = 0;
/// 用法错误退出码。
pub const EXIT_USAGE: i32 = 1;
/// 环境不满足退出码。
pub const EXIT_ENV: i32 = 2;
/// 审计链校验失败退出码。
pub const EXIT_AUDIT: i32 = 3;

/// 提示级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    /// 普通信息（例如"已跳过未提权才能改的项"）。
    Info,
    /// 警告（例如"这份策略覆盖了内置策略"）。
    Warning,
    /// 错误提示（不改变 `ok`，用于补充说明失败原因之外的问题）。
    Error,
}

impl NoticeLevel {
    /// 稳定短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            NoticeLevel::Info => "info",
            NoticeLevel::Warning => "warning",
            NoticeLevel::Error => "error",
        }
    }
}

/// 一条中英双语提示（进 `--json` 的 `notices` 数组）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notice {
    /// 级别。
    pub level: NoticeLevel,
    /// 中文文案。
    pub zh: String,
    /// 英文文案。
    pub en: String,
}

impl Notice {
    fn new(level: NoticeLevel, zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self {
            level,
            zh: zh.into(),
            en: en.into(),
        }
    }

    /// 信息提示。
    pub fn info(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self::new(NoticeLevel::Info, zh, en)
    }

    /// 警告提示。
    pub fn warning(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self::new(NoticeLevel::Warning, zh, en)
    }

    /// 错误提示。
    pub fn error(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self::new(NoticeLevel::Error, zh, en)
    }

    /// 两种语言相同（事实性提示，例如路径不存在）。
    pub fn same(level: NoticeLevel, text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            level,
            zh: text.clone(),
            en: text,
        }
    }

    /// 按语言取值。
    pub fn pick(&self, lang: Lang) -> &str {
        crate::i18n::pick(lang, &self.zh, &self.en)
    }
}

/// 统一结果：`--json` 的顶层对象，也是退出码的唯一来源。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Outcome<T> {
    /// JSON schema 版本。
    pub schema_version: u32,
    /// 命令是否成功。
    pub ok: bool,
    /// 命令名（`status` / `plan` / `apply` …）。
    pub command: String,
    /// 本次输出使用的语言。
    pub lang: Lang,
    /// 结构化数据（失败时也可能有部分结果）。
    pub data: Option<T>,
    /// 失败时的结构化错误。
    pub error: Option<CoreError>,
    /// 提示信息（警告、降级说明）。
    pub notices: Vec<Notice>,
}

impl<T> Outcome<T> {
    /// 成功结果。
    pub fn ok(command: impl Into<String>, lang: Lang, data: T) -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION,
            ok: true,
            command: command.into(),
            lang,
            data: Some(data),
            error: None,
            notices: Vec::new(),
        }
    }

    /// 失败结果（不带数据）。
    pub fn failed(command: impl Into<String>, lang: Lang, error: CoreError) -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION,
            ok: false,
            command: command.into(),
            lang,
            data: None,
            error: Some(error),
            notices: Vec::new(),
        }
    }

    /// 失败结果（**带**部分数据：例如执行到一半失败仍要看已完成步骤）。
    pub fn failed_with(command: impl Into<String>, lang: Lang, error: CoreError, data: T) -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION,
            ok: false,
            command: command.into(),
            lang,
            data: Some(data),
            error: Some(error),
            notices: Vec::new(),
        }
    }

    /// 追加一条提示。
    #[must_use]
    pub fn with_notice(mut self, notice: Notice) -> Self {
        self.notices.push(notice);
        self
    }

    /// 命令是否成功。
    pub const fn is_ok(&self) -> bool {
        self.ok
    }

    /// 退出码：成功 0；失败按 [`CoreError::exit_code`]。
    pub fn exit_code(&self) -> i32 {
        match &self.error {
            None => EXIT_OK,
            Some(error) => error.exit_code(),
        }
    }
}

impl<T: Serialize> Outcome<T> {
    /// 紧凑 JSON（一行）。
    ///
    /// 不 panic：`Outcome` 的序列化不可能失败，但错误路径仍然给出合法的 JSON 兜底。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|err| fallback_json(&err.to_string()))
    }

    /// 缩进 JSON（人读友好；`gopt --json` 用紧凑形式，`report --json` 用缩进形式）。
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|err| fallback_json(&err.to_string()))
    }
}

/// 序列化失败时的兜底 JSON（仍然遵循 schema，便于脚本统一处理）。
fn fallback_json(message: &str) -> String {
    let escaped = serde_json::Value::String(message.to_string()).to_string();
    format!(
        "{{\"schema_version\":{},\"ok\":false,\"command\":\"serialize\",\"lang\":\"zh\",\
         \"data\":null,\"error\":{{\"kind\":\"internal\",\"operation\":\"Outcome::to_json\",\
         \"message\":{escaped},\"hal\":null,\"journal\":null}},\"notices\":[]}}",
        crate::SCHEMA_VERSION
    )
}

/// 任意可序列化值的 JSON（不 panic）。
pub fn to_json_value<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_outcome_serialises_with_schema_version() {
        let outcome = Outcome::ok("status", Lang::Zh, serde_json::json!({"backend": "mock"}));
        assert!(outcome.is_ok());
        assert_eq!(outcome.exit_code(), EXIT_OK);
        let text = outcome.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(parsed["schema_version"], crate::SCHEMA_VERSION);
        assert_eq!(parsed["command"], "status");
        assert_eq!(parsed["lang"], "zh");
        assert_eq!(parsed["data"]["backend"], "mock");
        assert!(parsed["error"].is_null());
    }

    #[test]
    fn failed_outcome_carries_kind_and_exit_code() {
        let outcome: Outcome<()> = Outcome::failed(
            "plan",
            Lang::En,
            CoreError::not_found("Session::plan", "no game"),
        );
        assert_eq!(outcome.exit_code(), EXIT_ENV);
        let parsed: serde_json::Value = serde_json::from_str(&outcome.to_json()).expect("json");
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"]["kind"], "not_found");
        assert_eq!(parsed["lang"], "en");
    }

    #[test]
    fn notices_are_bilingual_and_typed() {
        let outcome = Outcome::ok("apply", Lang::En, 1u32)
            .with_notice(Notice::warning("已跳过", "skipped"))
            .with_notice(Notice::same(NoticeLevel::Info, "path=C:\\x"));
        assert_eq!(outcome.notices.len(), 2);
        assert_eq!(outcome.notices[0].pick(Lang::En), "skipped");
        assert_eq!(outcome.notices[1].level.as_str(), "info");
        let parsed: serde_json::Value = serde_json::from_str(&outcome.to_json()).expect("json");
        assert_eq!(parsed["notices"][0]["level"], "warning");
        assert_eq!(parsed["notices"][0]["zh"], "已跳过");
    }

    #[test]
    fn partial_data_survives_a_failure() {
        let outcome = Outcome::failed_with("apply", Lang::Zh, CoreError::io("apply", "disk"), 7u32);
        assert_eq!(outcome.exit_code(), EXIT_ENV);
        assert_eq!(outcome.data, Some(7));
        assert!(outcome.to_json_pretty().contains("schema_version"));
    }
}
