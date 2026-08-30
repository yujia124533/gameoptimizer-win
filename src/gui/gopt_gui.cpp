// GameOptimizer 图形界面（原生 Win32，复用核心库 AppCore）
// 支持：中文/English 下拉切换 · 选择游戏 · 代启动路径 · Pro(电源)开关 · 应用优化/回滚/刷新
#include <windows.h>
#include <commdlg.h>

#include <string>

#include "core/AppCore.h"
#include "i18n.h"
#include "license/License.h"
#include "version.h"

using gopt::AppConfig;
using gopt::AppCore;
using gopt::GameId;
using gopt::Lang;
using gopt::LicenseInfo;
using gopt::SetLang;
using gopt::T;

static const GameId kGames[] = {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                                GameId::PUBG, GameId::Valorant, GameId::Apex,
                                GameId::Dota2, GameId::Overwatch2};

static HWND g_combo, g_lang, g_path, g_power, g_log;
static HWND g_btnApply, g_btnRollback, g_btnRefresh, g_btnBrowse;
static AppCore* g_core = nullptr;

enum { IDC_APPLY = 101, IDC_ROLLBACK = 102, IDC_REFRESH = 103, IDC_POWER = 104,
       IDC_BROWSE = 105, IDC_LANG = 106 };

static std::wstring Utf8ToWide(const std::string& s) {
    if (s.empty()) return {};
    const int len = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
    if (len <= 1) return {};
    std::wstring w(static_cast<size_t>(len) - 1, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, &w[0], len);
    return w;
}

static void SetText(HWND h, const char* zh, const char* en) {
    SetWindowTextW(h, Utf8ToWide(T(zh, en)).c_str());
}

static void AddLog(const std::string& s) {
    const std::wstring w = Utf8ToWide(s);
    const int len = GetWindowTextLengthW(g_log);
    SendMessageW(g_log, EM_SETSEL, len, len);
    SendMessageW(g_log, EM_REPLACESEL, FALSE, reinterpret_cast<LPARAM>(w.c_str()));
    SendMessageW(g_log, EM_SCROLLCARET, 0, 0);
}

// 按当前语言刷新界面文案
static void ApplyLanguage() {
    SetText(g_btnApply, "应用优化", "Apply");
    SetText(g_btnRollback, "回滚", "Rollback");
    SetText(g_btnRefresh, "刷新", "Refresh");
    SetText(g_btnBrowse, "浏览...", "Browse...");
    SetText(g_power, "启用电源方案切换 (Pro)", "Enable power scheme switch (Pro)");
}

static void RefreshGameList() {
    SendMessageW(g_combo, CB_RESETCONTENT, 0, 0);
    for (const GameId id : kGames) {
        const std::wstring name = Utf8ToWide(gopt::GameIdToString(id));
        SendMessageW(g_combo, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(name.c_str()));
    }
    SendMessageW(g_combo, CB_SETCURSEL, 0, 0);
}

static GameId SelectedGame() {
    const int idx = static_cast<int>(SendMessageW(g_combo, CB_GETCURSEL, 0, 0));
    if (idx < 0 || idx >= static_cast<int>(sizeof(kGames) / sizeof(kGames[0]))) return kGames[0];
    return kGames[idx];
}

static AppCore* MakeCore() {
    AppConfig cfg;
    wchar_t path[MAX_PATH] = {};
    GetWindowTextW(g_path, path, MAX_PATH);
    const int len = WideCharToMultiByte(CP_UTF8, 0, path, -1, nullptr, 0, nullptr, nullptr);
    if (len > 1) {
        std::string s(static_cast<size_t>(len) - 1, '\0');
        WideCharToMultiByte(CP_UTF8, 0, path, -1, &s[0], len, nullptr, nullptr);
        cfg.gameExeOverride = s;
    }
    const bool power = SendMessageW(g_power, BM_GETCHECK, 0, 0) == BST_CHECKED;
    cfg.allowPowerSchemeSwitch = power;
    return new AppCore(cfg);
}

static void BrowsePath(HWND hwnd) {
    OPENFILENAMEW ofn{};
    wchar_t file[MAX_PATH] = {};
    ofn.lStructSize = sizeof(ofn);
    ofn.hwndOwner = hwnd;
    ofn.lpstrFilter = L"*.exe\0*.exe\0*.*\0*.*\0";
    ofn.lpstrFile = file;
    ofn.nMaxFile = MAX_PATH;
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;
    if (GetOpenFileNameW(&ofn)) SetWindowTextW(g_path, file);
}

static void LogIntro() {
    const LicenseInfo li = gopt::License::Check(g_core->Profile());
    AddLog(std::string("GameOptimizer v") + GOPT_VERSION_STR + "\n");
    AddLog(std::string(T("授权: ", "License: ")) + (li.isPro ? "Pro" : T("免费版", "Free"))
           + "  " + li.message + "\n");
    AddLog(T("选择游戏 -> 应用优化（自动快照 + 看门狗）。\n",
             "Select a game -> Apply (auto snapshot + watchdog).\n"));
    AddLog(T("填代启动路径可让工具拉起游戏；Pro 功能需授权。\n\n",
             "Set a launch path to start the game; Pro features need a license.\n\n"));
}

static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_CREATE: {
            const HINSTANCE hInst = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
            g_combo = CreateWindowExW(0, L"COMBOBOX", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP |
                                      CBS_DROPDOWNLIST, 20, 16, 180, 220, hwnd,
                                      nullptr, hInst, nullptr);
            g_lang = CreateWindowExW(0, L"COMBOBOX", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP |
                                      CBS_DROPDOWNLIST, 208, 16, 70, 120, hwnd,
                                      reinterpret_cast<HMENU>(IDC_LANG), hInst, nullptr);
            g_path = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                                     20, 50, 190, 24, hwnd, nullptr, hInst, nullptr);
            g_btnBrowse = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 218, 50, 52, 24,
                                          hwnd, reinterpret_cast<HMENU>(IDC_BROWSE), hInst, nullptr);
            g_power = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX,
                                      20, 82, 250, 22, hwnd, reinterpret_cast<HMENU>(IDC_POWER), hInst, nullptr);
            g_btnApply = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 300, 16, 90, 28,
                                         hwnd, reinterpret_cast<HMENU>(IDC_APPLY), hInst, nullptr);
            g_btnRollback = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 300, 50, 90, 28,
                                            hwnd, reinterpret_cast<HMENU>(IDC_ROLLBACK), hInst, nullptr);
            g_btnRefresh = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 300, 84, 90, 28,
                                           hwnd, reinterpret_cast<HMENU>(IDC_REFRESH), hInst, nullptr);
            g_log = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"",
                                    WS_CHILD | WS_VISIBLE | ES_MULTILINE | ES_AUTOVSCROLL |
                                    ES_READONLY | WS_VSCROLL, 20, 114, 370, 200, hwnd, nullptr, hInst, nullptr);

            RefreshGameList();
            SendMessageW(g_lang, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(L"中文"));
            SendMessageW(g_lang, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(L"English"));
            SendMessageW(g_lang, CB_SETCURSEL, 0, 0);
            ApplyLanguage();
            LogIntro();
        } break;

        case WM_COMMAND: {
            const int id = LOWORD(wp);
            const int code = HIWORD(wp);
            if (id == IDC_LANG && code == CBN_SELCHANGE) {
                const int idx = static_cast<int>(SendMessageW(g_lang, CB_GETCURSEL, 0, 0));
                SetLang(idx == 1 ? Lang::En : Lang::Zh);
                ApplyLanguage();
                break;
            }
            if (code != BN_CLICKED) break;
            if (id == IDC_BROWSE) {
                BrowsePath(hwnd);
            } else if (id == IDC_APPLY) {
                AppCore* core = MakeCore();
                AddLog(std::string(T("== 应用优化 ", "== Apply ")) + gopt::GameIdToString(SelectedGame()) + " ==\n");
                AddLog(core->OptimizeForGame(SelectedGame()));
                AddLog("\n\n");
                delete core;
            } else if (id == IDC_ROLLBACK) {
                AppCore* core = MakeCore();
                AddLog(core->Rollback());
                AddLog("\n\n");
                delete core;
            } else if (id == IDC_REFRESH) {
                const LicenseInfo li = gopt::License::Check(g_core->Profile());
                AddLog(std::string(T("授权: ", "License: ")) + (li.isPro ? "Pro" : T("免费版", "Free"))
                       + "  " + li.message + "\n");
                AddLog(g_core->Profile().ToString());
                AddLog("\n\n");
            }
        } break;

        case WM_SIZE: {
            RECT rc{};
            GetClientRect(hwnd, &rc);
            MoveWindow(g_log, 20, 114, rc.right - 40, rc.bottom - 134, TRUE);
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
                                410, 360, nullptr, nullptr, hInst, nullptr);
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
