//! `when` 条件：字段 + 比较算子，全部满足才算命中（AND 语义）。
//!
//! 支持六个字段（数值四个、文本一个、布尔一个）与四类算子：
//!
//! | 字段 | 类型 | 允许的算子 |
//! |---|---|---|
//! | `logical_cores` / `physical_cores` / `ram_gb` / `ram_mb` | 整数 | `gt` `gte` `lt` `lte` `eq` `ne` |
//! | `gpu_vendor` | 文本（`nvidia` `amd` `intel` `microsoft` `unknown` `none`） | `eq` `ne` |
//! | `is_elevated` | 布尔 | `eq` `ne` |
//!
//! 写法有两种，等价：
//!
//! ```toml
//! when = { logical_cores = 8 }                       # 标量简写 == eq
//! when = { ram_gb = { gte = 8, lt = 16 } }           # 区间（同一字段多个算子 = 同时满足）
//! when = { gpu_vendor = { ne = "none" } }
//! ```
//!
//! 缺省（不写 `when`，或 `when = {}`）表示**无条件命中**。
//! 校验（[`Comparison::validate`]）在加载期完成：类型不匹配、算子不支持、区间矛盾
//! （`gte = 16, lt = 8`）、`eq` 与 `ne` 同时出现等都会带行号报错，而不是留到求值期。

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::eval::EvalInput;

/// `u64` → `i64`：超过 `i64::MAX` 时饱和（条件比较用不到这么大的数，但绝不允许溢出/panic）。
const fn saturating_i64(value: u64) -> i64 {
    if value > i64::MAX as u64 {
        i64::MAX
    } else {
        value as i64
    }
}

/// 条件字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionField {
    /// 逻辑处理器数（含 SMT）。
    LogicalCores,
    /// 物理核数。
    PhysicalCores,
    /// 系统内存（GiB，由 MB 整除得到，与 C++ 版 `systemRamMB` 阈值同口径）。
    RamGb,
    /// 系统内存（MiB，用于需要精确阈值的场景）。
    RamMb,
    /// 首选显示适配器厂商。
    GpuVendor,
    /// 当前进程是否已提权（管理员）。
    IsElevated,
}

impl ConditionField {
    /// 全部字段（枚举顺序稳定：进 Plan 的 describe 与 TOML 文档）。
    pub const ALL: [ConditionField; 6] = [
        ConditionField::LogicalCores,
        ConditionField::PhysicalCores,
        ConditionField::RamGb,
        ConditionField::RamMb,
        ConditionField::GpuVendor,
        ConditionField::IsElevated,
    ];

    /// TOML 键名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ConditionField::LogicalCores => "logical_cores",
            ConditionField::PhysicalCores => "physical_cores",
            ConditionField::RamGb => "ram_gb",
            ConditionField::RamMb => "ram_mb",
            ConditionField::GpuVendor => "gpu_vendor",
            ConditionField::IsElevated => "is_elevated",
        }
    }

    /// 是否为整数型字段。
    pub const fn is_numeric(self) -> bool {
        matches!(
            self,
            ConditionField::LogicalCores
                | ConditionField::PhysicalCores
                | ConditionField::RamGb
                | ConditionField::RamMb
        )
    }

    /// 是否为文本型字段。
    pub const fn is_text(self) -> bool {
        matches!(self, ConditionField::GpuVendor)
    }

    /// 是否为布尔型字段。
    pub const fn is_bool(self) -> bool {
        matches!(self, ConditionField::IsElevated)
    }

    /// 由 TOML 键名反解（未知字段返回 `None`，由校验层翻译成带行号的错误）。
    pub fn from_key(key: &str) -> Option<Self> {
        ConditionField::ALL
            .into_iter()
            .find(|field| field.as_str() == key)
    }

    /// 中文标签（explain 用）。
    pub const fn label_zh(self) -> &'static str {
        match self {
            ConditionField::LogicalCores => "逻辑核",
            ConditionField::PhysicalCores => "物理核",
            ConditionField::RamGb => "内存",
            ConditionField::RamMb => "内存",
            ConditionField::GpuVendor => "显卡厂商",
            ConditionField::IsElevated => "管理员权限",
        }
    }

    /// 英文标签（explain 用）。
    pub const fn label_en(self) -> &'static str {
        match self {
            ConditionField::LogicalCores => "logical cores",
            ConditionField::PhysicalCores => "physical cores",
            ConditionField::RamGb => "RAM",
            ConditionField::RamMb => "RAM",
            ConditionField::GpuVendor => "GPU vendor",
            ConditionField::IsElevated => "administrator",
        }
    }

    /// 中文单位（无单位时为空串）。
    pub const fn unit_zh(self) -> &'static str {
        match self {
            ConditionField::RamGb => "GB",
            ConditionField::RamMb => "MB",
            _ => "",
        }
    }

    /// 英文单位（无单位时为空串；GB/MB 在英文里按 GiB/MiB 口径表述）。
    pub const fn unit_en(self) -> &'static str {
        match self {
            ConditionField::RamGb => "GiB",
            ConditionField::RamMb => "MiB",
            _ => "",
        }
    }
}

impl fmt::Display for ConditionField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 条件取值（TOML 里的整数 / 字符串 / 布尔）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", untagged)]
pub enum ConditionValue {
    /// 整数。
    Int(i64),
    /// 文本。
    Text(String),
    /// 布尔。
    Bool(bool),
}

impl ConditionValue {
    /// 整数视图。
    pub const fn as_int(&self) -> Option<i64> {
        match self {
            ConditionValue::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// 文本视图。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ConditionValue::Text(value) => Some(value),
            _ => None,
        }
    }

    /// 布尔视图。
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            ConditionValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// 类型名（错误消息用）。
    pub const fn type_name(&self) -> &'static str {
        match self {
            ConditionValue::Int(_) => "integer",
            ConditionValue::Text(_) => "string",
            ConditionValue::Bool(_) => "boolean",
        }
    }
}

impl fmt::Display for ConditionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConditionValue::Int(value) => write!(f, "{value}"),
            ConditionValue::Text(value) => f.write_str(value),
            ConditionValue::Bool(value) => write!(f, "{value}"),
        }
    }
}

/// 显示语言（describe 用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Zh,
    En,
}

impl Lang {
    const fn and(self) -> &'static str {
        match self {
            Lang::Zh => " 且 ",
            Lang::En => " and ",
        }
    }

    const fn yes_no(self, value: bool) -> &'static str {
        match (self, value) {
            (Lang::Zh, true) => "是",
            (Lang::Zh, false) => "否",
            (Lang::En, true) => "yes",
            (Lang::En, false) => "no",
        }
    }

    const fn none(self) -> &'static str {
        match self {
            Lang::Zh => "无适配器",
            Lang::En => "no adapter",
        }
    }
}

/// 单个字段的比较集合：所有给出的算子必须**同时**满足。
///
/// 字段私有 ⇒ 只能经构造器建立，因此"空比较"与"矛盾比较"在类型层面被收窄，
/// 校验错误集中在 [`Comparison::validate`] 一处。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Comparison {
    gt: Option<ConditionValue>,
    gte: Option<ConditionValue>,
    lt: Option<ConditionValue>,
    lte: Option<ConditionValue>,
    eq: Option<ConditionValue>,
    ne: Option<ConditionValue>,
}

impl Comparison {
    /// 空比较（任何值都不满足；校验层会拒绝，仅用于构造过程）。
    pub const fn empty() -> Self {
        Self {
            gt: None,
            gte: None,
            lt: None,
            lte: None,
            eq: None,
            ne: None,
        }
    }

    /// `> value`。
    pub fn above(value: i64) -> Self {
        Self {
            gt: Some(ConditionValue::Int(value)),
            ..Self::empty()
        }
    }

    /// `>= value`。
    pub fn at_least(value: i64) -> Self {
        Self {
            gte: Some(ConditionValue::Int(value)),
            ..Self::empty()
        }
    }

    /// `< value`。
    pub fn below(value: i64) -> Self {
        Self {
            lt: Some(ConditionValue::Int(value)),
            ..Self::empty()
        }
    }

    /// `<= value`。
    pub fn at_most(value: i64) -> Self {
        Self {
            lte: Some(ConditionValue::Int(value)),
            ..Self::empty()
        }
    }

    /// `== value`（整数）。
    pub fn equals_int(value: i64) -> Self {
        Self {
            eq: Some(ConditionValue::Int(value)),
            ..Self::empty()
        }
    }

    /// `== value`（任意取值类型；标量简写 `logical_cores = 8` 走这里）。
    pub fn equals(value: ConditionValue) -> Self {
        Self {
            eq: Some(value),
            ..Self::empty()
        }
    }

    /// 由各算子直接构造（TOML 解析路径使用；语义校验交给 [`Comparison::validate`]）。
    pub fn from_operators(
        gt: Option<ConditionValue>,
        gte: Option<ConditionValue>,
        lt: Option<ConditionValue>,
        lte: Option<ConditionValue>,
        eq: Option<ConditionValue>,
        ne: Option<ConditionValue>,
    ) -> Self {
        Self {
            gt,
            gte,
            lt,
            lte,
            eq,
            ne,
        }
    }

    /// `!= value`（整数）。
    pub fn not_equals_int(value: i64) -> Self {
        Self {
            ne: Some(ConditionValue::Int(value)),
            ..Self::empty()
        }
    }

    /// `== value`（文本）。
    pub fn equals_text(value: impl Into<String>) -> Self {
        Self {
            eq: Some(ConditionValue::Text(value.into())),
            ..Self::empty()
        }
    }

    /// `!= value`（文本）。
    pub fn not_equals_text(value: impl Into<String>) -> Self {
        Self {
            ne: Some(ConditionValue::Text(value.into())),
            ..Self::empty()
        }
    }

    /// `== value`（布尔）。
    pub fn equals_bool(value: bool) -> Self {
        Self {
            eq: Some(ConditionValue::Bool(value)),
            ..Self::empty()
        }
    }

    /// `!= value`（布尔）。
    pub fn not_equals_bool(value: bool) -> Self {
        Self {
            ne: Some(ConditionValue::Bool(value)),
            ..Self::empty()
        }
    }

    /// 追加下界（`>=`），用于测试与程序化构造区间。
    #[must_use]
    pub fn and_at_least(mut self, value: i64) -> Self {
        self.gte = Some(ConditionValue::Int(value));
        self
    }

    /// 追加上界（`<`）。
    #[must_use]
    pub fn and_below(mut self, value: i64) -> Self {
        self.lt = Some(ConditionValue::Int(value));
        self
    }

    /// `>` 算子。
    pub const fn gt(&self) -> Option<&ConditionValue> {
        self.gt.as_ref()
    }

    /// `>=` 算子。
    pub const fn gte(&self) -> Option<&ConditionValue> {
        self.gte.as_ref()
    }

    /// `<` 算子。
    pub const fn lt(&self) -> Option<&ConditionValue> {
        self.lt.as_ref()
    }

    /// `<=` 算子。
    pub const fn lte(&self) -> Option<&ConditionValue> {
        self.lte.as_ref()
    }

    /// `==` 算子。
    pub const fn eq(&self) -> Option<&ConditionValue> {
        self.eq.as_ref()
    }

    /// `!=` 算子。
    pub const fn ne(&self) -> Option<&ConditionValue> {
        self.ne.as_ref()
    }

    /// 是否一个算子都没有（校验层会拒绝）。
    pub fn is_empty(&self) -> bool {
        self.gt.is_none()
            && self.gte.is_none()
            && self.lt.is_none()
            && self.lte.is_none()
            && self.eq.is_none()
            && self.ne.is_none()
    }

    /// 在该字段的语义下做完整校验；返回稳定英文错误消息（由调用方补上文件/行号）。
    pub fn validate(&self, field: ConditionField) -> Result<(), String> {
        if self.is_empty() {
            return Err(format!(
                "comparison for `{}` is empty: give at least one of gt/gte/lt/lte/eq/ne",
                field.as_str()
            ));
        }
        if self.gt.is_some() && self.gte.is_some() {
            return Err(format!(
                "`{}` must not combine `gt` and `gte` (pick one lower bound)",
                field.as_str()
            ));
        }
        if self.lt.is_some() && self.lte.is_some() {
            return Err(format!(
                "`{}` must not combine `lt` and `lte` (pick one upper bound)",
                field.as_str()
            ));
        }
        if self.eq.is_some() && self.ne.is_some() {
            return Err(format!(
                "`{}` must not combine `eq` and `ne`: they can never both hold",
                field.as_str()
            ));
        }

        if field.is_numeric() {
            for (name, value) in [
                ("gt", &self.gt),
                ("gte", &self.gte),
                ("lt", &self.lt),
                ("lte", &self.lte),
                ("eq", &self.eq),
                ("ne", &self.ne),
            ] {
                if let Some(value) = value {
                    if value.as_int().is_none() {
                        return Err(format!(
                            "`{}` expects an integer comparison value, but `{}` is a {}",
                            field.as_str(),
                            name,
                            value.type_name()
                        ));
                    }
                }
            }
            return self.validate_numeric_bounds(field);
        }

        if field.is_text() {
            for (name, value) in [("eq", &self.eq), ("ne", &self.ne)] {
                if let Some(value) = value {
                    let text = value.as_text().ok_or_else(|| {
                        format!(
                            "`{}` expects a string comparison value, but `{}` is a {}",
                            field.as_str(),
                            name,
                            value.type_name()
                        )
                    })?;
                    if parse_gpu_vendor(text).is_none() {
                        return Err(format!(
                            "`{}` does not accept `{text}`; expected one of {}",
                            field.as_str(),
                            GPU_VENDOR_VALUES.join(", ")
                        ));
                    }
                }
            }
            if self.gt.is_some() || self.gte.is_some() || self.lt.is_some() || self.lte.is_some() {
                return Err(format!(
                    "`{}` supports only `eq` and `ne` (it is not ordered)",
                    field.as_str()
                ));
            }
            return Ok(());
        }

        // 布尔字段。
        for (name, value) in [("eq", &self.eq), ("ne", &self.ne)] {
            if let Some(value) = value {
                if value.as_bool().is_none() {
                    return Err(format!(
                        "`{}` expects a boolean comparison value, but `{}` is a {}",
                        field.as_str(),
                        name,
                        value.type_name()
                    ));
                }
            }
        }
        if self.gt.is_some() || self.gte.is_some() || self.lt.is_some() || self.lte.is_some() {
            return Err(format!(
                "`{}` supports only `eq` and `ne` (it is not ordered)",
                field.as_str()
            ));
        }
        Ok(())
    }

    /// 数值字段的区间矛盾检查（例如 `gte = 16, lt = 8` 永远不可能成立）。
    fn validate_numeric_bounds(&self, field: ConditionField) -> Result<(), String> {
        let lower = self
            .gt
            .as_ref()
            .or(self.gte.as_ref())
            .and_then(ConditionValue::as_int);
        let lower_inclusive = self.gt.is_none();
        let upper = self
            .lt
            .as_ref()
            .or(self.lte.as_ref())
            .and_then(ConditionValue::as_int);
        let upper_inclusive = self.lt.is_none();

        if let (Some(lower), Some(upper)) = (lower, upper) {
            let impossible =
                lower > upper || (lower == upper && !(lower_inclusive && upper_inclusive));
            if impossible {
                return Err(format!(
                    "`{}` has an empty range: the lower bound and the upper bound can never both hold",
                    field.as_str()
                ));
            }
        }

        if let Some(eq) = self.eq.as_ref().and_then(ConditionValue::as_int) {
            if let Some(lower) = self.gte.as_ref().and_then(ConditionValue::as_int) {
                if eq < lower {
                    return Err(format!(
                        "`{}`: `eq = {eq}` contradicts `gte = {lower}`",
                        field.as_str()
                    ));
                }
            }
            if let Some(lower) = self.gt.as_ref().and_then(ConditionValue::as_int) {
                if eq <= lower {
                    return Err(format!(
                        "`{}`: `eq = {eq}` contradicts `gt = {lower}`",
                        field.as_str()
                    ));
                }
            }
            if let Some(upper) = self.lte.as_ref().and_then(ConditionValue::as_int) {
                if eq > upper {
                    return Err(format!(
                        "`{}`: `eq = {eq}` contradicts `lte = {upper}`",
                        field.as_str()
                    ));
                }
            }
            if let Some(upper) = self.lt.as_ref().and_then(ConditionValue::as_int) {
                if eq >= upper {
                    return Err(format!(
                        "`{}`: `eq = {eq}` contradicts `lt = {upper}`",
                        field.as_str()
                    ));
                }
            }
        }
        Ok(())
    }

    /// 整数求值。校验通过的比较永远不会 panic。
    pub fn matches_int(&self, value: i64) -> bool {
        if let Some(bound) = self.gt.as_ref().and_then(ConditionValue::as_int) {
            if value <= bound {
                return false;
            }
        }
        if let Some(bound) = self.gte.as_ref().and_then(ConditionValue::as_int) {
            if value < bound {
                return false;
            }
        }
        if let Some(bound) = self.lt.as_ref().and_then(ConditionValue::as_int) {
            if value >= bound {
                return false;
            }
        }
        if let Some(bound) = self.lte.as_ref().and_then(ConditionValue::as_int) {
            if value > bound {
                return false;
            }
        }
        if let Some(bound) = self.eq.as_ref().and_then(ConditionValue::as_int) {
            if value != bound {
                return false;
            }
        }
        if let Some(bound) = self.ne.as_ref().and_then(ConditionValue::as_int) {
            if value == bound {
                return false;
            }
        }
        true
    }

    /// 文本求值（大小写不敏感）。
    pub fn matches_text(&self, value: &str) -> bool {
        if let Some(bound) = self.eq.as_ref().and_then(ConditionValue::as_text) {
            if !value.eq_ignore_ascii_case(bound) {
                return false;
            }
        }
        if let Some(bound) = self.ne.as_ref().and_then(ConditionValue::as_text) {
            if value.eq_ignore_ascii_case(bound) {
                return false;
            }
        }
        true
    }

    /// 布尔求值。
    pub fn matches_bool(&self, value: bool) -> bool {
        if let Some(bound) = self.eq.as_ref().and_then(ConditionValue::as_bool) {
            if value != bound {
                return false;
            }
        }
        if let Some(bound) = self.ne.as_ref().and_then(ConditionValue::as_bool) {
            if value == bound {
                return false;
            }
        }
        true
    }

    /// 生成人类可读片段（不含字段名），例如 `≥ 16`、`= nvidia`、`≠ none`。
    fn describe_ops(&self, field: ConditionField, lang: Lang) -> String {
        let mut parts: Vec<String> = Vec::new();
        let render = |op: &str, value: &ConditionValue| -> String {
            match value {
                ConditionValue::Bool(flag) => format!("{op} {}", lang.yes_no(*flag)),
                ConditionValue::Int(number) => {
                    let unit = match lang {
                        Lang::Zh => field.unit_zh(),
                        Lang::En => field.unit_en(),
                    };
                    if unit.is_empty() {
                        format!("{op} {number}")
                    } else {
                        format!("{op} {number} {unit}")
                    }
                }
                ConditionValue::Text(text) => {
                    if field == ConditionField::GpuVendor && text.eq_ignore_ascii_case("none") {
                        format!("{op} {}", lang.none())
                    } else {
                        format!("{op} {text}")
                    }
                }
            }
        };
        for (op, value) in [
            (">", &self.gt),
            ("≥", &self.gte),
            ("<", &self.lt),
            ("≤", &self.lte),
            ("=", &self.eq),
            ("≠", &self.ne),
        ] {
            if let Some(value) = value {
                parts.push(render(op, value));
            }
        }
        parts.join(lang.and())
    }
}

/// `gpu_vendor` 允许的取值（`none` 表示没有探测到显示适配器）。
pub const GPU_VENDOR_VALUES: [&str; 6] = ["nvidia", "amd", "intel", "microsoft", "unknown", "none"];

/// 解析 `gpu_vendor` 取值。`none` 表示"没有适配器"，与"认不出厂商"（`unknown`）区分开。
pub fn parse_gpu_vendor(text: &str) -> Option<GpuVendorValue> {
    let normalized = text.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "nvidia" | "nv" => Some(GpuVendorValue::Vendor(gopt_hal::GpuVendor::Nvidia)),
        "amd" | "ati" | "radeon" => Some(GpuVendorValue::Vendor(gopt_hal::GpuVendor::Amd)),
        "intel" => Some(GpuVendorValue::Vendor(gopt_hal::GpuVendor::Intel)),
        "microsoft" | "msft" | "basic" => {
            Some(GpuVendorValue::Vendor(gopt_hal::GpuVendor::Microsoft))
        }
        "unknown" => Some(GpuVendorValue::Vendor(gopt_hal::GpuVendor::Unknown)),
        "none" | "missing" => Some(GpuVendorValue::None),
        _ => None,
    }
}

/// `gpu_vendor` 条件的取值：某个厂商，或"没有适配器"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuVendorValue {
    /// 探测到的厂商（`unknown` 表示适配器存在但厂商无法识别）。
    Vendor(gopt_hal::GpuVendor),
    /// 没有探测到显示适配器（DXGI 返回空或降级）。
    None,
}

/// 条件的一项：字段 + 比较 + 来源行号（供 explain/审计）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionTerm {
    field: ConditionField,
    comparison: Comparison,
    line: Option<u32>,
}

impl ConditionTerm {
    /// 构造。
    pub fn new(field: ConditionField, comparison: Comparison, line: Option<u32>) -> Self {
        Self {
            field,
            comparison,
            line,
        }
    }

    /// 字段。
    pub const fn field(&self) -> ConditionField {
        self.field
    }

    /// 比较。
    pub const fn comparison(&self) -> &Comparison {
        &self.comparison
    }

    /// 来源行号。
    pub const fn line(&self) -> Option<u32> {
        self.line
    }

    /// 该项是否满足。
    pub fn matches(&self, input: &EvalInput) -> bool {
        match self.field {
            ConditionField::LogicalCores => self
                .comparison
                .matches_int(i64::from(input.hardware().logical_cores)),
            ConditionField::PhysicalCores => self
                .comparison
                .matches_int(i64::from(input.hardware().physical_cores)),
            ConditionField::RamGb => self.comparison.matches_int(i64::from(input.ram_gb())),
            ConditionField::RamMb => self.comparison.matches_int(saturating_i64(input.ram_mb())),
            ConditionField::GpuVendor => {
                // `none`（没有适配器）与 `unknown`（适配器存在但认不出厂商）刻意区分开。
                let actual = input.gpu_vendor();
                let expected = match self
                    .comparison
                    .eq()
                    .or_else(|| self.comparison.ne())
                    .and_then(ConditionValue::as_text)
                    .and_then(parse_gpu_vendor)
                {
                    Some(expected) => expected,
                    None => return false,
                };
                let hit = match (expected, actual) {
                    (GpuVendorValue::None, None) => true,
                    (GpuVendorValue::None, Some(_)) => false,
                    (GpuVendorValue::Vendor(vendor), Some(actual)) => actual == vendor,
                    (GpuVendorValue::Vendor(_), None) => false,
                };
                if self.comparison.eq().is_some() {
                    hit
                } else {
                    !hit
                }
            }
            ConditionField::IsElevated => self.comparison.matches_bool(input.is_elevated()),
        }
    }

    /// 中文描述，例如 `内存 ≥ 16 GB`。
    pub fn describe_zh(&self) -> String {
        self.describe(Lang::Zh)
    }

    /// 英文描述，例如 `RAM >= 16 GiB`。
    pub fn describe_en(&self) -> String {
        self.describe(Lang::En)
    }

    fn describe(&self, lang: Lang) -> String {
        let label = match lang {
            Lang::Zh => self.field.label_zh(),
            Lang::En => self.field.label_en(),
        };
        format!("{label} {}", self.comparison.describe_ops(self.field, lang))
    }
}

/// 一组条件的合取（AND）。空条件 = 无条件命中。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Condition {
    terms: Vec<ConditionTerm>,
}

impl Condition {
    /// 由若干项构造（顺序即 `describe` 顺序）。
    pub fn new(terms: Vec<ConditionTerm>) -> Self {
        Self { terms }
    }

    /// 无条件（永远命中）。
    pub fn always() -> Self {
        Self { terms: Vec::new() }
    }

    /// 全部项。
    pub fn terms(&self) -> &[ConditionTerm] {
        &self.terms
    }

    /// 是否无条件。
    pub fn is_always(&self) -> bool {
        self.terms.is_empty()
    }

    /// 求值：所有项都满足才命中。
    pub fn evaluate(&self, input: &EvalInput) -> bool {
        self.terms.iter().all(|term| term.matches(input))
    }

    /// 中文描述（用于 Plan 的"条件不满足"理由）。
    pub fn describe_zh(&self) -> String {
        self.describe(Lang::Zh)
    }

    /// 英文描述。
    pub fn describe_en(&self) -> String {
        self.describe(Lang::En)
    }

    fn describe(&self, lang: Lang) -> String {
        let separator = lang.and();
        self.terms
            .iter()
            .map(|term| term.describe(lang))
            .collect::<Vec<String>>()
            .join(separator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gopt_hal::{GpuInfo, GpuVendor, HardwareInfo, ProcessorGroup};

    fn hardware(logical: u32, physical: u32, ram_mb: u64, gpu: Option<GpuVendor>) -> HardwareInfo {
        HardwareInfo {
            cpu_model: "Test CPU".to_string(),
            physical_cores: physical,
            logical_cores: logical,
            supports_hyper_threading: logical > physical,
            cpu_base_freq_mhz: 3600,
            core_layout: Vec::new(),
            processor_groups: vec![ProcessorGroup::new(0, logical.min(64))],
            gpu: gpu.map(|vendor| GpuInfo {
                vendor,
                vendor_id: 0,
                device_id: 0,
                model: "Test GPU".to_string(),
                vram_mb: 8192,
                driver_version: None,
                is_hardware: true,
                is_software_adapter: false,
            }),
            system_ram_mb: ram_mb,
            available_ram_mb: ram_mb / 2,
            large_pages_available: false,
            warnings: Vec::new(),
        }
    }

    fn input(
        logical: u32,
        physical: u32,
        ram_mb: u64,
        gpu: Option<GpuVendor>,
        elevated: bool,
    ) -> EvalInput {
        EvalInput::new(hardware(logical, physical, ram_mb, gpu), elevated)
    }

    #[test]
    fn numeric_operators_match_the_expected_matrix() {
        let target = input(16, 8, 32768, Some(GpuVendor::Nvidia), false);
        assert!(Comparison::above(15).matches_int(16));
        assert!(!Comparison::above(16).matches_int(16));
        assert!(Comparison::at_least(16).matches_int(16));
        assert!(!Comparison::at_least(17).matches_int(16));
        assert!(Comparison::below(17).matches_int(16));
        assert!(!Comparison::below(16).matches_int(16));
        assert!(Comparison::at_most(16).matches_int(16));
        assert!(!Comparison::at_most(15).matches_int(16));
        assert!(Comparison::equals_int(16).matches_int(16));
        assert!(!Comparison::equals_int(8).matches_int(16));
        assert!(Comparison::not_equals_int(8).matches_int(16));
        assert!(!Comparison::not_equals_int(16).matches_int(16));

        // 区间：8 <= ram_gb < 16
        let range = Comparison::at_least(8).and_below(16);
        assert!(range.validate(ConditionField::RamGb).is_ok());
        assert!(range.matches_int(8));
        assert!(range.matches_int(15));
        assert!(!range.matches_int(16));
        assert!(!range.matches_int(7));

        // 条件在 EvalInput 上的端到端行为（32GB 内存 → ram_gb = 32）。
        assert_eq!(target.ram_gb(), 32);
        let condition = Condition::new(vec![ConditionTerm::new(
            ConditionField::RamGb,
            range,
            Some(3),
        )]);
        assert!(!condition.evaluate(&target));
        assert!(condition.evaluate(&input(16, 8, 12288, Some(GpuVendor::Amd), false)));
    }

    #[test]
    fn ram_gb_is_truncated_like_the_cpp_thresholds() {
        // 16384 MiB -> 16 GiB；15872 MiB（15.5 GiB）-> 15 GiB，与 C++ `systemRamMB < 16384` 同档。
        assert_eq!(input(8, 4, 16384, None, false).ram_gb(), 16);
        assert_eq!(input(8, 4, 15872, None, false).ram_gb(), 15);
        assert_eq!(input(8, 4, 8191, None, false).ram_gb(), 7);
        assert_eq!(input(8, 4, 8192, None, false).ram_mb(), 8192);
    }

    #[test]
    fn validation_rejects_conflicting_and_mistyped_comparisons() {
        assert!(Comparison::above(1)
            .and_at_least(2)
            .validate(ConditionField::RamGb)
            .is_err());
        assert!(Comparison::at_least(16)
            .and_below(8)
            .validate(ConditionField::RamGb)
            .is_err());
        assert!(Comparison::at_least(16)
            .and_below(16)
            .validate(ConditionField::RamGb)
            .is_err());
        // gte = 16 与 lte = 16 是合法的单点区间。
        let mut point = Comparison::at_least(16);
        point.lte = Some(ConditionValue::Int(16));
        assert!(point.validate(ConditionField::RamGb).is_ok());
        assert!(Comparison::empty().validate(ConditionField::RamGb).is_err());

        let mut both = Comparison::equals_int(1);
        both.ne = Some(ConditionValue::Int(2));
        assert!(both.validate(ConditionField::RamGb).is_err());

        let mut wrong_type = Comparison::empty();
        wrong_type.gte = Some(ConditionValue::Text("many".to_string()));
        assert!(wrong_type.validate(ConditionField::LogicalCores).is_err());

        assert!(Comparison::at_least(3)
            .validate(ConditionField::GpuVendor)
            .is_err());
        assert!(Comparison::equals_text("nvidia")
            .validate(ConditionField::GpuVendor)
            .is_ok());
        assert!(Comparison::equals_text("3dfx")
            .validate(ConditionField::GpuVendor)
            .is_err());
        assert!(Comparison::equals_bool(true)
            .validate(ConditionField::IsElevated)
            .is_ok());
        assert!(Comparison::equals_int(1)
            .validate(ConditionField::IsElevated)
            .is_err());

        // eq 与区间矛盾。
        let mut contradiction = Comparison::equals_int(4);
        contradiction.gte = Some(ConditionValue::Int(8));
        assert!(contradiction
            .validate(ConditionField::LogicalCores)
            .is_err());
    }

    #[test]
    fn gpu_vendor_conditions_handle_none_and_unknown() {
        let with_nvidia = input(8, 4, 16384, Some(GpuVendor::Nvidia), false);
        let no_gpu = input(8, 4, 16384, None, false);

        let eq_nvidia = Condition::new(vec![ConditionTerm::new(
            ConditionField::GpuVendor,
            Comparison::equals_text("nvidia"),
            None,
        )]);
        assert!(eq_nvidia.evaluate(&with_nvidia));
        assert!(!eq_nvidia.evaluate(&no_gpu));

        let ne_none = Condition::new(vec![ConditionTerm::new(
            ConditionField::GpuVendor,
            Comparison::not_equals_text("none"),
            None,
        )]);
        assert!(ne_none.evaluate(&with_nvidia));
        assert!(!ne_none.evaluate(&no_gpu));

        let eq_none = Condition::new(vec![ConditionTerm::new(
            ConditionField::GpuVendor,
            Comparison::equals_text("none"),
            None,
        )]);
        assert!(eq_none.evaluate(&no_gpu));
        assert!(!eq_none.evaluate(&with_nvidia));

        let ne_amd = Condition::new(vec![ConditionTerm::new(
            ConditionField::GpuVendor,
            Comparison::not_equals_text("amd"),
            None,
        )]);
        assert!(ne_amd.evaluate(&with_nvidia));
        assert!(ne_amd.evaluate(&no_gpu));

        // 认不出厂商 != 没有适配器。
        let unknown = input(8, 4, 16384, Some(GpuVendor::Unknown), false);
        assert!(!eq_none.evaluate(&unknown));
        assert!(ne_none.evaluate(&unknown));
    }

    #[test]
    fn elevation_and_empty_conditions() {
        let elevated = input(8, 4, 16384, None, true);
        let rule = Condition::new(vec![ConditionTerm::new(
            ConditionField::IsElevated,
            Comparison::equals_bool(true),
            None,
        )]);
        assert!(rule.evaluate(&elevated));
        assert!(!rule.evaluate(&input(8, 4, 16384, None, false)));

        let always = Condition::always();
        assert!(always.is_always());
        assert!(always.evaluate(&elevated));
        assert_eq!(always.describe_zh(), "");
        assert_eq!(always.describe_en(), "");
    }

    #[test]
    fn describe_is_bilingual_and_readable() {
        let term = ConditionTerm::new(
            ConditionField::RamGb,
            Comparison::at_least(8).and_below(16),
            Some(9),
        );
        assert_eq!(term.describe_zh(), "内存 ≥ 8 GB 且 < 16 GB");
        assert_eq!(term.describe_en(), "RAM ≥ 8 GiB and < 16 GiB");
        assert_eq!(term.line(), Some(9));

        let vendor = ConditionTerm::new(
            ConditionField::GpuVendor,
            Comparison::not_equals_text("none"),
            None,
        );
        assert_eq!(vendor.describe_zh(), "显卡厂商 ≠ 无适配器");
        assert_eq!(vendor.describe_en(), "GPU vendor ≠ no adapter");

        let elevated = ConditionTerm::new(
            ConditionField::IsElevated,
            Comparison::equals_bool(true),
            None,
        );
        assert_eq!(elevated.describe_zh(), "管理员权限 = 是");

        let combined = Condition::new(vec![
            ConditionTerm::new(ConditionField::LogicalCores, Comparison::at_least(8), None),
            ConditionTerm::new(ConditionField::RamGb, Comparison::at_least(16), None),
        ]);
        assert_eq!(combined.describe_zh(), "逻辑核 ≥ 8 且 内存 ≥ 16 GB");
        assert_eq!(combined.describe_en(), "logical cores ≥ 8 and RAM ≥ 16 GiB");
    }

    #[test]
    fn field_names_round_trip() {
        for field in ConditionField::ALL {
            assert_eq!(ConditionField::from_key(field.as_str()), Some(field));
        }
        assert_eq!(ConditionField::from_key("ram"), None);
        assert!(ConditionField::RamGb.is_numeric());
        assert!(ConditionField::GpuVendor.is_text());
        assert!(ConditionField::IsElevated.is_bool());
        assert_eq!(
            parse_gpu_vendor(" NVIDIA "),
            Some(GpuVendorValue::Vendor(GpuVendor::Nvidia))
        );
        assert_eq!(parse_gpu_vendor("none"), Some(GpuVendorValue::None));
        assert_eq!(parse_gpu_vendor("3dfx"), None);
    }
}
