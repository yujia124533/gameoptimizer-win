#pragma once

#include <atomic>
#include <cstdint>
#include <string>
#include <thread>
#include <vector>

#include "preset/GamePreset.h"

namespace gopt {

// 单次优化前的系统状态快照（多级撤销栈元素）
struct SavePoint {
    uint32_t processId = 0;        // 目标进程 PID
    uint32_t priorityClass = 0;    // 原优先级类（0=未知）
    uint64_t affinityMask = 0;     // 原亲和性掩码（组 0；0=未知）
    uint64_t workingSetMin = 0;    // 原工作集下限（字节）
    uint64_t workingSetMax = 0;    // 原工作集上限（字节）
    bool hasProcessState = false;  // 进程状态（优先级/亲和性）读取是否成功
    bool hasWorkingSet = false;    // 工作集读取是否成功
    bool hasPowerScheme = false;   // 电源方案读取是否成功
    std::string powerSchemeGuid;   // 原电源方案 GUID（字符串形式，便于日志）
    int64_t timestampMs = 0;
    std::string gameName;          // 本次优化针对的游戏（日志用）
    std::string description;
};

// 快照 + 多级回滚 + 心跳看门狗。
// 设计约束：回滚按逆序恢复；某一步失败继续尝试剩余步骤；不做任何注入/Hook。
class SecurityRollback {
public:
    ~SecurityRollback();

    // 捕获目标进程当前状态并压入快照栈（在应用任何修改前调用）
    bool CreateSavePoint(uint32_t processId, const std::string& gameName);

    // 应用预设：先 CreateSavePoint，再逐个执行（失败项降级记录，不中止后续）
    struct ApplyReport {
        bool ok = false;                    // 是否有任何项成功
        int appliedCount = 0;
        int failedCount = 0;
        std::vector<std::string> failures;  // 失败原因（含降级说明）
    };
    static ApplyReport ApplyPreset(uint32_t processId, const GamePreset& preset);

    // 回滚最近一次快照（逆序恢复；某一步失败继续剩余步骤）
    bool RollbackToLastSave();

    // 全部回滚（逐级撤销直到栈空）
    bool RollbackAll();

    // 看门狗：AppCore 在应用后启动；心跳线程测量调度抖动，持续异常时置不稳定标志
    struct WatchdogConfig {
        int sleepMs = 1;             // 心跳间隔
        int jitterThresholdMs = 25;  // 单次抖动阈值
        int graceSeconds = 10;       // 应用后宽限期（着色器编译/加载不计）
        int consecutiveHits = 30;    // 连续卡顿计数阈值
    };
    void StartWatchdog(const WatchdogConfig& cfg);
    void StopWatchdog();
    bool IsSystemStable() const;  // false = 系统响应异常，应触发自动回滚

    size_t SavePointCount() const;
    std::string LastErrorText() const;

private:
    void SetError(const std::string& msg) const;
    static int64_t NowMs();

    // 快照持久化：跨进程回滚（apply 与 rollback 是独立进程的两次运行）
    void EnsureLoaded();
    static std::string SaveFilePath();
    static std::vector<SavePoint> LoadSavePoints();
    static void AppendSavePoint(const SavePoint& sp);
    static void RewriteSavePoints(const std::vector<SavePoint>& list);
    static std::string Serialize(const SavePoint& sp);
    static bool Deserialize(const std::string& line, SavePoint* sp);

    std::vector<SavePoint> stack_;
    mutable std::string lastError_;
    bool loaded_ = false;

    std::thread watchdogThread_;
    std::atomic<bool> watchdogRunning_{false};
    std::atomic<bool> systemStable_{true};
    std::atomic<int> consecutiveHits_{0};
    int graceMillis_ = 0;
    int64_t appliedAtMs_ = 0;
};

}  // namespace gopt
