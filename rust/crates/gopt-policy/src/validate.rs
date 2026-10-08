//! 严格校验：把 [`crate::raw`] 的"字面量镜像"变成 [`crate::model`] 的"受约束模型"。
//!
//! 这是**唯一**允许产生 [`PolicyDiagnostic`] 的地方，也是"报错带文件名与行号"的落点：
//!
//! 1. 语法/类型错误由 `toml` 给出 span，转成 1 基行号列号；
//! 2. 语义错误（动作叠加、`realtime` 优先级、空区间、掩码为 0……）在这里逐条检查，
//!    并把诊断钉在**键所在行**（[`Ctx::error_at_key`]），而不是外层表头上；
//! 3. 任何一条错误都只影响**这一个文件**：加载器会继续解析其它文件（错误不传播、不 panic）。
//!
//! 校验清单（每条都有对应的集成测试）：
//!
//! * 文件级：`schema` 必须是 1；至少一个 `[[game]]`；未知键一律报错；
//! * 游戏级：`id` kebab-case 且文件内唯一；`name_zh` / `name_en` 非空；
//!   `match` 是 exe 名（不允许路径、不允许裸 `*`）；别名不重复、不与主模式重复；
//! * 规则级：`id` 非空且游戏内唯一（缺省按 `游戏id.动作` 推导）；`action` 恰好一个动作；
//! * 条件级：算子与字段类型匹配、区间不矛盾、`gpu_vendor` 取值受白名单限制；
//! * 动作级：`priority` 不得高于 HIGH（`realtime` 直接 PolicyDenied）、
//!   `affinity` 的 `mask` 与 `reserve_cores` 互斥且掩码非 0、
//!   `working_set` 的 `min_mb > 0` 且 `max_mb >= min_mb`、
//!   `run_entries` 的 `hive` 受白名单限制且名单非空。

use core::ops::Range;

use gopt_hal::{PriorityClass, RunEntry, WorkingSetLimits};

use crate::condition::{Comparison, Condition, ConditionTerm, ConditionValue};
use crate::error::{PolicyDiagnostic, PolicyOrigin};
use crate::matching::validate_exe_pattern;
use crate::model::{
    Action, AffinitySpec, GamePolicy, PowerSchemeChoice, ReserveSide, Rule, RunHiveSpec,
};
use crate::plan::Reason;
use crate::raw::{
    RawAction, RawAffinity, RawComparison, RawGame, RawLeaf, RawPolicyFile, RawPowerScheme,
    RawRule, RawRunEntries, RawWhen, RawWorkingSet,
};

/// 本构建理解的策略文件格式版本。
pub const SCHEMA_VERSION: i64 = 1;

/// 解析并严格校验一个策略文件的内容。
///
/// * `text`：TOML 全文；
/// * `origin`：内置文件名或用户文件路径，只用于诊断（也随策略一起保留，便于 explain 溯源）。
///
/// 成功时返回该文件里的全部游戏策略（顺序 = 文件中的声明顺序）；
/// 失败时返回**第一条**错误（带 `文件:行:列`），其余文件仍会被加载器继续处理。
pub fn parse_policy_file(
    text: &str,
    origin: PolicyOrigin,
) -> Result<Vec<GamePolicy>, PolicyDiagnostic> {
    let raw: RawPolicyFile = toml::from_str(text).map_err(|err| {
        let (line, column) = match err.span() {
            Some(span) => {
                let (line, column) = line_col(text, span.start);
                (Some(line), Some(column))
            }
            None => (None, None),
        };
        PolicyDiagnostic::error(origin.clone(), line, column, err.message().to_string())
    })?;

    let ctx = Ctx {
        text,
        origin: &origin,
    };
    build_games(&raw, &ctx)
}

/// 1 基（行, 列）；`offset` 超出文本时钳制到末尾。
fn line_col(text: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(text.len());
    let mut line = 1u32;
    let mut column = 1u32;
    for (index, character) in text.char_indices() {
        if index >= offset {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// 诊断上下文：源文本 + 来源，负责把 span / 键名翻译成"文件:行:列"。
struct Ctx<'a> {
    text: &'a str,
    origin: &'a PolicyOrigin,
}

impl Ctx<'_> {
    /// 以 span 起点为位置生成错误；没有 span 时退化为不带行号的诊断。
    fn error(&self, span: Option<Range<usize>>, message: impl Into<String>) -> PolicyDiagnostic {
        match span {
            Some(span) => {
                let (line, column) = line_col(self.text, span.start);
                PolicyDiagnostic::error(self.origin.clone(), Some(line), Some(column), message)
            }
            None => PolicyDiagnostic::error(self.origin.clone(), None, None, message),
        }
    }

    /// 先在 span 内找 `key =` 的位置，把诊断钉在键所在行（而不是外层表头）。
    fn error_at_key(
        &self,
        key: &str,
        span: Option<Range<usize>>,
        message: impl Into<String>,
    ) -> PolicyDiagnostic {
        match span {
            Some(span) => {
                let offset = self
                    .key_offset(key, Some(span.clone()))
                    .unwrap_or(span.start);
                let (line, column) = line_col(self.text, offset);
                PolicyDiagnostic::error(self.origin.clone(), Some(line), Some(column), message)
            }
            None => self.error(None, message),
        }
    }

    /// 在 `within` 区间内（缺省全文）寻找 `key` 后紧跟 `=` 的字节偏移。
    fn key_offset(&self, key: &str, within: Option<Range<usize>>) -> Option<usize> {
        let (start, end) = match within {
            Some(range) => (
                range.start.min(self.text.len()),
                range.end.min(self.text.len()),
            ),
            None => (0, self.text.len()),
        };
        if start >= end || key.is_empty() {
            return None;
        }
        let haystack = self.text.get(start..end)?;
        let bytes = haystack.as_bytes();
        let mut cursor = 0usize;
        while let Some(found) = haystack[cursor..].find(key) {
            let at = cursor + found;
            let before_ok =
                at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
            let after = at + key.len();
            let after_ok =
                after <= haystack.len() && haystack[after..].trim_start().starts_with('=');
            if before_ok && after_ok {
                return Some(start + at);
            }
            cursor = at + key.len();
            if cursor >= haystack.len() {
                break;
            }
        }
        None
    }
}

fn build_games(raw: &RawPolicyFile, ctx: &Ctx<'_>) -> Result<Vec<GamePolicy>, PolicyDiagnostic> {
    if let Some(schema) = raw.schema.as_ref() {
        let version = *schema.get_ref();
        if version != SCHEMA_VERSION {
            return Err(ctx.error(
                Some(schema.span()),
                format!("unsupported policy schema version {version}; this build understands schema = {SCHEMA_VERSION}"),
            ));
        }
    }

    if raw.games.is_empty() {
        return Err(ctx.error(Some(0..0), "policy file contains no [[game]] section"));
    }

    let mut games: Vec<GamePolicy> = Vec::new();
    let mut ids: Vec<String> = Vec::new();

    for spanned_game in &raw.games {
        let game_span = spanned_game.span();
        let raw_game: &RawGame = spanned_game.get_ref();

        let id = raw_game.id.get_ref().trim().to_string();
        if id.is_empty() {
            return Err(ctx.error(Some(raw_game.id.span()), "game `id` must not be empty"));
        }
        if !is_kebab_id(&id) {
            return Err(ctx.error(
                Some(raw_game.id.span()),
                format!("game id `{id}` must be kebab-case (lowercase letters, digits and single dashes)"),
            ));
        }
        if ids.iter().any(|existing| existing == &id) {
            return Err(ctx.error(
                Some(raw_game.id.span()),
                format!("duplicate game id `{id}` in the same file"),
            ));
        }

        let name_zh = required_text(&raw_game.name_zh, "name_zh", ctx)?;
        let name_en = required_text(&raw_game.name_en, "name_en", ctx)?;
        let exe_match = build_pattern(&raw_game.exe_match, "match", ctx)?;
        let exe_aliases = build_patterns(&raw_game.exe_aliases, "exe_aliases", ctx)?;
        for alias in &exe_aliases {
            if alias.eq_ignore_ascii_case(&exe_match) {
                return Err(ctx.error(
                    Some(raw_game.exe_match.span()),
                    format!("`exe_aliases` repeats the primary pattern `{exe_match}`"),
                ));
            }
        }
        let name_aliases = build_names(&raw_game.name_aliases, "name_aliases", ctx)?;
        let description_zh = optional_text(&raw_game.description_zh);
        let description_en = optional_text(&raw_game.description_en);

        let rules = build_rules(&id, &raw_game.rules, ctx)?;

        let (line, _column) = line_col(ctx.text, game_span.start);
        games.push(
            GamePolicy::new(
                id.clone(),
                name_zh,
                name_en,
                exe_match,
                rules,
                ctx.origin.clone(),
            )
            .with_exe_aliases(exe_aliases)
            .with_name_aliases(name_aliases)
            .with_descriptions(description_zh, description_en)
            .with_line(Some(line)),
        );
        ids.push(id);
    }

    Ok(games)
}

fn build_rules(
    game_id: &str,
    raw_rules: &[toml::Spanned<RawRule>],
    ctx: &Ctx<'_>,
) -> Result<Vec<Rule>, PolicyDiagnostic> {
    let mut rules: Vec<Rule> = Vec::new();
    let mut ids: Vec<String> = Vec::new();

    for spanned_rule in raw_rules {
        let rule_span = spanned_rule.span();
        let raw_rule: &RawRule = spanned_rule.get_ref();
        let action = build_action(raw_rule.action.get_ref(), raw_rule.action.span(), ctx)?;

        let id = match raw_rule.id.as_ref() {
            Some(field) => required_text(field, "rule id", ctx)?,
            None => {
                // 缺省 id：`游戏id.动作`；同种动作的第 k 条（k >= 2）加 `-k` 后缀。
                let kind = action.kind();
                let previous = rules
                    .iter()
                    .filter(|rule| rule.action().kind() == kind)
                    .count();
                if previous == 0 {
                    format!("{game_id}.{}", kind.as_str())
                } else {
                    format!("{game_id}.{}-{}", kind.as_str(), previous + 1)
                }
            }
        };
        if ids.iter().any(|existing| existing == &id) {
            return Err(ctx.error(
                Some(rule_span.clone()),
                format!("duplicate rule id `{id}` in game `{game_id}`"),
            ));
        }

        let when = match raw_rule.when.as_ref() {
            Some(spanned_when) => {
                let condition = build_condition(spanned_when.get_ref(), ctx)?;
                if condition.is_always() {
                    None // `when = {}` 等价于无条件
                } else {
                    Some(condition)
                }
            }
            None => None,
        };

        let (line, _column) = line_col(ctx.text, rule_span.start);
        rules.push(Rule::new(id.clone(), when, action).with_line(Some(line)));
        ids.push(id);
    }

    Ok(rules)
}

fn build_condition(raw: &RawWhen, ctx: &Ctx<'_>) -> Result<Condition, PolicyDiagnostic> {
    let mut terms: Vec<ConditionTerm> = Vec::new();
    for (field, spanned) in raw.iter() {
        let span = spanned.span();
        let comparison = convert_comparison(spanned.get_ref());
        comparison
            .validate(field)
            .map_err(|message| ctx.error_at_key(field.as_str(), Some(span.clone()), message))?;
        let (line, _column) = line_col(ctx.text, span.start);
        terms.push(ConditionTerm::new(field, comparison, Some(line)));
    }
    Ok(Condition::new(terms))
}

fn convert_comparison(raw: &RawComparison) -> Comparison {
    let value = |leaf: &RawLeaf| match leaf {
        RawLeaf::Int(number) => ConditionValue::Int(*number),
        RawLeaf::Text(text) => ConditionValue::Text(text.clone()),
        RawLeaf::Bool(flag) => ConditionValue::Bool(*flag),
    };
    match raw {
        // 标量简写：`ram_gb = 16` ⇒ `eq = 16`
        RawComparison::Scalar(leaf) => Comparison::equals(value(leaf)),
        RawComparison::Ops(ops) => Comparison::from_operators(
            ops.gt.as_ref().map(value),
            ops.gte.as_ref().map(value),
            ops.lt.as_ref().map(value),
            ops.lte.as_ref().map(value),
            ops.eq.as_ref().map(value),
            ops.ne.as_ref().map(value),
        ),
    }
}

fn build_action(
    raw: &RawAction,
    span: Range<usize>,
    ctx: &Ctx<'_>,
) -> Result<Action, PolicyDiagnostic> {
    const EXPECTED: &str =
        "exactly one of `priority`, `affinity`, `working_set`, `power_scheme`, `run_entries` or `skip`";

    let provided = [
        raw.priority.is_some(),
        raw.affinity.is_some(),
        raw.working_set.is_some(),
        raw.power_scheme.is_some(),
        raw.run_entries.is_some(),
        raw.skip.is_some(),
    ];
    let count = provided.iter().filter(|flag| **flag).count();
    if count == 0 {
        return Err(ctx.error(Some(span), format!("action is empty: it needs {EXPECTED}")));
    }
    if count > 1 {
        return Err(ctx.error(
            Some(span),
            format!("action must contain {EXPECTED}, but {count} were given"),
        ));
    }

    if let Some(priority) = &raw.priority {
        let text = priority.class.get_ref().trim();
        let class = PriorityClass::parse(text).map_err(|err| {
            ctx.error(
                Some(priority.class.span()),
                format!("priority: {}", err.message()),
            )
        })?;
        return Ok(Action::Priority { class });
    }
    if let Some(affinity) = &raw.affinity {
        return build_affinity(affinity, span, ctx);
    }
    if let Some(working_set) = &raw.working_set {
        return build_working_set(working_set, span, ctx);
    }
    if let Some(power_scheme) = &raw.power_scheme {
        return build_power_scheme(power_scheme, ctx);
    }
    if let Some(run_entries) = &raw.run_entries {
        return build_run_entries(run_entries, span, ctx);
    }
    if let Some(skip) = &raw.skip {
        let zh = required_text(&skip.reason_zh, "skip.reason_zh", ctx)?;
        let en = required_text(&skip.reason_en, "skip.reason_en", ctx)?;
        return Ok(Action::Skip {
            reason: Reason::new(zh, en),
        });
    }

    // 不可达：上面的 count 检查已经保证恰好一个动作存在。
    Err(ctx.error(Some(span), format!("action must contain {EXPECTED}")))
}

fn build_affinity(
    raw: &RawAffinity,
    span: Range<usize>,
    ctx: &Ctx<'_>,
) -> Result<Action, PolicyDiagnostic> {
    let mask = match raw.mask.as_ref() {
        Some(field) => Some(parse_mask(field, ctx)?),
        None => None,
    };
    let reserve_cores = raw
        .reserve_cores
        .as_ref()
        .map(|field| *field.get_ref())
        .unwrap_or(0);
    let physical_only = raw
        .physical_only
        .as_ref()
        .map(|field| *field.get_ref())
        .unwrap_or(false);

    if let (Some(_), Some(field)) = (mask, raw.reserve_cores.as_ref()) {
        return Err(ctx.error(
            Some(field.span()),
            "affinity must not combine `mask` and `reserve_cores`: pick one binding intent",
        ));
    }
    if mask.is_none() && reserve_cores == 0 && !physical_only {
        return Err(ctx.error(
            Some(span),
            "affinity needs at least one of `reserve_cores`, `mask` or `physical_only = true`",
        ));
    }

    let reserve_from = match raw.reserve_from.as_ref() {
        Some(field) => {
            if mask.is_some() {
                return Err(ctx.error(
                    Some(field.span()),
                    "`reserve_from` only applies together with `reserve_cores`",
                ));
            }
            let text = field.get_ref().trim();
            ReserveSide::parse(text).ok_or_else(|| {
                ctx.error(
                    Some(field.span()),
                    format!("`reserve_from = \"{text}\"` is not supported; expected \"first\" or \"last\""),
                )
            })?
        }
        None => ReserveSide::default(),
    };

    Ok(Action::Affinity(AffinitySpec::new(
        reserve_cores,
        reserve_from,
        physical_only,
        mask,
    )))
}

fn parse_mask(field: &toml::Spanned<RawLeaf>, ctx: &Ctx<'_>) -> Result<u64, PolicyDiagnostic> {
    let span = field.span();
    let value = match field.get_ref() {
        RawLeaf::Int(number) => {
            if *number < 0 {
                return Err(ctx.error(
                    Some(span),
                    "affinity `mask` must be positive; use a decimal integer or a hex string like \"0xffff\"",
                ));
            }
            *number as u64
        }
        RawLeaf::Text(text) => parse_mask_text(text).ok_or_else(|| {
            ctx.error(
                Some(span.clone()),
                format!("affinity `mask` = \"{text}\" is not a valid mask; use a decimal integer or a hex string like \"0xffff\""),
            )
        })?,
        RawLeaf::Bool(_) => {
            return Err(ctx.error(
                Some(span),
                "affinity `mask` must be a decimal integer or a hex string, not a boolean",
            ))
        }
    };
    if value == 0 {
        return Err(ctx.error(
            Some(span),
            "affinity `mask` must not be zero: it would bind no processor at all",
        ));
    }
    Ok(value)
}

/// 掩码文本：`0x` 前缀或含十六进制字母 → 十六进制；否则十进制。
fn parse_mask_text(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let (digits, forced_hex) = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(rest) => (rest, true),
        None => (trimmed, false),
    };
    if digits.is_empty()
        || !digits
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return None;
    }
    if forced_hex
        || digits
            .chars()
            .any(|character| character.is_ascii_alphabetic())
    {
        u64::from_str_radix(digits, 16).ok()
    } else {
        digits.parse::<u64>().ok()
    }
}

fn build_working_set(
    raw: &RawWorkingSet,
    span: Range<usize>,
    ctx: &Ctx<'_>,
) -> Result<Action, PolicyDiagnostic> {
    let min_mb = match raw.min_mb.as_ref() {
        Some(field) => *field.get_ref(),
        None => return Err(ctx.error(
            Some(span),
            "working_set requires `min_mb` (MiB, greater than zero); `max_mb` alone is not a limit",
        )),
    };
    if min_mb == 0 {
        return Err(ctx.error(
            Some(
                raw.min_mb
                    .as_ref()
                    .map_or(span.clone(), toml::Spanned::span),
            ),
            "working_set `min_mb` must be greater than zero",
        ));
    }
    let max_mb = raw
        .max_mb
        .as_ref()
        .map(|field| *field.get_ref())
        .unwrap_or(0);
    if max_mb != 0 && max_mb < min_mb {
        return Err(ctx.error(
            Some(
                raw.max_mb
                    .as_ref()
                    .map_or(span.clone(), toml::Spanned::span),
            ),
            format!("working_set `max_mb` ({max_mb}) must not be smaller than `min_mb` ({min_mb})"),
        ));
    }
    let limits = WorkingSetLimits::from_mb(min_mb, max_mb)
        .map_err(|err| ctx.error(Some(span), format!("working_set: {}", err.message())))?;
    Ok(Action::WorkingSet { limits })
}

fn build_power_scheme(raw: &RawPowerScheme, ctx: &Ctx<'_>) -> Result<Action, PolicyDiagnostic> {
    let text = raw.scheme.get_ref().trim();
    let scheme = PowerSchemeChoice::parse(text).ok_or_else(|| {
        ctx.error(
            Some(raw.scheme.span()),
            format!("power_scheme `{text}` is not supported; expected \"high\" or \"balanced\""),
        )
    })?;
    Ok(Action::PowerScheme { scheme })
}

fn build_run_entries(
    raw: &RawRunEntries,
    span: Range<usize>,
    ctx: &Ctx<'_>,
) -> Result<Action, PolicyDiagnostic> {
    let hive = match raw.hive.as_ref() {
        Some(field) => {
            let text = field.get_ref().trim();
            RunHiveSpec::parse(text).ok_or_else(|| {
                ctx.error(
                    Some(field.span()),
                    format!("run_entries `hive = \"{text}\"` is not supported; expected \"hkcu\", \"hklm\" or \"both\""),
                )
            })?
        }
        None => RunHiveSpec::default(),
    };

    if raw.disable.is_empty() {
        return Err(ctx.error(
            Some(span),
            "run_entries requires at least one startup entry name in `disable`",
        ));
    }

    let mut names: Vec<String> = Vec::new();
    for field in &raw.disable {
        let value = field.get_ref().trim();
        if value.is_empty() {
            return Err(ctx.error(
                Some(field.span()),
                "run_entries `disable` must not contain an empty name",
            ));
        }
        // HAL 的启用/禁用以"展示名"为准；写不带前缀的名字最直观，带前缀也接受。
        let display = RunEntry::display_name(value).to_string();
        if display.is_empty() {
            return Err(ctx.error(
                Some(field.span()),
                "run_entries `disable` contains a name that is only the disabled prefix",
            ));
        }
        if names.iter().any(|existing| existing == &display) {
            return Err(ctx.error(
                Some(field.span()),
                format!("run_entries `disable` lists `{display}` twice"),
            ));
        }
        names.push(display);
    }

    let ignore_missing = raw
        .ignore_missing
        .as_ref()
        .map(|field| *field.get_ref())
        .unwrap_or(true);
    Ok(Action::RunEntries {
        hive,
        names,
        enabled: false,
        ignore_missing,
    })
}

fn required_text(
    field: &toml::Spanned<String>,
    name: &str,
    ctx: &Ctx<'_>,
) -> Result<String, PolicyDiagnostic> {
    let value = field.get_ref().trim().to_string();
    if value.is_empty() {
        return Err(ctx.error(Some(field.span()), format!("`{name}` must not be empty")));
    }
    Ok(value)
}

fn optional_text(field: &Option<toml::Spanned<String>>) -> String {
    match field {
        Some(field) => field.get_ref().trim().to_string(),
        None => String::new(),
    }
}

fn build_pattern(
    field: &toml::Spanned<String>,
    name: &str,
    ctx: &Ctx<'_>,
) -> Result<String, PolicyDiagnostic> {
    let value = field.get_ref().to_string();
    validate_exe_pattern(&value)
        .map_err(|message| ctx.error(Some(field.span()), format!("`{name}`: {message}")))?;
    Ok(value)
}

fn build_patterns(
    fields: &[toml::Spanned<String>],
    name: &str,
    ctx: &Ctx<'_>,
) -> Result<Vec<String>, PolicyDiagnostic> {
    let mut patterns: Vec<String> = Vec::new();
    for field in fields {
        let value = build_pattern(field, name, ctx)?;
        if patterns
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&value))
        {
            return Err(ctx.error(
                Some(field.span()),
                format!("`{name}` contains the duplicate pattern `{value}`"),
            ));
        }
        patterns.push(value);
    }
    Ok(patterns)
}

fn build_names(
    fields: &[toml::Spanned<String>],
    name: &str,
    ctx: &Ctx<'_>,
) -> Result<Vec<String>, PolicyDiagnostic> {
    let mut names: Vec<String> = Vec::new();
    for field in fields {
        let value = field.get_ref().trim().to_string();
        if value.is_empty() {
            return Err(ctx.error(
                Some(field.span()),
                format!("`{name}` must not contain an empty entry"),
            ));
        }
        if names.iter().any(|existing| existing == &value) {
            return Err(ctx.error(
                Some(field.span()),
                format!("`{name}` contains the duplicate entry `{value}`"),
            ));
        }
        names.push(value);
    }
    Ok(names)
}

fn is_kebab_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(text: &'a str, origin: &'a PolicyOrigin) -> Ctx<'a> {
        Ctx { text, origin }
    }

    #[test]
    fn line_col_counts_from_one() {
        let text = "a\nbb\nccc";
        assert_eq!(line_col(text, 0), (1, 1));
        assert_eq!(line_col(text, 1), (1, 2));
        assert_eq!(line_col(text, 2), (2, 1));
        assert_eq!(line_col(text, 5), (3, 1));
        assert_eq!(line_col(text, 999), (3, 4));
        // CRLF：`\r` 只占一列，`\n` 换行。
        assert_eq!(line_col("a\r\nb", 3), (2, 1));
    }

    #[test]
    fn key_search_finds_the_key_not_the_header() {
        let text = "[[game.rules]]\nid = \"x\"\naction = { priority = { class = \"high\" } }\n";
        let origin = PolicyOrigin::builtin("t.toml");
        let ctx = ctx(text, &origin);
        let offset = ctx.key_offset("priority", None).expect("priority");
        assert_eq!(line_col(text, offset), (3, 12));
        // 表头 `[[game.rules]]` 不应被当成 `rules` 键。
        let offset = ctx.key_offset("game", None);
        assert_eq!(offset, None);
        // 区间限制：只在 `id = "x"` 那一行找 `id`。
        assert!(ctx.key_offset("id", Some(15..24)).is_some());
        assert!(ctx.key_offset("priority", Some(15..24)).is_none());
    }
}
