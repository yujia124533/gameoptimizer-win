#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace gopt {

// GPU 特性能力枚举（供 HardwareDetector::IsGpuFeatureSupported 查询）
enum class GpuFeature {
    HardwareAdapter,  // 存在真实硬件 GPU（非 Microsoft Basic Render Driver）
    MaxFrameLatency,  // 可配置最大帧延迟（需要驱动级支持：NVAPI DRS / ADL）
};

// 单个物理核的布局：所属处理器组 + 组内逻辑处理器位掩码
// 供"仅绑定物理核"（CS2）与"保留核给系统"（LoL）等亲和性策略使用
struct CoreLayout {
    uint32_t group = 0;     // 处理器组编号（逻辑处理器超过 64 个时可能非 0）
    uint64_t affinity = 0;  // 该物理核在组内的逻辑处理器位掩码
};

// 硬件指纹：一次 Detect() 的完整结果
struct HardwareProfile {
    // ---- CPU ----
    std::string cpuModel;              // 型号名，如 "Intel(R) Core(TM) i7-13700K"
    int physicalCores = 0;             // 物理核数
    int logicalCores = 0;              // 逻辑处理器数（含超线程）
    bool supportsHyperThreading = false;
    uint32_t cpuBaseFreqMHz = 0;       // 基础频率（MHz）
    uint32_t cpuMaxTurboMHz = 0;       // 最大睿频（MHz，0 表示未知/不可用）
    std::vector<CoreLayout> coreLayout;

    // ---- GPU ----
    std::string gpuVendor;             // "NVIDIA" / "AMD" / "Intel" / "Unknown"
    uint32_t gpuVendorId = 0;          // PCI Vendor ID（0x10DE / 0x1002 / 0x8086）
    std::string gpuModel;              // DXGI 适配器描述
    uint64_t vramMB = 0;               // 独显显存（MB）
    std::string gpuDriverVersion;      // "31.0.15.3742" 形式
    bool gpuIsHardware = false;        // 是否真实硬件（排除微软基础渲染驱动）

    // ---- 内存 ----
    uint64_t systemRamMB = 0;          // 系统总内存（MB，系统可见部分）
    uint64_t availableRamMB = 0;       // 当前可用内存（MB）
    uint32_t memoryFreqMHz = 0;        // 内存频率（SMBIOS Type 17，0 表示未知）
    bool largePagesEnabled = false;    // 当前进程是否持有 SeLockMemoryPrivilege（大页可用性）

    // 供日志 / CLI 使用的可读输出
    std::string ToString() const;
};

}  // namespace gopt
