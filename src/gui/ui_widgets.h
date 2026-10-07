#pragma once
// =============================================================================
// GameOptimizer v1.1.0 — UI 基础层：统一自绘控件（GDI / 官方 Win32 API）
// -----------------------------------------------------------------------------
// 设计约束：
//   * 只用 user32/gdi32（+ ui_theme 的令牌），无第三方渲染库、无 Hook；
//   * 所有函数都是「纯绘制」函数：给 HDC + RECT + 状态就画，不创建窗口、不装钩子，
//     页面组用消息驱动的 WM_DRAWITEM / WM_CTLCOLOR* / WM_PAINT 调它们即可；
//   * 全部坐标按 ui_theme 的 DPI 令牌缩放取（UiSp/UiControlHeight/UiRadiusPx），
//     禁止在控件函数里写死像素值。
//
// 【页面组推荐用法】以左侧导航为例（v1.0.19 已是 BS_OWNERDRAW 按钮）：
//   // WM_DRAWITEM
//   case WM_DRAWITEM: {
//       auto* di = reinterpret_cast<DRAWITEMSTRUCT*>(lp);
//       UiButtonState st = UiButtonStateFromDrawItem(di);          // 常态/悬停/按下/禁用/焦点
//       RECT rc = di->rcItem;
//       if (UiDrawButton(di->hDC, rc, st, LabelOf(di->CtlID), /*primary=*/di->CtlID == IDC_BIGOPT))
//           return TRUE;
//       break;                                                     // 非本层控件 → 交回默认绘制
//   }
//   // WM_MOUSEMOVE（自己记 hover id，用 TrackMouseEvent 收 WM_MOUSELEAVE 清空）
//   g_hotId = static_cast<int>(UiHitTestId(hwnd, pt));
//   // WM_THEMECHANGED / UiThemeChangedMessage() → InvalidateRect(hwnd, nullptr, TRUE)
//
// 其余控件（卡片/列表行/进度条/徽标/提示条）不需要窗口句柄，直接在 WM_PAINT 里画。
// 自检：UiWidgetsSelfTest() 会把本文件所有绘制函数跑一遍，返回 false 说明有分支未成功出图。
// =============================================================================

#include <windows.h>

#include <string>

#include "gui/ui_theme.h"

namespace gopt {
namespace ui {

// -----------------------------------------------------------------------------
// 控件状态
// -----------------------------------------------------------------------------
enum class UiButtonState { Normal, Hot, Pressed, Focused, Disabled };
// 注意：ui_widgets.cpp 内的状态位映射与 DRAWITEMSTRUCT::itemState 一一对应：
//   ODS_SELECTED → Pressed，ODS_DISABLED → Disabled，ODS_FOCUS → Focused，ODS_HOTLIGHT → Hot。

// 语义色调：徽标/提示条/进度条共用
enum class UiTone { Neutral, Accent, Success, Warning, Danger, Info };

// -----------------------------------------------------------------------------
// 1. 按钮（常态 / 悬停 / 按下 / 焦点 / 禁用）
// -----------------------------------------------------------------------------
// 画按钮。bPrimary=true 用主题色实底（主操作），false 为次级描边按钮。
// 返回 true 表示本次绘制生效（调用方应 return TRUE 吞掉默认绘制）。
bool UiDrawButton(HDC dc, const RECT& rc, UiButtonState state, const std::string& textUtf8,
                  bool bPrimary = false);

// 由 WM_DRAWITEM 推导按钮状态（含键盘焦点）；非按钮控件返回 Normal，调用方自行判 CtlType。
UiButtonState UiButtonStateFromDrawItem(const DRAWITEMSTRUCT* di);

// 逻辑坐标 → 控件 ID 命中测试（配合 WM_MOUSEMOVE 维护 hover 高亮）。
// 坐标已按窗口 DPI 缩放；bVisibleOnly=true 时跳过不可见/禁用窗口。
int UiHitTestId(HWND parent, POINT ptLogical, ULONG_PTR firstId, ULONG_PTR lastId,
                bool bVisibleOnly = true);

// 按文本与内边距算按钮最小宽度（DPI 令牌语义）
int UiButtonMinWidth(HDC dc, const std::string& textUtf8, bool bPrimary = false);

// -----------------------------------------------------------------------------
// 2. 卡片 / 区块
// -----------------------------------------------------------------------------
// 卡片：圆角 Surface 底 + 描边（可在卡内再叠加标题/数值）。bBorder=false 画无边框面板。
void UiDrawCard(HDC dc, const RECT& rc, bool bBorder = true);
// 卡片标题（Subtitle 字体，正文色，左对齐，单行省略）
void UiDrawCardTitle(HDC dc, const RECT& rc, const std::string& titleUtf8);
// 大号数字（Display 字体，用于 CPU%/分数/进程数等），color 默认主题色
void UiDrawMetric(HDC dc, const RECT& rc, const std::string& textUtf8, COLORREF color);
inline void UiDrawMetric(HDC dc, const RECT& rc, const std::string& textUtf8) {
    UiDrawMetric(dc, rc, textUtf8, UiColor(UiColorRole::Accent));
}
// 分隔线（1px，Divider 令牌）
void UiDrawDivider(HDC dc, const RECT& rc);

// -----------------------------------------------------------------------------
// 3. 列表行（替代裸 LISTBOX 的自绘行；键盘选中/悬停/斑马纹）
// -----------------------------------------------------------------------------
enum class UiRowState { Normal, Hover, Selected, Disabled };
void UiDrawListRow(HDC dc, const RECT& rc, UiRowState state, const std::string& textUtf8,
                   bool bZebra = false);
// 右侧对齐的次要文本（CPU%/路径等）
void UiDrawListRowRight(HDC dc, const RECT& rc, const std::string& textUtf8, UiRowState state);

// -----------------------------------------------------------------------------
// 4. 进度条
// -----------------------------------------------------------------------------
// progress 为 0.0~1.0（越界自动夹取）。state 取色调（默认 Accent）；bRunning=true 表示
// 「进行中」，未完成段用 Info/Earlier 底衬托。返回 false 表示矩形太小或进度非法，未出图。
bool UiDrawProgress(HDC dc, const RECT& rc, double progress, UiTone tone = UiTone::Accent);
bool UiDrawProgress(HDC dc, const RECT& rc, double progress, const std::string& labelUtf8,
                    UiTone tone = UiTone::Accent);

// -----------------------------------------------------------------------------
// 5. 徽标 / 提示条
// -----------------------------------------------------------------------------
// 胶囊徽标；返回实际绘制宽度（用于横向排布多个徽标）
int UiDrawBadge(HDC dc, int x, int y, const std::string& textUtf8, UiTone tone = UiTone::Neutral);
int UiBadgeWidth(HDC dc, const std::string& textUtf8);
// 状态提示条：tone 选底色，左侧画状态圆点，文本单行省略。返回绘制高度。
int UiDrawHintBanner(HDC dc, const RECT& rc, UiTone tone, const std::string& textUtf8);

// -----------------------------------------------------------------------------
// 6. 文件内自检（队长/页面组用来确认所有绘制函数可用；成功返回 true）
// -----------------------------------------------------------------------------
bool UiWidgetsSelfTest(HDC dc);

// -----------------------------------------------------------------------------
// 7. 页面组内部辅助：双缓冲 Paint（消除闪烁；CreateCompatibleDC/CreateDIBSection 官方 API）
// -----------------------------------------------------------------------------
struct UIPaintBuffer {
    HDC     dc = nullptr;      // 目标绘制 DC（离屏）
    HDC     target = nullptr;  // 真实窗口 DC
    HBITMAP bmp = nullptr;
    HBITMAP oldBmp = nullptr;
    RECT    rc{};
    int     width = 0, height = 0;
    bool    ready = false;
};
// 创建/复用离屏缓冲（宽高变化时自动重建）。ready=false 时页面组应退回直接画到窗口 DC。
void        UIPaintBufferBegin(UIPaintBuffer& buf, HDC target, const RECT& rc);
// 把离屏缓冲贴到窗口（BitBlt）
void        UIPaintBufferEnd(UIPaintBuffer& buf);
void        UIPaintBufferFree(UIPaintBuffer& buf);

// 文本辅助（UTF-8，超长自动省略号，绝不刷出矩形外）
void UiDrawTextClamped(HDC dc, const std::string& textUtf8, const RECT& rc, UINT flags,
                       COLORREF color, UiFontRole role);
// 自动换行文本（返回实际占用高度，供排版累加 Y）
int  UiDrawTextWrap(HDC dc, const std::string& textUtf8, const RECT& rc, COLORREF color,
                    UiFontRole role);
// 以 (x, y) 左上角为锚点画一行文本（返回文本宽度）
int UiDrawTextAt(HDC dc, int x, int y, const std::string& textUtf8, COLORREF color,
                 UiFontRole role);

// 色调 → 前景/背景令牌（进度条/边框/圆点用）
COLORREF UiToneFg(UiTone tone);
COLORREF UiToneBg(UiTone tone);

}  // namespace ui
}  // namespace gopt
