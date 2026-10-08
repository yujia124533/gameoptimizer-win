//! 载荷 schema：`before` / `after` 的字段约定，以及编码/解码的**唯一**实现。
//!
//! 写入方（策略引擎/CLI）与读取方（回滚计划生成器）共用本模块，避免"写进去的字段、
//! 回滚时读不出来"这类只在真机上暴露的问题。编码一律用 HAL 值类型，
//! 因此**红线在类型层面就已经成立**：载荷里不可能出现 `realtime` 优先级
//! ——[`PriorityClass`] 没有这一档，解码时 `0x100` 会被硬拒绝。
//!
//! # target 约定
//!
//! | target | 含义 |
//! | --- | --- |
//! | `pid:<pid>` | 进程级设置（优先级 / 亲和性 / 工作集） |
//! | `power-scheme` | 当前活动电源方案 |
//! | `run:<HIVE>:<name>` | 开机启动项 |
//! | `game:<index>` | 旧格式 `games.conf` 里的每游戏启动配置 |
//!
//! # 载荷字段
//!
//! * 优先级：`{"pid":1234,"name":"cs2.exe"|null,"priority":"high"}`
//! * 亲和性：`{"pid":1234,"name":…,"plan":{…AffinityPlan…}}`
//!   （也接受 `{"pid":…,"mask":"0x…","total_logical":32,"group":0}` 这种扁平写法）
//! * 工作集：`{"pid":1234,"name":…,"min_bytes":536870912,"max_bytes":2147483648}`
//!   （`max_bytes` 缺省 = 不设上限，沿用 HAL 语义）
//! * 电源方案：`{"guid":"8c5e7fda-…","name":"High performance","is_high_performance":true}`
//! * 启动项：`RunEntry` 原样序列化（`hive` / `value_name` / `name` / `command` / `enabled` / `expandable`）
//!
//! 所有解码器都是**宽容但受校验**的：接受多种等价写法，但绝不放宽安全边界。

use gopt_hal::{
    AffinityPlan, Guid, PowerScheme, PriorityClass, RunEntry, RunHive, WorkingSetLimits,
};
use serde_json::{json, Value};

use crate::canonical::brief;

/// `pid:<pid>` 前缀。
pub const PID_TARGET_PREFIX: &str = "pid:";
/// 电源方案目标（无参数）。
pub const POWER_TARGET: &str = "power-scheme";
/// `run:<HIVE>:<name>` 前缀。
pub const RUN_TARGET_PREFIX: &str = "run:";
/// `game:<index>` 前缀（旧格式导入）。
pub const GAME_TARGET_PREFIX: &str = "game:";

/// 进程目标的稳定写法。
pub fn pid_target(pid: u32) -> String {
    format!("{PID_TARGET_PREFIX}{pid}")
}

/// 启动项目标的稳定写法（`run:HKCU:Steam`）。
pub fn run_target(hive: RunHive, name: &str) -> String {
    format!("{RUN_TARGET_PREFIX}{hive}:{name}")
}

/// 旧格式游戏条目的稳定写法（`game:3`）。
pub fn game_target(index: i64) -> String {
    format!("{GAME_TARGET_PREFIX}{index}")
}

/// 进程优先级载荷。
pub fn priority(pid: u32, name: Option<&str>, class: PriorityClass) -> Value {
    json!({"pid": pid, "name": name, "priority": class.as_str()})
}

/// 亲和性载荷。
pub fn affinity(pid: u32, name: Option<&str>, plan: &AffinityPlan) -> Value {
    json!({"pid": pid, "name": name, "plan": plan})
}

/// 工作集载荷。
pub fn working_set(pid: u32, name: Option<&str>, limits: WorkingSetLimits) -> Value {
    json!({
        "pid": pid,
        "name": name,
        "min_bytes": limits.min_bytes,
        "max_bytes": limits.max_bytes,
    })
}

/// 电源方案载荷。
pub fn power_scheme(scheme: &PowerScheme) -> Value {
    json!({
        "guid": scheme.guid.to_string(),
        "name": scheme.name,
        "is_high_performance": scheme.is_high_performance,
    })
}

/// 启动项载荷（`RunEntry` 的字段本身就是理想载荷，直接序列化）。
pub fn run_entry(entry: &RunEntry) -> Value {
    json!(entry)
}

/// 解码进程 ID（接受 `{"pid":n}` 或裸数字）。
pub fn decode_pid(value: &Value) -> Result<u32, String> {
    let raw = match value {
        Value::Object(_) => field(value, &["pid", "process_id", "processId"]),
        _ => Some(value),
    };
    let Some(raw) = raw else {
        return Err(format!("payload {} has no `pid` field", brief(value)));
    };
    let number = as_u64_lenient(raw)
        .ok_or_else(|| format!("`pid` is not a non-negative integer: {}", brief(raw)))?;
    u32::try_from(number).map_err(|_| format!("pid {number} does not fit in 32 bits"))
}

/// 解码优先级（接受 `"high"` / `"0x80"` / `128` / `{"priority":…}`）。
pub fn decode_priority(value: &Value) -> Result<PriorityClass, String> {
    let raw = match value {
        Value::Object(_) => field(value, &["priority", "class", "value"]),
        _ => Some(value),
    };
    let Some(raw) = raw else {
        return Err(format!("payload {} has no `priority` field", brief(value)));
    };
    if let Some(text) = raw.as_str() {
        return PriorityClass::parse(text)
            .map_err(|err| format!("priority `{text}` is rejected: {}", err.message()));
    }
    if let Some(number) = as_u64_lenient(raw) {
        let raw32 = u32::try_from(number)
            .map_err(|_| format!("priority {number} does not fit in the 32-bit Win32 range"))?;
        return PriorityClass::from_raw(raw32)
            .map_err(|err| format!("priority 0x{raw32:x} is rejected: {}", err.message()));
    }
    Err(format!(
        "`priority` is not a name or a number: {}",
        brief(raw)
    ))
}

/// 解码亲和性：`(pid, AffinityPlan)`。
pub fn decode_affinity(value: &Value) -> Result<(u32, AffinityPlan), String> {
    let pid = decode_pid(value)?;
    if let Some(plan_value) = field(value, &["plan", "affinity_plan"]) {
        let plan: AffinityPlan = serde_json::from_value(plan_value.clone())
            .map_err(|err| format!("`plan` is not an affinity plan: {err}"))?;
        return Ok((pid, validated_plan(plan)?));
    }
    if let Some(mask_value) = field(value, &["mask", "affinity_mask"]) {
        let mask = as_u64_lenient(mask_value)
            .ok_or_else(|| format!("`mask` is not a 64-bit integer: {}", brief(mask_value)))?;
        let total = field(value, &["total_logical", "total_logical_processors"])
            .and_then(as_u64_lenient)
            .ok_or_else(|| {
                "an affinity payload with a flat `mask` must also carry `total_logical`".to_string()
            })?;
        let group = field(value, &["group"])
            .and_then(as_u64_lenient)
            .unwrap_or(0);
        let total =
            u32::try_from(total).map_err(|_| format!("total_logical {total} is too large"))?;
        let group = u32::try_from(group).map_err(|_| format!("group {group} is too large"))?;
        let plan = AffinityPlan::single(total, group, mask)
            .map_err(|err| format!("affinity mask is rejected: {}", err.message()))?;
        return Ok((pid, plan));
    }
    Err(format!(
        "payload {} has neither `plan` nor `mask`",
        brief(value)
    ))
}

/// 解码工作集：`(pid, WorkingSetLimits)`；`max_bytes` 缺省视为"不设上限"。
pub fn decode_working_set(value: &Value) -> Result<(u32, WorkingSetLimits), String> {
    let pid = decode_pid(value)?;
    let min = field(value, &["min_bytes", "working_set_min"])
        .and_then(as_u64_lenient)
        .ok_or_else(|| format!("payload {} has no `min_bytes` field", brief(value)))?;
    let max = field(value, &["max_bytes", "working_set_max"])
        .and_then(as_u64_lenient)
        .unwrap_or(0);
    let limits = WorkingSetLimits::new(min, max)
        .map_err(|err| format!("working set limits are rejected: {}", err.message()))?;
    Ok((pid, limits))
}

/// 解码电源方案：`(Guid, Option<name>)`。
pub fn decode_power_scheme(value: &Value) -> Result<(Guid, Option<String>), String> {
    let raw = match value {
        Value::Object(_) => field(value, &["guid", "power_scheme_guid", "powerSchemeGuid"]),
        _ => Some(value),
    };
    let Some(raw) = raw else {
        return Err(format!("payload {} has no `guid` field", brief(value)));
    };
    let text = raw
        .as_str()
        .ok_or_else(|| format!("`guid` is not a string: {}", brief(raw)))?;
    let guid = text
        .parse::<Guid>()
        .map_err(|err| format!("`{text}` is not a power scheme GUID: {}", err.message()))?;
    let name = field(value, &["name"]).and_then(|item| item.as_str());
    Ok((guid, name.map(str::to_string)))
}

/// 解码启动项：`(hive, value_name, enabled)`。
///
/// `value_name` 缺失时接受显示名 `name` + `enabled`：
/// `enabled = false` 会按 C++ 版规则补上 `"[disabled] "` 前缀。
pub fn decode_run_entry(value: &Value) -> Result<(RunHive, String, bool), String> {
    let hive_text = field(value, &["hive"])
        .and_then(|item| item.as_str())
        .ok_or_else(|| format!("payload {} has no `hive` field", brief(value)))?;
    let hive = RunHive::parse(hive_text)
        .map_err(|err| format!("`{hive_text}` is not a Run key hive: {}", err.message()))?;
    let value_name = field(value, &["value_name"])
        .and_then(|item| item.as_str())
        .map(str::to_string);
    let display = field(value, &["name"]).and_then(|item| item.as_str());
    let enabled = field(value, &["enabled"]).and_then(|item| item.as_bool());
    match (value_name, display, enabled) {
        (Some(value_name), _, enabled) => {
            let enabled = enabled.unwrap_or(!RunEntry::is_disabled(&value_name));
            Ok((hive, value_name, enabled))
        }
        (None, Some(name), enabled) => {
            let enabled = enabled.unwrap_or(true);
            let value_name = if enabled {
                name.to_string()
            } else {
                RunEntry::disabled_value_name(name)
            };
            Ok((hive, value_name, enabled))
        }
        (None, None, _) => Err(format!(
            "payload {} has neither `value_name` nor `name`",
            brief(value)
        )),
    }
}

/// 重新执行 [`AffinityPlan`] 的构造期校验。
///
/// `AffinityPlan` 的字段私有，但 `Deserialize` 会绕过 `from_requests`
/// ——被篡改的日志行可能塞进"空掩码/越界组号"。这里用公开 accessor 重建一次，
/// 把 HAL 的校验重新拉回日志读取路径。
fn validated_plan(plan: AffinityPlan) -> Result<AffinityPlan, String> {
    AffinityPlan::from_requests(plan.total_logical(), plan.requests().to_vec())
        .map_err(|err| format!("affinity plan is invalid: {}", err.message()))
}

/// 在对象里按候选键名取值。
fn field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let map = value.as_object()?;
    keys.iter().find_map(|key| map.get(*key))
}

/// 宽容取非负整数：数字、十进制/十六进制字符串、整值浮点。
pub(crate) fn as_u64_lenient(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return Some(number);
    }
    if let Some(text) = value.as_str() {
        return parse_u64_text(text);
    }
    if let Some(float) = value.as_f64() {
        if float.fract() == 0.0 && float >= 0.0 && float <= u64::MAX as f64 {
            return Some(float as u64);
        }
    }
    None
}

/// 解析十进制或 `0x` 前缀十六进制的无符号数。
pub(crate) fn parse_u64_text(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16).ok();
    }
    trimmed.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_stable() {
        assert_eq!(pid_target(1234), "pid:1234");
        assert_eq!(run_target(RunHive::CurrentUser, "Steam"), "run:HKCU:Steam");
        assert_eq!(game_target(3), "game:3");
        assert_eq!(POWER_TARGET, "power-scheme");
    }

    #[test]
    fn priority_round_trips_and_tolerates_forms() {
        let payload = priority(4242, Some("cs2.exe"), PriorityClass::High);
        assert_eq!(
            decode_priority(&payload).expect("decode"),
            PriorityClass::High
        );
        assert_eq!(
            decode_priority(&json!("high")).expect("decode"),
            PriorityClass::High
        );
        assert_eq!(
            decode_priority(&json!("0x80")).expect("decode"),
            PriorityClass::High
        );
        assert_eq!(
            decode_priority(&json!(128)).expect("decode"),
            PriorityClass::High
        );
        assert_eq!(
            decode_priority(&json!({"class": "ABOVE_NORMAL_PRIORITY_CLASS"})).expect("decode"),
            PriorityClass::AboveNormal
        );
        assert_eq!(decode_pid(&payload).expect("pid"), 4242);
    }

    #[test]
    fn realtime_payloads_are_rejected() {
        for payload in [
            json!({"pid": 1, "priority": "realtime"}),
            json!({"pid": 1, "priority": 0x100}),
            json!({"pid": 1, "priority": "0x100"}),
            json!(256),
        ] {
            let err = decode_priority(&payload).expect_err("REALTIME must be rejected");
            assert!(
                err.contains("rejected"),
                "unexpected message for {payload}: {err}"
            );
        }
    }

    #[test]
    fn affinity_round_trips_through_plan_and_flat_mask() {
        let plan = AffinityPlan::reserve_last_n_cores(32, 4).expect("plan");
        let payload = affinity(7, None, &plan);
        let (pid, decoded) = decode_affinity(&payload).expect("decode");
        assert_eq!(pid, 7);
        assert_eq!(decoded, plan);

        let flat = json!({"pid": 7, "mask": "0x000000000000000f", "total_logical": 32});
        let (pid, decoded) = decode_affinity(&flat).expect("decode flat");
        assert_eq!(pid, 7);
        assert_eq!(
            decoded.requests().first().map(|item| item.mask()),
            Some(0xf)
        );

        let empty_mask = json!({"pid": 7, "mask": 0, "total_logical": 32});
        assert!(decode_affinity(&empty_mask).is_err());
        let greedy = json!({"pid": 7, "mask": 1});
        assert!(decode_affinity(&greedy).is_err());
        // 反序列化绕过构造函数：越界掩码在解码期被 HAL 重新校验拦下。
        let bad_plan =
            json!({"pid": 7, "plan": {"total_logical": 8, "requests": [{"group": 0, "mask": 0}]}});
        assert!(decode_affinity(&bad_plan).is_err());
    }

    #[test]
    fn working_set_uses_hal_semantics() {
        let limits = WorkingSetLimits::from_mb(512, 2048).expect("limits");
        let payload = working_set(9, Some("game.exe"), limits);
        let (pid, decoded) = decode_working_set(&payload).expect("decode");
        assert_eq!(pid, 9);
        assert_eq!(decoded, limits);

        let min_only =
            decode_working_set(&json!({"pid": 9, "min_bytes": 1048576})).expect("min only");
        assert_eq!(min_only.1.max_bytes, WorkingSetLimits::NO_UPPER_BOUND);
        assert!(decode_working_set(&json!({"pid": 9, "min_bytes": 0})).is_err());
        assert!(decode_working_set(&json!({"pid": 9})).is_err());
    }

    #[test]
    fn power_scheme_and_run_entry_round_trip() {
        let scheme = PowerScheme::new(Guid::HIGH_PERFORMANCE, "High performance");
        let payload = power_scheme(&scheme);
        let (guid, name) = decode_power_scheme(&payload).expect("decode");
        assert_eq!(guid, Guid::HIGH_PERFORMANCE);
        assert_eq!(name.as_deref(), Some("High performance"));
        assert_eq!(
            decode_power_scheme(&json!("8c5e7fda-e8bf-4a96-9a85-a6e2638c635c"))
                .expect("bare guid")
                .0,
            Guid::HIGH_PERFORMANCE
        );
        assert!(decode_power_scheme(&json!({"guid": "not-a-guid"})).is_err());

        let entry = RunEntry::from_registry(RunHive::CurrentUser, "Steam", r"C:\steam.exe", false);
        let payload = run_entry(&entry);
        let (hive, value_name, enabled) = decode_run_entry(&payload).expect("decode");
        assert_eq!(hive, RunHive::CurrentUser);
        assert_eq!(value_name, "Steam");
        assert!(enabled);

        let disabled = RunEntry::from_registry(
            RunHive::LocalMachine,
            "[disabled] Steam",
            r"C:\steam.exe",
            false,
        );
        let payload = run_entry(&disabled);
        let (hive, value_name, enabled) = decode_run_entry(&payload).expect("decode");
        assert_eq!(hive, RunHive::LocalMachine);
        assert_eq!(value_name, "[disabled] Steam");
        assert!(!enabled);

        // 只有显示名 + enabled=false ⇒ 补上禁用前缀。
        let (_, value_name, enabled) =
            decode_run_entry(&json!({"hive": "HKCU", "name": "Steam", "enabled": false}))
                .expect("decode");
        assert_eq!(value_name, "[disabled] Steam");
        assert!(!enabled);
        assert!(decode_run_entry(&json!({"hive": "nope", "value_name": "x"})).is_err());
    }
}
