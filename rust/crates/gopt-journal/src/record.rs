//! 日志记录：`JournalRecord`（链上元素）与 `JournalDraft`（尚未入链的草稿）。
//!
//! # 记录 schema（JSONL 每行一个对象，字段序固定）
//!
//! ```text
//! {
//!   "id":          1,                      // 链上序号：从 1 起、严格 +1
//!   "ts_unix_ms":  1735689600123,          // UTC 毫秒；旧格式导入时沿用原时间戳（可为负）
//!   "kind":        "apply",                // apply | rollback | imported
//!   "target":      "pid:1234",             // 作用对象：pid:<pid> / power-scheme / run:<HIVE>:<name> / game:<index>
//!   "before":      {...} | null,           // 写入前的状态（回滚依据）；null = 未知/不可回滚
//!   "after":       {...} | null,           // 写入后的状态（可解释性）
//!   "rule_id":     "policy:game/cs2" | null, // 触发本次修改的策略/规则标识
//!   "prev_hash":   "…64 hex…",             // 上一条记录的 hash；首条为创世哈希（64 个 0）
//!   "hash":        "…64 hex…"              // SHA256( 规范化正文 ‖ prev_hash )
//! }
//! ```
//!
//! * 行必须**逐字段完整**：`serde(deny_unknown_fields)`，多一个未知键、缺一个字段都算格式异常
//!   （审计格式由本 crate 独占，严格比宽容更有价值）。
//! * 反序列化只用于**读取**；写盘一律走 [`JournalRecord::to_canonical_line`]，
//!   保证磁盘上的行 == 规范形式（[`crate::chain`] 会用这一点检出"被重新格式化/加键"的行）。
//! * `target` 与 `before`/`after` 的字段约定见 [`crate::payload`]。

use serde_json::Value;

use crate::canonical::{self, chain_hash};

/// 记录类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalKind {
    /// 一次状态修改**已应用**（`before` 是回滚依据）。
    Apply,
    /// 一次回滚**已执行**（审计条目；`plan_rollback` 不会再把它当成待撤销的修改）。
    Rollback,
    /// 由 C++ 旧格式（`savepoints.txt` / `games.conf`）导入的历史条目。
    Imported,
}

impl JournalKind {
    /// 稳定的蛇形命名（JSON 字段值）。
    pub const fn as_str(self) -> &'static str {
        match self {
            JournalKind::Apply => "apply",
            JournalKind::Rollback => "rollback",
            JournalKind::Imported => "imported",
        }
    }

    /// 全部类型（枚举顺序固定，供 CLI 文档/校验使用）。
    pub const ALL: [JournalKind; 3] = [
        JournalKind::Apply,
        JournalKind::Rollback,
        JournalKind::Imported,
    ];

    /// 解析蛇形命名（同时容忍 `Apply` / `APPLY` 这类手写形式）。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "apply" | "applied" => Some(JournalKind::Apply),
            "rollback" | "rolled_back" => Some(JournalKind::Rollback),
            "imported" | "import" | "legacy" => Some(JournalKind::Imported),
            _ => None,
        }
    }

    /// 该类记录是否代表"需要被回滚的修改"（`apply` / `imported`）。
    pub const fn is_reversible(self) -> bool {
        matches!(self, JournalKind::Apply | JournalKind::Imported)
    }
}

impl core::fmt::Display for JournalKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 链上一条记录（完整、自校验）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRecord {
    /// 链上序号：从 1 起，严格递增 +1。
    pub id: u64,
    /// UTC 毫秒时间戳（旧格式导入时沿用原值，可能为负）。
    pub ts_unix_ms: i64,
    /// 记录类型。
    pub kind: JournalKind,
    /// 作用对象（稳定字符串标识，见模块文档）。
    pub target: String,
    /// 写入前状态（回滚依据；`null` = 未知，不可回滚）。
    pub before: Option<Value>,
    /// 写入后状态（可解释性）。
    pub after: Option<Value>,
    /// 触发本次修改的策略/规则标识。
    pub rule_id: Option<String>,
    /// 上一条记录的 `hash`；链首为 [`GENESIS_HASH`](crate::canonical::GENESIS_HASH)。
    pub prev_hash: String,
    /// 本记录的 `SHA256(规范化正文 ‖ prev_hash)`。
    pub hash: String,
}

impl JournalRecord {
    /// 规范正文：`id, ts_unix_ms, kind, target, before, after, rule_id, prev_hash`（**不含 `hash`**）。
    ///
    /// 字段序由本函数显式写死，不依赖 `serde` 的字段序，也不依赖任何 map 的迭代顺序。
    pub fn body_json(&self) -> String {
        let mut out = String::with_capacity(256);
        write_fields(&mut out, self, false);
        out
    }

    /// 完整规范行（含 `hash`，**不含换行**）：写盘时其后恰好补一个 `\n`。
    pub fn to_canonical_line(&self) -> String {
        let mut out = String::with_capacity(256);
        write_fields(&mut out, self, true);
        out
    }

    /// 重算本记录的哈希（不修改 `hash` 字段）。
    pub fn compute_hash(&self) -> String {
        chain_hash(&self.body_json(), &self.prev_hash)
    }

    /// 重算哈希并与 `hash` 字段比较。
    pub fn has_valid_hash(&self) -> bool {
        self.compute_hash() == self.hash
    }

    /// 稳定单行描述 `#3 apply pid:1234`（日志/CLI 摘要用）。
    pub fn tag(&self) -> String {
        format!("#{} {} {}", self.id, self.kind, self.target)
    }

    /// 本记录是否可回滚（有 `before` 且类型可逆）。
    pub fn is_reversible(&self) -> bool {
        self.kind.is_reversible() && self.before.is_some()
    }
}

/// 尚未入链的记录草稿：`id` / `prev_hash` / `hash` 由 [`crate::Journal`] 追加时补齐。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JournalDraft {
    /// UTC 毫秒时间戳。
    pub ts_unix_ms: i64,
    /// 记录类型。
    pub kind: JournalKind,
    /// 作用对象。
    pub target: String,
    /// 写入前状态。
    pub before: Option<Value>,
    /// 写入后状态。
    pub after: Option<Value>,
    /// 策略/规则标识。
    pub rule_id: Option<String>,
}

impl JournalDraft {
    /// 构造（时间戳 0，`before`/`after`/`rule_id` 为空）。
    pub fn new(kind: JournalKind, target: impl Into<String>) -> Self {
        Self {
            ts_unix_ms: 0,
            kind,
            target: target.into(),
            before: None,
            after: None,
            rule_id: None,
        }
    }

    /// 构造并使用当前系统时间。
    pub fn now(kind: JournalKind, target: impl Into<String>) -> Self {
        let mut draft = Self::new(kind, target);
        draft.ts_unix_ms = now_unix_ms();
        draft
    }

    /// 构造并指定时间戳（导入旧格式时使用）。
    pub fn at(ts_unix_ms: i64, kind: JournalKind, target: impl Into<String>) -> Self {
        let mut draft = Self::new(kind, target);
        draft.ts_unix_ms = ts_unix_ms;
        draft
    }

    /// 设置 `before` / `after`。
    #[must_use]
    pub fn with_values(mut self, before: Option<Value>, after: Option<Value>) -> Self {
        self.before = before;
        self.after = after;
        self
    }

    /// 只设置 `before`（回滚依据）。
    #[must_use]
    pub fn with_before(mut self, before: Value) -> Self {
        self.before = Some(before);
        self
    }

    /// 只设置 `after`。
    #[must_use]
    pub fn with_after(mut self, after: Value) -> Self {
        self.after = Some(after);
        self
    }

    /// 设置策略/规则标识。
    #[must_use]
    pub fn with_rule_id(mut self, rule_id: impl Into<String>) -> Self {
        self.rule_id = Some(rule_id.into());
        self
    }

    /// 入链：补齐 `id` / `prev_hash` 并计算 `hash`。
    pub fn into_record(self, id: u64, prev_hash: impl Into<String>) -> JournalRecord {
        let mut record = JournalRecord {
            id,
            ts_unix_ms: self.ts_unix_ms,
            kind: self.kind,
            target: self.target,
            before: self.before,
            after: self.after,
            rule_id: self.rule_id,
            prev_hash: prev_hash.into(),
            hash: String::new(),
        };
        record.hash = record.compute_hash();
        record
    }
}

/// 当前 UTC 毫秒时间戳；系统时钟早于 1970 时返回 0（不 panic）。
pub fn now_unix_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

/// 按固定字段序写入记录（`include_hash` 决定是否写 `hash` 字段）。
fn write_fields(out: &mut String, record: &JournalRecord, include_hash: bool) {
    out.push_str("{\"id\":");
    out.push_str(&record.id.to_string());
    out.push_str(",\"ts_unix_ms\":");
    out.push_str(&record.ts_unix_ms.to_string());
    out.push_str(",\"kind\":");
    canonical::write_json_string(out, record.kind.as_str());
    out.push_str(",\"target\":");
    canonical::write_json_string(out, &record.target);
    out.push_str(",\"before\":");
    canonical::write_optional_value(out, record.before.as_ref());
    out.push_str(",\"after\":");
    canonical::write_optional_value(out, record.after.as_ref());
    out.push_str(",\"rule_id\":");
    canonical::write_optional_str(out, record.rule_id.as_deref());
    out.push_str(",\"prev_hash\":");
    canonical::write_json_string(out, &record.prev_hash);
    if include_hash {
        out.push_str(",\"hash\":");
        canonical::write_json_string(out, &record.hash);
    }
    out.push('}');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::GENESIS_HASH;
    use serde_json::json;

    fn sample_draft() -> JournalDraft {
        JournalDraft::at(1_700_000_000_000, JournalKind::Apply, "pid:4242")
            .with_before(json!({"pid": 4242, "priority": "normal"}))
            .with_after(json!({"pid": 4242, "priority": "high"}))
            .with_rule_id("policy:game/cs2")
    }

    #[test]
    fn record_line_is_canonical_and_verifiable() {
        let record = sample_draft().into_record(1, GENESIS_HASH);
        assert!(record.has_valid_hash());
        let line = record.to_canonical_line();
        let expected = format!(
            "{{\"id\":1,\"ts_unix_ms\":1700000000000,\"kind\":\"apply\",\"target\":\"pid:4242\",\
             \"before\":{{\"pid\":4242,\"priority\":\"normal\"}},\
             \"after\":{{\"pid\":4242,\"priority\":\"high\"}},\
             \"rule_id\":\"policy:game/cs2\",\"prev_hash\":\"{GENESIS_HASH}\",\"hash\":\"{}\"}}",
            canonical::chain_hash(&record.body_json(), GENESIS_HASH)
        );
        assert_eq!(line, expected);
        // 行里没有换行/尾随空白：换行由写盘方（Journal::append）恰好补一个。
        assert!(!line.contains('\n'));
        assert_eq!(line.trim_end(), line);
        assert!(crate::canonical::is_well_formed_hash(&record.hash));
    }

    #[test]
    fn body_excludes_hash_and_is_stable() {
        let record = sample_draft().into_record(7, "a".repeat(64));
        let body = record.body_json();
        assert!(!body.contains("\"hash\""));
        assert!(body.contains("\"prev_hash\":\"aaaa"));
        assert_eq!(body, record.body_json());
    }

    #[test]
    fn serde_round_trip_preserves_canonical_line() {
        let record = sample_draft().into_record(3, GENESIS_HASH);
        let line = record.to_canonical_line();
        let parsed: JournalRecord = serde_json::from_str(&line).expect("line parses");
        assert_eq!(parsed, record);
        assert_eq!(parsed.to_canonical_line(), line);
        assert_eq!(parsed.compute_hash(), record.hash);
    }

    #[test]
    fn unknown_fields_are_rejected_and_optional_fields_must_stay_explicit() {
        let record = sample_draft().into_record(1, GENESIS_HASH);
        let line = record.to_canonical_line();
        // 未知键：`deny_unknown_fields` ⇒ 直接判为格式异常。
        let with_extra = line.replace(",\"hash\"", ",\"extra\":1,\"hash\"");
        assert!(serde_json::from_str::<JournalRecord>(&with_extra).is_err());
        // 必需字段缺失 ⇒ 格式异常。
        let without_ts = line.replace(",\"ts_unix_ms\":1700000000000", "");
        assert!(serde_json::from_str::<JournalRecord>(&without_ts).is_err());
        // 可选字段缺失：serde 会补 `None`（Option 的特例），但那样它就不再是规范行，
        // 自哈希也对不上 —— 链校验会以 `record_hash` 报出。
        let without_rule = line.replace(",\"rule_id\":\"policy:game/cs2\"", "");
        let stripped: JournalRecord = serde_json::from_str(&without_rule)
            .expect("a missing Option field deserializes to None");
        assert_eq!(stripped.rule_id, None);
        assert_ne!(stripped.to_canonical_line(), without_rule);
        assert!(!stripped.has_valid_hash());
    }

    #[test]
    fn tampering_with_a_field_changes_the_hash() {
        let record = sample_draft().into_record(1, GENESIS_HASH);
        let mut tampered = record.clone();
        tampered.target = "pid:9999".to_string();
        assert_ne!(tampered.compute_hash(), record.hash);
        assert!(!tampered.has_valid_hash());
    }

    #[test]
    fn kind_names_and_reversibility() {
        assert_eq!(JournalKind::Apply.as_str(), "apply");
        assert_eq!(JournalKind::parse("IMPORTED"), Some(JournalKind::Imported));
        assert_eq!(JournalKind::parse("nope"), None);
        assert!(JournalKind::Apply.is_reversible());
        assert!(JournalKind::Imported.is_reversible());
        assert!(!JournalKind::Rollback.is_reversible());
        let record = sample_draft().into_record(1, GENESIS_HASH);
        assert!(record.is_reversible());
        assert_eq!(record.tag(), "#1 apply pid:4242");
    }

    #[test]
    fn now_unix_ms_is_plausible() {
        // 2020-01-01 之后、2100 之前（不依赖精确时钟，只保证量级正确）。
        assert!(now_unix_ms() > 1_577_836_800_000);
    }
}
