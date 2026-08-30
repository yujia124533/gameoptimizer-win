#include "preset/GamePreset.h"

#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0601
#endif
#ifndef WINVER
#define WINVER 0x0601
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif

#include <windows.h>  // HIGH_PRIORITY_CLASS 等

#include "hal/HAL.h"  // 驱动级帧延迟能力检查（降级判定）

namespace gopt {

std::string GameIdToString(GameId id) {
    switch (id) {
        case GameId::DeltaForce:      return "三角洲行动";
        case GameId::LeagueOfLegends: return "英雄联盟";
        case GameId::CS2:             return "CS2";
        case GameId::PUBG:            return "绝地求生";
        case GameId::Valorant:        return "无畏契约";
        case GameId::Apex:            return "Apex Legends";
        case GameId::Dota2:           return "Dota 2";
        case GameId::Overwatch2:      return "守望先锋2";
    }
    return "未知游戏";
}

std::wstring GameExeName(GameId id) {
    switch (id) {
        case GameId::DeltaForce:      return L"DeltaForceClient-Win64-Shipping.exe";
        case GameId::LeagueOfLegends: return L"League of Legends.exe";
        case GameId::CS2:             return L"cs2.exe";
        case GameId::PUBG:            return L"TslGame.exe";
        case GameId::Valorant:        return L"VALORANT-Win64-Shipping.exe";
        case GameId::Apex:            return L"r5apex.exe";
        case GameId::Dota2:           return L"dota2.exe";
        case GameId::Overwatch2:      return L"Overwatch.exe";
    }
    return L"";
}

GamePreset GameOptimizationPreset::GetPreset(GameId id) {
    GamePreset p;
    switch (id) {
        case GameId::DeltaForce:
            p.processPriorityClass = HIGH_PRIORITY_CLASS;
            p.leaveCoresForSystem = 1;
            p.bindPhysicalOnly = false;
            p.gpuMaxFrames = 1;
            p.switchHighPerformancePower = false;
            p.description =
                "High 优先级；全逻辑核（保留 1 核给系统）；帧延迟 1；工作集按内存降级";
            break;

        case GameId::LeagueOfLegends:
            p.processPriorityClass = ABOVE_NORMAL_PRIORITY_CLASS;
            p.leaveCoresForSystem = 1;
            p.bindPhysicalOnly = false;
            p.gpuMaxFrames = 2;
            p.switchHighPerformancePower = false;
            p.description =
                "AboveNormal 优先级；全逻辑核（保留 1 核给系统）；帧延迟 2";
            break;

        case GameId::CS2:
            p.processPriorityClass = HIGH_PRIORITY_CLASS;
            p.leaveCoresForSystem = 1;
            p.bindPhysicalOnly = true;
            p.gpuMaxFrames = 1;
            p.workingSetMinMB = 256;
            p.switchHighPerformancePower = false;
            p.description =
                "High 优先级；仅物理核（保留 1 物理核给系统）；帧延迟 1；工作集下限 256MB";
            break;

        case GameId::PUBG:
            p.processPriorityClass = HIGH_PRIORITY_CLASS;
            p.leaveCoresForSystem = 1;
            p.bindPhysicalOnly = true;
            p.gpuMaxFrames = 1;
            p.workingSetMinMB = 512;
            p.description =
                "High 优先级；仅物理核（保留 1 物理核给系统）；帧延迟 1；工作集下限 512MB";
            break;

        case GameId::Valorant:
            p.processPriorityClass = HIGH_PRIORITY_CLASS;
            p.leaveCoresForSystem = 1;
            p.bindPhysicalOnly = false;
            p.gpuMaxFrames = 1;
            p.workingSetMinMB = 256;
            p.description =
                "High 优先级；全逻辑核（保留 1 核给系统）；帧延迟 1；工作集下限 256MB";
            break;

        case GameId::Apex:
            p.processPriorityClass = ABOVE_NORMAL_PRIORITY_CLASS;
            p.leaveCoresForSystem = 1;
            p.bindPhysicalOnly = false;
            p.gpuMaxFrames = 2;
            p.description =
                "AboveNormal 优先级；全逻辑核（保留 1 核给系统）；帧延迟 2";
            break;

        case GameId::Dota2:
            p.processPriorityClass = ABOVE_NORMAL_PRIORITY_CLASS;
            p.leaveCoresForSystem = 1;
            p.bindPhysicalOnly = false;
            p.gpuMaxFrames = 2;
            p.description =
                "AboveNormal 优先级；全逻辑核（保留 1 核给系统）；帧延迟 2";
            break;

        case GameId::Overwatch2:
            p.processPriorityClass = HIGH_PRIORITY_CLASS;
            p.leaveCoresForSystem = 1;
            p.bindPhysicalOnly = false;
            p.gpuMaxFrames = 1;
            p.workingSetMinMB = 256;
            p.description =
                "High 优先级；全逻辑核（保留 1 核给系统）；帧延迟 1；工作集下限 256MB";
            break;
    }
    return p;
}

GamePreset GameOptimizationPreset::Resolve(GameId id, const HardwareProfile& hw) {
    GamePreset p = GetPreset(id);
    ApplyHardwareDegradation(p, hw);

    // 亲和性掩码计算（仅当预设要求设置且未因降级被关闭时）
    if (p.leaveCoresForSystem >= 0) {
        p.cpuAffinityMask = ComputeAffinityMask(p, hw);
        if (p.cpuAffinityMask == 0) {
            p.leaveCoresForSystem = -1;  // 计算失败 / 硬件不支持 → 不设置亲和性
        }
    }
    return p;
}

void GameOptimizationPreset::ApplyHardwareDegradation(GamePreset& p, const HardwareProfile& hw) {
    // 1) 亲和性：物理核过少时绑定反而危险 → 不设置
    if (p.leaveCoresForSystem >= 0 && hw.physicalCores <= 2) {
        p.leaveCoresForSystem = -1;
    }
    // 2) 驱动级帧延迟：厂商库不可用 → 跳过
    if (p.gpuMaxFrames > 0 && !HAL::IsDriverFrameLatencySupported(hw)) {
        p.gpuMaxFrames = 0;
    }
    // 3) 工作集：内存 < 8GB → 跳过（避免挤占系统）；8~16GB → 下限减半
    if (p.workingSetMinMB > 0) {
        if (hw.systemRamMB < 8192) {
            p.workingSetMinMB = 0;
        } else if (hw.systemRamMB < 16384) {
            p.workingSetMinMB /= 2;
        }
    }
}

uint64_t GameOptimizationPreset::ComputeAffinityMask(const GamePreset& p,
                                                     const HardwareProfile& hw) {
    // v1 仅支持组 0（逻辑处理器 <= 64）；存在多组时降级为不设置
    bool hasMultiGroup = false;
    for (const auto& c : hw.coreLayout) {
        if (c.group != 0) {
            hasMultiGroup = true;
            break;
        }
    }
    if (hasMultiGroup) return 0;

    if (p.bindPhysicalOnly) {
        // coreLayout 每项 = 一个物理核（含其全部逻辑处理器）；跳过前 leaveCoresForSystem 个物理核
        uint64_t mask = 0;
        int skipped = 0;
        for (const auto& c : hw.coreLayout) {
            if (c.group != 0) continue;
            if (skipped < p.leaveCoresForSystem) {
                ++skipped;
                continue;
            }
            mask |= c.affinity;
        }
        return mask;
    }

    // 全逻辑核：全掩码，清除前 leaveCoresForSystem 个最低置位（保留给系统）
    uint64_t all = 0;
    for (const auto& c : hw.coreLayout) {
        if (c.group != 0) continue;
        all |= c.affinity;
    }
    uint64_t mask = all;
    int toClear = p.leaveCoresForSystem;
    for (int i = 0; i < 64 && toClear > 0; ++i) {
        if (mask & (1ull << i)) {
            mask &= ~(1ull << i);
            --toClear;
        }
    }
    return mask;
}

}  // namespace gopt
