//! 运行期语言与中英双语文案（红线：全部功能中英双语）。
//!
//! 语言只影响**前端可见的文案**：审计日志、策略诊断、HAL/日志错误消息都保持稳定英文
//! （它们是跨语言可检索的机器事实），中文由前端按同一个结构化字段本地化。
//!
//! 语言来源优先级：`--lang` 参数 > `GOPT_LANG` 环境变量 > 默认中文。

use core::fmt;

use serde::{Deserialize, Serialize};

/// 运行期语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// 中文。
    Zh,
    /// English.
    En,
}

impl Lang {
    /// 全部语言。
    pub const ALL: [Lang; 2] = [Lang::Zh, Lang::En];

    /// 稳定短名（`--json` 的 `lang` 字段与 `--lang` 取值）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    /// BCP-47 语言标记（给将来的 GUI / 本地化工具用）。
    pub const fn bcp47(self) -> &'static str {
        match self {
            Lang::Zh => "zh-CN",
            Lang::En => "en-US",
        }
    }

    /// 语言的自称（`--lang` 帮助文本里用）。
    pub const fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "English",
        }
    }

    /// 宽松解析：`zh` / `zh-CN` / `zh_cn` / `cn` / `chinese` / `中文` / `en` / `en-US` / `english` / `英文`。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "zh" | "zh-cn" | "zh_cn" | "zh-hans" | "cn" | "chinese" | "中文" | "简体中文" => {
                Some(Lang::Zh)
            }
            "en" | "en-us" | "en_us" | "en-gb" | "english" | "英文" => Some(Lang::En),
            _ => None,
        }
    }

    /// 读取 `GOPT_LANG` 环境变量（缺省或非法时返回 `None`）。
    pub fn from_env() -> Option<Self> {
        std::env::var("GOPT_LANG")
            .ok()
            .and_then(|value| Lang::parse(&value))
    }

    /// 默认语言：`GOPT_LANG` 说了算，否则中文（与 C++ 版默认语言一致）。
    pub fn default_lang() -> Self {
        Self::from_env().unwrap_or(Lang::Zh)
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 按语言在两条文案里选一条（没有分配、没有全局状态）。
///
/// ```
/// use gopt_core::{pick, Lang};
///
/// assert_eq!(pick(Lang::Zh, "已完成", "done"), "已完成");
/// assert_eq!(pick(Lang::En, "已完成", "done"), "done");
/// ```
pub fn pick<'a>(lang: Lang, zh: &'a str, en: &'a str) -> &'a str {
    match lang {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

/// 一条中英双语文本（进 `--json` 时两种语言都在，进终端时按 `--lang` 选一条）。
///
/// 与 `gopt_policy::Reason` 是同一个形状，但这里是"内核自己产生"的文案
/// （策略理由仍然原样透传 `Reason`，不重新包装，避免出现两份可漂移的文本）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Text {
    /// 中文文案。
    pub zh: String,
    /// 英文文案。
    pub en: String,
}

impl Text {
    /// 构造。
    pub fn new(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self {
            zh: zh.into(),
            en: en.into(),
        }
    }

    /// 两种语言相同（例如只含数字/路径的事实性提示）。
    pub fn same(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            zh: text.clone(),
            en: text,
        }
    }

    /// 按语言取值。
    pub fn pick(&self, lang: Lang) -> &str {
        match lang {
            Lang::Zh => &self.zh,
            Lang::En => &self.en,
        }
    }
}

impl fmt::Display for Text {
    /// 默认显示中文（只在终端兜底路径里用到；`--json` 走结构化字段）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.zh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_tags_and_self_names() {
        for text in ["zh", "zh-CN", "ZH_cn", "cn", "中文", "Chinese"] {
            assert_eq!(Lang::parse(text), Some(Lang::Zh), "{text}");
        }
        for text in ["en", "EN-US", "english", "英文"] {
            assert_eq!(Lang::parse(text), Some(Lang::En), "{text}");
        }
        assert_eq!(Lang::parse("de"), None);
        assert_eq!(Lang::parse(""), None);
    }

    #[test]
    fn names_are_stable_and_text_picks_by_lang() {
        assert_eq!(Lang::Zh.as_str(), "zh");
        assert_eq!(Lang::En.bcp47(), "en-US");
        assert_eq!(Lang::ALL.len(), 2);
        let text = Text::new("高", "high");
        assert_eq!(text.pick(Lang::Zh), "高");
        assert_eq!(text.pick(Lang::En), "high");
        assert_eq!(Text::same("ok").pick(Lang::En), "ok");
        assert_eq!(text.to_string(), "高");
    }
}
