#pragma once

#include <cstdint>
#include <string>

#include "hardware/HardwareProfile.h"

namespace gopt {

// 支持的游戏
enum class GameId { DeltaForce, LeagueOfLegends, CS2,
                    PUBG, Valorant, Apex, Dota2, Overwatch2 };

// 显示名（中文）与目标进程可执行文件名
std::string GameIdToString(GameId id);
std::wstring GameExeName(GameId id);

// 单款游戏的预设策略（静态配置；运行时按硬件指纹降级）
struct GamePreset {
    // ---- 静态配置 ----
    uint32_t processPriorityClass = 0;       // 0=不设置；ABOVE_NORMAL / HIGH（REALTIME 禁用）
    int leaveCoresForSystem = -1;            // -1=不设置亲和性；>=0=保留前 N 个逻辑/物理核给系统
    bool bindPhysicalOnly = false;           // true=仅绑定物理核（排除 HT 兄弟核）
    uint32_t gpuMaxFrames = 0;               // 驱动级最大帧延迟（1~3；0=不启用）
    uint64_t workingSetMinMB = 0;            // 工作集下限（0=不设置）
    uint64_t workingSetMaxMB = 0;            // 工作集上限（0=不设置）
    bool switchHighPerformancePower = false; // 是否建议切换高性能电源（实际开关由 AppCore 配置决定）
    std::string description;                 // 预设说明（日志/UI 展示）

    // ---- Resolve 输出（按硬件计算后的最终参数） ----
    uint32_t cpuAffinityGroup = 0;           // 恒 0（v1 仅组 0）
    uint64_t cpuAffinityMask = 0;            // 0 = 不设置亲和性
};

// 预设注册表
class GameOptimizationPreset {
public:
    // 读取原始静态预设
    static GamePreset GetPreset(GameId id);

    // 按硬件指纹降级后的最终可执行参数（含亲和性掩码计算）
    static GamePreset Resolve(GameId id, const HardwareProfile& hw);

private:
    static void ApplyHardwareDegradation(GamePreset& p, const HardwareProfile& hw);
    static uint64_t ComputeAffinityMask(const GamePreset& p, const HardwareProfile& hw);
};

}  // namespace gopt
