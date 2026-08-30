#include "core/AppCore.h"

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

#include <windows.h>
#include <tlhelp32.h>

#include <algorithm>
#include <cctype>
#include <sstream>

#include "config/GameConfig.h"
#include "hal/HAL.h"
#include "hardware/HardwareDetector.h"

namespace gopt {

namespace {

// UTF-8 std::string <-> std::wstring（Windows 路径与进程名转换）
std::wstring Utf8ToWide(const std::string& s) {
    if (s.empty()) return {};
    const int len = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
    if (len <= 1) return {};
    std::wstring w(static_cast<size_t>(len) - 1, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, &w[0], len);
    return w;
}

std::string WideToUtf8(const wchar_t* wstr) {
    if (!wstr || !*wstr) return {};
    const int len = WideCharToMultiByte(CP_UTF8, 0, wstr, -1, nullptr, 0, nullptr, nullptr);
    if (len <= 1) return {};
    std::string s(static_cast<size_t>(len) - 1, '\0');
    WideCharToMultiByte(CP_UTF8, 0, wstr, -1, &s[0], len, nullptr, nullptr);
    return s;
}

// 按可执行文件名查找运行中的进程 PID（大小写不敏感，返回首个匹配）
uint32_t FindProcessByExeName(const std::wstring& exeName) {
    HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snap == INVALID_HANDLE_VALUE) return 0;

    std::wstring lowerTarget = exeName;
    std::transform(lowerTarget.begin(), lowerTarget.end(), lowerTarget.begin(),
                   [](wchar_t c) { return static_cast<wchar_t>(std::towlower(c)); });

    uint32_t pid = 0;
    PROCESSENTRY32W pe{};
    pe.dwSize = sizeof(pe);
    if (Process32FirstW(snap, &pe)) {
        do {
            std::wstring name = pe.szExeFile;
            std::transform(name.begin(), name.end(), name.begin(),
                           [](wchar_t c) { return static_cast<wchar_t>(std::towlower(c)); });
            if (name == lowerTarget) {
                pid = pe.th32ProcessID;
                break;
            }
        } while (Process32NextW(snap, &pe));
    }
    CloseHandle(snap);
    return pid;
}

const GameId kAllGames[] = {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                            GameId::PUBG, GameId::Valorant, GameId::Apex,
                            GameId::Dota2, GameId::Overwatch2};

}  // namespace

AppCore::AppCore(const AppConfig& cfg) : config_(cfg) {
    cachedProfile_ = HardwareDetector::Detect();
    license_ = License::Check(cachedProfile_);
}

HardwareProfile AppCore::Profile() const {
    return cachedProfile_;
}

GamePreset AppCore::ResolvedPreset(GameId id) const {
    return GameOptimizationPreset::Resolve(id, cachedProfile_);
}

LicenseInfo AppCore::License() const {
    return license_;
}

std::string AppCore::OptimizeForGame(GameId id) {
    lastGame_ = id;
    const std::string gameName = GameIdToString(id);
    GamePreset preset = ResolvedPreset(id);

    // 所有功能免费：无授权门控；优化项仅受系统能力限制（管理员/厂商库支持）
    // 每游戏「优化启动」开关门控
    const GameLaunchConfig gc = GameConfig::Get(id);
    if (!gc.frameLatency) preset.gpuMaxFrames = 0;
    if (!gc.workingSet) {
        preset.workingSetMinMB = 0;
        preset.workingSetMaxMB = 0;
    }
    if (!gc.powerScheme) preset.switchHighPerformancePower = false;

    std::ostringstream os;
    os << "== 优化 " << gameName << " ==\n";
    os << "所有功能免费：全部优化项对所有人开放。\n";
    os << "预设: " << preset.description << "\n";

    // 1) 找到或代启动游戏进程（路径：CLI/GUI 显式 > 每游戏配置）
    const std::wstring launchExe = !config_.gameExeOverride.empty()
                                       ? Utf8ToWide(config_.gameExeOverride)
                                       : Utf8ToWide(gc.exePath);
    uint32_t pid = FindProcessByExeName(GameExeName(id));
    if (pid == 0 && !launchExe.empty()) {
        const bool applyOnLaunch = gc.optimizedOnLaunch;
        HAL::LaunchResult lr = HAL::LaunchGameSuspended(
            launchExe, Utf8ToWide(gc.args),
            applyOnLaunch ? preset.processPriorityClass : 0,
            preset.cpuAffinityGroup,
            applyOnLaunch ? preset.cpuAffinityMask : 0,
            applyOnLaunch ? preset.workingSetMinMB : 0,
            applyOnLaunch ? preset.workingSetMaxMB : 0);
        if (lr.ok) {
            pid = lr.pid;
            os << "代启动: pid=" << lr.pid
               << (applyOnLaunch ? "" : "（未自动应用优化，仅启动）");
            if (!lr.error.empty()) os << "（非致命警告: " << lr.error << "）";
            os << "\n";
            CloseHandle(lr.hThread);
            CloseHandle(lr.hProcess);
        } else {
            os << "代启动失败: " << lr.error << "\n";
        }
    }

    if (pid == 0) {
        os << "未找到运行中的 " << WideToUtf8(GameExeName(id).c_str())
           << "，且未提供可执行文件路径。\n"
           << "请先启动游戏，或使用 --game-exe <路径> 让工具代启动。\n";
        return os.str();
    }

    // 2) 快照（应用任何修改之前）
    rollback_.CreateSavePoint(pid, gameName);

    // 3) 应用（代启动模式下设置已生效，这里幂等重放以统一记录）
    const SecurityRollback::ApplyReport rep = SecurityRollback::ApplyPreset(pid, preset);
    os << "应用结果: 成功 " << rep.appliedCount << " 项";
    if (rep.failedCount > 0) os << "，失败 " << rep.failedCount << " 项";
    os << "\n";
    for (const auto& f : rep.failures) os << "  - " << f << "\n";

    // 4) 电源方案（默认关闭；需要管理员权限）
    if (config_.allowPowerSchemeSwitch && preset.switchHighPerformancePower) {
        if (HAL::ActivateHighPerformanceScheme()) {
            os << "电源方案: 已切换高性能\n";
        } else {
            os << "电源方案: 切换失败（" << HAL::LastErrorText() << "）\n";
        }
    } else if (preset.switchHighPerformancePower) {
        os << "电源方案: 未启用（需 --power 且以管理员运行）\n";
    }

    // 5) 看门狗
    SecurityRollback::WatchdogConfig wc;
    rollback_.StartWatchdog(wc);
    os << "看门狗: 已启动（10 秒宽限期；异常将"
       << (config_.autoRollbackOnUnstable ? "自动回滚" : "提示人工回滚") << "）\n";
    return os.str();
}

std::string AppCore::Rollback() {
    rollback_.StopWatchdog();
    const bool ok = rollback_.RollbackToLastSave();
    return ok ? "已回滚最近一次优化。" : "回滚失败或部分失败: " + rollback_.LastErrorText();
}

std::string AppCore::OptimizeAuto() {
    // 一键：检测第一个运行中的支持游戏并应用其预设
    for (const GameId id : kAllGames) {
        if (FindProcessByExeName(GameExeName(id)) != 0) {
            return OptimizeForGame(id);
        }
    }
    // 无游戏运行 → 系统级一键性能优化（无需先启动游戏）
    return OptimizeSystem();
}

std::string AppCore::OptimizeSystem() {
    std::ostringstream os;
    os << "== 系统一键性能优化 ==\n";
    os << "所有功能免费：全部优化项对所有人开放。\n";
    if (!config_.allowPowerSchemeSwitch) {
        os << "未启用电源方案切换（需勾选/--power）。启用后将无需游戏运行即可切高性能。\n";
        return os.str();
    }
    if (HAL::ActivateHighPerformanceScheme()) {
        os << "电源方案: 已切换高性能（无需游戏运行）。\n";
    } else {
        os << "电源方案: 切换失败（" << HAL::LastErrorText() << "，可能需管理员权限）。\n";
    }
    return os.str();
}

std::string AppCore::RollbackAll() {
    rollback_.StopWatchdog();
    const bool ok = rollback_.RollbackAll();
    return ok ? "已回滚全部优化。"
              : "回滚存在失败项（详见日志）: " + rollback_.LastErrorText();
}

bool AppCore::IsStable() const {
    return rollback_.IsSystemStable();
}

bool AppCore::HasActiveOptimization() const {
    return rollback_.SavePointCount() > 0;
}

void AppCore::StopWatchdog() {
    rollback_.StopWatchdog();
}

}  // namespace gopt
