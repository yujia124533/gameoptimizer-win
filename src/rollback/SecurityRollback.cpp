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

std::string SecurityRollback::SaveFilePath() {
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
    return WideToUtf8(dir.c_str()) + "\\savepoints.txt";
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

bool SecurityRollback::Deserialize(const std::string& line, SavePoint* sp) {
    std::istringstream is(line);
    std::string s;
    auto next = [&](std::string& out) { return static_cast<bool>(std::getline(is, out, '|')); };
    if (!next(s)) return false;  sp->processId = static_cast<uint32_t>(std::stoul(s));
    if (!next(s)) return false;  sp->priorityClass = static_cast<uint32_t>(std::stoul(s));
    if (!next(s)) return false;  sp->affinityMask = static_cast<uint64_t>(std::stoull(s));
    if (!next(s)) return false;  sp->workingSetMin = static_cast<uint64_t>(std::stoull(s));
    if (!next(s)) return false;  sp->workingSetMax = static_cast<uint64_t>(std::stoull(s));
    if (!next(s)) return false;  sp->hasProcessState = (s == "1");
    if (!next(s)) return false;  sp->hasWorkingSet = (s == "1");
    if (!next(s)) return false;  sp->hasPowerScheme = (s == "1");
    if (!next(s)) return false;  sp->powerSchemeGuid = s;
    if (!next(s)) return false;  sp->timestampMs = std::stoll(s);
    if (!next(s)) return false;  sp->gameName = s;
    if (!next(s)) return false;  sp->description = s;
    return true;
}

std::vector<SavePoint> SecurityRollback::LoadSavePoints() {
    std::vector<SavePoint> list;
    std::ifstream in(SaveFilePath());
    std::string line;
    while (std::getline(in, line)) {
        if (line.empty()) continue;
        SavePoint sp;
        if (Deserialize(line, &sp)) list.push_back(sp);
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
        if (HAL::SetProcessPriority(h, preset.processPriorityClass)) {
            ++rep.appliedCount;
        } else {
            ++rep.failedCount;
            rep.failures.push_back(HAL::LastErrorText());
        }
    }
    if (preset.cpuAffinityMask != 0) {
        if (HAL::SetProcessAffinity(h, preset.cpuAffinityGroup, preset.cpuAffinityMask)) {
            ++rep.appliedCount;
        } else {
            ++rep.failedCount;
            rep.failures.push_back(HAL::LastErrorText());
        }
    }
    if (preset.workingSetMinMB != 0 || preset.workingSetMaxMB != 0) {
        if (HAL::SetProcessWorkingSetMB(h, preset.workingSetMinMB, preset.workingSetMaxMB)) {
            ++rep.appliedCount;
        } else {
            ++rep.failedCount;
            rep.failures.push_back(HAL::LastErrorText());
        }
    }
    if (preset.switchHighPerformancePower) {
        // 电源切换需要管理员权限；失败降级跳过（不中止其余优化）
        if (HAL::ActivateHighPerformanceScheme()) {
            ++rep.appliedCount;
        } else {
            rep.failures.push_back("电源方案切换跳过: " + HAL::LastErrorText());
        }
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
