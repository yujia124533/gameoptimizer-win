//! 内核输出模型（DTO）：CLI 的**唯一**数据来源，`--json` 直接序列化这些结构。
//!
//! 为什么单独一个模块：前端（CLI、将来的 GUI、CI 脚本）需要的数据形状必须只有一份定义，
//! 否则"人读的输出"与"`--json` 的输出"会漂移。这里所有类型都 `Serialize`，
//! 字段名即 JSON 字段名；中英双语文案一律用 `Text` / `Reason` 成对出现。

use serde::Serialize;
use serde_json::Value;

use gopt_hal::{
    HalOp, HardwareInfo, PowerScheme, PriorityClass, ProcessInfo, RunEntry, WorkingSetLimits,
};
use gopt_journal::{
    ChainAnchor, ChainBreak, ChainReport, JournalKind, JournalRecord, RollbackAction, SourceReport,
    TailRepair,
};
use gopt_policy::{Plan, PlanSkip, PolicyDiagnostic};

use crate::error::CoreError;
use crate::i18n::{Lang, Text};

// ---------------------------------------------------------------------------
// detect / status / list
// ---------------------------------------------------------------------------

/// 一款可用策略的摘要（`gopt list games`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameSummary {
    /// 策略 id。
    pub id: String,
    /// 中文名。
    pub name_zh: String,
    /// 英文名。
    pub name_en: String,
    /// 主 exe 通配模式。
    pub exe_match: String,
    /// 其它可执行文件名。
    pub exe_aliases: Vec<String>,
    /// 规则条数。
    pub rules: usize,
    /// 主要 HAL 操作（按规则顺序去重）。
    pub hal_ops: Vec<String>,
    /// 策略来源（`<builtin>/cs2.toml` 或用户文件路径）。
    pub origin: String,
    /// 来源层级：`builtin` / `user`。
    pub layer: String,
    /// 当前是否正在运行（命中进程的 pid）。
    pub running_pid: Option<u32>,
    /// 中文说明。
    pub description_zh: String,
    /// 英文说明。
    pub description_en: String,
}

/// 正在运行、且命中策略的进程（`status` 的 `running_games`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunningGame {
    /// 策略 id。
    pub game_id: String,
    /// 中文名。
    pub name_zh: String,
    /// 英文名。
    pub name_en: String,
    /// 进程 ID。
    pub pid: u32,
    /// 可执行文件名。
    pub exe: String,
    /// 计划里的可执行步骤数。
    pub steps: usize,
    /// 计划里被跳过的规则数。
    pub skipped: usize,
    /// 是否需要管理员权限。
    pub requires_elevation: bool,
}

/// `gopt status` 的结果：一眼看清"在哪台机器上、用哪份数据目录、日志链是否可信、哪些游戏在跑"。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    /// 产品名。
    pub product: String,
    /// 本构建版本。
    pub version: String,
    /// C++ 正式发布版版本。
    pub cpp_release: String,
    /// `--json` schema 版本。
    pub schema_version: u32,
    /// HAL 后端：`win32` / `mock`。
    pub backend: String,
    /// 当前进程是否已提权。
    pub elevated: bool,
    /// 输出语言。
    pub lang: Lang,
    /// 数据目录（含来源说明）。
    pub data_dir: String,
    /// 审计日志路径。
    pub journal_path: String,
    /// 审计日志是否已存在。
    pub journal_exists: bool,
    /// 已提交记录数。
    pub journal_records: usize,
    /// 链是否可信。
    pub chain_ok: bool,
    /// 链校验的一行英文摘要。
    pub chain_summary: String,
    /// 第一处不一致（若有）。
    pub first_problem: Option<ChainBreak>,
    /// 打开日志时修复的尾部残字节数（崩溃残留）。
    pub tail_repair_bytes: Option<u64>,
    /// 还有多少条记录待撤销。
    pub pending_rollbacks: usize,
    /// 已生效的策略数。
    pub policies_total: usize,
    /// 内置策略数。
    pub policies_builtin: usize,
    /// 用户策略数。
    pub policies_user: usize,
    /// 策略加载错误数。
    pub policy_errors: usize,
    /// 策略加载警告数。
    pub policy_warnings: usize,
    /// 用户策略目录。
    pub user_policy_dir: String,
    /// 硬件画像。
    pub hardware: HardwareInfo,
    /// 正在运行且命中策略的进程。
    pub running_games: Vec<RunningGame>,
    /// 策略加载诊断（错误 + 警告）。
    pub policy_diagnostics: Vec<PolicyDiagnostic>,
    /// 内核产生的提示（中英双语）。
    pub notices: Vec<Text>,
}

// ---------------------------------------------------------------------------
// plan
// ---------------------------------------------------------------------------

/// `gopt plan <游戏>` 的结果：计划本身 + 它是怎么被选出来的。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanReport {
    /// 用户输入的查询串。
    pub query: String,
    /// 是否命中一份策略。
    pub matched: bool,
    /// 目标游戏是否正在运行。
    pub running: bool,
    /// 计划（`pid = 0` 表示游戏没在运行，只是预览）。
    pub plan: Plan,
    /// 命中的进程名。
    pub process_name: Option<String>,
    /// 当前有多少个同名进程在跑。
    pub process_count: usize,
    /// 匹配到的候选（查询是模糊匹配时列出全部命中）。
    pub candidates: Vec<String>,
    /// 附加说明。
    pub notes: Vec<Text>,
}

// ---------------------------------------------------------------------------
// apply
// ---------------------------------------------------------------------------

/// 执行选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ApplyOptions {
    /// 只读预演：读取当前值、计算前后差异，但**不写系统、不写日志**。
    pub dry_run: bool,
}

impl ApplyOptions {
    /// 预演（默认安全：CLI 没有 `--yes` 时用这个）。
    pub const fn preview() -> Self {
        Self { dry_run: true }
    }

    /// 真正执行（CLI 有 `--yes` 时用这个）。
    pub const fn commit() -> Self {
        Self { dry_run: false }
    }

    /// 是否预演。
    pub const fn is_dry_run(self) -> bool {
        self.dry_run
    }
}

/// 单个步骤的执行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// 预演：只读取了当前值，没有写。
    DryRun,
    /// 已写入系统并记入审计日志。
    Applied,
    /// 已经是目标值：不写、不记（幂等）。
    Unchanged,
    /// 目标不存在且策略允许忽略。
    SkippedMissing,
    /// 该步骤没有可执行的撤销动作（例如写入前状态不可知）——不是错误，而是能力边界。
    NotActionable,
    /// 执行失败。
    Failed,
    /// 因审计日志写不进去而停手（前面的步骤仍然生效）。
    Stopped,
}

impl StepStatus {
    /// 稳定短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            StepStatus::DryRun => "dry_run",
            StepStatus::Applied => "applied",
            StepStatus::Unchanged => "unchanged",
            StepStatus::SkippedMissing => "skipped_missing",
            StepStatus::NotActionable => "not_actionable",
            StepStatus::Failed => "failed",
            StepStatus::Stopped => "stopped",
        }
    }

    /// 中文标签。
    pub const fn label_zh(self) -> &'static str {
        match self {
            StepStatus::DryRun => "预演",
            StepStatus::Applied => "已应用",
            StepStatus::Unchanged => "已是目标值",
            StepStatus::SkippedMissing => "目标不存在（已跳过）",
            StepStatus::NotActionable => "不可执行（需人工确认）",
            StepStatus::Failed => "失败",
            StepStatus::Stopped => "已停手（日志不可写）",
        }
    }

    /// 英文标签。
    pub const fn label_en(self) -> &'static str {
        match self {
            StepStatus::DryRun => "dry-run",
            StepStatus::Applied => "applied",
            StepStatus::Unchanged => "already at target",
            StepStatus::SkippedMissing => "target missing (skipped)",
            StepStatus::NotActionable => "not actionable (needs a human)",
            StepStatus::Failed => "failed",
            StepStatus::Stopped => "stopped (journal unwritable)",
        }
    }

    /// 该步骤是否**没有产生错误**（预演/已应用/已是目标值/目标不存在/不可执行都算没有出错）。
    pub const fn is_ok(self) -> bool {
        !matches!(self, StepStatus::Failed | StepStatus::Stopped)
    }
}

/// 一个步骤的执行结果（含前后值，字段与审计日志同一套载荷 schema）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppliedStep {
    /// 计划内的顺序（1 起）。
    pub order: u32,
    /// 策略规则 id。
    pub rule_id: String,
    /// 规则在来源文件中的 1 基行号。
    pub rule_line: Option<u32>,
    /// 该步骤调用的 HAL 操作。
    pub hal_op: HalOp,
    /// 执行状态。
    pub status: StepStatus,
    /// 写入前状态（`null` = 不可知，例如目标进程受保护、读不到前值）。
    pub before: Option<Value>,
    /// 写入后状态。
    pub after: Option<Value>,
    /// 失败原因（结构化）。
    pub error: Option<CoreError>,
    /// 入链后的记录 id（预演与"已是目标值"为 `null`）。
    pub journal_id: Option<u64>,
    /// 附加说明（降级、读回不一致、不可回滚等）。
    pub note: Option<Text>,
    /// 该步骤是否可回滚（`before` 不可知时为 `false`）。
    pub reversible: bool,
}

impl AppliedStep {
    /// 是否真的改变了系统状态。
    pub const fn changed_state(&self) -> bool {
        matches!(self.status, StepStatus::Applied)
    }
}

/// `gopt apply` 的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApplyReport {
    /// 策略 id。
    pub game_id: String,
    /// 中文名。
    pub game_name_zh: String,
    /// 英文名。
    pub game_name_en: String,
    /// 目标进程。
    pub pid: u32,
    /// 策略来源。
    pub policy_origin: String,
    /// 是否预演。
    pub dry_run: bool,
    /// 已应用步骤数。
    pub applied: usize,
    /// 已是目标值、未写系统的步骤数。
    pub unchanged: usize,
    /// 目标不存在被跳过的步骤数。
    pub skipped_missing: usize,
    /// 失败步骤数。
    pub failed: usize,
    /// 是否因日志不可写而中途停手。
    pub journal_blocked: bool,
    /// 逐步骤结果。
    pub steps: Vec<AppliedStep>,
    /// 策略里被跳过的规则（可解释性的一半）。
    pub skipped: Vec<PlanSkip>,
    /// 本次写入的审计记录 id。
    pub journal_ids: Vec<u64>,
    /// 审计日志路径。
    pub journal_path: String,
    /// 计划是否含需要提权的步骤。
    pub requires_elevation: bool,
    /// 计划是否含危险动作（改整机状态）。
    pub has_dangerous_steps: bool,
    /// 执行期间的提示。
    pub notices: Vec<Text>,
}

impl ApplyReport {
    /// 是否全部步骤都成功收场（含预演与"已是目标值"）。
    pub fn all_ok(&self) -> bool {
        self.failed == 0 && !self.journal_blocked
    }

    /// 真的写了系统的步骤数。
    pub const fn changed(&self) -> usize {
        self.applied
    }
}

// ---------------------------------------------------------------------------
// rollback
// ---------------------------------------------------------------------------

/// 回滚选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RollbackOptions {
    /// 回滚到哪条记录（含）；`None` = 由 `all`/`pending` 决定。
    pub to_id: Option<u64>,
    /// 撤销全部可逆记录。
    pub all: bool,
    /// 只撤销"还没被撤销过"的 apply（跳过已撤销的）。
    pub pending: bool,
    /// 只生成计划、不执行（默认安全：CLI 没有 `--yes` 时用这个）。
    pub dry_run: bool,
}

impl RollbackOptions {
    /// `--all` 的默认值（预演）。
    pub const fn all() -> Self {
        Self {
            to_id: None,
            all: true,
            pending: false,
            dry_run: false,
        }
    }

    /// 回滚到指定 id（含）。
    pub const fn to(id: u64) -> Self {
        Self {
            to_id: Some(id),
            all: false,
            pending: false,
            dry_run: false,
        }
    }

    /// 只撤销尚未撤销的 apply。
    pub const fn pending() -> Self {
        Self {
            to_id: None,
            all: false,
            pending: true,
            dry_run: false,
        }
    }

    /// 转成预演。
    pub const fn preview(self) -> Self {
        Self {
            dry_run: true,
            ..self
        }
    }
}

/// 一条撤销步骤的执行结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RollbackStepReport {
    /// 被撤销的审计记录 id。
    pub journal_id: u64,
    /// 被撤销记录的类型。
    pub kind: JournalKind,
    /// 被撤销记录的作用对象。
    pub target: String,
    /// 类型化动作（结构化）。
    pub action: RollbackAction,
    /// 一行英文描述。
    pub description: String,
    /// 该动作是否可执行。
    pub actionable: bool,
    /// 执行状态。
    pub status: StepStatus,
    /// 失败原因。
    pub error: Option<CoreError>,
    /// 本次执行追加的 `kind = rollback` 记录 id。
    pub rollback_record_id: Option<u64>,
}

/// `gopt rollback` 的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RollbackReport {
    /// 计划覆盖到哪条记录（含）。
    pub to_id: u64,
    /// 是否预演。
    pub dry_run: bool,
    /// 计划步骤总数。
    pub planned: usize,
    /// 其中可执行步骤数。
    pub executable: usize,
    /// 实际执行成功数。
    pub executed: usize,
    /// 执行失败数。
    pub failed: usize,
    /// 不可执行（需要人工确认）的步骤数。
    pub not_actionable: usize,
    /// 逐步骤结果（按记录 id 降序）。
    pub steps: Vec<RollbackStepReport>,
    /// 本次追加的审计记录 id。
    pub journal_ids: Vec<u64>,
    /// 审计日志路径。
    pub journal_path: String,
    /// 一行英文摘要（来自 `RollbackPlan::summary`）。
    pub summary: String,
    /// 执行期间的提示。
    pub notices: Vec<Text>,
}

impl RollbackReport {
    /// 是否全部可执行步骤都成功。
    pub fn all_ok(&self) -> bool {
        self.failed == 0
    }
}

// ---------------------------------------------------------------------------
// journal / verify-journal
// ---------------------------------------------------------------------------

/// 一条审计记录的可展示视图。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JournalEntryView {
    /// 链上序号。
    pub id: u64,
    /// UTC 毫秒时间戳。
    pub ts_unix_ms: i64,
    /// 记录类型。
    pub kind: JournalKind,
    /// 作用对象。
    pub target: String,
    /// 触发修改的策略/规则。
    pub rule_id: Option<String>,
    /// 写入前状态。
    pub before: Option<Value>,
    /// 写入后状态。
    pub after: Option<Value>,
    /// 记录哈希。
    pub hash: String,
    /// 上一条哈希。
    pub prev_hash: String,
    /// 自哈希是否自洽。
    pub hash_ok: bool,
    /// 是否可回滚（有 `before` 且类型可逆）。
    pub reversible: bool,
    /// 是否已经被回滚记录覆盖。
    pub undone: bool,
    /// 文件中的 1 基行号。
    pub line_no: usize,
}

impl JournalEntryView {
    /// 由链上记录构造（`undone` 由调用方按 [`gopt_journal::undone_apply_ids`] 标记）。
    pub fn from_record(record: &JournalRecord, line_no: usize, undone: bool) -> Self {
        Self {
            id: record.id,
            ts_unix_ms: record.ts_unix_ms,
            kind: record.kind,
            target: record.target.clone(),
            rule_id: record.rule_id.clone(),
            before: record.before.clone(),
            after: record.after.clone(),
            hash: record.hash.clone(),
            prev_hash: record.prev_hash.clone(),
            hash_ok: record.has_valid_hash(),
            reversible: record.is_reversible(),
            undone,
            line_no,
        }
    }
}

/// 日志过滤条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct JournalFilter {
    /// 只保留指定类型的记录。
    pub kind: Option<JournalKind>,
    /// 最多显示多少条（`None` = 全部）。
    pub limit: Option<usize>,
}

/// `gopt journal` 的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JournalView {
    /// 日志路径。
    pub path: String,
    /// 文件是否存在。
    pub exists: bool,
    /// 已提交记录数。
    pub records: usize,
    /// 无法解析的行数。
    pub malformed_lines: usize,
    /// 跳过的空行数。
    pub skipped_blank_lines: usize,
    /// 本次展示的记录（按 id 升序，`limit` 生效，过滤后）。
    pub entries: Vec<JournalEntryView>,
    /// 链校验报告。
    pub chain: ChainReport,
    /// 打开时修复的尾部残字节。
    pub tail_repair: Option<TailRepair>,
    /// 还没有被撤销的 apply 记录 id。
    pub pending_apply_ids: Vec<u64>,
    /// 已被撤销的 apply 记录 id。
    pub undone_apply_ids: Vec<u64>,
}

/// 审计链校验结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    /// 链完好。
    Ok,
    /// 只有尾部半行（崩溃残留）——可修复，记录本身没被篡改。`--strict` 下按失败处理。
    Recoverable,
    /// 链被篡改或损坏 ⇒ 退出码 3。
    Broken,
}

impl VerifyStatus {
    /// 稳定短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            VerifyStatus::Ok => "ok",
            VerifyStatus::Recoverable => "recoverable",
            VerifyStatus::Broken => "broken",
        }
    }
}

/// `verify-journal` 选项。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct VerifyOptions {
    /// 外部锚点（能额外检出"尾部整行被删除"）。
    pub anchor: Option<ChainAnchor>,
    /// 严格模式：把"可修复的尾部半行"也算失败。
    pub strict: bool,
}

/// `gopt verify-journal` 的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerifyReport {
    /// 日志路径。
    pub path: String,
    /// 文件是否存在（不存在 ⇒ 空日志，视为完好）。
    pub exists: bool,
    /// 结论。
    pub status: VerifyStatus,
    /// 参与校验的行数。
    pub total: usize,
    /// 通过校验的记录数。
    pub verified: usize,
    /// 已提交记录数。
    pub records: usize,
    /// 无法解析的行数。
    pub malformed_lines: usize,
    /// 是否使用了外部锚点。
    pub anchored: bool,
    /// 第一处不一致。
    pub first_break: Option<ChainBreak>,
    /// 尾部半行修复信息（若本次打开时修复了）。
    pub tail_repair: Option<TailRepair>,
    /// 是否只是"可修复的尾部半行"。
    pub repairable: bool,
    /// 一行英文摘要。
    pub summary: String,
    /// 还有多少条记录待撤销。
    pub pending_rollbacks: usize,
}

impl VerifyReport {
    /// 进程退出码：0 通过（`Recoverable` 非严格模式也算通过），3 失败。
    pub const fn exit_code(&self) -> i32 {
        match self.status {
            VerifyStatus::Ok | VerifyStatus::Recoverable => crate::outcome::EXIT_OK,
            VerifyStatus::Broken => crate::outcome::EXIT_AUDIT,
        }
    }
}

// ---------------------------------------------------------------------------
// explain
// ---------------------------------------------------------------------------

/// 一条规则的完整解释（`--rule` / 游戏详情里都用这个形状）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuleExplanation {
    /// 所属游戏 id。
    pub game_id: String,
    /// 规则 id。
    pub rule_id: String,
    /// 规则在来源文件中的 1 基行号。
    pub rule_line: Option<u32>,
    /// 条件的中文描述（`always` 表示无条件）。
    pub condition_zh: String,
    /// 条件的英文描述。
    pub condition_en: String,
    /// 条件当前是否满足（`None` = 该游戏没有运行、未求值）。
    pub condition_met: Option<bool>,
    /// 动作种类。
    pub action_kind: String,
    /// 动作的中文描述。
    pub action_zh: String,
    /// 动作的英文描述。
    pub action_en: String,
    /// 对应的 HAL 操作（`skip` 动作为 `None`）。
    pub hal_op: Option<String>,
    /// 是否需要管理员权限。
    pub requires_elevation: bool,
    /// 是否为危险动作。
    pub is_dangerous: bool,
    /// 若没有产生步骤，这里是跳过原因。
    pub skip_cause: Option<String>,
}

/// 一款游戏的完整解释。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GameExplanation {
    /// 策略 id。
    pub id: String,
    /// 中文名。
    pub name_zh: String,
    /// 英文名。
    pub name_en: String,
    /// 策略来源。
    pub origin: String,
    /// 来源层级。
    pub layer: String,
    /// 主 exe 模式。
    pub exe_match: String,
    /// 其它 exe。
    pub exe_aliases: Vec<String>,
    /// 名称别名。
    pub name_aliases: Vec<String>,
    /// 中文说明。
    pub description_zh: String,
    /// 英文说明。
    pub description_en: String,
    /// 逐规则解释。
    pub rules: Vec<RuleExplanation>,
}

/// 一条审计记录的完整解释（含它会被怎么撤销）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JournalExplanation {
    /// 记录 id。
    pub id: u64,
    /// 作用对象。
    pub target: String,
    /// 记录类型。
    pub kind: JournalKind,
    /// 触发修改的规则。
    pub rule_id: Option<String>,
    /// 撤销动作描述（来自 `gopt-journal` 的类型化解码）。
    pub rollback_description: String,
    /// 撤销动作是否可执行。
    pub actionable: bool,
    /// 自哈希是否自洽。
    pub hash_ok: bool,
    /// 是否已被回滚记录覆盖。
    pub undone: bool,
    /// 写入前状态。
    pub before: Option<Value>,
    /// 写入后状态。
    pub after: Option<Value>,
    /// 文件中的 1 基行号。
    pub line_no: usize,
}

/// `gopt explain` 的联合结果（三种查询共用一种输出形状）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExplainReport {
    /// 查询类型：`game` / `rule` / `journal`。
    pub query_kind: String,
    /// 查询串。
    pub query: String,
    /// 游戏解释（`query_kind = game`）。
    pub game: Option<GameExplanation>,
    /// 规则解释（`query_kind = rule`，可能命中多个游戏）。
    pub rules: Vec<RuleExplanation>,
    /// 记录解释（`query_kind = journal`）。
    pub journal: Option<JournalExplanation>,
    /// 附加说明。
    pub notes: Vec<Text>,
}

// ---------------------------------------------------------------------------
// prio / tune / startup
// ---------------------------------------------------------------------------

/// `gopt prio` 的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrioReport {
    /// 目标进程。
    pub pid: u32,
    /// 进程名（可查到时有）。
    pub name: Option<String>,
    /// 写入前的优先级。
    pub before: PriorityClass,
    /// 写入后（或当前）的优先级。
    pub after: PriorityClass,
    /// 是否真的改了。
    pub changed: bool,
    /// 是否预演。
    pub dry_run: bool,
    /// 入链记录 id。
    pub journal_id: Option<u64>,
    /// 审计日志路径。
    pub journal_path: String,
}

/// `gopt tune` 的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TuneReport {
    /// 是否只查询。
    pub query_only: bool,
    /// 目标选择（`high` / `balanced`）。
    pub target: Option<String>,
    /// 切换前的电源方案。
    pub before: PowerScheme,
    /// 切换后（或当前）的电源方案。
    pub after: PowerScheme,
    /// 是否真的改了。
    pub changed: bool,
    /// 是否预演。
    pub dry_run: bool,
    /// 入链记录 id。
    pub journal_id: Option<u64>,
    /// 审计日志路径。
    pub journal_path: String,
}

/// `gopt startup` 的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartupReport {
    /// 动作：`list` / `enable` / `disable`。
    pub action: String,
    /// 启动项列表（`list` 时非空）。
    pub entries: Vec<RunEntry>,
    /// 变更前的条目（启用/禁用时有）。
    pub before: Option<RunEntry>,
    /// 变更后的条目。
    pub after: Option<RunEntry>,
    /// 是否真的改了。
    pub changed: bool,
    /// 是否预演。
    pub dry_run: bool,
    /// 入链记录 id。
    pub journal_id: Option<u64>,
    /// 审计日志路径。
    pub journal_path: String,
}

// ---------------------------------------------------------------------------
// import-legacy
// ---------------------------------------------------------------------------

/// 旧格式来源的报告（`gopt-journal::SourceReport` 的可序列化投影）。
///
/// 为什么投影一份：`SourceReport` 属于 `gopt-journal`，它刻意只带 `Debug/Clone`（不引入 serde 依赖），
/// 而 `--json` 需要它可序列化——于是这里做一次显式字段搬运，两边字段名保持一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceSummary {
    /// 文件路径。
    pub path: String,
    /// 是否存在。
    pub exists: bool,
    /// 文件字节数。
    pub bytes: u64,
    /// 可解析行数。
    pub valid_lines: usize,
    /// 坏行数。
    pub bad_lines: usize,
    /// 空行数。
    pub blank_lines: usize,
    /// 被忽略的行数（例如 `games.conf` 里的负索引）。
    pub ignored_lines: usize,
    /// 说明（坏行原因、幂等提示等）。
    pub notes: Vec<String>,
    /// 一行英文摘要。
    pub summary: String,
}

impl SourceSummary {
    /// 由 `gopt-journal` 的报告构造。
    pub fn from_source_report(report: &SourceReport) -> Self {
        Self {
            path: report.path.clone(),
            exists: report.exists,
            bytes: report.bytes,
            valid_lines: report.valid_lines,
            bad_lines: report.bad_lines,
            blank_lines: report.blank_lines,
            ignored_lines: report.ignored_lines,
            notes: report.notes.clone(),
            summary: report.summary(),
        }
    }
}

/// `gopt import-legacy` 的选项。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct LegacyImportOptions {
    /// `savepoints.txt` 路径（缺省 = 数据目录下的同名文件）。
    pub savepoints: Option<std::path::PathBuf>,
    /// `games.conf` 路径（缺省 = 数据目录下的同名文件）。
    pub games_conf: Option<std::path::PathBuf>,
    /// 只解析、不写日志（默认安全：CLI 没有 `--yes` 时用这个）。
    pub dry_run: bool,
}

/// `gopt import-legacy` 的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportReport {
    /// `savepoints.txt` 的来源报告。
    pub savepoints: SourceSummary,
    /// `games.conf` 的来源报告。
    pub games_conf: SourceSummary,
    /// 解析出的草稿条数。
    pub drafts: usize,
    /// 真正写入日志的条数。
    pub imported: usize,
    /// 因红线（REALTIME）被过滤掉条数。
    pub policy_filtered: usize,
    /// 是否预演。
    pub dry_run: bool,
    /// 入链记录 id。
    pub journal_ids: Vec<u64>,
    /// 来源说明（坏行、只读保证等）。
    pub notes: Vec<String>,
    /// 一行英文摘要。
    pub summary: String,
}

// ---------------------------------------------------------------------------
// watch
// ---------------------------------------------------------------------------

/// `gopt watch` 选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WatchOptions {
    /// 轮询间隔（秒，最小 1）。
    pub interval_secs: u64,
    /// 总时长（秒）；`0` = 一直跑到 Ctrl+C。
    pub duration_secs: u64,
    /// 只跑一轮就退出。
    pub once: bool,
    /// 是否真的应用（`false` = 只报告会做什么）。
    pub apply: bool,
}

/// `gopt watch` 的跨轮状态（同一进程内复用，避免重复优化同一进程）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WatchState {
    /// 已经处理过的 (游戏 id, pid)。
    pub processed: Vec<(String, u32)>,
    /// 已完成的轮数。
    pub ticks: u64,
}

impl WatchState {
    /// 该进程是否已经处理过。
    pub fn has_processed(&self, game_id: &str, pid: u32) -> bool {
        self.processed
            .iter()
            .any(|(id, seen)| id == game_id && *seen == pid)
    }

    /// 标记已处理。
    pub fn mark_processed(&mut self, game_id: &str, pid: u32) {
        if !self.has_processed(game_id, pid) {
            self.processed.push((game_id.to_string(), pid));
        }
    }
}

/// 单轮里对一个游戏进程做的事。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WatchTickItem {
    /// 策略 id。
    pub game_id: String,
    /// 中文名。
    pub name_zh: String,
    /// 英文名。
    pub name_en: String,
    /// 进程 ID。
    pub pid: u32,
    /// 动作：`applied` / `unchanged` / `already_optimized` / `previewed` / `failed`。
    pub action: String,
    /// 成功应用的步骤数。
    pub applied: usize,
    /// 失败步骤数。
    pub failed: usize,
    /// 失败原因（一行英文）。
    pub error: Option<String>,
}

/// `gopt watch` 的一轮结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WatchTick {
    /// 第几轮（1 起）。
    pub tick: u64,
    /// 已运行秒数。
    pub elapsed_secs: f64,
    /// 命中策略的进程数。
    pub matched: usize,
    /// 本轮每个命中进程的结果。
    pub items: Vec<WatchTickItem>,
    /// 是否还会继续下一轮。
    pub continuing: bool,
}

// ---------------------------------------------------------------------------
// report
// ---------------------------------------------------------------------------

/// `gopt report` 的结果：把一次"体检"需要的全部事实打包（文本报告与 `--json` 用同一份数据）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportData {
    /// 生成时间（UTC 毫秒）。
    pub generated_at_unix_ms: i64,
    /// 环境 + 策略 + 日志的总览。
    pub status: StatusReport,
    /// 全部可用策略。
    pub games: Vec<GameSummary>,
    /// 正在运行的进程数。
    pub processes: usize,
    /// 正在运行且命中策略的进程。
    pub running: Vec<RunningGame>,
    /// 审计日志最近若干条记录（按时间正序）。
    pub journal_tail: Vec<JournalEntryView>,
    /// 待撤销的 apply 记录数。
    pub pending_rollbacks: usize,
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 进程信息 → 一行摘要 `1234 cs2.exe (24 threads)`。
pub fn describe_process(process: &ProcessInfo) -> String {
    format!(
        "{} {} ({} threads)",
        process.pid, process.name, process.thread_count
    )
}

/// `WorkingSetLimits` → 一行摘要 `min 512 MiB / max 2048 MiB`（无上限时写 `unbounded`）。
pub fn describe_working_set(limits: WorkingSetLimits) -> String {
    let to_mib = |bytes: u64| bytes / (1024 * 1024);
    if limits.max_bytes >= WorkingSetLimits::NO_UPPER_BOUND {
        format!("min {} MiB / max unbounded", to_mib(limits.min_bytes))
    } else {
        format!(
            "min {} MiB / max {} MiB",
            to_mib(limits.min_bytes),
            to_mib(limits.max_bytes)
        )
    }
}

/// 计划 → 一行摘要（终端标题用）。
pub fn describe_plan(plan: &Plan) -> String {
    format!(
        "{} / {} — {} step(s), {} skipped, rules from {}",
        plan.game_id,
        plan.game_name_en,
        plan.step_count(),
        plan.skipped_count(),
        plan.policy_origin.display_path()
    )
}
