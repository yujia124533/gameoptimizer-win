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
#include "tuning/SystemTuner.h"

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

std::string AppCore::OptimizeForGame(GameId id, const FlowCallback& cb, int stepDelayMs) {
    lastGame_ = id;
    const std::string gameName = GameIdToString(id);
    GamePreset preset = ResolvedPreset(id);

    // 所有功能免费：无授权门控；每游戏「优化启动」开关门控
    const GameLaunchConfig gc = GameConfig::Get(id);
    if (!gc.frameLatency) preset.gpuMaxFrames = 0;
    if (!gc.workingSet) {
        preset.workingSetMinMB = 0;
        preset.workingSetMaxMB = 0;
    }
    if (!gc.powerScheme) preset.switchHighPerformancePower = false;
    if (!config_.allowPowerSchemeSwitch) preset.switchHighPerformancePower = false;

    // 总步骤数（目标进程 + 快照 + 逐项 + 看门狗）→ 进度条
    const int itemsCount = (preset.processPriorityClass != 0 ? 1 : 0) +
                           (preset.cpuAffinityMask != 0 ? 1 : 0) +
                           ((preset.workingSetMinMB != 0 || preset.workingSetMaxMB != 0) ? 1 : 0) +
                           (preset.switchHighPerformancePower ? 1 : 0);
    const int total = 2 + itemsCount + 1;

    std::ostringstream os;
    auto now = []() { return static_cast<int64_t>(GetTickCount64()); };
    // 事件输出：CLI 只保留结果行；回调（GUI）收到完整事件序列
    auto emit = [&](const FlowEvent& e) {
        if (e.kind == FlowEvent::Info) {
            os << e.text << "\n";
        } else if (e.kind == FlowEvent::StepOk || e.kind == FlowEvent::StepFail) {
            os << "  " << e.step << ") " << e.text << ": "
               << (e.kind == FlowEvent::StepOk ? "成功" : "失败")
               << "（" << e.elapsedMs << " ms）\n";
        }
        if (cb) {
            cb(e);
            if (stepDelayMs > 0) Sleep(static_cast<DWORD>(stepDelayMs));
        }
    };
    auto info = [&](const std::string& t) { emit({FlowEvent::Info, t, 0, 0, 0}); };
    auto start = [&](int step, const std::string& label) {
        emit({FlowEvent::StepStart, label, step, total, 0});
    };
    auto finish = [&](int step, const std::string& label, bool ok, int ms) {
        emit({ok ? FlowEvent::StepOk : FlowEvent::StepFail, label, step, total, ms});
    };

    info("== 优化 " + gameName + " ==");
    info("所有功能免费：全部优化项对所有人开放。");
    info("预设方案: " + preset.description);
    info("");
    info("-- 优化流程 --");

    int step = 1;
    // [1] 目标进程：找到运行中的游戏，或按配置代启动
    start(step, "目标进程");
    const int64_t tProc = now();
    const std::wstring launchExe = !config_.gameExeOverride.empty()
                                       ? Utf8ToWide(config_.gameExeOverride)
                                       : Utf8ToWide(gc.exePath);
    uint32_t pid = FindProcessByExeName(GameExeName(id));
    bool launched = false;
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
            launched = true;
            CloseHandle(lr.hThread);
            CloseHandle(lr.hProcess);
        }
    }
    finish(step, "目标进程", pid != 0, static_cast<int>(now() - tProc));
    if (pid == 0) {
        info("  未找到运行中的 " + WideToUtf8(GameExeName(id).c_str())
             + "，且未配置可执行文件路径。");
        info("  请先启动游戏，或用 game <name> --exe <路径> 配置后重试。");
        return os.str();
    }
    if (launched) info("  （代启动 pid=" + std::to_string(pid) + "）");
    ++step;

    // [2] 快照（持久化到磁盘，可跨进程回滚）
    start(step, "快照");
    const int64_t tSnap = now();
    rollback_.CreateSavePoint(pid, gameName);
    finish(step, "快照", true, static_cast<int>(now() - tSnap));
    ++step;

    // [3..] 逐项应用：每步独立执行并计时（真实耗时），失败不中断
    HANDLE h = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA,
                           FALSE, pid);
    auto applyItem = [&](const std::string& label, auto&& fn) {
        start(step, label);
        const int64_t t0 = now();
        const bool ok = fn();
        finish(step, label, ok, static_cast<int>(now() - t0));
        ++step;
    };
    if (h != nullptr) {
        if (preset.processPriorityClass != 0)
            applyItem("进程优先级", [&] { return HAL::SetProcessPriority(h, preset.processPriorityClass); });
        if (preset.cpuAffinityMask != 0)
            applyItem("CPU 亲和性", [&] { return HAL::SetProcessAffinity(h, preset.cpuAffinityGroup, preset.cpuAffinityMask); });
        if (preset.workingSetMinMB != 0 || preset.workingSetMaxMB != 0)
            applyItem("工作集", [&] { return HAL::SetProcessWorkingSetMB(h, preset.workingSetMinMB, preset.workingSetMaxMB); });
        if (preset.switchHighPerformancePower)
            applyItem("电源方案（高性能）", [&] { return HAL::ActivateHighPerformanceScheme(); });
        CloseHandle(h);
    } else {
        start(step, "应用优化");
        finish(step, "应用优化", false, 0);
        ++step;
    }

    // [最后] 看门狗
    start(step, "看门狗");
    SecurityRollback::WatchdogConfig wc;
    rollback_.StartWatchdog(wc);
    finish(step, "看门狗", true, 0);
    return os.str();
}

std::string AppCore::Rollback() {
    rollback_.StopWatchdog();
    const bool ok = rollback_.RollbackToLastSave();
    return ok ? "已回滚最近一次优化。" : "回滚失败或部分失败: " + rollback_.LastErrorText();
}

std::string AppCore::OptimizeAuto(const FlowCallback& cb, int stepDelayMs) {
    // 一键：检测第一个运行中的支持游戏并应用其预设
    for (const GameId id : kAllGames) {
        if (FindProcessByExeName(GameExeName(id)) != 0) {
            return OptimizeForGame(id, cb, stepDelayMs);
        }
    }
    // 无游戏运行 → 系统级一键性能优化（无需先启动游戏）
    std::ostringstream os;
    const auto now = []() { return static_cast<int64_t>(GetTickCount64()); };
    auto emit = [&](const FlowEvent& e) {
        if (e.kind == FlowEvent::Info) os << e.text << "\n";
        else if (e.kind == FlowEvent::StepOk || e.kind == FlowEvent::StepFail)
            os << "  " << e.step << ") " << e.text << ": "
               << (e.kind == FlowEvent::StepOk ? "成功" : "失败")
               << "（" << e.elapsedMs << " ms）\n";
        if (cb) { cb(e); if (stepDelayMs > 0) Sleep(static_cast<DWORD>(stepDelayMs)); }
    };
    emit({FlowEvent::Info, "== 系统一键性能优化 ==", 0, 0, 0});
    emit({FlowEvent::Info, "所有功能免费：全部优化项对所有人开放。", 0, 0, 0});
    emit({FlowEvent::Info, "", 0, 0, 0});
    emit({FlowEvent::Info, "-- 优化流程 --", 0, 0, 0});
    emit({FlowEvent::StepStart, "电源方案（高性能）", 1, 1, 0});
    const int64_t t0 = now();
    const bool trySwitch = config_.allowPowerSchemeSwitch;
    const bool ok = trySwitch && HAL::ActivateHighPerformanceScheme();
    const int ms = static_cast<int>(now() - t0);
    if (ok) {
        emit({FlowEvent::StepOk, "电源方案（高性能）", 1, 1, ms});
    } else {
        emit({FlowEvent::StepFail, "电源方案（高性能）", 1, 1, ms});
        emit({FlowEvent::Info, "  " + std::string(trySwitch ? ("切换失败（" + HAL::LastErrorText() + "，可能需管理员权限）")
                                                           : "未启用电源方案切换（需勾选/--power）"), 0, 0, 0});
    }
    return os.str();
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

std::vector<std::pair<GameId, uint32_t>> AppCore::RunningGames() const {
    std::vector<std::pair<GameId, uint32_t>> out;
    for (const GameId id : kAllGames) {
        const uint32_t pid = FindProcessByExeName(GameExeName(id));
        if (pid != 0) out.emplace_back(id, pid);
    }
    return out;
}

std::string AppCore::OptimizeAll(const FlowCallback& cb, int stepDelayMs) {
    const auto running = RunningGames();
    if (running.empty()) {
        if (cb) cb({FlowEvent::Info, "（未检测到运行中的支持游戏，执行系统级一键优化）", 0, 0, 0});
        return OptimizeAuto(cb, stepDelayMs);
    }
    if (cb) {
        cb({FlowEvent::Info, "（并发处理：检测到 " + std::to_string(running.size())
                             + " 个运行中的支持游戏，逐一优化）", 0, 0, 0});
    }
    std::string all;
    for (const auto& [id, pid] : running) {
        (void)pid;
        all += OptimizeForGame(id, cb, stepDelayMs);
        all += "\n";
    }
    return all;
}

std::string AppCore::TuneSystem(bool highPerf) {
    return SystemTuner::Tune(cachedProfile_, highPerf);
}

std::string AppCore::RestoreTune() {
    return SystemTuner::Restore();
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
