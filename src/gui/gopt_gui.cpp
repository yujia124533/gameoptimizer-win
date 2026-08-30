// GameOptimizer 图形界面（原生 Win32，复用核心库 AppCore）
// 支持：中/英切换 · 一键优化 · 每游戏选择/路径/参数/电源开关 · 应用优化/回滚/刷新
#include <windows.h>
#include <commdlg.h>

#include <string>

#include "config/GameConfig.h"
#include "core/AppCore.h"
#include "i18n.h"
#include "license/License.h"
#include "version.h"

using gopt::AppConfig;
using gopt::AppCore;
using gopt::GameConfig;
using gopt::GameId;
using gopt::GameLaunchConfig;
using gopt::Lang;
using gopt::LicenseInfo;
using gopt::SetLang;
using gopt::T;

static const GameId kGames[] = {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                                GameId::PUBG, GameId::Valorant, GameId::Apex,
                                GameId::Dota2, GameId::Overwatch2};

static HWND g_combo, g_lang, g_path, g_args, g_power, g_log;
static HWND g_btnApply, g_btnRollback, g_btnRefresh, g_btnBrowse;
static HWND g_btnAuto, g_btnSave;
static AppCore* g_core = nullptr;

enum { IDC_APPLY = 101, IDC_ROLLBACK = 102, IDC_REFRESH = 103, IDC_POWER = 104,
       IDC_BROWSE = 105, IDC_LANG = 106, IDC_AUTO = 107, IDC_SAVE = 108 };

static std::wstring Utf8ToWide(const std::string& s) {
    if (s.empty()) return {};
    const int len = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
    if (len <= 1) return {};
    std::wstring w(static_cast<size_t>(len) - 1, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, &w[0], len);
    return w;
}

static std::string WideToUtf8(HWND hwndEdit) {
    wchar_t buf[MAX_PATH] = {};
    GetWindowTextW(hwndEdit, buf, MAX_PATH);
    const int len = WideCharToMultiByte(CP_UTF8, 0, buf, -1, nullptr, 0, nullptr, nullptr);
    if (len <= 1) return {};
    std::string s(static_cast<size_t>(len) - 1, '\0');
    WideCharToMultiByte(CP_UTF8, 0, buf, -1, &s[0], len, nullptr, nullptr);
    return s;
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

// 当前所选游戏
static GameId CurrentGame() {
    const int idx = static_cast<int>(SendMessageW(g_combo, CB_GETCURSEL, 0, 0));
    if (idx < 0 || idx >= static_cast<int>(sizeof(kGames) / sizeof(kGames[0]))) return kGames[0];
    return kGames[idx];
}

// 把所选游戏的配置加载到界面
static void LoadGameConfigToUI() {
    const GameLaunchConfig gc = GameConfig::Get(CurrentGame());
    SetWindowTextW(g_path, Utf8ToWide(gc.exePath).c_str());
    SetWindowTextW(g_args, Utf8ToWide(gc.args).c_str());
    SendMessageW(g_power, BM_SETCHECK, gc.powerScheme ? BST_CHECKED : BST_UNCHECKED, 0);
}

static void ApplyLanguage() {
    SetText(g_btnApply, "应用优化", "Apply");
    SetText(g_btnRollback, "回滚", "Rollback");
    SetText(g_btnRefresh, "刷新", "Refresh");
    SetText(g_btnBrowse, "浏览...", "Browse...");
    SetText(g_btnAuto, "一键优化", "One-click Optimize");
    SetText(g_btnSave, "保存游戏设置", "Save Settings");
    SetText(g_power, "启用电源方案切换 (Pro)", "Enable power scheme switch (Pro)");
}

static void RefreshGameList() {
    SendMessageW(g_combo, CB_RESETCONTENT, 0, 0);
    for (const GameId id : kGames) {
        const std::wstring name = Utf8ToWide(gopt::GameIdToString(id));
        SendMessageW(g_combo, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(name.c_str()));
    }
    SendMessageW(g_combo, CB_SETCURSEL, 0, 0);
    LoadGameConfigToUI();
}

static AppCore* MakeCore() {
    AppConfig cfg;
    cfg.gameExeOverride = WideToUtf8(g_path);
    cfg.allowPowerSchemeSwitch = SendMessageW(g_power, BM_GETCHECK, 0, 0) == BST_CHECKED;
    return new AppCore(cfg);
}

// 保存当前界面为所选游戏的「优化启动」配置
static void SaveCurrentGameConfig() {
    GameLaunchConfig gc = GameConfig::Get(CurrentGame());
    gc.exePath = WideToUtf8(g_path);
    gc.args = WideToUtf8(g_args);
    gc.powerScheme = SendMessageW(g_power, BM_GETCHECK, 0, 0) == BST_CHECKED;
    GameConfig::Set(CurrentGame(), gc);
    AddLog(std::string(T("已保存 ", "Saved ")) + gopt::GameIdToString(CurrentGame())
           + T(" 的优化启动配置。\n\n", " launch config.\n\n"));
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
    AddLog(T("一键优化 = 自动检测运行中的游戏并应用；或选游戏后点”应用优化“。\n",
             "One-click = auto-detect & optimize; or select a game then Apply.\n"));
    AddLog(T("为每款游戏保存启动路径/参数/电源开关（保存游戏设置），即可一键优化启动。\n\n",
             "Save per-game launch path/args/power (Save Settings) for one-click launch.\n\n"));
}

static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_CREATE: {
            const HINSTANCE hInst = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
            g_combo = CreateWindowExW(0, L"COMBOBOX", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP |
                                      CBS_DROPDOWNLIST, 16, 14, 170, 220, hwnd, nullptr, hInst, nullptr);
            g_lang = CreateWindowExW(0, L"COMBOBOX", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP |
                                      CBS_DROPDOWNLIST, 192, 14, 72, 120, hwnd,
                                      reinterpret_cast<HMENU>(IDC_LANG), hInst, nullptr);
            g_btnApply = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 274, 14, 110, 26,
                                         hwnd, reinterpret_cast<HMENU>(IDC_APPLY), hInst, nullptr);
            g_path = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                                     16, 46, 150, 24, hwnd, nullptr, hInst, nullptr);
            g_btnBrowse = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 172, 46, 50, 24,
                                          hwnd, reinterpret_cast<HMENU>(IDC_BROWSE), hInst, nullptr);
            g_btnRollback = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 274, 46, 110, 26,
                                            hwnd, reinterpret_cast<HMENU>(IDC_ROLLBACK), hInst, nullptr);
            g_args = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                                     16, 78, 200, 24, hwnd, nullptr, hInst, nullptr);
            g_btnSave = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 274, 78, 110, 26,
                                        hwnd, reinterpret_cast<HMENU>(IDC_SAVE), hInst, nullptr);
            g_power = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX,
                                      16, 110, 250, 22, hwnd, reinterpret_cast<HMENU>(IDC_POWER), hInst, nullptr);
            g_btnRefresh = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 274, 110, 110, 26,
                                           hwnd, reinterpret_cast<HMENU>(IDC_REFRESH), hInst, nullptr);
            g_btnAuto = CreateWindowExW(0, L"BUTTON", L"", WS_CHILD | WS_VISIBLE, 16, 140, 150, 28,
                                        hwnd, reinterpret_cast<HMENU>(IDC_AUTO), hInst, nullptr);
            g_log = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"",
                                    WS_CHILD | WS_VISIBLE | ES_MULTILINE | ES_AUTOVSCROLL |
                                    ES_READONLY | WS_VSCROLL, 16, 176, 368, 190, hwnd, nullptr, hInst, nullptr);

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
            // 游戏下拉框（控件 ID 为 0）：切换游戏时加载其保存的配置
            if (id == 0 && code == CBN_SELCHANGE) {
                LoadGameConfigToUI();
                break;
            }
            if (code != BN_CLICKED) break;
            if (id == IDC_BROWSE) {
                BrowsePath(hwnd);
            } else if (id == IDC_SAVE) {
                SaveCurrentGameConfig();
            } else if (id == IDC_APPLY) {
                AppCore* core = MakeCore();
                AddLog(std::string(T("== 应用优化 ", "== Apply ")) + gopt::GameIdToString(CurrentGame()) + " ==\n");
                AddLog(core->OptimizeForGame(CurrentGame()));
                AddLog("\n\n");
                delete core;
            } else if (id == IDC_AUTO) {
                AppCore* core = MakeCore();
                AddLog(std::string(T("== 一键优化 ==\n", "== One-click optimize ==\n")));
                AddLog(core->OptimizeAuto());
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
            MoveWindow(g_log, 16, 176, rc.right - 32, rc.bottom - 192, TRUE);
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
                                400, 400, nullptr, nullptr, hInst, nullptr);
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
