//! HAL 值类型：与平台无关的公开数据契约（Win32 与 Mock 两个后端都返回这些类型）。
//!
//! 刻意不把 `windows` crate 的类型暴露在公开 API 里：`windows` 只是 Win32 后端的
//! 实现细节，前端（CLI/GUI）与策略引擎只依赖本模块的自有类型。

use core::fmt;
use core::str::FromStr;

use crate::error::{HalError, HalResult};

// ---------------------------------------------------------------------------
// GUID
// ---------------------------------------------------------------------------

/// 16 字节 GUID（电源方案标识等）。
///
/// 文本形式与 Windows 一致：`8c5e7fda-e8bf-4a96-9a85-a6e2638c635c`，带花括号或全小写
/// 均可解析；序列化为同一种文本，便于审计日志与 TOML 配置读写。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl Guid {
    /// 系统"高性能"电源方案（Windows 内置，英文系统名 "High performance"）。
    pub const HIGH_PERFORMANCE: Guid = Guid::from_u128(0x8c5e7fda_e8bf_4a96_9a85_a6e2638c635c);
    /// 系统"节能"电源方案（Power saver）。
    pub const POWER_SAVER: Guid = Guid::from_u128(0xa1841308_3541_4fab_bc81_f71556f20b4a);
    /// 系统"平衡"电源方案（Balanced）。
    pub const BALANCED: Guid = Guid::from_u128(0x381b4222_f694_41f0_9685_ff5bb260df2e);

    /// 按 Windows GUID 的四个字段构造。
    pub const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }

    /// 按 RFC 4122 的 128 位大端布局构造。
    pub const fn from_u128(value: u128) -> Self {
        Self {
            data1: (value >> 96) as u32,
            data2: (value >> 80) as u16,
            data3: (value >> 64) as u16,
            data4: [
                (value >> 56) as u8,
                (value >> 48) as u8,
                (value >> 40) as u8,
                (value >> 32) as u8,
                (value >> 24) as u8,
                (value >> 16) as u8,
                (value >> 8) as u8,
                value as u8,
            ],
        }
    }

    /// 转回 128 位大端布局。
    pub const fn to_u128(self) -> u128 {
        let d4 = self.data4;
        ((self.data1 as u128) << 96)
            | ((self.data2 as u128) << 80)
            | ((self.data3 as u128) << 64)
            | ((d4[0] as u128) << 56)
            | ((d4[1] as u128) << 48)
            | ((d4[2] as u128) << 40)
            | ((d4[3] as u128) << 32)
            | ((d4[4] as u128) << 24)
            | ((d4[5] as u128) << 16)
            | ((d4[6] as u128) << 8)
            | (d4[7] as u128)
    }

    /// 是否为全零 GUID。
    pub const fn is_nil(self) -> bool {
        self.to_u128() == 0
    }

    /// crate 内部：按 Windows 的四个字段拆开（Win32 后端做类型转换用）。
    pub(crate) const fn parts(self) -> (u32, u16, u16, [u8; 8]) {
        (self.data1, self.data2, self.data3, self.data4)
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d4 = self.data4;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.data1,
            self.data2,
            self.data3,
            d4[0],
            d4[1],
            d4[2],
            d4[3],
            d4[4],
            d4[5],
            d4[6],
            d4[7]
        )
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Guid({self})")
    }
}

impl FromStr for Guid {
    type Err = HalError;

    /// 接受 `{...}` / 带连字符 / 纯 32 位十六进制三种写法。
    fn from_str(text: &str) -> HalResult<Guid> {
        let trimmed = text.trim();
        let bare = trimmed
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .unwrap_or(trimmed);
        let hex: String = bare.chars().filter(|c| *c != '-').collect();
        if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(HalError::invalid_argument(
                "Guid::from_str",
                format!("`{text}` is not a 32-hex-digit GUID"),
            ));
        }
        u128::from_str_radix(&hex, 16)
            .map(Guid::from_u128)
            .map_err(|_| {
                HalError::invalid_argument(
                    "Guid::from_str",
                    format!("`{text}` is not a valid GUID"),
                )
            })
    }
}

impl serde::Serialize for Guid {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Guid {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        Guid::from_str(&text).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// 进程优先级
// ---------------------------------------------------------------------------

/// 进程优先级类（白名单，共 5 档）。
///
/// **类型级红线**：`REALTIME_PRIORITY_CLASS` 不是本枚举的成员，因此"HAL 绝不会把进程
/// 提升到 REALTIME"由类型系统保证，而不是靠运行时检查；[`PriorityClass::from_raw`]
/// 是唯一的原始值入口，遇到 `0x100` 返回 [`crate::HalErrorKind::PolicyDenied`]。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum PriorityClass {
    /// `IDLE_PRIORITY_CLASS` (0x40)：后台/下载类进程。
    Idle,
    /// `BELOW_NORMAL_PRIORITY_CLASS` (0x4000)。
    BelowNormal,
    /// `NORMAL_PRIORITY_CLASS` (0x20)：Windows 默认值。
    Normal,
    /// `ABOVE_NORMAL_PRIORITY_CLASS` (0x8000)。
    AboveNormal,
    /// `HIGH_PRIORITY_CLASS` (0x80)：本工具允许的最高档（上限）。
    High,
}

impl PriorityClass {
    /// 本工具允许的最高优先级（红线：不得高于 HIGH）。
    pub const MAX_ALLOWED: PriorityClass = PriorityClass::High;

    /// 白名单全集（按从低到高排列，供 CLI `--json` 与配置校验枚举）。
    pub const ALL: [PriorityClass; 5] = [
        PriorityClass::Idle,
        PriorityClass::BelowNormal,
        PriorityClass::Normal,
        PriorityClass::AboveNormal,
        PriorityClass::High,
    ];

    /// Win32 `SetPriorityClass` 的原始常量值。
    pub const fn raw(self) -> u32 {
        match self {
            PriorityClass::Idle => 0x0000_0040,
            PriorityClass::BelowNormal => 0x0000_4000,
            PriorityClass::Normal => 0x0000_0020,
            PriorityClass::AboveNormal => 0x0000_8000,
            PriorityClass::High => 0x0000_0080,
        }
    }

    /// 稳定短名（配置与 JSON 用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            PriorityClass::Idle => "idle",
            PriorityClass::BelowNormal => "below-normal",
            PriorityClass::Normal => "normal",
            PriorityClass::AboveNormal => "above-normal",
            PriorityClass::High => "high",
        }
    }

    /// 是否为允许的最高档。
    pub const fn is_max_allowed(self) -> bool {
        matches!(self, PriorityClass::High)
    }

    /// 原始值 → 枚举。**安全红线集中在这里**：`0x100`（REALTIME）被硬拒绝。
    pub fn from_raw(raw: u32) -> HalResult<Self> {
        match raw {
            0x0000_0040 => Ok(PriorityClass::Idle),
            0x0000_4000 => Ok(PriorityClass::BelowNormal),
            0x0000_0020 => Ok(PriorityClass::Normal),
            0x0000_8000 => Ok(PriorityClass::AboveNormal),
            0x0000_0080 => Ok(PriorityClass::High),
            REALTIME_PRIORITY_CLASS => Err(HalError::policy_denied(
                "PriorityClass::from_raw",
                "REALTIME_PRIORITY_CLASS (0x100) is rejected by policy: gopt never raises a \
                 process above HIGH_PRIORITY_CLASS",
            )),
            other => Err(HalError::invalid_argument(
                "PriorityClass::from_raw",
                format!("0x{other:x} is not a whitelisted process priority class"),
            )),
        }
    }

    /// 宽松解析：接受 `high` / `above-normal` / `ABOVE_NORMAL_PRIORITY_CLASS` / `0x80` / `128`。
    ///
    /// 供声明式策略引擎（TOML）使用：`priority = "high"` 与 `priority = 0x80` 等价，
    /// 而 `priority = 0x100` 会被红线拦截（[`crate::HalErrorKind::PolicyDenied`]）。
    pub fn parse(text: &str) -> HalResult<Self> {
        let trimmed = text.trim();
        if let Some(hex) = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
        {
            let raw = u32::from_str_radix(hex, 16).map_err(|_| {
                HalError::invalid_argument(
                    "PriorityClass::parse",
                    format!("`{text}` is not a hex priority"),
                )
            })?;
            return PriorityClass::from_raw(raw);
        }
        if let Ok(raw) = trimmed.parse::<u32>() {
            return PriorityClass::from_raw(raw);
        }
        let normalized = trimmed
            .to_ascii_lowercase()
            .replace(['_', ' ', '.'], "-")
            .replace("priority-class", "")
            .replace("priority", "")
            .trim_matches('-')
            .to_string();
        match normalized.as_str() {
            "idle" => Ok(PriorityClass::Idle),
            "belownormal" | "below-normal" => Ok(PriorityClass::BelowNormal),
            "normal" => Ok(PriorityClass::Normal),
            "abovenormal" | "above-normal" => Ok(PriorityClass::AboveNormal),
            "high" => Ok(PriorityClass::High),
            "realtime" | "real-time" => Err(HalError::policy_denied(
                "PriorityClass::parse",
                "`realtime` is rejected by policy: gopt never raises a process above HIGH_PRIORITY_CLASS",
            )),
            _ => Err(HalError::invalid_argument(
                "PriorityClass::parse",
                format!("`{text}` is not a known priority class"),
            )),
        }
    }
}

const REALTIME_PRIORITY_CLASS: u32 = 0x0000_0100;

impl FromStr for PriorityClass {
    type Err = HalError;

    fn from_str(text: &str) -> HalResult<Self> {
        PriorityClass::parse(text)
    }
}

impl fmt::Display for PriorityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// 工作集
// ---------------------------------------------------------------------------

/// 工作集上下限（字节）。语义与 C++ 版一致：`max_bytes == 0` 且 `min_bytes > 0`
/// 表示"只设下限、不设上限"（内部用 [`WorkingSetLimits::NO_UPPER_BOUND`] 顶替）。
///
/// 两条取值的来源，规则不同、都必须遵守：
///
/// * **写路径**（策略/CLI 产生的目标值）用 [`WorkingSetLimits::new`]，要求 `min_bytes > 0`
///   —— 不产生"最小工作集为 0"的写入请求；
/// * **读路径**（[`crate::SystemApi::get_working_set`] 反映的系统真实状态）用
///   [`WorkingSetLimits::observed`]，允许 `min_bytes == 0`，因为系统确实可能这么报告；
///   这种值能否作为写入目标由 [`WorkingSetLimits::is_restorable`] 回答。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkingSetLimits {
    /// 最小工作集（字节）。写路径要求 > 0；读路径忠实反映系统（可能为 0）。
    pub min_bytes: u64,
    /// 最大工作集（字节），必须 >= `min_bytes`。
    pub max_bytes: u64,
}

impl WorkingSetLimits {
    /// "无上限"哨兵值（8 TiB），沿用 C++ 版行为。
    pub const NO_UPPER_BOUND: u64 = 8 * 1024 * 1024 * 1024 * 1024;

    /// 构造并校验：`min_bytes > 0`，`min <= max`；`max_bytes == 0` 视为"不设上限"
    /// （沿用 C++ 版语义，直接展开为 [`WorkingSetLimits::NO_UPPER_BOUND`]）。
    pub fn new(min_bytes: u64, max_bytes: u64) -> HalResult<Self> {
        if min_bytes == 0 {
            return Err(HalError::invalid_argument(
                "WorkingSetLimits::new",
                "min_bytes must be greater than zero",
            ));
        }
        Self::observed(min_bytes, max_bytes)
    }

    /// 构造一个**观测值**（[`crate::SystemApi::get_working_set`] 的返回路径）：
    /// 与 [`WorkingSetLimits::new`] 的唯一区别是允许 `min_bytes == 0`
    /// —— 系统可能报告"不设最小工作集"，这里不粉饰、不取整。
    pub fn observed(min_bytes: u64, max_bytes: u64) -> HalResult<Self> {
        let max_bytes = if max_bytes == 0 {
            Self::NO_UPPER_BOUND
        } else {
            max_bytes
        };
        if min_bytes > max_bytes {
            return Err(HalError::invalid_argument(
                "WorkingSetLimits::observed",
                format!("min_bytes ({min_bytes}) must not exceed max_bytes ({max_bytes})"),
            ));
        }
        Ok(Self {
            min_bytes,
            max_bytes,
        })
    }

    /// 能否作为 [`crate::SystemApi::set_working_set`] 的目标值。
    ///
    /// 观测到的 `min_bytes == 0` 无法经 HAL 写回（写路径拒绝 min=0），
    /// 因此这种"写入前状态"不可精确还原——调用方应据此把步骤标为不可回滚，
    /// 而不是记录一个还原不了的 `before`。
    pub const fn is_restorable(&self) -> bool {
        self.min_bytes > 0
    }

    /// 以 MB 为单位构造并校验。
    pub fn from_mb(min_mb: u64, max_mb: u64) -> HalResult<Self> {
        let to_bytes = |mb: u64| mb.saturating_mul(1024 * 1024);
        Self::new(to_bytes(min_mb), to_bytes(max_mb))
    }

    /// 只设下限：`max_bytes == 0` 视为无上限。
    pub fn min_only(min_bytes: u64) -> HalResult<Self> {
        Self::new(min_bytes, Self::NO_UPPER_BOUND)
    }

    /// 归一化：把 `max_bytes == 0` 展开为无上限哨兵值。
    #[must_use]
    pub fn normalized(self) -> Self {
        if self.max_bytes == 0 {
            Self {
                min_bytes: self.min_bytes,
                max_bytes: Self::NO_UPPER_BOUND,
            }
        } else {
            self
        }
    }
}

// ---------------------------------------------------------------------------
// 进程
// ---------------------------------------------------------------------------

/// 运行中的进程快照（`list_processes` 的元素）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcessInfo {
    /// 进程 ID。
    pub pid: u32,
    /// 父进程 ID（已退出时为原父 PID）。
    pub parent_pid: u32,
    /// 可执行文件名（如 `cs2.exe`）。
    pub name: String,
    /// 完整路径；受保护进程无法查询时为 `None`（不视为错误）。
    pub exe_path: Option<String>,
    /// 线程数。
    pub thread_count: u32,
}

impl ProcessInfo {
    /// 构造（`exe_path` 未知时传 `None`）。
    pub fn new(pid: u32, parent_pid: u32, name: impl Into<String>, thread_count: u32) -> Self {
        Self {
            pid,
            parent_pid,
            name: name.into(),
            exe_path: None,
            thread_count,
        }
    }

    /// 附带完整路径。
    #[must_use]
    pub fn with_exe_path(mut self, path: Option<String>) -> Self {
        self.exe_path = path;
        self
    }
}

// ---------------------------------------------------------------------------
// 开机启动项
// ---------------------------------------------------------------------------

/// 启动项所在的注册表根（仅 Run 键，用户态、可回滚）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum RunHive {
    /// `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`（无需管理员）。
    CurrentUser,
    /// `HKEY_LOCAL_MACHINE\Software\Microsoft\Windows\CurrentVersion\Run`（写入需管理员）。
    LocalMachine,
}

impl RunHive {
    /// 稳定短名（`HKCU` / `HKLM`，与 C++ 版备份文件格式兼容）。
    pub const fn as_str(self) -> &'static str {
        match self {
            RunHive::CurrentUser => "HKCU",
            RunHive::LocalMachine => "HKLM",
        }
    }

    /// 全部根（枚举顺序固定，保证输出稳定）。
    pub const ALL: [RunHive; 2] = [RunHive::CurrentUser, RunHive::LocalMachine];

    /// 解析 `HKCU` / `HKLM`（大小写不敏感，也接受 `current_user` / `local_machine`）。
    pub fn parse(text: &str) -> HalResult<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "hkcu" | "currentuser" | "current_user" | "user" => Ok(RunHive::CurrentUser),
            "hklm" | "localmachine" | "local_machine" | "machine" => Ok(RunHive::LocalMachine),
            _ => Err(HalError::invalid_argument(
                "RunHive::parse",
                format!("`{text}` is not a Run key hive (expected HKCU or HKLM)"),
            )),
        }
    }

    /// 注册表子键路径（两个根共用同一路径）。
    pub const fn key_path(self) -> &'static str {
        r"Software\Microsoft\Windows\CurrentVersion\Run"
    }
}

impl fmt::Display for RunHive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单个开机启动项。
///
/// 禁用策略与 C++ 版一致：**改名迁移**而不是删除——`Foo` 改名为 `[disabled] Foo`，
/// 值内容与类型原样保留，因此"启用"就是把名字改回去，天然可回滚。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunEntry {
    /// 所在根。
    pub hive: RunHive,
    /// 注册表中的原始值名（可能带 [`RunEntry::DISABLED_PREFIX`]）。
    pub value_name: String,
    /// 展示名（去掉禁用前缀），也是启用/禁用操作的稳定标识。
    pub name: String,
    /// 启动命令行原文（`REG_SZ` / `REG_EXPAND_SZ` 内容，未展开环境变量）。
    pub command: String,
    /// 是否启用。
    pub enabled: bool,
    /// 是否为 `REG_EXPAND_SZ`（启动时展开 `%VAR%`）。
    pub expandable: bool,
}

impl RunEntry {
    /// 禁用改名时使用的前缀（与 C++ 版逐字节一致，保证备份/恢复互操作）。
    pub const DISABLED_PREFIX: &'static str = "[disabled] ";

    /// 由注册表原始值名、命令行与类型构造。
    pub fn from_registry(
        hive: RunHive,
        value_name: impl Into<String>,
        command: impl Into<String>,
        expandable: bool,
    ) -> Self {
        let value_name = value_name.into();
        let enabled = !Self::is_disabled(&value_name);
        Self {
            hive,
            name: Self::display_name(&value_name).to_string(),
            value_name,
            command: command.into(),
            enabled,
            expandable,
        }
    }

    /// 该原始值名是否处于"已禁用"（带前缀）状态。
    pub fn is_disabled(value_name: &str) -> bool {
        value_name.starts_with(Self::DISABLED_PREFIX)
    }

    /// 去掉禁用前缀后的展示名。
    pub fn display_name(value_name: &str) -> &str {
        value_name
            .strip_prefix(Self::DISABLED_PREFIX)
            .unwrap_or(value_name)
    }

    /// 展示名 → 禁用态的原始值名。
    pub fn disabled_value_name(name: &str) -> String {
        format!("{}{}", Self::DISABLED_PREFIX, Self::display_name(name))
    }

    /// 该启动项的稳定标识 `HKCU:Foo`（日志/审计链使用）。
    pub fn id(&self) -> String {
        format!("{}:{}", self.hive, self.name)
    }
}

// ---------------------------------------------------------------------------
// 电源方案
// ---------------------------------------------------------------------------

/// 一个已安装的电源方案。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PowerScheme {
    /// 方案 GUID。
    pub guid: Guid,
    /// 本地化友好名（如 "High performance" / "高性能"）。
    pub name: String,
    /// 是否判定为"高性能"方案（名称匹配或 GUID 等于内置值）。
    pub is_high_performance: bool,
}

impl PowerScheme {
    /// 构造。
    pub fn new(guid: Guid, name: impl Into<String>) -> Self {
        let name = name.into();
        let is_high_performance = Guid::is_high_performance(guid, &name);
        Self {
            guid,
            name,
            is_high_performance,
        }
    }
}

impl Guid {
    /// 判定某 GUID + 友好名是否为"高性能"方案（覆盖本地化名称）。
    pub fn is_high_performance(guid: Guid, name: &str) -> bool {
        if guid == Guid::HIGH_PERFORMANCE {
            return true;
        }
        let lowered = name.trim().to_ascii_lowercase();
        lowered == "high performance" || name.trim() == "高性能"
    }
}

/// 设置电源方案时的目标选择。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSchemeSelector {
    /// 显式指定 GUID（必须是系统上已安装的方案）。
    Explicit(Guid),
    /// 按名称解析"高性能"方案（英文 "High performance" / 中文 "高性能"）。
    HighPerformance,
}

/// 电源方案切换结果；`previous` 是回滚所需的信息，`current` 是回读后的实际状态。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PowerSchemeChange {
    /// 切换前的活动方案。
    pub previous: PowerScheme,
    /// 切换后的活动方案（回读确认）。
    pub current: PowerScheme,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_round_trips_through_text() {
        let text = "8c5e7fda-e8bf-4a96-9a85-a6e2638c635c";
        let guid: Guid = text.parse().expect("valid guid");
        assert_eq!(guid, Guid::HIGH_PERFORMANCE);
        assert_eq!(guid.to_string(), text);
        assert_eq!(Guid::from_u128(guid.to_u128()), guid);
    }

    #[test]
    fn guid_accepts_braces_and_rejects_garbage() {
        let braced: Guid = "{381B4222-F694-41F0-9685-FF5BB260DF2E}"
            .parse()
            .expect("braced guid");
        assert_eq!(braced, Guid::BALANCED);
        assert!(Guid::from_str("not-a-guid").is_err());
        assert!(Guid::from_str("381b4222-f694-41f0-9685-ff5bb260df2").is_err());
    }

    #[test]
    fn realtime_is_hard_rejected_from_raw() {
        let err = PriorityClass::from_raw(0x100).expect_err("REALTIME must be rejected");
        assert_eq!(err.kind(), crate::HalErrorKind::PolicyDenied);
        assert!(err.is_policy_denial());
    }

    #[test]
    fn priority_raw_values_match_win32() {
        assert_eq!(PriorityClass::Idle.raw(), 0x40);
        assert_eq!(PriorityClass::BelowNormal.raw(), 0x4000);
        assert_eq!(PriorityClass::Normal.raw(), 0x20);
        assert_eq!(PriorityClass::AboveNormal.raw(), 0x8000);
        assert_eq!(PriorityClass::High.raw(), 0x80);
        assert_eq!(PriorityClass::MAX_ALLOWED, PriorityClass::High);
        for class in PriorityClass::ALL {
            assert_eq!(
                PriorityClass::from_raw(class.raw()).expect("round trip"),
                class
            );
        }
    }

    #[test]
    fn priority_parse_handles_config_forms_and_blocks_realtime() {
        assert_eq!(
            PriorityClass::parse("high").expect("high"),
            PriorityClass::High
        );
        assert_eq!(
            PriorityClass::parse("0x80").expect("0x80"),
            PriorityClass::High
        );
        assert_eq!(
            PriorityClass::parse("128").expect("128"),
            PriorityClass::High
        );
        assert_eq!(
            PriorityClass::parse("ABOVE_NORMAL_PRIORITY_CLASS").expect("above normal"),
            PriorityClass::AboveNormal
        );
        assert_eq!(
            PriorityClass::parse("below-normal").expect("below"),
            PriorityClass::BelowNormal
        );
        let err = PriorityClass::parse("0x100").expect_err("realtime rejected");
        assert_eq!(err.kind(), crate::HalErrorKind::PolicyDenied);
        let err = PriorityClass::parse("realtime").expect_err("realtime rejected");
        assert_eq!(err.kind(), crate::HalErrorKind::PolicyDenied);
    }

    #[test]
    fn working_set_validation_and_normalization() {
        assert!(WorkingSetLimits::new(0, 1024).is_err());
        assert!(WorkingSetLimits::new(2048, 1024).is_err());
        let limits = WorkingSetLimits::from_mb(512, 2048).expect("valid");
        assert_eq!(limits.min_bytes, 512 * 1024 * 1024);
        assert_eq!(limits.max_bytes, 2048 * 1024 * 1024);
        let min_only = WorkingSetLimits::new(1024, 0).expect("valid");
        assert_eq!(
            min_only.normalized().max_bytes,
            WorkingSetLimits::NO_UPPER_BOUND
        );
    }

    #[test]
    fn observed_limits_report_the_system_truthfully() {
        // 读路径允许 min=0（系统确实可能这么报告），且不做粉饰。
        let observed = WorkingSetLimits::observed(0, 1_413_120).expect("observed zero minimum");
        assert_eq!(observed.min_bytes, 0);
        assert_eq!(observed.max_bytes, 1_413_120);
        assert!(!observed.is_restorable(), "min=0 cannot be written back");
        assert_eq!(observed.normalized(), observed, "nothing to normalize");

        // max=0 与写路径同一套语义：视为"不设上限"。
        let unbounded = WorkingSetLimits::observed(204_800, 0).expect("observed");
        assert_eq!(unbounded.max_bytes, WorkingSetLimits::NO_UPPER_BOUND);
        assert!(unbounded.is_restorable());

        // 观测同样不能自相矛盾。
        assert!(WorkingSetLimits::observed(4096, 1024).is_err());

        // 写路径构造出来的值一定可还原。
        assert!(WorkingSetLimits::from_mb(1, 0)
            .expect("valid")
            .is_restorable());
    }

    #[test]
    fn run_entry_name_migration_is_reversible() {
        let entry = RunEntry::from_registry(RunHive::CurrentUser, "Steam", r"C:\steam.exe", false);
        assert!(entry.enabled);
        let disabled = RunEntry::disabled_value_name(&entry.name);
        assert_eq!(disabled, "[disabled] Steam");
        assert!(RunEntry::is_disabled(&disabled));
        let restored = RunEntry::display_name(&disabled);
        assert_eq!(restored, "Steam");
        assert_eq!(entry.id(), "HKCU:Steam");

        let disabled_entry = RunEntry::from_registry(RunHive::LocalMachine, disabled, "x", true);
        assert!(!disabled_entry.enabled);
        assert_eq!(disabled_entry.name, "Steam");
        assert!(disabled_entry.expandable);
    }

    #[test]
    fn power_scheme_detects_high_performance_by_name_or_guid() {
        assert!(PowerScheme::new(Guid::HIGH_PERFORMANCE, "Custom Name").is_high_performance);
        assert!(PowerScheme::new(Guid::from_u128(1), "High performance").is_high_performance);
        assert!(PowerScheme::new(Guid::from_u128(1), "高性能").is_high_performance);
        assert!(!PowerScheme::new(Guid::BALANCED, "Balanced").is_high_performance);
    }
}
