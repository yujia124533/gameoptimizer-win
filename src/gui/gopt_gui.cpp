// GameOptimizer 原生 GUI（v1.1.0 外壳）
// -----------------------------------------------------------------------------
// 结构：外壳只负责 DPI/主题、导航与切页、日志与状态栏、托盘、流程反馈面板，
// 并把宿主能力（AppCore/HAL/SystemTuner/StartupManager）通过 hooks 注入页面；
// 五个页面各自成模块：page_dashboard / page_game（页面组 A，C 接口）
//                   page_tune / page_process / page_startup（页面组 B，类接口）
// 红线：仅官方 Win32 API；无注入、无内核 Hook；优先级上限 HIGH；一切可回滚。
#include <windows.h>
#include <shellapi.h>
#include <psapi.h>

#include <cmath>
#include <functional>
#include <string>
#include <vector>

#include "core/AppCore.h"
#include "gui/page_dashboard.h"
#include "gui/page_game.h"
#include "gui/page_process.h"
#include "gui/page_startup.h"
#include "gui/page_tune.h"
#include "gui/ui_theme.h"
#include "gui/ui_widgets.h"
#include "hal/HAL.h"
#include "i18n.h"
#include "tuning/StartupManager.h"
#include "tuning/SystemTuner.h"
#include "version.h"

using gopt::AppConfig;
using gopt::AppCore;
using gopt::Lang;
using gopt::SetLang;
using gopt::StartupManager;
using gopt::SystemTuner;
using gopt::T;

namespace ui = gopt::ui;

// ---------- 外壳自有控件 ID（页面控件 ID 由页面模块自管，外壳不得处理） ----------
enum {
    IDT_FLOW = 102,
    IDC_LANG = 106,
    IDC_NAV0 = 201, IDC_NAV1 = 202, IDC_NAV2 = 203, IDC_NAV3 = 204, IDC_NAV4 = 205,
    ID_FLOWPANEL = 700,
    WM_TRAY = WM_APP + 3,
};

// ---------- 状态 ----------
static AppCore* g_core = nullptr;
static AppConfig g_cfg;
static HWND g_hwnd = nullptr, g_log = nullptr, g_footer = nullptr, g_lang = nullptr;
static HWND g_pages[5] = {};
static HWND g_nav[5] = {};
static HWND g_pageTune = nullptr, g_pageProcess = nullptr, g_pageStartup = nullptr;
static int g_page = 0;
static HFONT g_font = nullptr;
static HBRUSH g_brushPanel = nullptr;

static NOTIFYICONDATAW g_tray = {};
static bool g_trayAdded = false, g_trayBalloonShown = false;

// 流程反馈面板（由页面日志驱动，不依赖页面日志的具体格式）
struct FlowLineUI {
    std::string text;
    int tone = 0;  // 0=进行中 1=成功 2=失败
};
static std::vector<FlowLineUI> g_flowLines;
static HWND g_flowPanel = nullptr;
static bool g_flowActive = false, g_flowHasFail = false;
static int g_flowAngle = 0;
static ULONGLONG g_flowLastTick = 0;

// ---------- 小工具 ----------
static std::wstring Utf8ToWide(const std::string& s) {
    if (s.empty()) return std::wstring();
    const int len = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
    if (len <= 1) return std::wstring();
    std::wstring w(static_cast<size_t>(len) - 1, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, &w[0], len);
    return w;
}

static bool Contains(const std::string& hay, const char* needle) {
    return hay.find(needle) != std::string::npos;
}

static void AddLog(const std::string& s);

// ---------- 流程面板 ----------
static void FlowPanelSync() {
    if (g_flowPanel == nullptr) return;
    ShowWindow(g_flowPanel, g_flowActive ? SW_SHOW : SW_HIDE);
    if (g_flowActive) InvalidateRect(g_flowPanel, nullptr, FALSE);
}

static void FlowBegin() {
    g_flowLines.clear();
    g_flowHasFail = false;
    g_flowActive = true;
    g_flowLastTick = GetTickCount64();
    FlowPanelSync();
}

static void FlowEnd() {
    if (!g_flowActive) return;
    g_flowActive = false;
    FlowPanelSync();
}

static void FlowObserve(const std::string& line) {
    std::string t = line;
    while (!t.empty() && (t.back() == '\n' || t.back() == '\r' || t.back() == ' ')) t.pop_back();
    while (!t.empty() && (t.front() == ' ' || t.front() == '\t')) t.erase(t.begin());
    if (t.empty()) return;
    if (t.rfind("==", 0) == 0) {
        if (Contains(t, "完成") || Contains(t, "done")) { FlowEnd(); return; }
        if (Contains(t, "优化") || Contains(t, "Optimiz") || Contains(t, "回滚") || Contains(t, "Rollback") ||
            Contains(t, "清理") || Contains(t, "Clean") || Contains(t, "调优") || Contains(t, "Tune")) {
            FlowBegin();
            return;
        }
    }
    if (!g_flowActive) return;
    FlowLineUI row;
    row.text = t;
    if (Contains(t, "失败") || Contains(t, "FAIL") || Contains(t, "✗")) row.tone = 2;
    else if (Contains(t, "成功") || Contains(t, "OK") || Contains(t, "✓") || Contains(t, "完成")) row.tone = 1;
    if (row.tone == 2) g_flowHasFail = true;
    g_flowLines.push_back(row);
    if (g_flowLines.size() > 8) g_flowLines.erase(g_flowLines.begin());
    g_flowLastTick = GetTickCount64();
    if (g_flowPanel != nullptr) InvalidateRect(g_flowPanel, nullptr, FALSE);
}

static void AddLog(const std::string& s) {
    if (g_log == nullptr) return;
    FlowObserve(s);
    const std::wstring w = Utf8ToWide(s);
    const int len = GetWindowTextLengthW(g_log);
    SendMessageW(g_log, EM_SETSEL, len, len);
    SendMessageW(g_log, EM_REPLACESEL, FALSE, reinterpret_cast<LPARAM>(w.c_str()));
    SendMessageW(g_log, EM_SCROLLCARET, 0, 0);
}

static void SetFooterStatus(const char* utf8) {
    if (g_footer == nullptr || utf8 == nullptr) return;
    SetWindowTextW(g_footer, Utf8ToWide(std::string(utf8)).c_str());
}

// ---------- 宿主能力 → 页面 hooks ----------
static ui::PageHostHooks MakePageHostHooks() {
    ui::PageHostHooks h;
    h.sharedCore = g_core;
    h.AppendLog = [](const char* s) { if (s != nullptr) AddLog(std::string(s)); };
    h.SetStatus = [](const char* s) { SetFooterStatus(s); };
    return h;
}

static ui::PageTune::Hooks MakeTuneHooks() {
    ui::PageTune::Hooks h;
    h.recommendHighPerf = []() { return g_core != nullptr && SystemTuner::RecommendHighPerf(g_core->Profile()); };
    h.activePowerSchemeName = []() {
        GUID scheme{};
        if (gopt::HAL::QueryActivePowerScheme(&scheme)) return gopt::HAL::PowerSchemeName(scheme);
        return std::string("?");
    };
    h.isElevated = []() { return gopt::HAL::IsElevated(); };
    h.tune = [](bool highPerf) { AppCore core(g_cfg); return core.TuneSystem(highPerf); };
    h.restoreTune = []() { AppCore core(g_cfg); return core.RestoreTune(); };
    h.cleanTemp = []() { return SystemTuner::CleanTemp(); };
    h.receipt = [](const std::string& s) { AddLog(s + "\n"); };
    return h;
}

static ui::PageProcess::ProcSnapshot CollectProcSnapshot() {
    ui::PageProcess::ProcSnapshot snap;
    snap.logicalCores = (g_core != nullptr) ? g_core->Profile().logicalCores : 1;
    if (snap.logicalCores < 1) snap.logicalCores = 1;
    if (g_core == nullptr) return snap;
    for (const auto& entry : g_core->RunningGames()) {
        ui::PageProcess::ProcSample s;
        s.name = gopt::GameIdToString(entry.first);
        s.pid = entry.second;
        HANDLE h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, s.pid);
        if (h != nullptr) {
            s.accessible = true;
            s.priorityClass = GetPriorityClass(h);
            FILETIME c{}, e{}, k{}, u{};
            if (GetProcessTimes(h, &c, &e, &k, &u)) {
                ULARGE_INTEGER kk{}, uu{};
                kk.HighPart = k.dwHighDateTime; kk.LowPart = k.dwLowDateTime;
                uu.HighPart = u.dwHighDateTime; uu.LowPart = u.dwLowDateTime;
                s.cpuTicks = kk.QuadPart + uu.QuadPart;
            }
            PROCESS_MEMORY_COUNTERS pmc{};
            pmc.cb = sizeof(pmc);
            if (GetProcessMemoryInfo(h, &pmc, sizeof(pmc))) s.memBytes = pmc.WorkingSetSize;
            CloseHandle(h);
        }
        snap.items.push_back(s);
    }
    return snap;
}

static ui::PageProcess::OpResult SetProcPriority(uint32_t pid, bool high) {
    ui::PageProcess::OpResult r;
    HANDLE h = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
    if (h == nullptr) {
        r.ok = false;
        r.text = T("无法打开进程（可能已退出或无权限）。", "Cannot open process (exited or no access).");
        return r;
    }
    r.ok = gopt::HAL::SetProcessPriority(h, high ? HIGH_PRIORITY_CLASS : NORMAL_PRIORITY_CLASS);
    CloseHandle(h);
    r.text = std::string(T("进程 ", "process ")) + std::to_string(pid) + " -> " +
             (high ? T("高优先级", "High priority") : T("正常优先级", "Normal priority")) + (r.ok ? "  OK" : "  FAIL");
    return r;
}

static ui::PageProcess::Hooks MakeProcessHooks() {
    ui::PageProcess::Hooks h;
    h.snapshot = []() { return CollectProcSnapshot(); };
    h.setPriority = [](uint32_t pid, bool high) { return SetProcPriority(pid, high); };
    h.receipt = [](const std::string& s) { AddLog(s + "\n"); };
    return h;
}

static std::vector<ui::PageStartup::StartupRowInfo> ListStartupRows() {
    std::vector<ui::PageStartup::StartupRowInfo> out;
    for (const auto& e : StartupManager::List()) {
        ui::PageStartup::StartupRowInfo r;
        r.hive = e.hive;
        r.name = e.name;
        r.value = e.value;
        r.disabled = e.name.rfind("[disabled] ", 0) == 0;
        out.push_back(r);
    }
    return out;
}

static ui::PageStartup::Hooks MakeStartupHooks() {
    ui::PageStartup::Hooks h;
    h.list = []() { return ListStartupRows(); };
    h.disable = [](const std::string& name) {
        ui::PageStartup::OpResult r;
        r.ok = StartupManager::Disable(name);
        r.affected = r.ok ? 1 : 0;
        r.text = std::string(r.ok ? "OK  " : "FAIL  ") + name;
        return r;
    };
    h.enable = [](const std::string& name) {
        ui::PageStartup::OpResult r;
        r.ok = StartupManager::Enable(name);
        r.affected = r.ok ? 1 : 0;
        r.text = std::string(r.ok ? "OK  " : "FAIL  ") + name;
        return r;
    };
    h.restoreAll = []() {
        ui::PageStartup::OpResult r;
        const int n = StartupManager::RestoreAll();
        r.ok = n > 0;
        r.affected = n;
        r.text = std::string(T("已恢复 ", "Restored ")) + std::to_string(n) + T(" 个启动项", " entries");
        return r;
    };
    h.receipt = [](const std::string& s) { AddLog(s + "\n"); };
    return h;
}

// ---------- 托盘 ----------
static void TrayAdd() {
    if (g_trayAdded || g_hwnd == nullptr) return;
    ZeroMemory(&g_tray, sizeof(g_tray));
    g_tray.cbSize = sizeof(g_tray);
    g_tray.hWnd = g_hwnd;
    g_tray.uID = 1;
    g_tray.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    g_tray.uCallbackMessage = WM_TRAY;
    HINSTANCE hi = GetModuleHandleW(nullptr);
    g_tray.hIcon = LoadIconW(hi, MAKEINTRESOURCEW(1));
    if (g_tray.hIcon == nullptr) g_tray.hIcon = LoadIconW(nullptr, MAKEINTRESOURCEW(32512));
    lstrcpynW(g_tray.szTip, L"GameOptimizer", 64);
    g_trayAdded = Shell_NotifyIconW(NIM_ADD, &g_tray) != FALSE;
}

static void TrayRemove() {
    if (g_trayAdded) {
        Shell_NotifyIconW(NIM_DELETE, &g_tray);
        g_trayAdded = false;
    }
}

static void TrayBalloon(const std::wstring& title, const std::wstring& msg) {
    if (!g_trayAdded) return;
    g_tray.uFlags = NIF_INFO;
    g_tray.dwInfoFlags = NIIF_INFO;
    lstrcpynW(g_tray.szInfoTitle, title.c_str(), 64);
    lstrcpynW(g_tray.szInfo, msg.c_str(), 256);
    Shell_NotifyIconW(NIM_MODIFY, &g_tray);
    g_tray.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
}

static void ShowMainWindow(HWND hwnd) {
    ShowWindow(hwnd, SW_SHOW);
    SetForegroundWindow(hwnd);
}

// ---------- 布局（全部按 DPI 缩放） ----------
static void LayoutShell() {
    if (g_hwnd == nullptr) return;
    RECT rc{};
    GetClientRect(g_hwnd, &rc);
    const int pad = ui::UiScale(10);
    const int headerH = ui::UiScale(48);
    const int navW = ui::UiScale(190);
    const int navItemH = ui::UiScale(40);
    const int navGap = ui::UiScale(6);
    const int footerH = ui::UiScale(22);
    const int logH = ui::UiScale(120);
    const int gap = ui::UiScale(8);

    for (int i = 0; i < 5; ++i) {
        if (g_nav[i] != nullptr)
            MoveWindow(g_nav[i], pad, headerH + gap + i * (navItemH + navGap), navW - pad, navItemH, TRUE);
    }
    if (g_lang != nullptr)
        MoveWindow(g_lang, rc.right - ui::UiScale(120), ui::UiScale(9), ui::UiScale(104), ui::UiScale(220), TRUE);

    const int logTop = rc.bottom - footerH - logH - pad;
    if (g_log != nullptr) MoveWindow(g_log, pad, logTop, rc.right - 2 * pad, logH, TRUE);
    if (g_footer != nullptr) MoveWindow(g_footer, pad, rc.bottom - footerH, rc.right - 2 * pad, footerH, TRUE);

    const int px = navW + gap;
    const int py = headerH + gap;
    const int pw = rc.right - px - pad;
    const int ph = logTop - gap - py;
    for (int i = 0; i < 5; ++i) {
        if (g_pages[i] != nullptr)
            MoveWindow(g_pages[i], px, py, pw > 0 ? pw : 0, ph > 0 ? ph : 0, TRUE);
    }
    if (g_flowPanel != nullptr)
        MoveWindow(g_flowPanel, px, py, pw > 0 ? pw : 0, ph > 0 ? ph : 0, TRUE);

    ui::DashboardPageLayout();
    ui::GamePageLayout();
    if (g_pageTune != nullptr) ui::PageTune::FillParent(g_pageTune);
    if (g_pageProcess != nullptr) ui::PageProcess::FillParent(g_pageProcess);
    if (g_pageStartup != nullptr) ui::PageStartup::FillParent(g_pageStartup);
    InvalidateRect(g_hwnd, nullptr, TRUE);
}

static void ShowPage(int page) {
    g_page = page;
    for (int i = 0; i < 5; ++i)
        if (g_pages[i] != nullptr) ShowWindow(g_pages[i], i == page ? SW_SHOW : SW_HIDE);
    if (g_pageTune != nullptr) ui::PageTune::Show(g_pageTune, page == 2);
    if (g_pageProcess != nullptr) ui::PageProcess::Show(g_pageProcess, page == 3);
    if (g_pageStartup != nullptr) ui::PageStartup::Show(g_pageStartup, page == 4);
    if (page == 0) ui::DashboardPageOnShow();
    if (page == 1) ui::GamePageOnShow();
    for (int i = 0; i < 5; ++i)
        if (g_nav[i] != nullptr) InvalidateRect(g_nav[i], nullptr, TRUE);
    InvalidateRect(g_hwnd, nullptr, TRUE);
}

static void ApplyLanguageAll() {
    ui::DashboardPageApplyLanguage();
    ui::GamePageApplyLanguage();
    if (g_pageTune != nullptr) ui::PageTune::ApplyLabels(g_pageTune);
    if (g_pageProcess != nullptr) ui::PageProcess::ApplyLabels(g_pageProcess);
    if (g_pageStartup != nullptr) ui::PageStartup::ApplyLabels(g_pageStartup);
    for (int i = 0; i < 5; ++i)
        if (g_nav[i] != nullptr) InvalidateRect(g_nav[i], nullptr, TRUE);
    InvalidateRect(g_hwnd, nullptr, TRUE);
}

static void ApplyThemeAll() {
    if (g_brushPanel != nullptr) DeleteObject(g_brushPanel);
    g_brushPanel = CreateSolidBrush(ui::UiColor(ui::UiColorRole::WindowBg));
    ui::DashboardPageApplyTheme();
    ui::GamePageApplyTheme();
    if (g_pageTune != nullptr) { ui::PageTune::ApplyLabels(g_pageTune); InvalidateRect(g_pageTune, nullptr, TRUE); }
    if (g_pageProcess != nullptr) InvalidateRect(g_pageProcess, nullptr, TRUE);
    if (g_pageStartup != nullptr) InvalidateRect(g_pageStartup, nullptr, TRUE);
    if (g_flowPanel != nullptr) InvalidateRect(g_flowPanel, nullptr, TRUE);
    if (g_log != nullptr) InvalidateRect(g_log, nullptr, TRUE);
    if (g_footer != nullptr) InvalidateRect(g_footer, nullptr, TRUE);
    InvalidateRect(g_hwnd, nullptr, TRUE);
}

// ---------- 流程面板绘制 ----------
static void DrawFlowPanel(HDC dc, const RECT& pr) {
    HBRUSH bg = CreateSolidBrush(ui::UiColor(ui::UiColorRole::Surface));
    FillRect(dc, &pr, bg);
    DeleteObject(bg);
    HBRUSH frame = CreateSolidBrush(ui::UiColor(ui::UiColorRole::Accent));
    FrameRect(dc, &pr, frame);
    DeleteObject(frame);
    SetBkMode(dc, TRANSPARENT);

    ui::UiSelectFont(dc, ui::UiFontRole::Title);
    SetTextColor(dc, ui::UiColor(ui::UiColorRole::Accent));
    RECT tr{pr.left + ui::UiScale(14), pr.top + ui::UiScale(8), pr.right - ui::UiScale(60), pr.top + ui::UiScale(30)};
    DrawTextW(dc, Utf8ToWide(T("优化流程", "OPERATION")).c_str(), -1, &tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

    {  // 旋转指示器
        const int cx = pr.right - ui::UiScale(24), cy = pr.top + ui::UiScale(20), r = ui::UiScale(8);
        HPEN ring = CreatePen(PS_SOLID, 1, ui::UiColor(ui::UiColorRole::Border));
        HPEN oldPen = static_cast<HPEN>(SelectObject(dc, ring));
        HBRUSH oldBr = static_cast<HBRUSH>(SelectObject(dc, GetStockObject(NULL_BRUSH)));
        Ellipse(dc, cx - r, cy - r, cx + r, cy + r);
        SelectObject(dc, oldPen);
        DeleteObject(ring);
        HPEN arc = CreatePen(PS_SOLID, 2, ui::UiColor(ui::UiColorRole::Accent));
        oldPen = static_cast<HPEN>(SelectObject(dc, arc));
        const double a0 = g_flowAngle * 3.14159265358979 / 180.0;
        POINT pts[3] = {
            {cx + static_cast<int>(r * 0.95 * std::cos(a0)), cy + static_cast<int>(r * 0.95 * std::sin(a0))},
            {cx + static_cast<int>(r * 0.95 * std::cos(a0 + 1.2)), cy + static_cast<int>(r * 0.95 * std::sin(a0 + 1.2))},
            {cx, cy}};
        Polygon(dc, pts, 3);
        SelectObject(dc, oldPen);
        SelectObject(dc, oldBr);
        DeleteObject(arc);
    }

    ui::UiSelectFont(dc, ui::UiFontRole::Body);
    int y = pr.top + ui::UiScale(34);
    const int rowH = ui::UiScale(22);
    for (size_t i = 0; i < g_flowLines.size(); ++i) {
        if (y + rowH > pr.bottom - ui::UiScale(26)) break;
        const FlowLineUI& row = g_flowLines[i];
        COLORREF fg = ui::UiColor(ui::UiColorRole::TextPrimary);
        if (row.tone == 1) fg = ui::UiColor(ui::UiColorRole::Success);
        else if (row.tone == 2) fg = ui::UiColor(ui::UiColorRole::Danger);
        RECT lr{pr.left + ui::UiScale(30), y, pr.right - ui::UiScale(14), y + rowH};
        SetTextColor(dc, fg);
        DrawTextW(dc, Utf8ToWide(row.text).c_str(), -1, &lr,
                  DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
        const int nx = pr.left + ui::UiScale(16), ny = y + rowH / 2;
        HBRUSH dot = CreateSolidBrush(fg);
        HBRUSH oldB = static_cast<HBRUSH>(SelectObject(dc, dot));
        HPEN noPen = static_cast<HPEN>(SelectObject(dc, GetStockObject(NULL_PEN)));
        Ellipse(dc, nx - ui::UiScale(3), ny - ui::UiScale(3), nx + ui::UiScale(3), ny + ui::UiScale(3));
        SelectObject(dc, noPen);
        SelectObject(dc, oldB);
        DeleteObject(dot);
        y += rowH;
    }

    RECT info{pr.left + ui::UiScale(14), pr.bottom - ui::UiScale(22), pr.right - ui::UiScale(14), pr.bottom - ui::UiScale(4)};
    ui::UiSelectFont(dc, ui::UiFontRole::Caption);
    SetTextColor(dc, g_flowHasFail ? ui::UiColor(ui::UiColorRole::Danger) : ui::UiColor(ui::UiColorRole::TextMuted));
    DrawTextW(dc,
              Utf8ToWide(g_flowHasFail ? T("存在失败项（可随时回滚）", "has failures (rollback anytime)")
                                       : T("实时反馈；完成后自动隐藏；可随时回滚", "live feedback; auto-hide when done; rollback anytime"))
                  .c_str(),
              -1, &info, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
}

// ---------- 窗口过程 ----------
static WNDPROC g_pageProcOld[5] = {};

static LRESULT CALLBACK PageProc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_COMMAND) return SendMessageW(GetParent(h), m, w, l);
    int idx = -1;
    for (int i = 0; i < 5; ++i)
        if (g_pages[i] == h) { idx = i; break; }
    if (idx >= 0 && g_pageProcOld[idx] != nullptr) return CallWindowProcW(g_pageProcOld[idx], h, m, w, l);
    return DefWindowProcW(h, m, w, l);
}

static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_CREATE: {
            const HINSTANCE hInst = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
            g_hwnd = hwnd;
            // 主题令牌默认按「系统 DPI」生成；本窗口可能在更高 DPI 的显示器上（本机 144），
            // 这里先按窗口 DPI 同步一次，保证外壳布局与页面模块使用同一套缩放。
            ui::UiOnDpiChanged(ui::UiDpiForWindow(hwnd));
            g_font = ui::UiFont(ui::UiFontRole::Body);
            g_brushPanel = CreateSolidBrush(ui::UiColor(ui::UiColorRole::WindowBg));

            auto makeCtl = [&](HWND parent, const wchar_t* cls, const wchar_t* text, DWORD style, int id) {
                HWND c = CreateWindowExW(0, cls, text, style | WS_CHILD | WS_VISIBLE, 0, 0, 10, 10, parent,
                                         reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), hInst, nullptr);
                if (c != nullptr && g_font != nullptr) SendMessageW(c, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);
                return c;
            };

            g_lang = makeCtl(hwnd, L"COMBOBOX", L"", CBS_DROPDOWNLIST, IDC_LANG);
            SendMessageW(g_lang, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(L"中文"));
            SendMessageW(g_lang, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(L"English"));
            SendMessageW(g_lang, CB_SETCURSEL, 0, 0);

            for (int i = 0; i < 5; ++i) {
                g_nav[i] = makeCtl(hwnd, L"BUTTON", L"", BS_OWNERDRAW, IDC_NAV0 + i);
                if (g_nav[i] != nullptr) ui::PageEnableButtonHover(g_nav[i], false);
            }

            for (int i = 0; i < 5; ++i) {
                g_pages[i] = CreateWindowExW(0, L"STATIC", L"", WS_CHILD | WS_CLIPSIBLINGS, 0, 0, 10, 10, hwnd,
                                             nullptr, hInst, nullptr);
                if (g_pages[i] != nullptr)
                    g_pageProcOld[i] = reinterpret_cast<WNDPROC>(
                        SetWindowLongPtrW(g_pages[i], GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(PageProc)));
            }

            g_log = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"",
                                    WS_CHILD | WS_VISIBLE | ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY | WS_VSCROLL,
                                    0, 0, 10, 10, hwnd, nullptr, hInst, nullptr);
            if (g_log != nullptr) SendMessageW(g_log, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);
            g_footer = makeCtl(hwnd, L"STATIC", L"", 0, 0);

            g_flowPanel = CreateWindowExW(0, L"STATIC", L"", WS_CHILD | SS_OWNERDRAW, 0, 0, 10, 10, hwnd,
                                          reinterpret_cast<HMENU>(static_cast<INT_PTR>(ID_FLOWPANEL)), hInst, nullptr);
            ShowWindow(g_flowPanel, SW_HIDE);

            const ui::PageHostHooks pageHooks = MakePageHostHooks();
            ui::DashboardPageCreate(g_pages[0], pageHooks);
            ui::GamePageCreate(g_pages[1], pageHooks);

            RECT prc{};
            GetClientRect(hwnd, &prc);
            const ui::PageTune::Hooks tuneHooks = MakeTuneHooks();
            const ui::PageProcess::Hooks procHooks = MakeProcessHooks();
            const ui::PageStartup::Hooks startHooks = MakeStartupHooks();
            g_pageTune = ui::PageTune::Create(g_pages[2], prc, tuneHooks);
            g_pageProcess = ui::PageProcess::Create(g_pages[3], prc, procHooks);
            g_pageStartup = ui::PageStartup::Create(g_pages[4], prc, startHooks);

            TrayAdd();
            LayoutShell();
            ShowPage(0);
            SetTimer(hwnd, IDT_FLOW, 1000, nullptr);
            AddLog(std::string("GameOptimizer v") + GOPT_VERSION_STR + T("  ·  所有功能免费\n", "  ·  all features free\n"));
        } break;

        case WM_SIZE:
            LayoutShell();
            break;

        case WM_DPICHANGED: {
            const RECT* sug = reinterpret_cast<const RECT*>(lp);
            if (sug != nullptr)
                SetWindowPos(hwnd, nullptr, sug->left, sug->top, sug->right - sug->left, sug->bottom - sug->top,
                             SWP_NOZORDER | SWP_NOACTIVATE);
            ui::UiOnDpiChanged(HIWORD(wp));
            g_font = ui::UiFont(ui::UiFontRole::Body);
            if (g_log != nullptr) SendMessageW(g_log, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);
            if (g_footer != nullptr) SendMessageW(g_footer, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);
            if (g_lang != nullptr) SendMessageW(g_lang, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);
            ApplyThemeAll();
            LayoutShell();
        } break;

        case WM_THEMECHANGED:
            ui::UiThemeRefresh();
            ApplyThemeAll();
            LayoutShell();
            break;

        case WM_DRAWITEM: {
            auto* dis = reinterpret_cast<DRAWITEMSTRUCT*>(lp);
            if (dis == nullptr) break;
            if (dis->CtlID >= IDC_NAV0 && dis->CtlID <= IDC_NAV4) {
                const int page = static_cast<int>(dis->CtlID - IDC_NAV0);
                static const char* zh[5] = {"总览", "游戏优化", "系统调优", "进程", "启动项"};
                static const char* en[5] = {"Dashboard", "Game Tune", "System Tune", "Processes", "Startup"};
                ui::UiDrawButton(dis->hDC, dis->rcItem, ui::UiButtonStateFromDrawItem(dis), T(zh[page], en[page]),
                                 page == g_page);
                return TRUE;
            }
            if (dis->CtlID == ID_FLOWPANEL) {
                DrawFlowPanel(dis->hDC, dis->rcItem);
                return TRUE;
            }
        } break;

        case WM_CTLCOLORSTATIC:
        case WM_CTLCOLOREDIT:
        case WM_CTLCOLORLISTBOX: {
            HDC dc = reinterpret_cast<HDC>(wp);
            if (dc != nullptr) {
                SetTextColor(dc, ui::UiColor(ui::UiColorRole::TextPrimary));
                SetBkColor(dc, ui::UiColor(ui::UiColorRole::WindowBg));
            }
            if (g_brushPanel != nullptr) return reinterpret_cast<LRESULT>(g_brushPanel);
        } break;

        case WM_ERASEBKGND:
            return 1;

        case WM_PAINT: {
            PAINTSTRUCT ps{};
            HDC dc = BeginPaint(hwnd, &ps);
            RECT rc{};
            GetClientRect(hwnd, &rc);
            HBRUSH bg = CreateSolidBrush(ui::UiColor(ui::UiColorRole::WindowBg));
            FillRect(dc, &rc, bg);
            DeleteObject(bg);
            RECT hdr{0, 0, rc.right, ui::UiScale(48)};
            HBRUSH hb = CreateSolidBrush(ui::UiColor(ui::UiColorRole::Accent));
            FillRect(dc, &hdr, hb);
            DeleteObject(hb);
            RECT side{0, ui::UiScale(48), ui::UiScale(190), rc.bottom};
            HBRUSH sb = CreateSolidBrush(ui::UiColor(ui::UiColorRole::PanelBg));
            FillRect(dc, &side, sb);
            DeleteObject(sb);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, ui::UiColor(ui::UiColorRole::TextOnAccent));
            ui::UiSelectFont(dc, ui::UiFontRole::Title);
            RECT tr{ui::UiScale(12), 0, ui::UiScale(360), ui::UiScale(48)};
            DrawTextW(dc, L"GameOptimizer", -1, &tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            EndPaint(hwnd, &ps);
        } break;

        case WM_COMMAND: {
            const int id = LOWORD(wp);
            const int code = HIWORD(wp);
            if (id == IDC_LANG && code == CBN_SELCHANGE) {
                SetLang(SendMessageW(g_lang, CB_GETCURSEL, 0, 0) == 1 ? Lang::En : Lang::Zh);
                ApplyLanguageAll();
                return 0;
            }
            if (code == BN_CLICKED && id >= IDC_NAV0 && id <= IDC_NAV4) {
                ShowPage(id - IDC_NAV0);
                return 0;
            }
            // 页面控件通知由页面面板就地分发；页容器转发的未知 ID 一律忽略
        } break;

        case WM_TIMER:
            if (wp == static_cast<WPARAM>(IDT_FLOW)) {
                if (g_flowActive) {
                    g_flowAngle = (g_flowAngle + 4) % 360;
                    if (g_flowPanel != nullptr) InvalidateRect(g_flowPanel, nullptr, FALSE);
                    if (GetTickCount64() - g_flowLastTick > 6000) FlowEnd();
                }
            }
            return 0;

        case WM_TRAY:
            switch (static_cast<int>(lp)) {
                case WM_LBUTTONDBLCLK:
                    ShowMainWindow(hwnd);
                    break;
                case WM_RBUTTONUP: {
                    POINT pt{};
                    GetCursorPos(&pt);
                    HMENU menu = CreatePopupMenu();
                    AppendMenuW(menu, MF_STRING, 1, L"打开主界面 (Open)");
                    AppendMenuW(menu, MF_STRING, 4, L"清理临时文件 (Clean Temp)");
                    AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
                    AppendMenuW(menu, MF_STRING, 2, L"退出 (Exit)");
                    SetForegroundWindow(hwnd);
                    const int cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, nullptr);
                    DestroyMenu(menu);
                    if (cmd == 1) {
                        ShowMainWindow(hwnd);
                    } else if (cmd == 4) {
                        ShowMainWindow(hwnd);
                        AddLog(std::string(T("== 临时文件清理 ==\n", "== Temp clean ==\n")));
                        AddLog(SystemTuner::CleanTemp());
                        AddLog("\n\n");
                    } else if (cmd == 2) {
                        TrayRemove();
                        DestroyWindow(hwnd);
                    }
                } break;
                default:
                    break;
            }
            return 0;

        case WM_CLOSE:
            if (g_trayAdded) {
                ShowWindow(hwnd, SW_HIDE);
                if (!g_trayBalloonShown) {
                    g_trayBalloonShown = true;
                    TrayBalloon(L"GameOptimizer",
                                Utf8ToWide(T("仍在后台运行（看门狗生效中）。双击图标恢复窗口，右键可退出。",
                                             "Still running in background (watchdog active). Double-click to restore; right-click to exit.")));
                }
                return 0;
            }
            DestroyWindow(hwnd);
            return 0;

        case WM_DESTROY:
            KillTimer(hwnd, IDT_FLOW);
            ui::DashboardPageDestroy();
            ui::GamePageDestroy();
            if (g_pageTune != nullptr) { ui::PageTune::Destroy(g_pageTune); g_pageTune = nullptr; }
            if (g_pageProcess != nullptr) { ui::PageProcess::Destroy(g_pageProcess); g_pageProcess = nullptr; }
            if (g_pageStartup != nullptr) { ui::PageStartup::Destroy(g_pageStartup); g_pageStartup = nullptr; }
            TrayRemove();
            PostQuitMessage(0);
            break;

        default:
            break;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

int WINAPI WinMain(HINSTANCE hInst, HINSTANCE, LPSTR, int nShow) {
    g_core = new AppCore();
    g_cfg.allowPowerSchemeSwitch = true;  // 用户在系统调优页显式调优时允许切换电源方案

    ui::UiEnablePerMonitorDpi();
    ui::UiThemeInit(true);

    const wchar_t cls[] = L"gopt_gui";
    WNDCLASSW wc{};
    wc.lpfnWndProc = WndProc;
    wc.hInstance = hInst;
    wc.lpszClassName = cls;
    wc.hCursor = LoadCursorW(nullptr, reinterpret_cast<LPCWSTR>(IDC_ARROW));
    wc.hbrBackground = nullptr;
    RegisterClassW(&wc);

    HANDLE hMutex = CreateMutexW(nullptr, FALSE, L"GameOptimizer_SingleInstance");
    if (hMutex != nullptr && GetLastError() == ERROR_ALREADY_EXISTS) {
        HWND prev = FindWindowW(cls, nullptr);
        if (prev != nullptr) {
            ShowWindow(prev, SW_RESTORE);
            SetForegroundWindow(prev);
        }
        CloseHandle(hMutex);
        delete g_core;
        return 0;
    }

    RECT wa{};
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &wa, 0);
    const int margin = ui::UiScale(16);
    int winW = (wa.right - wa.left) - margin * 2;
    int winH = (wa.bottom - wa.top) - margin * 2;
    if (winW < ui::UiScale(900)) winW = ui::UiScale(900);
    if (winH < ui::UiScale(600)) winH = ui::UiScale(600);

    HWND hwnd = CreateWindowExW(0, cls, L"GameOptimizer v" GOPT_VERSION_STR, WS_OVERLAPPEDWINDOW,
                                wa.left + margin, wa.top + margin, winW, winH, nullptr, nullptr, hInst, nullptr);
    if (hwnd == nullptr) {
        delete g_core;
        return 0;
    }
    ShowWindow(hwnd, nShow);
    UpdateWindow(hwnd);

    MSG msg{};
    while (GetMessageW(&msg, nullptr, 0, 0)) {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    ui::UiFontShutdown();
    if (g_brushPanel != nullptr) { DeleteObject(g_brushPanel); g_brushPanel = nullptr; }
    if (hMutex != nullptr) CloseHandle(hMutex);
    delete g_core;
    return 0;
}
