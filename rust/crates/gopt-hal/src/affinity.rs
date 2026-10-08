//! CPU 亲和性：处理器组模型与掩码计算（纯函数，与平台无关，可完整单测）。
//!
//! Windows 的亲和性掩码是"每组 64 位"的：逻辑处理器超过 64 个的机器会被划分成多个
//! **处理器组**（processor group）。`SetProcessAffinityMask` 只作用于进程主组，跨组必须
//! 逐线程用 `SetThreadGroupAffinity`——所以"保留最后 N 个核给系统"这类策略在 >64 逻辑核
//! 机器上会自然产出"每组一段掩码"的结果，本模块负责把它算对、算稳、算可解释。
//!
//! 计算规则（与 C++ 版策略语义一致，且对 >64 核显式建模而不是静默降级）：
//!
//! * 逻辑处理器按全局序号 0..N 连续编号，第 g 组持有 `[g*64, g*64+64)` 中的逻辑处理器；
//! * `reserve_last_n_cores` 从**全局序号最大的核**开始往前清位，可能横跨多个组；
//! * 掩码只使用低位（组内第 i 个逻辑处理器 → 第 i 位）。

use core::fmt;

use crate::error::{HalError, HalResult};

/// 单个处理器组最多容纳的逻辑处理器数（Win32 固定值）。
pub const MAX_LOGICAL_PER_GROUP: u32 = 64;

/// 一个处理器组的容量描述。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ProcessorGroup {
    /// 组序号（从 0 开始）。
    pub index: u32,
    /// 该组中处于活动状态的逻辑处理器数（<= [`MAX_LOGICAL_PER_GROUP`]）。
    pub logical_count: u32,
}

impl ProcessorGroup {
    /// 构造。
    pub const fn new(index: u32, logical_count: u32) -> Self {
        Self {
            index,
            logical_count,
        }
    }

    /// 该组的完整掩码（考虑 64 位边界，`logical_count == 64` 时不会溢出）。
    pub const fn full_mask(self) -> u64 {
        full_group_mask(self.logical_count)
    }
}

/// 组内前 `logical_count` 个逻辑处理器的掩码；`>= 64` 时返回全 1（避免移位溢出）。
pub const fn full_group_mask(logical_count: u32) -> u64 {
    if logical_count >= MAX_LOGICAL_PER_GROUP {
        u64::MAX
    } else {
        (1u64 << logical_count) - 1
    }
}

/// 按逻辑处理器总数推导处理器组布局（>64 时自动分组）。
pub fn processor_groups(total_logical: u32) -> Vec<ProcessorGroup> {
    let mut groups = Vec::new();
    let mut remaining = total_logical;
    let mut index = 0u32;
    while remaining > 0 {
        let count = remaining.min(MAX_LOGICAL_PER_GROUP);
        groups.push(ProcessorGroup::new(index, count));
        remaining -= count;
        index += 1;
    }
    groups
}

/// 单组亲和性请求：`group` 组内掩码 `mask`。
///
/// 字段私有，只能经 [`AffinityRequest::new`] 构造——"掩码为 0 的请求"在类型层面不可表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AffinityRequest {
    group: u32,
    mask: u64,
}

impl AffinityRequest {
    /// 构造并校验掩码非 0。
    pub fn new(group: u32, mask: u64) -> HalResult<Self> {
        if mask == 0 {
            return Err(HalError::invalid_argument(
                "AffinityRequest::new",
                format!("affinity mask for processor group {group} is empty"),
            ));
        }
        Ok(Self { group, mask })
    }

    /// 处理器组序号。
    pub const fn group(self) -> u32 {
        self.group
    }

    /// 组内逻辑处理器位掩码（保证非 0）。
    pub const fn mask(self) -> u64 {
        self.mask
    }

    /// 该请求选中的逻辑处理器数。
    pub const fn selected_logical(self) -> u32 {
        self.mask.count_ones()
    }

    /// 掩码是否落在 `group` 的容量内。
    pub fn fits(self, group: ProcessorGroup) -> bool {
        self.group == group.index && self.mask & !group.full_mask() == 0
    }
}

/// 亲和性计划：一个或多个处理器组上的掩码（>64 逻辑核机器上会是多组）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AffinityPlan {
    total_logical: u32,
    requests: Vec<AffinityRequest>,
}

impl AffinityPlan {
    /// 由若干单组请求构造，并做完整校验（组序号唯一且递增、掩码非 0 且不越界）。
    pub fn from_requests(total_logical: u32, requests: Vec<AffinityRequest>) -> HalResult<Self> {
        if total_logical == 0 {
            return Err(HalError::invalid_argument(
                "AffinityPlan::from_requests",
                "the machine reports zero logical processors",
            ));
        }
        if requests.is_empty() {
            return Err(HalError::invalid_argument(
                "AffinityPlan::from_requests",
                "an affinity plan needs at least one processor group",
            ));
        }
        let groups = processor_groups(total_logical);
        let mut previous: Option<u32> = None;
        for request in &requests {
            if request.mask == 0 {
                // 防御性检查：AffinityRequest::new 已经拦住了空掩码，这里保证计划级不变量。
                return Err(HalError::invalid_argument(
                    "AffinityPlan::from_requests",
                    format!(
                        "affinity mask for processor group {} is empty",
                        request.group
                    ),
                ));
            }
            if let Some(prev) = previous {
                if request.group <= prev {
                    return Err(HalError::invalid_argument(
                        "AffinityPlan::from_requests",
                        format!(
                            "processor group {} must be greater than the previous group {prev}",
                            request.group
                        ),
                    ));
                }
            }
            previous = Some(request.group);
            let group = groups.get(request.group as usize).ok_or_else(|| {
                HalError::invalid_argument(
                    "AffinityPlan::from_requests",
                    format!(
                        "processor group {} does not exist (the machine has {} group(s))",
                        request.group,
                        groups.len()
                    ),
                )
            })?;
            if !request.fits(*group) {
                return Err(HalError::invalid_argument(
                    "AffinityPlan::from_requests",
                    format!(
                        "mask {:#018x} does not fit processor group {} ({} logical processors)",
                        request.mask, request.group, group.logical_count
                    ),
                ));
            }
        }
        Ok(Self {
            total_logical,
            requests,
        })
    }

    /// 全部逻辑处理器都可用的计划。
    pub fn full(total_logical: u32) -> HalResult<Self> {
        let requests = processor_groups(total_logical)
            .into_iter()
            .map(|group| AffinityRequest {
                group: group.index,
                mask: group.full_mask(),
            })
            .collect();
        Self::from_requests(total_logical, requests)
    }

    /// "保留最后 N 个逻辑处理器给系统"的计划（供 LoL 等预设使用）。
    ///
    /// 保留顺序从全局序号最大的核开始，跨组时先清高位组。
    pub fn reserve_last_n_cores(total_logical: u32, reserve_last_n_cores: u32) -> HalResult<Self> {
        Self::from_requests(
            total_logical,
            reserve_last_n_cores_to_requests(total_logical, reserve_last_n_cores)?,
        )
    }

    /// 单组计划（`group == 0` 走 `SetProcessAffinityMask`；其它组走逐线程组亲和性）。
    pub fn single(total_logical: u32, group: u32, mask: u64) -> HalResult<Self> {
        Self::from_requests(total_logical, vec![AffinityRequest::new(group, mask)?])
    }

    /// 机器上的逻辑处理器总数。
    pub const fn total_logical(&self) -> u32 {
        self.total_logical
    }

    /// 各组请求（按组序号递增）。
    pub fn requests(&self) -> &[AffinityRequest] {
        &self.requests
    }

    /// 计划选中的逻辑处理器总数。
    pub fn selected_logical(&self) -> u32 {
        self.requests
            .iter()
            .map(|request| request.selected_logical())
            .sum()
    }

    /// 计划是否只涉及一个处理器组。
    pub fn is_single_group(&self) -> bool {
        self.requests.len() == 1
    }

    /// 计划涉及的第一个处理器组序号。
    pub fn primary_group(&self) -> Option<u32> {
        self.requests.first().map(|request| request.group)
    }

    /// 拆成"每组一个单组计划"，供 >64 逻辑核机器上逐组逐线程应用。
    pub fn per_group(&self) -> Vec<AffinityPlan> {
        self.requests
            .iter()
            .map(|request| AffinityPlan {
                total_logical: self.total_logical,
                requests: vec![*request],
            })
            .collect()
    }

    /// 该计划是否为"整机"（选中全部逻辑处理器）。
    pub fn is_all(&self) -> bool {
        self.selected_logical() == self.total_logical
            && self.requests.len() == processor_groups(self.total_logical).len()
    }
}

impl fmt::Display for AffinityPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let groups: Vec<String> = self
            .requests
            .iter()
            .map(|request| format!("g{}={:#018x}", request.group, request.mask))
            .collect();
        write!(
            f,
            "{} of {} logical processors [{}]",
            self.selected_logical(),
            self.total_logical,
            groups.join(", ")
        )
    }
}
/// 计算"保留最后 N 个核"的分组掩码（纯函数，便于单测与跨端复用）。
///
/// 整组被保留时该组会从结果中消失（不会返回掩码为 0 的请求）；
/// `reserve_last_n_cores >= total_logical` 视为非法参数。
pub fn reserve_last_n_cores_to_requests(
    total_logical: u32,
    reserve_last_n_cores: u32,
) -> HalResult<Vec<AffinityRequest>> {
    if total_logical == 0 {
        return Err(HalError::invalid_argument(
            "reserve_last_n_cores",
            "the machine reports zero logical processors",
        ));
    }
    if reserve_last_n_cores >= total_logical {
        return Err(HalError::invalid_argument(
            "reserve_last_n_cores",
            format!(
                "reserving {reserve_last_n_cores} of {total_logical} logical processors would leave no core to bind"
            ),
        ));
    }

    let mut requests: Vec<AffinityRequest> = processor_groups(total_logical)
        .into_iter()
        .map(|group| AffinityRequest {
            group: group.index,
            mask: group.full_mask(),
        })
        .collect();

    let mut to_reserve = reserve_last_n_cores;
    for request in requests.iter_mut().rev() {
        if to_reserve == 0 {
            break;
        }
        let available = request.selected_logical();
        let reserved_here = to_reserve.min(available);
        let kept = available - reserved_here;
        request.mask = full_group_mask(kept);
        to_reserve -= reserved_here;
    }

    // 被整组保留（掩码清零）的组直接从计划里消失，而不是留下一个"掩码为 0"的非法请求。
    // 由于 reserve < total_logical，至少还有一个组留有可用核，因此结果永远非空。
    requests.retain(|request| request.mask != 0);

    Ok(requests)
}

/// `get_affinity` 的结果快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AffinityInfo {
    /// 目标进程 ID。
    pub pid: u32,
    /// 目标进程当前所在的处理器组（`SetProcessAffinityMask` 的作用域）。
    pub group: u32,
    /// 进程当前亲和性掩码（组内）。
    pub process_mask: u64,
    /// 该组内系统可用处理器的掩码（`GetProcessAffinityMask` 的第二返回值）。
    pub system_mask: u64,
    /// 机器逻辑处理器总数（用于换算 >64 核场景）。
    pub total_logical: u32,
}

/// 实际生效的亲和性设置手段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffinityMethod {
    /// 主组内整进程绑定：`SetProcessAffinityMask`。
    ProcessAffinityMask,
    /// 非主组（>64 逻辑核）：逐线程 `SetThreadGroupAffinity`。
    ThreadGroupAffinity,
}

impl AffinityMethod {
    /// 稳定短名（JSON / 审计日志用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            AffinityMethod::ProcessAffinityMask => "process_affinity_mask",
            AffinityMethod::ThreadGroupAffinity => "thread_group_affinity",
        }
    }
}

/// `set_affinity` 的结果：实际生效的计划、手段与受影响线程数（供审计日志）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AffinityApplied {
    /// 已生效的亲和性计划（回滚时用它恢复原值）。
    pub plan: AffinityPlan,
    /// 生效手段。
    pub method: AffinityMethod,
    /// `ThreadGroupAffinity` 时被设置的线程数；进程级绑定为 0。
    pub threads_updated: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_group_mask_handles_the_64_bit_edge() {
        assert_eq!(full_group_mask(0), 0);
        assert_eq!(full_group_mask(1), 0b1);
        assert_eq!(full_group_mask(63), 0x7fff_ffff_ffff_ffff);
        assert_eq!(full_group_mask(64), u64::MAX);
        assert_eq!(full_group_mask(65), u64::MAX);
    }

    #[test]
    fn processor_groups_split_at_64() {
        assert_eq!(processor_groups(0), Vec::new());
        assert_eq!(processor_groups(8), vec![ProcessorGroup::new(0, 8)]);
        assert_eq!(processor_groups(64), vec![ProcessorGroup::new(0, 64)]);
        assert_eq!(
            processor_groups(96),
            vec![ProcessorGroup::new(0, 64), ProcessorGroup::new(1, 32)]
        );
        assert_eq!(
            processor_groups(128),
            vec![ProcessorGroup::new(0, 64), ProcessorGroup::new(1, 64)]
        );
    }

    #[test]
    fn reserve_last_n_cores_single_group() {
        let requests = reserve_last_n_cores_to_requests(8, 4).expect("valid");
        assert_eq!(
            requests,
            vec![AffinityRequest::new(0, 0b0000_1111).expect("mask")]
        );
        let requests = reserve_last_n_cores_to_requests(16, 0).expect("valid");
        assert_eq!(
            requests,
            vec![AffinityRequest::new(0, 0xffff).expect("mask")]
        );
    }

    #[test]
    fn reserve_last_n_cores_spans_multiple_groups() {
        // 96 逻辑核 = 组0(64) + 组1(32)；保留最后 4 核 → 组1 只剩低 28 位。
        let requests = reserve_last_n_cores_to_requests(96, 4).expect("valid");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0],
            AffinityRequest::new(0, u64::MAX).expect("mask")
        );
        assert_eq!(
            requests[1],
            AffinityRequest::new(1, 0x0fff_ffff).expect("mask")
        );

        let plan = AffinityPlan::reserve_last_n_cores(96, 4).expect("valid plan");
        assert_eq!(plan.selected_logical(), 92);
        assert_eq!(plan.total_logical(), 96);
        assert!(!plan.is_single_group());
        assert_eq!(plan.primary_group(), Some(0));
        assert_eq!(plan.per_group().len(), 2);
        assert!(plan.per_group().iter().all(AffinityPlan::is_single_group));
    }

    #[test]
    fn reserve_last_n_cores_crosses_the_group_boundary() {
        // 保留 32 个：组1(32 核) 全被保留 → 该组从计划中消失，只剩组0。
        let requests = reserve_last_n_cores_to_requests(96, 32).expect("valid");
        assert_eq!(
            requests,
            vec![AffinityRequest::new(0, u64::MAX).expect("mask")]
        );
        let plan = AffinityPlan::reserve_last_n_cores(96, 32).expect("valid plan");
        assert_eq!(plan.selected_logical(), 64);
        assert!(plan.is_single_group());

        // 保留 64 个：组1 消失，组0 只剩低 32 核。
        let plan = AffinityPlan::reserve_last_n_cores(96, 64).expect("valid plan");
        assert_eq!(
            plan.requests(),
            &[AffinityRequest::new(0, 0xffff_ffff).expect("mask")]
        );
        assert_eq!(plan.selected_logical(), 32);

        // 保留 65 个：组1 消失，组0 只剩低 31 核。
        let plan = AffinityPlan::reserve_last_n_cores(96, 65).expect("valid plan");
        assert_eq!(plan.selected_logical(), 31);

        // 保留数必须严格小于逻辑处理器总数，否则没有核可以绑定。
        assert!(reserve_last_n_cores_to_requests(96, 96).is_err());
        assert!(reserve_last_n_cores_to_requests(96, 100).is_err());
        assert!(reserve_last_n_cores_to_requests(0, 0).is_err());
        assert!(reserve_last_n_cores_to_requests(4, 4).is_err());
        assert_eq!(
            reserve_last_n_cores_to_requests(4, 4)
                .expect_err("reserve >= total")
                .kind(),
            crate::HalErrorKind::InvalidArgument
        );
    }

    #[test]
    fn plan_validation_rejects_bad_input() {
        // 空掩码在 AffinityRequest 层就不可表达（字段私有，只能经 new 构造）。
        let empty = AffinityRequest::new(0, 0).expect_err("empty mask must be rejected");
        assert_eq!(empty.kind(), crate::HalErrorKind::InvalidArgument);

        assert!(AffinityPlan::from_requests(16, Vec::new()).is_err());
        assert!(AffinityPlan::from_requests(0, Vec::new()).is_err());

        // 组序号必须存在且递增。
        let ok = AffinityRequest::new(0, 0x3).expect("mask");
        assert!(AffinityPlan::from_requests(16, vec![ok]).is_ok());
        assert!(
            AffinityPlan::from_requests(16, vec![AffinityRequest::new(1, 0x3).expect("mask")])
                .is_err()
        );
        let group0 = AffinityRequest::new(0, 0x3).expect("mask");
        let group1 = AffinityRequest::new(1, 0x3).expect("mask");
        assert!(AffinityPlan::from_requests(96, vec![group0, group1]).is_ok());
        assert!(AffinityPlan::from_requests(96, vec![group1, group0]).is_err());

        // 掩码不得超出该组容量：32 核机器上第 33 位非法。
        assert!(AffinityPlan::from_requests(
            32,
            vec![AffinityRequest::new(0, 1 << 32).expect("mask")]
        )
        .is_err());
        assert!(AffinityPlan::from_requests(
            32,
            vec![AffinityRequest::new(0, 1 << 31).expect("mask")]
        )
        .is_ok());
    }

    #[test]
    fn full_plan_selects_every_logical_processor() {
        let plan = AffinityPlan::full(96).expect("valid");
        assert!(plan.is_all());
        assert_eq!(plan.selected_logical(), 96);
        // 掩码按 18 列零填充（含 0x 前缀），便于日志里纵向对齐。
        assert_eq!(
            plan.to_string(),
            "96 of 96 logical processors [g0=0xffffffffffffffff, g1=0x00000000ffffffff]"
        );
    }

    #[test]
    fn affinity_method_names_are_stable() {
        assert_eq!(
            AffinityMethod::ProcessAffinityMask.as_str(),
            "process_affinity_mask"
        );
        assert_eq!(
            AffinityMethod::ThreadGroupAffinity.as_str(),
            "thread_group_affinity"
        );
    }
}
