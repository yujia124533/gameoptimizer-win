#pragma once
// =============================================================================
// GameOptimizer v1.1.0 — UI 基础层：设计令牌（design tokens）+ 主题 + 高 DPI
// -----------------------------------------------------------------------------
// 仅使用官方 Win32 API（user32 / gdi32 / advapi32），无第三方库、无 Hook。
//
// 【页面组怎么用】
//   1) 启动（WinMain 最开头，创建任何窗口/句柄之前）：
//          UiEnablePerMonitorDpi();     // 动态调用 SetProcessDpiAwarenessContext，失败自动降级
//          UiThemeInit();               // 探测跟随系统主题（HKCU\...\Personalize\AppsUseLightTheme）
//   2) 窗口过程 WM_CREATE 末尾：
//          UiWidgetsSelfTest();         // 可选：自检全部控件绘制路径，返回 false 说明有漏画（写日志用）
//   3) 取色/间距/字体（全部经函数取，别硬编码 RGB）：
//          COLORREF c = UiColor(UiColorRole::Surface);
//          int gap    = UiSp(UiSpace::Md);            // 8px 栅格，已按 DPI 缩放
//          HFONT f    = UiFont(UiFontRole::Body);
//   4) 绘按钮/卡片/列表/进度条/徽标/提示条 → ui_widgets.h
//   5) WM_DPICHANGED / 主题变化：调用 UiOnDpiChanged() / UiThemeRefresh()，再 InvalidateRect。
//
// 【高 DPI 降级行为】见 ui_theme.cpp 顶部说明。
// =============================================================================

#include <windows.h>

namespace gopt {
namespace ui {

// -----------------------------------------------------------------------------
// 1. 色彩令牌（浅色/深色两套，语义命名，禁止业务代码直接写 RGB）
// -----------------------------------------------------------------------------
enum class UiColorRole {
    WindowBg,        // 窗口底
    PanelBg,         // 页容器/面板底
    Surface,         // 卡片底
    SurfaceAlt,      // 卡片次层/斑马纹
    SurfaceHover,    // 卡片/行悬停
    Border,          // 常规描边
    BorderStrong,    // 强调描边
    Divider,         // 分隔线
    TextPrimary,     // 正文
    TextSecondary,   // 次要说明
    TextMuted,       // 更弱的辅助文字
    TextOnAccent,    // 主题色底上的文字
    Accent,          // 品牌主题色
    AccentHover,     // 主题色悬停
    AccentPressed,   // 主题色按下
    AccentSoft,      // 主题色浅底（选中行/软标签）
    DisabledBg,      // 禁用底
    DisabledText,    // 禁用文字
    FocusRing,       // 键盘焦点环
    Success,         // 状态：成功
    SuccessBg,       // 状态：成功浅底
    Warning,         // 状态：警告
    WarningBg,       // 状态：警告浅底
    Danger,          // 状态：失败
    DangerBg,        // 状态：失败浅底
    Info,            // 状态：进行中
    InfoBg,          // 状态：进行中浅底
    Track,           // 进度条轨道
    Shadow,          // 卡片投影
    Overlay,         // 覆盖层底
    Count            // 令牌数量（内部使用）
};

// -----------------------------------------------------------------------------
// 2. 间距令牌（8px 栅格）
// -----------------------------------------------------------------------------
enum class UiSpace { Xxs, Xs, Sm, Md, Lg, Xl, Xxl };

// -----------------------------------------------------------------------------
// 3. 字号阶梯（按 DPI 缩放的实际像素高度）
// -----------------------------------------------------------------------------
enum class UiFontRole {
    Caption,      // 11pt 级：次要说明/徽标
    Body,         // 12pt 级：正文
    BodyBold,     // 正文加粗
    Subtitle,     // 14pt 级：小标题/卡片标题
    Title,        // 16pt 级：页面标题
    Display,      // 22pt 级：大号数字（CPU%/分数等）
    Mono,         // 等宽：日志/路径
    Count
};

// -----------------------------------------------------------------------------
// 4. 圆角 / 控件高度令牌
// -----------------------------------------------------------------------------
enum class UiRadius { Sm, Md, Lg, Pill };      // 4 / 8 / 12 / 999(胶囊)
enum class UiControlH { Sm, Md, Lg, Xl };      // 24 / 32 / 40 / 56

// 数组维度常量（与上面枚举一一对应，ui_theme.cpp 内有 static_assert 保证不漂移）
constexpr int kColorRoleCount = 30;   // UiColorRole: WindowBg..Overlay
constexpr int kSpaceCount     = 7;    // UiSpace: Xxs..Xxl
constexpr int kFontRoleCount  = 7;    // UiFontRole: Caption..Mono
constexpr int kRadiusCount    = 4;    // UiRadius: Sm..Pill
constexpr int kControlHCount  = 4;    // UiControlH: Sm..Xl

// 令牌聚合体（内部存储；页面组通过下列函数读取，不要直接碰结构体字段）
struct Theme {
    COLORREF color[kColorRoleCount];
    int      space[kSpaceCount];
    int      fontSize[kFontRoleCount];
    int      radius[kRadiusCount];
    int      controlH[kControlHCount];
    DWORD    dpi;            // 令牌生成时的 DPI（96 为 1.0x）
};

// -----------------------------------------------------------------------------
// 5. 主题 / DPI 接口
// -----------------------------------------------------------------------------
// 探测并应用主题。bFollowSystem=true 时读取 HKCU\Software\Microsoft\Windows\CurrentVersion
// \Themes\Personalize\AppsUseLightTheme（稳定注册表 API：RegOpenKeyExW/RegQueryValueExW）；
// 读不到（键不存在/无权限/非 Windows10+）降级为浅色。返回 true 表示当前为深色。
bool UiThemeInit(bool bFollowSystem = true);

// 运行中刷新：重新读取系统主题（仅在跟随系统模式下生效），重建令牌与字体，并在主题真的
// 变化时向全部顶层窗口广播 UiThemeChangedMessage()。
// 返回 true = 「令牌已重建，调用方应当重绘」：跟随系统模式下等价于「浅/深色发生切换」，
// 手动指定模式（UiThemeMode::Light/Dark）下始终为 true（该模式下主题由调用方指定，函数
// 只负责按当前 DPI 重建令牌与字体）。返回 false = 主题与令牌都没有变化。
bool UiThemeRefresh();

// 手动指定主题（"浅色/深色/跟随系统"三态保存在 ui_theme 内部，可持久化到配置的接口）
enum class UiThemeMode { FollowSystem, Light, Dark };
void        UiSetThemeMode(UiThemeMode mode);
UiThemeMode UiGetThemeMode();
bool        UiIsDark();

const Theme& UiTheme();
inline COLORREF UiColor(UiColorRole r) {
    return UiTheme().color[static_cast<int>(r)];
}
inline int UiSp(UiSpace s) { return UiTheme().space[static_cast<int>(s)]; }
inline int UiFontPx(UiFontRole r) { return UiTheme().fontSize[static_cast<int>(r)]; }
inline int UiRadiusPx(UiRadius r) { return UiTheme().radius[static_cast<int>(r)]; }
inline int UiControlHeight(UiControlH h) { return UiTheme().controlH[static_cast<int>(h)]; }

// 主题变化广播消息（ui_widgets 用它作为「重绘入口」；页面组想额外刷新标签文案可自行处理）
UINT UiThemeChangedMessage();

// -----------------------------------------------------------------------------
// 6. 高 DPI
// -----------------------------------------------------------------------------
// per-monitor DPI 感知：动态 GetProcAddress(SetProcessDpiAwarenessContext)，
// 失败依次尝试 SetProcessDpiAwareness / SetProcessDPIAware（老系统降级：系统级 DPI 感知）。
// 必须在创建任何窗口之前调用。返回实际生效级别（0=都失败，1=SystemAware，2=PerMonitor，3=PerMonitorV2）。
int UiEnablePerMonitorDpi();

// 当前进程级 DPI 因子（1.0 == 96 DPI）。未启用 DPI 感知时由系统缩放，仍返回 1.0，
// 此时所有坐标按「逻辑像素」处理，视觉与 v1.0.19 一致。
double UiDpiFactor();
UINT   UiDpi();

// 指定窗口的 DPI / 缩放（窗口跨显示器时逐窗口取值，优先 GetDpiForWindow）
UINT UiDpiForWindow(HWND hwnd);
double UiScaleForWindow(HWND hwnd);

// 缩放工具：坐标/尺寸/矩形
inline int    UiScale(int v) { return MulDiv(v, static_cast<int>(UiDpi()), 96); }
inline int    UiUnscale(int v) { return MulDiv(v, 96, static_cast<int>(UiDpi())); }
inline int    UiScaleAt(int v, UINT dpi) { return MulDiv(v, static_cast<int>(dpi), 96); }
inline RECT   UiScaleRect(const RECT& r) {
    RECT o{};
    o.left   = UiScale(r.left);
    o.top    = UiScale(r.top);
    o.right  = UiScale(r.right);
    o.bottom = UiScale(r.bottom);
    return o;
}
inline POINT  UiScalePoint(const POINT& p) { POINT o{ UiScale(p.x), UiScale(p.y) }; return o; }

// DPI 变化处理：清空缩放缓存并重建字体（页面组在 WM_DPICHANGED 里调用，然后按 lParam
// 给出的 RECT 重排布局、InvalidateRect(TRUE)）。返回新 DPI。
UINT UiOnDpiChanged(UINT newDpi = 0);

// 自动缩放辅助：把「96 DPI 逻辑坐标」一次批量转成物理坐标（对话框/布局表用）
void UiScaleSpan(const int* src, int* dst, int count);

// -----------------------------------------------------------------------------
// 7. 字体
// -----------------------------------------------------------------------------
// 取（缓存的）字体。首选取 "Microsoft YaHei UI"（中英混排），缺失时用
// SystemParametersInfoW(SPI_GETNONCLIENTMETRICS) 的界面字体兜底；创建失败返回 nullptr，
// 此时页面组应跳过 WM_SETFONT 并退回系统默认字体（不崩、不花屏）。
HFONT UiFont(UiFontRole role);

// GDI 便捷封装（全部选择新对象并返回旧对象，调用方负责 SelectObject 还原）
HFONT UiSelectFont(HDC dc, UiFontRole role);
HBRUSH UiFillSolid(HDC dc, COLORREF color);
HPEN   UiStrokeSolid(HDC dc, COLORREF color, int width = 1);

// 文本度量/绘制（以当前选中的字体为准；失败返回 0）
int UiTextWidth(HDC dc, const char* utf8);
int UiTextHeight(HDC dc, const char* utf8);
// flags 为 DT_* 组合（如 DT_CENTER|DT_VCENTER|DT_SINGLELINE）；内部自动加 DT_NOPREFIX
void UiDrawText(HDC dc, const char* utf8, const RECT& rc, UINT flags, COLORREF color);

// 圆角矩形填充/描边（内部用 RoundRect，GDI 原生抗锯齿边缘）
void UiFillRoundRect(HDC dc, const RECT& rc, int radius, COLORREF fill);
void UiStrokeRoundRect(HDC dc, const RECT& rc, int radius, COLORREF stroke, int width = 1);

// 释放字体缓存（WM_DESTROY / 进程退出时调用；调用后再次 UiFont 会自动重建）
void UiFontShutdown();

}  // namespace ui
}  // namespace gopt
