#include "hardware/HardwareProfile.h"

#include <sstream>

namespace gopt {

std::string HardwareProfile::ToString() const {
    std::ostringstream os;
    os << "CPU : " << cpuModel << "\n"
       << "      物理核 " << physicalCores << " / 逻辑核 " << logicalCores
       << (supportsHyperThreading ? "（含超线程）" : "") << "\n"
       << "      基础频率 " << cpuBaseFreqMHz << " MHz";
    if (cpuMaxTurboMHz > 0) {
        os << " / 最大睿频 " << cpuMaxTurboMHz << " MHz";
    }
    os << "\n"
       << "GPU : " << gpuVendor << " " << gpuModel
       << "（显存 " << vramMB << " MB）\n"
       << "      驱动版本 " << (gpuDriverVersion.empty() ? "未知" : gpuDriverVersion) << "\n"
       << "RAM : " << systemRamMB << " MB 总量 / " << availableRamMB << " MB 可用";
    if (memoryFreqMHz > 0) {
        os << " @" << memoryFreqMHz << " MHz";
    }
    os << "\n"
       << "大页: "
       << (largePagesEnabled ? "可用（SeLockMemoryPrivilege 已启用）"
                             : "不可用（SeLockMemoryPrivilege 未启用）");
    return os.str();
}

}  // namespace gopt
