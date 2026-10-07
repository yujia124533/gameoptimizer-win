#include "rollback/SecurityRollback.h"

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

#include <chrono>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <fstream>
#include <sstream>

#include "hal/HAL.h"
#include "i18n.h"

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
    if (std::sscanf(s.c_str(),
                    "%08x-%04x-%04x-%02x%02x-%02x%02x%02x%02x%02x%02x",
                    &d1, &d2, &d3, &b[0], &b[1], &b[2], &b[3],
                    &b[4], &b[5], &b[6], &b[7]) != 11) {
        return false;
    }
    out->Data1 = d1;
    out->Data2 = static_cast<unsigned short>(d2);
    out->Data3 = static_cast<unsigned short>(d3);
    for (int i = 0; i < 8; ++i) out->Data4[i] = static_cast<unsigned char>(b[i]);
    return true;
}

// ---------------- 只读历史展示辅助 ----------------

std::string Hex64(uint64_t v) {
    char buf[32] = {};
    std::snprintf(buf, sizeof(buf), "0x%016llx", static_cast<unsigned long long>(v));
    return buf;
}

// 优先级类可读名（与 HAL 的合法范围一致）
std::string PriorityText(uint32_t pc) {
    switch (pc) {
        case IDLE_PRIORITY_CLASS:         return T("空闲", "Idle");
        case BELOW_NORMAL_PRIORITY_CLASS: return T("低于正常", "BelowNormal");
        case NORMAL_PRIORITY_CLASS:       return T("正常", "Normal");
        case ABOVE_NORMAL_PRIORITY_CLASS: return T("高于正常", "AboveNormal");
        case HIGH_PRIORITY_CLASS:         return T("高", "High");
        case REALTIME_PRIORITY_CLASS:     return T("实时（已被安全红线拒绝）", "Realtime (rejected)");
        default:                          return T("未知", "unknown");
    }
}

// ---------------- 快照时间戳 → 可读本地时间 ----------------
//
// 历史条目里存的是写入时的 NowMs()（steady_clock 毫秒）。steady_clock 的原点随
// 实现而变：MinGW libstdc++ 下与 Unix 纪元同源（本仓库实测 sys==steady），
// MSVC 下则是 QPC（自开机计时）。同一个存档可能被不同工具链构建的版本先后写入，
// 因此这里不写死假设，而是运行时判定原点：
//   |system - steady| < 48h  → 判为「与墙钟同源」，直接用时间戳；
//   否则取开机偏移 origin = system - steady，按「自开机计时」换算。
// 换算结果若落在未来（未来 6h 之外）或 2000 年之前，说明该条目由另一次开机/
// 另一种时钟原点的版本写入，时间无法可靠换算——此时返回说明文本，不编造时间。
// 只读：本函数只读时钟，不触碰文件与状态。
constexpr int64_t kEpochOriginMaxGapMs = 48LL * 3600 * 1000;  // 判定 steady 原点是否同源于墙钟
constexpr int64_t kYear2000Ms = 946684800000LL;
constexpr int64_t kFutureSkewToleranceMs = 24LL * 3600 * 1000;  // 允许的时钟回拨/NTP 偏差

std::string LocalTimeText(std::time_t t) {
#if defined(_MSC_VER) || defined(__MINGW32__)
    // MSVC 与 MinGW-W64（本仓库工具链，UCRT 变体）都提供 localtime_s，
    // 且都是 MS 参数序 (std::tm*, const std::time_t*) 与 errno_t 返回。
    // 注意：MinGW 并未定义 __STDC_LIB_EXT1__，因此这里只能按编译器宏区分，
    // 不能按 C11 Annex K 宏判定；localtime_r 在本工具链上不可用。
    std::tm tmv{};
    if (localtime_s(&tmv, &t) != 0) return {};
#else
    // 其他实现：退回标准 localtime（返回静态缓冲区，需立即复制）。
    std::tm* tmp = std::localtime(&t);
    if (tmp == nullptr) return {};
    const std::tm tmv = *tmp;
#endif
    char buf[32] = {};
    std::snprintf(buf, sizeof(buf), "%04d-%02d-%02d %02d:%02d:%02d",
                  tmv.tm_year + 1900, tmv.tm_mon + 1, tmv.tm_mday,
                  tmv.tm_hour, tmv.tm_min, tmv.tm_sec);
    return buf;
}

std::string FormatSnapshotTime(int64_t timestampMs) {
    if (timestampMs <= 0) return T("未知", "unknown");
    const int64_t sysMs = std::chrono::duration_cast<std::chrono::milliseconds>(
                              std::chrono::system_clock::now().time_since_epoch())
                              .count();
    const int64_t steadyMs = std::chrono::duration_cast<std::chrono::milliseconds>(
                                 std::chrono::steady_clock::now().time_since_epoch())
                                 .count();
    const int64_t gap = sysMs - steadyMs;
    const int64_t wallMs = (gap > -kEpochOriginMaxGapMs && gap < kEpochOriginMaxGapMs)
                               ? timestampMs          // steady 与墙钟同源
                               : timestampMs + gap;   // steady 自开机计时 → 换算到墙钟
    if (wallMs < kYear2000Ms || wallMs > sysMs + kFutureSkewToleranceMs) {
        return T("较早一次运行（时间未知）", "earlier session (time unknown)");
    }
    const std::string text = LocalTimeText(static_cast<std::time_t>(wallMs / 1000));
    return text.empty() ? std::string(T("未知", "unknown")) : text;
}

}  // namespace

// ---------------- 基础 ----------------

SecurityRollback::~SecurityRollback() {
    StopWatchdog();
}

void SecurityRollback::SetError(const std::string& msg) const {
    lastError_ = msg;
}

std::string SecurityRollback::LastErrorText() const {
    return lastError_;
}

size_t SecurityRollback::SavePointCount() const {
    return stack_.size();
}

int64_t SecurityRollback::NowMs() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(
               std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

// ---------------- 快照持久化（跨进程回滚） ----------------

// 快照目录：%LOCALAPPDATA%\GameOptimizer（取不到时退回当前目录）。不求值创建任何东西。
std::wstring SecurityRollback::SaveFileDirW() {
    wchar_t base[MAX_PATH] = {};
    std::wstring dir;
    if (GetEnvironmentVariableW(L"LOCALAPPDATA", base, MAX_PATH) > 0) {
        dir = base;
    } else {
        GetCurrentDirectoryW(MAX_PATH, base);
        dir = base;
    }
    dir += L"\\GameOptimizer";
    return dir;
}

std::string SecurityRollback::SaveFilePath() {
    const std::wstring dir = SaveFileDirW();
    CreateDirectoryW(dir.c_str(), nullptr);
    return WideToUtf8(dir.c_str()) + "\\savepoints.txt";
}

// 只读路径：与 SaveFilePath() 完全同址，但不创建目录（查询路径零文件系统副作用）
std::string SecurityRollback::SaveFilePathNoCreate() {
    return WideToUtf8(SaveFileDirW().c_str()) + "\\savepoints.txt";
}

std::string SecurityRollback::Serialize(const SavePoint& sp) {
    std::ostringstream os;
    os << sp.processId << '|'
       << sp.priorityClass << '|'
       << sp.affinityMask << '|'
       << sp.workingSetMin << '|'
       << sp.workingSetMax << '|'
       << (sp.hasProcessState ? 1 : 0) << '|'
       << (sp.hasWorkingSet ? 1 : 0) << '|'
       << (sp.hasPowerScheme ? 1 : 0) << '|'
       << sp.powerSchemeGuid << '|'
       << sp.timestampMs << '|'
       << sp.gameName << '|'
       << sp.description;
    return os.str();
}

// 解析实现：合法行行为与 v1.0.19 完全一致；数字字段损坏时返回 false，不抛异常。
// 相对 v1.0.19 增加两类「格式异常」判负（原先会被 std::stoul/stoull/stoll 静默回绕）：
//   * 负数：如 "-5" 会被 stoul 回绕成 0xFFFFFFFB，回滚时变成不可用的垃圾值；
//   * 超出字段范围：如 5000 位数字串会被 stoull 回绕成随机值（stoull 规定返回 ULLONG_MAX）。
// 两类都只可能出现在被外部篡改/损坏的存档里（本仓库写入路径只会产出非负且范围内的值），
// 因此判负不影响向后兼容，只是让「损坏文件 -> 空列表 + 可读原因」更彻底。
namespace {

bool ParseField(const std::string& s, unsigned long long maxValue, bool allowNegative,
                long long* out) {
    if (s.empty()) return false;
    const bool negative = (s[0] == '-');
    if (negative && !allowNegative) return false;
    if (negative) {
        // 时间戳字段：允许负值，用 stoll 保证完整消费（越界会抛异常）
        try {
            size_t pos = 0;
            const long long v = std::stoll(s, &pos);
            if (pos != s.size()) return false;
            if (static_cast<unsigned long long>(-(v + 1)) + 1ULL > maxValue) return false;
            *out = v;
            return true;
        } catch (...) {
            return false;
        }
    }
    // 非负字段：用 stoull 覆盖完整 uint64 范围（stoll 会拒绝 > LLONG_MAX 的合法值）。
    // 先试十进制（本仓库 Serialize 只写十进制）；若因残留字符失败再试十六进制，
    // 以兼容可能带 0x 前缀的旧存档。注意 stoull 的 base=10 并不接受 0x，必须显式换 base。
    for (const int base : {10, 16}) {
        try {
            size_t pos = 0;
            const unsigned long long v = std::stoull(s, &pos, base);
            if (pos != s.size()) continue;  // 该进制下有残留字符 → 换一种进制
            if (v > maxValue) return false;
            *out = static_cast<long long>(v);
            return true;
        } catch (...) {
            // 该进制下无法解析（非数字 / 越界）→ 换一种进制
        }
    }
    return false;
}

}  // namespace

bool SecurityRollback::TryDeserialize(const std::string& line, SavePoint* sp) {
    if (!sp) return false;
    std::istringstream is(line);
    std::string s;
    const auto next = [&is](std::string& out) {
        return static_cast<bool>(std::getline(is, out, '|'));
    };
    long long v = 0;
    if (!next(s) || !ParseField(s, 0xFFFFFFFFULL, false, &v)) return false;
    sp->processId = static_cast<uint32_t>(v);
    if (!next(s) || !ParseField(s, 0xFFFFFFFFULL, false, &v)) return false;
    sp->priorityClass = static_cast<uint32_t>(v);
    if (!next(s) || !ParseField(s, 0xFFFFFFFFFFFFFFFFULL, false, &v)) return false;
    sp->affinityMask = static_cast<uint64_t>(v);
    if (!next(s) || !ParseField(s, 0xFFFFFFFFFFFFFFFFULL, false, &v)) return false;
    sp->workingSetMin = static_cast<uint64_t>(v);
    if (!next(s) || !ParseField(s, 0xFFFFFFFFFFFFFFFFULL, false, &v)) return false;
    sp->workingSetMax = static_cast<uint64_t>(v);
    if (!next(s)) return false;
    sp->hasProcessState = (s == "1");
    if (!next(s)) return false;
    sp->hasWorkingSet = (s == "1");
    if (!next(s)) return false;
    sp->hasPowerScheme = (s == "1");
    if (!next(s)) return false;
    sp->powerSchemeGuid = s;
    // 时间戳：唯一允许负值的字段（历史版本/异常时钟下可能为负，属于可展示的历史数据）
    if (!next(s)) return false;
    {
        size_t pos = 0;
        try {
            sp->timestampMs = std::stoll(s, &pos);
        } catch (...) {
            return false;
        }
        if (pos != s.size()) return false;
    }
    if (!next(s)) return false;
    sp->gameName = s;
    if (!next(s)) return false;
    sp->description = s;
    return true;
}

bool SecurityRollback::Deserialize(const std::string& line, SavePoint* sp) {
    return TryDeserialize(line, sp);
}

std::vector<SavePoint> SecurityRollback::LoadSavePoints() {
    std::vector<SavePoint> list;
    std::ifstream in(SaveFilePath());
    std::string line;
    while (std::getline(in, line)) {
        if (!line.empty() && line.back() == '\r') line.pop_back();  // 容忍 CRLF
        if (line.empty()) continue;
        SavePoint sp;
        if (TryDeserialize(line, &sp)) list.push_back(sp);
    }
    return list;
}

// 只读解析：不创建目录、不写文件；badLines 记录无法解析的行数（用于格式异常降级）
std::vector<SavePoint> SecurityRollback::LoadSavePointsReadOnly(size_t* badLines) {
    if (badLines) *badLines = 0;
    std::vector<SavePoint> list;
    std::ifstream in(SaveFilePathNoCreate());
    if (!in.is_open()) return list;  // 文件不存在/不可读 → 空列表（原因由调用方判定）
    std::string line;
    while (std::getline(in, line)) {
        if (!line.empty() && line.back() == '\r') line.pop_back();  // 容忍 CRLF
        if (line.empty()) continue;
        SavePoint sp;
        if (TryDeserialize(line, &sp)) {
            list.push_back(sp);
        } else if (badLines) {
            ++*badLines;
        }
    }
    return list;
}

void SecurityRollback::AppendSavePoint(const SavePoint& sp) {
    std::ofstream out(SaveFilePath(), std::ios::app);
    out << Serialize(sp) << "\n";
}

void SecurityRollback::RewriteSavePoints(const std::vector<SavePoint>& list) {
    std::ofstream out(SaveFilePath(), std::ios::trunc);
    for (const auto& sp : list) out << Serialize(sp) << "\n";
}

void SecurityRollback::EnsureLoaded() {
    if (loaded_) return;
    stack_ = LoadSavePoints();
    loaded_ = true;
}

// ---------------- 只读历史查询 ----------------

// 快照 → 展示条目（条件是「回滚时真的会恢复哪些项」，与 RollbackToLastSave 逐项对齐）
std::vector<std::string> SecurityRollback::BuildEntries(const SavePoint& sp) {
    std::vector<std::string> out;
    if (sp.hasProcessState && sp.priorityClass != 0) {
        out.push_back(std::string(T("进程优先级", "Priority")) + ": "
                      + PriorityText(sp.priorityClass) + " (" + Hex64(sp.priorityClass) + ")");
    }
    if (sp.hasProcessState && sp.affinityMask != 0) {
        out.push_back(std::string(T("CPU 亲和性", "CPU affinity")) + ": " + Hex64(sp.affinityMask));
    }
    if (sp.hasWorkingSet) {
        out.push_back(std::string(T("工作集", "Working set")) + ": "
                      + std::to_string(sp.workingSetMin) + " / " + std::to_string(sp.workingSetMax)
                      + " " + T("字节（下限/上限）", "bytes (min/max)"));
    }
    if (sp.hasPowerScheme && !sp.powerSchemeGuid.empty()) {
        out.push_back(std::string(T("电源方案", "Power scheme")) + ": " + sp.powerSchemeGuid);
    }
    return out;
}

SavepointInfo SecurityRollback::MakeInfo(const SavePoint& sp, int index, bool isLatest) {
    SavepointInfo info;
    info.index = index;
    info.timestampMs = sp.timestampMs;
    info.timeText = FormatSnapshotTime(sp.timestampMs);
    info.gameName = sp.gameName;
    info.processId = sp.processId;
    info.processSummary = (sp.gameName.empty() ? std::string(T("未知游戏", "unknown game")) : sp.gameName);
    info.processSummary += (sp.processId != 0)
                               ? (" (PID " + std::to_string(sp.processId) + ")")
                               : std::string(T("（无进程信息）", " (no process info)"));
    info.entries = BuildEntries(sp);
    info.entryCount = static_cast<int>(info.entries.size());
    info.isLatest = isLatest;
    info.raw = sp;
    return info;
}

std::vector<SavepointInfo> SecurityRollback::CollectSavepoints() const {
    SetSavepointError("");
    const std::string path = SaveFilePathNoCreate();
    size_t badLines = 0;
    const std::vector<SavePoint> raw = LoadSavePointsReadOnly(&badLines);

    // 降级一：任一行无法解析 → 视为文件损坏，拒绝展示部分结果
    // （部分解析会误报「可回滚内容」，且后续 RollbackAll 的改写可能丢条目）。
    // 返回空列表 + 可读原因，不抛异常、不崩溃。
    if (badLines > 0) {
        SetSavepointError(
            std::string(T("快照文件格式异常：", "savepoints file is malformed: ")) +
            std::to_string(badLines) +
            T(" 行无法解析，已隐藏全部历史（不影响回滚逻辑）。文件: ",
              " line(s) unparsable; history hidden (rollback logic untouched). File: ") +
            path);
        return {};
    }
    // 降级二：文件不存在（从未优化过 / 目录被清理）→ 空列表 + 原因
    if (raw.empty()) {
        std::ifstream probe(path);
        if (!probe.is_open()) {
            SetSavepointError(
                std::string(T("快照文件不存在（尚未优化过）：", "savepoints file not found (never optimized): ")) +
                path);
        }
        return {};
    }

    std::vector<SavepointInfo> out;
    out.reserve(raw.size());
    for (size_t i = 0; i < raw.size(); ++i) {
        out.push_back(MakeInfo(raw[i], static_cast<int>(i) + 1, i + 1 == raw.size()));
    }
    return out;  // 文件顺序（旧 → 新）；isLatest = 最后一条
}

std::vector<SavepointInfo> SecurityRollback::SavepointList() const {
    return CollectSavepoints();
}

std::vector<SavepointInfo> SecurityRollback::RecentSavepoints(size_t maxCount) const {
    const std::vector<SavepointInfo> all = CollectSavepoints();
    std::vector<SavepointInfo> out;
    const size_t want = (maxCount == 0) ? all.size() : maxCount;
    out.reserve(want < all.size() ? want : all.size());
    for (size_t i = all.size(); i > 0 && out.size() < want; --i) {
        out.push_back(all[i - 1]);
    }
    return out;  // 最新在前
}

std::string SecurityRollback::SavepointListError() const {
    return savepointsError_;
}

std::string SecurityRollback::SavepointFilePath() {
    return SaveFilePathNoCreate();
}

void SecurityRollback::SetSavepointError(const std::string& msg) const {
    savepointsError_ = msg;
}

// ---------------- 快照 ----------------

bool SecurityRollback::CreateSavePoint(uint32_t processId, const std::string& gameName) {
    SetError("");
    EnsureLoaded();
    SavePoint sp;
    sp.processId = processId;
    sp.gameName = gameName;
    sp.timestampMs = NowMs();

    HANDLE h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION,
                           FALSE, processId);
    if (h != nullptr) {
        const DWORD pc = GetPriorityClass(h);
        if (pc != 0) {
            sp.priorityClass = pc;
            sp.hasProcessState = true;
        }
        DWORD_PTR pm = 0, sm = 0;
        if (GetProcessAffinityMask(h, &pm, &sm)) {
            sp.affinityMask = static_cast<uint64_t>(pm);
            sp.hasProcessState = true;
        }
        SIZE_T wmin = 0, wmax = 0;
        if (GetProcessWorkingSetSize(h, &wmin, &wmax)) {
            sp.workingSetMin = static_cast<uint64_t>(wmin);
            sp.workingSetMax = static_cast<uint64_t>(wmax);
            sp.hasWorkingSet = true;
        }
        CloseHandle(h);
    }

    GUID scheme{};
    if (HAL::QueryActivePowerScheme(&scheme)) {
        sp.powerSchemeGuid = GuidToString(scheme);
        sp.hasPowerScheme = true;
    }

    sp.description = "应用前快照（" + gameName + "）";
    stack_.push_back(sp);
    AppendSavePoint(sp);
    return true;
}

// ---------------- 应用 ----------------

SecurityRollback::ApplyReport SecurityRollback::ApplyPreset(uint32_t processId,
                                                            const GamePreset& preset) {
    ApplyReport rep;
    HANDLE h = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA,
                           FALSE, processId);
    if (h == nullptr) {
        rep.failures.push_back("OpenProcess 失败（进程可能不存在，或需要管理员权限）");
        return rep;
    }

    if (preset.processPriorityClass != 0) {
        const ULONGLONG t0 = GetTickCount64();
        const bool ok = HAL::SetProcessPriority(h, preset.processPriorityClass);
        rep.items.push_back({"进程优先级", ok, static_cast<int>(GetTickCount64() - t0)});
        if (ok) ++rep.appliedCount;
        else { ++rep.failedCount; rep.failures.push_back(HAL::LastErrorText()); }
    }
    if (preset.cpuAffinityMask != 0) {
        const ULONGLONG t0 = GetTickCount64();
        const bool ok = HAL::SetProcessAffinity(h, preset.cpuAffinityGroup, preset.cpuAffinityMask);
        rep.items.push_back({"CPU 亲和性", ok, static_cast<int>(GetTickCount64() - t0)});
        if (ok) ++rep.appliedCount;
        else { ++rep.failedCount; rep.failures.push_back(HAL::LastErrorText()); }
    }
    if (preset.workingSetMinMB != 0 || preset.workingSetMaxMB != 0) {
        const ULONGLONG t0 = GetTickCount64();
        const bool ok = HAL::SetProcessWorkingSetMB(h, preset.workingSetMinMB, preset.workingSetMaxMB);
        rep.items.push_back({"工作集", ok, static_cast<int>(GetTickCount64() - t0)});
        if (ok) ++rep.appliedCount;
        else { ++rep.failedCount; rep.failures.push_back(HAL::LastErrorText()); }
    }
    if (preset.switchHighPerformancePower) {
        // 电源切换需要管理员权限；失败降级跳过（不中止其余优化）
        const ULONGLONG t0 = GetTickCount64();
        const bool ok = HAL::ActivateHighPerformanceScheme();
        rep.items.push_back({"电源方案（高性能）", ok, static_cast<int>(GetTickCount64() - t0)});
        if (ok) ++rep.appliedCount;
        else rep.failures.push_back("电源方案切换跳过: " + HAL::LastErrorText());
    }
    CloseHandle(h);
    rep.ok = rep.appliedCount > 0;
    return rep;
}

// ---------------- 回滚 ----------------

bool SecurityRollback::RollbackToLastSave() {
    SetError("");
    EnsureLoaded();
    if (stack_.empty()) {
        SetError("没有可回滚的快照");
        return false;
    }
    const SavePoint sp = stack_.back();
    stack_.pop_back();
    RewriteSavePoints(stack_);

    std::vector<std::string> failures;

    // 逆序恢复：电源 → 工作集 → 亲和性 → 优先级
    if (sp.hasPowerScheme && !sp.powerSchemeGuid.empty()) {
        GUID target{};
        if (StringToGuid(sp.powerSchemeGuid, &target)) {
            // 仅当与当前方案不同时才恢复（电源未被改过则无需，且避免无谓的管理员权限请求）
            bool identical = false;
            GUID current{};
            if (HAL::QueryActivePowerScheme(&current)) {
                identical = (current.Data1 == target.Data1 && current.Data2 == target.Data2 &&
                             current.Data3 == target.Data3 &&
                             std::memcmp(current.Data4, target.Data4, sizeof(current.Data4)) == 0);
            }
            if (!identical) {
                if (!HAL::ActivatePowerScheme(target)) {
                    failures.push_back("电源方案恢复失败: " + HAL::LastErrorText());
                }
            }
        }
    }

    HANDLE h = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA,
                           FALSE, sp.processId);
    if (h != nullptr) {
        if (sp.hasWorkingSet) {
            if (!HAL::SetProcessWorkingSet(h, sp.workingSetMin, sp.workingSetMax)) {
                failures.push_back("工作集恢复失败: " + HAL::LastErrorText());
            }
        }
        if (sp.hasProcessState && sp.affinityMask != 0) {
            if (!HAL::SetProcessAffinity(h, 0, sp.affinityMask)) {
                failures.push_back("亲和性恢复失败: " + HAL::LastErrorText());
            }
        }
        if (sp.hasProcessState && sp.priorityClass != 0) {
            if (!HAL::SetProcessPriority(h, sp.priorityClass)) {
                failures.push_back("优先级恢复失败: " + HAL::LastErrorText());
            }
        }
        CloseHandle(h);
    }
    // 进程已退出：进程相关项无需恢复（自然恢复），不视为失败

    if (!failures.empty()) {
        std::string msg = "回滚部分失败（进程可能已退出或需要管理员权限）: ";
        for (const auto& f : failures) msg += f + "; ";
        SetError(msg);
        return false;
    }
    return true;
}

bool SecurityRollback::RollbackAll() {
    SetError("");
    EnsureLoaded();
    bool allOk = true;
    while (!stack_.empty()) {
        if (!RollbackToLastSave()) allOk = false;  // 继续回滚剩余快照
    }
    return allOk;
}

// ---------------- 看门狗 ----------------

void SecurityRollback::StartWatchdog(const WatchdogConfig& cfg) {
    StopWatchdog();
    systemStable_.store(true);
    consecutiveHits_.store(0);
    graceMillis_ = cfg.graceSeconds * 1000;
    appliedAtMs_ = NowMs();
    watchdogRunning_.store(true);

    watchdogThread_ = std::thread([this, cfg]() {
        while (watchdogRunning_.load()) {
            const int64_t t0 = NowMs();
            std::this_thread::sleep_for(std::chrono::milliseconds(cfg.sleepMs));
            const int64_t delta = NowMs() - t0 - cfg.sleepMs;

            if (NowMs() - appliedAtMs_ < graceMillis_) continue;  // 宽限期不计

            if (delta > cfg.jitterThresholdMs) {
                if (consecutiveHits_.fetch_add(1) + 1 >= cfg.consecutiveHits) {
                    systemStable_.store(false);  // 系统响应异常
                    break;
                }
            } else {
                consecutiveHits_.store(0);
            }
        }
    });
}

void SecurityRollback::StopWatchdog() {
    watchdogRunning_.store(false);
    if (watchdogThread_.joinable()) watchdogThread_.join();
}

bool SecurityRollback::IsSystemStable() const {
    return systemStable_.load();
}

}  // namespace gopt
