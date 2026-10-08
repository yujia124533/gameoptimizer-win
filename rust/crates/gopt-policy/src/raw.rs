//! TOML 反序列化镜像类型（**不对外**）。
//!
//! 它们存在的唯一理由：与 [`toml::Spanned`] 配合，为每个值保留字节区间（span），
//! 使校验失败能报出"文件:行:列"。真正的公开模型是 [`crate::model`]——那里只有
//! "已经校验过、满足不变量"的类型。
//!
//! 两个类型需要手写 [`serde::Deserialize`]：
//!
//! * [`RawLeaf`]：一个条件取值，接受整数 / 字符串 / 布尔（TOML 是自描述格式，
//!   用 `deserialize_any` 分派即可）；
//! * [`RawComparison`]：既接受标量简写（`ram_gb = 16` ⇒ `eq = 16`），也接受算子表
//!   （`ram_gb = { gte = 8, lt = 16 }`）；算子名写错时报 `unknown_field` 并列出合法算子，
//!   而不是 untagged 枚举那种"data did not match any variant"的糊状错误。

use core::fmt;

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use toml::Spanned;

use crate::condition::ConditionField;

/// 一个策略文件的原始形态。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPolicyFile {
    /// 可选：策略文件格式版本（缺省视为 1）。
    #[serde(default)]
    pub(crate) schema: Option<Spanned<i64>>,
    /// 全部游戏定义。
    #[serde(default, rename = "game")]
    pub(crate) games: Vec<Spanned<RawGame>>,
}

/// `[[game]]` 的原始形态。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGame {
    pub(crate) id: Spanned<String>,
    pub(crate) name_zh: Spanned<String>,
    pub(crate) name_en: Spanned<String>,
    #[serde(rename = "match")]
    pub(crate) exe_match: Spanned<String>,
    #[serde(default)]
    pub(crate) exe_aliases: Vec<Spanned<String>>,
    #[serde(default)]
    pub(crate) name_aliases: Vec<Spanned<String>>,
    #[serde(default)]
    pub(crate) description_zh: Option<Spanned<String>>,
    #[serde(default)]
    pub(crate) description_en: Option<Spanned<String>>,
    #[serde(default, rename = "rules")]
    pub(crate) rules: Vec<Spanned<RawRule>>,
}

/// `[[game.rules]]` 的原始形态。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRule {
    #[serde(default)]
    pub(crate) id: Option<Spanned<String>>,
    #[serde(default)]
    pub(crate) when: Option<Spanned<RawWhen>>,
    pub(crate) action: Spanned<RawAction>,
}

/// `when = { ... }` 的原始形态（字段名与 [`ConditionField`] 一一对应）。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWhen {
    #[serde(default)]
    pub(crate) logical_cores: Option<Spanned<RawComparison>>,
    #[serde(default)]
    pub(crate) physical_cores: Option<Spanned<RawComparison>>,
    #[serde(default)]
    pub(crate) ram_gb: Option<Spanned<RawComparison>>,
    #[serde(default)]
    pub(crate) ram_mb: Option<Spanned<RawComparison>>,
    #[serde(default)]
    pub(crate) gpu_vendor: Option<Spanned<RawComparison>>,
    #[serde(default)]
    pub(crate) is_elevated: Option<Spanned<RawComparison>>,
}

impl RawWhen {
    /// 按 [`ConditionField::ALL`] 的固定顺序遍历已给出的条件（保证 Plan 输出稳定）。
    pub(crate) fn iter(&self) -> impl Iterator<Item = (ConditionField, &Spanned<RawComparison>)> {
        let slots = [
            (ConditionField::LogicalCores, self.logical_cores.as_ref()),
            (ConditionField::PhysicalCores, self.physical_cores.as_ref()),
            (ConditionField::RamGb, self.ram_gb.as_ref()),
            (ConditionField::RamMb, self.ram_mb.as_ref()),
            (ConditionField::GpuVendor, self.gpu_vendor.as_ref()),
            (ConditionField::IsElevated, self.is_elevated.as_ref()),
        ];
        slots
            .into_iter()
            .filter_map(|(field, slot)| slot.map(|spanned| (field, spanned)))
    }
}

/// 一个条件取值：整数 / 文本 / 布尔。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RawLeaf {
    /// 整数。
    Int(i64),
    /// 文本。
    Text(String),
    /// 布尔。
    Bool(bool),
}

impl<'de> Deserialize<'de> for RawLeaf {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LeafVisitor;

        impl<'de> Visitor<'de> for LeafVisitor {
            type Value = RawLeaf;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an integer, a string or a boolean")
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<RawLeaf, E> {
                Ok(RawLeaf::Int(value))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<RawLeaf, E> {
                i64::try_from(value).map(RawLeaf::Int).map_err(|_| {
                    E::custom("comparison value is too large for a signed 64-bit integer")
                })
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<RawLeaf, E> {
                Ok(RawLeaf::Text(value.to_string()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<RawLeaf, E> {
                Ok(RawLeaf::Text(value))
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<RawLeaf, E> {
                Ok(RawLeaf::Bool(value))
            }
        }

        deserializer.deserialize_any(LeafVisitor)
    }
}

/// 算子表：`{ gte = 8, lt = 16 }`。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct RawOps {
    /// `gt = ...`
    pub(crate) gt: Option<RawLeaf>,
    /// `gte = ...`
    pub(crate) gte: Option<RawLeaf>,
    /// `lt = ...`
    pub(crate) lt: Option<RawLeaf>,
    /// `lte = ...`
    pub(crate) lte: Option<RawLeaf>,
    /// `eq = ...`
    pub(crate) eq: Option<RawLeaf>,
    /// `ne = ...`
    pub(crate) ne: Option<RawLeaf>,
}

/// 一个字段的比较：标量简写或算子表。
#[derive(Debug, PartialEq)]
pub(crate) enum RawComparison {
    /// `logical_cores = 8`（等价于 `eq = 8`）。
    Scalar(RawLeaf),
    /// `logical_cores = { gte = 8 }`。
    Ops(RawOps),
}

impl<'de> Deserialize<'de> for RawComparison {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ComparisonVisitor;

        const OPERATORS: [&str; 6] = ["gt", "gte", "lt", "lte", "eq", "ne"];

        impl<'de> Visitor<'de> for ComparisonVisitor {
            type Value = RawComparison;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "either a scalar value (shorthand for `eq`) or a table of `gt`/`gte`/`lt`/`lte`/`eq`/`ne`",
                )
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<RawComparison, E> {
                Ok(RawComparison::Scalar(RawLeaf::Int(value)))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<RawComparison, E> {
                i64::try_from(value)
                    .map(|value| RawComparison::Scalar(RawLeaf::Int(value)))
                    .map_err(|_| {
                        E::custom("comparison value is too large for a signed 64-bit integer")
                    })
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<RawComparison, E> {
                Ok(RawComparison::Scalar(RawLeaf::Text(value.to_string())))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<RawComparison, E> {
                Ok(RawComparison::Scalar(RawLeaf::Text(value)))
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<RawComparison, E> {
                Ok(RawComparison::Scalar(RawLeaf::Bool(value)))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawComparison, A::Error> {
                let mut ops = RawOps::default();
                while let Some(key) = map.next_key::<String>()? {
                    let slot = match key.as_str() {
                        "gt" => &mut ops.gt,
                        "gte" => &mut ops.gte,
                        "lt" => &mut ops.lt,
                        "lte" => &mut ops.lte,
                        "eq" => &mut ops.eq,
                        "ne" => &mut ops.ne,
                        other => return Err(de::Error::unknown_field(other, &OPERATORS)),
                    };
                    if slot.is_some() {
                        return Err(de::Error::custom(format!(
                            "operator `{key}` is given twice"
                        )));
                    }
                    *slot = Some(map.next_value::<RawLeaf>()?);
                }
                Ok(RawComparison::Ops(ops))
            }
        }

        deserializer.deserialize_any(ComparisonVisitor)
    }
}

/// `action = { ... }` 的原始形态。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAction {
    #[serde(default)]
    pub(crate) priority: Option<RawPriority>,
    #[serde(default)]
    pub(crate) affinity: Option<RawAffinity>,
    #[serde(default)]
    pub(crate) working_set: Option<RawWorkingSet>,
    #[serde(default)]
    pub(crate) power_scheme: Option<RawPowerScheme>,
    #[serde(default)]
    pub(crate) run_entries: Option<RawRunEntries>,
    #[serde(default)]
    pub(crate) skip: Option<RawSkip>,
}

/// `priority { class }`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPriority {
    pub(crate) class: Spanned<String>,
}

/// `affinity { ... }`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAffinity {
    #[serde(default)]
    pub(crate) reserve_cores: Option<Spanned<u32>>,
    #[serde(default)]
    pub(crate) reserve_from: Option<Spanned<String>>,
    #[serde(default)]
    pub(crate) physical_only: Option<Spanned<bool>>,
    #[serde(default)]
    pub(crate) mask: Option<Spanned<RawLeaf>>,
}

/// `working_set { min_mb, max_mb }`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWorkingSet {
    #[serde(default)]
    pub(crate) min_mb: Option<Spanned<u64>>,
    #[serde(default)]
    pub(crate) max_mb: Option<Spanned<u64>>,
}

/// `power_scheme { scheme }`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPowerScheme {
    pub(crate) scheme: Spanned<String>,
}

/// `run_entries { hive, disable, ignore_missing }`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRunEntries {
    #[serde(default)]
    pub(crate) hive: Option<Spanned<String>>,
    #[serde(default)]
    pub(crate) disable: Vec<Spanned<String>>,
    #[serde(default)]
    pub(crate) ignore_missing: Option<Spanned<bool>>,
}

/// `skip { reason_zh, reason_en }`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSkip {
    pub(crate) reason_zh: Spanned<String>,
    pub(crate) reason_en: Spanned<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_accepts_scalar_shorthand_and_operator_tables() {
        #[derive(Debug, Deserialize)]
        struct Holder {
            value: Spanned<RawComparison>,
        }

        let scalar: Holder = toml::from_str("value = 16").expect("scalar");
        assert_eq!(
            scalar.value.get_ref(),
            &RawComparison::Scalar(RawLeaf::Int(16))
        );

        let text: Holder = toml::from_str(r#"value = "nvidia""#).expect("text");
        assert_eq!(
            text.value.get_ref(),
            &RawComparison::Scalar(RawLeaf::Text("nvidia".to_string()))
        );

        let flag: Holder = toml::from_str("value = true").expect("bool");
        assert_eq!(
            flag.value.get_ref(),
            &RawComparison::Scalar(RawLeaf::Bool(true))
        );

        let table: Holder = toml::from_str("value = { gte = 8, lt = 16 }").expect("table");
        match table.value.get_ref() {
            RawComparison::Ops(ops) => {
                assert_eq!(ops.gte, Some(RawLeaf::Int(8)));
                assert_eq!(ops.lt, Some(RawLeaf::Int(16)));
                assert_eq!(ops.gt, None);
            }
            other => panic!("expected operators, got {other:?}"),
        }
    }

    #[test]
    fn comparison_rejects_unknown_or_duplicate_operators() {
        #[derive(Debug, Deserialize)]
        struct Holder {
            #[allow(dead_code)]
            value: Spanned<RawComparison>,
        }

        let err = toml::from_str::<Holder>("value = { gt3 = 8 }").expect_err("unknown operator");
        let message = err.message();
        assert!(message.contains("gt3"), "{message}");
        assert!(message.contains("gte"), "{message}");

        let err = toml::from_str::<Holder>("value = { gte = 8, gte = 9 }").expect_err("duplicate");
        // TOML 自身会拒绝同一个内联表里的重复键（比我们的检查更早，且带 span）。
        assert!(err.message().contains("gte"), "{}", err.message());
        assert!(err.message().contains("duplicate"), "{}", err.message());
        assert!(err.span().is_some());
    }

    #[test]
    fn when_iterates_in_a_fixed_field_order() {
        let when: Spanned<RawWhen> = {
            #[derive(Deserialize)]
            struct Holder {
                when: Spanned<RawWhen>,
            }
            toml::from_str::<Holder>("when = { gpu_vendor = \"nvidia\", logical_cores = 8 }")
                .expect("when")
                .when
        };
        let fields: Vec<ConditionField> = when.get_ref().iter().map(|(field, _)| field).collect();
        assert_eq!(
            fields,
            vec![ConditionField::LogicalCores, ConditionField::GpuVendor]
        );
    }

    #[test]
    fn unknown_keys_are_rejected_with_spans() {
        let err =
            toml::from_str::<RawPolicyFile>("schema = 1\ngamme = []\n").expect_err("unknown key");
        assert!(err.message().contains("gamme"), "{}", err.message());
        assert!(err.span().is_some());

        let err =
            toml::from_str::<RawPolicyFile>("[[game]]\nid = \"a\"\n").expect_err("missing fields");
        assert!(err.message().contains("name_zh"), "{}", err.message());
    }
}
