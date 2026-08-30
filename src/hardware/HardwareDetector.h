#pragma once

#include "hardware/HardwareProfile.h"

namespace gopt {

// 硬件探测：CPU / GPU / 内存全量识别，统一抽象为 HardwareProfile。
// 仅使用官方用户态 API（GetSystemInfo / GetLogicalProcessorInformationEx /
// DXGI 枚举 / GlobalMemoryStatusEx / SMBIOS / 注册表），不做任何内核级操作。
class HardwareDetector {
public:
    // 全量探测（目标耗时 <100ms）。结果内部缓存，设备热插拔后调用 ResetCache() 重新探测。
    static HardwareProfile Detect();

    // 特性能力检查（基于最近一次探测结果；未探测时自动触发探测）。
    static bool IsGpuFeatureSupported(GpuFeature feature);

    // 清除缓存，强制下一次 Detect() 重新探测。
    static void ResetCache();
};

}  // namespace gopt
