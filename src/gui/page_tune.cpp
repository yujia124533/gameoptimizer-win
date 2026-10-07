// =============================================================================
// GameOptimizer v1.1.0 — 页面组 B：系统调优页实现（page_tune.cpp）
// -----------------------------------------------------------------------------
// API 白名单：user32（窗口/标控件/消息/子类化）+ gdi32（经 ui_theme/ui_widgets 封装）。
// 无第三方库、无全局 Hook、无 API Hook；SetWindowLongPtrW 只用于本模块自建子控件的
// 进程内子类化（与 gopt_gui.cpp 的 PageProc 同一技术），用途仅是 hover 高亮。
//
// 本文件不含任何 OpenProcess / DeleteFile / RegSetValueEx / 电源方案切换调用。
// 「清理临时文件」唯一路径 = Hooks::cleanTemp（推荐接线 SystemTuner::CleanTemp()），
// 其 24 小时保留策略位于 src/tuning/SystemTuner.cpp（kKeepRecent100ns），本任务不修改该
// 文件，页面也没有任何绕过它的删除路径 —— 安全策略不可能被本页弱化；并且本页额外加了
// 二次确认与常驻安全说明。
// =============================================================================

#include "gui/page_tune.h"

#include <algorithm>
#include <map>
#include <string>
#include <vector>

#include "i18n.h"

namespace gopt {
namespace ui {
namespace {

// -----------------------------------------------------------------------------
// 基础工具
// -----------------------------------------------------------------------------
std::wstring ToWide(const std::string& s) {
    if (s.empty()) return std::wstring();
    const int need = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), static_cast<int>(s.size()),
                                         nullptr, 0);
    if (need <= 0) return std::wstring();
    std::wstring out(static_cast<size_t>(need), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), static_cast<int>(s.size()), &out[0], need);
    return out;
}

inline const char* Tr(const char* zh, const char* en) { return gopt::T(zh, en); }

int ClampInt(int v, int lo, int hi) {
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}

RECT MakeRect(int l, int t, int r, int b) {
    RECT rc{};
    rc.left = l;
    rc.top = t;
    rc.right = r;
    rc.bottom = b;
    return rc;
}

bool ValidRect(const RECT& r) { return r.right > r.left && r.bottom > r.top; }

bool RectsOverlap(const RECT& a, const RECT& b) {
    if (!ValidRect(a) || !ValidRect(b)) return false;
    return a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
}

// -----------------------------------------------------------------------------
// 按钮表
// -----------------------------------------------------------------------------
struct ButtonSpec {
    int id;
    const char* zh;
    const char* en;
};

const ButtonSpec kButtons[PageTune::kBtnCount] = {
    {PageTune::kBtnHigh,     "高性能档",     "High Performance"},
    {PageTune::kBtnBalanced, "平衡档",       "Balanced"},
    {PageTune::kBtnRestore,  "恢复调优",     "Restore Tune"},
    {PageTune::kBtnClean,    "清理临时文件", "Clean Temp"},
};

// -----------------------------------------------------------------------------
// 布局计划（纯几何：不碰窗口、不碰 GDI —— 自检可以对任意尺寸直接跑）
// -----------------------------------------------------------------------------
struct Plan {
    RECT title{};
    RECT subtitle{};
    RECT infoCard{};
    RECT noteCard{};
    RECT receipt{};
    RECT buttons[PageTune::kBtnCount]{};
    int  buttonCount = 0;
    bool noteVisible = false;
};

constexpr int kArmWindowMs = 5000;
constexpr UINT_PTR kTimerArm = 1;

Plan PlanLayout(int w, int h, const int* btnW, int btnN) {
    Plan p;
    if (w <= 0 || h <= 0) return p;
    const int pad = UiSp(UiSpace::Md);   // 16：8px 栅格的 2 倍
    const int gap = UiSp(UiSpace::Sm);   // 8
    const int left = pad;
    const int right = std::max(left, w - pad);

    int y = pad;
    p.title = MakeRect(left, y, right, y + UiSp(UiSpace::Xl));          // 32
    y = p.title.bottom + UiSp(UiSpace::Xs);                            // +4
    p.subtitle = MakeRect(left, y, right, y + UiSp(UiSpace::Lg));       // 24
    y = p.subtitle.bottom + gap;

    const int infoH = UiControlHeight(UiControlH::Xl) + UiSp(UiSpace::Md);  // 56+16 = 72
    p.infoCard = MakeRect(left, y, right, y + infoH);
    y = p.infoCard.bottom + gap;

    // 按钮：流式排布（左→右，放不下换行）→ 结构上既不会互相重叠，也不会压到上下区块；
    // 若在客户区高度内放不下多行，退化为**单行等宽**（单行共享同一 y、x 互不重叠）
    const int bh = UiControlHeight(UiControlH::Lg);   // 40
    const int btnBottomLimit = h - pad;
    int estRows = 1;
    {
        int x = left;
        for (int i = 0; i < btnN && i < PageTune::kBtnCount; ++i) {
            const int bw = ClampInt(btnW != nullptr ? btnW[i] : 0,
                                    UiControlHeight(UiControlH::Xl), right - left);
            if (x != left && x + bw > right) { x = left; ++estRows; }
            x += bw + gap;
        }
    }
    const int maxRows = std::max(1, (btnBottomLimit - y + gap) / (bh + gap));
    const bool singleRow = estRows > maxRows;
    const int eqW = singleRow && btnN > 0 ? std::max(1, ((right - left) - (btnN - 1) * gap) / btnN) : 0;
    int bx = left;
    int by = y;
    for (int i = 0; i < btnN && i < PageTune::kBtnCount; ++i) {
        int bw = singleRow ? eqW
                           : ClampInt(btnW != nullptr ? btnW[i] : 0,
                                      UiControlHeight(UiControlH::Xl), right - left);
        if (bw > right - left) bw = right - left;
        if (!singleRow && bx != left && bx + bw > right) {
            bx = left;
            by += bh + gap;
        }
        const int bxr = std::min(bx + bw, right);
        int byTop = by;
        int byBot = by + bh;
        if (byBot > btnBottomLimit) {              // 窗口过矮：夹取到客户区内，绝不越界
            byBot = btnBottomLimit;
            if (byBot - byTop < 1) { byTop = std::max(0, btnBottomLimit - 1); byBot = byTop + 1; }
        }
        p.buttons[i] = MakeRect(bx, byTop, bxr, byBot);
        ++p.buttonCount;
        bx = bxr + gap;
    }
    y = by + bh + gap;

    // 说明卡 + 回执卡：空间足够就都画，不够就按优先级降级（只裁剪，绝不重叠）
    const int noteH = UiSp(UiSpace::Xl) * 2 + UiSp(UiSpace::Md);   // 80：小标题 + 3 行换行
    const int recH = UiSp(UiSpace::Xl) + UiSp(UiSpace::Md);        // 48：2~3 行回执
    const int avail = h - pad - y;
    const int bottomLimit = h - pad - recH;
    if (avail >= noteH + gap + recH) {
        p.noteCard = MakeRect(left, y, right, y + noteH);
        p.receipt = MakeRect(left, bottomLimit, right, bottomLimit + recH);
        p.noteVisible = true;
    } else if (avail >= recH) {
        p.noteCard = MakeRect(0, 0, 0, 0);         // 省略说明卡（安全策略仍由标题徽标常驻显示）
        p.receipt = MakeRect(left, bottomLimit, right, bottomLimit + recH);
    } else if (avail > UiSp(UiSpace::Xs)) {
        p.noteCard = MakeRect(0, 0, 0, 0);
        p.receipt = MakeRect(left, y, right, y + avail);
    } else {
        p.noteCard = MakeRect(0, 0, 0, 0);
        p.receipt = MakeRect(0, 0, 0, 0);
    }
    return p;
}

// -----------------------------------------------------------------------------
// 页面上下文（单实例；与 v1.0.19 的 GUI 全局状态风格一致）
// -----------------------------------------------------------------------------
struct Ctx {
    HWND root = nullptr;
    PageTune::Hooks hooks;
    int  hoverId = 0;
    bool armedClean = false;
    ULONGLONG armedAt = 0;
    std::string receiptText;
    UiTone receiptTone = UiTone::Neutral;
    bool recommendedHigh = false;
    std::string powerScheme;
    bool elevated = false;
    Plan plan{};
    int  btnW[PageTune::kBtnCount] = {};
    UIPaintBuffer buf{};
    HBRUSH brushPanel = nullptr;      // 页面底/按钮擦除底（避免圆角外的系统灰边）
    std::map<HWND, WNDPROC> oldProc;  // 子类化前的原窗口过程
};

Ctx g;

// -----------------------------------------------------------------------------
// 文案
// -----------------------------------------------------------------------------
std::string LabelOf(int id) {
    for (const ButtonSpec& b : kButtons) {
        if (b.id != id) continue;
        if (id == PageTune::kBtnClean && g.armedClean)
            return Tr("确认清理（再点一次）", "Confirm clean (click again)");
        return Tr(b.zh, b.en);
    }
    return std::string();
}

std::string PageTitle() { return Tr("系统调优", "System Tune"); }
std::string PageSubtitle() {
    return Tr("电源方案 + 处理器性能档 + 系统调度优先级；应用前自动快照，可一键恢复。",
              "Power scheme + processor performance + scheduling priority; auto snapshot, one-click restore.");
}

bool IsPrimaryButton(int id) {
    if (id == PageTune::kBtnHigh) return g.recommendedHigh;
    if (id == PageTune::kBtnBalanced) return !g.recommendedHigh;
    return false;
}

// 结果文本 → 色调（对宿主返回的文本做保守启发式判断，未知倾向按成功处理）
UiTone ToneForResult(const std::string& s) {
    if (s.find("FAIL") != std::string::npos || s.find("失败") != std::string::npos ||
        s.find("fail") != std::string::npos)
        return UiTone::Danger;
    if (s.find("跳过") != std::string::npos || s.find("降级") != std::string::npos ||
        s.find("skip") != std::string::npos)
        return UiTone::Warning;
    return UiTone::Success;
}

// -----------------------------------------------------------------------------
// 绘制工具
// -----------------------------------------------------------------------------
void DropBrushes() {
    if (g.brushPanel != nullptr) {
        DeleteObject(g.brushPanel);
        g.brushPanel = nullptr;
    }
}

HBRUSH PanelBrush() {
    if (g.brushPanel == nullptr) g.brushPanel = CreateSolidBrush(UiColor(UiColorRole::PanelBg));
    return g.brushPanel;
}

// 多行提示卡（UiDrawHintBanner 只支持单行；回执/说明可能有多行）
void DrawBannerCard(HDC dc, const RECT& rc, UiTone tone, const std::string& text) {
    if (dc == nullptr || !ValidRect(rc)) return;
    UiFillRoundRect(dc, rc, UiRadiusPx(UiRadius::Md), UiToneBg(tone));
    UiStrokeRoundRect(dc, rc, UiRadiusPx(UiRadius::Md), UiToneFg(tone), 1);

    const int dot = std::max(6, UiSp(UiSpace::Sm));
    const int cx = rc.left + UiSp(UiSpace::Md);
    const int cy = rc.top + std::min(UiSp(UiSpace::Md), static_cast<int>((rc.bottom - rc.top) / 2));
    HBRUSH dotBrush = CreateSolidBrush(UiToneFg(tone));
    if (dotBrush != nullptr) {
        HGDIOBJ oldBrush = SelectObject(dc, dotBrush);
        HGDIOBJ oldPen = SelectObject(dc, GetStockObject(NULL_PEN));
        Ellipse(dc, cx - dot / 2, cy - dot / 2, cx + dot / 2, cy + dot / 2);
        if (oldPen != nullptr) SelectObject(dc, oldPen);
        if (oldBrush != nullptr) SelectObject(dc, oldBrush);
        DeleteObject(dotBrush);
    }

    RECT textRc = rc;
    textRc.left = cx + dot / 2 + UiSp(UiSpace::Sm);
    textRc.right -= UiSp(UiSpace::Md);
    textRc.top += UiSp(UiSpace::Xs);
    textRc.bottom -= UiSp(UiSpace::Xs);
    if (ValidRect(textRc)) UiDrawTextWrap(dc, text, textRc, UiToneFg(tone), UiFontRole::Caption);
}

void DrawColumn(HDC dc, const RECT& rc, const std::string& caption, const std::string& value,
                COLORREF valueColor) {
    if (!ValidRect(rc)) return;
    const int h = rc.bottom - rc.top;
    RECT capRc = rc;
    capRc.bottom = rc.top + h / 2 - UiSp(UiSpace::Xxs);
    RECT valRc = rc;
    valRc.top = capRc.bottom + UiSp(UiSpace::Xxs);
    UiDrawTextClamped(dc, caption, capRc, DT_LEFT | DT_VCENTER,
                      UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);
    UiDrawTextClamped(dc, value, valRc, DT_LEFT | DT_VCENTER, valueColor, UiFontRole::Subtitle);
}

// -----------------------------------------------------------------------------
// 回执
// -----------------------------------------------------------------------------
void InvalidatePage() {
    if (g.root != nullptr) InvalidateRect(g.root, nullptr, FALSE);
}

void SetReceiptQuiet(const std::string& text, UiTone tone) {
    g.receiptText = text;
    g.receiptTone = tone;
    InvalidatePage();
}

void PushReceipt(const std::string& text, UiTone tone) {
    SetReceiptQuiet(text, tone);
    if (g.hooks.receipt) g.hooks.receipt(text);
}

// -----------------------------------------------------------------------------
// 布局应用
// -----------------------------------------------------------------------------
void RecalcButtonWidths() {
    HDC dc = CreateCompatibleDC(nullptr);
    for (int i = 0; i < PageTune::kBtnCount; ++i) {
        int w = UiControlHeight(UiControlH::Xl);
        if (dc != nullptr) {
            HGDIOBJ oldFont = UiSelectFont(dc, UiFontRole::BodyBold);
            const int measured = UiButtonMinWidth(dc, LabelOf(kButtons[i].id), false);
            if (oldFont != nullptr) SelectObject(dc, oldFont);
            w = std::max(w, measured);
        }
        g.btnW[i] = ClampInt(w, UiControlHeight(UiControlH::Lg), UiScaleAt(280, UiDpi()));
    }
    if (dc != nullptr) DeleteDC(dc);
}

void LayoutSelf() {
    if (g.root == nullptr) return;
    RECT rc{};
    if (!GetClientRect(g.root, &rc)) return;
    g.plan = PlanLayout(rc.right - rc.left, rc.bottom - rc.top, g.btnW, PageTune::kBtnCount);
    for (int i = 0; i < g.plan.buttonCount; ++i) {
        HWND btn = GetDlgItem(g.root, kButtons[i].id);
        if (btn == nullptr) continue;
        const RECT& r = g.plan.buttons[i];
        MoveWindow(btn, r.left, r.top, r.right - r.left, r.bottom - r.top, TRUE);
    }
    InvalidatePage();
}

// -----------------------------------------------------------------------------
// 数据
// -----------------------------------------------------------------------------
void RefreshData() {
    g.recommendedHigh = g.hooks.recommendHighPerf ? g.hooks.recommendHighPerf() : false;
    g.powerScheme = g.hooks.activePowerSchemeName ? g.hooks.activePowerSchemeName() : std::string();
    if (g.powerScheme.empty()) g.powerScheme = Tr("未知", "unknown");
    g.elevated = g.hooks.isElevated ? g.hooks.isElevated() : false;
}

// -----------------------------------------------------------------------------
// 动作
// -----------------------------------------------------------------------------
void DisarmClean() {
    if (g.root != nullptr) KillTimer(g.root, kTimerArm);
    if (!g.armedClean) return;
    g.armedClean = false;
    RecalcButtonWidths();
    LayoutSelf();
    if (g.root != nullptr) {
        if (HWND btn = GetDlgItem(g.root, PageTune::kBtnClean))
            SetWindowTextW(btn, ToWide(LabelOf(PageTune::kBtnClean)).c_str());
        InvalidateRect(g.root, nullptr, TRUE);
    }
}

void ArmClean() {
    g.armedClean = true;
    g.armedAt = GetTickCount64();
    if (g.root != nullptr) {
        SetTimer(g.root, kTimerArm, kArmWindowMs, nullptr);
        if (HWND btn = GetDlgItem(g.root, PageTune::kBtnClean))
            SetWindowTextW(btn, ToWide(LabelOf(PageTune::kBtnClean)).c_str());
    }
    RecalcButtonWidths();
    LayoutSelf();
    PushReceipt(Tr("二次确认：5 秒内再次点击「清理临时文件」才会执行。"
                   "安全策略：%TEMP% 下 24 小时内修改过的文件一律保留（不可关闭）。",
                   "Confirm: click Clean Temp again within 5 seconds. "
                   "Safety policy: files under %TEMP% modified within 24h are always kept."),
                UiTone::Warning);
}

void DoTune(bool highPerf) {
    DisarmClean();
    const char* tier = highPerf ? Tr("高性能档", "high performance") : Tr("平衡档", "balanced");
    if (!g.hooks.tune) {
        PushReceipt(Tr("未接线：应用档位需要宿主提供 Hooks::tune。",
                       "Not wired: Hooks::tune is required."),
                    UiTone::Warning);
        return;
    }
    const std::string result = g.hooks.tune(highPerf);
    PushReceipt(std::string(Tr("已应用 ", "Applied ")) + tier + Tr("：", ": ") + result,
                ToneForResult(result));
    RefreshData();
}

void DoRestore() {
    DisarmClean();
    if (!g.hooks.restoreTune) {
        PushReceipt(Tr("未接线：恢复调优需要宿主提供 Hooks::restoreTune。",
                       "Not wired: Hooks::restoreTune is required."),
                    UiTone::Warning);
        return;
    }
    const std::string result = g.hooks.restoreTune();
    PushReceipt(std::string(Tr("恢复调优：", "Restore tune: ")) + result, ToneForResult(result));
    RefreshData();
}

void DoCleanStep() {
    // 先判「是否已在确认态」——绝不能在判断之前复位确认态（否则第二次点击永远回到确认态）
    const ULONGLONG now = GetTickCount64();
    const bool confirming = g.armedClean && (now - g.armedAt) <= kArmWindowMs;
    if (!confirming) {
        DisarmClean();   // 清掉过期/残留的确认态（含 5 秒超时后的状态）
        if (!g.hooks.cleanTemp) {
            PushReceipt(Tr("未接线：清理临时文件需要宿主提供 Hooks::cleanTemp。",
                           "Not wired: Hooks::cleanTemp is required."),
                        UiTone::Warning);
            return;
        }
        ArmClean();
        return;
    }
    // 第二次点击 = 真执行；先复位确认态（按钮文案立即恢复），再调用核心
    DisarmClean();
    if (!g.hooks.cleanTemp) {
        PushReceipt(Tr("未接线：清理临时文件需要宿主提供 Hooks::cleanTemp。",
                       "Not wired: Hooks::cleanTemp is required."),
                    UiTone::Warning);
        return;
    }
    const std::string result = g.hooks.cleanTemp();
    PushReceipt(std::string(Tr("清理完成（安全策略：24 小时内文件已保留）：",
                               "Clean done (safety policy: files newer than 24h kept): ")) + result,
                ToneForResult(result));
}

// -----------------------------------------------------------------------------
// 绘制
// -----------------------------------------------------------------------------
void PaintContent(HDC dc, const RECT& client) {
    if (dc == nullptr) return;
    HBRUSH bg = PanelBrush();
    if (bg != nullptr) FillRect(dc, &client, bg);
    const Plan& p = g.plan;

    // 标题 + 常驻安全策略徽标
    UiDrawTextClamped(dc, PageTitle(), p.title, DT_LEFT | DT_VCENTER,
                      UiColor(UiColorRole::TextPrimary), UiFontRole::Title);
    if (ValidRect(p.title)) {
        const std::string policy = Tr("安全：24 小时保留", "Safe: keep 24h");
        const int bw = UiBadgeWidth(dc, policy);
        const int bh = UiControlHeight(UiControlH::Sm);
        if (bw + UiSp(UiSpace::Md) < (p.title.right - p.title.left)) {
            const int by = p.title.top +
                           std::max(0, static_cast<int>(((p.title.bottom - p.title.top) - bh) / 2));
            UiDrawBadge(dc, p.title.right - bw, by, policy, UiTone::Success);
        }
    }
    UiDrawTextClamped(dc, PageSubtitle(), p.subtitle, DT_LEFT | DT_VCENTER,
                      UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);

    // 信息卡：推荐档位 / 当前电源方案 / 权限
    if (ValidRect(p.infoCard)) {
        UiDrawCard(dc, p.infoCard, true);
        RECT inner = p.infoCard;
        InflateRect(&inner, -UiSp(UiSpace::Md), -UiSp(UiSpace::Sm));
        const int innerW = inner.right - inner.left;
        const int colW = innerW / 3;
        if (colW > 0) {
            RECT c1 = MakeRect(inner.left, inner.top, inner.left + colW - UiSp(UiSpace::Sm), inner.bottom);
            RECT c2 = MakeRect(c1.right + UiSp(UiSpace::Sm), inner.top,
                               c1.right + UiSp(UiSpace::Sm) + colW - UiSp(UiSpace::Sm), inner.bottom);
            RECT c3 = MakeRect(c2.right + UiSp(UiSpace::Sm), inner.top, inner.right, inner.bottom);
            const std::string rec = g.recommendedHigh ? Tr("高性能档", "High performance")
                                                      : Tr("平衡档", "Balanced");
            DrawColumn(dc, c1, Tr("推荐档位", "Recommended tier"), rec,
                       g.recommendedHigh ? UiColor(UiColorRole::Accent) : UiColor(UiColorRole::Info));
            DrawColumn(dc, c2, Tr("当前电源方案", "Active power scheme"), g.powerScheme,
                       UiColor(UiColorRole::TextPrimary));
            DrawColumn(dc, c3, Tr("权限", "Privileges"),
                       g.elevated ? Tr("管理员", "Administrator")
                                  : Tr("标准用户（调优可能失败）", "Standard user"),
                       g.elevated ? UiColor(UiColorRole::Success) : UiColor(UiColorRole::Warning));
            // 列间分隔线（1px）
            for (int i = 1; i <= 2; ++i) {
                const RECT& prev = (i == 1) ? c1 : c2;
                RECT div = MakeRect(prev.right + UiSp(UiSpace::Xs), inner.top,
                                    prev.right + UiSp(UiSpace::Xs) + std::max(1, UiScale(1)),
                                    inner.bottom);
                UiDrawDivider(dc, div);
            }
        }
    }

    // 说明卡：调优范围 + 清洁策略（保证「24 小时保留」文案常驻）
    if (p.noteVisible && ValidRect(p.noteCard)) {
        UiDrawCard(dc, p.noteCard, false);
        RECT inner = p.noteCard;
        InflateRect(&inner, -UiSp(UiSpace::Md), -UiSp(UiSpace::Sm));
        RECT titleRc = inner;
        titleRc.bottom = inner.top + UiSp(UiSpace::Lg);
        UiDrawCardTitle(dc, titleRc, Tr("安全策略与范围", "Safety & scope"));
        RECT bodyRc = inner;
        bodyRc.top = titleRc.bottom;
        UiDrawTextWrap(dc,
                       Tr("调优范围：电源方案（高性能/平衡）+ 处理器最小/最大性能档 + 系统调度优先级；"
                          "应用前自动快照，「恢复调优」可一键还原。\n"
                          "清洁范围：仅 %TEMP% 下 24 小时前、未被占用的文件与空目录；"
                          "24 小时内修改过的文件一律保留（核心安全策略，不可关闭）。",
                          "Tune: power scheme + processor min/max + scheduling priority; auto snapshot before apply, Restore Tune reverts it.\n"
                          "Clean: only items older than 24h under %TEMP%; files modified within 24h are always kept (core safety policy)."),
                       bodyRc, UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);
    }

    // 回执卡（所有入口都有可见反馈；同时经 Hooks::receipt 进入宿主日志）
    if (ValidRect(p.receipt)) {
        DrawBannerCard(dc, p.receipt, g.receiptTone,
                       g.receiptText.empty() ? Tr("就绪", "Ready") : g.receiptText);
    }
}

void OnPaint(HWND hwnd) {
    PAINTSTRUCT ps{};
    HDC dc = BeginPaint(hwnd, &ps);
    if (dc == nullptr) return;
    RECT rc{};
    GetClientRect(hwnd, &rc);
    UIPaintBufferBegin(g.buf, dc, rc);
    PaintContent(g.buf.ready ? g.buf.dc : dc, rc);
    if (g.buf.ready) UIPaintBufferEnd(g.buf);
    EndPaint(hwnd, &ps);
}

// -----------------------------------------------------------------------------
// 子类化：hover 高亮（进程内子类化，转发原过程，不改变消息语义）
// -----------------------------------------------------------------------------
LRESULT CALLBACK ControlProc(HWND h, UINT msg, WPARAM wp, LPARAM lp);

void SubclassControl(HWND h) {
    if (h == nullptr) return;
    WNDPROC old = reinterpret_cast<WNDPROC>(
        SetWindowLongPtrW(h, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(ControlProc)));
    if (old != nullptr) g.oldProc[h] = old;
}

LRESULT CALLBACK ControlProc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    std::map<HWND, WNDPROC>::iterator it = g.oldProc.find(h);
    WNDPROC old = (it != g.oldProc.end()) ? it->second : nullptr;
    if (msg == WM_NCDESTROY) {
        g.oldProc.erase(h);
        return old != nullptr ? CallWindowProcW(old, h, msg, wp, lp)
                              : DefWindowProcW(h, msg, wp, lp);
    }
    if (msg == WM_MOUSEMOVE) {
        TRACKMOUSEEVENT tme{};
        tme.cbSize = sizeof(tme);
        tme.dwFlags = TME_LEAVE;
        tme.hwndTrack = h;
        TrackMouseEvent(&tme);
        const int id = GetDlgCtrlID(h);
        if (id != g.hoverId) {
            g.hoverId = id;
            InvalidateRect(h, nullptr, TRUE);
        }
    } else if (msg == WM_MOUSELEAVE) {
        if (g.hoverId == GetDlgCtrlID(h)) {
            g.hoverId = 0;
            InvalidateRect(h, nullptr, TRUE);
        }
    }
    return old != nullptr ? CallWindowProcW(old, h, msg, wp, lp) : DefWindowProcW(h, msg, wp, lp);
}

// -----------------------------------------------------------------------------
// 窗口过程
// -----------------------------------------------------------------------------
LRESULT CALLBACK PageProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    if (msg == UiThemeChangedMessage()) {
        DropBrushes();
        RecalcButtonWidths();
        LayoutSelf();
        InvalidateRect(hwnd, nullptr, TRUE);
        return 0;
    }
    switch (msg) {
        case WM_ERASEBKGND:
            return 1;  // 双缓冲自绘，禁止系统擦底（消闪）
        case WM_SIZE:
            LayoutSelf();
            return 0;
        case WM_PAINT:
            OnPaint(hwnd);
            return 0;
        case WM_DPICHANGED: {
            UiOnDpiChanged(static_cast<UINT>(LOWORD(wp)));
            DropBrushes();
            RecalcButtonWidths();
            LayoutSelf();
            InvalidateRect(hwnd, nullptr, TRUE);
            return 0;
        }
        case WM_DRAWITEM: {
            auto* di = reinterpret_cast<DRAWITEMSTRUCT*>(lp);
            if (di == nullptr || di->CtlType != ODT_BUTTON) break;
            if (di->CtlID < static_cast<UINT>(PageTune::kIdBase) ||
                di->CtlID > static_cast<UINT>(PageTune::kIdLast))
                break;
            UiButtonState state = UiButtonStateFromDrawItem(di);
            if (state == UiButtonState::Normal && g.hoverId == static_cast<int>(di->CtlID))
                state = UiButtonState::Hot;
            if (UiDrawButton(di->hDC, di->rcItem, state, LabelOf(static_cast<int>(di->CtlID)),
                             IsPrimaryButton(static_cast<int>(di->CtlID))))
                return TRUE;
        } break;
        case WM_CTLCOLORBTN:
            // 自绘按钮擦底用页面底色（否则圆角外会露出系统灰边）
            if (reinterpret_cast<HDC>(wp) != nullptr) {
                SetBkColor(reinterpret_cast<HDC>(wp), UiColor(UiColorRole::PanelBg));
                SetTextColor(reinterpret_cast<HDC>(wp), UiColor(UiColorRole::TextPrimary));
            }
            if (PanelBrush() != nullptr) return reinterpret_cast<LRESULT>(PanelBrush());
            break;
        case WM_COMMAND: {
            const int id = LOWORD(wp);
            const int code = HIWORD(wp);
            if (code != BN_CLICKED) return 0;
            switch (id) {
                case PageTune::kBtnHigh:     DoTune(true);  return 0;
                case PageTune::kBtnBalanced: DoTune(false); return 0;
                case PageTune::kBtnRestore:  DoRestore();   return 0;
                case PageTune::kBtnClean:    DoCleanStep(); return 0;
                default: break;
            }
        } break;
        case WM_TIMER:
            if (wp == static_cast<WPARAM>(kTimerArm)) {
                DisarmClean();
                SetReceiptQuiet(Tr("确认超时，已取消清理操作（未删除任何文件）。",
                                   "Confirmation timed out; nothing was deleted."),
                                UiTone::Info);
                return 0;
            }
            break;
        case WM_DESTROY:
            KillTimer(hwnd, kTimerArm);
            UIPaintBufferFree(g.buf);
            DropBrushes();
            g.oldProc.clear();
            g.root = nullptr;
            return 0;
        default:
            break;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

}  // namespace

// -----------------------------------------------------------------------------
// 公开装配接口
// -----------------------------------------------------------------------------
HWND PageTune::Create(HWND parent, const RECT& rc, const Hooks& hooks) {
    if (parent == nullptr) return nullptr;
    if (g.root != nullptr && IsWindow(g.root)) return g.root;   // 单实例：重复 Create 直接返回
    if (UiSp(UiSpace::Md) <= 0) UiThemeInit();                  // 防御：宿主未初始化主题时兜底
    g = Ctx{};

    WNDCLASSEXW wc{};
    wc.cbSize = sizeof(wc);
    wc.style = CS_HREDRAW | CS_VREDRAW;
    wc.lpfnWndProc = PageProc;
    wc.hInstance = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
    wc.hCursor = LoadCursorW(nullptr, MAKEINTRESOURCEW(32512));  // IDC_ARROW（本工程未定义 UNICODE）
    wc.hbrBackground = nullptr;
    wc.lpszClassName = kClassName;
    if (RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) return nullptr;

    const HINSTANCE hInst = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
    HWND root = CreateWindowExW(0, kClassName, L"",
                                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                                rc.left, rc.top, std::max(1, static_cast<int>(rc.right - rc.left)),
                                std::max(1, static_cast<int>(rc.bottom - rc.top)), parent,
                                reinterpret_cast<HMENU>(static_cast<INT_PTR>(kIdBase)), hInst,
                                nullptr);
    if (root == nullptr) return nullptr;
    g.root = root;
    g.hooks = hooks;

    for (const ButtonSpec& b : kButtons) {
        HWND btn = CreateWindowExW(0, L"BUTTON", L"",
                                   WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW, 0, 0, 10, 10,
                                   root, reinterpret_cast<HMENU>(static_cast<INT_PTR>(b.id)), hInst,
                                   nullptr);
        if (btn == nullptr) continue;
        if (HFONT font = UiFont(UiFontRole::BodyBold))
            SendMessageW(btn, WM_SETFONT, reinterpret_cast<WPARAM>(font), TRUE);
        SubclassControl(btn);
    }

    ApplyLabels(root);
    RefreshData();
    RecalcButtonWidths();
    LayoutSelf();
    SetReceiptQuiet(Tr("就绪：选择档位、恢复调优或清理临时文件（清理需二次确认）。",
                       "Ready: pick a tier, restore, or clean temp (clean needs a second click)."),
                    UiTone::Info);
    return root;
}

void PageTune::SetHooks(HWND page, const Hooks& hooks) {
    if (page == nullptr || page != g.root) return;
    g.hooks = hooks;
    RefreshData();
    InvalidateRect(page, nullptr, TRUE);
}

void PageTune::Layout(HWND page, const RECT& rc) {
    if (page == nullptr || page != g.root) return;
    MoveWindow(page, rc.left, rc.top, std::max(1, static_cast<int>(rc.right - rc.left)),
               std::max(1, static_cast<int>(rc.bottom - rc.top)), TRUE);
    LayoutSelf();
}

void PageTune::FillParent(HWND page) {
    if (page == nullptr || page != g.root) return;
    HWND parent = GetParent(page);
    if (parent == nullptr) return;
    RECT rc{};
    if (!GetClientRect(parent, &rc)) return;
    Layout(page, rc);
}

void PageTune::Show(HWND page, bool visible) {
    if (page == nullptr || page != g.root) return;
    ShowWindow(page, visible ? SW_SHOW : SW_HIDE);
    if (visible) {
        RefreshData();
        LayoutSelf();
    }
}

void PageTune::Refresh(HWND page) {
    if (page == nullptr || page != g.root) return;
    RefreshData();
    LayoutSelf();
}

void PageTune::ApplyLabels(HWND page) {
    if (g.root == nullptr) return;
    if (page != nullptr && page != g.root) return;
    for (const ButtonSpec& b : kButtons) {
        HWND btn = GetDlgItem(g.root, b.id);
        if (btn == nullptr) continue;
        SetWindowTextW(btn, ToWide(LabelOf(b.id)).c_str());
    }
    RecalcButtonWidths();
    LayoutSelf();
    InvalidateRect(g.root, nullptr, TRUE);
}

void PageTune::Destroy(HWND page) {
    if (page == nullptr) return;
    if (page != g.root) {
        DestroyWindow(page);
        return;
    }
    g.armedClean = false;
    if (IsWindow(page)) DestroyWindow(page);   // 触发 WM_DESTROY：释放缓冲/画刷/timer
    g = Ctx{};
}

bool PageTune::IsPageWindow(HWND h) {
    if (h == nullptr) return false;
    wchar_t cls[64] = {};
    if (GetClassNameW(h, cls, 64) <= 0) return false;
    return lstrcmpW(cls, kClassName) == 0;
}

// -----------------------------------------------------------------------------
// 自检：纯几何排布 + 真实子窗口矩形 + ID 唯一性
// -----------------------------------------------------------------------------
std::string PageTune::SelfCheck(HWND page) {
    std::string out = "PageTune self-check\n";
    int failures = 0;

    bool idsUnique = true;
    for (int i = 0; i < kBtnCount; ++i)
        for (int j = i + 1; j < kBtnCount; ++j)
            if (kButtons[i].id == kButtons[j].id) idsUnique = false;
    out += std::string("  control ids: count=") + std::to_string(kBtnCount) +
           (idsUnique ? " unique=yes" : " unique=NO") + "\n";
    if (!idsUnique) ++failures;

    int widths[kBtnCount] = {};
    for (int i = 0; i < kBtnCount; ++i)
        widths[i] = g.btnW[i] > 0 ? g.btnW[i] : UiControlHeight(UiControlH::Xl);

    const int sizes[5][2] = {{1280, 800}, {1024, 700}, {900, 600}, {640, 420}, {420, 300}};
    for (const int (&sz)[2] : sizes) {
        const int w = sz[0];
        const int h = sz[1];
        const Plan p = PlanLayout(w, h, widths, kBtnCount);

        struct Entry {
            std::string name;
            RECT rc;
        };
        std::vector<Entry> rects;
        rects.push_back({"title", p.title});
        rects.push_back({"subtitle", p.subtitle});
        rects.push_back({"infoCard", p.infoCard});
        rects.push_back({"noteCard", p.noteCard});
        rects.push_back({"receipt", p.receipt});
        for (int i = 0; i < p.buttonCount; ++i)
            rects.push_back({std::string("button#") + std::to_string(kButtons[i].id),
                             p.buttons[i]});

        int overlaps = 0;
        int outOfBounds = 0;
        int empty = 0;
        for (size_t i = 0; i < rects.size(); ++i) {
            const RECT& r = rects[i].rc;
            if (!ValidRect(r)) { ++empty; continue; }
            if (r.left < 0 || r.top < 0 || r.right > w || r.bottom > h) ++outOfBounds;
            for (size_t j = i + 1; j < rects.size(); ++j)
                if (RectsOverlap(r, rects[j].rc)) ++overlaps;
        }
        const bool ok = (overlaps == 0 && outOfBounds == 0);
        if (!ok) ++failures;
        out += std::string("  ") + std::to_string(w) + "x" + std::to_string(h) +
               ": rects=" + std::to_string(rects.size()) + " empty=" + std::to_string(empty) +
               " overlaps=" + std::to_string(overlaps) +
               " outOfBounds=" + std::to_string(outOfBounds) + (ok ? " OK" : " FAIL") + "\n";
    }

    if (page != nullptr && page == g.root) {
        RECT client{};
        GetClientRect(page, &client);
        const Plan p = PlanLayout(client.right - client.left, client.bottom - client.top, widths,
                                  kBtnCount);
        int liveOverlaps = 0;
        RECT prev{};
        for (int i = 0; i < p.buttonCount; ++i) {
            HWND btn = GetDlgItem(page, kButtons[i].id);
            if (btn == nullptr) continue;
            RECT r{};
            if (!GetWindowRect(btn, &r)) continue;
            POINT tl{r.left, r.top};
            ScreenToClient(page, &tl);
            RECT lr = MakeRect(tl.x, tl.y, tl.x + (r.right - r.left), tl.y + (r.bottom - r.top));
            if (ValidRect(prev) && RectsOverlap(prev, lr)) ++liveOverlaps;
            prev = lr;
        }
        out += std::string("  live window: buttonOverlaps=") + std::to_string(liveOverlaps) + "\n";
        if (liveOverlaps != 0) ++failures;
    }

    out += failures == 0 ? "  result: PASS\n" : "  result: FAIL\n";
    return out;
}

}  // namespace ui
}  // namespace gopt
