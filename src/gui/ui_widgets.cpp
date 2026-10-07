// =============================================================================
// GameOptimizer v1.1.0 — UI 基础层实现：统一自绘控件
// -----------------------------------------------------------------------------
// API 白名单：user32（DrawTextW/GetClientRect/GetWindowRect/GetDC/IsWindowVisible/
// ScreenToClient/GetWindow）、gdi32（CreateSolidBrush·RoundRect·FillRect·BitBlt·
// TextOutW·SetTextColor·CreateCompatibleDC·CreateDIBSection）。
// 无第三方库、无 Hook、无子类化、无全局钩子；所有控件均为「纯绘制 + 命中测试」函数，
// 由页面组在既有窗口过程里调用（不改变消息流，便于灰度与回滚）。
// =============================================================================

#include "gui/ui_widgets.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <string>

namespace gopt {
namespace ui {
namespace {

int g_drawCallCount = 0;  // 供自检确认「每条绘制路径都真的被调用」

inline void ResetLastError() { SetLastError(ERROR_SUCCESS); }

// UTF-8 → UTF-16（转换失败/空串返回空）
std::wstring ToWide(const std::string& s) {
    if (s.empty()) return std::wstring();
    const int need = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), static_cast<int>(s.size()), nullptr, 0);
    if (need <= 0) return std::wstring();
    std::wstring out(static_cast<size_t>(need), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), static_cast<int>(s.size()), &out[0], need);
    return out;
}

// 等宽数字（亚像素走查用不到，这里只是取整，避免负数取模）
inline int ClampInt(int v, int lo, int hi) {
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}

inline bool IsEmptyRect(const RECT& r) { return r.right <= r.left || r.bottom <= r.top; }

// 圆角填充：统一走 ui_theme 的 UiFillRoundRect（单一实现口径）；1~3px 的极小矩形退回直角
void FillRound(HDC dc, const RECT& rc, int radius, COLORREF color) {
    if (IsEmptyRect(rc)) return;
    HBRUSH brush = CreateSolidBrush(color);
    if (brush == nullptr) return;
    HGDIOBJ oldBrush = SelectObject(dc, brush);
    HPEN pen = CreatePen(PS_SOLID, 1, color);
    HGDIOBJ oldPen = pen != nullptr ? SelectObject(dc, pen) : nullptr;
    const int w = rc.right - rc.left, h = rc.bottom - rc.top;
    bool drawn = false;
    if (radius > 0 && w >= 4 && h >= 4) {
        ResetLastError();
        const int e = radius * 2;
        drawn = RoundRect(dc, rc.left, rc.top, rc.right, rc.bottom, e, e) != 0;
    }
    if (!drawn) {
        RECT r = rc;
        FillRect(dc, &r, brush);
    }
    if (oldPen != nullptr) SelectObject(dc, oldPen);
    if (oldBrush != nullptr) SelectObject(dc, oldBrush);
    if (pen != nullptr) DeleteObject(pen);
    DeleteObject(brush);
}

}  // namespace

// -----------------------------------------------------------------------------
// 色调令牌
// -----------------------------------------------------------------------------
COLORREF UiToneFg(UiTone tone) {
    switch (tone) {
        case UiTone::Accent:  return UiColor(UiColorRole::Accent);
        case UiTone::Success: return UiColor(UiColorRole::Success);
        case UiTone::Warning: return UiColor(UiColorRole::Warning);
        case UiTone::Danger:  return UiColor(UiColorRole::Danger);
        case UiTone::Info:    return UiColor(UiColorRole::Info);
        case UiTone::Neutral:
        default:              return UiColor(UiColorRole::TextSecondary);
    }
}

COLORREF UiToneBg(UiTone tone) {
    switch (tone) {
        case UiTone::Accent:  return UiColor(UiColorRole::AccentSoft);
        case UiTone::Success: return UiColor(UiColorRole::SuccessBg);
        case UiTone::Warning: return UiColor(UiColorRole::WarningBg);
        case UiTone::Danger:  return UiColor(UiColorRole::DangerBg);
        case UiTone::Info:    return UiColor(UiColorRole::InfoBg);
        case UiTone::Neutral:
        default:              return UiColor(UiColorRole::SurfaceAlt);
    }
}

// -----------------------------------------------------------------------------
// 文本辅助
// -----------------------------------------------------------------------------
void UiDrawTextClamped(HDC dc, const std::string& textUtf8, const RECT& rc, UINT flags,
                       COLORREF color, UiFontRole role) {
    if (dc == nullptr || IsEmptyRect(rc)) return;
    const std::wstring w = ToWide(textUtf8);
    if (w.empty()) return;
    HGDIOBJ oldFont = UiSelectFont(dc, role);
    const COLORREF oldColor = SetTextColor(dc, color);
    const int oldMode = SetBkMode(dc, TRANSPARENT);
    RECT r = rc;
    ResetLastError();
    DrawTextW(dc, w.c_str(), static_cast<int>(w.size()), &r,
              flags | DT_NOPREFIX | DT_END_ELLIPSIS | DT_SINGLELINE);
    SetBkMode(dc, oldMode);
    SetTextColor(dc, oldColor);
    if (oldFont != nullptr) SelectObject(dc, oldFont);
    ++g_drawCallCount;
}

int UiDrawTextWrap(HDC dc, const std::string& textUtf8, const RECT& rc, COLORREF color,
                   UiFontRole role) {
    if (dc == nullptr || IsEmptyRect(rc)) return 0;
    const std::wstring w = ToWide(textUtf8);
    if (w.empty()) return 0;
    HGDIOBJ oldFont = UiSelectFont(dc, role);
    const COLORREF oldColor = SetTextColor(dc, color);
    const int oldMode = SetBkMode(dc, TRANSPARENT);
    RECT r = rc;
    const int height = DrawTextW(dc, w.c_str(), static_cast<int>(w.size()), &r,
                                 DT_NOPREFIX | DT_WORDBREAK | DT_EDITCONTROL);
    SetBkMode(dc, oldMode);
    SetTextColor(dc, oldColor);
    if (oldFont != nullptr) SelectObject(dc, oldFont);
    ++g_drawCallCount;
    return height;
}

int UiDrawTextAt(HDC dc, int x, int y, const std::string& textUtf8, COLORREF color,
                 UiFontRole role) {
    if (dc == nullptr) return 0;
    const std::wstring w = ToWide(textUtf8);
    if (w.empty()) return 0;
    HGDIOBJ oldFont = UiSelectFont(dc, role);
    const COLORREF oldColor = SetTextColor(dc, color);
    const int oldMode = SetBkMode(dc, TRANSPARENT);
    ResetLastError();
    const BOOL ok = TextOutW(dc, x, y, w.c_str(), static_cast<int>(w.size()));
    SetBkMode(dc, oldMode);
    SetTextColor(dc, oldColor);
    if (oldFont != nullptr) SelectObject(dc, oldFont);
    const int len = ok ? UiTextWidth(dc, textUtf8.c_str()) : 0;
    ++g_drawCallCount;
    return len;
}

// -----------------------------------------------------------------------------
// 1. 按钮
// -----------------------------------------------------------------------------
bool UiDrawButton(HDC dc, const RECT& rc, UiButtonState state, const std::string& textUtf8,
                  bool bPrimary) {
    if (dc == nullptr || IsEmptyRect(rc)) return false;
    const int radius = UiRadiusPx(UiRadius::Md);

    COLORREF fill = UiColor(UiColorRole::Surface);
    COLORREF border = UiColor(UiColorRole::BorderStrong);
    COLORREF text = UiColor(UiColorRole::TextPrimary);

    if (bPrimary) {
        fill = UiColor(UiColorRole::Accent);
        border = UiColor(UiColorRole::Accent);
        text = UiColor(UiColorRole::TextOnAccent);
        switch (state) {
            case UiButtonState::Hot:      fill = UiColor(UiColorRole::AccentHover);   break;
            case UiButtonState::Pressed:  fill = UiColor(UiColorRole::AccentPressed); break;
            case UiButtonState::Focused:  border = UiColor(UiColorRole::FocusRing);   break;
            case UiButtonState::Disabled: fill = UiColor(UiColorRole::DisabledBg);
                                          border = UiColor(UiColorRole::Border);
                                          text = UiColor(UiColorRole::DisabledText);  break;
            case UiButtonState::Normal:
            default: break;
        }
    } else {
        switch (state) {
            case UiButtonState::Hot:      fill = UiColor(UiColorRole::SurfaceHover);
                                          border = UiColor(UiColorRole::Accent);       break;
            case UiButtonState::Pressed:  fill = UiColor(UiColorRole::AccentSoft);
                                          border = UiColor(UiColorRole::AccentPressed); break;
            case UiButtonState::Focused:  border = UiColor(UiColorRole::FocusRing);    break;
            case UiButtonState::Disabled: fill = UiColor(UiColorRole::DisabledBg);
                                          border = UiColor(UiColorRole::Border);
                                          text = UiColor(UiColorRole::DisabledText);  break;
            case UiButtonState::Normal:
            default: break;
        }
    }
    FillRound(dc, rc, radius, fill);

    // 焦点环：内缩 1px 的描边（不改布局，仅视觉）
    if (state == UiButtonState::Focused) {
        RECT inner = rc;
        InflateRect(&inner, -1, -1);
        UiStrokeRoundRect(dc, inner, radius, UiColor(UiColorRole::FocusRing), 2);
    } else if (!bPrimary || state == UiButtonState::Disabled) {
        UiStrokeRoundRect(dc, rc, radius, border, 1);
    }

    RECT textRc = rc;
    textRc.left += UiSp(UiSpace::Sm);
    textRc.right -= UiSp(UiSpace::Sm);
    const std::wstring w = ToWide(textUtf8);
    if (!w.empty()) {
        HGDIOBJ oldFont = UiSelectFont(dc, UiFontRole::BodyBold);
        const COLORREF oldColor = SetTextColor(dc, text);
        const int oldMode = SetBkMode(dc, TRANSPARENT);
        ResetLastError();
        DrawTextW(dc, w.c_str(), static_cast<int>(w.size()), &textRc,
                  DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        SetBkMode(dc, oldMode);
        SetTextColor(dc, oldColor);
        if (oldFont != nullptr) SelectObject(dc, oldFont);
    }
    ++g_drawCallCount;
    return true;
}

UiButtonState UiButtonStateFromDrawItem(const DRAWITEMSTRUCT* di) {
    if (di == nullptr) return UiButtonState::Normal;
    const UINT s = di->itemState;
    if ((s & ODS_DISABLED) != 0) return UiButtonState::Disabled;
    if ((s & ODS_SELECTED) != 0) return UiButtonState::Pressed;
    if ((s & ODS_FOCUS) != 0) return UiButtonState::Focused;
    if ((s & ODS_HOTLIGHT) != 0) return UiButtonState::Hot;
    return UiButtonState::Normal;
}

int UiHitTestId(HWND parent, POINT ptLogical, ULONG_PTR firstId, ULONG_PTR lastId,
                bool bVisibleOnly) {
    if (parent == nullptr) return 0;
    const UINT dpi = UiDpiForWindow(parent);
    for (ULONG_PTR id = firstId; id <= lastId; ++id) {
        HWND child = GetDlgItem(parent, static_cast<int>(id));
        if (child == nullptr || !IsWindowVisible(child)) {
            if (bVisibleOnly) continue;
            if (child == nullptr) continue;
        }
        if (!IsWindowEnabled(child) && bVisibleOnly) continue;
        RECT r{};
        if (!GetWindowRect(child, &r)) continue;
        POINT tl{ r.left, r.top };
        POINT br{ r.right, r.bottom };
        ScreenToClient(parent, &tl);
        ScreenToClient(parent, &br);
        (void)dpi;  // 命中测试用物理像素：与 WM_MOUSEMOVE 的 lParam 同一坐标系
        if (ptLogical.x >= tl.x && ptLogical.x < br.x && ptLogical.y >= tl.y && ptLogical.y < br.y) {
            return static_cast<int>(id);
        }
    }
    return 0;
}

int UiButtonMinWidth(HDC dc, const std::string& textUtf8, bool bPrimary) {
    const int pad = bPrimary ? UiSp(UiSpace::Lg) : UiSp(UiSpace::Md);
    const int textW = UiTextWidth(dc, textUtf8.c_str());
    return std::max(UiControlHeight(UiControlH::Md), textW + 2 * pad + UiSp(UiSpace::Xs));
}

// -----------------------------------------------------------------------------
// 2. 卡片 / 区块
// -----------------------------------------------------------------------------
void UiDrawCard(HDC dc, const RECT& rc, bool bBorder) {
    if (dc == nullptr || IsEmptyRect(rc)) return;
    FillRound(dc, rc, UiRadiusPx(UiRadius::Lg), UiColor(UiColorRole::Surface));
    if (bBorder) UiStrokeRoundRect(dc, rc, UiRadiusPx(UiRadius::Lg), UiColor(UiColorRole::Border), 1);
    ++g_drawCallCount;
}

void UiDrawCardTitle(HDC dc, const RECT& rc, const std::string& titleUtf8) {
    UiDrawTextClamped(dc, titleUtf8, rc, DT_LEFT | DT_VCENTER, UiColor(UiColorRole::TextPrimary),
                      UiFontRole::Subtitle);
}

void UiDrawMetric(HDC dc, const RECT& rc, const std::string& textUtf8, COLORREF color) {
    UiDrawTextClamped(dc, textUtf8, rc, DT_LEFT | DT_BOTTOM, color, UiFontRole::Display);
}

void UiDrawDivider(HDC dc, const RECT& rc) {
    if (dc == nullptr || IsEmptyRect(rc)) return;
    RECT line = rc;
    line.top = rc.top + (rc.bottom - rc.top) / 2;
    line.bottom = line.top + std::max(1, UiScale(1));
    HBRUSH brush = CreateSolidBrush(UiColor(UiColorRole::Divider));
    if (brush != nullptr) {
        FillRect(dc, &line, brush);
        DeleteObject(brush);
    }
    ++g_drawCallCount;
}

// -----------------------------------------------------------------------------
// 3. 列表行
// -----------------------------------------------------------------------------
void UiDrawListRow(HDC dc, const RECT& rc, UiRowState state, const std::string& textUtf8,
                   bool bZebra) {
    if (dc == nullptr || IsEmptyRect(rc)) return;
    COLORREF bg = UiColor(UiColorRole::Surface);
    COLORREF fg = UiColor(UiColorRole::TextPrimary);
    switch (state) {
        case UiRowState::Hover:    bg = UiColor(UiColorRole::SurfaceHover); break;
        case UiRowState::Selected: bg = UiColor(UiColorRole::AccentSoft);
                                   fg = UiColor(UiColorRole::TextPrimary);  break;
        case UiRowState::Disabled: bg = UiColor(UiColorRole::DisabledBg);
                                   fg = UiColor(UiColorRole::DisabledText); break;
        case UiRowState::Normal:
        default:                   bg = bZebra ? UiColor(UiColorRole::SurfaceAlt)
                                              : UiColor(UiColorRole::Surface); break;
    }
    HBRUSH brush = CreateSolidBrush(bg);
    if (brush != nullptr) {
        FillRect(dc, &rc, brush);
        DeleteObject(brush);
    }
    RECT textRc = rc;
    textRc.left += UiSp(UiSpace::Sm);
    textRc.right -= UiSp(UiSpace::Sm);
    UiDrawTextClamped(dc, textUtf8, textRc, DT_LEFT | DT_VCENTER, fg, UiFontRole::Body);
    ++g_drawCallCount;
}

void UiDrawListRowRight(HDC dc, const RECT& rc, const std::string& textUtf8, UiRowState state) {
    if (dc == nullptr || IsEmptyRect(rc)) return;
    const COLORREF fg = (state == UiRowState::Disabled) ? UiColor(UiColorRole::DisabledText)
                                                        : UiColor(UiColorRole::TextSecondary);
    RECT textRc = rc;
    textRc.right -= UiSp(UiSpace::Sm);
    UiDrawTextClamped(dc, textUtf8, textRc, DT_RIGHT | DT_VCENTER, fg, UiFontRole::Caption);
    ++g_drawCallCount;
}

// -----------------------------------------------------------------------------
// 4. 进度条
// -----------------------------------------------------------------------------
bool UiDrawProgress(HDC dc, const RECT& rc, double progress, UiTone tone) {
    if (dc == nullptr) return false;
    if (!(progress >= 0.0)) progress = 0.0;   // 同时挡住 NaN
    if (!(progress <= 1.0)) progress = 1.0;
    const int w = rc.right - rc.left, h = rc.bottom - rc.top;
    if (w < 8 || h < 4) return false;
    const int radius = std::max(1, std::min(h / 2, UiRadiusPx(UiRadius::Sm)));
    FillRound(dc, rc, radius, UiColor(UiColorRole::Track));
    if (progress > 0.0) {
        RECT fill = rc;
        fill.right = fill.left + std::max(2, static_cast<int>(std::lround(progress * w)));
        if (fill.right > rc.right) fill.right = rc.right;
        FillRound(dc, fill, radius, UiToneFg(tone));
    }
    ++g_drawCallCount;
    return true;
}

bool UiDrawProgress(HDC dc, const RECT& rc, double progress, const std::string& labelUtf8,
                    UiTone tone) {
    if (!UiDrawProgress(dc, rc, progress, tone)) return false;
    if (!labelUtf8.empty()) {
        RECT textRc = rc;
        textRc.left += UiSp(UiSpace::Sm);
        textRc.right -= UiSp(UiSpace::Sm);
        UiDrawTextClamped(dc, labelUtf8, textRc, DT_CENTER | DT_VCENTER,
                          UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);
    }
    return true;
}

// -----------------------------------------------------------------------------
// 5. 徽标 / 提示条
// -----------------------------------------------------------------------------
int UiBadgeWidth(HDC dc, const std::string& textUtf8) {
    if (dc == nullptr) return 0;
    HGDIOBJ oldFont = UiSelectFont(dc, UiFontRole::Caption);
    const int textW = UiTextWidth(dc, textUtf8.c_str());
    if (oldFont != nullptr) SelectObject(dc, oldFont);
    return textW + 2 * UiSp(UiSpace::Sm);
}

int UiDrawBadge(HDC dc, int x, int y, const std::string& textUtf8, UiTone tone) {
    if (dc == nullptr) return 0;
    const int h = UiControlHeight(UiControlH::Sm);
    const int w = std::max(h, UiBadgeWidth(dc, textUtf8));
    RECT rc{ x, y, x + w, y + h };
    FillRound(dc, rc, h / 2, UiToneBg(tone));
    RECT textRc = rc;
    UiDrawTextClamped(dc, textUtf8, textRc, DT_CENTER | DT_VCENTER, UiToneFg(tone),
                      UiFontRole::Caption);
    ++g_drawCallCount;
    return w;
}

int UiDrawHintBanner(HDC dc, const RECT& rc, UiTone tone, const std::string& textUtf8) {
    if (dc == nullptr || IsEmptyRect(rc)) return 0;
    const int h = rc.bottom - rc.top;
    FillRound(dc, rc, UiRadiusPx(UiRadius::Md), UiToneBg(tone));
    UiStrokeRoundRect(dc, rc, UiRadiusPx(UiRadius::Md), UiToneFg(tone), 1);

    // 左侧状态圆点（Ellipse）
    const int dot = std::max(6, UiSp(UiSpace::Sm));
    const int cy = rc.top + h / 2;
    const int cx = rc.left + UiSp(UiSpace::Md);
    HBRUSH dotBrush = CreateSolidBrush(UiToneFg(tone));
    if (dotBrush != nullptr) {
        HGDIOBJ ob = SelectObject(dc, dotBrush);
        HGDIOBJ op = SelectObject(dc, GetStockObject(NULL_PEN));
        Ellipse(dc, cx - dot / 2, cy - dot / 2, cx + dot / 2, cy + dot / 2);
        if (op != nullptr) SelectObject(dc, op);
        SelectObject(dc, ob);
        DeleteObject(dotBrush);
    }
    RECT textRc = rc;
    textRc.left = cx + dot / 2 + UiSp(UiSpace::Sm);
    textRc.right -= UiSp(UiSpace::Md);
    UiDrawTextClamped(dc, textUtf8, textRc, DT_LEFT | DT_VCENTER, UiToneFg(tone), UiFontRole::Body);
    ++g_drawCallCount;
    return h;
}

// -----------------------------------------------------------------------------
// 7. 双缓冲 Paint
// -----------------------------------------------------------------------------
void UIPaintBufferBegin(UIPaintBuffer& buf, HDC target, const RECT& rc) {
    buf.target = target;
    buf.rc = rc;
    const int w = rc.right - rc.left, h = rc.bottom - rc.top;
    buf.ready = false;
    if (target == nullptr || w <= 0 || h <= 0) return;
    if (buf.dc != nullptr && (buf.width != w || buf.height != h)) {
        if (buf.oldBmp != nullptr && buf.dc != nullptr) SelectObject(buf.dc, buf.oldBmp);
        if (buf.bmp != nullptr) DeleteObject(buf.bmp);
        DeleteDC(buf.dc);
        buf.dc = nullptr;
        buf.bmp = nullptr;
        buf.oldBmp = nullptr;
    }
    if (buf.dc == nullptr) {
        buf.dc = CreateCompatibleDC(target);
        if (buf.dc == nullptr) return;
        BITMAPINFO bi{};
        bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;  // 自上而下，避免坐标翻转
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        void* bits = nullptr;
        buf.bmp = CreateDIBSection(buf.dc, &bi, DIB_RGB_COLORS, &bits, nullptr, 0);
        if (buf.bmp == nullptr) {
            DeleteDC(buf.dc);
            buf.dc = nullptr;
            return;
        }
        buf.oldBmp = static_cast<HBITMAP>(SelectObject(buf.dc, buf.bmp));
        buf.width = w;
        buf.height = h;
    }
    // 每次绘制都清底：否则残留污染外观（v1.0.19 用 whiteBrush 手工擦，这里统一）
    RECT full{ 0, 0, w, h };
    HBRUSH bg = CreateSolidBrush(UiColor(UiColorRole::PanelBg));
    if (bg != nullptr) {
        FillRect(buf.dc, &full, bg);
        DeleteObject(bg);
    }
    buf.ready = true;
}

void UIPaintBufferEnd(UIPaintBuffer& buf) {
    if (!buf.ready || buf.dc == nullptr || buf.target == nullptr) return;
    BitBlt(buf.target, buf.rc.left, buf.rc.top, buf.width, buf.height, buf.dc, 0, 0, SRCCOPY);
}

void UIPaintBufferFree(UIPaintBuffer& buf) {
    if (buf.dc != nullptr) {
        if (buf.oldBmp != nullptr) SelectObject(buf.dc, buf.oldBmp);
        if (buf.bmp != nullptr) DeleteObject(buf.bmp);
        DeleteDC(buf.dc);
    }
    buf.dc = nullptr;
    buf.bmp = nullptr;
    buf.oldBmp = nullptr;
    buf.width = buf.height = 0;
    buf.ready = false;
}

// -----------------------------------------------------------------------------
// 6. 自检
// -----------------------------------------------------------------------------
namespace {

// 把一条绘制分支画进独立离屏 DIB，矩形从 (0,0) 起（绝不会被裁到画布外），
// 扫描「与底色 RGB(1,1,1) 不同的像素」数量——大于 0 才说明这一步真的出图。
// 返回 false 表示 DIB/DC 创建失败；*drew 表示是否落了像素。
bool RenderCaseHit(HDC screen, int w, int h, void (*fn)(HDC, const RECT&), bool* drew) {
    if (drew != nullptr) *drew = false;
    if (screen == nullptr || fn == nullptr) return false;
    if (w <= 0) w = 1;
    if (h <= 0) h = 1;
    HDC mem = CreateCompatibleDC(screen);
    if (mem == nullptr) return false;
    void* bits = nullptr;
    BITMAPINFO bi{};
    bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bi.bmiHeader.biWidth = w;
    bi.bmiHeader.biHeight = -h;  // 自上而下，坐标与屏幕一致
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;
    HBITMAP bmp = CreateDIBSection(mem, &bi, DIB_RGB_COLORS, &bits, nullptr, 0);
    if (bmp == nullptr) {
        DeleteDC(mem);
        return false;
    }
    bool ok = false;
    HGDIOBJ old = SelectObject(mem, bmp);
    if (old != nullptr) {
        const RECT rc{ 0, 0, w, h };
        HBRUSH black = CreateSolidBrush(RGB(1, 1, 1));
        if (black != nullptr) {
            FillRect(mem, &rc, black);
            DeleteObject(black);
            SetLastError(ERROR_SUCCESS);
            fn(mem, rc);
            for (int x = 0; x < w && !ok; ++x) {
                for (int y = 0; y < h; ++y) {
                    const COLORREF c = GetPixel(mem, x, y);
                    if (c != CLR_INVALID && c != RGB(1, 1, 1)) {
                        ok = true;
                        break;
                    }
                }
            }
        }
        SelectObject(mem, old);
    }
    DeleteObject(bmp);
    DeleteDC(mem);
    if (drew != nullptr) *drew = ok;
    return true;
}

}  // namespace

bool UiWidgetsSelfTest(HDC dc) {
    (void)dc;  // 全部绘制都跑在函数内部自建的离屏 DIB 上，绝不画到窗口/屏幕 DC（不可能阻塞消息循环）
    // 每条分支独立画布：宽高按各控件「最小可用尺寸」给——进度条/徽标/提示条本来就要求
    // 一定高度才能成形（UiDrawProgress 在 <8x4 时按设计直接返回 false），用 1x1 判定会把
    // 「设计上的尺寸门槛」误判成实现缺陷。矩形始终从 (0,0) 起，避免裁掉绘制区域。
    const int cellW = UiScale(96), cellH = UiScale(24), badgeCell = UiScale(48);
    struct Case { const char* name; int w; int h; void (*fn)(HDC, const RECT&); };
    const Case cases[] = {
        { "button.normal",   cellW, cellH, [](HDC d, const RECT& r) { UiDrawButton(d, r, UiButtonState::Normal, "OK", false); } },
        { "button.hot",      cellW, cellH, [](HDC d, const RECT& r) { UiDrawButton(d, r, UiButtonState::Hot, "OK", false); } },
        { "button.pressed",  cellW, cellH, [](HDC d, const RECT& r) { UiDrawButton(d, r, UiButtonState::Pressed, "OK", true); } },
        { "button.focused",  cellW, cellH, [](HDC d, const RECT& r) { UiDrawButton(d, r, UiButtonState::Focused, "OK", false); } },
        { "button.disabled", cellW, cellH, [](HDC d, const RECT& r) { UiDrawButton(d, r, UiButtonState::Disabled, "OK", true); } },
        { "card",            cellW, cellH, [](HDC d, const RECT& r) { UiDrawCard(d, r, true); } },
        { "card.title",      cellW, cellH, [](HDC d, const RECT& r) { UiDrawCardTitle(d, r, "T"); } },
        { "row.normal",      cellW, cellH, [](HDC d, const RECT& r) { UiDrawListRow(d, r, UiRowState::Normal, "OK", true); } },
        { "row.hover",       cellW, cellH, [](HDC d, const RECT& r) { UiDrawListRow(d, r, UiRowState::Hover, "OK", false); } },
        { "row.selected",    cellW, cellH, [](HDC d, const RECT& r) { UiDrawListRow(d, r, UiRowState::Selected, "OK", false); } },
        { "row.disabled",    cellW, cellH, [](HDC d, const RECT& r) { UiDrawListRow(d, r, UiRowState::Disabled, "OK", false); } },
        { "row.right",       cellW, cellH, [](HDC d, const RECT& r) { UiDrawListRowRight(d, r, "1%", UiRowState::Normal); } },
        { "progress.track",  cellW, cellH, [](HDC d, const RECT& r) { UiDrawProgress(d, r, 0.0, UiTone::Accent); } },
        { "progress.fill",   cellW, cellH, [](HDC d, const RECT& r) { UiDrawProgress(d, r, 1.0, UiTone::Success); } },
        { "progress.label",  cellW, cellH, [](HDC d, const RECT& r) { UiDrawProgress(d, r, 0.5, "50%", UiTone::Info); } },
        { "badge",           badgeCell, badgeCell, [](HDC d, const RECT& r) { (void)r; (void)UiDrawBadge(d, 0, 0, "OK", UiTone::Danger); } },
        { "banner",          cellW, UiControlHeight(UiControlH::Lg), [](HDC d, const RECT& r) { UiDrawHintBanner(d, r, UiTone::Warning, "w"); } },
        { "divider",         cellW, UiScale(4), [](HDC d, const RECT& r) { UiDrawDivider(d, r); } },
        { "metric",          cellW, cellH, [](HDC d, const RECT& r) { UiDrawMetric(d, r, "1"); } },
        { "text.at",         cellW, cellH, [](HDC d, const RECT& r) { (void)r; UiDrawTextAt(d, 0, 0, "x", UiColor(UiColorRole::TextPrimary), UiFontRole::Body); } },
        { "text.clamped",    cellW, cellH, [](HDC d, const RECT& r) { UiDrawTextClamped(d, "x", r, DT_LEFT | DT_VCENTER, UiColor(UiColorRole::TextPrimary), UiFontRole::Body); } },
        { "text.wrap",       cellW, cellH, [](HDC d, const RECT& r) { UiDrawTextWrap(d, "x", r, UiColor(UiColorRole::TextSecondary), UiFontRole::Caption); } },
    };
    const int caseCount = static_cast<int>(sizeof(cases) / sizeof(cases[0]));

    int failures = 0;
    bool allDrew = true;
    HDC screen = GetDC(nullptr);
    if (screen == nullptr) return false;
    for (int i = 0; i < caseCount; ++i) {
        bool drew = false;
        if (!RenderCaseHit(screen, cases[i].w, cases[i].h, cases[i].fn, &drew) || !drew) {
            ++failures;
            allDrew = false;
        }
    }
    ReleaseDC(nullptr, screen);

    // 一句无副作用的 API 冒烟（派生函数也不应崩）
    (void)UiButtonStateFromDrawItem(nullptr);
    (void)UiToneFg(UiTone::Neutral);
    (void)UiToneBg(UiTone::Neutral);
    (void)g_drawCallCount;
    return failures == 0 && allDrew;
}

}  // namespace ui
}  // namespace gopt
