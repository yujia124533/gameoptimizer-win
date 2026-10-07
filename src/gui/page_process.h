#pragma once
// =============================================================================
// GameOptimizer v1.1.0 — 页面组 B：进程页（自包含子窗口）
// -----------------------------------------------------------------------------
// 纯 UI 页面（窗口类 GoptPageProcess）：列表显示「优先级 + CPU% + 内存 MB」，
// 按 CPU% 降序排序，选中项按 PID 保持，双击行 = 提升优先级，另有刷新 / 提升 / 恢复正常。
//
// 安全边界（红线）：
//   * 本模块不调用 OpenProcess / SetPriorityClass / 任何注入或 Hook API；进程枚举与
//     优先级修改全部经 Hooks 回调交宿主（宿主内部走 HAL::SetProcessPriority，上限 HIGH）。
//     因此「优先级上限 HIGH、不使用 REALTIME、无注入」的红线由宿主/HAL 单点保证，页面无权绕过。
//   * CPU% 采样口径与 v1.0.19 完全一致：GetProcessTimes 累计 tick 差值 /
//     (毫秒 * 10000 * 逻辑核数)；无采样数据（首次刷新）按 0 参与排序。
//
// 【语义不回退的保证（进程页）】
//   1) 排序：std::stable_sort 按 cpuPct 降序 —— 相同 CPU%（含首轮无采样）保持宿主枚举顺序；
//   2) 选中：重排前先记下「当前选中行的 pid」，重排后按 pid 找回落点；pid 已退出时回退第 0 行
//      （与 v1.0.19「总有一行被选中」行为一致）；列表为空时清空选择（cursel=-1），
//      此时三条操作按钮一律提示「请先在列表中选择」，不会误操作；
//   3) 双击行与「提升优先级」按钮共用同一个 DoBoost(true) 代码路径，逻辑不分叉。
//
// 【宿主装配示例（gopt_gui.cpp 由队长收口）】
//   using gopt::ui::PageProcess;
//   PageProcess::Hooks h;
//   h.snapshot = [] {
//       PageProcess::ProcSnapshot s;
//       gopt::HardwareProfile p = g_core->Profile();
//       s.logicalCores = p.logicalCores > 0 ? p.logicalCores : 1;
//       for (const auto& [id, pid] : g_core->RunningGames()) {
//           PageProcess::ProcSample it;
//           it.name = gopt::GameIdToString(id);
//           it.pid  = pid;
//           HANDLE ph = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
//           if (ph) {
//               it.accessible = true;
//               it.priorityClass = GetPriorityClass(ph);
//               FILETIME c{}, e{}, k{}, u{};
//               if (GetProcessTimes(ph, &c, &e, &k, &u)) { /* k+u → it.cpuTicks */ }
//               PROCESS_MEMORY_COUNTERS pmc{}; pmc.cb = sizeof(pmc);
//               if (GetProcessMemoryInfo(ph, &pmc, sizeof(pmc))) it.memBytes = pmc.WorkingSetSize;
//               CloseHandle(ph);
//           }
//           s.items.push_back(it);
//       }
//       return s; };
//   h.setPriority = [](uint32_t pid, bool high) {
//       PageProcess::OpResult r;
//       HANDLE ph = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
//       if (!ph) { r.text = T("无法打开进程（可能已退出或无权限）。", "Cannot open process."); return r; }
//       r.ok = gopt::HAL::SetProcessPriority(ph, high ? HIGH_PRIORITY_CLASS : NORMAL_PRIORITY_CLASS);
//       CloseHandle(ph);
//       r.text = ...;   // 人类可读回执（页面直接显示）
//       return r; };
//   h.receipt = [](const std::string& s) { AddLog(s + "\n\n"); };
//   g_pageProc = PageProcess::Create(g_pages[3], rect, h);
// =============================================================================

#include <windows.h>

#include <cstdint>
#include <functional>
#include <string>
#include <vector>

#include "gui/ui_theme.h"
#include "gui/ui_widgets.h"

namespace gopt {
namespace ui {

class PageProcess {
public:
    // 控件 ID（4100 段）
    static constexpr int kIdBase = 4100;
    enum : int {
        kList       = kIdBase + 1,  // 进程列表（LBS_OWNERDRAWFIXED，自绘行）
        kBtnRefresh = kIdBase + 2,  // 刷新
        kBtnBoost   = kIdBase + 3,  // 提升优先级（上限 HIGH）
        kBtnNormal  = kIdBase + 4,  // 恢复正常
        kIdLast     = kIdBase + 4,
        kBtnCount   = 3,
    };
    static constexpr const wchar_t* kClassName = L"GoptPageProcess";

    // 宿主提供的单进程原始样本（页面不做任何进程访问）
    struct ProcSample {
        std::string name;               // 显示名（宿主本地化后的游戏名）
        uint32_t    pid = 0;
        bool        accessible = false; // 是否成功读取到指标
        uint32_t    priorityClass = 0;  // GetPriorityClass 结果（0 = 未知）
        uint64_t    cpuTicks = 0;       // 内核 + 用户累计 tick（100ns）
        uint64_t    memBytes = 0;       // 工作集字节
    };
    struct ProcSnapshot {
        int logicalCores = 1;                 // CPU% 归一化用（≥1）
        std::vector<ProcSample> items;        // 宿主枚举顺序（稳定排序的兜底顺序）
    };
    struct OpResult {
        bool ok = false;
        std::string text;   // 人类可读回执
    };
    struct Hooks {
        std::function<ProcSnapshot()>            snapshot;    // 采集一帧（宿主实现）
        std::function<OpResult(uint32_t, bool)>  setPriority; // (pid, high) → 结果
        std::function<void(const std::string&)>  receipt;     // 回执 → 宿主日志
    };

    // ---- 装配接口 ----
    static HWND Create(HWND parent, const RECT& rc, const Hooks& hooks);
    static void SetHooks(HWND page, const Hooks& hooks);
    static void FillParent(HWND page);
    static void Layout(HWND page, const RECT& rc);
    static void Show(HWND page, bool visible);
    // 重新采集一帧（announce=true 时额外 push 一条「已刷新」回执；定时器自动刷新用 false）
    static void Refresh(HWND page, bool announce = false);
    static void ApplyLabels(HWND page);
    static void Destroy(HWND page);
    static bool IsPageWindow(HWND h);

    // 选中项的 pid（无选中返回 0）；供宿主导航/联动查询
    static uint32_t SelectedPid(HWND page);

    static std::string SelfCheck(HWND page = nullptr);
};

}  // namespace ui
}  // namespace gopt
