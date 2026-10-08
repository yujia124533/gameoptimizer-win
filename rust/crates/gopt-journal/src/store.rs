//! 日志文件：追加写 + `fsync`、崩溃安全的尾部半行修复、原子替换。
//!
//! 文件位置：`%LOCALAPPDATA%\GameOptimizer\journal.jsonl`（无 `LOCALAPPDATA` 时退化为
//! 当前目录下的 `GameOptimizer\journal.jsonl`，与 C++ 版 `SecurityRollback::SaveFileDirW`
//! 的策略一致）。
//!
//! # 崩溃安全（JSONL 追加日志的标准做法）
//!
//! * **提交路径**：把若干条记录的规范行拼成一个缓冲区，**一次 `write_all`** 写入，然后
//!   `flush` + `sync_data`（Windows 上是 `FlushFileBuffers`）。返回时数据已在盘上。
//! * **崩溃残留**：进程在 `write_all` 中途被杀，文件尾部可能留下半行。**打开时**
//!   `Journal::open`（默认 [`JournalOptions::repair_torn_tail`]）会把"最后一个换行符之后的
//!   残余字节"截掉——无论它看起来是否像一条完整记录。因为追加永远以 `\n` 结束，
//!   "不以 `\n` 结尾的尾部"只可能是崩溃残留，留着它会让下一条记录和它粘在一行。
//! * **诊断路径**：只想读不想改的调用方用 [`JournalOptions::read_only`]，此时尾部残留
//!   不会被截断，而是保留成一条 `terminated = false` 的行，校验报告会明确报出
//!   [`crate::ChainProblem::MalformedLine`]，且**拒绝追加**。
//! * **不把新记录接在坏链上**：打开时的校验结果不通过 ⇒ [`Journal::append`] 直接返回
//!   [`crate::JournalErrorKind::ChainBroken`]；另外每次追加前比对文件长度，
//!   发现日志被外部改动就返回 [`crate::JournalErrorKind::Stale`]（要求重新打开）。
//! * **原子替换**：[`Journal::rewrite_atomic`] / [`Journal::rewrite_atomic_with`] 走
//!   "临时文件 → `sync_all` → `rename`"，用于日志压缩/截断；提供的记录必须先通过链校验，
//!   否则拒绝写入（不允许静默丢记录）。

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::chain::{self, ChainAnchor, ChainReport, JournalLine};
use crate::error::{JournalError, JournalResult};
use crate::record::{JournalDraft, JournalRecord};
use crate::rollback::{self, RollbackPlan};

/// 日志文件名。
pub const JOURNAL_FILE_NAME: &str = "journal.jsonl";

/// 数据目录名（与 C++ 版一致）。
pub const DATA_DIR_NAME: &str = "GameOptimizer";

/// 打开日志的选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalOptions {
    /// 打开时截断"尾部半行"（崩溃残留）。默认开启。
    pub repair_torn_tail: bool,
    /// 每次追加后 `fsync`。默认开启（审计日志不允许"返回成功但没落盘"）。
    pub fsync: bool,
}

impl JournalOptions {
    /// 默认：修复尾部半行 + 每次追加 `fsync`。
    pub const fn new() -> Self {
        Self {
            repair_torn_tail: true,
            fsync: true,
        }
    }

    /// 只读诊断：不修改文件（不截断、不 fsync）——用于校验/展示。
    pub const fn read_only() -> Self {
        Self {
            repair_torn_tail: false,
            fsync: false,
        }
    }
}

impl Default for JournalOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// 尾部半行的修复记录（打开日志时截断掉的崩溃残留）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TailRepair {
    /// 截断掉的字节数。
    pub bytes_truncated: u64,
    /// 被丢弃的原始字节（`UTF-8` 有损解码，仅用于诊断展示）。
    pub raw: String,
}

/// 哈希链审计日志（单写者）。
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    options: JournalOptions,
    lines: Vec<JournalLine>,
    file_exists: bool,
    /// 打开时的文件长度，用于检出"外部改写"。
    len_on_open: u64,
    skipped_blank_lines: usize,
    tail_repair: Option<TailRepair>,
    verification_on_open: ChainReport,
}

impl Journal {
    /// 打开（默认选项）：不存在的文件视为空日志。
    pub fn open(path: impl AsRef<Path>) -> JournalResult<Self> {
        Self::open_with(path, JournalOptions::new())
    }

    /// 打开默认路径 `%LOCALAPPDATA%\GameOptimizer\journal.jsonl`。
    pub fn open_default() -> JournalResult<Self> {
        Self::open(Self::default_path())
    }

    /// 按指定选项打开。
    ///
    /// 只有[尾部半行修复](JournalOptions::repair_torn_tail)会写文件，且只做"截短"；
    /// 绝不创建目录、绝不改写已提交的行。
    pub fn open_with(path: impl AsRef<Path>, options: JournalOptions) -> JournalResult<Self> {
        let path = path.as_ref().to_path_buf();
        let bytes = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(JournalError::io("fs::read", &path, &err)),
        };

        let mut journal = Self {
            path,
            options,
            lines: Vec::new(),
            file_exists: false,
            len_on_open: 0,
            skipped_blank_lines: 0,
            tail_repair: None,
            verification_on_open: ChainReport {
                total: 0,
                verified: 0,
                first_inconsistency: None,
                anchored: false,
            },
        };

        if let Some(bytes) = bytes {
            journal.len_on_open = bytes.len() as u64;
            journal.file_exists = true;
            journal.ingest(&bytes)?;
        }
        journal.verification_on_open = chain::verify_lines(&journal.lines, None);
        Ok(journal)
    }

    /// 默认数据目录：`%LOCALAPPDATA%\GameOptimizer`（只计算路径，不创建目录）。
    pub fn default_dir() -> PathBuf {
        match std::env::var_os("LOCALAPPDATA") {
            Some(value) if !value.is_empty() => PathBuf::from(value).join(DATA_DIR_NAME),
            _ => std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(DATA_DIR_NAME),
        }
    }

    /// 默认日志路径：`%LOCALAPPDATA%\GameOptimizer\journal.jsonl`。
    pub fn default_path() -> PathBuf {
        Self::default_dir().join(JOURNAL_FILE_NAME)
    }

    /// 日志文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 打开选项。
    pub const fn options(&self) -> JournalOptions {
        self.options
    }

    /// 文件是否已存在（打开时判定）。
    pub const fn file_exists(&self) -> bool {
        self.file_exists
    }

    /// 已读入的日志行（含无法解析的行；空行与已修复的尾部半行不在其中）。
    pub fn lines(&self) -> &[JournalLine] {
        &self.lines
    }

    /// 已读入的行数（含无法解析的行）。
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// 成功的记录（无法解析的行被跳过）。
    pub fn records(&self) -> impl Iterator<Item = &JournalRecord> {
        self.lines.iter().filter_map(|line| line.parsed.as_ref())
    }

    /// 记录条数。
    pub fn record_count(&self) -> usize {
        self.records().count()
    }

    /// 记录副本（供需要所有权的调用方，例如回滚计划生成）。
    pub fn records_owned(&self) -> Vec<JournalRecord> {
        self.records().cloned().collect()
    }

    /// 被跳过的空行数（打开时的统计）。
    pub const fn skipped_blank_lines(&self) -> usize {
        self.skipped_blank_lines
    }

    /// 打开时修复的尾部半行（若有）。
    pub fn tail_repair(&self) -> Option<&TailRepair> {
        self.tail_repair.as_ref()
    }

    /// 无法解析的行。
    pub fn malformed_lines(&self) -> impl Iterator<Item = &JournalLine> {
        self.lines.iter().filter(|line| line.parsed.is_none())
    }

    /// 打开时做的那次链校验（快照；`verify_chain` 会重新计算）。
    pub const fn verification_on_open(&self) -> &ChainReport {
        &self.verification_on_open
    }

    /// 打开时链是否完好——[`Journal::append`] 用这个结果把关。
    pub fn is_chain_healthy(&self) -> bool {
        self.verification_on_open.is_ok()
    }

    /// 链上最后一条记录的哈希；空日志返回创世哈希。
    pub fn last_hash(&self) -> String {
        self.lines
            .iter()
            .rev()
            .find_map(|line| line.parsed.as_ref())
            .map_or_else(
                || crate::canonical::GENESIS_HASH.to_string(),
                |r| r.hash.clone(),
            )
    }

    /// 下一条记录的 id（从 1 起）。
    pub fn next_id(&self) -> u64 {
        self.lines
            .iter()
            .rev()
            .find_map(|line| line.parsed.as_ref())
            .map_or(1, |record| record.id.saturating_add(1))
    }

    /// 重新校验哈希链（不使用外部锚点）。
    pub fn verify_chain(&self) -> ChainReport {
        chain::verify_lines(&self.lines, None)
    }

    /// 用外部锚点校验：能额外检出"尾部记录被删除"与"前缀被替换"。
    pub fn verify_chain_with_anchor(&self, anchor: &ChainAnchor) -> ChainReport {
        chain::verify_lines(&self.lines, Some(anchor))
    }

    /// 生成当前链的锚点（把链"钉"在日志文件之外）。
    pub fn anchor(&self) -> ChainAnchor {
        ChainAnchor {
            len: self.record_count() as u64,
            last_hash: self.last_hash(),
        }
    }

    /// 生成"回滚到 `to_id`（含）"的逆序撤销计划；链不可信时拒绝生成。
    pub fn plan_rollback(&self, to_id: u64) -> JournalResult<RollbackPlan> {
        self.ensure_chain_trusted()?;
        rollback::plan_rollback(&self.records_owned(), to_id)
    }

    /// 生成"撤销全部可逆记录"的计划。
    pub fn plan_rollback_all(&self) -> JournalResult<RollbackPlan> {
        self.ensure_chain_trusted()?;
        rollback::plan_rollback_all(&self.records_owned())
    }

    /// 追加一条记录（返回入链后的完整记录）。
    pub fn append(&mut self, draft: JournalDraft) -> JournalResult<JournalRecord> {
        let record = self.prepare(draft)?;
        self.commit(std::slice::from_ref(&record))?;
        Ok(record)
    }

    /// 批量追加（一次 `write_all` + 一次 `fsync`，用于导入/批量应用）。
    pub fn append_all(&mut self, drafts: &[JournalDraft]) -> JournalResult<Vec<JournalRecord>> {
        let mut prev_hash = self.last_hash();
        let mut id = self.next_id();
        let mut records = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let record = draft.clone().into_record(id, prev_hash);
            prev_hash = record.hash.clone();
            id = id.saturating_add(1);
            records.push(record);
        }
        if records.is_empty() {
            return Ok(records);
        }
        self.commit(&records)?;
        Ok(records)
    }

    /// 用当前内存中的记录原子重写整个文件（要求链完好；等价内容 → 字节不变）。
    pub fn rewrite_atomic(&mut self) -> JournalResult<()> {
        let records = self.records_owned();
        self.rewrite_atomic_with(&records)
    }

    /// 用**指定**记录集原子重写文件（临时文件 → `sync_all` → `rename`）。
    ///
    /// 供日志压缩/截断使用；`records` 必须自身构成一条合法链（id 从 1 连续、
    /// `prev_hash` 链接正确、哈希自洽），否则拒绝写入——不允许静默丢记录。
    pub fn rewrite_atomic_with(&mut self, records: &[JournalRecord]) -> JournalResult<()> {
        let report = chain::verify_chain(records);
        if !report.is_ok() {
            return Err(JournalError::chain_broken(&self.path, &report));
        }
        let mut buffer = String::with_capacity(256 * records.len());
        for record in records {
            buffer.push_str(&record.to_canonical_line());
            buffer.push('\n');
        }

        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);

        if let Err(err) = fs::write(&tmp, buffer.as_bytes()) {
            let _ = fs::remove_file(&tmp);
            return Err(JournalError::io("fs::write", &tmp, &err));
        }
        // 先让临时文件落盘，再 rename：崩溃只会留下一个 .tmp，正式文件要么是旧内容、要么是新内容。
        if let Err(err) = OpenOptions::new()
            .write(true)
            .open(&tmp)
            .and_then(|file| file.sync_all())
        {
            let _ = fs::remove_file(&tmp);
            return Err(JournalError::io("fs::File::sync_all", &tmp, &err));
        }
        if let Err(err) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(JournalError::io("fs::rename", &tmp, &err));
        }

        self.lines = records
            .iter()
            .enumerate()
            .map(|(index, record)| JournalLine::from_record(record, index + 1))
            .collect();
        self.file_exists = true;
        self.len_on_open = buffer.len() as u64;
        self.skipped_blank_lines = 0;
        self.tail_repair = None;
        self.verification_on_open = report;
        Ok(())
    }

    /// 由草稿生成入链记录（校验链状态 + 补齐 id / prev_hash / hash）。
    fn prepare(&self, draft: JournalDraft) -> JournalResult<JournalRecord> {
        self.gate()?;
        Ok(draft.into_record(self.next_id(), self.last_hash()))
    }

    /// 追加前把关：链必须完好（打开时的校验快照），且文件没有被外部改动。
    fn gate(&self) -> JournalResult<()> {
        if !self.verification_on_open.is_ok() {
            return Err(JournalError::chain_broken(
                &self.path,
                &self.verification_on_open,
            ));
        }
        match fs::metadata(&self.path) {
            Ok(meta) if meta.len() == self.len_on_open => Ok(()),
            Ok(meta) => Err(JournalError::stale(&self.path, self.len_on_open, meta.len())),
            Err(err) if err.kind() == io::ErrorKind::NotFound && !self.file_exists => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(JournalError::new(
                crate::error::JournalErrorKind::Stale,
                "Journal::append",
                format!(
                    "the journal file {} disappeared after it was opened; reopen it before appending",
                    self.path.display()
                ),
            )),
            Err(err) => Err(JournalError::io("fs::metadata", &self.path, &err)),
        }
    }

    /// 链不可信就拒绝生成回滚计划。
    fn ensure_chain_trusted(&self) -> JournalResult<()> {
        let report = self.verify_chain();
        if report.is_ok() {
            Ok(())
        } else {
            Err(JournalError::chain_broken(&self.path, &report))
        }
    }

    /// 把一批记录一次性写盘（单次 `write_all` + `fsync`）并更新内存状态。
    fn commit(&mut self, records: &[JournalRecord]) -> JournalResult<()> {
        let mut buffer = String::with_capacity(256 * records.len());
        for record in records {
            buffer.push_str(&record.to_canonical_line());
            buffer.push('\n');
        }

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|err| JournalError::io("fs::create_dir_all", parent, &err))?;
            }
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|err| JournalError::io("OpenOptions::open", &self.path, &err))?;
        // 单次 write_all：一整批记录要么整段进入页缓存，要么（中途被杀时）在尾部留下半行，
        // 由下次打开时的尾部修复处理。
        file.write_all(buffer.as_bytes())
            .map_err(|err| JournalError::io("File::write_all", &self.path, &err))?;
        file.flush()
            .map_err(|err| JournalError::io("File::flush", &self.path, &err))?;
        if self.options.fsync {
            file.sync_data()
                .map_err(|err| JournalError::io("File::sync_data", &self.path, &err))?;
        }
        drop(file);

        let base = self.lines.len() + 1;
        for (offset, record) in records.iter().enumerate() {
            self.lines
                .push(JournalLine::from_record(record, base + offset));
        }
        self.len_on_open += buffer.len() as u64;
        self.file_exists = true;
        Ok(())
    }

    /// 逐行解析文件内容；必要时截断尾部半行。
    fn ingest(&mut self, bytes: &[u8]) -> JournalResult<()> {
        let mut start = 0usize;
        let mut complete_end = 0usize;
        let mut line_no = 0usize;
        for (index, byte) in bytes.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            line_no += 1;
            self.push_line(line_no, &bytes[start..index], true);
            complete_end = index + 1;
            start = index + 1;
        }

        let tail = &bytes[start..];
        if tail.is_empty() {
            return Ok(());
        }
        if self.options.repair_torn_tail {
            truncate_file(&self.path, complete_end as u64)?;
            self.tail_repair = Some(TailRepair {
                bytes_truncated: tail.len() as u64,
                raw: String::from_utf8_lossy(tail).into_owned(),
            });
            self.len_on_open = complete_end as u64;
        } else {
            line_no += 1;
            self.push_line(line_no, tail, false);
        }
        Ok(())
    }

    /// 记录一行（空行跳过；无法解析的行保留 `parsed = None`）。
    fn push_line(&mut self, line_no: usize, bytes: &[u8], terminated: bool) {
        let raw = String::from_utf8_lossy(bytes).into_owned();
        // CRLF：`\r` 留在 `raw` 里（这样"被改成 CRLF 的行"会在规范形式检查里暴露），
        // 只在解析时忽略行尾的 `\r`。
        let parsed_text = raw.trim_end_matches('\r');
        if parsed_text.trim().is_empty() {
            self.skipped_blank_lines += 1;
            return;
        }
        // 未终止的尾部残行一律不作为记录（即使它碰巧是合法 JSON）：
        // 它没有换行结尾，无法确定是否被写全。
        let parsed = if terminated {
            serde_json::from_str::<JournalRecord>(parsed_text).ok()
        } else {
            None
        };
        self.lines.push(JournalLine {
            line_no,
            raw,
            terminated,
            parsed,
        });
    }
}

/// 把文件截断到 `len` 字节并落盘。
fn truncate_file(path: &Path, len: u64) -> JournalResult<()> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|err| JournalError::io("OpenOptions::open", path, &err))?;
    file.set_len(len)
        .map_err(|err| JournalError::io("File::set_len", path, &err))?;
    file.sync_all()
        .map_err(|err| JournalError::io("File::sync_all", path, &err))?;
    Ok(())
}
