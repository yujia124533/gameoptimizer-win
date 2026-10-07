#pragma once
// =============================================================================
// GameOptimizer v1.1.0 — 页面组 B：启动项页（自包含子窗口）
// -----------------------------------------------------------------------------
// 纯 UI 页面（窗口类 GoptPageStartup）：列表 + 禁用选中 / 启用选中 / 恢复全部，
// 每个操作都有回执（页面横幅 + Hooks::receipt → 宿主日志），并显示选中项的命令行。
//
// 安全边界（红线）：
//   * 本模块不读写注册表、不改动任何启动项；全部动作经 Hooks 回调交宿主。推荐的接线是
//     gopt::StartupManager::{List,Disable,Enable,RestoreAll}（禁用 = 值名加 "[disabled] "
//     前缀并记录备份；恢复全部 = 按备份还原），本任务不修改 src/tuning/*，
//     页面没有绕过备份机制的路径。
//   * **命名要点**：StartupManager::Disable(name) / Enable(name) 期望的都是「原始值名」
//     （函数内部自行加/去 "[disabled] " 前缀）。页面已经剥掉前缀，所以 Hooks 回调收到的
//     `name` 一定是原始值名，宿主直接透传给 StartupManager 即可。
//
// 【宿主装配示例（gopt_gui.cpp 由队长收口）】
//   using gopt::ui::PageStartup;
//   PageStartup::Hooks h;
//   h.list = [] {
//       std::vector<PageStartup::StartupRowInfo> out;
//       for (const auto& e : gopt::StartupManager::List()) {
//           PageStartup::StartupRowInfo r;
//           r.hive = e.hive; r.name = e.name; r.value = e.value;
//           r.disabled = e.name.rfind("[disabled] ", 0) == 0;   // 页面也会自行判定
//           out.push_back(r);
//       }
//       return out; };
//   h.disable = [](const std::string& name) {
//       PageStartup::OpResult r;
//       r.ok = gopt::StartupManager::Disable(name);
//       r.text = std::string(r.ok ? "OK: " : "FAIL: ") + name;
//       return r; };
//   h.enable  = [](const std::string& name) { /* StartupManager::Enable(name) 同构 */ };
//   h.restoreAll = [] {
//       PageStartup::OpResult r;
//       r.affected = gopt::StartupManager::RestoreAll();
//       r.ok = r.affected >= 0;
//       r.text = ...;
//       return r; };
//   h.receipt = [](const std::string& s) { AddLog(s + "\n\n"); };
//   g_pageStartup = PageStartup::Create(g_pages[4], rect, h);
// =============================================================================

#include <windows.h>

#include <functional>
#include <string>
#include <vector>

#include "gui/ui_theme.h"
#include "gui/ui_widgets.h"

namespace gopt {
namespace ui {

class PageStartup {
public:
    // 控件 ID（4200 段）
    static constexpr int kIdBase = 4200;
    enum : int {
        kList        = kIdBase + 1,  // 启动项列表（LBS_OWNERDRAWFIXED，自绘行）
        kBtnRefresh  = kIdBase + 2,  // 刷新
        kBtnDisable  = kIdBase + 3,  // 禁用选中
        kBtnEnable   = kIdBase + 4,  // 启用选中
        kBtnRestore  = kIdBase + 5,  // 恢复全部
        kIdLast      = kIdBase + 5,
        kBtnCount    = 4,
    };
    static constexpr const wchar_t* kClassName = L"GoptPageStartup";

    // 宿主提供的启动项数据（页面不读注册表）
    struct StartupRowInfo {
        std::string hive;      // "HKCU" / "HKLM"
        std::string name;      // Run 值名（被禁用项带 "[disabled] " 前缀）
        std::string value;     // 值内容（命令行）
        bool        disabled = false;  // 可选；宿主未填时页面按 "[disabled] " 前缀判定
    };
    struct OpResult {
        bool ok = false;
        int  affected = 0;     // 受影响条目数（恢复全部用）
        std::string text;      // 人类可读回执
    };
    struct Hooks {
        std::function<std::vector<StartupRowInfo>()> list;
        std::function<OpResult(const std::string& name)> disable;  // 传「原始值名」
        std::function<OpResult(const std::string& name)> enable;   // 传「原始值名」
        std::function<OpResult()> restoreAll;
        std::function<void(const std::string&)> receipt;
    };

    // ---- 装配接口 ----
    static HWND Create(HWND parent, const RECT& rc, const Hooks& hooks);
    static void SetHooks(HWND page, const Hooks& hooks);
    static void FillParent(HWND page);
    static void Layout(HWND page, const RECT& rc);
    static void Show(HWND page, bool visible);
    // 重新读取启动项列表（announce=true 额外 push 一条「已刷新」回执）
    static void Refresh(HWND page, bool announce = false);
    static void ApplyLabels(HWND page);
    static void Destroy(HWND page);
    static bool IsPageWindow(HWND h);

    // 选中项的原始值名（无选中返回空串）与 hive
    static std::string SelectedName(HWND page);
    static std::string SelectedHive(HWND page);

    static std::string SelfCheck(HWND page = nullptr);
};

}  // namespace ui
}  // namespace gopt
