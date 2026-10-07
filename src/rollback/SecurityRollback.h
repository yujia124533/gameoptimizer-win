#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <string>
#include <thread>
#include <utility>
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

// 快照历史条目（只读视图，供 CLI/GUI 展示「可回滚」能力）。
// 由 SecurityRollback::SavepointList() 从持久化文件解析得到，不反映内存栈、不修改任何状态。
struct SavepointInfo {
    int index = 0;                    // 序号：在持久化文件中的顺序（1 起；越大越新）
    int64_t timestampMs = 0;          // 原始快照时间戳（写入时的 NowMs() 值）
    std::string timeText;             // 可读本地时间文本；无法可靠换算时为说明文本（非空）
    std::string gameName;             // 涉及游戏名（可能为空）
    uint32_t processId = 0;           // 目标进程 PID（0 = 未记录）
    std::string processSummary;       // 「游戏/进程」单行概要（列表用，如 "三角洲行动 (PID 38760)"）
    int entryCount = 0;               // 可恢复条目数（= entries.size()，与 Rollback 实际恢复项一致）
    std::vector<std::string> entries; // 条目明细（如 "进程优先级: 高 (0x80)"），供 CLI show / GUI 详情
    bool isLatest = false;            // 是否最新一条（RollbackToLastSave 回滚的就是它）
    SavePoint raw;                    // 原始快照（完整字段；调用方如需自行格式化/双语可改用它）
};

// 快照 + 多级回滚 + 心跳看门狗。
// 设计约束：回滚按逆序恢复；某一步失败继续尝试剩余步骤；不做任何注入/Hook。
class SecurityRollback {
public:
    ~SecurityRollback();

    // 捕获目标进程当前状态并压入快照栈（在应用任何修改前调用）
    bool CreateSavePoint(uint32_t processId, const std::string& gameName);

    // 应用预设：先 CreateSavePoint，再逐个执行（失败项降级记录，不中止后续）
    struct StepItem {
        std::string label;   // 步骤名（如"进程优先级"）
        bool ok = false;     // 是否成功
        int elapsedMs = 0;   // 该步实际耗时
    };
    struct ApplyReport {
        bool ok = false;                    // 是否有任何项成功
        int appliedCount = 0;
        int failedCount = 0;
        std::vector<std::string> failures;  // 失败原因（含降级说明）
        std::vector<StepItem> items;        // 每步结果，供流程显示
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

    // ---------------- 只读快照历史查询（可观测性） ----------------
    // 数据源：持久化文件 %LOCALAPPDATA%\GameOptimizer\savepoints.txt（与回滚同址、跨进程有效）。
    // 严格只读：只做打开/读取，不创建目录、不写文件、不改内存栈，也不触碰回滚错误状态；
    //           调用前后文件内容、大小与最后写入时间均不变。
    // 降级（绝不抛异常、绝不崩溃）：
    //   * 文件不存在（从未优化过 / 目录被清理）→ 返回空列表；
    //   * 任一行无法解析（格式异常）→ 视为文件损坏，返回空列表而不是部分结果
    //     （部分解析会误报「可回滚内容」）；
    //   * 两种情况的可读原因都放在 SavepointListError()；查询成功时它为空字符串。
    // 顺序：文件顺序（旧 → 新），index 从 1 递增，最后一条 isLatest=true。
    std::vector<SavepointInfo> SavepointList() const;

    // 最近 maxCount 条快照（最新在前）；maxCount==0 表示不限（等价于 SavepointList() 的倒序）。
    // 降级与 SavepointListError() 语义同 SavepointList()。
    std::vector<SavepointInfo> RecentSavepoints(size_t maxCount) const;

    // 最近一次快照历史查询的错误/降级说明（空 = 查询成功）。
    // 与回滚路径的 LastErrorText() 完全独立，互不覆盖。
    std::string SavepointListError() const;

    // 快照文件绝对路径（只读解析，不创建目录/不写文件），供诊断显示
    static std::string SavepointFilePath();

private:
    void SetError(const std::string& msg) const;
    void SetSavepointError(const std::string& msg) const;
    static int64_t NowMs();

    // 快照持久化：跨进程回滚（apply 与 rollback 是独立进程的两次运行）
    void EnsureLoaded();
    static std::wstring SaveFileDirW();     // 所在目录（不求值创建）
    static std::string SaveFilePath();      // 可写路径（会确保目录存在）
    static std::string SaveFilePathNoCreate();  // 只读路径（不产生任何文件系统副作用）
    static std::vector<SavePoint> LoadSavePoints();
    static std::vector<SavePoint> LoadSavePointsReadOnly(size_t* badLines);  // 只读；统计不可解析行
    static void AppendSavePoint(const SavePoint& sp);
    static void RewriteSavePoints(const std::vector<SavePoint>& list);
    static std::string Serialize(const SavePoint& sp);
    static bool TryDeserialize(const std::string& line, SavePoint* sp);  // 真实解析（不抛异常）
    static bool Deserialize(const std::string& line, SavePoint* sp);     // 兼容包装（= TryDeserialize）
    static SavepointInfo MakeInfo(const SavePoint& sp, int index, bool isLatest);
    static std::vector<std::string> BuildEntries(const SavePoint& sp);
    std::vector<SavepointInfo> CollectSavepoints() const;  // 两个公开查询的共用实现

    std::vector<SavePoint> stack_;
    mutable std::string lastError_;        // 回滚路径错误（语义与 v1.0.19 一致）
    mutable std::string savepointsError_;  // 只读查询错误（与回滚路径隔离）
    bool loaded_ = false;

    std::thread watchdogThread_;
    std::atomic<bool> watchdogRunning_{false};
    std::atomic<bool> systemStable_{true};
    std::atomic<int> consecutiveHits_{0};
    int graceMillis_ = 0;
    int64_t appliedAtMs_ = 0;
};

}  // namespace gopt
