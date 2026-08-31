#pragma once

#include <string>

#include "hardware/HardwareProfile.h"

namespace gopt {

// 系统级性能调优（BoosterX 风格的安全子集）：
//   - 电源方案：高性能 / 平衡
//   - 处理器性能档（powercfg 官方别名：PROCTHROTTLEMIN/MAX、PERFBOOSTMODE）
//   - 系统调度优先级注册表（Win32PrioritySeparation）
// 全部官方机制（PowerSetActiveScheme / powercfg.exe / 注册表），可快照恢复。
// 说明：不直接修改 CPU/GPU 电压与频率（真正超频需厂商工具/BIOS）。

class SystemTuner {
public:
    // 按硬件自动推荐：物理核>=8 且 内存>=16GB 建议高性能档
    static bool RecommendHighPerf(const HardwareProfile& hw);

    // 应用性能档位（highPerf=true 高性能档，否则平衡档）；先自动快照，返回人类可读结果
    static std::string Tune(const HardwareProfile& hw, bool highPerf);

    // 恢复最近一次调优（方案 + 注册表 + 处理器状态回默认档）
    static std::string Restore();

private:
    static std::string SnapshotPath();
    static bool ReadPrioritySeparation(unsigned long* out);
    static bool WritePrioritySeparation(unsigned long value);
};

}  // namespace gopt
