#pragma once

#include <string>

#include "preset/GamePreset.h"

namespace gopt {

// 每款游戏的「优化启动」配置（代启动 / 一键优化使用；随游戏持久化）
struct GameLaunchConfig {
    bool optimizedOnLaunch = true;  // 代启动时自动应用优化
    bool powerScheme = false;       // 允许电源方案切换（Pro + 管理员）
    bool frameLatency = false;      // 允许驱动级帧延迟（Pro）
    bool workingSet = true;         // 允许工作集策略（Pro）
    std::string exePath;            // 该游戏的 exe 路径（优化启动用）
    std::string args;               // 启动参数（原样附加）
};

// 每游戏配置存储：%LOCALAPPDATA%\GameOptimizer\games.conf
// 格式（逐行，| 分隔）：<gameIndex>|<exe>|<args>|<oI:power:frameLatency:workingSet>
class GameConfig {
public:
    static GameLaunchConfig Get(GameId id);
    // 写入并立即存盘；返回是否成功
    static bool Set(GameId id, const GameLaunchConfig& cfg);
    static bool Remove(GameId id);
    static std::string ConfigPath();
};

}  // namespace gopt
