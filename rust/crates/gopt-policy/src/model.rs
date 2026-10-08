//! 校验后的策略模型：一款游戏（[`GamePolicy`]）由若干有序规则（[`Rule`]）组成，
//! 每条规则 = 可选条件（[`crate::Condition`]）+ 一个动作（[`Action`]）。
//!
//! 与 [`crate::plan`] 的区别：
//!
//! * 本模块是**约束保护型**模型：字段私有、只能经校验器或构造器建立，任何"能构造出来"的
//!   对象都满足加载期不变量（动作只有一种、掩码非 0、名称非空……）；
//! * [`crate::plan`] 是**输出记录**：字段公开、可直接读取，因为它是求值结果而不是输入。

use serde::{Deserialize, Serialize};

use gopt_hal::{PowerSchemeSelector, PriorityClass, RunHive, WorkingSetLimits};

use crate::condition::Condition;
use crate::error::PolicyOrigin;
use crate::matching::wildcard_match;
use crate::plan::Reason;

/// 动作种类（规则 id 推导、CLI 展示、测试断言共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionKind {
    /// `priority { class }`
    Priority,
    /// `affinity { ... }`
    Affinity,
    /// `working_set { min_mb, max_mb }`
    WorkingSet,
    /// `power_scheme { scheme }`
    PowerScheme,
    /// `run_entries { disable/enable }`
    RunEntries,
    /// `skip { reason_zh, reason_en }`：显式空动作，用于把"硬件降级/不适用"写进数据里。
    Skip,
}

impl ActionKind {
    /// 全部动作种类（枚举顺序稳定）。
    pub const ALL: [ActionKind; 6] = [
        ActionKind::Priority,
        ActionKind::Affinity,
        ActionKind::WorkingSet,
        ActionKind::PowerScheme,
        ActionKind::RunEntries,
        ActionKind::Skip,
    ];

    /// 稳定短名（同时用于推导默认规则 id 的后缀）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ActionKind::Priority => "priority",
            ActionKind::Affinity => "affinity",
            ActionKind::WorkingSet => "working-set",
            ActionKind::PowerScheme => "power-scheme",
            ActionKind::RunEntries => "run-entries",
            ActionKind::Skip => "skip",
        }
    }

    /// 中文名（CLI/explain 用）。
    pub const fn label_zh(self) -> &'static str {
        match self {
            ActionKind::Priority => "进程优先级",
            ActionKind::Affinity => "CPU 亲和性",
            ActionKind::WorkingSet => "工作集",
            ActionKind::PowerScheme => "电源方案",
            ActionKind::RunEntries => "开机启动项",
            ActionKind::Skip => "跳过",
        }
    }

    /// 英文名。
    pub const fn label_en(self) -> &'static str {
        match self {
            ActionKind::Priority => "process priority",
            ActionKind::Affinity => "CPU affinity",
            ActionKind::WorkingSet => "working set",
            ActionKind::PowerScheme => "power scheme",
            ActionKind::RunEntries => "startup entries",
            ActionKind::Skip => "skip",
        }
    }
}

/// 保留核给系统时"从哪一端保留"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReserveSide {
    /// 保留全局序号**最小**的 N 个核（与 C++ 版 `leaveCoresForSystem` 语义一致，缺省）。
    #[default]
    First,
    /// 保留全局序号**最大**的 N 个核。
    Last,
}

impl ReserveSide {
    /// 稳定短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ReserveSide::First => "first",
            ReserveSide::Last => "last",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "first" | "lowest" | "min" => Some(ReserveSide::First),
            "last" | "highest" | "max" => Some(ReserveSide::Last),
            _ => None,
        }
    }
}

/// 亲和性动作的参数（`affinity { ... }`）。
///
/// `mask` 与 `reserve_cores` 二选一：显式掩码用于"我就是知道该用哪几位"的场景，
/// 声明式保留核数用于"给系统留一点余量"这种跨机器可迁移的意图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffinitySpec {
    reserve_cores: u32,
    reserve_from: ReserveSide,
    physical_only: bool,
    mask: Option<u64>,
}

impl AffinitySpec {
    /// 构造（`reserve_cores = 0` 表示不保留核）。
    pub const fn new(
        reserve_cores: u32,
        reserve_from: ReserveSide,
        physical_only: bool,
        mask: Option<u64>,
    ) -> Self {
        Self {
            reserve_cores,
            reserve_from,
            physical_only,
            mask,
        }
    }

    /// 保留给系统的核数。
    pub const fn reserve_cores(&self) -> u32 {
        self.reserve_cores
    }

    /// 从哪一端保留。
    pub const fn reserve_from(&self) -> ReserveSide {
        self.reserve_from
    }

    /// 是否仅绑定物理核（排除 SMT 兄弟核）。
    pub const fn physical_only(&self) -> bool {
        self.physical_only
    }

    /// 显式掩码（处理器组 0）。
    pub const fn mask(&self) -> Option<u64> {
        self.mask
    }

    /// 中文描述（不含具体机器上的掩码）。
    pub fn describe_zh(&self) -> String {
        self.describe(true)
    }

    /// 英文描述。
    pub fn describe_en(&self) -> String {
        self.describe(false)
    }

    fn describe(&self, chinese: bool) -> String {
        if let Some(mask) = self.mask {
            return if chinese {
                format!("显式掩码 {mask:#018x}（处理器组 0）")
            } else {
                format!("explicit mask {mask:#018x} (processor group 0)")
            };
        }
        let mut parts: Vec<String> = Vec::new();
        if self.physical_only {
            parts.push(if chinese {
                "仅物理核".to_string()
            } else {
                "physical cores only".to_string()
            });
        }
        if self.reserve_cores > 0 {
            let count = self.reserve_cores;
            if chinese {
                let unit = if self.physical_only {
                    "个物理核"
                } else {
                    "个逻辑核"
                };
                let side = match self.reserve_from {
                    ReserveSide::First => "全局序号最小的",
                    ReserveSide::Last => "全局序号最大的",
                };
                parts.push(format!("保留{side} {count} {unit}给系统"));
            } else {
                let unit = match (self.physical_only, count) {
                    (true, 1) => "physical core",
                    (true, _) => "physical cores",
                    (false, 1) => "logical core",
                    (false, _) => "logical cores",
                };
                let side = match self.reserve_from {
                    ReserveSide::First => "lowest-numbered",
                    ReserveSide::Last => "highest-numbered",
                };
                parts.push(format!("{count} {unit} ({side}) reserved for the system"));
            }
        }
        if parts.is_empty() {
            return if chinese {
                "全逻辑处理器".to_string()
            } else {
                "all logical processors".to_string()
            };
        }
        parts.join(if chinese { "；" } else { "; " })
    }
}

/// 电源方案动作的目标（`power_scheme { scheme = "high" | "balanced" }`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerSchemeChoice {
    /// 高性能方案（Windows 内置 GUID 或按名称匹配）。
    High,
    /// 平衡方案（Windows 默认）。
    Balanced,
}

impl PowerSchemeChoice {
    /// 全部取值。
    pub const ALL: [PowerSchemeChoice; 2] = [PowerSchemeChoice::High, PowerSchemeChoice::Balanced];

    /// 稳定短名（策略文件里的写法）。
    pub const fn as_str(self) -> &'static str {
        match self {
            PowerSchemeChoice::High => "high",
            PowerSchemeChoice::Balanced => "balanced",
        }
    }

    /// 中文名。
    pub const fn label_zh(self) -> &'static str {
        match self {
            PowerSchemeChoice::High => "高性能",
            PowerSchemeChoice::Balanced => "平衡",
        }
    }

    /// 英文名。
    pub const fn label_en(self) -> &'static str {
        match self {
            PowerSchemeChoice::High => "High performance",
            PowerSchemeChoice::Balanced => "Balanced",
        }
    }

    /// 宽松解析（接受 `high` / `high-performance` / `高性能` 等写法）。
    pub fn parse(text: &str) -> Option<Self> {
        let normalized = text.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        match normalized.as_str() {
            "high" | "high-performance" | "performance" | "高性能" => {
                Some(PowerSchemeChoice::High)
            }
            "balanced" | "balance" | "normal" | "平衡" => Some(PowerSchemeChoice::Balanced),
            _ => None,
        }
    }

    /// 转成 HAL 的选择器：`high` 走"按名称/GUID 解析高性能方案"，`balanced` 走内置平衡方案 GUID。
    pub fn selector(self) -> PowerSchemeSelector {
        match self {
            PowerSchemeChoice::High => PowerSchemeSelector::HighPerformance,
            PowerSchemeChoice::Balanced => PowerSchemeSelector::Explicit(gopt_hal::Guid::BALANCED),
        }
    }
}

/// `run_entries` 动作作用于哪个注册表根。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunHiveSpec {
    /// 只碰 `HKCU\...\Run`（默认：不需要管理员）。
    #[default]
    CurrentUser,
    /// 只碰 `HKLM\...\Run`（需要管理员）。
    LocalMachine,
    /// 两个根都碰（HKLM 部分需要管理员）。
    Both,
}

impl RunHiveSpec {
    /// 全部取值。
    pub const ALL: [RunHiveSpec; 3] = [
        RunHiveSpec::CurrentUser,
        RunHiveSpec::LocalMachine,
        RunHiveSpec::Both,
    ];

    /// 稳定短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            RunHiveSpec::CurrentUser => "hkcu",
            RunHiveSpec::LocalMachine => "hklm",
            RunHiveSpec::Both => "both",
        }
    }

    /// 宽松解析。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "hkcu" | "current_user" | "currentuser" | "user" => Some(RunHiveSpec::CurrentUser),
            "hklm" | "local_machine" | "localmachine" | "machine" => {
                Some(RunHiveSpec::LocalMachine)
            }
            "both" | "all" => Some(RunHiveSpec::Both),
            _ => None,
        }
    }

    /// 展开成具体的注册表根（顺序稳定：HKCU 在前）。
    pub fn hives(self) -> &'static [RunHive] {
        const USER: [RunHive; 1] = [RunHive::CurrentUser];
        const MACHINE: [RunHive; 1] = [RunHive::LocalMachine];
        const BOTH: [RunHive; 2] = [RunHive::CurrentUser, RunHive::LocalMachine];
        match self {
            RunHiveSpec::CurrentUser => &USER,
            RunHiveSpec::LocalMachine => &MACHINE,
            RunHiveSpec::Both => &BOTH,
        }
    }
}

/// 一条规则的动作。
///
/// 枚举变体的字段天然随枚举可见——这里的数据都是"已经过 HAL 类型收窄"的值
/// （[`PriorityClass`] 里没有 REALTIME、[`WorkingSetLimits`] 已校验上下限），
/// 因此公开读取是安全的；唯一有不变量的 [`AffinitySpec`] 用私有字段保护。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// 设置进程优先级（上限 HIGH）。
    Priority {
        /// 目标优先级类。
        class: PriorityClass,
    },
    /// 绑定 CPU 亲和性。
    Affinity(AffinitySpec),
    /// 设置工作集上下限。
    WorkingSet {
        /// 已校验的上下限（`max_bytes == NO_UPPER_BOUND` 表示只设下限）。
        limits: WorkingSetLimits,
    },
    /// 切换电源方案。
    PowerScheme {
        /// 目标方案。
        scheme: PowerSchemeChoice,
    },
    /// 启用/禁用开机启动项（求值时会按 hive 展开成多个步骤）。
    RunEntries {
        /// 作用范围。
        hive: RunHiveSpec,
        /// 启动项展示名列表。
        names: Vec<String>,
        /// `true` = 启用，`false` = 禁用（改名迁移）。
        enabled: bool,
        /// 条目不存在时按"跳过"处理而不是报错。
        ignore_missing: bool,
    },
    /// 显式空动作：规则命中但什么都不做，并把"为什么不做"写进 Plan 的 skipped 段。
    Skip {
        /// 中英双语理由。
        reason: Reason,
    },
}

impl Action {
    /// 动作种类。
    pub const fn kind(&self) -> ActionKind {
        match self {
            Action::Priority { .. } => ActionKind::Priority,
            Action::Affinity(_) => ActionKind::Affinity,
            Action::WorkingSet { .. } => ActionKind::WorkingSet,
            Action::PowerScheme { .. } => ActionKind::PowerScheme,
            Action::RunEntries { .. } => ActionKind::RunEntries,
            Action::Skip { .. } => ActionKind::Skip,
        }
    }

    /// 中文一句话描述（不含具体机器信息）。
    pub fn describe_zh(&self) -> String {
        match self {
            Action::Priority { class } => format!(
                "优先级 = {}（{}）",
                priority_label_zh(*class),
                class.as_str()
            ),
            Action::Affinity(spec) => format!("CPU 亲和性 = {}", spec.describe_zh()),
            Action::WorkingSet { limits } => {
                format!("工作集下限 = {} MB", limits.min_bytes / (1024 * 1024))
            }
            Action::PowerScheme { scheme } => format!("电源方案 = {}", scheme.label_zh()),
            Action::RunEntries {
                hive,
                names,
                enabled,
                ..
            } => format!(
                "{}启动项（{}）= {}",
                if *enabled { "启用" } else { "禁用" },
                hive.as_str(),
                names.join(", ")
            ),
            Action::Skip { reason } => format!("跳过：{}", reason.zh),
        }
    }

    /// 英文一句话描述。
    pub fn describe_en(&self) -> String {
        match self {
            Action::Priority { class } => format!(
                "priority = {} ({})",
                class.as_str(),
                priority_label_en(*class)
            ),
            Action::Affinity(spec) => format!("CPU affinity = {}", spec.describe_en()),
            Action::WorkingSet { limits } => {
                format!(
                    "working-set floor = {} MiB",
                    limits.min_bytes / (1024 * 1024)
                )
            }
            Action::PowerScheme { scheme } => format!("power scheme = {}", scheme.label_en()),
            Action::RunEntries {
                hive,
                names,
                enabled,
                ..
            } => format!(
                "{} startup entries ({}) = {}",
                if *enabled { "enable" } else { "disable" },
                hive.as_str(),
                names.join(", ")
            ),
            Action::Skip { reason } => format!("skipped: {}", reason.en),
        }
    }

    /// 该动作是否（可能）需要管理员权限。
    ///
    /// 判定依据来自 HAL 契约：写 HKLM 启动项与切换电源方案在未提权时会返回
    /// `AccessDenied`；进程级操作（优先级/亲和性/工作集）对同用户进程不需要提权。
    pub const fn requires_elevation(&self) -> bool {
        match self {
            Action::Priority { .. }
            | Action::Affinity(_)
            | Action::WorkingSet { .. }
            | Action::Skip { .. } => false,
            Action::PowerScheme { .. } => true,
            Action::RunEntries { hive, .. } => !matches!(hive, RunHiveSpec::CurrentUser),
        }
    }

    /// 该动作是否属于"危险动作"（改整机状态、用户可见，前端应二次确认）。
    pub const fn is_dangerous(&self) -> bool {
        match self {
            Action::Priority { .. }
            | Action::Affinity(_)
            | Action::WorkingSet { .. }
            | Action::Skip { .. } => false,
            Action::PowerScheme { .. } | Action::RunEntries { .. } => true,
        }
    }
}

/// 优先级类的中文名。
pub const fn priority_label_zh(class: PriorityClass) -> &'static str {
    match class {
        PriorityClass::Idle => "空闲",
        PriorityClass::BelowNormal => "低于正常",
        PriorityClass::Normal => "正常",
        PriorityClass::AboveNormal => "高于正常",
        PriorityClass::High => "高",
    }
}

/// 优先级类的英文名。
pub const fn priority_label_en(class: PriorityClass) -> &'static str {
    match class {
        PriorityClass::Idle => "idle",
        PriorityClass::BelowNormal => "below normal",
        PriorityClass::Normal => "normal",
        PriorityClass::AboveNormal => "above normal",
        PriorityClass::High => "high",
    }
}

/// 一条策略规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    id: String,
    when: Option<Condition>,
    action: Action,
    line: Option<u32>,
}

impl Rule {
    /// 程序化构造（不做严格校验；TOML 解析路径才是权威入口，见 [`crate::parse_policy_file`]）。
    pub fn new(id: impl Into<String>, when: Option<Condition>, action: Action) -> Self {
        Self {
            id: id.into(),
            when,
            action,
            line: None,
        }
    }

    /// 附带来源行号。
    #[must_use]
    pub fn with_line(mut self, line: Option<u32>) -> Self {
        self.line = line;
        self
    }

    /// 规则 id（进 Plan 步骤与审计日志的稳定标识）。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 条件（`None` = 无条件命中）。
    pub const fn when(&self) -> Option<&Condition> {
        self.when.as_ref()
    }

    /// 动作。
    pub const fn action(&self) -> &Action {
        &self.action
    }

    /// 来源文件中的 1 基行号。
    pub const fn line(&self) -> Option<u32> {
        self.line
    }

    /// 条件是否命中（无条件视为命中）。
    pub fn matches(&self, input: &crate::eval::EvalInput) -> bool {
        match &self.when {
            Some(condition) => condition.evaluate(input),
            None => true,
        }
    }
}

/// 一款游戏的策略：稳定 id + 中英双语名 + exe 匹配模式 + 有序规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamePolicy {
    id: String,
    name_zh: String,
    name_en: String,
    exe_match: String,
    exe_aliases: Vec<String>,
    name_aliases: Vec<String>,
    description_zh: String,
    description_en: String,
    rules: Vec<Rule>,
    origin: PolicyOrigin,
    line: Option<u32>,
}

impl GamePolicy {
    /// 程序化构造（别名与描述留空；TOML 解析路径是权威入口）。
    pub fn new(
        id: impl Into<String>,
        name_zh: impl Into<String>,
        name_en: impl Into<String>,
        exe_match: impl Into<String>,
        rules: Vec<Rule>,
        origin: PolicyOrigin,
    ) -> Self {
        Self {
            id: id.into(),
            name_zh: name_zh.into(),
            name_en: name_en.into(),
            exe_match: exe_match.into(),
            exe_aliases: Vec::new(),
            name_aliases: Vec::new(),
            description_zh: String::new(),
            description_en: String::new(),
            rules,
            origin,
            line: None,
        }
    }

    /// 追加 exe 别名（同一款游戏的多个可执行文件）。
    #[must_use]
    pub fn with_exe_aliases(mut self, aliases: Vec<String>) -> Self {
        self.exe_aliases = aliases;
        self
    }

    /// 追加名称别名（搜索/展示用）。
    #[must_use]
    pub fn with_name_aliases(mut self, aliases: Vec<String>) -> Self {
        self.name_aliases = aliases;
        self
    }

    /// 追加中英双语描述。
    #[must_use]
    pub fn with_descriptions(mut self, zh: impl Into<String>, en: impl Into<String>) -> Self {
        self.description_zh = zh.into();
        self.description_en = en.into();
        self
    }

    /// 附带来源行号。
    #[must_use]
    pub fn with_line(mut self, line: Option<u32>) -> Self {
        self.line = line;
        self
    }

    /// 稳定 id（用户覆盖的键）。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 中文名。
    pub fn name_zh(&self) -> &str {
        &self.name_zh
    }

    /// 英文名。
    pub fn name_en(&self) -> &str {
        &self.name_en
    }

    /// 主 exe 匹配模式。
    pub fn exe_match(&self) -> &str {
        &self.exe_match
    }

    /// exe 别名模式。
    pub fn exe_aliases(&self) -> &[String] {
        &self.exe_aliases
    }

    /// 名称别名。
    pub fn name_aliases(&self) -> &[String] {
        &self.name_aliases
    }

    /// 中文描述（可能为空）。
    pub fn description_zh(&self) -> &str {
        &self.description_zh
    }

    /// 英文描述（可能为空）。
    pub fn description_en(&self) -> &str {
        &self.description_en
    }

    /// 有序规则。
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// 来源（内置 / 用户）。
    pub const fn origin(&self) -> &PolicyOrigin {
        &self.origin
    }

    /// 来源文件中的 1 基行号。
    pub const fn line(&self) -> Option<u32> {
        self.line
    }

    /// 全部 exe 匹配模式（主模式 + 别名，保持声明顺序）。
    pub fn patterns(&self) -> impl Iterator<Item = &str> {
        core::iter::once(self.exe_match.as_str()).chain(self.exe_aliases.iter().map(String::as_str))
    }

    /// 该游戏的 exe 名是否命中本策略。
    pub fn matches_exe(&self, exe_name: &str) -> bool {
        self.patterns()
            .any(|pattern| wildcard_match(pattern, exe_name))
    }

    /// 按 id / 中英文名 / 名称别名 / exe 模式做宽松查找（大小写不敏感）。
    pub fn matches_query(&self, query: &str) -> bool {
        let query = query.trim();
        if query.is_empty() {
            return false;
        }
        if self.id.eq_ignore_ascii_case(query)
            || self.name_zh.eq_ignore_ascii_case(query)
            || self.name_en.eq_ignore_ascii_case(query)
        {
            return true;
        }
        if self
            .name_aliases
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(query))
        {
            return true;
        }
        self.matches_exe(query)
    }

    /// 按规则 id 取规则。
    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::PlanAction;
    use gopt_hal::Guid;

    fn make_policy(id: &str, patterns: &[&str], rules: Vec<Rule>) -> GamePolicy {
        let mut policy = GamePolicy::new(
            id,
            "中文名",
            "English Name",
            patterns[0],
            rules,
            PolicyOrigin::builtin("test.toml"),
        );
        if patterns.len() > 1 {
            policy =
                policy.with_exe_aliases(patterns[1..].iter().map(|p| (*p).to_string()).collect());
        }
        policy
    }

    #[test]
    fn action_kinds_and_labels_are_stable() {
        for kind in ActionKind::ALL {
            assert!(!kind.as_str().is_empty());
            assert!(!kind.label_zh().is_empty());
            assert!(!kind.label_en().is_empty());
        }
        assert_eq!(ActionKind::RunEntries.as_str(), "run-entries");
        assert_eq!(ActionKind::Skip.as_str(), "skip");
    }

    #[test]
    fn elevation_and_danger_flags_follow_the_hal_contract() {
        assert!(!Action::Priority {
            class: PriorityClass::High
        }
        .requires_elevation());
        assert!(!Action::Priority {
            class: PriorityClass::High
        }
        .is_dangerous());
        assert!(Action::PowerScheme {
            scheme: PowerSchemeChoice::High
        }
        .requires_elevation());
        assert!(Action::PowerScheme {
            scheme: PowerSchemeChoice::High
        }
        .is_dangerous());

        let user_scope = Action::RunEntries {
            hive: RunHiveSpec::CurrentUser,
            names: vec!["Discord".to_string()],
            enabled: false,
            ignore_missing: true,
        };
        assert!(!user_scope.requires_elevation());
        assert!(user_scope.is_dangerous());

        let machine_scope = Action::RunEntries {
            hive: RunHiveSpec::Both,
            names: vec!["Discord".to_string()],
            enabled: false,
            ignore_missing: true,
        };
        assert!(machine_scope.requires_elevation());
    }

    #[test]
    fn run_hive_spec_expands_in_a_stable_order() {
        assert_eq!(RunHiveSpec::CurrentUser.hives(), &[RunHive::CurrentUser]);
        assert_eq!(RunHiveSpec::LocalMachine.hives(), &[RunHive::LocalMachine]);
        assert_eq!(
            RunHiveSpec::Both.hives(),
            &[RunHive::CurrentUser, RunHive::LocalMachine]
        );
        assert_eq!(RunHiveSpec::parse("HKCU"), Some(RunHiveSpec::CurrentUser));
        assert_eq!(RunHiveSpec::parse("hklm"), Some(RunHiveSpec::LocalMachine));
        assert_eq!(RunHiveSpec::parse("both"), Some(RunHiveSpec::Both));
        assert_eq!(RunHiveSpec::parse("nope"), None);
    }

    #[test]
    fn power_scheme_choice_maps_onto_hal_selectors() {
        assert_eq!(
            PowerSchemeChoice::High.selector(),
            PowerSchemeSelector::HighPerformance
        );
        assert_eq!(
            PowerSchemeChoice::Balanced.selector(),
            PowerSchemeSelector::Explicit(Guid::BALANCED)
        );
        assert_eq!(
            PowerSchemeChoice::parse("高性能"),
            Some(PowerSchemeChoice::High)
        );
        assert_eq!(
            PowerSchemeChoice::parse("High-Performance"),
            Some(PowerSchemeChoice::High)
        );
        assert_eq!(
            PowerSchemeChoice::parse("平衡"),
            Some(PowerSchemeChoice::Balanced)
        );
        assert_eq!(PowerSchemeChoice::parse("turbo"), None);
    }

    #[test]
    fn affinity_descriptions_are_bilingual() {
        let spec = AffinitySpec::new(1, ReserveSide::First, true, None);
        assert_eq!(
            spec.describe_zh(),
            "仅物理核；保留全局序号最小的 1 个物理核给系统"
        );
        assert_eq!(
            spec.describe_en(),
            "physical cores only; 1 physical core (lowest-numbered) reserved for the system"
        );
        let mask = AffinitySpec::new(0, ReserveSide::First, false, Some(0xffff));
        assert_eq!(
            mask.describe_zh(),
            "显式掩码 0x000000000000ffff（处理器组 0）"
        );
        let all = AffinitySpec::new(0, ReserveSide::First, false, None);
        assert_eq!(all.describe_zh(), "全逻辑处理器");
        assert_eq!(all.describe_en(), "all logical processors");
    }

    #[test]
    fn game_policy_matching_covers_id_name_and_patterns() {
        let policy = make_policy("cs2", &["cs2.exe", "csgo.exe"], Vec::new());
        assert!(policy.matches_exe("CS2.EXE"));
        assert!(policy.matches_exe("csgo.exe"));
        assert!(!policy.matches_exe("cs.exe"));
        assert!(policy.matches_query("cs2"));
        assert!(policy.matches_query("中文名"));
        assert!(policy.matches_query("english name"));
        assert!(policy.matches_query("csgo.exe"));
        assert!(!policy.matches_query("   "));
        assert!(!policy.matches_query("valorant"));

        // 名称别名也参与查找。
        let aliased = make_policy("valorant", &["VALORANT-Win64-Shipping.exe"], Vec::new())
            .with_name_aliases(vec!["瓦罗兰特".to_string()]);
        assert!(aliased.matches_query("瓦罗兰特"));
    }

    #[test]
    fn plan_action_hal_ops_are_derived_from_the_action() {
        use gopt_hal::HalOp;
        let steps = [
            (
                PlanAction::Priority {
                    class: PriorityClass::High,
                },
                HalOp::SetPriority,
            ),
            (
                PlanAction::WorkingSet {
                    limits: WorkingSetLimits::from_mb(256, 0).expect("limits"),
                },
                HalOp::SetWorkingSet,
            ),
            (
                PlanAction::PowerScheme {
                    scheme: PowerSchemeChoice::High,
                    selector: PowerSchemeChoice::High.selector(),
                },
                HalOp::SetPowerScheme,
            ),
            (
                PlanAction::RunEntry {
                    hive: RunHive::CurrentUser,
                    name: "Steam".to_string(),
                    enabled: false,
                    ignore_missing: true,
                },
                HalOp::SetRunEntryEnabled,
            ),
        ];
        for (action, expected) in steps {
            assert_eq!(action.hal_op(), expected);
        }
    }
}
