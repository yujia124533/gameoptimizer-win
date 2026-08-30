#include "hal/HAL.h"

#include <powrprof.h>

#include <cstdio>
#include <cstdlib>
#include <optional>
#include <vector>

namespace gopt {

namespace {

std::string WideToUtf8(const wchar_t* wstr) {
    if (!wstr || !*wstr) return {};
    const int len = WideCharToMultiByte(CP_UTF8, 0, wstr, -1, nullptr, 0, nullptr, nullptr);
    if (len <= 1) return {};
    std::string s(static_cast<size_t>(len) - 1, '\0');
    WideCharToMultiByte(CP_UTF8, 0, wstr, -1, &s[0], len, nullptr, nullptr);
    return s;
}

std::string WinErrorText(DWORD code) {
    char buf[256] = {};
    std::snprintf(buf, sizeof(buf), "WinErr=%lu", static_cast<unsigned long>(code));
    return buf;
}

// 高性能电源方案的常见 GUID（英文系统；优先按名称枚举匹配）
constexpr GUID kHighPerformanceGUID = {0x8c5e7fda, 0xe8bf, 0x4a96,
                                       {0x9a, 0x85, 0xa6, 0xe2, 0x3a, 0x8c, 0x63, 0x5c}};

bool IsValidPriorityClass(DWORD priorityClass) {
    switch (priorityClass) {
        case IDLE_PRIORITY_CLASS:
        case BELOW_NORMAL_PRIORITY_CLASS:
        case NORMAL_PRIORITY_CLASS:
        case ABOVE_NORMAL_PRIORITY_CLASS:
        case HIGH_PRIORITY_CLASS:
            return true;
        default:
            return false;  // 含 REALTIME_PRIORITY_CLASS —— 安全红线，一律拒绝
    }
}

std::wstring PowerSchemeFriendlyName(const GUID& scheme) {
    // NO_SUBGROUP_GUID / NO_POWER_SETTING_GUID 在部分工具链（如 mingw）未导出，二者实为 GUID_NULL
    static const GUID kNoSubgroup = {0, 0, 0, {0, 0, 0, 0, 0, 0, 0, 0}};
    static const GUID kNoSetting = {0, 0, 0, {0, 0, 0, 0, 0, 0, 0, 0}};
    DWORD size = 0;
    if (PowerReadFriendlyName(nullptr, &scheme, &kNoSubgroup, &kNoSetting,
                              nullptr, &size) != ERROR_SUCCESS || size == 0) {
        return {};
    }
    std::vector<wchar_t> buf(size / sizeof(wchar_t) + 1, 0);
    if (PowerReadFriendlyName(nullptr, &scheme, &kNoSubgroup, &kNoSetting,
                              reinterpret_cast<BYTE*>(buf.data()), &size) != ERROR_SUCCESS) {
        return {};
    }
    return buf.data();
}

// 按友好名称查找电源方案（处理本地化名称）
bool FindSchemeByName(const wchar_t* nameA, const wchar_t* nameB, GUID* out) {
    GUID scheme{};
    DWORD index = 0;
    DWORD bufSize = sizeof(scheme);
    while (PowerEnumerate(nullptr, nullptr, nullptr, ACCESS_SCHEME, index,
                          reinterpret_cast<BYTE*>(&scheme), &bufSize) == ERROR_SUCCESS) {
        const std::wstring name = PowerSchemeFriendlyName(scheme);
        if (!name.empty() && (name == nameA || (nameB && name == nameB))) {
            *out = scheme;
            return true;
        }
        ++index;
        bufSize = sizeof(scheme);
    }
    return false;
}

}  // namespace

// ---------------- 诊断 ----------------

std::string& HAL::LastErrorStorage() {
    static std::string s;
    return s;
}

std::string HAL::LastErrorText() {
    return LastErrorStorage();
}

void HAL::ClearLastError() {
    LastErrorStorage().clear();
}

// ---------------- 进程优先级 ----------------

bool HAL::SetProcessPriority(HANDLE hProcess, DWORD priorityClass) {
    ClearLastError();
    if (!IsValidPriorityClass(priorityClass)) {
        LastErrorStorage() = "非法优先级类（REALTIME 被安全红线拒绝）";
        return false;
    }
    if (!SetPriorityClass(hProcess, priorityClass)) {
        LastErrorStorage() = "SetPriorityClass 失败: " + WinErrorText(GetLastError());
        return false;
    }
    return true;
}

bool HAL::SetThreadPriority(HANDLE hThread, int priority) {
    ClearLastError();
    if (priority < THREAD_PRIORITY_IDLE || priority > THREAD_PRIORITY_TIME_CRITICAL) {
        LastErrorStorage() = "非法线程优先级";
        return false;
    }
    if (!SetThreadPriority(hThread, priority)) {
        LastErrorStorage() = "SetThreadPriority 失败: " + WinErrorText(GetLastError());
        return false;
    }
    return true;
}

// ---------------- CPU 亲和性 ----------------

bool HAL::SetProcessAffinity(HANDLE hProcess, uint32_t group, uint64_t mask) {
    ClearLastError();
    if (mask == 0) {
        LastErrorStorage() = "亲和性掩码为 0";
        return false;
    }
    if (group != 0) {
        // 组感知亲和性（>64 逻辑核）需要 SetThreadGroupAffinity 系列；
        // 当前版本按设计降级跳过，不产生副作用。
        LastErrorStorage() = "当前版本仅支持组 0（逻辑处理器<=64），亲和性降级跳过";
        return false;
    }
    if (!SetProcessAffinityMask(hProcess, static_cast<DWORD_PTR>(mask))) {
        LastErrorStorage() = "SetProcessAffinityMask 失败: " + WinErrorText(GetLastError());
        return false;
    }
    return true;
}

// ---------------- 工作集策略 ----------------

bool HAL::SetProcessWorkingSet(HANDLE hProcess, uint64_t minBytes, uint64_t maxBytes) {
    ClearLastError();
    // 语义：maxBytes==0 且 minBytes>0 → "仅设下限、不设上限"，用一个超大值作为上限
    if (minBytes > 0 && maxBytes == 0) {
        maxBytes = 8ull * 1024 * 1024 * 1024 * 1024;  // 8TB，视作无上限
    }
    if (minBytes > maxBytes) {
        LastErrorStorage() = "工作集参数非法（min > max）";
        return false;
    }
    if (!SetProcessWorkingSetSize(hProcess, static_cast<SIZE_T>(minBytes),
                                  static_cast<SIZE_T>(maxBytes))) {
        LastErrorStorage() = "SetProcessWorkingSetSize 失败: " + WinErrorText(GetLastError());
        return false;
    }
    return true;
}

bool HAL::SetProcessWorkingSetMB(HANDLE hProcess, uint64_t minMB, uint64_t maxMB) {
    return SetProcessWorkingSet(hProcess, minMB * 1024ull * 1024ull,
                                maxMB * 1024ull * 1024ull);
}

// ---------------- 电源方案 ----------------

bool HAL::QueryActivePowerScheme(GUID* outScheme) {
    ClearLastError();
    GUID* scheme = nullptr;
    if (PowerGetActiveScheme(nullptr, &scheme) != ERROR_SUCCESS || scheme == nullptr) {
        LastErrorStorage() = "PowerGetActiveScheme 失败";
        return false;
    }
    if (outScheme != nullptr) *outScheme = *scheme;
    LocalFree(scheme);
    return true;
}

bool HAL::ActivatePowerScheme(const GUID& scheme) {
    ClearLastError();
    // 记住切换前的方案，供 RestorePowerScheme 使用
    static std::optional<GUID> previous;
    GUID* current = nullptr;
    if (PowerGetActiveScheme(nullptr, &current) == ERROR_SUCCESS && current != nullptr) {
        previous = *current;
        LocalFree(current);
    }
    if (PowerSetActiveScheme(nullptr, &scheme) != ERROR_SUCCESS) {
        LastErrorStorage() = "PowerSetActiveScheme 失败（可能需要管理员权限）: " +
                             WinErrorText(GetLastError());
        return false;
    }
    return true;
}

bool HAL::ActivateHighPerformanceScheme() {
    ClearLastError();
    GUID scheme{};
    if (FindSchemeByName(L"High performance", L"高性能", &scheme)) {
        return ActivatePowerScheme(scheme);
    }
    // 名称匹配失败（自定义方案名等）时退化为已知 GUID
    return ActivatePowerScheme(kHighPerformanceGUID);
}

bool HAL::RestorePowerScheme() {
    static std::optional<GUID> previous;
    ClearLastError();
    // 从最近一次 ActivatePowerScheme 读取（用函数级静态保持状态）
    if (!previous.has_value()) {
        LastErrorStorage() = "没有可恢复的电源方案";
        return false;
    }
    const GUID target = *previous;
    previous.reset();
    return ActivatePowerScheme(target);
}

std::string HAL::PowerSchemeName(const GUID& scheme) {
    const std::wstring name = PowerSchemeFriendlyName(scheme);
    return name.empty() ? "<unknown>" : WideToUtf8(name.c_str());
}

// ---------------- 代启动游戏 ----------------

HAL::LaunchResult HAL::LaunchGameSuspended(const std::wstring& exePath,
                                           const std::wstring& args,
                                           DWORD priorityClass,
                                           uint32_t affinityGroup, uint64_t affinityMask,
                                           uint64_t workingSetMinMB, uint64_t workingSetMaxMB) {
    ClearLastError();
    LaunchResult result;

    std::wstring commandLine = L"\"" + exePath + L"\"";
    if (!args.empty()) commandLine += L" " + args;

    STARTUPINFOW si{};
    si.cb = sizeof(si);
    PROCESS_INFORMATION pi{};

    if (!CreateProcessW(exePath.c_str(), &commandLine[0], nullptr, nullptr, FALSE,
                        CREATE_SUSPENDED, nullptr, nullptr, &si, &pi)) {
        LastErrorStorage() = "CreateProcessW 失败: " + WinErrorText(GetLastError());
        return result;
    }

    result.hProcess = pi.hProcess;
    result.hThread = pi.hThread;
    result.pid = pi.dwProcessId;

    // 逐个应用设置；任一失败不中止启动（降级哲学），原因收集进 error
    if (priorityClass != 0) {
        if (!SetProcessPriority(pi.hProcess, priorityClass)) {
            result.error = LastErrorText();
        }
    }
    if (affinityMask != 0) {
        if (!SetProcessAffinity(pi.hProcess, affinityGroup, affinityMask)) {
            if (result.error.empty()) result.error = LastErrorText();
        }
    }
    if (workingSetMinMB != 0 || workingSetMaxMB != 0) {
        if (!SetProcessWorkingSetMB(pi.hProcess, workingSetMinMB, workingSetMaxMB)) {
            if (result.error.empty()) result.error = LastErrorText();
        }
    }

    if (ResumeThread(pi.hThread) == static_cast<DWORD>(-1)) {
        if (result.error.empty()) result.error = "ResumeThread 失败";
    }

    result.ok = true;
    return result;
}

// ---------------- 驱动级帧延迟（NVAPI / ADL） ----------------

bool HAL::IsDriverFrameLatencySupported(const HardwareProfile& p) {
    if (!p.gpuIsHardware) return false;
    // 驱动随安装提供厂商库；动态探测，避免硬依赖
    if (p.gpuVendorId == 0x10DE) {  // NVIDIA
        HMODULE h = LoadLibraryW(L"nvapi64.dll");
        if (h != nullptr) {
            FreeLibrary(h);
            return true;
        }
        return false;
    }
    if (p.gpuVendorId == 0x1002) {  // AMD
        HMODULE h = LoadLibraryW(L"adl64.dll");
        if (h != nullptr) {
            FreeLibrary(h);
            return true;
        }
        return false;
    }
    return false;
}

bool HAL::SetDriverFrameLatency(const std::wstring& gameExeName, uint32_t maxFrames) {
    ClearLastError();
    // TODO(商业化版本)：引入 NVIDIA NVAPI SDK（DRS MaxFramesAllowed）与 AMD ADL
    // 应用配置接口后在此实现。当前版本按设计降级跳过。
    (void)gameExeName;
    (void)maxFrames;
    LastErrorStorage() = "驱动级帧延迟未启用（需集成 NVAPI/ADL SDK；当前自动降级跳过）";
    return false;
}

}  // namespace gopt
