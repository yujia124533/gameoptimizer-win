#pragma once

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

    // 全流程优化；返回人类可读结果（含失败 / 降级说明）
    std::string OptimizeForGame(GameId id);

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
