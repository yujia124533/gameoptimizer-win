// GameOptimizer 命令行入口（第 5 步：AppCore 协调层 + CLI，发布版）
#include <cctype>
#include <chrono>
#include <cstdio>
#include <string>
#include <thread>

#include "config/GameConfig.h"
#include "core/AppCore.h"
#include "hal/HAL.h"
#include "i18n.h"
#include "license/License.h"
#include "version.h"

using gopt::AppConfig;
using gopt::AppCore;
using gopt::GameConfig;
using gopt::GameId;
using gopt::GameLaunchConfig;
using gopt::Lang;
using gopt::SetLang;
using gopt::T;

static void PrintUsage() {
    std::printf("GameOptimizer CLI v%s (build %d.%d.%d)\n",
                GOPT_VERSION_STR, GOPT_VERSION_MAJOR, GOPT_VERSION_MINOR, GOPT_VERSION_PATCH);
    std::puts(T(
        "\n用法:\n"
        "  gopt_cli status                      查看硬件指纹、预设与授权\n"
        "  gopt_cli apply <game> [选项]         应用优化（自动快照 + 看门狗监控）\n"
        "  gopt_cli rollback                    回滚最近一次优化\n"
        "  gopt_cli rollback-all                回滚全部\n"
        "  gopt_cli fingerprint                 显示本机机器指纹（授权绑定用）\n"
        "  gopt_cli license status              查看授权状态\n"
        "  gopt_cli license activate <code>     安装授权码\n"
        "  gopt_cli license gen <hash> <ed> [expiry]   开发者：为机器指纹生成授权码\n"
        "\n游戏: deltaforce | lol | cs2 | pubg | valorant | apex | dota2 | ow\n"
        "\n选项 (apply):\n"
        "  --game-exe <path>   工具代启动游戏（CREATE_SUSPENDED → 设置 → Resume）\n"
        "  --power             允许切换高性能电源方案（Pro，需管理员，默认关闭）\n"
        "  --lang zh|en        界面语言（默认 zh）\n"
        "\n版本: 免费版 = 优先级 + CPU 亲和性；Pro = 额外支持 电源/驱动帧延迟/工作集。\n"
        "安全边界: 无注入、无内核 Hook；优先级上限 HIGH；全部修改可一键回滚。",
        "\nUsage:\n"
        "  gopt_cli status                      Show hardware, presets and license\n"
        "  gopt_cli apply <game> [options]      Apply optimization (auto snapshot + watchdog)\n"
        "  gopt_cli rollback                    Rollback the last optimization\n"
        "  gopt_cli rollback-all                Rollback everything\n"
        "  gopt_cli fingerprint                 Show machine fingerprint (for licensing)\n"
        "  gopt_cli license status              Show license status\n"
        "  gopt_cli license activate <code>     Install a license code\n"
        "  gopt_cli license gen <hash> <ed> [expiry]   Dev: generate a license code\n"
        "\nGames: deltaforce | lol | cs2 | pubg | valorant | apex | dota2 | ow\n"
        "\nOptions (apply):\n"
        "  --game-exe <path>   Tool starts the game (CREATE_SUSPENDED -> settings -> Resume)\n"
        "  --power             Allow high-performance power scheme (Pro, admin, off by default)\n"
        "  --lang zh|en        UI language (default zh)\n"
        "\nFree = priority + CPU affinity; Pro adds power / frame latency / working set.\n"
        "Safety: no injection, no kernel hooks; priority capped at HIGH; every change is rollable."));
}

// 解析游戏名（中英文别名），失败返回 false
static bool ParseGame(const char* s, GameId* out) {
    if (!s || !*s) return false;
    std::string v(s);
    for (char& c : v) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    if (v == "deltaforce" || v == "df" || v == "三角洲" || v == "三角洲行动") {
        *out = GameId::DeltaForce;
        return true;
    }
    if (v == "lol" || v == "league" || v == "lol1" || v == "英雄联盟") {
        *out = GameId::LeagueOfLegends;
        return true;
    }
    if (v == "cs2" || v == "cs") {
        *out = GameId::CS2;
        return true;
    }
    if (v == "pubg" || v == "吃鸡" || v == "绝地求生") {
        *out = GameId::PUBG;
        return true;
    }
    if (v == "valorant" || v == "无畏" || v == "无畏契约") {
        *out = GameId::Valorant;
        return true;
    }
    if (v == "apex" || v == "apexlegends") {
        *out = GameId::Apex;
        return true;
    }
    if (v == "dota" || v == "dota2") {
        *out = GameId::Dota2;
        return true;
    }
    if (v == "ow" || v == "ow2" || v == "overwatch" || v == "守望") {
        *out = GameId::Overwatch2;
        return true;
    }
    return false;
}

int main(int argc, char** argv) {
    if (argc < 2) {
        PrintUsage();
        return 0;
    }
    const std::string cmd = argv[1];

    if (cmd == "--version" || cmd == "-v") {
        std::printf("GameOptimizer v%s\n", GOPT_VERSION_STR);
        return 0;
    }

    // 每游戏「优化启动」配置（独立解析，避免与公共选项冲突）
    if (cmd == "game") {
        GameId id = GameId::DeltaForce;
        bool haveGame = false;
        GameLaunchConfig gc;
        bool changed = false;
        for (int i = 2; i < argc; ++i) {
            const std::string a = argv[i];
            if (a == "--exe" && i + 1 < argc) { gc.exePath = argv[++i]; changed = true; }
            else if (a == "--args" && i + 1 < argc) { gc.args = argv[++i]; changed = true; }
            else if (a == "--power") { gc.powerScheme = true; changed = true; }
            else if (a == "--no-power") { gc.powerScheme = false; changed = true; }
            else if (a == "--auto") { gc.optimizedOnLaunch = true; changed = true; }
            else if (a == "--no-auto") { gc.optimizedOnLaunch = false; changed = true; }
            else if (a == "--latency") { gc.frameLatency = true; changed = true; }
            else if (a == "--no-latency") { gc.frameLatency = false; changed = true; }
            else if (a == "--ws") { gc.workingSet = true; changed = true; }
            else if (a == "--no-ws") { gc.workingSet = false; changed = true; }
            else if (!a.empty() && a[0] != '-' && !haveGame) {
                if (ParseGame(a.c_str(), &id)) haveGame = true;
            }
        }
        if (!haveGame) {
            std::puts(T("用法: gopt_cli game <game> [--exe <路径>] [--args \"<参数>\"] "
                        "[--power] [--no-power] [--auto] [--no-auto] [--latency] [--no-latency] [--ws] [--no-ws]",
                        "Usage: gopt_cli game <game> [--exe <path>] [--args \"<...>\"] ..."));
            return 1;
        }
        gc = changed ? (GameConfig::Set(id, gc) ? gc : GameConfig::Get(id))
                     : GameConfig::Get(id);
        std::printf("%s %s:\n"
                    "  exe   : %s\n  args  : %s\n  auto  : %s\n  power : %s\n"
                    "  frame : %s\n  wkset : %s\n  cfg   : %s\n",
                    T("配置", "config"), gopt::GameIdToString(id).c_str(),
                    gc.exePath.empty() ? T("(未设置)", "(unset)") : gc.exePath.c_str(),
                    gc.args.empty() ? T("(无)", "(none)") : gc.args.c_str(),
                    gc.optimizedOnLaunch ? T("开", "on") : T("关", "off"),
                    gc.powerScheme ? T("开", "on") : T("关", "off"),
                    gc.frameLatency ? T("开", "on") : T("关", "off"),
                    gc.workingSet ? T("开", "on") : T("关", "off"),
                    GameConfig::ConfigPath().c_str());
        return 0;
    }

    // 解析公共选项
    std::string gameArg, gameExe;
    AppConfig cfg;
    for (int i = 2; i < argc; ++i) {
        const std::string a = argv[i];
        if (a == "--game-exe" && i + 1 < argc) {
            gameExe = argv[++i];
        } else if (a == "--power") {
            cfg.allowPowerSchemeSwitch = true;
        } else if (a == "--lang" && i + 1 < argc) {
            SetLang(std::string(argv[++i]) == "en" ? Lang::En : Lang::Zh);
        } else if (gameArg.empty() && !a.empty() && a[0] != '-') {
            gameArg = a;
        }
    }

    if (cmd == "status") {
        AppCore core(cfg);
        const gopt::LicenseInfo li = core.License();
        std::printf("%s: %s (%s)\n", T("授权", "License"),
                    li.isPro ? "Pro" : T("免费版", "Free"), li.message.c_str());
        std::puts(core.Profile().ToString().c_str());
        std::puts(T("\n预设概览：", "\nPresets overview:"));
        for (const GameId id : {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                                GameId::PUBG, GameId::Valorant, GameId::Apex,
                                GameId::Dota2, GameId::Overwatch2}) {
            const gopt::GamePreset p = core.ResolvedPreset(id);
            std::printf("  %-10s %s\n", gopt::GameIdToString(id).c_str(), p.description.c_str());
        }
        GUID scheme{};
        if (gopt::HAL::QueryActivePowerScheme(&scheme)) {
            std::printf("%s: %s\n", T("当前电源方案", "Power scheme"),
                        gopt::HAL::PowerSchemeName(scheme).c_str());
        }
        std::printf("%s: %s\n", T("驱动级帧延迟配置", "Driver frame latency"),
                    gopt::HAL::IsDriverFrameLatencySupported(core.Profile())
                        ? T("支持", "supported")
                        : T("不支持（将降级跳过）", "unsupported (degraded)"));
        return 0;
    }

    if (cmd == "fingerprint") {
        AppCore core(cfg);
        const gopt::MachineFingerprint mf = gopt::ComputeMachineFingerprint(core.Profile());
        std::printf("本机机器指纹:\n  主板序列号 : %s\n  指纹哈希   : %s\n",
                    mf.boardSerial.empty() ? "(未读出)" : mf.boardSerial.c_str(),
                    mf.hash.c_str());
        return 0;
    }

    if (cmd == "license" && argc >= 3) {
        const std::string sub = argv[2];
        AppCore core(cfg);
        if (sub == "status") {
            const gopt::LicenseInfo li = gopt::License::Check(core.Profile());
            std::printf("授权状态:\n  %s\n  %s\n", li.valid ? "有效" : "无效/未激活",
                        li.message.c_str());
            return 0;
        }
        if (sub == "activate" && argc >= 4) {
            const gopt::LicenseInfo li = gopt::License::Activate(argv[3], core.Profile());
            std::printf("激活结果: %s\n", li.message.c_str());
            return li.valid ? 0 : 1;
        }
        if (sub == "gen" && argc >= 5) {
            const std::string code = gopt::License::Generate(
                argv[3], argv[4], argc >= 6 ? argv[5] : "permanent");
            std::printf("授权码:\n%s\n", code.c_str());
            return 0;
        }
    }

    if (cmd == "apply") {
        GameId id;
        if (!ParseGame(gameArg.c_str(), &id)) {
            std::printf("未知游戏: %s（支持 deltaforce / lol / cs2）\n", gameArg.c_str());
            return 1;
        }
        cfg.gameExeOverride = gameExe;
        AppCore core(cfg);
        std::puts(core.OptimizeForGame(id).c_str());

        if (!core.HasActiveOptimization()) {
            std::puts("\n（未实际应用优化，已跳过监控）");
            return 0;
        }

        std::puts("\n正在监控系统响应（最长 30 秒）...");
        bool stable = true;
        for (int i = 0; i < 60; ++i) {
            std::this_thread::sleep_for(std::chrono::milliseconds(500));
            if (!core.IsStable()) {
                stable = false;
                std::printf("检测到系统响应异常，自动回滚: %s\n", core.Rollback().c_str());
                break;
            }
        }
        if (stable) {
            core.StopWatchdog();
            std::puts("监控结束：系统响应正常，优化保持生效。");
        }
        return 0;
    }

    if (cmd == "optimize") {
        AppCore core(cfg);
        if (gameArg.empty()) {
            // 一键：有运行中的游戏→优化游戏；否则系统级性能优化（无需游戏）
            std::puts(core.OptimizeAuto().c_str());
        } else if (gameArg == "system" || gameArg == "sys") {
            std::puts(core.OptimizeSystem().c_str());
        } else {
            GameId id;
            if (!ParseGame(gameArg.c_str(), &id)) {
                std::printf(T("未知游戏: %s（支持 deltaforce / lol / cs2 等）\n",
                              "Unknown game: %s (deltaforce / lol / cs2 ...)\n"),
                            gameArg.c_str());
                return 1;
            }
            cfg.gameExeOverride = gameExe;
            AppCore core2(cfg);
            std::puts(core2.OptimizeForGame(id).c_str());
        }
        if (!core.HasActiveOptimization()) {
            std::puts(T("\n（未实际应用优化，已跳过监控）", "\n(not applied, skip monitor)"));
            return 0;
        }
        std::puts(T("\n正在监控系统响应（最长 30 秒）...",
                    "\nMonitoring system response (max 30s)..."));
        bool stable = true;
        for (int i = 0; i < 60; ++i) {
            std::this_thread::sleep_for(std::chrono::milliseconds(500));
            if (!core.IsStable()) {
                stable = false;
                std::printf(T("检测到系统响应异常，自动回滚: %s\n",
                              "Abnormal system response, auto rollback: %s\n"),
                            core.Rollback().c_str());
                break;
            }
        }
        if (stable) {
            core.StopWatchdog();
            std::puts(T("监控结束：系统响应正常，优化保持生效。",
                        "Monitor done: system stable, optimization kept."));
        }
        return 0;
    }

    if (cmd == "rollback") {
        AppCore core(cfg);
        std::puts(core.Rollback().c_str());
        return 0;
    }

    if (cmd == "rollback-all") {
        AppCore core(cfg);
        std::puts(core.RollbackAll().c_str());
        return 0;
    }

    PrintUsage();
    return 0;
}
