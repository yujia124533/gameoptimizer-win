#pragma once
// =============================================================================
// GameOptimizer v1.1.0 — 页面组 A 公共契约 + 总览页（含优化历史 / 一键回滚）
// -----------------------------------------------------------------------------
// 本文件 =「页面组 A」装配契约 + 总览页装配接口；page_game.h/.cpp 复用同一契约
// （宿主回调 / 页内共享开关 / 几何度量 / UTF-8 工具 / 自绘按钮悬停）。
//
// 【宿主（gopt_gui.cpp）装配清单 —— 只需 6 个时机，页面模块其余自带】
//   WM_CREATE :
//       DashboardPageCreate(g_pages[0], hooks);      // hooks.sharedCore = g_core
//       GamePageCreate(g_pages[1], hooks);
//   WM_SIZE    : DashboardPageLayout();  GamePageLayout();
//                （页面也会在 1 秒定时器里自愈：面板尺寸与页容器不一致时自动重排）
//   切页       : DashboardPageOnShow();  GamePageOnShow();
//   语言切换   : DashboardPageApplyLanguage();  GamePageApplyLanguage();
//   主题/DPI   : DashboardPageApplyTheme();     GamePageApplyTheme();
//   WM_DESTROY : DashboardPageDestroy();        GamePageDestroy();
//
// 【通知路径（相对 v1.0.19 的唯一变化，已按任务要求在此说明）】
//   v1.0.19：页容器(STATIC) → PageProc 子类化 → 转发 WM_COMMAND 给主窗口。
//   v1.1.0 页面组 A：页容器仍是宿主创建的 STATIC（PageProc 原样保留、未改动），
//   页面在其内部再建一层「页内面板」自绘窗口；面板是页内控件的直接父窗口，因此
//   控件通知落到面板，由面板就地调用页面自己的分发函数（按钮不可能再变死代码，
//   也不依赖宿主是否记得路由新 ID）。面板同时把 WM_COMMAND 转发给页容器，既有
//   PageProc → 主窗口 链路保持可用（宿主 WndProc 不识别这些 ID 时会走 default）。
//   => 宿主**不要**再为页面组 A 的控件 ID 写任何处理分支，否则会重复执行。
//
// 【红线】仅官方 user32/gdi32/advapi32/kernel32 API；无注入、无 Hook、无第三方库；
//         一键优化与回滚都在工作线程执行、用 PostMessage 回 UI（绝不阻塞界面）。
// =============================================================================

#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <string>
#include <vector>

#include "gui/ui_widgets.h"  // UiButtonState / UiTone（页面组共用）；内部已含 ui_theme.h

namespace gopt {
class AppCore;

namespace ui {

// -----------------------------------------------------------------------------
// 0. 纯整数小工具（两页共用；刻意不用 std::min/max，避免 windows.h 的 min/max 宏污染）
// -----------------------------------------------------------------------------
inline int  PageMinI(int a, int b) { return a < b ? a : b; }
inline int  PageMaxI(int a, int b) { return a > b ? a : b; }
inline int  PageClampI(int v, int lo, int hi) { return v < lo ? lo : (v > hi ? hi : v); }
inline bool PageRectEmpty(const RECT& r) { return r.right <= r.left || r.bottom <= r.top; }
inline void PageClearRect(RECT* r) {
    if (r != nullptr) { r->left = 0; r->top = 0; r->right = 0; r->bottom = 0; }
}
inline bool PageRectInside(const RECT& inner, const RECT& outer) {
    return inner.left >= outer.left && inner.top >= outer.top && inner.right <= outer.right &&
           inner.bottom <= outer.bottom;
}
// 两个矩形是否重叠（空矩形视为「未使用」，永不重叠）
inline bool PageRectOverlap(const RECT& a, const RECT& b) {
    if (PageRectEmpty(a) || PageRectEmpty(b)) return false;
    return a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
}
inline void PageSetRect(RECT* r, int l, int t, int rt, int bt) {
    if (r == nullptr) return;
    r->left = l; r->top = t; r->right = rt; r->bottom = bt;
}

// -----------------------------------------------------------------------------
// 1. 宿主回调（全部可空；为空时页面退化为「页内状态条 + 对话框」反馈，绝不静默）
// -----------------------------------------------------------------------------
struct PageHostHooks {
    gopt::AppCore* sharedCore = nullptr;            // 宿主 g_core：**仅 UI 线程只读使用**
    void (*AppendLog)(const char* utf8) = nullptr;  // 追加到主窗口日志区
    void (*SetStatus)(const char* utf8) = nullptr;  // 覆盖底部状态栏文本
};

// -----------------------------------------------------------------------------
// 2. 页组 A 共享的 UI 开关（单进程单实例，仅 UI 线程读写）
//    例：游戏优化页的「启用电源方案切换」勾选同步给总览页的一键优化使用。
// -----------------------------------------------------------------------------
struct SharedUiFlags {
    bool allowPowerSchemeSwitch = false;  // 对应 AppConfig::allowPowerSchemeSwitch
};
SharedUiFlags& PageSharedFlags();

// -----------------------------------------------------------------------------
// 3. 建议的最小内容区（96 DPI 逻辑像素）；宿主可用 WM_GETMINMAXINFO 约束窗口下限。
//    低于此值时页面不重叠、不裁切，但按降级规则隐藏次要控件（见 DashLayout::note）。
// -----------------------------------------------------------------------------
constexpr int kPageMinWidthPx  = 640;
constexpr int kPageMinHeightPx = 300;

// -----------------------------------------------------------------------------
// 4. UTF-8 <-> UTF-16 工具（两页共用；全部走 MultiByteToWideChar/WideCharToMultiByte）
// -----------------------------------------------------------------------------
std::wstring PageToWide(const std::string& utf8);
std::string  PageFromWide(const wchar_t* w);
std::string  PageGetTextUtf8(HWND h);                       // 读控件文本 → UTF-8
void         PageSetTextUtf8(HWND h, const std::string& s);  // UTF-8 → 控件文本

// -----------------------------------------------------------------------------
// 5. 自绘按钮悬停支持（页组共用）
//    BS_OWNERDRAW 按钮默认只有 常态/按下/焦点/禁用；这里用「每窗口子类化」补出
//    悬停态（WM_MOUSEMOVE + TrackMouseEvent/TME_LEAVE），属于进程内 SetWindowLongPtr，
//    不是系统 Hook。primary 标志随按钮登记，供 UiDrawButton 使用。
// -----------------------------------------------------------------------------
bool PageEnableButtonHover(HWND button, bool primary);
void PageDisableAllButtonHover();
bool PageIsButtonHot(HWND button);
bool PageIsButtonPrimary(HWND button);
// DRAWITEMSTRUCT → 控件状态（含上面登记的悬停态）；非本页控件返回 Normal
UiButtonState PageButtonStateFromDrawItem(const DRAWITEMSTRUCT* di);

// -----------------------------------------------------------------------------
// 6. 几何度量令牌快照（纯整数；布局计算不依赖任何窗口/设备上下文 → 可单测）
// -----------------------------------------------------------------------------
struct PageMetrics {
    int dpi      = 96;
    int sp[7]    = {2, 4, 8, 16, 24, 32, 48};       // UiSpace: Xxs..Xxl
    int ctrlH[4] = {24, 32, 40, 56};                // UiControlH: Sm..Xl
    int radius[4]= {4, 8, 12, 999};                 // UiRadius: Sm..Pill
    int px(int logical) const { return MulDiv(logical, dpi, 96); }  // 96DPI 逻辑值 → 物理像素
    int space(int i) const { return (i >= 0 && i < 7) ? sp[i] : 0; }
    int height(int i) const { return (i >= 0 && i < 4) ? ctrlH[i] : 0; }
};
PageMetrics MakePageMetrics();  // 从 ui_theme 令牌 + 当前 DPI 生成

// 页组共用绘制：左侧色调圆点 + 单行文本（页内状态行，紧凑版 HintBanner）。
// 供总览页与游戏优化页共用，保证两页反馈样式一致。
void PageDrawStatusLine(HDC dc, const RECT& rc, UiTone tone, const std::string& textUtf8,
                        const PageMetrics& m, UiFontRole role = UiFontRole::Caption);

// -----------------------------------------------------------------------------
// 7. 总览页控件 ID（页面组 A 独占区间 1201..1206；避开既有 201-205/301-311/
//    401-404/501-605/700-701 与计时器 101/102/106）
// -----------------------------------------------------------------------------
enum DashCtlId {
    IDC_DASH_BOOST        = 1201,  // 一键性能优化（主按钮，工作线程）
    IDC_DASH_AUTOSTART    = 1202,  // 开机自启动（原生复选框，HKCU\...\Run）
    IDC_DASH_ABOUT        = 1203,  // 诊断 / 关于（只读信息对话框）
    IDC_DASH_HIST_REFRESH = 1204,  // 刷新优化历史（只读查询）
    IDC_DASH_ROLLBACK     = 1205,  // 一键回滚最近一次（工作线程 + PostMessage 回 UI）
    IDC_DASH_HIST_LIST    = 1206,  // 优化历史列表（LBS_OWNERDRAWFIXED，自绘行）
};
constexpr int kDashCtlFirst = 1201;
constexpr int kDashCtlLast  = 1206;

// -----------------------------------------------------------------------------
// 8. 总览页布局（纯整数运算；空矩形 = 该控件被降级隐藏，永不用重叠换空间）
// -----------------------------------------------------------------------------
struct DashLayout {
    RECT panel{};       // 面板整块（页容器客户区）
    RECT cardHw{};      // 卡1：硬件信息
    RECT cardLive{};    // 卡2：实时 CPU/内存（大号数字 + 进度条）
    RECT cardCurve{};   // 卡3：实时曲线（48 秒 CPU/RAM）
    RECT areaAct{};     // 左下动作区（无卡片底：按钮 + 状态行）
    RECT cardHist{};    // 右下：优化历史（只读）

    RECT hwTitle{};     // 卡1 内绘制区
    RECT hwLine[4]{};   // CPU 型号 / 核数与频率 / GPU / 内存
    RECT liveTitle{};   // 卡2 内绘制区
    RECT metricCpu{};   // CPU% 大号数字
    RECT barCpu{};      // CPU 进度条（h=10）
    RECT metricRam{};   // 内存占用% 大号数字
    RECT barRam{};      // 内存进度条（h=10）
    RECT liveFoot{};    // 卡2 底部：图例 / 一键优化进度（flow 进行中时）
    RECT curveTitle{};  // 卡3 内绘制区
    RECT curveLegend{};
    RECT curveArea{};

    RECT btnBoost{};     // 控件 1201
    RECT chkAutostart{}; // 控件 1202
    RECT btnRollback{};  // 控件 1205
    RECT btnAbout{};     // 控件 1203
    RECT statusLine{};   // 页内状态行（可见反馈）
    RECT histTitle{};    // 卡内绘制区
    RECT btnHistRefresh{};  // 控件 1204
    RECT lstHist{};         // 控件 1206
    RECT histStatus{};   // 历史卡底部：条数/最新时间/降级或错误文案

    bool        valid = false;  // false = 内容区小于建议值，已按规则隐藏次要控件
    bool        compact = false;  // true = 紧凑模式（<640x300 逻辑：曲线让位，历史与动作区保留）
    std::string note;           // 降级说明（可读，供日志/报告）
};
DashLayout ComputeDashLayout(int w, int h, const PageMetrics& m);
// 无重叠自检：同层矩形两两不重叠 + 全部落在所属容器内 + 非负宽高。
// 返回 false 时 detail 给出首个冲突（含矩形名）。
bool DashLayoutSelfCheck(const DashLayout& L, std::string* detail);

// -----------------------------------------------------------------------------
// 9. 总览页装配接口（宿主按文件头「装配清单」调用）
// -----------------------------------------------------------------------------
// 在页容器内建面板与全部控件；返回 false 表示创建失败（此时不应再调其它接口）。
bool DashboardPageCreate(HWND pageContainer, const PageHostHooks& hooks);
void DashboardPageDestroy();
void DashboardPageLayout();        // 读页容器客户区 → 面板填满 → 按 8px 栅格重排
void DashboardPageOnShow();        // 切回总览页：刷新硬件/实时采样 + 优化历史
bool DashboardPageCommand(int id, int code);  // 页面内部使用（宿主**不要**调用）
void DashboardPageApplyLanguage(); // 语言切换：刷新静态文案 + 重绘
void DashboardPageApplyTheme();    // 主题/DPI 变化：重设字体、列表行高、重排重绘
void DashboardPageRefresh();       // 立即刷新（硬件信息 + 实时数字 + 历史）

}  // namespace ui
}  // namespace gopt
