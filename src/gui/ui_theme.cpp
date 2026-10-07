// =============================================================================
// GameOptimizer v1.1.0 — UI 基础层实现：设计令牌 / 主题跟随 / 高 DPI / 字体缓存
// -----------------------------------------------------------------------------
// API 白名单：user32（SetProcessDpiAwarenessContext·GetDpiForWindow·SystemParametersInfo·
// SendMessageTimeout·InvalidateRect）、gdi32（CreateFont·RoundRect·TextOut·GetTextExtentPoint32）、
// advapi32（RegOpenKeyEx·RegQueryValueEx）、kernel32（LoadLibrary·GetProcAddress·MulDiv）。
// 无第三方库、无 Hook、无注入；只读注册表，从不写入系统设置。
//
// 【高 DPI 降级链】
//   PerMonitorV2（Win10 1703+，动态解析 SetProcessDpiAwarenessContext）
//     → PerMonitor（Win8.1+，SetProcessDpiAwareness 动态解析）
//       → SystemAware（SetProcessDPIAware，Vista+ 静态导入，必成功）
//         → 全部失败：进程保持「不感知」，由系统 DWM 位图拉伸，行为等同 v1.0.19（不报错）。
//   注意：SetProcessDpiAwarenessContext 必须在创建任何 HWND 之前调用，否则返回失败。
//   本仓库 resources/manifest.xml 目前未声明 dpiAware/dpiAwareness，因此必须靠上面这次
//   动态调用；若队长愿意在清单里补 <dpiAwareness>PerMonitorV2</dpiAwareness>，则可双保险
//   （清单属于队长收口范围，本文件不改它）。
//
// 【主题跟随降级链】
//   HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme
//     → 键不存在/无权限（Win7~8.1、被精简的系统）：降级为浅色。
//   运行中切换：WM_SETTINGCHANGE / WM_THEMECHANGED 后调用 UiThemeRefresh()，
//   重读注册表 → 重建字体 → 广播 UiThemeChangedMessage() → 各窗口重绘。
// =============================================================================

#include "gui/ui_theme.h"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <string>

namespace gopt {
namespace ui {
namespace {

constexpr int kUIFontPixel = -12;  // 令牌中 Body 的基础像素高度（正数记法）

// ---- 枚举与数组维度一致性护栏（改枚举必须同步 k*Count，否则编译期报错） ----
static_assert(static_cast<int>(UiColorRole::Count) == kColorRoleCount, "ui color token count drifted");
static_assert(static_cast<int>(UiFontRole::Count) == kFontRoleCount, "ui font role count drifted");
static_assert(static_cast<int>(UiSpace::Xxl) == kSpaceCount - 1, "ui space token count drifted");
static_assert(static_cast<int>(UiRadius::Pill) == kRadiusCount - 1, "ui radius token count drifted");
static_assert(static_cast<int>(UiControlH::Xl) == kControlHCount - 1, "ui control height count drifted");

inline COLORREF MakeRGB(BYTE r, BYTE g, BYTE b) { return RGB(r, g, b); }

// ---- 浅色令牌（沿用 v1.0.19 已有调色板，保证共存期不突兀） ----
const COLORREF kLightColors[kColorRoleCount] = {
    /*WindowBg      */ MakeRGB(246, 248, 252),
    /*PanelBg       */ MakeRGB(255, 255, 255),
    /*Surface       */ MakeRGB(255, 255, 255),
    /*SurfaceAlt    */ MakeRGB(241, 245, 249),
    /*SurfaceHover  */ MakeRGB(226, 232, 240),
    /*Border        */ MakeRGB(226, 232, 240),
    /*BorderStrong  */ MakeRGB(203, 213, 225),
    /*Divider       */ MakeRGB(232, 237, 244),
    /*TextPrimary   */ MakeRGB(15, 23, 42),
    /*TextSecondary */ MakeRGB(71, 85, 105),
    /*TextMuted     */ MakeRGB(100, 116, 139),
    /*TextOnAccent  */ MakeRGB(255, 255, 255),
    /*Accent        */ MakeRGB(37, 99, 235),
    /*AccentHover   */ MakeRGB(59, 130, 246),
    /*AccentPressed */ MakeRGB(29, 78, 216),
    /*AccentSoft    */ MakeRGB(219, 234, 254),
    /*DisabledBg    */ MakeRGB(241, 245, 249),
    /*DisabledText  */ MakeRGB(148, 163, 184),
    /*FocusRing     */ MakeRGB(37, 99, 235),
    /*Success       */ MakeRGB(22, 163, 74),
    /*SuccessBg     */ MakeRGB(220, 252, 231),
    /*Warning       */ MakeRGB(217, 119, 6),
    /*WarningBg     */ MakeRGB(254, 249, 195),
    /*Danger        */ MakeRGB(220, 38, 38),
    /*DangerBg      */ MakeRGB(254, 226, 226),
    /*Info          */ MakeRGB(8, 145, 178),
    /*InfoBg        */ MakeRGB(207, 250, 254),
    /*Track         */ MakeRGB(226, 232, 240),
    /*Shadow        */ MakeRGB(203, 213, 225),
    /*Overlay       */ MakeRGB(255, 255, 255),
};

// ---- 深色令牌（沿用 v1.0.19 科技感深色，如 #0B1220 底 / #00E5FF 强调） ----
const COLORREF kDarkColors[kColorRoleCount] = {
    /*WindowBg      */ MakeRGB(11, 18, 32),
    /*PanelBg       */ MakeRGB(17, 24, 39),
    /*Surface       */ MakeRGB(30, 41, 59),
    /*SurfaceAlt    */ MakeRGB(24, 33, 50),
    /*SurfaceHover  */ MakeRGB(41, 56, 82),
    /*Border        */ MakeRGB(51, 65, 85),
    /*BorderStrong  */ MakeRGB(71, 85, 105),
    /*Divider       */ MakeRGB(38, 52, 74),
    /*TextPrimary   */ MakeRGB(241, 245, 249),
    /*TextSecondary */ MakeRGB(148, 163, 184),
    /*TextMuted     */ MakeRGB(100, 116, 139),
    /*TextOnAccent  */ MakeRGB(5, 12, 24),
    /*Accent        */ MakeRGB(0, 229, 255),
    /*AccentHover   */ MakeRGB(56, 189, 248),
    /*AccentPressed */ MakeRGB(34, 211, 238),
    /*AccentSoft    */ MakeRGB(15, 43, 62),
    /*DisabledBg    */ MakeRGB(30, 41, 59),
    /*DisabledText  */ MakeRGB(71, 85, 105),
    /*FocusRing     */ MakeRGB(34, 211, 238),
    /*Success       */ MakeRGB(74, 222, 128),
    /*SuccessBg     */ MakeRGB(6, 46, 30),
    /*Warning       */ MakeRGB(251, 191, 36),
    /*WarningBg     */ MakeRGB(58, 42, 8),
    /*Danger        */ MakeRGB(248, 113, 113),
    /*DangerBg      */ MakeRGB(66, 21, 24),
    /*Info          */ MakeRGB(34, 211, 238),
    /*InfoBg        */ MakeRGB(10, 47, 61),
    /*Track         */ MakeRGB(30, 41, 59),
    /*Shadow        */ MakeRGB(6, 11, 20),
    /*Overlay       */ MakeRGB(17, 24, 39),
};

// ---- 8px 间距栅格 / 字号阶梯 / 圆角 / 控件高度（单位为「96 DPI 逻辑像素」） ----
constexpr int kSpace[kSpaceCount]         = {2, 4, 8, 16, 24, 32, 48};
constexpr int kBaseFontSize[kFontRoleCount] = {11, 12, 12, 14, 16, 22, 12};  // Caption..Mono
constexpr int kRadiusBase[kRadiusCount]   = {4, 8, 12, 999};
constexpr int kControlBase[kControlHCount] = {24, 32, 40, 56};

// ---- 全局状态 ----
Theme      g_theme{};
UINT       g_dpi = 96;
bool       g_inited = false;
bool       g_dark = false;
UiThemeMode g_mode = UiThemeMode::FollowSystem;

HFONT      g_fonts[kFontRoleCount] = {};          // 0 表示尚未创建
const UINT kThemeChangedMsg = WM_APP + 0x51;      // 主题变化广播（重绘入口）

inline int ScaleBy(int v, UINT dpi) {
    return static_cast<int>(MulDiv(v, static_cast<int>(dpi), 96));
}

// 读取 HKCU 主题偏好。true=浅色 App 模式；读不到时 *ok=false（调用方决定降级）。
bool ReadAppsUseLightTheme(bool* ok) {
    if (ok) *ok = false;
    HKEY key = nullptr;
    const wchar_t* path =
        L"Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
    if (RegOpenKeyExW(HKEY_CURRENT_USER, path, 0, KEY_READ, &key) != ERROR_SUCCESS) return true;  // 降级浅色
    DWORD value = 1, size = sizeof(value), type = 0;
    const LONG rc = RegQueryValueExW(key, L"AppsUseLightTheme", nullptr, &type,
                                     reinterpret_cast<LPBYTE>(&value), &size);
    RegCloseKey(key);
    if (rc != ERROR_SUCCESS || type != REG_DWORD) return true;  // 值缺失：降级浅色
    if (ok) *ok = true;
    return value != 0;
}

// 应用字体族：优先中英混排的 "Microsoft YaHei UI"，缺失时退回系统界面字体
void QueryUIFace(wchar_t* face, int cch) {
    if (face == nullptr || cch <= 0) return;
    face[0] = L'\0';
    if (GetTextFaceW(GetDC(nullptr), cch, face) > 0 && face[0] != L'\0') return;
    NONCLIENTMETRICSW ncm{};
    ncm.cbSize = sizeof(ncm);
    if (SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, sizeof(ncm), &ncm, 0)) {
        lstrcpynW(face, ncm.lfMessageFont.lfFaceName, cch);
    }
    if (face[0] == L'\0') lstrcpynW(face, L"Segoe UI", cch);
}

void QueryMonoFace(wchar_t* face, int cch) {
    if (face == nullptr || cch <= 0) return;
    if (GetTextFaceW(GetDC(nullptr), cch, face) > 0 && face[0] != L'\0') return;
    lstrcpynW(face, L"Consolas", cch);
}

void DestroyFonts() {
    for (int i = 0; i < kFontRoleCount; ++i) {
        if (g_fonts[i] != nullptr) {
            DeleteObject(g_fonts[i]);
            g_fonts[i] = nullptr;
        }
    }
}

}  // namespace

UINT UiThemeChangedMessage() { return kThemeChangedMsg; }

const Theme& UiTheme() { return g_theme; }
bool UiIsDark() { return g_dark; }
UiThemeMode UiGetThemeMode() { return g_mode; }
UINT UiDpi() { return g_dpi == 0 ? 96 : g_dpi; }

// -----------------------------------------------------------------------------
// 令牌构建：把「96 DPI 逻辑值」按当前 DPI 展开成实际像素
// -----------------------------------------------------------------------------
static void ApplyTokens(UINT dpi) {
    const COLORREF* src = g_dark ? kDarkColors : kLightColors;
    for (int i = 0; i < kColorRoleCount; ++i) g_theme.color[i] = src[i];
    for (int i = 0; i < kSpaceCount; ++i) g_theme.space[i] = ScaleBy(kSpace[i], dpi);
    for (int i = 0; i < kFontRoleCount; ++i) g_theme.fontSize[i] = ScaleBy(kBaseFontSize[i], dpi);
    for (int i = 0; i < kRadiusCount; ++i) g_theme.radius[i] = ScaleBy(kRadiusBase[i], dpi);
    for (int i = 0; i < kControlHCount; ++i) g_theme.controlH[i] = ScaleBy(kControlBase[i], dpi);
    g_theme.radius[static_cast<int>(UiRadius::Pill)] = 999 * std::max(1, static_cast<int>(dpi) / 96);
    g_theme.dpi = dpi;
    g_dpi = dpi;
}

// -----------------------------------------------------------------------------
// 高 DPI：PerMonitorV2 → PerMonitor → SystemAware 降级
// -----------------------------------------------------------------------------
int UiEnablePerMonitorDpi() {
    // 1) PerMonitorV2：动态解析（老 SDK/老系统没有该导出，不能静态链接）
    typedef BOOL(WINAPI * PFN_SetCtx)(void*);
    HMODULE user32 = LoadLibraryW(L"user32.dll");
    int level = 0;
    if (user32 != nullptr) {
        PFN_SetCtx setCtx = reinterpret_cast<PFN_SetCtx>(
            reinterpret_cast<void*>(GetProcAddress(user32, "SetProcessDpiAwarenessContext")));
        if (setCtx != nullptr) {
            // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 == (HANDLE)-4
            if (setCtx(reinterpret_cast<void*>(static_cast<INT_PTR>(-4)))) level = 3;
        }
        if (level == 0) {
            // 2) PerMonitor（Win8.1+）：SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE=2)
            typedef HRESULT(WINAPI * PFN_SetAwareness)(int);
            PFN_SetAwareness setAware = reinterpret_cast<PFN_SetAwareness>(
                reinterpret_cast<void*>(GetProcAddress(user32, "SetProcessDpiAwareness")));
            if (setAware != nullptr && SUCCEEDED(setAware(2))) level = 2;
        }
        FreeLibrary(user32);
    }
    // 3) SystemAware（Vista+，静态导入）
    if (level == 0 && SetProcessDPIAware()) level = 1;

    // 探测当前进程 DPI 并刷新令牌
    HDC screen = GetDC(nullptr);
    UINT dpi = 96;
    if (screen != nullptr) {
        const int logPixels = GetDeviceCaps(screen, LOGPIXELSY);
        if (logPixels > 0) dpi = static_cast<UINT>(logPixels);
        ReleaseDC(nullptr, screen);
    }
    ApplyTokens(dpi);
    DestroyFonts();  // 新 DPI 下重建字体
    return level;
}

double UiDpiFactor() { return static_cast<double>(UiDpi()) / 96.0; }

UINT UiDpiForWindow(HWND hwnd) {
    if (hwnd == nullptr) return UiDpi();
    typedef UINT(WINAPI * PFN_GetDpiForWindow)(HWND);
    HMODULE user32 = GetModuleHandleW(L"user32.dll");
    if (user32 != nullptr) {
        PFN_GetDpiForWindow getDpi = reinterpret_cast<PFN_GetDpiForWindow>(
            reinterpret_cast<void*>(GetProcAddress(user32, "GetDpiForWindow")));
        if (getDpi != nullptr) {
            const UINT dpi = getDpi(hwnd);
            if (dpi >= 48) return dpi;
        }
    }
    // 降级：整机 DPI（多显示器非感知进程下由 DWM 拉伸，视觉等价 v1.0.19）
    HDC dc = GetDC(hwnd);
    UINT dpi = UiDpi();
    if (dc != nullptr) {
        const int logPixels = GetDeviceCaps(dc, LOGPIXELSY);
        if (logPixels > 0) dpi = static_cast<UINT>(logPixels);
        ReleaseDC(hwnd, dc);
    }
    return dpi;
}

double UiScaleForWindow(HWND hwnd) {
    return static_cast<double>(UiDpiForWindow(hwnd)) / 96.0;
}

UINT UiOnDpiChanged(UINT newDpi) {
    UINT dpi = newDpi;
    if (dpi < 48) dpi = UiDpi();
    if (dpi < 48) dpi = 96;
    ApplyTokens(dpi);
    DestroyFonts();  // 下次 UiFont 按新 DPI 重建
    return dpi;
}

void UiScaleSpan(const int* src, int* dst, int count) {
    if (src == nullptr || dst == nullptr || count <= 0) return;
    for (int i = 0; i < count; ++i) dst[i] = UiScale(src[i]);
}

// -----------------------------------------------------------------------------
// 主题初始化 / 运行中切换 / 广播
// -----------------------------------------------------------------------------
bool UiThemeInit(bool bFollowSystem) {
    if (bFollowSystem && g_mode != UiThemeMode::FollowSystem) g_mode = UiThemeMode::FollowSystem;
    bool dark = g_dark;
    if (g_mode == UiThemeMode::Dark) {
        dark = true;
    } else if (g_mode == UiThemeMode::Light) {
        dark = false;
    } else {
        bool ok = false;
        const bool light = ReadAppsUseLightTheme(&ok);  // ok=false → 读不到，降级浅色
        dark = ok ? !light : false;
    }
    g_dark = dark;
    ApplyTokens(g_dpi == 0 ? 96 : g_dpi);
    DestroyFonts();
    g_inited = true;
    return g_dark;
}

bool UiThemeRefresh() {
    const bool wasDark = g_dark;
    if (g_mode != UiThemeMode::FollowSystem) {
        // 手动模式：浅/深色由调用方指定，这里只按当前 DPI 重建令牌与字体并请求重绘
        ApplyTokens(g_dpi == 0 ? 96 : g_dpi);
        DestroyFonts();
        return true;
    }
    bool ok = false;
    const bool light = ReadAppsUseLightTheme(&ok);
    g_dark = ok ? !light : false;
    const bool changed = (wasDark != g_dark);
    ApplyTokens(g_dpi == 0 ? 96 : g_dpi);
    DestroyFonts();
    if (changed) {
        DWORD_PTR res = 0;
        // 广播给全部顶层窗口（含本进程窗口），页面组据此重绘；不等待（超时 50ms 防卡死）
        SendMessageTimeoutW(HWND_BROADCAST, kThemeChangedMsg, 0, 0,
                            SMTO_ABORTIFHUNG | SMTO_NORMAL, 50, &res);
    }
    return changed;
}

void UiSetThemeMode(UiThemeMode mode) {
    g_mode = mode;
    bool dark = false;
    if (mode == UiThemeMode::Dark) {
        dark = true;
    } else if (mode == UiThemeMode::Light) {
        dark = false;
    } else {
        bool ok = false;
        const bool light = ReadAppsUseLightTheme(&ok);
        dark = ok ? !light : false;
    }
    const bool changed = (g_dark != dark);
    g_dark = dark;
    ApplyTokens(g_dpi == 0 ? 96 : g_dpi);
    DestroyFonts();
    if (changed) {
        DWORD_PTR res = 0;
        SendMessageTimeoutW(HWND_BROADCAST, kThemeChangedMsg, 0, 0,
                            SMTO_ABORTIFHUNG | SMTO_NORMAL, 50, &res);
    }
}

// -----------------------------------------------------------------------------
// 字体缓存
// -----------------------------------------------------------------------------
HFONT UiFont(UiFontRole role) {
    const int idx = static_cast<int>(role);
    if (idx < 0 || idx >= kFontRoleCount) return nullptr;
    if (g_fonts[idx] != nullptr) return g_fonts[idx];
    if (g_theme.dpi == 0) ApplyTokens(UiDpi());

    const bool bold = (role == UiFontRole::BodyBold || role == UiFontRole::Subtitle ||
                       role == UiFontRole::Title || role == UiFontRole::Display);
    const int height = -std::max(8, g_theme.fontSize[idx]);

    wchar_t face[LF_FACESIZE] = {};
    if (role == UiFontRole::Mono) QueryMonoFace(face, LF_FACESIZE);
    else QueryUIFace(face, LF_FACESIZE);

    HFONT font = CreateFontW(height, 0, 0, 0, bold ? FW_BOLD : FW_NORMAL, FALSE, FALSE, FALSE,
                             DEFAULT_CHARSET, OUT_TT_PRECIS, CLIP_DEFAULT_PRECIS,
                             CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_DONTCARE, face);
    if (font == nullptr) {
        // 兜底：系统界面字体（不花屏、不崩，仅字号不再受令牌控制）
        font = static_cast<HFONT>(GetStockObject(DEFAULT_GUI_FONT));
    }
    g_fonts[idx] = font;
    return font;
}

void UiFontShutdown() { DestroyFonts(); }

HFONT UiSelectFont(HDC dc, UiFontRole role) {
    HFONT font = UiFont(role);
    if (dc == nullptr || font == nullptr) return nullptr;
    return static_cast<HFONT>(SelectObject(dc, font));
}

HBRUSH UiFillSolid(HDC dc, COLORREF color) {
    if (dc == nullptr) return nullptr;
    HBRUSH brush = CreateSolidBrush(color);
    if (brush == nullptr) return nullptr;
    return static_cast<HBRUSH>(SelectObject(dc, brush));
}

HPEN UiStrokeSolid(HDC dc, COLORREF color, int width) {
    if (dc == nullptr) return nullptr;
    HPEN pen = CreatePen(PS_SOLID, width <= 0 ? 1 : width, color);
    if (pen == nullptr) return nullptr;
    return static_cast<HPEN>(SelectObject(dc, pen));
}

// -----------------------------------------------------------------------------
// 文本度量 / 绘制（UTF-8 入参，内部转 UTF-16 走 W API）
// -----------------------------------------------------------------------------
int UiTextWidth(HDC dc, const char* utf8) {
    if (dc == nullptr || utf8 == nullptr) return 0;
    const int need = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, nullptr, 0);
    if (need <= 1) return 0;
    std::wstring text(static_cast<size_t>(need), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8, -1, &text[0], need);
    SIZE sz{};
    if (!GetTextExtentPoint32W(dc, text.c_str(), static_cast<int>(text.size()) - 1, &sz)) return 0;
    return static_cast<int>(sz.cx);
}

int UiTextHeight(HDC dc, const char* utf8) {
    if (dc == nullptr || utf8 == nullptr) return 0;
    const int need = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, nullptr, 0);
    if (need <= 1) return 0;
    std::wstring text(static_cast<size_t>(need), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8, -1, &text[0], need);
    SIZE sz{};
    if (!GetTextExtentPoint32W(dc, text.c_str(), static_cast<int>(text.size()) - 1, &sz)) return 0;
    return static_cast<int>(sz.cy);
}

void UiDrawText(HDC dc, const char* utf8, const RECT& rc, UINT flags, COLORREF color) {
    if (dc == nullptr || utf8 == nullptr) return;
    const int need = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, nullptr, 0);
    if (need <= 1) return;
    std::wstring text(static_cast<size_t>(need), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8, -1, &text[0], need);
    const int prevMode = SetBkMode(dc, TRANSPARENT);
    const COLORREF prevColor = SetTextColor(dc, color);
    RECT r = rc;
    DrawTextW(dc, text.c_str(), static_cast<int>(text.size()) - 1, &r,
              (flags | DT_NOPREFIX) & ~DT_EDITCONTROL);
    SetTextColor(dc, prevColor);
    SetBkMode(dc, prevMode);
}

// -----------------------------------------------------------------------------
// 圆角矩形（GDI RoundRect：内部用椭圆四角拼接，达到圆角填充的抗锯齿外观）
// -----------------------------------------------------------------------------
void UiFillRoundRect(HDC dc, const RECT& rc, int radius, COLORREF fill) {
    if (dc == nullptr || rc.right <= rc.left || rc.bottom <= rc.top) return;
    HBRUSH brush = CreateSolidBrush(fill);
    HGDIOBJ oldBrush = brush != nullptr ? SelectObject(dc, brush) : nullptr;
    const int prevMode = SetBkMode(dc, TRANSPARENT);
    HPEN pen = CreatePen(PS_SOLID, 1, fill);
    HGDIOBJ oldPen = pen != nullptr ? SelectObject(dc, pen) : nullptr;
    const int e = std::max(0, radius) * 2;
    if (RoundRect(dc, rc.left, rc.top, rc.right, rc.bottom, e, e) == 0) {
        // 极端小矩形下 RoundRect 可能失败：退回直角填充，保证内容仍可见
        RECT r = rc;
        HBRUSH fb = CreateSolidBrush(fill);
        FillRect(dc, &r, fb);
        if (fb != nullptr) DeleteObject(fb);
    }
    if (oldPen != nullptr) SelectObject(dc, oldPen);
    if (oldBrush != nullptr) SelectObject(dc, oldBrush);
    if (pen != nullptr) DeleteObject(pen);
    if (brush != nullptr) DeleteObject(brush);
    SetBkMode(dc, prevMode);
}

void UiStrokeRoundRect(HDC dc, const RECT& rc, int radius, COLORREF stroke, int width) {
    if (dc == nullptr || rc.right <= rc.left || rc.bottom <= rc.top) return;
    const int w = width <= 0 ? 1 : width;
    HPEN pen = CreatePen(PS_SOLID, w, stroke);
    HGDIOBJ oldPen = pen != nullptr ? SelectObject(dc, pen) : nullptr;
    HGDIOBJ oldBrush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    const int e = std::max(0, radius) * 2;
    RECT r = rc;
    // 让描边落在矩形内沿，避免半像素溢出到相邻卡片
    InflateRect(&r, -w / 2, -w / 2);
    RoundRect(dc, r.left, r.top, r.right, r.bottom, e, e);
    if (oldBrush != nullptr) SelectObject(dc, oldBrush);
    if (oldPen != nullptr) SelectObject(dc, oldPen);
    if (pen != nullptr) DeleteObject(pen);
}

}  // namespace ui
}  // namespace gopt
