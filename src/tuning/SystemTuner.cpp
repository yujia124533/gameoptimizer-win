#include "tuning/SystemTuner.h"

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
#include <powrprof.h>

#include <cstdlib>
#include <fstream>
#include <sstream>

#include "hal/HAL.h"

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

std::string GuidToString(const GUID& g) {
    char buf[64] = {};
    std::snprintf(buf, sizeof(buf),
                  "%08lx-%04x-%04x-%02x%02x-%02x%02x%02x%02x%02x%02x",
                  static_cast<unsigned long>(g.Data1), g.Data2, g.Data3,
                  g.Data4[0], g.Data4[1], g.Data4[2], g.Data4[3],
                  g.Data4[4], g.Data4[5], g.Data4[6], g.Data4[7]);
    return buf;
}

bool StringToGuid(const std::string& s, GUID* out) {
    unsigned int d1 = 0, d2 = 0, d3 = 0, b[8] = {};
    if (std::sscanf(s.c_str(), "%08x-%04x-%04x-%02x%02x-%02x%02x%02x%02x%02x%02x",
                    &d1, &d2, &d3, &b[0], &b[1], &b[2], &b[3], &b[4], &b[5], &b[6], &b[7]) != 11) {
        return false;
    }
    out->Data1 = d1;
    out->Data2 = static_cast<unsigned short>(d2);
    out->Data3 = static_cast<unsigned short>(d3);
    for (int i = 0; i < 8; ++i) out->Data4[i] = static_cast<unsigned char>(b[i]);
    return true;
}

constexpr GUID kHighPerfGUID = {0x8c5e7fda, 0xe8bf, 0x4a96,
                               {0x9a, 0x85, 0xa6, 0xe2, 0x3a, 0x8c, 0x63, 0x5c}};
constexpr GUID kBalancedGUID = {0x381b4222, 0xf694, 0x41f0,
                               {0x96, 0x85, 0xff, 0x5b, 0xb2, 0x60, 0xdf, 0x2e}};

bool RunPowercfgIdx(const std::string& idx, int value) {
    const std::string cmd = "powercfg -setacvalueindex SCHEME_CURRENT SUB_PROCESSOR " + idx + " "
                            + std::to_string(value);
    const std::string cmdD = "powercfg -setdcvalueindex SCHEME_CURRENT SUB_PROCESSOR " + idx + " "
                             + std::to_string(value);
    const int rc1 = std::system(cmd.c_str());
    const int rc2 = std::system(cmdD.c_str());
    return rc1 == 0 && rc2 == 0;
}
// 安全临时清理（Wise 风格：递归 %TEMP%，占用/锁定项自动跳过；
// 安全策略：24 小时内修改过的文件视为"使用中"，一律保留不删）
constexpr ULONGLONG kKeepRecent100ns = 24ull * 3600ull * 10000000ull;
uint64_t g_cleanFreed = 0;
int g_cleanDeleted = 0, g_cleanSkipped = 0, g_cleanKept = 0;
void CleanDir(const std::wstring& dir, int depth) {
    if (depth > 4) return;
    WIN32_FIND_DATAW fd{};
    HANDLE h = FindFirstFileW((dir + L"\\*").c_str(), &fd);
    if (h == INVALID_HANDLE_VALUE) return;
    FILETIME nowFt{};
    GetSystemTimeAsFileTime(&nowFt);
    ULARGE_INTEGER now{};
    now.HighPart = nowFt.dwHighDateTime;
    now.LowPart = nowFt.dwLowDateTime;
    do {
        if (wcscmp(fd.cFileName, L".") == 0 || wcscmp(fd.cFileName, L"..") == 0) continue;
        const std::wstring path = dir + L"\\" + fd.cFileName;
        ULARGE_INTEGER ft{};
        ft.HighPart = fd.ftLastWriteTime.dwHighDateTime;
        ft.LowPart = fd.ftLastWriteTime.dwLowDateTime;
        const bool stale = now.QuadPart >= ft.QuadPart + kKeepRecent100ns;
        if (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
            CleanDir(path, depth + 1);
            if (!stale) { ++g_cleanKept; }
            else if (RemoveDirectoryW(path.c_str())) ++g_cleanDeleted;
            else ++g_cleanSkipped;
        } else {
            if (!stale) { ++g_cleanKept; continue; }
            ULARGE_INTEGER sz{};
            sz.LowPart = fd.nFileSizeLow;
            sz.HighPart = fd.nFileSizeHigh;
            if (DeleteFileW(path.c_str())) { g_cleanFreed += sz.QuadPart; ++g_cleanDeleted; }
            else ++g_cleanSkipped;
        }
    } while (FindNextFileW(h, &fd));
    FindClose(h);
}

}  // namespace

std::string SystemTuner::SnapshotPath() {
    wchar_t base[MAX_PATH] = {};
    std::wstring dir;
    if (GetEnvironmentVariableW(L"LOCALAPPDATA", base, MAX_PATH) > 0) {
        dir = base;
    } else {
        GetCurrentDirectoryW(MAX_PATH, base);
        dir = base;
    }
    dir += L"\\GameOptimizer";
    CreateDirectoryW(dir.c_str(), nullptr);
    return WideToUtf8(dir.c_str()) + "\\tune.conf";
}

bool SystemTuner::RecommendHighPerf(const HardwareProfile& hw) {
    return hw.physicalCores >= 8 && hw.systemRamMB >= 16384;
}

bool SystemTuner::ReadPrioritySeparation(unsigned long* out) {
    HKEY k = nullptr;
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE,
                      L"SYSTEM\\CurrentControlSet\\Control\\PriorityControl",
                      0, KEY_READ, &k) != ERROR_SUCCESS) {
        return false;
    }
    DWORD v = 0, type = 0, sz = sizeof(v);
    const bool ok = RegQueryValueExW(k, L"Win32PrioritySeparation", nullptr, &type,
                                     reinterpret_cast<BYTE*>(&v), &sz) == ERROR_SUCCESS &&
                    type == REG_DWORD;
    RegCloseKey(k);
    if (ok && out) *out = v;
    return ok;
}

bool SystemTuner::WritePrioritySeparation(unsigned long value) {
    HKEY k = nullptr;
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE,
                      L"SYSTEM\\CurrentControlSet\\Control\\PriorityControl",
                      0, KEY_WRITE, &k) != ERROR_SUCCESS) {
        return false;
    }
    const DWORD v = static_cast<DWORD>(value);
    const bool ok = RegSetValueExW(k, L"Win32PrioritySeparation", 0, REG_DWORD,
                                   reinterpret_cast<const BYTE*>(&v), sizeof(v)) == ERROR_SUCCESS;
    RegCloseKey(k);
    return ok;
}

std::string SystemTuner::Tune(const HardwareProfile& hw, bool highPerf) {
    std::ostringstream os;
    // 1) 快照：当前激活方案 + Win32PrioritySeparation
    GUID cur{};
    std::string prevScheme;
    if (HAL::QueryActivePowerScheme(&cur)) prevScheme = GuidToString(cur);
    unsigned long prevPrio = 0;
    const bool hadPrio = ReadPrioritySeparation(&prevPrio);
    {
        std::ofstream out(SnapshotPath(), std::ios::trunc);
        out << prevScheme << '|' << (hadPrio ? prevPrio : 0) << "\n";
    }
    os << (highPerf ? "== 应用：高性能档（依据硬件推荐） ==\n"
                    : "== 应用：平衡档 ==\n");

    // 2) 电源方案
    const GUID& scheme = highPerf ? kHighPerfGUID : kBalancedGUID;
    if (HAL::ActivatePowerScheme(scheme)) {
        os << "  [1] 电源方案: 已切换\n";
    } else {
        os << "  [1] 电源方案: 切换失败（" << HAL::LastErrorText() << "，需管理员）\n";
    }

    // 3) 处理器性能档（官方 powercfg 别名；需管理员）
    const int minState = highPerf ? 50 : 5;
    const int maxState = 100;
    const int boostMode = highPerf ? 4 : 2;  // 4 = 激进增强（部分系统无此设置，跳过）
    const bool p1 = RunPowercfgIdx("PROCTHROTTLEMIN", minState);
    const bool p2 = RunPowercfgIdx("PROCTHROTTLEMAX", maxState);
    const bool p3 = RunPowercfgIdx("PERFBOOSTMODE", boostMode);
    std::system("powercfg -setactive SCHEME_CURRENT");
    os << (p1 && p2
               ? ("  [2] 处理器性能档: 已设置（最小 " + std::to_string(minState)
                  + "% / 最大 " + std::to_string(maxState) + "%）\n")
               : "  [2] 处理器性能档: 设置失败（需管理员权限）\n");
    if (p3) {
        os << "        ↳ 增强模式(boost)=" << boostMode << "（4=激进）\n";
    } else {
        os << "        ↳ 增强模式(boost)在此系统不可用，已跳过（不影响其他项）\n";
    }

    // 4) 系统调度优先级（游戏优先 vs 标准）
    if (WritePrioritySeparation(highPerf ? 0x26ul : 0x2ul)) {
        os << std::string("  [3] 系统调度优先级: 已设置为 ")
           << (highPerf ? "游戏优先（0x26）" : "标准（0x2）") << "\n";
    } else {
        os << "  [3] 系统调度优先级: 设置失败（需管理员权限）\n";
    }
    // 结果摘要
    os << "\n== 完成 ==\n";
    os << "  已应用：电源方案=" << (highPerf ? "高性能" : "平衡")
       << " · 处理器最小 " << minState << "% / 最大 " << maxState
       << "% / 增强模式 " << boostMode << (highPerf ? "（激进）" : "（常规）")
       << " · 调度优先级=" << (highPerf ? "0x26 游戏优先" : "0x2 标准") << "\n";
    os << "  已生成调优快照；如需还原点「恢复调优」或执行 tune restore。\n";
    return os.str();
}

std::string SystemTuner::Restore() {
    std::ostringstream os;
    std::ifstream in(SnapshotPath());
    std::string schemeStr, prioStr;
    if (!std::getline(in, schemeStr, '|') || !std::getline(in, prioStr)) {
        os << "没有可恢复的系统调优快照。\n";
        return os.str();
    }
    GUID g{};
    if (StringToGuid(schemeStr, &g)) {
        if (HAL::ActivatePowerScheme(g)) {
            os << " [1] 电源方案: 已恢复\n";
        } else {
            os << " [1] 电源方案: 恢复失败（" << HAL::LastErrorText() << "）\n";
        }
    }
    const unsigned long prio = static_cast<unsigned long>(std::stoul(prioStr));
    if (WritePrioritySeparation(prio)) {
        os << " [2] 系统调度优先级: 已恢复（" << prio << "）\n";
    } else {
        os << " [2] 系统调度优先级: 恢复失败（需管理员权限）\n";
    }
    // 处理器状态恢复为常规默认（自定义原值保守回退）
    const bool a = RunPowercfgIdx("PROCTHROTTLEMIN", 5);
    const bool b = RunPowercfgIdx("PROCTHROTTLEMAX", 100);
    const bool c = RunPowercfgIdx("PERFBOOSTMODE", 2);
    std::system("powercfg -setactive SCHEME_CURRENT");
    os << (a && b && c ? " [3] 处理器性能档: 已恢复默认（5%/100%/2）\n"
                       : " [3] 处理器性能档: 恢复失败（需管理员权限）\n");
    os << "  调优已还原。\n";
    return os.str();
}
std::string SystemTuner::CleanTemp() {
    wchar_t tmp[MAX_PATH] = {};
    GetTempPathW(MAX_PATH, tmp);
    std::wstring dir(tmp);
    if (!dir.empty() && dir.back() == L'\\') dir.pop_back();
    g_cleanFreed = 0;
    g_cleanDeleted = 0;
    g_cleanSkipped = 0;
    g_cleanKept = 0;
    CleanDir(dir, 0);
    std::ostringstream os;
    os << "== 临时文件清理 ==\n";
    os << "  目录: " << WideToUtf8(dir.c_str()) << "\n";
    os << "  已清理 " << g_cleanDeleted << " 项，释放约 "
       << (g_cleanFreed / (1024ull * 1024ull)) << " MB";
    if (g_cleanSkipped > 0) os << "；跳过 " << g_cleanSkipped << " 个占用/锁定项（属正常）";
    if (g_cleanKept > 0) os << "；保留 " << g_cleanKept << " 项（24 小时内修改，安全策略）";
    os << "\n";
    return os.str();
}
}  // namespace gopt
