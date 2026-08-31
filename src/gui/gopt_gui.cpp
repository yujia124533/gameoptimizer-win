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
static HFONT g_font = nullptr;

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
    AddLog(std::string("GameOptimizer v") + GOPT_VERSION_STR + "\n");
    AddLog(T("所有功能免费：全部优化项对所有人开放。\n",
             "All features free: every optimization enabled for everyone.\n"));
    AddLog(T("一键优化 = 自动检测运行中的游戏并应用；或选游戏后点”应用优化“。\n",
             "One-click = auto-detect & optimize; or select a game then Apply.\n"));
    AddLog(T("为每款游戏保存启动路径/参数/电源开关（保存游戏设置），即可一键优化启动。\n\n",
             "Save per-game launch path/args/power (Save Settings) for one-click launch.\n\n"));
}

static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_CREATE: {
            const HINSTANCE hInst = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
            // 字体：微软雅黑 UI（Win10+），并让标准控件走现代主题（manifest 已启用 v6 控件）
            if (!g_font) {
                g_font = CreateFontW(-15, 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
                                     DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                                     CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Microsoft YaHei UI");
            }
            auto makeCtl = [&](const wchar_t* cls, const wchar_t* text, DWORD style,
                               int x, int y, int w, int h, int id, DWORD exStyle = 0) {
                HWND c = CreateWindowExW(exStyle, cls, text, style | WS_CHILD | WS_VISIBLE,
                                         x, y, w, h, hwnd, reinterpret_cast<HMENU>(id), hInst, nullptr);
                if (c && g_font) SendMessageW(c, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);
                return c;
            };
            g_combo = makeCtl(L"COMBOBOX", L"", WS_TABSTOP | CBS_DROPDOWNLIST, 16, 64, 170, 220, 0);
            g_lang = makeCtl(L"COMBOBOX", L"", WS_TABSTOP | CBS_DROPDOWNLIST, 192, 64, 72, 120, IDC_LANG);
            g_btnApply = makeCtl(L"BUTTON", L"", 0, 274, 64, 110, 28, IDC_APPLY);
            g_path = makeCtl(L"EDIT", L"", WS_TABSTOP, 16, 98, 150, 26, 0, WS_EX_CLIENTEDGE);
            g_btnBrowse = makeCtl(L"BUTTON", L"", 0, 172, 98, 50, 26, IDC_BROWSE);
            g_btnRollback = makeCtl(L"BUTTON", L"", 0, 274, 98, 110, 28, IDC_ROLLBACK);
            g_args = makeCtl(L"EDIT", L"", WS_TABSTOP, 16, 130, 200, 26, 0, WS_EX_CLIENTEDGE);
            g_btnSave = makeCtl(L"BUTTON", L"", 0, 274, 130, 110, 28, IDC_SAVE);
            g_power = makeCtl(L"BUTTON", L"", BS_AUTOCHECKBOX, 16, 162, 250, 24, IDC_POWER);
            g_btnRefresh = makeCtl(L"BUTTON", L"", 0, 274, 162, 110, 28, IDC_REFRESH);
            g_btnAuto = makeCtl(L"BUTTON", L"", 0, 16, 192, 150, 30, IDC_AUTO);
            g_log = makeCtl(L"EDIT", L"", ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY | WS_VSCROLL,
                            16, 232, 368, 190, 0, WS_EX_CLIENTEDGE);
            // 让按钮/勾选使用系统主题外观
            SendMessageW(g_power, WM_SETFONT, reinterpret_cast<WPARAM>(g_font), TRUE);

            RefreshGameList();
            SendMessageW(g_lang, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(L"中文"));
            SendMessageW(g_lang, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(L"English"));
            SendMessageW(g_lang, CB_SETCURSEL, 0, 0);
            ApplyLanguage();
            LogIntro();
        } break;

        case WM_PAINT: {
            PAINTSTRUCT ps{};
            HDC dc = BeginPaint(hwnd, &ps);
            RECT rc{};
            GetClientRect(hwnd, &rc);
            // 顶部标题带（蓝色渐变感：双色块）
            RECT hdr{0, 0, rc.right, 52};
            HBRUSH b1 = CreateSolidBrush(RGB(37, 99, 235));
            FillRect(dc, &hdr, b1);
            DeleteObject(b1);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, RGB(255, 255, 255));
            const HFONT old = static_cast<HFONT>(SelectObject(dc, g_font ? g_font : GetStockObject(DEFAULT_GUI_FONT)));
            RECT tr{14, 6, rc.right - 14, 46};
            DrawTextW(dc, L"GameOptimizer", -1, &tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            SelectObject(dc, old);
            EndPaint(hwnd, &ps);
        } break;

        case WM_CTLCOLORSTATIC:
        case WM_CTLCOLOREDIT:
        case WM_CTLCOLORLISTBOX: {
            SetTextColor(reinterpret_cast<HDC>(wp), RGB(30, 30, 30));
            SetBkColor(reinterpret_cast<HDC>(wp), RGB(255, 255, 255));
            static HBRUSH white = CreateSolidBrush(RGB(255, 255, 255));
            return reinterpret_cast<LRESULT>(white);
        }

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
                core->OptimizeForGame(CurrentGame(),
                                      [](const std::string& l) { AddLog(l); }, 450);
                AddLog("\n\n");
                delete core;
            } else if (id == IDC_AUTO) {
                AppCore* core = MakeCore();
                AddLog(std::string(T("== 一键优化 ==\n", "== One-click optimize ==\n")));
                core->OptimizeAuto([](const std::string& l) { AddLog(l); }, 450);
                AddLog("\n\n");
                delete core;
            } else if (id == IDC_ROLLBACK) {
                AppCore* core = MakeCore();
                AddLog(core->Rollback());
                AddLog("\n\n");
                delete core;
            } else if (id == IDC_REFRESH) {
                AddLog(T("所有功能免费：全部优化项对所有人开放。\n",
                         "All features free: every optimization enabled for everyone.\n"));
                AddLog(g_core->Profile().ToString());
                AddLog("\n\n");
            }
        } break;

        case WM_SIZE: {
            RECT rc{};
            GetClientRect(hwnd, &rc);
            MoveWindow(g_log, 16, 232, rc.right - 32, rc.bottom - 248, TRUE);
        } break;

        case WM_DESTROY:
            if (g_font) {
                DeleteObject(g_font);
                g_font = nullptr;
            }
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
                                400, 440, nullptr, nullptr, hInst, nullptr);
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
