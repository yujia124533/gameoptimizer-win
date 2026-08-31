#pragma once

#include <functional>
#include <string>

#include "hardware/HardwareProfile.h"
#include "license/License.h"
#include "preset/GamePreset.h"
#include "rollback/SecurityRollback.h"

namespace gopt {

// 应用配置（CLI / UI 绑定层；后续可扩展为配置文件）
struct AppConfig {
    bool allowPowerSchemeSwitch = false;  // 电源切换默认关闭（需管理员权限）
    bool autoRollbackOnUnstable = true;   // 看门狗检测到异常时自动回滚
    std::string gameExeOverride;          // 可选：代启动游戏的可执行文件路径
};

// 协调层：用户选择游戏 → 探测硬件 → 解析预设 → 快照 → 应用 → 看门狗
class AppCore {
public:
    explicit AppCore(const AppConfig& cfg = AppConfig());

    // 流程事件：每一步「开始 / 完成」含真实耗时，供 GUI 逐条动画与进度条
    struct FlowEvent {
        enum Kind { Info, StepStart, StepOk, StepFail };
        Kind kind = Info;
        std::string text;    // 步骤名 / 信息文本
        int step = 0;        // 当前步骤号（1 起）
        int total = 0;       // 总步骤数（进度条）
        int elapsedMs = 0;   // StepOk/Fail 该步真实耗时
    };
    using FlowCallback = std::function<void(const FlowEvent&)>;

    // 全流程优化；返回人类可读结果（含失败 / 降级说明）
    std::string OptimizeForGame(GameId id, const FlowCallback& cb = {}, int stepDelayMs = 0);

    // 一键优化：自动检测第一个运行中的支持游戏并应用其预设；无游戏时做系统级性能优化
    std::string OptimizeAuto(const FlowCallback& cb = {}, int stepDelayMs = 0);

    // 系统级一键性能优化（无需游戏运行）：高性能电源方案等
    std::string OptimizeSystem();

    // 回滚最近一次优化（并停止看门狗）
    std::string Rollback();
    // 全部回滚（并停止看门狗）
    std::string RollbackAll();

    // 信息查询
    HardwareProfile Profile() const;
    GamePreset ResolvedPreset(GameId id) const;
    LicenseInfo License() const;

    // 看门狗轮询：false = 系统响应异常（AppCore 未配置自动回滚时由调用方处理）
    bool IsStable() const;
    void StopWatchdog();
    bool HasActiveOptimization() const;  // 是否已实际应用优化（供 CLI 决定是否监控）

private:
    AppConfig config_;
    SecurityRollback rollback_;
    HardwareProfile cachedProfile_;
    GameId lastGame_ = GameId::DeltaForce;
    LicenseInfo license_;  // 当前授权（免费版/Pro），用于功能门控
};

}  // namespace gopt
