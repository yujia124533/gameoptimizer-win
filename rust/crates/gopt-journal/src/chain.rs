//! 哈希链校验：连接性、记录自哈希、行规范形式，以及可选的**外部锚点**。
//!
//! 校验顺序（每条记录从左到右，第一个失败点即 `first_inconsistency`）：
//!
//! 1. 行能否按 schema 解析（含"尾部半行且未修复"）；
//! 2. `id` 是否等于期望序号（1 起、每条 +1）——中途删行/插行会在这里暴露；
//! 3. 链首 `prev_hash` 是否等于 [`GENESIS_HASH`]；
//! 4. `prev_hash` 是否等于上一条的 `hash`——改写某条记录（哪怕重算它自己的哈希）会在这里暴露；
//! 5. 记录的 `hash` 是否等于重算值——只改字段不改哈希会在这里暴露；
//! 6. 行字节是否与规范形式逐字节一致——把行重新格式化、加空格、改键序会在这里暴露。
//!
//! # 能力边界（必须诚实说明）
//!
//! 单靠文件自身，**尾部被整行删除/截断无法检出**：剩下的前缀仍然是一条自洽的链。
//! 因此本模块提供 [`ChainAnchor`]（`len` + 该位置记录的 `hash`）作为外部锚点：
//! 把锚点存到别处（配置、日志、注册表、CI 产物），就能检出"链变短"与"前缀被换掉"。
//! [`crate::Journal::anchor`] 生成锚点，[`crate::Journal::verify_chain_with_anchor`] 用它校验。

use crate::canonical::{is_well_formed_hash, GENESIS_HASH};
use crate::record::JournalRecord;

/// 一条已从磁盘读入的日志行。
///
/// `parsed == None` 表示该行不符合 schema（JSON 解析失败、字段缺失/多余，或不可解码的 UTF-8）；
/// 校验报告会把它标成 [`ChainProblem::MalformedLine`]，而不是静默跳过。
#[derive(Debug, Clone, PartialEq)]
pub struct JournalLine {
    /// 行号（1 起，按文件顺序；空行不占行号）。
    pub line_no: usize,
    /// 行原文（不含行尾的 `\n`；含可能存在的 `\r`，以便检出 CRLF 改写）。
    pub raw: String,
    /// 行是否以来 `\n` 结尾。`false` 只在"尾部半行 + 关闭自动修复"时出现。
    pub terminated: bool,
    /// 解析结果。
    pub parsed: Option<JournalRecord>,
}

impl JournalLine {
    /// 由记录构造（规范行、已终止）——内存中构造校验输入用。
    pub fn from_record(record: &JournalRecord, line_no: usize) -> Self {
        Self {
            line_no,
            raw: record.to_canonical_line(),
            terminated: true,
            parsed: Some(record.clone()),
        }
    }
}

/// 不一致的分类（机器可读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainProblem {
    /// 行无法按 schema 解析（或被截断且未修复）。
    MalformedLine,
    /// `id` 不等于期望序号（删行、插行、重排）。
    IdSequence,
    /// 链首 `prev_hash` 不是创世哈希。
    GenesisPrevHash,
    /// `prev_hash` 与上一条记录的 `hash` 不一致（记录被搬动/重写）。
    PrevHashLink,
    /// 记录自哈希与存储的 `hash` 不一致（字段被改）。
    RecordHash,
    /// 行字节与规范形式不一致（键序/空白/转义被重新格式化）。
    LineCanonical,
    /// 记录数少于外部锚点覆盖的长度（尾部被删除）。
    AnchorLength,
    /// 锚点位置的 `hash` 与锚点记录不一致（前缀被替换）。
    AnchorHash,
}

impl ChainProblem {
    /// 稳定的蛇形命名（JSON 输出用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ChainProblem::MalformedLine => "malformed_line",
            ChainProblem::IdSequence => "id_sequence",
            ChainProblem::GenesisPrevHash => "genesis_prev_hash",
            ChainProblem::PrevHashLink => "prev_hash_link",
            ChainProblem::RecordHash => "record_hash",
            ChainProblem::LineCanonical => "line_canonical",
            ChainProblem::AnchorLength => "anchor_length",
            ChainProblem::AnchorHash => "anchor_hash",
        }
    }

    /// 稳定英文解释（CLI 按本枚举本地化成中文，不翻译 `detail`）。
    pub const fn explanation(self) -> &'static str {
        match self {
            ChainProblem::MalformedLine => "the line does not match the journal record schema",
            ChainProblem::IdSequence => "record ids are not contiguous and increasing from 1",
            ChainProblem::GenesisPrevHash => {
                "the first record does not start a new chain (prev_hash is not the genesis hash)"
            }
            ChainProblem::PrevHashLink => "the record does not link to the hash of its predecessor",
            ChainProblem::RecordHash => "the stored hash does not match the record content",
            ChainProblem::LineCanonical => {
                "the line bytes are not the canonical form of the record (reformatted or extended)"
            }
            ChainProblem::AnchorLength => "the journal is shorter than the external anchor",
            ChainProblem::AnchorHash => "the anchored record hash does not match",
        }
    }

    /// 该问题是否说明"链本身已被破坏"（而不是"需要外部锚点才能判定"）。
    pub const fn breaks_chain(self) -> bool {
        !matches!(self, ChainProblem::AnchorLength | ChainProblem::AnchorHash)
    }
}

impl core::fmt::Display for ChainProblem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 第一处不一致的详情。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChainBreak {
    /// 行下标（0 起；锚点问题时为记录数）。
    pub index: usize,
    /// 行号（1 起）。
    pub line_no: usize,
    /// 涉及的记录 id（行无法解析时为期望的 id；锚点长度问题为 `None`）。
    pub id: Option<u64>,
    /// 问题分类。
    pub problem: ChainProblem,
    /// 具体数值证据（英文，含期望值/实际值）。
    pub detail: String,
}

impl ChainBreak {
    /// 一行英文摘要，例如 `#3 (line 3) record_hash: stored … != recomputed …`。
    pub fn summary(&self) -> String {
        let id = match self.id {
            Some(id) => format!("#{id}"),
            None => "#?".to_string(),
        };
        format!(
            "{id} (line {}) [{}]: {} — {}",
            self.line_no,
            self.problem,
            self.problem.explanation(),
            self.detail
        )
    }
}

/// 校验报告。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChainReport {
    /// 参与校验的行数。
    pub total: usize,
    /// 第一处不一致之前"完全通过"的记录数。
    pub verified: usize,
    /// 第一处不一致；`None` = 链完好。
    pub first_inconsistency: Option<ChainBreak>,
    /// 本次校验是否使用了外部锚点。
    pub anchored: bool,
}

impl ChainReport {
    /// 链是否完好。
    pub const fn is_ok(&self) -> bool {
        self.first_inconsistency.is_none()
    }

    /// 第一处不一致。
    pub const fn first(&self) -> Option<&ChainBreak> {
        self.first_inconsistency.as_ref()
    }

    /// 稳定英文摘要（CLI `--json` 之外的文本输出与审计日志用）。
    pub fn summary(&self) -> String {
        match &self.first_inconsistency {
            None => format!(
                "hash chain verified: {} of {} records{}",
                self.verified,
                self.total,
                if self.anchored {
                    " (anchor matched)"
                } else {
                    ""
                }
            ),
            Some(item) => format!(
                "hash chain broken after {} of {} records — {}",
                self.verified,
                self.total,
                item.summary()
            ),
        }
    }
}

/// 外部锚点：把"链在某个时刻的长度与该位置的哈希"钉在日志文件之外。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChainAnchor {
    /// 锚定时的记录条数。
    pub len: u64,
    /// 第 `len` 条记录的 `hash`；`len == 0` 时为创世哈希。
    pub last_hash: String,
}

impl ChainAnchor {
    /// 由记录序列生成锚点（空链 ⇒ `len = 0` + 创世哈希）。
    pub fn of(records: &[JournalRecord]) -> Self {
        match records.last() {
            Some(record) => Self {
                len: records.len() as u64,
                last_hash: record.hash.clone(),
            },
            None => Self {
                len: 0,
                last_hash: GENESIS_HASH.to_string(),
            },
        }
    }

    /// 空链锚点。
    pub fn empty() -> Self {
        Self {
            len: 0,
            last_hash: GENESIS_HASH.to_string(),
        }
    }

    /// 锚点自身是否自洽（`len == 0` 必须是创世哈希，否则必须是合法哈希文本）。
    pub fn is_well_formed(&self) -> bool {
        if !is_well_formed_hash(&self.last_hash) {
            return false;
        }
        self.len > 0 || self.last_hash == GENESIS_HASH
    }
}

/// 校验一批记录（等价于对它们的规范行做校验）。
pub fn verify_chain(records: &[JournalRecord]) -> ChainReport {
    let lines: Vec<JournalLine> = records
        .iter()
        .enumerate()
        .map(|(index, record)| JournalLine::from_record(record, index + 1))
        .collect();
    verify_lines(&lines, None)
}

/// 校验一批记录，并用外部锚点检查"链是否被截短 / 前缀是否被替换"。
pub fn verify_chain_with_anchor(records: &[JournalRecord], anchor: &ChainAnchor) -> ChainReport {
    let lines: Vec<JournalLine> = records
        .iter()
        .enumerate()
        .map(|(index, record)| JournalLine::from_record(record, index + 1))
        .collect();
    verify_lines(&lines, Some(anchor))
}

/// 校验已读入的日志行（`anchor` 为 `None` 时只做文件内自洽性检查）。
pub fn verify_lines(lines: &[JournalLine], anchor: Option<&ChainAnchor>) -> ChainReport {
    let mut verified = 0usize;
    let mut prev_hash = GENESIS_HASH.to_string();
    let mut breach: Option<ChainBreak> = None;

    // `expected_id` 与行下标同时推进：id 必须从 1 起、每条 +1。
    for (expected_id, (index, line)) in (1u64..).zip(lines.iter().enumerate()) {
        let Some(record) = line.parsed.as_ref() else {
            breach = Some(ChainBreak {
                index,
                line_no: line.line_no,
                id: None,
                problem: ChainProblem::MalformedLine,
                detail: if line.terminated {
                    format!(
                        "the line is not a valid journal record: {}",
                        truncate(&line.raw, 200)
                    )
                } else {
                    "the line is a torn tail (no trailing newline) and automatic repair was disabled"
                        .to_string()
                },
            });
            break;
        };

        if record.id != expected_id {
            breach = Some(ChainBreak {
                index,
                line_no: line.line_no,
                id: Some(record.id),
                problem: ChainProblem::IdSequence,
                detail: format!("expected id {expected_id}, found {}", record.id),
            });
            break;
        }

        if index == 0 && record.prev_hash != GENESIS_HASH {
            breach = Some(ChainBreak {
                index,
                line_no: line.line_no,
                id: Some(record.id),
                problem: ChainProblem::GenesisPrevHash,
                detail: format!(
                    "expected the genesis hash {}, found {}",
                    GENESIS_HASH, record.prev_hash
                ),
            });
            break;
        }

        if record.prev_hash != prev_hash {
            breach = Some(ChainBreak {
                index,
                line_no: line.line_no,
                id: Some(record.id),
                problem: ChainProblem::PrevHashLink,
                detail: format!(
                    "expected prev_hash {} (hash of #{expected_id}), found {}",
                    prev_hash, record.prev_hash
                ),
            });
            break;
        }

        let recomputed = record.compute_hash();
        if recomputed != record.hash {
            breach = Some(ChainBreak {
                index,
                line_no: line.line_no,
                id: Some(record.id),
                problem: ChainProblem::RecordHash,
                detail: format!(
                    "stored hash {} != recomputed hash {}",
                    record.hash, recomputed
                ),
            });
            break;
        }

        if line.raw != record.to_canonical_line() {
            let offset = first_difference(&line.raw, &record.to_canonical_line());
            breach = Some(ChainBreak {
                index,
                line_no: line.line_no,
                id: Some(record.id),
                problem: ChainProblem::LineCanonical,
                detail: format!(
                    "line bytes differ from the canonical form at byte {offset} \
                     (line {} bytes, canonical {} bytes)",
                    line.raw.len(),
                    record.to_canonical_line().len()
                ),
            });
            break;
        }

        verified += 1;
        prev_hash = record.hash.clone();
    }

    if breach.is_none() {
        if let Some(anchor) = anchor {
            if let Some(item) = check_anchor(lines, anchor) {
                breach = Some(item);
            }
        }
    }

    ChainReport {
        total: lines.len(),
        verified,
        first_inconsistency: breach,
        anchored: anchor.is_some(),
    }
}

/// 锚点校验：长度不能变短；锚点位置的哈希必须一致。
fn check_anchor(lines: &[JournalLine], anchor: &ChainAnchor) -> Option<ChainBreak> {
    let Some(anchor_index) = usize::try_from(anchor.len).ok().filter(|len| *len > 0) else {
        // len == 0：只要求"锚点自洽"，任何后续记录都算新增。
        return None;
    };
    if lines.len() < anchor_index {
        return Some(ChainBreak {
            index: lines.len(),
            line_no: lines.len() + 1,
            id: None,
            problem: ChainProblem::AnchorLength,
            detail: format!(
                "the journal holds {} records but the anchor covers {} (records were removed)",
                lines.len(),
                anchor.len
            ),
        });
    }
    let record = lines
        .get(anchor_index - 1)
        .and_then(|line| line.parsed.as_ref())?;
    if record.hash != anchor.last_hash {
        return Some(ChainBreak {
            index: anchor_index - 1,
            line_no: lines
                .get(anchor_index - 1)
                .map_or(anchor_index, |line| line.line_no),
            id: Some(record.id),
            problem: ChainProblem::AnchorHash,
            detail: format!(
                "anchored hash {} does not match the record hash {}",
                anchor.last_hash, record.hash
            ),
        });
    }
    None
}

/// 首个不同字节的下标（长度相同时也不会 panic）。
fn first_difference(left: &str, right: &str) -> usize {
    left.bytes()
        .zip(right.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| left.len().min(right.len()))
}

/// 截断到最多 `max` 个字符（不切断 UTF-8）。
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{JournalDraft, JournalKind};
    use serde_json::json;

    fn chain(len: u64) -> Vec<JournalRecord> {
        let mut records = Vec::new();
        let mut prev = GENESIS_HASH.to_string();
        for index in 1..=len {
            let draft = JournalDraft::at(
                1_700_000_000_000 + index as i64,
                JournalKind::Apply,
                format!("pid:{}", 1000 + index),
            )
            .with_before(json!({"pid": 1000 + index, "priority": "normal"}))
            .with_after(json!({"pid": 1000 + index, "priority": "high"}));
            let record = draft.into_record(index, prev);
            prev = record.hash.clone();
            records.push(record);
        }
        records
    }

    #[test]
    fn intact_chain_verifies() {
        let records = chain(3);
        let report = verify_chain(&records);
        assert!(report.is_ok(), "{}", report.summary());
        assert_eq!(report.verified, 3);
        assert_eq!(report.total, 3);
        assert!(report.summary().contains("verified: 3 of 3"));
        let record = records.first().expect("first record");
        assert_eq!(record.prev_hash, GENESIS_HASH);
    }

    #[test]
    fn field_edit_is_reported_as_record_hash_mismatch() {
        let mut records = chain(4);
        records[1].target = "pid:9999".to_string();
        let report = verify_chain(&records);
        let breach = report.first().expect("break");
        assert_eq!(breach.id, Some(2));
        assert_eq!(breach.problem, ChainProblem::RecordHash);
        assert_eq!(report.verified, 1);
    }

    #[test]
    fn self_consistent_rewrite_is_caught_by_the_link() {
        let mut records = chain(3);
        // 攻击者改字段并重算自己的哈希：下一条的 prev_hash 就断了。
        records[1].target = "pid:9999".to_string();
        records[1].hash = records[1].compute_hash();
        let report = verify_chain(&records);
        let breach = report.first().expect("break");
        assert_eq!(breach.id, Some(3));
        assert_eq!(breach.problem, ChainProblem::PrevHashLink);
    }

    #[test]
    fn deleted_middle_record_is_reported_as_id_gap() {
        let mut records = chain(4);
        records.remove(1);
        let report = verify_chain(&records);
        let breach = report.first().expect("break");
        assert_eq!(breach.problem, ChainProblem::IdSequence);
        assert_eq!(breach.id, Some(3));
        assert!(breach.detail.contains("expected id 2"));
    }

    #[test]
    fn wrong_genesis_is_reported() {
        let mut records = chain(1);
        records[0].prev_hash = "f".repeat(64);
        records[0].hash = records[0].compute_hash();
        let report = verify_chain(&records);
        assert_eq!(
            report.first().expect("break").problem,
            ChainProblem::GenesisPrevHash
        );
    }

    #[test]
    fn reformatted_line_is_reported() {
        let records = chain(2);
        let lines = vec![
            JournalLine::from_record(&records[0], 1),
            JournalLine {
                line_no: 2,
                raw: records[1].to_canonical_line().replace(',', ", "),
                terminated: true,
                parsed: Some(records[1].clone()),
            },
        ];
        let report = verify_lines(&lines, None);
        assert_eq!(
            report.first().expect("break").problem,
            ChainProblem::LineCanonical
        );
    }

    #[test]
    fn malformed_line_is_reported_first() {
        let records = chain(2);
        let lines = vec![
            JournalLine::from_record(&records[0], 1),
            JournalLine {
                line_no: 2,
                raw: "{\"id\":".to_string(),
                terminated: false,
                parsed: None,
            },
        ];
        let report = verify_lines(&lines, None);
        assert_eq!(
            report.first().expect("break").problem,
            ChainProblem::MalformedLine
        );
        assert!(report.summary().contains("torn tail"));
    }

    #[test]
    fn anchor_detects_truncated_tail() {
        let records = chain(4);
        let anchor = ChainAnchor::of(&records);
        assert!(anchor.is_well_formed());

        // 完好：带锚点校验通过。
        assert!(verify_chain_with_anchor(&records, &anchor).is_ok());

        // 删掉最后两条：文件内自洽，锚点检出。
        let truncated = &records[..2];
        assert!(verify_chain(truncated).is_ok());
        let report = verify_chain_with_anchor(truncated, &anchor);
        let breach = report.first().expect("anchor break");
        assert_eq!(breach.problem, ChainProblem::AnchorLength);
        assert!(report.anchored);

        // 前缀被换：锚点哈希不一致（改写者把最后一条改成自洽的假记录）。
        let mut replaced = records.clone();
        replaced[3].target = "pid:1".to_string();
        replaced[3].hash = replaced[3].compute_hash();
        let report = verify_chain_with_anchor(&replaced, &anchor);
        assert_eq!(
            report.first().expect("anchor break").problem,
            ChainProblem::AnchorHash
        );

        // 锚点未覆盖的更长链：新增记录不算篡改。
        let longer = chain(6);
        let short_anchor = ChainAnchor::of(&longer[..4]);
        let report = verify_chain_with_anchor(&longer, &short_anchor);
        assert!(report.is_ok(), "{}", report.summary());
    }

    #[test]
    fn empty_chain_with_empty_anchor_is_ok() {
        let report = verify_chain_with_anchor(&[], &ChainAnchor::empty());
        assert!(report.is_ok());
        assert_eq!(report.total, 0);
        assert!(ChainAnchor::empty().is_well_formed());
        assert!(!ChainAnchor {
            len: 0,
            last_hash: "f".repeat(64)
        }
        .is_well_formed());
    }

    #[test]
    fn problem_names_and_explanations_are_stable() {
        assert_eq!(ChainProblem::RecordHash.as_str(), "record_hash");
        assert!(ChainProblem::AnchorLength.explanation().contains("shorter"));
        assert!(!ChainProblem::AnchorLength.breaks_chain());
        assert!(ChainProblem::RecordHash.breaks_chain());
    }
}
