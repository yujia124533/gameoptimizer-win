#pragma once
// =============================================================================
// GameOptimizer v1.1.0 — 页面组 B：系统调优页（自包含子窗口）
// -----------------------------------------------------------------------------
// 本模块是**纯 UI 页面**：自建一个子窗口（窗口类 GoptPageTune），内部全部用 t1 的设计
// 令牌（ui_theme）与统一自绘控件（ui_widgets）绘制。它自己处理 WM_PAINT / WM_DRAWITEM /
// WM_COMMAND / WM_SIZE / WM_DPICHANGED，宿主只需要 Create / Layout / Show / ApplyLabels。
//
// 安全边界（红线）：
//   * 本模块不 OpenProcess、不 DeleteFile、不写注册表、不切换电源方案、不装任何 Hook；
//   * 所有系统动作都通过 Hooks 回调交给宿主（HAL / SystemTuner / AppCore）执行，
//     页面只负责把「人类可读回执」显示出来并转发给宿主日志。
//   * 「清理临时文件」唯一入口 = hooks.cleanTemp()。推荐的接线是
//     gopt::SystemTuner::CleanTemp()，其 24 小时保留策略（kKeepRecent100ns）在
//     src/tuning/SystemTuner.cpp 内保持不变，本任务不修改 src/tuning/*，页面也没有
//     任何绕过它的删除路径 —— 因此「保留 24 小时内文件」的安全策略不可能被本页弱化。
//   * 清理按钮为**二次确认**（首次点击进入 5 秒确认态并改文案，再点一次才执行），
//     页面上永久显示安全策略说明；这只会加强、不会削弱原策略。
//
// 【宿主装配示例（gopt_gui.cpp 由队长收口）】
//   using gopt::ui::PageTune;
//   PageTune::Hooks h;
//   h.recommendHighPerf   = [] { return gopt::SystemTuner::RecommendHighPerf(g_core->Profile()); };
//   h.activePowerSchemeName = [] {
//       GUID g{}; if (gopt::HAL::QueryActivePowerScheme(&g)) return gopt::HAL::PowerSchemeName(g);
//       return std::string(T("未知", "unknown")); };
//   h.isElevated   = [] { return gopt::HAL::IsElevated(); };
//   h.tune         = [](bool high) { AppCore* c = MakeCore(); std::string r = c->TuneSystem(high); delete c; return r; };
//   h.restoreTune  = [] { AppCore* c = MakeCore(); std::string r = c->RestoreTune(); delete c; return r; };
//   h.cleanTemp    = [] { return gopt::SystemTuner::CleanTemp(); };   // 24h 策略在核心内
//   h.receipt      = [](const std::string& s) { AddLog(s + "\n\n"); };
//   g_pageTune = PageTune::Create(g_pages[2], pageRect, h);   // 或直接以主窗口为 parent
//
//   // WM_SIZE        : PageTune::FillParent(g_pageTune);   // 铺满 parent 客户区并重排
//   // 语言切换        : PageTune::ApplyLabels(g_pageTune);
//   // 诊断页/自检     : AddLog(PageTune::SelfCheck());
//   // 展示/隐藏页面    : PageTune::Show(g_pageTune, on);
//   // WM_DESTROY      : PageTune::Destroy(g_pageTune);
// =============================================================================

#include <windows.h>

#include <functional>
#include <string>

#include "gui/ui_theme.h"    // 设计令牌：UiColor/UiSp/UiFontRole/UiControlHeight
#include "gui/ui_widgets.h"  // UiTone / UiDrawButton / UiDrawCard / UiDrawHintBanner ...

namespace gopt {
namespace ui {

class PageTune {
public:
    // 控件 ID（4000 段；与 gopt_gui.cpp 既有 1xx~7xx 不冲突）
    static constexpr int kIdBase = 4000;
    enum : int {
        kBtnHigh     = kIdBase + 1,  // 高性能档
        kBtnBalanced = kIdBase + 2,  // 平衡档
        kBtnRestore  = kIdBase + 3,  // 恢复调优
        kBtnClean    = kIdBase + 4,  // 清理临时文件（二次确认）
        kIdLast      = kIdBase + 4,
        kBtnCount    = 4,
    };
    static constexpr const wchar_t* kClassName = L"GoptPageTune";

    // 宿主接线：数据查询 + 动作执行 + 回执出口（任何一项都可以留空，页面会显示「未接线」）
    struct Hooks {
        std::function<bool()>                   recommendHighPerf;      // true = 本机建议高性能档
        std::function<std::string()>            activePowerSchemeName;  // 当前电源方案可读名
        std::function<bool()>                   isElevated;             // 是否管理员
        std::function<std::string(bool highPerf)> tune;                 // 应用档位 → 结果文本
        std::function<std::string()>            restoreTune;            // 恢复调优 → 结果文本
        std::function<std::string()>            cleanTemp;              // 清理临时文件 → 结果文本
        std::function<void(const std::string&)> receipt;                // 回执 → 宿主日志/状态栏
    };

    // ---- 装配接口 ----
    // 创建页面根窗口（作为 parent 的子窗口；rc 为 parent 客户区坐标）。已注册过类则复用。
    static HWND Create(HWND parent, const RECT& rc, const Hooks& hooks);
    static void SetHooks(HWND page, const Hooks& hooks);
    // 铺满 parent 客户区并重排（宿主 WM_SIZE 调用；内部含 8px 栅格重排）
    static void FillParent(HWND page);
    // 指定矩形重排（不做 MoveWindow 之外的任何事）
    static void Layout(HWND page, const RECT& rc);
    static void Show(HWND page, bool visible);
    // 重新读取推荐档位 / 电源方案 / 权限并重绘
    static void Refresh(HWND page);
    // 语言切换后刷新全部文案（按钮、标题、说明、回执前缀）
    static void ApplyLabels(HWND page);
    static void Destroy(HWND page);
    static bool IsPageWindow(HWND h);

    // ---- 布局/ID 自检（纯几何 + 真实子窗口矩形，返回多行报告）----
    // 覆盖 1280x800 / 1024x700 / 900x600 / 640x420 / 420x300 五种尺寸：
    // 逐对检查区块与按钮矩形是否相交、矩形是否越界、按钮 ID 是否唯一；全部通过时打印 PASS。
    static std::string SelfCheck(HWND page = nullptr);
};

}  // namespace ui
}  // namespace gopt
