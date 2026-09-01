#pragma once

// 统一硬件操作接口（HAL）。
// 设计约束（与用户确认）：
//   - 仅官方用户态 API，禁止内核 Hook 与任何形式的进程注入
//   - 优先级上限 HIGH_PRIORITY_CLASS，REALTIME 一律拒绝
//   - 电源方案切换需要管理员权限，由 AppCore 决定是否启用（默认关闭）
//   - 驱动级帧延迟（NVAPI/ADL）动态加载，不支持时降级跳过

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

#include <cstdint>
#include <string>

#include "hardware/HardwareProfile.h"

namespace gopt {

class HAL {
public:
    // ---------------- 进程优先级 ----------------
    // priorityClass 合法值：IDLE / BELOW_NORMAL / NORMAL / ABOVE_NORMAL / HIGH
    // REALTIME_PRIORITY_CLASS 被安全红线拒绝。
    static bool SetProcessPriority(HANDLE hProcess, DWORD priorityClass);
    static bool SetThreadPriority(HANDLE hThread, int priority);

    // ---------------- CPU 亲和性 ----------------
    // mask 为 group 内的逻辑处理器位掩码。当前版本仅支持 group 0（≤64 逻辑核），
    // 超出时返回 false 并记录原因（调用方按"降级跳过"处理）。
    static bool SetProcessAffinity(HANDLE hProcess, uint32_t group, uint64_t mask);

    // ---------------- 工作集策略（替代原"内存池预分配"） ----------------
    // 对游戏进程设置工作集上下限（字节 / MB）。要求 0 <= minBytes <= maxBytes。
    static bool SetProcessWorkingSet(HANDLE hProcess, uint64_t minBytes, uint64_t maxBytes);
    static bool SetProcessWorkingSetMB(HANDLE hProcess, uint64_t minMB, uint64_t maxMB);

    // ---------------- 电源方案（切换需管理员权限） ----------------
    static bool QueryActivePowerScheme(GUID* outScheme);  // 只读，安全
    static bool ActivateHighPerformanceScheme();          // 按名称查找"高性能"并激活
    static bool ActivatePowerScheme(const GUID& scheme);  // 显式激活（自动记住切换前方案）
    static bool RestorePowerScheme();                     // 恢复最近一次切换前
    static std::string PowerSchemeName(const GUID& scheme);  // 可读名称（日志用）

    // ---------------- 代启动游戏（工具代启动模式） ----------------
    // CREATE_SUSPENDED 启动 → 应用优先级/亲和性/工作集 → ResumeThread。
    // 任一设置失败不会中止启动（降级哲学），失败原因收集在 result.error。
    struct LaunchResult {
        bool ok = false;
        HANDLE hProcess = nullptr;  // 调用方负责 CloseHandle
        HANDLE hThread = nullptr;   // 调用方负责 CloseHandle
        DWORD pid = 0;
        std::string error;          // 非致命警告 / 失败原因
    };
    // priorityClass==0 表示不设置优先级；affinityMask==0 表示不设置亲和性。
    static LaunchResult LaunchGameSuspended(const std::wstring& exePath,
                                            const std::wstring& args,
                                            DWORD priorityClass,
                                            uint32_t affinityGroup, uint64_t affinityMask,
                                            uint64_t workingSetMinMB, uint64_t workingSetMaxMB);

    // ---------------- 驱动级帧延迟（NVAPI DRS / ADL，动态加载） ----------------
    static bool IsDriverFrameLatencySupported(const HardwareProfile& p);
    // 按可执行文件名配置驱动级最大预渲染帧数。当前版本未集成厂商 SDK 时返回 false
    // （调用方按"降级跳过"处理），接口保留供后续引入 NVAPI/ADL 后启用。
    static bool SetDriverFrameLatency(const std::wstring& gameExeName, uint32_t maxFrames);

    // ---------------- 诊断 ----------------
    static std::string LastErrorText();
    static void ClearLastError();

    // ---------------- 权限检测 ----------------
    // 当前进程是否以管理员权限（已提权）运行；未提权时电源/调优/注册表项会失败
    static bool IsElevated();

private:
    static std::string& LastErrorStorage();
};

}  // namespace gopt
