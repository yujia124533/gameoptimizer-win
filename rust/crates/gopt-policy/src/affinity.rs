//! 策略层亲和性解析：把声明式的「保留 N 个核 / 仅物理核 / 显式掩码」翻译成
//! HAL 的 [`AffinityPlan`]（每组一个掩码）。
//!
//! 语义与转义规则：
//!
//! * `reserve_cores = N, reserve_from = "first"`（缺省）——保留**全局序号最小**的 N 个核给系统，
//!   与 C++ 版 `leaveCoresForSystem` 的"清除最低 N 个置位"逐位等价（**兼容性锚点**）；
//! * `reserve_from = "last"`——保留全局序号最大的 N 个核，直接复用 HAL 已测过的
//!   [`AffinityPlan::reserve_last_n_cores`]；
//! * `physical_only = true`——只绑定物理核（把每个物理核的 SMT 兄弟核一起选中），
//!   按 `hardware.core_layout` 里的物理核序号跳过 N 个；
//! * `mask = ...`——显式掩码，作用在处理器组 0；
//! * 逻辑处理器 > 64（多处理器组）时本模块会产出**跨组计划**（C++ 版在此直接放弃亲和性），
//!   执行方需按 [`AffinityPlan::per_group`] 逐组应用——不做静默降级，也不越权"替用户决定"。
//!
//! 解析失败一律返回 [`Reason`]（中英双语），由求值器写进 `Plan::skipped` 的"硬件降级"段，
//! 而不是让整份计划失败。

use gopt_hal::{
    full_group_mask, processor_groups, AffinityPlan, AffinityRequest, CoreLayout, HardwareInfo,
};

use crate::model::{AffinitySpec, ReserveSide};
use crate::plan::Reason;

/// 解析亲和性动作。`Err` 表示"按当前硬件画像无法执行"，理由可直接展示。
pub fn resolve_affinity(
    hardware: &HardwareInfo,
    spec: &AffinitySpec,
) -> Result<AffinityPlan, Reason> {
    let total = hardware.logical_cores;
    if total == 0 {
        return Err(degrade(
            "硬件画像报告 0 个逻辑处理器，跳过 CPU 亲和性绑定",
            "the hardware profile reports zero logical processors; affinity binding is skipped",
        ));
    }

    if let Some(mask) = spec.mask() {
        return AffinityPlan::single(total, 0, mask).map_err(|err| {
            degrade(
                format!(
                    "显式掩码 {mask:#018x} 不适用于处理器组 0（本机 {total} 个逻辑处理器）：{}",
                    err.message()
                ),
                format!(
                    "explicit mask {mask:#018x} does not fit processor group 0 ({total} logical processors): {}",
                    err.message()
                ),
            )
        });
    }

    if spec.physical_only() {
        return physical_only_plan(hardware, spec);
    }

    if spec.reserve_cores() == 0 {
        return AffinityPlan::full(total).map_err(|err| {
            degrade(
                format!("无法构造整机亲和性计划：{}", err.message()),
                format!(
                    "cannot build an all-processors affinity plan: {}",
                    err.message()
                ),
            )
        });
    }

    match spec.reserve_from() {
        ReserveSide::Last => AffinityPlan::reserve_last_n_cores(total, spec.reserve_cores()).map_err(|err| {
            degrade(
                format!(
                    "保留全局序号最大的 {} 个逻辑核对本机（{} 个逻辑处理器）不可行：{}",
                    spec.reserve_cores(),
                    total,
                    err.message()
                ),
                format!(
                    "reserving the {} highest-numbered logical cores is impossible on this machine ({total} logical processors): {}",
                    spec.reserve_cores(),
                    err.message()
                ),
            )
        }),
        ReserveSide::First => reserve_first_plan(total, spec.reserve_cores()),
    }
}

/// 保留**全局序号最小**的 N 个逻辑核（C++ `leaveCoresForSystem` 口径）。
fn reserve_first_plan(total: u32, reserve: u32) -> Result<AffinityPlan, Reason> {
    if reserve >= total {
        return Err(degrade(
            format!("保留 {reserve} 个逻辑核会耗尽全部 {total} 个逻辑处理器，跳过 CPU 亲和性绑定"),
            format!(
                "reserving {reserve} logical cores would consume all {total} logical processors; affinity binding is skipped"
            ),
        ));
    }
    let mut requests: Vec<AffinityRequest> = Vec::new();
    let mut to_reserve = reserve;
    for group in processor_groups(total) {
        let mut mask = group.full_mask();
        if to_reserve > 0 {
            let take = to_reserve.min(group.logical_count);
            // 组内低位 = 全局序号更小的逻辑处理器：保留 = 把最低的 take 位清掉。
            mask &= !full_group_mask(take);
            to_reserve -= take;
        }
        if mask != 0 {
            requests.push(AffinityRequest::new(group.index, mask).map_err(|err| {
                degrade(
                    format!("亲和性掩码不合法：{}", err.message()),
                    format!("invalid affinity mask: {}", err.message()),
                )
            })?);
        }
    }
    build_plan(total, requests)
}

/// `physical_only`：按物理核序号跳过 N 个核，把其余物理核的（含 SMT 兄弟）掩码按组汇总。
fn physical_only_plan(
    hardware: &HardwareInfo,
    spec: &AffinitySpec,
) -> Result<AffinityPlan, Reason> {
    let total = hardware.logical_cores;
    let layout: Vec<&CoreLayout> = hardware
        .core_layout
        .iter()
        .filter(|core| core.affinity != 0)
        .collect();
    if layout.is_empty() {
        return Err(degrade(
            "硬件画像里没有物理核布局（探测降级），无法按「仅物理核」绑定，已跳过",
            "the hardware profile carries no physical core layout (probe downgraded); physical-core-only binding is skipped",
        ));
    }

    let physical_cores = layout.len() as u32;
    let reserve = spec.reserve_cores();
    if reserve >= physical_cores {
        return Err(degrade(
            format!("保留 {reserve} 个物理核会耗尽全部 {physical_cores} 个物理核，跳过 CPU 亲和性绑定"),
            format!(
                "reserving {reserve} physical cores would consume all {physical_cores} physical cores; affinity binding is skipped"
            ),
        ));
    }

    let skip = reserve as usize;
    let selected: Vec<&CoreLayout> = match spec.reserve_from() {
        ReserveSide::First => layout.iter().skip(skip).copied().collect(),
        ReserveSide::Last => layout.iter().take(layout.len() - skip).copied().collect(),
    };

    let groups = processor_groups(total);
    let mut masks = vec![0u64; groups.len()];
    for core in selected {
        if let Some(slot) = masks.get_mut(core.group as usize) {
            *slot |= core.affinity;
        }
    }

    let mut requests: Vec<AffinityRequest> = Vec::new();
    for group in &groups {
        let mask = masks.get(group.index as usize).copied().unwrap_or(0);
        if mask != 0 {
            requests.push(AffinityRequest::new(group.index, mask).map_err(|err| {
                degrade(
                    format!("亲和性掩码不合法：{}", err.message()),
                    format!("invalid affinity mask: {}", err.message()),
                )
            })?);
        }
    }

    if requests.is_empty() {
        return Err(degrade(
            "按「仅物理核」计算后没有剩下任何可绑定的处理器，已跳过",
            "physical-core-only binding left no processor to bind; skipped",
        ));
    }
    build_plan(total, requests)
}

fn build_plan(total: u32, requests: Vec<AffinityRequest>) -> Result<AffinityPlan, Reason> {
    AffinityPlan::from_requests(total, requests).map_err(|err| {
        degrade(
            format!("无法构造亲和性计划：{}", err.message()),
            format!("cannot build the affinity plan: {}", err.message()),
        )
    })
}

fn degrade(zh: impl Into<String>, en: impl Into<String>) -> Reason {
    Reason::new(zh, en)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gopt_hal::CoreLayout;

    fn hardware(logical: u32, physical: u32, layout: Vec<CoreLayout>) -> HardwareInfo {
        HardwareInfo {
            cpu_model: "Test CPU".to_string(),
            physical_cores: physical,
            logical_cores: logical,
            supports_hyper_threading: logical > physical,
            cpu_base_freq_mhz: 3600,
            core_layout: layout,
            processor_groups: processor_groups(logical),
            gpu: None,
            system_ram_mb: 32768,
            available_ram_mb: 16384,
            large_pages_available: false,
            warnings: Vec::new(),
        }
    }

    /// `physical` 个物理核、每核 `smt` 个逻辑处理器（与 MockApi 的布局口径一致）。
    fn smt_topology(physical: u32, smt: u32) -> Vec<CoreLayout> {
        let mut layout = Vec::new();
        let mut logical = 0u32;
        for core in 0..physical {
            let mut mask = 0u64;
            for _ in 0..smt {
                mask |= 1u64 << logical;
                logical += 1;
            }
            layout.push(CoreLayout::new(core, 0, mask));
        }
        layout
    }

    #[test]
    fn first_side_reserves_the_lowest_cores_like_the_cpp_version() {
        let hw = hardware(16, 8, smt_topology(8, 2));
        let plan = resolve_affinity(&hw, &AffinitySpec::new(1, ReserveSide::First, false, None))
            .expect("plan");
        // 与 C++ 一致：全掩码清除最低 1 个置位 → 0xfffe，保留核 0 给系统。
        assert_eq!(plan.requests()[0].mask(), 0xfffe);
        assert_eq!(plan.selected_logical(), 15);
        assert!(plan.is_single_group());
    }

    #[test]
    fn last_side_reserves_the_highest_cores() {
        let hw = hardware(16, 8, smt_topology(8, 2));
        let plan = resolve_affinity(&hw, &AffinitySpec::new(2, ReserveSide::Last, false, None))
            .expect("plan");
        assert_eq!(plan.requests()[0].mask(), 0x3fff);
        assert_eq!(plan.selected_logical(), 14);
    }

    #[test]
    fn zero_reserve_binds_every_logical_processor() {
        let hw = hardware(16, 8, smt_topology(8, 2));
        let plan = resolve_affinity(&hw, &AffinitySpec::new(0, ReserveSide::First, false, None))
            .expect("plan");
        assert!(plan.is_all());
        assert_eq!(plan.selected_logical(), 16);
    }

    #[test]
    fn physical_only_keeps_smt_siblings_and_skips_whole_cores() {
        let hw = hardware(16, 8, smt_topology(8, 2));
        let plan = resolve_affinity(&hw, &AffinitySpec::new(1, ReserveSide::First, true, None))
            .expect("plan");
        // 跳过物理核 0（逻辑核 0、1）→ 0xfffc。
        assert_eq!(plan.requests()[0].mask(), 0xfffc);
        assert_eq!(plan.selected_logical(), 14);

        let last = resolve_affinity(&hw, &AffinitySpec::new(1, ReserveSide::Last, true, None))
            .expect("plan");
        // 跳过最后一个物理核（逻辑核 14、15）→ 0x3fff。
        assert_eq!(last.requests()[0].mask(), 0x3fff);
        assert_eq!(last.selected_logical(), 14);
    }

    #[test]
    fn explicit_mask_is_applied_to_group_zero() {
        let hw = hardware(16, 8, smt_topology(8, 2));
        let plan = resolve_affinity(
            &hw,
            &AffinitySpec::new(0, ReserveSide::First, false, Some(0x00ff)),
        )
        .expect("plan");
        assert_eq!(plan.requests()[0].mask(), 0x00ff);
        assert_eq!(plan.selected_logical(), 8);

        // 掩码超出该组容量 → 降级而不是 panic/报错。
        let err = resolve_affinity(
            &hw,
            &AffinitySpec::new(0, ReserveSide::First, false, Some(1 << 40)),
        )
        .expect_err("mask must not fit");
        assert!(err.zh.contains("不适用于处理器组 0"), "{}", err.zh);
        assert!(err.en.contains("does not fit"), "{}", err.en);
    }

    #[test]
    fn multi_group_machines_get_a_cross_group_plan() {
        let hw = hardware(96, 48, Vec::new());
        let plan = resolve_affinity(&hw, &AffinitySpec::new(4, ReserveSide::Last, false, None))
            .expect("plan");
        assert_eq!(plan.requests().len(), 2);
        assert_eq!(plan.requests()[0].mask(), u64::MAX);
        assert_eq!(plan.requests()[1].mask(), 0x0fff_ffff);
        assert_eq!(plan.per_group().len(), 2);

        // first 口径在跨组机器上同样可用：只清掉组 0 的最低位。
        let plan = resolve_affinity(&hw, &AffinitySpec::new(1, ReserveSide::First, false, None))
            .expect("plan");
        assert_eq!(plan.requests()[0].mask(), 0xffff_ffff_ffff_fffe);
        assert_eq!(plan.selected_logical(), 95);
    }

    #[test]
    fn impossible_reservations_degrade_with_a_bilingual_reason() {
        let hw = hardware(4, 4, smt_topology(4, 1));
        for spec in [
            AffinitySpec::new(4, ReserveSide::First, false, None),
            AffinitySpec::new(8, ReserveSide::First, false, None),
            AffinitySpec::new(4, ReserveSide::Last, false, None),
            AffinitySpec::new(4, ReserveSide::First, true, None),
            AffinitySpec::new(4, ReserveSide::Last, true, None),
        ] {
            let reason = resolve_affinity(&hw, &spec).expect_err("must degrade");
            assert!(!reason.zh.is_empty());
            assert!(!reason.en.is_empty());
        }

        // 0 个逻辑处理器 / 缺物理核布局：也要给出可解释的降级理由。
        let empty = HardwareInfo {
            logical_cores: 0,
            ..hardware(4, 4, Vec::new())
        };
        assert!(resolve_affinity(
            &empty,
            &AffinitySpec::new(1, ReserveSide::First, false, None)
        )
        .is_err());
        let no_layout = hardware(8, 4, Vec::new());
        let reason = resolve_affinity(
            &no_layout,
            &AffinitySpec::new(1, ReserveSide::First, true, None),
        )
        .expect_err("no layout");
        assert!(reason.zh.contains("物理核布局"), "{}", reason.zh);
    }

    #[test]
    fn physical_only_uses_core_groups_on_multi_group_machines() {
        // 96 逻辑核：前 32 个物理核在组 0，后 16 个在组 1。
        let mut layout = Vec::new();
        for core in 0..48u32 {
            let mask = if core < 32 {
                0b11u64 << (core * 2)
            } else {
                0b11u64 << ((core - 32) * 2)
            };
            layout.push(CoreLayout::new(core, core / 32, mask));
        }
        let hw = hardware(96, 48, layout);
        let plan = resolve_affinity(&hw, &AffinitySpec::new(1, ReserveSide::First, true, None))
            .expect("plan");
        assert_eq!(plan.requests().len(), 2);
        assert_eq!(plan.requests()[0].mask(), !0b11u64);
        assert_eq!(plan.requests()[1].mask(), 0x0000_0000_ffff_ffff);
        assert_eq!(plan.selected_logical(), 94);
    }
}
