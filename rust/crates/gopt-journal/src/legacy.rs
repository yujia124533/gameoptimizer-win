//! 旧格式兼容：**只读**解析 C++ 版（v1.1.0）的 `savepoints.txt` 与 `games.conf`。
//!
//! # 验收红线
//!
//! * **只读**：import 路径只做 `fs::read` / `fs::metadata`；不创建目录、不写文件、
//!   不改 mtime（集成测试会逐字节比对 + 比对目录清单）。
//! * **不 panic**：坏行跳过并计数（`bad_lines`），非 UTF-8 字节走有损解码后按坏行处理，
//!   目录/权限错误降级为 `notes` 而不是 `Err`。
//! * **不猜**：解析规则逐条对齐 C++ 实现（见下），差异只出现在"C++ 会抛异常/崩溃"的地方
//!   ——那些地方 Rust 版必须降级成"坏行跳过"，并在 `notes` 里说明。
//!
//! # 格式 A：`savepoints.txt`（对齐 `SecurityRollback::Serialize` / `TryDeserialize`）
//!
//! 每行 12 个字段，`|` 分隔；无表头：
//!
//! ```text
//! processId|priorityClass|affinityMask|workingSetMin|workingSetMax|hasProcessState|hasWorkingSet|hasPowerScheme|powerSchemeGuid|timestampMs|gameName|description
//! ```
//!
//! 对齐要点（与 C++ 逐条一致）：
//!
//! * 数字字段：十进制优先，**失败后退回十六进制**（兼容 `0x` 前缀）；必须整串消费；
//!   `processId` / `priorityClass` 上限 `u32::MAX`，掩码/工作集上限 `u64::MAX`；
//!   前导 `-` 一律判负（C++ 的 `ParseField` 显式拒绝负数回绕）。
//! * `has*` 字段只有恰好 `"1"` 才是 `true`（其它值一律 `false`）。
//! * `timestampMs` 是唯一允许负值的字段，且**只用十进制**（C++ 用 `stoll`，不回退十六进制）。
//! * 第 12 个字段（`description`）之后的剩余字段被忽略（C++ 只读 12 次 `getline`）；
//!   原始整行会作为 `legacy.raw` 进审计记录，因此信息不会丢。
//! * 行尾 `\r`（CRLF）按 C++ 的做法剥掉；空行跳过。
//!
//! # 格式 B：`games.conf`（对齐 `GameConfig.cpp` 的 `Store()` / `Persist()`）
//!
//! ```text
//! <gameIndex>|<exePath>|<args>|<oI:power:frameLatency:workingSet>
//! ```
//!
//! * `gameIndex` 用 `std::stoi` 的宽松语义（允许前导空白、正负号、尾部残留字符）。
//!   **C++ 在 `stoi` 抛异常时会让异常穿透 `Store()`（未捕获）**；Rust 版把这种行记为坏行跳过。
//! * 第 4 个字段不足 4 个字符时，4 个开关保持结构体默认值
//!   （`optimizedOnLaunch = true, powerScheme = false, frameLatency = false, workingSet = true`），
//!   而不是全 `false`——这是 C++ 的实际行为。
//! * `gameIndex < 0` 的行会被 C++ 静默忽略（不写入 map），这里记为 `ignored_lines`。
//! * 重复的 `gameIndex` 在 C++ 里是"后者覆盖前者"；这里两条都保留进审计链，并记一条 note。

use std::fs;
use std::path::{Path, PathBuf};

use gopt_hal::{PriorityClass, WorkingSetLimits};
use serde_json::{json, Value};

use crate::error::JournalResult;
use crate::payload;
use crate::record::{JournalDraft, JournalKind, JournalRecord};
use crate::store::Journal;

/// 旧格式快照文件名（与 C++ 版同址同名的**只读**来源）。
pub const SAVEPOINTS_FILE_NAME: &str = "savepoints.txt";

/// 旧格式每游戏配置文件名。
pub const GAMES_CONF_FILE_NAME: &str = "games.conf";

/// 导入 `savepoints.txt` 记录使用的 `rule_id`。
pub const LEGACY_SAVEPOINTS_RULE_ID: &str = "legacy:savepoints.txt";

/// 导入 `games.conf` 记录使用的 `rule_id`。
pub const LEGACY_GAMES_CONF_RULE_ID: &str = "legacy:games.conf";

/// C++ `gopt::SavePoint` 的逐字段镜像（`SecurityRollback::Serialize` 的 12 个字段）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LegacySavepoint {
    /// `processId`。
    pub process_id: u32,
    /// `priorityClass`（原始 Win32 常量；`0` = 未知）。
    pub priority_class: u32,
    /// `affinityMask`（处理器组 0；`0` = 未知）。
    pub affinity_mask: u64,
    /// `workingSetMin`（字节）。
    pub working_set_min: u64,
    /// `workingSetMax`（字节）。
    pub working_set_max: u64,
    /// `hasProcessState`：优先级/亲和性是否成功读取。
    pub has_process_state: bool,
    /// `hasWorkingSet`：工作集是否成功读取。
    pub has_working_set: bool,
    /// `hasPowerScheme`：电源方案是否成功读取。
    pub has_power_scheme: bool,
    /// `powerSchemeGuid`（文本形式）。
    pub power_scheme_guid: String,
    /// `timestampMs`（允许负值）。
    pub timestamp_ms: i64,
    /// `gameName`。
    pub game_name: String,
    /// `description`。
    pub description: String,
}

/// C++ `gopt::GameLaunchConfig` + `gameIndex` 的镜像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyGameConfig {
    /// `gameIndex`（可为负；负值在 C++ 里会被忽略）。
    pub index: i64,
    /// `exePath`。
    pub exe_path: String,
    /// `args`。
    pub args: String,
    /// `optimizedOnLaunch`。
    pub optimized_on_launch: bool,
    /// `powerScheme`。
    pub power_scheme: bool,
    /// `frameLatency`。
    pub frame_latency: bool,
    /// `workingSet`。
    pub working_set: bool,
}

/// 单个旧格式文件的读取报告。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceReport {
    /// 文件路径（文本形式）。
    pub path: String,
    /// 打开时是否存在。
    pub exists: bool,
    /// 文件字节数。
    pub bytes: u64,
    /// 成功解析并转成 journal 记录的条数。
    pub valid_lines: usize,
    /// 无法解析、被跳过的行数。
    pub bad_lines: usize,
    /// 空行数（跳过但不算坏行）。
    pub blank_lines: usize,
    /// 被 C++ 语义静默忽略的行数（`games.conf` 的 `index < 0`）。
    pub ignored_lines: usize,
    /// 降级/提示说明（英文，稳定）。
    pub notes: Vec<String>,
}

impl SourceReport {
    /// 新建（仅路径已知）。
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.display().to_string(),
            ..Self::default()
        }
    }

    /// 一行英文摘要。
    pub fn summary(&self) -> String {
        format!(
            "{}: exists={} bytes={} valid={} bad={} blank={} ignored={}",
            self.path,
            self.exists,
            self.bytes,
            self.valid_lines,
            self.bad_lines,
            self.blank_lines,
            self.ignored_lines
        )
    }

    /// 文件是否读到了内容。
    pub const fn is_readable(&self) -> bool {
        self.exists
    }
}

/// 一次旧格式导入的结果：待入链的草稿 + 两个来源的报告。
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyImport {
    drafts: Vec<JournalDraft>,
    savepoints: SourceReport,
    games_conf: SourceReport,
    policy_filtered: usize,
}

impl LegacyImport {
    /// 待入链的草稿（`kind = imported`，保留原时间戳）。
    pub fn drafts(&self) -> &[JournalDraft] {
        &self.drafts
    }

    /// 取走草稿。
    pub fn into_drafts(self) -> Vec<JournalDraft> {
        self.drafts
    }

    /// `savepoints.txt` 的读取报告。
    pub const fn savepoints(&self) -> &SourceReport {
        &self.savepoints
    }

    /// `games.conf` 的读取报告。
    pub const fn games_conf(&self) -> &SourceReport {
        &self.games_conf
    }

    /// 因安全红线被丢弃的旧优先级值个数（`REALTIME_PRIORITY_CLASS`）。
    pub const fn policy_filtered(&self) -> usize {
        self.policy_filtered
    }

    /// 是否没有导入任何记录。
    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }

    /// 记录条数。
    pub fn len(&self) -> usize {
        self.drafts.len()
    }

    /// 两个来源的说明汇总。
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        notes.extend(self.savepoints.notes.iter().cloned());
        notes.extend(self.games_conf.notes.iter().cloned());
        notes
    }

    /// 一行英文摘要（CLI 文本输出/审计说明用）。
    pub fn summary(&self) -> String {
        format!(
            "legacy import: {} record(s) from savepoints.txt ({} bad line(s) skipped) \
             and games.conf ({} bad line(s) skipped, {} ignored)",
            self.drafts.len(),
            self.savepoints.bad_lines,
            self.games_conf.bad_lines,
            self.games_conf.ignored_lines,
        )
    }
}

impl Journal {
    /// 把旧格式导入的草稿追加进链（`kind = imported`，保留原时间戳）。
    pub fn append_legacy_import(
        &mut self,
        import: &LegacyImport,
    ) -> JournalResult<Vec<JournalRecord>> {
        self.append_all(import.drafts())
    }
}

/// 解析一行 `savepoints.txt`。
///
/// 返回 `Err(稳定英文原因)` 表示该行不可解析（调用方负责计数跳过），**绝不 panic**。
pub fn parse_savepoint_line(line: &str) -> Result<LegacySavepoint, String> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let fields: Vec<&str> = line.split('|').collect();
    if fields.len() < 12 {
        return Err(format!(
            "expected 12 '|'-separated fields, found {}",
            fields.len()
        ));
    }

    let process_id = parse_unsigned_field(fields[0], u32::MAX as u64, "processId")?;
    let priority_class = parse_unsigned_field(fields[1], u32::MAX as u64, "priorityClass")?;
    let affinity_mask = parse_unsigned_field(fields[2], u64::MAX, "affinityMask")?;
    let working_set_min = parse_unsigned_field(fields[3], u64::MAX, "workingSetMin")?;
    let working_set_max = parse_unsigned_field(fields[4], u64::MAX, "workingSetMax")?;
    let timestamp_ms = parse_timestamp_field(fields[9])?;

    Ok(LegacySavepoint {
        process_id: process_id as u32,
        priority_class: priority_class as u32,
        affinity_mask,
        working_set_min,
        working_set_max,
        // C++ 只在字段恰好等于 "1" 时视为 true。
        has_process_state: fields[5] == "1",
        has_working_set: fields[6] == "1",
        has_power_scheme: fields[7] == "1",
        power_scheme_guid: fields[8].to_string(),
        timestamp_ms,
        game_name: fields[10].to_string(),
        // 第 12 个字段之后的剩余字段被 C++ 忽略，这里保持一致（整行仍会进 `legacy.raw`）。
        description: fields[11].to_string(),
    })
}

/// 解析一行 `games.conf`。
pub fn parse_game_conf_line(line: &str) -> Result<LegacyGameConfig, String> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let fields: Vec<&str> = line.split('|').collect();
    if fields.len() < 4 {
        return Err(format!(
            "expected 4 '|'-separated fields, found {}",
            fields.len()
        ));
    }
    let index = parse_leading_i64(fields[0])
        .ok_or_else(|| format!("gameIndex `{}` is not an integer", fields[0]))?;
    let flags = fields[3];
    // 字段不足 4 个字符 ⇒ 保持 `GameLaunchConfig` 的默认值（与 C++ 一致，注意默认值是
    // optimizedOnLaunch=true / workingSet=true，不是全 false）。
    let (optimized_on_launch, power_scheme, frame_latency, working_set) = if flags.len() >= 4 {
        let mut bits = [false; 4];
        for (index, byte) in flags.bytes().take(4).enumerate() {
            bits[index] = byte == b'1';
        }
        (bits[0], bits[1], bits[2], bits[3])
    } else {
        (true, false, false, true)
    };
    Ok(LegacyGameConfig {
        index,
        exe_path: fields[1].to_string(),
        args: fields[2].to_string(),
        optimized_on_launch,
        power_scheme,
        frame_latency,
        working_set,
    })
}

/// 只读导入默认目录（`%LOCALAPPDATA%\GameOptimizer`）下的两个旧格式文件。
pub fn import_legacy_default() -> LegacyImport {
    let dir = Journal::default_dir();
    import_legacy(
        &dir.join(SAVEPOINTS_FILE_NAME),
        Some(&dir.join(GAMES_CONF_FILE_NAME)),
    )
}

/// 只读导入指定的旧格式文件；`games_conf = None` 表示不导入第二个来源。
///
/// 永不返回 `Err`：文件不存在/不可读/是目录/含二进制垃圾都降级成报告里的 `notes`，
/// 坏行只计数跳过。
pub fn import_legacy(savepoints: &Path, games_conf: Option<&Path>) -> LegacyImport {
    let mut drafts = Vec::new();
    let mut policy_filtered = 0usize;
    let savepoint_report = import_savepoints(savepoints, &mut drafts, &mut policy_filtered);
    let games_conf_report = match games_conf {
        Some(path) => import_games_conf(path, &mut drafts),
        None => SourceReport {
            notes: vec!["games.conf import was not requested".to_string()],
            ..SourceReport::default()
        },
    };
    LegacyImport {
        drafts,
        savepoints: savepoint_report,
        games_conf: games_conf_report,
        policy_filtered,
    }
}

/// `savepoints.txt` → 草稿（`kind = imported`）。
fn import_savepoints(
    path: &Path,
    drafts: &mut Vec<JournalDraft>,
    policy_filtered: &mut usize,
) -> SourceReport {
    let mut report = SourceReport::new(path);
    let bytes = match read_legacy_file(path, &mut report) {
        Some(bytes) => bytes,
        None => return report,
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut notes: Vec<String> = Vec::new();

    for (offset, raw_line) in text_lines(&text).into_iter().enumerate() {
        let line_no = offset + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.trim().is_empty() {
            report.blank_lines += 1;
            continue;
        }
        match parse_savepoint_line(line) {
            Ok(savepoint) => {
                report.valid_lines += 1;
                let mut local_notes: Vec<String> = Vec::new();
                let draft = savepoint_draft(
                    &savepoint,
                    report.valid_lines,
                    line,
                    &mut local_notes,
                    policy_filtered,
                );
                notes.extend(local_notes);
                drafts.push(draft);
            }
            Err(reason) => {
                report.bad_lines += 1;
                if report.bad_lines <= 3 {
                    notes.push(format!("line {line_no} skipped: {reason}"));
                }
            }
        }
    }

    if report.bad_lines > 3 {
        notes.push(format!(
            "{} more malformed line(s) were skipped",
            report.bad_lines - 3
        ));
    }
    if *policy_filtered > 0 {
        notes.push(format!(
            "{policy_filtered} savepoint(s) carried REALTIME_PRIORITY_CLASS (0x100); \
             the value was dropped by policy and recorded only as `priority_raw_blocked`"
        ));
    }
    notes.extend(report.notes);
    report.notes = notes;
    report
}

/// `games.conf` → 草稿（`kind = imported`）。
fn import_games_conf(path: &Path, drafts: &mut Vec<JournalDraft>) -> SourceReport {
    let mut report = SourceReport::new(path);
    let bytes = match read_legacy_file(path, &mut report) {
        Some(bytes) => bytes,
        None => return report,
    };
    let text = String::from_utf8_lossy(&bytes);
    let timestamp = file_mtime_ms(path);
    let mut notes: Vec<String> = Vec::new();
    let mut seen: Vec<i64> = Vec::new();
    let mut duplicates = 0usize;

    for (offset, raw_line) in text_lines(&text).into_iter().enumerate() {
        let line_no = offset + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.trim().is_empty() {
            report.blank_lines += 1;
            continue;
        }
        match parse_game_conf_line(line) {
            Ok(config) => {
                if config.index < 0 {
                    // 与 C++ 一致：负索引的行被忽略（不写入配置）。
                    report.ignored_lines += 1;
                    continue;
                }
                if seen.contains(&config.index) {
                    duplicates += 1;
                }
                seen.push(config.index);
                report.valid_lines += 1;
                drafts.push(game_conf_draft(&config, line_no, line, timestamp));
            }
            Err(reason) => {
                report.bad_lines += 1;
                if report.bad_lines <= 3 {
                    notes.push(format!("line {line_no} skipped: {reason}"));
                }
            }
        }
    }

    if report.bad_lines > 3 {
        notes.push(format!(
            "{} more malformed line(s) were skipped",
            report.bad_lines - 3
        ));
    }
    if duplicates > 0 {
        notes.push(format!(
            "{duplicates} duplicated game index(es) found; the C++ loader keeps only the last \
             one, every line is kept here for the audit trail"
        ));
    }
    notes.extend(report.notes);
    report.notes = notes;
    report
}

/// 单条 savepoint → journal 草稿。
fn savepoint_draft(
    savepoint: &LegacySavepoint,
    index: usize,
    raw_line: &str,
    notes: &mut Vec<String>,
    policy_filtered: &mut usize,
) -> JournalDraft {
    let mut payload = serde_json::Map::new();
    payload.insert("pid".to_string(), json!(savepoint.process_id));
    let mut restorable = false;

    if savepoint.has_process_state {
        if savepoint.priority_class == 0 {
            notes.push(format!(
                "savepoint #{index}: priority class is unknown (0) and was not imported"
            ));
        } else {
            match PriorityClass::from_raw(savepoint.priority_class) {
                Ok(class) => {
                    payload.insert("priority".to_string(), json!(class.as_str()));
                    restorable = true;
                }
                Err(err) => {
                    // 红线：REALTIME 绝不进可回滚载荷；只留原始值便于 explain。
                    *policy_filtered += 1;
                    payload.insert(
                        "priority_raw_blocked".to_string(),
                        json!(savepoint.priority_class),
                    );
                    notes.push(format!(
                        "savepoint #{index}: priority 0x{:x} was dropped ({})",
                        savepoint.priority_class,
                        err.message()
                    ));
                }
            }
        }
        if savepoint.affinity_mask == 0 {
            notes.push(format!(
                "savepoint #{index}: affinity mask is unknown (0) and was not imported"
            ));
        } else {
            payload.insert(
                "affinity_mask".to_string(),
                json!(format!("0x{:016x}", savepoint.affinity_mask)),
            );
            restorable = true;
        }
    } else {
        notes.push(format!(
            "savepoint #{index}: the C++ run did not capture process state; priority/affinity are unavailable"
        ));
    }

    if savepoint.has_working_set {
        match WorkingSetLimits::new(savepoint.working_set_min, savepoint.working_set_max) {
            Ok(limits) => {
                payload.insert("min_bytes".to_string(), json!(limits.min_bytes));
                payload.insert("max_bytes".to_string(), json!(limits.max_bytes));
                restorable = true;
            }
            Err(err) => notes.push(format!(
                "savepoint #{index}: working set limits were dropped ({})",
                err.message()
            )),
        }
    } else {
        notes.push(format!(
            "savepoint #{index}: the C++ run did not capture the working set"
        ));
    }

    if savepoint.has_power_scheme && !savepoint.power_scheme_guid.is_empty() {
        if savepoint
            .power_scheme_guid
            .parse::<gopt_hal::Guid>()
            .is_ok()
        {
            payload.insert("guid".to_string(), json!(savepoint.power_scheme_guid));
            restorable = true;
        } else {
            notes.push(format!(
                "savepoint #{index}: power scheme `{}` is not a GUID and was dropped",
                savepoint.power_scheme_guid
            ));
        }
    }

    let mut legacy = serde_json::Map::new();
    legacy.insert("source".to_string(), json!(SAVEPOINTS_FILE_NAME));
    legacy.insert("index".to_string(), json!(index));
    legacy.insert("game_name".to_string(), json!(savepoint.game_name));
    legacy.insert("description".to_string(), json!(savepoint.description));
    legacy.insert(
        "has_process_state".to_string(),
        json!(savepoint.has_process_state),
    );
    legacy.insert(
        "has_working_set".to_string(),
        json!(savepoint.has_working_set),
    );
    legacy.insert(
        "has_power_scheme".to_string(),
        json!(savepoint.has_power_scheme),
    );
    legacy.insert("timestamp_ms".to_string(), json!(savepoint.timestamp_ms));
    legacy.insert("raw".to_string(), json!(raw_line));
    if !restorable {
        legacy.insert(
            "note".to_string(),
            json!("this legacy savepoint carries no restorable field"),
        );
    }
    payload.insert("legacy".to_string(), Value::Object(legacy));

    JournalDraft::at(
        savepoint.timestamp_ms,
        JournalKind::Imported,
        payload::pid_target(savepoint.process_id),
    )
    .with_values(Some(Value::Object(payload)), None)
    .with_rule_id(LEGACY_SAVEPOINTS_RULE_ID)
}

/// 单条 `games.conf` 条目 → journal 草稿（启动偏好，没有可回滚的系统状态）。
fn game_conf_draft(
    config: &LegacyGameConfig,
    line_no: usize,
    raw_line: &str,
    timestamp_ms: i64,
) -> JournalDraft {
    let payload = json!({
        "legacy_index": config.index,
        "exe_path": config.exe_path,
        "args": config.args,
        "optimized_on_launch": config.optimized_on_launch,
        "power_scheme": config.power_scheme,
        "frame_latency": config.frame_latency,
        "working_set": config.working_set,
        "legacy": {
            "source": GAMES_CONF_FILE_NAME,
            "line_no": line_no,
            "raw": raw_line,
            "note": "games.conf entries are launch preferences, not live system state; \
                     there is nothing to restore",
        },
    });
    JournalDraft::at(
        timestamp_ms,
        JournalKind::Imported,
        payload::game_target(config.index),
    )
    .with_values(Some(payload), None)
    .with_rule_id(LEGACY_GAMES_CONF_RULE_ID)
}

/// 只读读取旧格式文件；失败时把原因写进 `report.notes`（不返回 `Err`）。
fn read_legacy_file(path: &Path, report: &mut SourceReport) -> Option<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => {
            report.exists = true;
            report.bytes = bytes.len() as u64;
            if std::str::from_utf8(&bytes).is_err() {
                report
                    .notes
                    .push("the file is not valid UTF-8; it was decoded lossily (bad lines are counted as malformed)".to_string());
            }
            Some(bytes)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            report.notes.push(
                "the file does not exist (never optimized with the C++ release?)".to_string(),
            );
            None
        }
        Err(err) => {
            report.notes.push(format!("the file cannot be read: {err}"));
            None
        }
    }
}

/// 按 `\n` 切行；末尾换行不产生额外空行。
fn text_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// 文件最后修改时间（毫秒）；不可得时为 `0`。
fn file_mtime_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|delta| i64::try_from(delta.as_millis()).ok())
        .unwrap_or(0)
}

/// 与 C++ `ParseField` 的非负分支对齐：十进制优先、整串消费、失败退回十六进制、上限检查。
fn parse_unsigned_field(text: &str, max: u64, field: &str) -> Result<u64, String> {
    // `strtoull` 会跳过前导空白；Rust 的 `parse` 不会，这里显式对齐。
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Err(format!("`{field}` is empty"));
    }
    if trimmed.starts_with('-') {
        return Err(format!("`{field}` is negative (`{text}`)"));
    }
    if let Ok(value) = trimmed.parse::<u64>() {
        return check_range(value, max, field, text);
    }
    let without_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if let Ok(value) = u64::from_str_radix(without_prefix, 16) {
        return check_range(value, max, field, text);
    }
    Err(format!("`{field}` is not a number (`{text}`)"))
}

/// 上限检查（C++ 里超限直接判负，不再尝试别的进制）。
fn check_range(value: u64, max: u64, field: &str, original: &str) -> Result<u64, String> {
    if value > max {
        return Err(format!("`{field}` = {original} exceeds {max}"));
    }
    Ok(value)
}

/// 与 C++ 的时间戳字段对齐：只用十进制、整串消费、允许负值。
fn parse_timestamp_field(text: &str) -> Result<i64, String> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Err("`timestampMs` is empty".to_string());
    }
    trimmed
        .parse::<i64>()
        .map_err(|_| format!("`timestampMs` is not a decimal integer (`{text}`)"))
}

/// 与 `std::stoi` 的宽松语义对齐：跳过前导空白，取最长的整数前缀（含正负号）。
fn parse_leading_i64(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let end = digits
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map_or(digits.len(), |(index, _)| index);
    if end == 0 {
        return None;
    }
    let value = digits.get(..end)?.parse::<i64>().ok()?;
    Some(sign * value)
}

/// 旧格式默认目录下的两个文件路径（供 CLI 展示；不创建目录）。
pub fn legacy_paths() -> (PathBuf, PathBuf) {
    let dir = Journal::default_dir();
    (
        dir.join(SAVEPOINTS_FILE_NAME),
        dir.join(GAMES_CONF_FILE_NAME),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "4242|32|0x000000000000000f|536870912|2147483648|1|1|1|\
                         381b4222-f694-41f0-9685-ff5bb260df2e|1700000000000|cs2.exe|optimized run";

    #[test]
    fn valid_line_parses_field_by_field() {
        let savepoint = parse_savepoint_line(VALID).expect("valid line");
        assert_eq!(savepoint.process_id, 4242);
        assert_eq!(savepoint.priority_class, 32);
        assert_eq!(savepoint.affinity_mask, 0xf);
        assert_eq!(savepoint.working_set_min, 536_870_912);
        assert_eq!(savepoint.working_set_max, 2_147_483_648);
        assert!(savepoint.has_process_state);
        assert!(savepoint.has_working_set);
        assert!(savepoint.has_power_scheme);
        assert_eq!(
            savepoint.power_scheme_guid,
            "381b4222-f694-41f0-9685-ff5bb260df2e"
        );
        assert_eq!(savepoint.timestamp_ms, 1_700_000_000_000);
        assert_eq!(savepoint.game_name, "cs2.exe");
        assert_eq!(savepoint.description, "optimized run");
    }

    #[test]
    fn parse_mirrors_cpp_edge_cases() {
        // 十进制 32 先被接受（不会退到十六进制解释成 50）。
        assert_eq!(parse_unsigned_field("32", u64::MAX, "f").expect("dec"), 32);
        // 前导空白：strtoull 会跳过，Rust 的 parse 不会 ⇒ 必须显式对齐。
        assert_eq!(parse_unsigned_field(" 42", u64::MAX, "f").expect("ws"), 42);
        // 前导 0x：十进制失败后回退十六进制。
        assert_eq!(
            parse_unsigned_field("0x10", u64::MAX, "f").expect("hex"),
            16
        );
        // 负数判负（C++ 显式拒绝回绕）。
        assert!(parse_unsigned_field("-5", u64::MAX, "f").is_err());
        // 尾部残留字符判负。
        assert!(parse_unsigned_field("5x", u64::MAX, "f").is_err());
        assert!(parse_unsigned_field("", u64::MAX, "f").is_err());
        // 超出字段范围判负（C++ 用 maxValue 拦住 stoull 回绕）。
        assert!(parse_unsigned_field("4294967296", u32::MAX as u64, "processId").is_err());
        assert_eq!(
            parse_unsigned_field("4294967295", u32::MAX as u64, "processId").expect("max"),
            4_294_967_295
        );
        // 超长数字串不会 panic，也不会回绕成垃圾值。
        assert!(parse_unsigned_field(&"9".repeat(5000), u64::MAX, "f").is_err());
        // 时间戳：允许负值，但只认十进制。
        assert_eq!(parse_timestamp_field("-1").expect("negative"), -1);
        assert!(parse_timestamp_field("0x10").is_err());
    }

    #[test]
    fn malformed_lines_are_rejected_without_panicking() {
        assert!(parse_savepoint_line("").is_err());
        assert!(parse_savepoint_line("1|2|3").is_err());
        // 字段不足 12 ⇒ 判负。
        assert!(parse_savepoint_line("1|2|3|4|5|6|7|8|guid|0|game").is_err());
        // 负数 PID ⇒ 判负。
        assert!(parse_savepoint_line(&VALID.replace("4242|32", "-4242|32")).is_err());
        // 时间戳非十进制 ⇒ 判负。
        let bad_ts = VALID.replace("1700000000000", "17e12");
        assert!(parse_savepoint_line(&bad_ts).is_err());
        // 第 12 个字段之后的剩余字段被忽略（C++ 只读 12 次 getline）。
        let trailing = parse_savepoint_line(&format!("{VALID}|extra|fields")).expect("trailing");
        assert_eq!(trailing.description, "optimized run");
        // CRLF 容忍。
        assert!(parse_savepoint_line(&format!("{VALID}\r")).is_ok());
        // 非 "1" 的布尔字段一律 false。
        let zero_flags =
            parse_savepoint_line("1|32|0|0|0|2|00|yes|guid|0|game|desc").expect("flags");
        assert!(!zero_flags.has_process_state);
        assert!(!zero_flags.has_working_set);
        assert!(!zero_flags.has_power_scheme);
    }

    #[test]
    fn game_conf_lines_mirror_the_cpp_loader() {
        let config = parse_game_conf_line("0|C:\\games\\cs2.exe|--novid|1001").expect("valid");
        assert_eq!(config.index, 0);
        assert_eq!(config.exe_path, "C:\\games\\cs2.exe");
        assert_eq!(config.args, "--novid");
        assert!(config.optimized_on_launch);
        assert!(!config.power_scheme);
        assert!(!config.frame_latency);
        assert!(config.working_set);

        // 第 4 个字段不足 4 个字符 ⇒ 结构体默认值。
        let short = parse_game_conf_line("3|exe|args|1").expect("short flags");
        assert!(short.optimized_on_launch);
        assert!(!short.power_scheme);
        assert!(short.working_set);

        // 尾部残留字符：C++ 的 stoi 不看 pos ⇒ 取整数前缀。
        assert_eq!(
            parse_game_conf_line("3abc|exe|args|1111")
                .expect("lenient")
                .index,
            3
        );
        assert_eq!(
            parse_game_conf_line("-1|exe|args|1111")
                .expect("negative")
                .index,
            -1
        );
        // C++ 这里会抛异常（未捕获）⇒ Rust 版必须降级成坏行。
        assert!(parse_game_conf_line("abc|exe|args|1111").is_err());
        assert!(parse_game_conf_line("0|exe|args").is_err());
        assert!(parse_game_conf_line("").is_err());
        assert!(parse_game_conf_line(&format!("{}|exe|args|1111", "9".repeat(5000))).is_err());
    }

    #[test]
    fn savepoint_draft_carries_provenance_and_red_lines() {
        let mut notes = Vec::new();
        let mut filtered = 0usize;
        let savepoint = parse_savepoint_line(VALID).expect("valid");
        let draft = savepoint_draft(&savepoint, 1, VALID, &mut notes, &mut filtered);
        assert_eq!(draft.kind, JournalKind::Imported);
        assert_eq!(draft.ts_unix_ms, 1_700_000_000_000);
        assert_eq!(draft.target, "pid:4242");
        assert_eq!(draft.rule_id.as_deref(), Some(LEGACY_SAVEPOINTS_RULE_ID));
        assert_eq!(filtered, 0);
        let before = draft.before.clone().expect("before");
        assert_eq!(before["priority"], "normal");
        assert_eq!(before["affinity_mask"], "0x000000000000000f");
        assert_eq!(before["min_bytes"].as_u64(), Some(536_870_912));
        assert_eq!(before["max_bytes"].as_u64(), Some(2_147_483_648));
        assert_eq!(before["legacy"]["raw"], VALID);
        assert_eq!(before["legacy"]["index"].as_u64(), Some(1));

        // REALTIME 只在 `priority_raw_blocked` 里留痕，绝不进可回滚字段。
        let realtime = VALID.replace("4242|32", "4242|256");
        let savepoint = parse_savepoint_line(&realtime).expect("realtime line");
        let draft = savepoint_draft(&savepoint, 2, &realtime, &mut notes, &mut filtered);
        let before = draft.before.clone().expect("before");
        assert!(before.get("priority").is_none());
        assert_eq!(before["priority_raw_blocked"].as_u64(), Some(256));
        assert_eq!(filtered, 1);
        assert!(notes.iter().any(|note| note.contains("0x100")));
    }

    #[test]
    fn text_lines_handles_trailing_newline() {
        assert_eq!(text_lines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(text_lines("a\nb"), vec!["a", "b"]);
        assert!(text_lines("").is_empty());
        assert_eq!(text_lines("\n"), vec![""]);
    }
}
