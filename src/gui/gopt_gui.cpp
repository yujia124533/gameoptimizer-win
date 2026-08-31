// GameOptimizer 原生 GUI（v2 重构版：多面板现代布局，参考 Wise365 / Process Lasso / BoosterX）
// 布局：顶部标题栏 · 左侧导航(总览/游戏优化/系统调优/进程/启动项) · 内容区 · 底部日志 + 状态栏
#include <windows.h>
#include <commdlg.h>

#include <string>
#include <thread>
#include <tuple>
#include <vector>

#include "config/GameConfig.h"
#include "core/AppCore.h"
#include "hal/HAL.h"
#include "i18n.h"
#include "license/License.h"
#include "tuning/StartupManager.h"
#include "tuning/SystemTuner.h"
#include "version.h"

using gopt::AppConfig;
using gopt::AppCore;
using gopt::GameConfig;
using gopt::GameId;
using gopt::GameLaunchConfig;
using gopt::Lang;
using gopt::StartupManager;
using gopt::SetLang;
using gopt::SystemTuner;
using gopt::T;

// ---------- 控件 / 常量 ----------
static AppCore* g_core = nullptr;
static HWND g_hwnd = nullptr, g_footer = nullptr, g_log = nullptr;
static HWND g_lang = nullptr;
static HWND g_pages[5] = {};
static HWND g_nav[5] = {};
static HWND g_bigOpt = nullptr;                        // 总览：一键优化大按钮
// 游戏优化页
static HWND g_combo, g_path, g_args, g_power, g_btnSave, g_btnBrowse, g_btnApply, g_btnRollback;
// 系统调优页
static HWND g_btnTuneHigh, g_btnTuneBal, g_btnTuneRestore;
// 进程页
static HWND g_listProc, g_btnProcRefresh;
// 启动项页
static HWND g_listStartup, g_btnStartupRefresh, g_btnStartupDisable, g_btnStartupEnable, g_btnStartupRestore;
// 总览卡片
static HWND g_dashHelp, g_dashCpu, g_dashGpu, g_dashRam, g_dashNote;
// 每页操作提示
static HWND g_hint1, g_hint2, g_hint3, g_hint4;

static int g_page = 0;
static HFONT g_font = nullptr, g_fontBold = nullptr, g_fontBig = nullptr;
static HBRUSH g_whiteBrush = nullptr;
static double g_progress = 0.0;
static int g_pendingStart = 0, g_pendingLen = 0;
static std::vector<gopt::StartupEntry> g_startups;
static std::vector<std::pair<GameId, uint32_t>> g_procs;

enum {
    IDC_LANG = 106,
    IDC_NAV0 = 201, IDC_NAV1 = 202, IDC_NAV2 = 203, IDC_NAV3 = 204, IDC_NAV4 = 205,
    IDC_BIGOPT = 301,
    IDC_COMBO = 302, IDC_PATH = 303, IDC_BROWSE = 304, IDC_ARGS = 305, IDC_SAVE = 306,
    IDC_POWERCHK = 307, IDC_APPLY = 308, IDC_ROLLBACK = 309,
    IDC_TUNE_HIGH = 401, IDC_TUNE_BAL = 402, IDC_TUNE_RESTORE = 403,
    IDC_PROCLIST = 501, IDC_PROC_REFRESH = 502,
    IDC_STARTUP_LIST = 601, IDC_STARTUP_REFRESH = 602, IDC_STARTUP_DISABLE = 603,
    IDC_STARTUP_ENABLE = 604, IDC_STARTUP_RESTORE = 605,
};

#define WM_APP_UIEVENT (WM_APP + 1)
#define WM_APP_FINISH  (WM_APP + 2)
struct PendingFlow { gopt::AppCore::FlowEvent e; };

// ---------- 工具 ----------
static std::wstring Utf8ToWide(const std::string& s) {
    if (s.empty()) return {};
    const int len = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
    if (len <= 1) return {};
    std::wstring w(static_cast<size_t>(len) - 1, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, &w[0], len);
    return w;
}

static std::string WideToUtf8(HWND h) {
    wchar_t buf[MAX_PATH] = {};
    GetWindowTextW(h, buf, MAX_PATH);
    const int len = WideCharToMultiByte(CP_UTF8, 0, buf, -1, nullptr, 0, nullptr, nullptr);
    if (len <= 1) return {};
    std::string s(static_cast<size_t>(len) - 1, '\0');
    WideCharToMultiByte(CP_UTF8, 0, buf, -1, &s[0], len, nullptr, nullptr);
    return s;
}

// 双语标签表：语言切换时统一刷新
static std::vector<std::tuple<HWND, std::string, std::string>> g_labels;
static void Label(HWND h, const char* zh, const char* en) {
    g_labels.emplace_back(h, zh, en);
    SetWindowTextW(h, Utf8ToWide(T(zh, en)).c_str());
}
static void ApplyAllLabels() {
    for (auto& [h, zh, en] : g_labels) SetWindowTextW(h, Utf8ToWide(T(zh.c_str(), en.c_str())).c_str());
}

static void AddLog(const std::string& s) {
    const std::wstring w = Utf8ToWide(s);
    const int len = GetWindowTextLengthW(g_log);
    SendMessageW(g_log, EM_SETSEL, len, len);
    SendMessageW(g_log, EM_REPLACESEL, FALSE, reinterpret_cast<LPARAM>(w.c_str()));
    SendMessageW(g_log, EM_SCROLLCARET, 0, 0);
}

static void RenderFlowEvent(const gopt::AppCore::FlowEvent& e) {
    switch (e.kind) {
        case gopt::AppCore::FlowEvent::Info:
            AddLog(e.text + "\n");
            break;
        case gopt::AppCore::FlowEvent::StepStart: {
            const int start = GetWindowTextLengthW(g_log);
            g_pendingStart = start;
            AddLog("  " + std::to_string(e.step) + ") " + e.text + ": "
                   + T("处理中...", "running...") + "\n");
            g_pendingLen = GetWindowTextLengthW(g_log) - start;
            break;
        }
        case gopt::AppCore::FlowEvent::StepOk:
        case gopt::AppCore::FlowEvent::StepFail: {
            const std::string line = "  " + std::to_string(e.step) + ") " + e.text + ": "
                + (e.kind == gopt::AppCore::FlowEvent::StepOk ? T("成功 ✓", "OK ✓") : T("失败 ✗", "FAIL ✗"))
                + "（" + std::to_string(e.elapsedMs) + " ms）\n";
            SendMessageW(g_log, EM_SETSEL, g_pendingStart, g_pendingStart + g_pendingLen);
            SendMessageW(g_log, EM_REPLACESEL, FALSE,
                         reinterpret_cast<LPARAM>(Utf8ToWide(line).c_str()));
            break;
        }
    }
    if (e.total > 0) g_progress = static_cast<double>(e.step) / static_cast<double>(e.total);
    if (g_hwnd != nullptr) InvalidateRect(g_hwnd, nullptr, FALSE);
}

static void UpdateFooter() {
    if (!g_footer || !g_core) return;
    const gopt::HardwareProfile p = g_core->Profile();
    GUID scheme{};
    std::string power = "?";
    if (gopt::HAL::QueryActivePowerScheme(&scheme)) power = gopt::HAL::PowerSchemeName(scheme);
    const std::string txt = std::string(T("全部免费", "Free")) + " · " + T("内存", "RAM") + " "
        + std::to_string(p.availableRamMB / 1024) + "/" + std::to_string(p.systemRamMB / 1024)
        + " GB · " + T("电源", "Power") + " " + power
        + (g_procs.empty() ? "" : (std::string(" · ") + T("运行中", "Running") + " " + std::to_string(g_procs.size())));
    SetWindowTextW(g_footer, Utf8ToWide(txt).c_str());
}

// ---------- 页面逻辑 ----------
static GameId CurrentGame() {
    const int idx = static_cast<int>(SendMessageW(g_combo, CB_GETCURSEL, 0, 0));
    static const GameId kGames[] = {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                                    GameId::PUBG, GameId::Valorant, GameId::Apex,
                                    GameId::Dota2, GameId::Overwatch2};
    if (idx < 0 || idx >= 8) return GameId::DeltaForce;
    return kGames[idx];
}

static void LoadGameConfigToUI() {
    const GameLaunchConfig gc = GameConfig::Get(CurrentGame());
    SetWindowTextW(g_path, Utf8ToWide(gc.exePath).c_str());
    SetWindowTextW(g_args, Utf8ToWide(gc.args).c_str());
    SendMessageW(g_power, BM_SETCHECK, gc.powerScheme ? BST_CHECKED : BST_UNCHECKED, 0);
}

static void RefreshGameList() {
    SendMessageW(g_combo, CB_RESETCONTENT, 0, 0);
    const GameId kGames[] = {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                             GameId::PUBG, GameId::Valorant, GameId::Apex,
                             GameId::Dota2, GameId::Overwatch2};
    for (const GameId id : kGames)
        SendMessageW(g_combo, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(Utf8ToWide(gopt::GameIdToString(id)).c_str()));
    SendMessageW(g_combo, CB_SETCURSEL, 0, 0);
    LoadGameConfigToUI();
}

static AppCore* MakeCore() {
    AppConfig cfg;
    cfg.gameExeOverride = WideToUtf8(g_path);
    cfg.allowPowerSchemeSwitch = SendMessageW(g_power, BM_GETCHECK, 0, 0) == BST_CHECKED;
    return new AppCore(cfg);
}

static void BrowsePath() {
    OPENFILENAMEW ofn{};
    wchar_t file[MAX_PATH] = {};
    ofn.lStructSize = sizeof(ofn);
    ofn.hwndOwner = g_hwnd;
    ofn.lpstrFilter = L"*.exe\0*.exe\0*.*\0*.*\0";
    ofn.lpstrFile = file;
    ofn.nMaxFile = MAX_PATH;
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;
    if (GetOpenFileNameW(&ofn)) SetWindowTextW(g_path, file);
}

static void SaveCurrentGameConfig() {
    GameLaunchConfig gc = GameConfig::Get(CurrentGame());
    gc.exePath = WideToUtf8(g_path);
    gc.args = WideToUtf8(g_args);
    gc.powerScheme = SendMessageW(g_power, BM_GETCHECK, 0, 0) == BST_CHECKED;
    GameConfig::Set(CurrentGame(), gc);
    AddLog(std::string(T("已保存 ", "Saved ")) + gopt::GameIdToString(CurrentGame())
           + T(" 的优化启动配置。\n\n", " launch config.\n\n"));
}

static void RefreshProcList() {
    g_procs = g_core->RunningGames();
    SendMessageW(g_listProc, LB_RESETCONTENT, 0, 0);
    if (g_procs.empty()) {
        SendMessageW(g_listProc, LB_ADDSTRING, 0,
                     reinterpret_cast<LPARAM>(Utf8ToWide(T("（没有运行中的支持游戏）", "(no supported games running)")).c_str()));
    } else {
        for (const auto& [id, pid] : g_procs)
            SendMessageW(g_listProc, LB_ADDSTRING, 0,
                         reinterpret_cast<LPARAM>(Utf8ToWide(gopt::GameIdToString(id) + "  (pid " + std::to_string(pid) + ")").c_str()));
    }
    SendMessageW(g_listProc, LB_SETCURSEL, 0, 0);
    UpdateFooter();
}

static void RefreshStartupList() {
    g_startups = StartupManager::List();
    SendMessageW(g_listStartup, LB_RESETCONTENT, 0, 0);
    if (g_startups.empty()) {
        SendMessageW(g_listStartup, LB_ADDSTRING, 0,
                     reinterpret_cast<LPARAM>(Utf8ToWide(T("（没有启动项）", "(no startup entries)")).c_str()));
    } else {
        for (const auto& e : g_startups)
            SendMessageW(g_listStartup, LB_ADDSTRING, 0,
                         reinterpret_cast<LPARAM>(Utf8ToWide("[" + e.hive + "] " + e.name).c_str()));
    }
    SendMessageW(g_listStartup, LB_SETCURSEL, 0, 0);
}

static void UpdateDashboard() {
    if (!g_core || !g_dashCpu) return;
    const gopt::HardwareProfile p = g_core->Profile();
    SetWindowTextW(g_dashCpu, Utf8ToWide("CPU: " + p.cpuModel + "（" + std::to_string(p.physicalCores)
        + " " + T("物理核", "cores") + " / " + std::to_string(p.logicalCores) + " "
        + T("逻辑", "threads") + " @ " + std::to_string(p.cpuBaseFreqMHz) + " MHz）").c_str());
    SetWindowTextW(g_dashGpu, Utf8ToWide("GPU: " + p.gpuVendor + " " + p.gpuModel + "（"
        + std::to_string(p.vramMB / 1024) + " GB，Driver " + p.gpuDriverVersion + "）").c_str());
    SetWindowTextW(g_dashRam, Utf8ToWide("RAM: " + std::to_string(p.systemRamMB / 1024)
        + " GB（" + T("可用", "free") + " " + std::to_string(p.availableRamMB / 1024) + " GB）").c_str());
    UpdateFooter();
}

static void ShowPage(int page) {
    g_page = page;
    for (int i = 0; i < 5; ++i)
        if (g_pages[i] != nullptr) ShowWindow(g_pages[i], i == page ? SW_SHOW : SW_HIDE);
    if (g_hwnd != nullptr) InvalidateRect(g_hwnd, nullptr, TRUE);
}

// ---------- 窗口过程 ----------
static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_CREATE: {
            const HINSTANCE hInst = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
            g_hwnd = hwnd;
            g_whiteBrush = CreateSolidBrush(RGB(255, 255, 255));
            g_font = CreateFontW(-15, 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
                                 DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                                 CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Microsoft YaHei UI");
            g_fontBold = CreateFontW(-15, 0, 0, 0, FW_BOLD, FALSE, FALSE, FALSE,
                                     DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                                     CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Microsoft YaHei UI");
            g_fontBig = CreateFontW(-22, 0, 0, 0, FW_BOLD, FALSE, FALSE, FALSE,
                                    DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                                    CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Microsoft YaHei UI");

            auto makeCtl = [&](HWND parent, const wchar_t* cls, const wchar_t* text, DWORD style,
                               int x, int y, int w, int h, int id, DWORD ex = 0) {
                HWND c = CreateWindowExW(ex, cls, text, style | WS_CHILD | WS_VISIBLE,
                                         x, y, w, h, parent, reinterpret_cast<HMENU>(id), hInst, nullptr);
                if (c && g_font) SendMessageW(c, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);
                return c;
            };

            // ----- 顶部：语言 -----
            g_lang = makeCtl(hwnd, L"COMBOBOX", L"", CBS_DROPDOWNLIST, 760, 12, 90, 120, IDC_LANG);
            SendMessageW(g_lang, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(L"中文"));
            SendMessageW(g_lang, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(L"English"));
            SendMessageW(g_lang, CB_SETCURSEL, 0, 0);

            // ----- 左侧导航 -----
            const wchar_t* navNames[5] = {L"", L"", L"", L"", L""};
            for (int i = 0; i < 5; ++i) {
                g_nav[i] = makeCtl(hwnd, L"BUTTON", L"", BS_OWNERDRAW, 12, 60 + i * 48, 192, 40, IDC_NAV0 + i);
                (void)navNames;
            }
            // ----- 页面容器 -----
            for (int i = 0; i < 5; ++i)
                g_pages[i] = CreateWindowExW(0, L"STATIC", L"", WS_CHILD | WS_CLIPSIBLINGS,
                                             222, 62, 770, 320, hwnd, nullptr, hInst, nullptr);

            // 【页0 总览】使用引导 + 硬件信息卡 + 一键大按钮
            HWND p = g_pages[0];
            g_dashHelp = makeCtl(p, L"STATIC", L"", 0, 18, 12, 720, 24, 0);
            g_dashCpu = makeCtl(p, L"STATIC", L"", 0, 18, 46, 720, 26, 0);
            g_dashGpu = makeCtl(p, L"STATIC", L"", 0, 18, 80, 720, 26, 0);
            g_dashRam = makeCtl(p, L"STATIC", L"", 0, 18, 114, 720, 26, 0);
            g_bigOpt = makeCtl(p, L"BUTTON", L"", BS_PUSHBUTTON, 18, 158, 220, 54, IDC_BIGOPT);
            SendMessageW(g_bigOpt, WM_SETFONT, reinterpret_cast<WPARAM>(g_fontBig), TRUE);
            g_dashNote = makeCtl(p, L"STATIC", L"", 0, 18, 226, 720, 22, 0);
            // 【页1 游戏优化】
            p = g_pages[1];
            g_combo = makeCtl(p, L"COMBOBOX", L"", CBS_DROPDOWNLIST, 18, 16, 200, 200, IDC_COMBO);
            g_path = makeCtl(p, L"EDIT", L"", 0, 18, 54, 200, 26, IDC_PATH, WS_EX_CLIENTEDGE);
            g_btnBrowse = makeCtl(p, L"BUTTON", L"", 0, 226, 54, 60, 26, IDC_BROWSE);
            g_args = makeCtl(p, L"EDIT", L"", 0, 18, 92, 200, 26, IDC_ARGS, WS_EX_CLIENTEDGE);
            g_power = makeCtl(p, L"BUTTON", L"", BS_AUTOCHECKBOX, 18, 130, 300, 22, IDC_POWERCHK);
            g_btnApply = makeCtl(p, L"BUTTON", L"", 0, 18, 170, 100, 30, IDC_APPLY);
            g_btnRollback = makeCtl(p, L"BUTTON", L"", 0, 128, 170, 100, 30, IDC_ROLLBACK);
            g_btnSave = makeCtl(p, L"BUTTON", L"", 0, 238, 170, 100, 30, IDC_SAVE);
            SendMessageW(g_btnApply, WM_SETFONT, reinterpret_cast<WPARAM>(g_fontBold), TRUE);
            g_hint1 = makeCtl(p, L"STATIC", L"", 0, 18, 226, 720, 60, 0);
            // 【页2 系统调优】
            p = g_pages[2];
            g_btnTuneHigh = makeCtl(p, L"BUTTON", L"", 0, 18, 30, 200, 40, IDC_TUNE_HIGH);
            g_btnTuneBal = makeCtl(p, L"BUTTON", L"", 0, 18, 84, 200, 40, IDC_TUNE_BAL);
            g_btnTuneRestore = makeCtl(p, L"BUTTON", L"", 0, 18, 138, 200, 40, IDC_TUNE_RESTORE);
            SendMessageW(g_btnTuneHigh, WM_SETFONT, reinterpret_cast<WPARAM>(g_fontBold), TRUE);
            g_hint2 = makeCtl(p, L"STATIC", L"", 0, 18, 196, 720, 60, 0);
            // 【页3 进程】
            p = g_pages[3];
            g_listProc = makeCtl(p, L"LISTBOX", L"", LBS_NOTIFY | WS_TABSTOP, 18, 16, 460, 200, IDC_PROCLIST);
            g_btnProcRefresh = makeCtl(p, L"BUTTON", L"", 0, 490, 16, 90, 26, IDC_PROC_REFRESH);
            g_hint3 = makeCtl(p, L"STATIC", L"", 0, 18, 232, 720, 44, 0);
            // 【页4 启动项】
            p = g_pages[4];
            g_listStartup = makeCtl(p, L"LISTBOX", L"", LBS_NOTIFY | WS_TABSTOP, 18, 16, 360, 200, IDC_STARTUP_LIST);
            g_btnStartupRefresh = makeCtl(p, L"BUTTON", L"", 0, 390, 16, 90, 26, IDC_STARTUP_REFRESH);
            g_btnStartupDisable = makeCtl(p, L"BUTTON", L"", 0, 390, 50, 90, 26, IDC_STARTUP_DISABLE);
            g_btnStartupEnable = makeCtl(p, L"BUTTON", L"", 0, 390, 84, 90, 26, IDC_STARTUP_ENABLE);
            g_btnStartupRestore = makeCtl(p, L"BUTTON", L"", 0, 390, 118, 110, 26, IDC_STARTUP_RESTORE);
            g_hint4 = makeCtl(p, L"STATIC", L"", 0, 18, 232, 720, 44, 0);

            // ----- 底部日志 + 状态栏 -----
            g_log = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"",
                                    WS_CHILD | WS_VISIBLE | ES_MULTILINE | ES_AUTOVSCROLL |
                                    ES_READONLY | WS_VSCROLL, 12, 390, 980, 210, hwnd, nullptr, hInst, nullptr);
            if (g_log) SendMessageW(g_log, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);
            g_footer = CreateWindowExW(0, L"STATIC", L"", WS_CHILD | WS_VISIBLE,
                                       12, 606, 980, 20, hwnd, nullptr, hInst, nullptr);
            if (g_footer) SendMessageW(g_footer, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);

            // 标签注册（语言切换刷新）
            const char* navZh[5] = {"总览", "游戏优化", "系统调优", "进程", "启动项"};
            const char* navEn[5] = {"Dashboard", "Game Tune", "System Tune", "Processes", "Startup"};
            for (int i = 0; i < 5; ++i) Label(g_nav[i], navZh[i], navEn[i]);
            Label(g_bigOpt, "一键性能优化", "One-click Boost");
            Label(g_btnBrowse, "浏览...", "Browse...");
            Label(g_btnApply, "应用优化", "Apply");
            Label(g_btnRollback, "回滚", "Rollback");
            Label(g_btnSave, "保存游戏设置", "Save Settings");
            Label(g_power, "启用电源方案切换", "Enable power scheme switch");
            Label(g_btnTuneHigh, "高性能档", "High Performance");
            Label(g_btnTuneBal, "平衡档", "Balanced");
            Label(g_btnTuneRestore, "恢复调优", "Restore Tune");
            Label(g_btnProcRefresh, "刷新", "Refresh");
            Label(g_btnStartupRefresh, "刷新", "Refresh");
            Label(g_btnStartupDisable, "禁用选中", "Disable");
            Label(g_btnStartupEnable, "启用选中", "Enable");
            Label(g_btnStartupRestore, "恢复全部", "Restore All");
            Label(g_hint1,
                  "玩法：路径留空 → 优化正在运行的游戏；填写该游戏 exe 路径并「保存游戏设置」→「应用优化」会先代启动并自动优化。",
                  "Tip: leave path empty to optimize a running game; set the exe path + Save, then Apply will launch and optimize it.");
            Label(g_hint2,
                  "调优 = 切换高性能/平衡电源 + 处理器最大/最小频率 + 系统调度优先级（需管理员权限）。应用前自动快照，「恢复调优」可还原。",
                  "Tune = power scheme + processor min/max + priority separation (admin). Auto snapshot; Restore Tune reverts it.");
            Label(g_hint3,
                  "列表仅显示 8 款支持游戏中正在运行的；「一键优化」会自动并发优化其中全部游戏。",
                  "Lists the supported games currently running; One-click Boost optimizes all of them.");
            Label(g_hint4,
                  "选中后可禁用/启用（禁用=改名保留并记录备份）；「恢复全部」还原所有被本工具禁用的项。",
                  "Select an entry to disable/enable (rename-based, backed up); Restore All reverts them.");

            // 初始化
            SetWindowTextW(g_dashHelp,
                Utf8ToWide(T("怎么用：① 启动游戏（或到「游戏优化」页配置路径后由工具代启动）→ ② 点上面一键 → ③ 随时「回滚」。",
                             "How to use: 1) start a game (or set its exe path in Game Tune)  2) click Boost  3) Rollback anytime.")).c_str());
            SetWindowTextW(g_dashNote,
                Utf8ToWide(T("所有功能免费 · 无注入、无内核 Hook · 每次优化自动快照可回滚",
                             "All free · no injection, no kernel hooks · auto snapshot each optimize")).c_str());
            RefreshGameList();
            RefreshProcList();
            RefreshStartupList();
            ShowPage(0);
            UpdateDashboard();
            AddLog(std::string("GameOptimizer v") + GOPT_VERSION_STR + "  所有功能免费\n");
            AddLog(T("左侧导航切换功能；一键优化 = 并发处理所有运行中的支持游戏。\n\n",
                     "Use the nav; one-click = concurrent batch optimize of running games.\n\n"));
        } break;

        case WM_DRAWITEM: {
            // 左侧导航自绘：激活页高亮蓝色 + 白色文字
            auto* dis = reinterpret_cast<DRAWITEMSTRUCT*>(lp);
            if (dis != nullptr && dis->CtlID >= IDC_NAV0 && dis->CtlID <= IDC_NAV4) {
                const int page = dis->CtlID - IDC_NAV0;
                const bool active = (page == g_page);
                HDC dc = dis->hDC;
                RECT r = dis->rcItem;
                HBRUSH bg = CreateSolidBrush(active ? RGB(37, 99, 235) : RGB(241, 245, 249));
                FillRect(dc, &r, bg);
                DeleteObject(bg);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, active ? RGB(255, 255, 255) : RGB(40, 55, 80));
                static const char* zh[5] = {"总览", "游戏优化", "系统调优", "进程", "启动项"};
                static const char* en[5] = {"Dashboard", "Game Tune", "System Tune", "Processes", "Startup"};
                const std::wstring t = Utf8ToWide(T(zh[page], en[page]));
                RECT tr = r;
                tr.left += 12;
                DrawTextW(dc, t.c_str(), -1, &tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                return TRUE;
            }
            break;
        }

        case WM_PAINT: {
            PAINTSTRUCT ps{};
            HDC dc = BeginPaint(hwnd, &ps);
            RECT rc{};
            GetClientRect(hwnd, &rc);
            // 顶部标题栏
            RECT hdr{0, 0, rc.right, 52};
            HBRUSH b1 = CreateSolidBrush(RGB(37, 99, 235));
            FillRect(dc, &hdr, b1);
            DeleteObject(b1);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, RGB(255, 255, 255));
            const HFONT old = static_cast<HFONT>(SelectObject(dc, g_fontBold ? g_fontBold : GetStockObject(DEFAULT_GUI_FONT)));
            RECT tr{16, 6, 400, 46};
            DrawTextW(dc, L"GameOptimizer", -1, &tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            SelectObject(dc, old);
            // 侧边栏背景（浅灰）+ 当前页高亮
            RECT sb{0, 52, 216, rc.bottom};
            HBRUSH sbB = CreateSolidBrush(RGB(241, 245, 249));
            FillRect(dc, &sb, sbB);
            DeleteObject(sbB);
            RECT hl{12, 60 + g_page * 48, 204, 60 + g_page * 48 + 40};
            HBRUSH hlB = CreateSolidBrush(RGB(37, 99, 235));
            FillRect(dc, &hl, hlB);
            DeleteObject(hlB);
            // 进度条（仅在执行流程时显示）
            if (g_progress > 0.0) {
                RECT track{228, 58 + 332, rc.right - 16, 58 + 338};
                HBRUSH bgT = CreateSolidBrush(RGB(226, 232, 240));
                FillRect(dc, &track, bgT);
                DeleteObject(bgT);
                const int w = static_cast<int>((track.right - track.left) * g_progress);
                RECT fill = track;
                fill.right = track.left + w;
                HBRUSH fg = CreateSolidBrush(RGB(37, 99, 235));
                FillRect(dc, &fill, fg);
                DeleteObject(fg);
            }
            EndPaint(hwnd, &ps);
        } break;

        case WM_CTLCOLORSTATIC:
        case WM_CTLCOLOREDIT:
        case WM_CTLCOLORLISTBOX: {
            // 页面容器（STATIC）与控件统一白色
            if (reinterpret_cast<HDC>(wp) != nullptr) {
                SetTextColor(reinterpret_cast<HDC>(wp), RGB(30, 30, 30));
                SetBkColor(reinterpret_cast<HDC>(wp), RGB(255, 255, 255));
            }
            return reinterpret_cast<LRESULT>(g_whiteBrush);
        }

        case WM_COMMAND: {
            const int id = LOWORD(wp);
            const int code = HIWORD(wp);
            if (id == IDC_LANG && code == CBN_SELCHANGE) {
                SetLang(SendMessageW(g_lang, CB_GETCURSEL, 0, 0) == 1 ? Lang::En : Lang::Zh);
                ApplyAllLabels();
                RefreshProcList();
                RefreshStartupList();
                UpdateFooter();
                return 0;
            }
            if (code == CBN_SELCHANGE && id == IDC_COMBO) {
                LoadGameConfigToUI();
                return 0;
            }
            if (code != BN_CLICKED) return 0;
            if (id >= IDC_NAV0 && id <= IDC_NAV4) {
                ShowPage(id - IDC_NAV0);
                return 0;
            }
            if (id == IDC_BIGOPT) {
                AddLog(std::string(T("== 一键性能优化 ==\n", "== One-click boost ==\n")));
                AppCore* core = MakeCore();
                std::thread([core]() {
                    core->OptimizeAll([](const gopt::AppCore::FlowEvent& e) {
                        PostMessageW(g_hwnd, WM_APP_UIEVENT, 0, reinterpret_cast<LPARAM>(new PendingFlow{e}));
                    }, 300);
                    PostMessageW(g_hwnd, WM_APP_FINISH, 0, 0);
                    delete core;
                }).detach();
                return 0;
            }
            if (id == IDC_BROWSE) { BrowsePath(); return 0; }
            if (id == IDC_SAVE) { SaveCurrentGameConfig(); return 0; }
            if (id == IDC_APPLY) {
                const GameId sel = CurrentGame();
                AddLog(std::string(T("== 应用优化 ", "== Apply ")) + gopt::GameIdToString(sel) + " ==\n");
                AppCore* core = MakeCore();
                std::thread([core, sel]() {
                    core->OptimizeForGame(sel, [](const gopt::AppCore::FlowEvent& e) {
                        PostMessageW(g_hwnd, WM_APP_UIEVENT, 0, reinterpret_cast<LPARAM>(new PendingFlow{e}));
                    }, 300);
                    PostMessageW(g_hwnd, WM_APP_FINISH, 0, 0);
                    delete core;
                }).detach();
                return 0;
            }
            if (id == IDC_ROLLBACK) {
                AppCore* core = MakeCore();
                AddLog(core->Rollback());
                AddLog("\n\n");
                delete core;
                return 0;
            }
            if (id == IDC_TUNE_HIGH || id == IDC_TUNE_BAL) {
                AppCore* core = MakeCore();
                AddLog(std::string(T("== 系统性能调优 ==\n", "== System tune ==\n")));
                AddLog(core->TuneSystem(id == IDC_TUNE_HIGH));
                AddLog("\n\n");
                delete core;
                return 0;
            }
            if (id == IDC_TUNE_RESTORE) {
                AppCore* core = MakeCore();
                AddLog(core->RestoreTune());
                AddLog("\n\n");
                delete core;
                return 0;
            }
            if (id == IDC_PROC_REFRESH) { RefreshProcList(); return 0; }
            if (id == IDC_STARTUP_REFRESH) { RefreshStartupList(); return 0; }
            if (id == IDC_STARTUP_RESTORE) {
                const int n = StartupManager::RestoreAll();
                AddLog(std::string(T("已恢复 ", "Restored ")) + std::to_string(n) + " " + T("个启动项。\n\n", "startup entries.\n\n"));
                RefreshStartupList();
                return 0;
            }
            if (id == IDC_STARTUP_DISABLE || id == IDC_STARTUP_ENABLE) {
                const int sel = static_cast<int>(SendMessageW(g_listStartup, LB_GETCURSEL, 0, 0));
                if (sel < 0 || sel >= static_cast<int>(g_startups.size()) || g_startups.empty()) {
                    AddLog(std::string(T("请先在列表中选择要操作的启动项。\n\n",
                                         "Please select a startup entry first.\n\n")));
                    return 0;
                }
                const bool ok = (id == IDC_STARTUP_DISABLE)
                                    ? StartupManager::Disable(g_startups[sel].name)
                                    : StartupManager::Enable(g_startups[sel].name);
                AddLog(std::string(ok ? "OK: " : "FAIL: ") + g_startups[sel].name + "\n\n");
                RefreshStartupList();
                return 0;
            }
        } break;

        case WM_APP_UIEVENT: {
            auto* p = reinterpret_cast<PendingFlow*>(lp);
            if (p != nullptr) { RenderFlowEvent(p->e); delete p; }
            return 0;
        }
        case WM_APP_FINISH: {
            AddLog("\n\n");
            g_progress = 0.0;
            RefreshProcList();
            UpdateDashboard();
            InvalidateRect(g_hwnd, nullptr, FALSE);
            return 0;
        }
        case WM_SIZE: {
            RECT rc{};
            GetClientRect(hwnd, &rc);
            MoveWindow(g_log, 12, rc.bottom - 226, rc.right - 24, 176, TRUE);
            MoveWindow(g_footer, 12, rc.bottom - 28, rc.right - 24, 20, TRUE);
            for (int i = 0; i < 5; ++i)
                if (g_pages[i]) MoveWindow(g_pages[i], 222, 62, rc.right - 234, rc.bottom - 300, TRUE);
        } break;

        case WM_DESTROY:
            PostQuitMessage(0);
            break;

        default:
            return DefWindowProcW(hwnd, msg, wp, lp);
    }
    return 0;
}

int WINAPI WinMain(HINSTANCE hInst, HINSTANCE, LPSTR, int nShow) {
    g_core = new AppCore();

    const wchar_t cls[] = L"gopt_gui";
    WNDCLASSW wc{};
    wc.lpfnWndProc = WndProc;
    wc.hInstance = hInst;
    wc.lpszClassName = cls;
    wc.hCursor = LoadCursorW(nullptr, reinterpret_cast<LPCWSTR>(IDC_ARROW));
    wc.hbrBackground = reinterpret_cast<HBRUSH>(COLOR_WINDOW + 1);
    RegisterClassW(&wc);

    HWND hwnd = CreateWindowExW(0, cls, L"GameOptimizer" L" v" GOPT_VERSION_STR,
                                WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT,
                                1010, 660, nullptr, nullptr, hInst, nullptr);
    if (!hwnd) return 0;
    ShowWindow(hwnd, nShow);
    UpdateWindow(hwnd);

    MSG msg{};
    while (GetMessageW(&msg, nullptr, 0, 0)) {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    delete g_core;
    return 0;
}
