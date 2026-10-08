//! 硬件信息值类型（`SystemApi::hardware()` 的返回契约）。
//!
//! 与 C++ 版 `HardwareProfile` 对齐，但只保留官方用户态 API 可稳定获得的字段：
//! 内存频率需要解析 SMBIOS、最大睿频需要 CPUID leaf 0x16，二者都不在本任务范围内，
//! 因此这里**不提供**永久为 0 的字段（宁缺毋滥，避免下游误以为是真实测量值）。

use crate::affinity::ProcessorGroup;

/// 单个物理核的布局：所属处理器组 + 组内逻辑处理器位掩码。
///
/// 供"仅绑定物理核"（CS2 预设）与"保留核给系统"（LoL 预设）两类亲和性策略使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoreLayout {
    /// 物理核序号（0 起，按系统枚举顺序）。
    pub index: u32,
    /// 所属处理器组（逻辑处理器 >64 时可能非 0）。
    pub group: u32,
    /// 该物理核在组内的逻辑处理器位掩码（含超线程兄弟）。
    pub affinity: u64,
}

impl CoreLayout {
    /// 构造。
    pub const fn new(index: u32, group: u32, affinity: u64) -> Self {
        Self {
            index,
            group,
            affinity,
        }
    }

    /// 该物理核包含的逻辑处理器数（1 = 无超线程，2 = 典型 SMT）。
    pub const fn logical_count(&self) -> u32 {
        self.affinity.count_ones()
    }
}

/// GPU 厂商分类（由 PCI Vendor ID 判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    /// NVIDIA（0x10DE）。
    Nvidia,
    /// AMD（0x1002）。
    Amd,
    /// Intel（0x8086）。
    Intel,
    /// Microsoft 基础渲染驱动（0x1414）。
    Microsoft,
    /// 未知厂商。
    Unknown,
}

impl GpuVendor {
    /// 由 PCI Vendor ID 判定。
    pub const fn from_vendor_id(vendor_id: u32) -> Self {
        match vendor_id {
            0x10de => GpuVendor::Nvidia,
            0x1002 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            0x1414 => GpuVendor::Microsoft,
            _ => GpuVendor::Unknown,
        }
    }

    /// 稳定短名（JSON / 审计日志用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "nvidia",
            GpuVendor::Amd => "amd",
            GpuVendor::Intel => "intel",
            GpuVendor::Microsoft => "microsoft",
            GpuVendor::Unknown => "unknown",
        }
    }
}

/// 首选显示适配器信息（DXGI 枚举结果）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GpuInfo {
    /// 厂商分类。
    pub vendor: GpuVendor,
    /// PCI Vendor ID。
    pub vendor_id: u32,
    /// PCI Device ID。
    pub device_id: u32,
    /// 适配器描述（如 `NVIDIA GeForce RTX 4070`）。
    pub model: String,
    /// 独显显存（MB，`DedicatedVideoMemory`）。
    pub vram_mb: u64,
    /// WDDM 驱动版本（`31.0.15.3742` 形式），不可得时为 `None`。
    pub driver_version: Option<String>,
    /// 是否为真实硬件（排除 Microsoft Basic Render Driver）。
    pub is_hardware: bool,
    /// 是否为软件渲染适配器（`DXGI_ADAPTER_FLAG_SOFTWARE`）。
    pub is_software_adapter: bool,
}

/// 硬件画像：一次 `hardware()` 调用的完整结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HardwareInfo {
    /// CPU 型号名（注册表 `ProcessorNameString`）。
    pub cpu_model: String,
    /// 物理核数。
    pub physical_cores: u32,
    /// 逻辑处理器数（含 SMT）。
    pub logical_cores: u32,
    /// 是否启用超线程/SMT（逻辑核 > 物理核）。
    pub supports_hyper_threading: bool,
    /// CPU 标称频率（MHz，注册表 `~MHz`），未知为 0。
    pub cpu_base_freq_mhz: u32,
    /// 物理核布局（逐核的组与掩码）。
    pub core_layout: Vec<CoreLayout>,
    /// 处理器组划分（>64 逻辑核时多组）。
    pub processor_groups: Vec<ProcessorGroup>,
    /// 首选显示适配器；DXGI 探测失败时为 `None`。
    pub gpu: Option<GpuInfo>,
    /// 系统总内存（MB）。
    pub system_ram_mb: u64,
    /// 当前可用内存（MB）。
    pub available_ram_mb: u64,
    /// 本进程是否持有 `SeLockMemoryPrivilege`（大页可用性）。
    pub large_pages_available: bool,
    /// 降级说明：哪些子项没能探测到、为什么（供 CLI/报告展示，不影响调用方流程）。
    pub warnings: Vec<String>,
}

impl HardwareInfo {
    /// 逻辑核数是否跨处理器组（此时进程亲和性需要逐线程组绑定）。
    pub fn is_multi_group(&self) -> bool {
        self.processor_groups.len() > 1
    }
}
