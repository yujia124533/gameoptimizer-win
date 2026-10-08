//! 规范形式（canonical form）与 SHA-256：哈希链的密码学根。
//!
//! 审计链可验证的前提是「同一条记录在任何环境、任何 serde 版本下都序列化成同一串字节」。
//! 因此本模块**不使用 `serde_json` 的序列化器**，而是自己实现一个规范化 JSON 写入器：
//!
//! | 值 | 规范写法 |
//! | --- | --- |
//! | 对象 | 键按 **UTF-8 字节序**升序排列，`{"k":v,"k2":v2}`，无空格 |
//! | 数组 | `[v1,v2]`，无空格 |
//! | 字符串 | `"` + 转义 + `"`；仅 `"` `\` 用短转义，其余控制字符 `< 0x20` 写成 `\u00xx`（小写十六进制）；`>= 0x20` 的字符（含非 ASCII / 中文）原样输出 UTF-8 |
//! | 整数 | 十进制（`serde_json::Number` 的 `Display`） |
//! | 浮点 | `serde_json` 的最短往返表示（ryu）——同一 `f64` 一定得到同一串文本 |
//! | 其它 | `null` / `true` / `false` |
//!
//! 规范形式里没有换行、没有尾随空白；写盘时由 [`crate::JournalRecord::to_canonical_line`]
//! 在其后**恰好**补一个 `\n`。
//!
//! 哈希定义（与任务书逐字一致）：
//!
//! ```text
//! hash = SHA256( canonical(record 去掉 hash 字段) ‖ prev_hash )
//! ```
//!
//! `prev_hash` 是定长 64 字节的十六进制文本，因此拼接不存在边界歧义。

use core::fmt::Write as _;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 创世哈希：链上第一条记录的 `prev_hash`（64 个小写 `0`）。
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// 是否为合法的小写十六进制 SHA-256 文本（长度 64）。
pub fn is_well_formed_hash(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `SHA-256(bytes)` 的小写十六进制文本（64 字符）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        // 写入 String 永不失败；这里刻意不用 unwrap（crate 内禁止 unwrap/expect）。
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 链式哈希：`SHA256(canonical_body ‖ prev_hash)`。
///
/// `canonical_body` 是**不含 `hash` 字段**的规范正文（含 `prev_hash` 字段本身）；
/// 再追加一次 `prev_hash` 文本，使"链上位置"同时编码进字段与哈希输入——
/// 这样即使字段序/前缀被重组，也无法把记录搬到链上的另一个位置。
pub fn chain_hash(canonical_body: &str, prev_hash: &str) -> String {
    let mut buffer = String::with_capacity(canonical_body.len() + prev_hash.len());
    buffer.push_str(canonical_body);
    buffer.push_str(prev_hash);
    sha256_hex(buffer.as_bytes())
}

/// 任意 JSON 值的规范形式（不带换行）。
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_json_value(&mut out, value);
    out
}

/// 供错误消息使用的紧凑摘要：规范形式截断到 120 个字符（不切断 UTF-8 字符）。
pub fn brief(value: &Value) -> String {
    let text = canonical_json(value);
    if text.chars().count() <= 120 {
        return text;
    }
    let mut out: String = text.chars().take(120).collect();
    out.push('…');
    out
}

/// 规范 JSON 值写入。
pub(crate) fn write_json_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => write_json_string(out, text),
        Value::Array(items) => {
            out.push('[');
            let mut first = true;
            for item in items {
                if !first {
                    out.push(',');
                }
                first = false;
                write_json_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // 键序不依赖 map 实现（serde_json 默认 BTreeMap，但显式再排序一次），
            // 按 UTF-8 字节序 ⇒ 跨平台、跨版本稳定。
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            out.push('{');
            let mut first = true;
            for key in keys {
                let Some(item) = map.get(key.as_str()) else {
                    continue; // 不可能发生：key 来自同一个 map
                };
                if !first {
                    out.push(',');
                }
                first = false;
                write_json_string(out, key);
                out.push(':');
                write_json_value(out, item);
            }
            out.push('}');
        }
    }
}

/// 规范 JSON 字符串写入（转义规则见模块文档）。
pub(crate) fn write_json_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// `Option<&str>` → 规范 JSON（`None` 写成 `null`）。
pub(crate) fn write_optional_str(out: &mut String, value: Option<&str>) {
    match value {
        Some(text) => write_json_string(out, text),
        None => out.push_str("null"),
    }
}

/// `Option<&Value>` → 规范 JSON（`None` 写成 `null`）。
pub(crate) fn write_optional_value(out: &mut String, value: Option<&Value>) {
    match value {
        Some(item) => write_json_value(out, item),
        None => out.push_str("null"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256("abc") 的标准测试向量。
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_well_formed_hash(&sha256_hex(b"")));
        assert!(!is_well_formed_hash(&sha256_hex(b"").to_uppercase()));
        assert!(!is_well_formed_hash("abc"));
    }

    #[test]
    fn object_keys_are_sorted_by_utf8_bytes() {
        let first = canonical_json(&json!({"b": 2, "a": 1, "中": 3}));
        let second = canonical_json(&json!({"中": 3, "a": 1, "b": 2}));
        assert_eq!(first, second);
        // ASCII 键在非 ASCII 键之前（字节序 0x62 < 0xe4）。
        assert_eq!(first, r#"{"a":1,"b":2,"中":3}"#);
    }

    #[test]
    fn strings_escape_only_quotes_backslash_and_controls() {
        // `"`、`\`、`\n`(0x0a) 都要转义；U+007F 与中文 >= 0x20 ⇒ 原样输出。
        let value = json!({"s": "a\"b\\c\nd\u{7f}中文"});
        assert_eq!(
            canonical_json(&value),
            "{\"s\":\"a\\\"b\\\\c\\u000ad\u{7f}中文\"}"
        );
        assert_eq!(canonical_json(&json!("\u{1}")), "\"\\u0001\"");
        assert_eq!(canonical_json(&json!("\t")), "\"\\u0009\"");
    }

    #[test]
    fn floats_and_integers_are_deterministic() {
        let text = canonical_json(&json!({"f": 0.1, "i": 9007199254740993_u64}));
        assert_eq!(text, r#"{"f":0.1,"i":9007199254740993}"#);
    }

    #[test]
    fn chain_hash_depends_on_body_and_prev_hash() {
        let body = r#"{"id":1}"#;
        let none = GENESIS_HASH;
        let other = "1".repeat(64);
        assert_ne!(chain_hash(body, none), chain_hash(body, &other));
        assert_ne!(chain_hash(body, none), chain_hash(r#"{"id":2}"#, none));
        assert!(is_well_formed_hash(&chain_hash(body, none)));
    }

    #[test]
    fn brief_truncates_without_panicking_on_unicode() {
        let long = json!({"s": "中".repeat(200)});
        let text = brief(&long);
        assert!(text.chars().count() <= 121);
        assert!(text.ends_with('…'));
    }
}
