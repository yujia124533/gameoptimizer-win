// =============================================================================
// GameOptimizer v1.1.0 — 页面组 B：启动项页实现（page_startup.cpp）
// -----------------------------------------------------------------------------
// API 白名单：user32 + gdi32（经 ui_theme/ui_widgets）。
// 本文件不调用任何注册表 API（RegOpenKeyExW/RegSetValueExW/RegDeleteValueW 全无），
// 也不装全局钩子；启动项数据与增删改全部经 Hooks 交宿主（StartupManager）。
//
// 语义要点（与 v1.0.19 一致，不回退）：
//   * 禁用/启用都传「原始值名」给宿主 —— StartupManager::Disable/Enable 内部自行加/去
//     "[disabled] " 前缀；页面在显示前剥掉前缀，避免出现 "[disabled] [disabled] x"。
//   * 禁用 = 改名 + 写备份（核心实现），页面没有任何删除注册表值的路径；
//   * 「恢复全部」= 按备份还原；三者都走同一 RefreshList()，列表与选中随操作即时更新。
//   * 选中项按「hive + 原始值名」保持：改名不会导致选中丢失。
// =============================================================================

#include "gui/page_startup.h"

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

// "[disabled] " 前缀（与 src/tuning/StartupManager.cpp 的 DisabledName 完全一致）
const char kDisabledPrefix[] = "[disabled] ";
constexpr size_t kDisabledPrefixLen = 11;

bool HasDisabledPrefix(const std::string& name) {
    return name.size() >= kDisabledPrefixLen &&
           name.compare(0, kDisabledPrefixLen, kDisabledPrefix) == 0;
}

std::string BaseName(const std::string& name) {
    return HasDisabledPrefix(name) ? name.substr(kDisabledPrefixLen) : name;
}

// -----------------------------------------------------------------------------
// 按钮 / 文案
// -----------------------------------------------------------------------------
struct ButtonSpec {
    int id;
    const char* zh;
    const char* en;
};

const ButtonSpec kButtons[PageStartup::kBtnCount] = {
    {PageStartup::kBtnRefresh, "刷新",     "Refresh"},
    {PageStartup::kBtnDisable, "禁用选中", "Disable"},
    {PageStartup::kBtnEnable,  "启用选中", "Enable"},
    {PageStartup::kBtnRestore, "恢复全部", "Restore All"},
};

std::string LabelOf(int id) {
    for (const ButtonSpec& b : kButtons)
        if (b.id == id) return Tr(b.zh, b.en);
    return std::string();
}

std::string PageTitle() { return Tr("启动项", "Startup"); }

// -----------------------------------------------------------------------------
// 布局计划（纯几何；与进程页同一套「侧栏 / 堆叠」策略）
// -----------------------------------------------------------------------------
struct Plan {
    RECT title{};
    RECT subtitle{};
    RECT listCard{};
    RECT list{};
    RECT noteCard{};
    RECT receipt{};
    RECT buttons[PageStartup::kBtnCount]{};
    int  buttonCount = 0;
    bool sideBySide = false;
    bool noteVisible = false;
};

Plan PlanLayout(int w, int h, const int* btnW, int btnN) {
    Plan p;
    if (w <= 0 || h <= 0) return p;
    const int pad = UiSp(UiSpace::Md);
    const int gap = UiSp(UiSpace::Sm);
    const int gap2 = UiSp(UiSpace::Md);
    const int left = pad;
    const int right = std::max(left, w - pad);

    int y = pad;
    p.title = MakeRect(left, y, right, y + UiSp(UiSpace::Xl));
    y = p.title.bottom + UiSp(UiSpace::Xs);
    p.subtitle = MakeRect(left, y, right, y + UiSp(UiSpace::Lg));
    y = p.subtitle.bottom + gap;

    const int recH = UiSp(UiSpace::Xl) + UiSp(UiSpace::Md);
    const int noteH = UiSp(UiSpace::Xl) * 2 + UiSp(UiSpace::Md);
    const int minContentH = UiControlHeight(UiControlH::Lg);

    // 底部三段落自下而上预留：[内容区][策略卡][回执卡]；放不下的段落直接省略（绝不重叠）
    int contentBottom = 0;
    int noteTop = 0;
    int receiptTop = 0;
    bool hasNote = false;
    bool hasReceipt = false;
    const int budget = (h - pad) - y;
    if (budget >= minContentH + gap2 + noteH + gap + recH) {
        contentBottom = y + (budget - (gap2 + noteH + gap + recH));
        noteTop = contentBottom + gap2;
        receiptTop = h - pad - recH;
        hasNote = true;
        hasReceipt = true;
    } else if (budget >= minContentH + gap2 + recH) {
        contentBottom = y + (budget - (gap2 + recH));
        receiptTop = h - pad - recH;
        hasReceipt = true;
    } else if (budget >= minContentH) {
        contentBottom = h - pad;
    } else {
        contentBottom = std::max(y + 1, h - pad);
    }
    p.noteCard = hasNote ? MakeRect(left, noteTop, right, noteTop + noteH) : MakeRect(0, 0, 0, 0);
    p.receipt = hasReceipt ? MakeRect(left, receiptTop, right, receiptTop + recH)
                           : MakeRect(0, 0, 0, 0);
    p.noteVisible = hasNote;

    const int bH = UiControlHeight(UiControlH::Lg);
    int maxBtnW = 0;
    for (int i = 0; i < btnN; ++i)
        maxBtnW = std::max(maxBtnW, btnW != nullptr ? btnW[i] : 0);
    const int btnColW = std::max(UiControlHeight(UiControlH::Xl),
                                 std::min(maxBtnW, std::max(1, (right - left) / 3)));
    const int vBtnTotal = btnN > 0 ? btnN * bH + (btnN - 1) * gap : 0;
    const bool canSide = vBtnTotal > 0 &&
                         (right - left - gap2 - btnColW) >= UiScaleAt(340, UiDpi()) &&
                         (contentBottom - y) >= vBtnTotal;

    if (canSide) {
        p.sideBySide = true;
        p.list = MakeRect(left, y, right - gap2 - btnColW, contentBottom);
        int by = y;
        for (int i = 0; i < btnN; ++i) {
            int top = by;
            int bot = by + bH;
            if (bot > contentBottom) bot = contentBottom;
            if (bot - top < 1) { top = std::max(0, contentBottom - 1); bot = top + 1; }
            p.buttons[i] = MakeRect(right - btnColW, top, right, bot);
            ++p.buttonCount;
            by += bH + gap;
        }
    } else {
        // 横排按钮：多行放不下时退化为**单行等宽**（单行共享同一 y、x 互不重叠 → 不会重叠）
        int rows = 1;
        {
            int x = left;
            for (int i = 0; i < btnN; ++i) {
                const int bw = ClampInt(btnW != nullptr ? btnW[i] : 0,
                                        UiControlHeight(UiControlH::Xl), right - left);
                if (x != left && x + bw > right) { x = left; ++rows; }
                x += bw + gap;
            }
        }
        const int listMinH = UiControlHeight(UiControlH::Lg);
        const bool singleRow = rows > 1 &&
                               (rows * bH + (rows - 1) * gap) > (contentBottom - (y + listMinH));
        if (singleRow) rows = 1;
        const int btnTotalH = rows * bH + (rows - 1) * gap;
        int btnTop = contentBottom - btnTotalH;
        if (btnTop < y + listMinH) btnTop = y + listMinH;
        if (btnTop > contentBottom) btnTop = contentBottom;
        p.list = MakeRect(left, y, right, std::max(y + 1, btnTop - gap2));
        const int eqW = singleRow && btnN > 0
                            ? std::max(1, ((right - left) - (btnN - 1) * gap) / btnN)
                            : 0;
        int bx = left;
        int by = btnTop;
        for (int i = 0; i < btnN; ++i) {
            const int bw = singleRow
                               ? eqW
                               : ClampInt(btnW != nullptr ? btnW[i] : 0,
                                          UiControlHeight(UiControlH::Xl), right - left);
            if (!singleRow && bx != left && bx + bw > right) { bx = left; by += bH + gap; }
            const int bxr = std::min(bx + bw, right);
            int top = by;
            int bot = by + bH;
            if (bot > contentBottom) bot = contentBottom;
            if (bot - top < 1) { top = std::max(0, contentBottom - 1); bot = top + 1; }
            p.buttons[i] = MakeRect(bx, top, bxr, bot);
            ++p.buttonCount;
            bx = bxr + gap;
        }
    }

    // 策略卡与回执卡的矩形已在上方按「预留预算」算好（放不下就为空矩形 → 不绘制、不占位）

    p.listCard = p.list;
    if (ValidRect(p.listCard)) InflateRect(&p.listCard, UiSp(UiSpace::Xs), UiSp(UiSpace::Xs));
    return p;
}

// -----------------------------------------------------------------------------
// 页面上下文（单实例）
// -----------------------------------------------------------------------------
struct Row {
    std::string hive;
    std::string baseName;
    std::string value;
    bool disabled = false;
    std::string left;
    std::string right;
};

struct Ctx {
    HWND root = nullptr;
    PageStartup::Hooks hooks;
    std::vector<Row> rows;
    std::string placeholder;
    int hoverId = 0;
    int hoverItem = -1;
    std::string receiptText;
    UiTone receiptTone = UiTone::Neutral;
    std::string countBadge;
    std::string disabledBadge;
    std::string subtitleText;
    Plan plan{};
    int btnW[PageStartup::kBtnCount] = {};
    UIPaintBuffer buf{};
    HBRUSH brushPanel = nullptr;
    HBRUSH brushSurface = nullptr;
    std::map<HWND, WNDPROC> oldProc;
};

Ctx g;

constexpr int kSelSep = 0x1f;  // hive/baseName 组合键分隔符（不会出现在值名里）

std::string SelKeyOf(const Row& r) {
    return r.hive + static_cast<char>(kSelSep) + r.baseName;
}

// -----------------------------------------------------------------------------
// 绘制工具
// -----------------------------------------------------------------------------
void DropBrushes() {
    if (g.brushPanel != nullptr) { DeleteObject(g.brushPanel); g.brushPanel = nullptr; }
    if (g.brushSurface != nullptr) { DeleteObject(g.brushSurface); g.brushSurface = nullptr; }
}

HBRUSH PanelBrush() {
    if (g.brushPanel == nullptr) g.brushPanel = CreateSolidBrush(UiColor(UiColorRole::PanelBg));
    return g.brushPanel;
}

HBRUSH SurfaceBrush() {
    if (g.brushSurface == nullptr) g.brushSurface = CreateSolidBrush(UiColor(UiColorRole::Surface));
    return g.brushSurface;
}

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
        HGDIOBJ oldPen = SelectObject(dc, GetObjectType(GetStockObject(NULL_PEN)) != 0
                                               ? SelectObject(dc, GetStockObject(NULL_PEN))
                                               : nullptr);
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

int RowHeight() {
    int h = UiControlHeight(UiControlH::Sm);
    HDC dc = CreateCompatibleDC(nullptr);
    if (dc != nullptr) {
        HGDIOBJ oldFont = UiSelectFont(dc, UiFontRole::Body);
        const int textH = UiTextHeight(dc, "Ag中");
        if (oldFont != nullptr) SelectObject(dc, oldFont);
        DeleteDC(dc);
        if (textH > 0) h = std::max(h, textH + UiSp(UiSpace::Sm));
    }
    return h;
}

// -----------------------------------------------------------------------------
// 列表 / 摘要
// -----------------------------------------------------------------------------
int SelectedIndexInternal(HWND list) {
    if (list == nullptr) return -1;
    const int sel = static_cast<int>(SendMessageW(list, LB_GETCURSEL, 0, 0));
    if (sel < 0 || sel >= static_cast<int>(g.rows.size())) return -1;
    return sel;
}

void UpdateSummary() {
    int disabled = 0;
    for (const Row& r : g.rows)
        if (r.disabled) ++disabled;
    g.countBadge = std::string(Tr("共 ", "Total ")) + std::to_string(g.rows.size()) + Tr(" 项", "");
    g.disabledBadge = std::string(Tr("已禁用 ", "Disabled ")) + std::to_string(disabled) + Tr(" 项", "");

    HWND list = g.root != nullptr ? GetDlgItem(g.root, PageStartup::kList) : nullptr;
    const int sel = SelectedIndexInternal(list);
    if (sel < 0) {
        g.subtitleText = Tr("仅 HKCU/HKLM 的 Run 键；选中一行后这里显示它的命令行。",
                            "HKCU/HKLM Run keys only; select a row to see its command line here.");
    } else {
        const Row& r = g.rows[static_cast<size_t>(sel)];
        g.subtitleText = std::string(Tr("选中 ", "Selected ")) + r.hive + " · " + r.baseName +
                         Tr(" ｜命令：", " | command: ") + r.value;
    }
}

void RefreshList(bool announce) {
    if (g.root == nullptr) return;
    HWND list = GetDlgItem(g.root, PageStartup::kList);
    if (list == nullptr) return;

    // 1) 记录重排前的选中键（hive + 原始值名；禁用会改名，所以不能用值名原始串）
    std::string keyBefore;
    {
        const int sel = SelectedIndexInternal(list);
        if (sel >= 0) keyBefore = SelKeyOf(g.rows[static_cast<size_t>(sel)]);
    }

    // 2) 采集（宿主读注册表）
    std::vector<PageStartup::StartupRowInfo> infos;
    if (g.hooks.list) infos = g.hooks.list();
    std::vector<Row> rows;
    rows.reserve(infos.size());
    for (const PageStartup::StartupRowInfo& in : infos) {
        Row r;
        r.hive = in.hive.empty() ? std::string("HKCU") : in.hive;
        r.value = in.value;
        r.baseName = BaseName(in.name);
        r.disabled = in.disabled || HasDisabledPrefix(in.name);
        r.left = "[" + r.hive + "] " + r.baseName;
        r.right = r.disabled ? Tr("已禁用", "Disabled") : Tr("已启用", "Enabled");
        rows.push_back(r);
    }
    g.rows.swap(rows);

    // 3) 重填（暂停重绘消闪）+ 按选中键恢复
    SendMessageW(list, WM_SETREDRAW, FALSE, 0);
    SendMessageW(list, LB_RESETCONTENT, 0, 0);
    if (g.rows.empty()) {
        g.placeholder = Tr("（没有启动项）", "(no startup entries)");
        const std::wstring w = ToWide(g.placeholder);
        SendMessageW(list, LB_ADDSTRING, 0, reinterpret_cast<LPARAM>(w.c_str()));
        SendMessageW(list, LB_SETCURSEL, static_cast<WPARAM>(-1), 0);
        g.hoverItem = -1;
    } else {
        for (const Row& r : g.rows) {
            const std::wstring w = ToWide(r.left + "  " + r.right);
            SendMessageW(list, LB_ADDSTRING, 0, reinterpret_cast<LPARAM>(w.c_str()));
        }
        int restore = 0;
        for (size_t i = 0; i < g.rows.size(); ++i)
            if (!keyBefore.empty() && SelKeyOf(g.rows[i]) == keyBefore) {
                restore = static_cast<int>(i);
                break;
            }
        SendMessageW(list, LB_SETCURSEL, static_cast<WPARAM>(restore), 0);
    }
    SendMessageW(list, WM_SETREDRAW, TRUE, 0);
    InvalidateRect(list, nullptr, TRUE);

    UpdateSummary();
    InvalidatePage();

    if (announce) {
        int disabled = 0;
        for (const Row& r : g.rows)
            if (r.disabled) ++disabled;
        PushReceipt(std::string(Tr("已刷新：", "Refreshed: ")) + std::to_string(g.rows.size()) +
                        Tr(" 项，其中已禁用 ", " entries, disabled ") + std::to_string(disabled) +
                        Tr(" 项。", "."),
                    UiTone::Info);
    }
}

// -----------------------------------------------------------------------------
// 动作
// -----------------------------------------------------------------------------
void DoToggle(bool disable) {
    if (g.root == nullptr) return;
    HWND list = GetDlgItem(g.root, PageStartup::kList);
    const int sel = SelectedIndexInternal(list);
    if (sel < 0) {
        PushReceipt(Tr("请先在列表中选择要操作的启动项。", "Please select a startup entry first."),
                    UiTone::Warning);
        return;
    }
    const Row row = g.rows[static_cast<size_t>(sel)];   // 拷贝：刷新后原引用会失效
    if (disable == row.disabled) {
        PushReceipt(row.disabled
                        ? std::string(Tr("「", "\"")) + row.baseName +
                              Tr("」当前已是禁用状态（无需重复禁用）。", "\" is already disabled.")
                        : std::string(Tr("「", "\"")) + row.baseName +
                              Tr("」当前是启用状态（无需重复启用）。", "\" is already enabled."),
                    UiTone::Info);
        return;
    }
    const std::function<PageStartup::OpResult(const std::string&)>& hook =
        disable ? g.hooks.disable : g.hooks.enable;
    if (!hook) {
        PushReceipt(Tr("未接线：禁用/启用需要宿主提供 Hooks::disable / Hooks::enable。",
                       "Not wired: Hooks::disable / Hooks::enable are required."),
                    UiTone::Warning);
        return;
    }
    const PageStartup::OpResult res = hook(row.baseName);   // 传原始值名（核心内部自行加前缀）
    std::string text = res.text;
    if (text.empty()) {
        text = std::string(disable ? Tr("已禁用 ", "Disabled ") : Tr("已启用 ", "Enabled ")) +
               row.baseName + ": " + (res.ok ? "OK" : "FAIL") +
               (disable ? Tr("（可「恢复全部」一键还原）", " (Restore All reverts it)") : "");
    }
    PushReceipt(text, res.ok ? UiTone::Success : UiTone::Danger);
    RefreshList(false);
}

void DoRestoreAll() {
    if (g.root == nullptr) return;
    if (!g.hooks.restoreAll) {
        PushReceipt(Tr("未接线：恢复全部需要宿主提供 Hooks::restoreAll。",
                       "Not wired: Hooks::restoreAll is required."),
                    UiTone::Warning);
        return;
    }
    const PageStartup::OpResult res = g.hooks.restoreAll();
    std::string text = res.text;
    if (text.empty()) {
        text = std::string(Tr("已恢复 ", "Restored ")) + std::to_string(res.affected) +
               Tr(" 个启动项。", " startup entries.");
    }
    PushReceipt(text, res.ok ? UiTone::Success : UiTone::Danger);
    RefreshList(false);
}

// -----------------------------------------------------------------------------
// 布局应用
// -----------------------------------------------------------------------------
void RecalcButtonWidths() {
    HDC dc = CreateCompatibleDC(nullptr);
    for (int i = 0; i < PageStartup::kBtnCount; ++i) {
        int w = UiControlHeight(UiControlH::Xl);
        if (dc != nullptr) {
            HGDIOBJ oldFont = UiSelectFont(dc, UiFontRole::BodyBold);
            const int measured = UiButtonMinWidth(dc, LabelOf(kButtons[i].id), false);
            if (oldFont != nullptr) SelectObject(dc, oldFont);
            w = std::max(w, measured);
        }
        g.btnW[i] = ClampInt(w, UiControlHeight(UiControlH::Lg), UiScaleAt(300, UiDpi()));
    }
    if (dc != nullptr) DeleteDC(dc);
}

void LayoutSelf() {
    if (g.root == nullptr) return;
    RECT rc{};
    if (!GetClientRect(g.root, &rc)) return;
    g.plan = PlanLayout(rc.right - rc.left, rc.bottom - rc.top, g.btnW, PageStartup::kBtnCount);
    HWND list = GetDlgItem(g.root, PageStartup::kList);
    if (list != nullptr && ValidRect(g.plan.list))
        MoveWindow(list, g.plan.list.left, g.plan.list.top, g.plan.list.right - g.plan.list.left,
                   g.plan.list.bottom - g.plan.list.top, TRUE);
    for (int i = 0; i < g.plan.buttonCount; ++i) {
        HWND btn = GetDlgItem(g.root, kButtons[i].id);
        if (btn == nullptr) continue;
        const RECT& r = g.plan.buttons[i];
        MoveWindow(btn, r.left, r.top, r.right - r.left, r.bottom - r.top, TRUE);
    }
    InvalidatePage();
}

// -----------------------------------------------------------------------------
// 绘制
// -----------------------------------------------------------------------------
void PaintContent(HDC dc, const RECT& client) {
    if (dc == nullptr) return;
    HBRUSH bg = PanelBrush();
    if (bg != nullptr) FillRect(dc, &client, bg);
    const Plan& p = g.plan;

    UiDrawTextClamped(dc, PageTitle(), p.title, DT_LEFT | DT_VCENTER,
                      UiColor(UiColorRole::TextPrimary), UiFontRole::Title);
    // 标题右侧两个统计徽标（右→左排布）
    if (ValidRect(p.title)) {
        const int bh = UiControlHeight(UiControlH::Sm);
        const int by = p.title.top +
                       std::max(0, static_cast<int>(((p.title.bottom - p.title.top) - bh) / 2));
        int x = p.title.right;
        const int w1 = UiBadgeWidth(dc, g.disabledBadge);
        if (x - w1 > p.title.left + UiSp(UiSpace::Md)) {
            UiDrawBadge(dc, x - w1, by, g.disabledBadge, UiTone::Warning);
            x -= w1 + UiSp(UiSpace::Sm);
        }
        const int w0 = UiBadgeWidth(dc, g.countBadge);
        if (x - w0 > p.title.left + UiSp(UiSpace::Md))
            UiDrawBadge(dc, x - w0, by, g.countBadge, UiTone::Info);
    }
    UiDrawTextClamped(dc, g.subtitleText, p.subtitle, DT_LEFT | DT_VCENTER,
                      UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);

    if (ValidRect(p.listCard)) UiDrawCard(dc, p.listCard, true);

    if (p.noteVisible && ValidRect(p.noteCard)) {
        UiDrawCard(dc, p.noteCard, false);
        RECT inner = p.noteCard;
        InflateRect(&inner, -UiSp(UiSpace::Md), -UiSp(UiSpace::Sm));
        RECT titleRc = inner;
        titleRc.bottom = inner.top + UiSp(UiSpace::Lg);
        UiDrawCardTitle(dc, titleRc, Tr("安全做法", "Safe by design"));
        RECT bodyRc = inner;
        bodyRc.top = titleRc.bottom;
        UiDrawTextWrap(dc,
                       Tr("禁用 = 把值名改为「[disabled] 原名」并把改动写入备份文件"
                          "（%LOCALAPPDATA%\\GameOptimizer\\startup_backup.conf）；"
                          "「恢复全部」按备份一键还原。只处理 HKCU/HKLM 的 Run 键，不删除任何值。",
                          "Disable renames the value to \"[disabled] name\" and records the change in a "
                          "backup file; Restore All reverts it from that backup. Only the HKCU/HKLM Run "
                          "keys are touched; no registry value is ever deleted."),
                       bodyRc, UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);
    }

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
// 子类化：hover
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
        if (h == GetDlgItem(g.root, PageStartup::kList)) {
            const int x = static_cast<short>(LOWORD(lp));
            const int y = static_cast<short>(HIWORD(lp));
            const DWORD r = static_cast<DWORD>(SendMessageW(
                h, LB_ITEMFROMPOINT, 0, MAKELPARAM(static_cast<WORD>(x), static_cast<WORD>(y))));
            const int idx = HIWORD(r) != 0 ? -1 : static_cast<int>(LOWORD(r));
            if (idx != g.hoverItem) { g.hoverItem = idx; InvalidateRect(h, nullptr, FALSE); }
        } else {
            const int id = GetDlgCtrlID(h);
            if (id != g.hoverId) { g.hoverId = id; InvalidateRect(h, nullptr, TRUE); }
        }
    } else if (msg == WM_MOUSELEAVE) {
        if (h == GetDlgItem(g.root, PageStartup::kList)) {
            if (g.hoverItem != -1) { g.hoverItem = -1; InvalidateRect(h, nullptr, FALSE); }
        } else if (g.hoverId == GetDlgCtrlID(h)) {
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
            return 1;
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
        case WM_MEASUREITEM: {
            auto* mis = reinterpret_cast<MEASUREITEMSTRUCT*>(lp);
            if (mis != nullptr && mis->CtlType == ODT_LISTBOX &&
                mis->CtlID == static_cast<UINT>(PageStartup::kList)) {
                mis->itemHeight = static_cast<UINT>(RowHeight());
                return TRUE;
            }
        } break;
        case WM_DRAWITEM: {
            auto* di = reinterpret_cast<DRAWITEMSTRUCT*>(lp);
            if (di == nullptr) break;
            if (di->CtlType == ODT_LISTBOX && di->CtlID == static_cast<UINT>(PageStartup::kList)) {
                const int idx = static_cast<int>(di->itemID);
                const bool empty = idx < 0 || idx >= static_cast<int>(g.rows.size());
                UiRowState state = (di->itemState & ODS_SELECTED) != 0 ? UiRowState::Selected
                                                                        : UiRowState::Normal;
                if ((di->itemState & ODS_DISABLED) != 0 || empty) state = UiRowState::Disabled;
                else if (idx == g.hoverItem) state = UiRowState::Hover;
                const RECT rc = di->rcItem;
                RECT leftRc = rc;
                leftRc.right = rc.left + (rc.right - rc.left) * 60 / 100;
                RECT rightRc = rc;
                rightRc.left = leftRc.right;
                const std::string leftText = empty ? g.placeholder : g.rows[idx].left;
                const std::string rightText = empty ? std::string() : g.rows[idx].right;
                const bool zebra = (idx % 2) == 1;
                UiDrawListRow(di->hDC, rc, state, leftText, zebra);
                if (!rightText.empty()) UiDrawListRowRight(di->hDC, rightRc, rightText, state);
                return TRUE;
            }
            if (di->CtlType == ODT_BUTTON && di->CtlID >= static_cast<UINT>(PageStartup::kIdBase) &&
                di->CtlID <= static_cast<UINT>(PageStartup::kIdLast)) {
                UiButtonState state = UiButtonStateFromDrawItem(di);
                if (state == UiButtonState::Normal && g.hoverId == static_cast<int>(di->CtlID))
                    state = UiButtonState::Hot;
                if (UiDrawButton(di->hDC, di->rcItem, state, LabelOf(static_cast<int>(di->CtlID)),
                                 di->CtlID == static_cast<UINT>(PageStartup::kBtnDisable)))
                    return TRUE;
            }
        } break;
        case WM_CTLCOLORBTN:
            if (reinterpret_cast<HDC>(wp) != nullptr) {
                SetBkColor(reinterpret_cast<HDC>(wp), UiColor(UiColorRole::PanelBg));
                SetTextColor(reinterpret_cast<HDC>(wp), UiColor(UiColorRole::TextPrimary));
            }
            if (PanelBrush() != nullptr) return reinterpret_cast<LRESULT>(PanelBrush());
            break;
        case WM_CTLCOLORLISTBOX:
            if (reinterpret_cast<HDC>(wp) != nullptr) {
                SetBkColor(reinterpret_cast<HDC>(wp), UiColor(UiColorRole::Surface));
                SetTextColor(reinterpret_cast<HDC>(wp), UiColor(UiColorRole::TextPrimary));
            }
            if (SurfaceBrush() != nullptr) return reinterpret_cast<LRESULT>(SurfaceBrush());
            break;
        case WM_COMMAND: {
            const int id = LOWORD(wp);
            const int code = HIWORD(wp);
            if (id == PageStartup::kList) {
                if (code == LBN_SELCHANGE) {
                    UpdateSummary();
                    InvalidatePage();
                }
                return 0;
            }
            if (code != BN_CLICKED) return 0;
            switch (id) {
                case PageStartup::kBtnRefresh: RefreshList(true);  return 0;
                case PageStartup::kBtnDisable: DoToggle(true);     return 0;
                case PageStartup::kBtnEnable:  DoToggle(false);    return 0;
                case PageStartup::kBtnRestore: DoRestoreAll();     return 0;
                default: break;
            }
        } break;
        case WM_DESTROY:
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
HWND PageStartup::Create(HWND parent, const RECT& rc, const Hooks& hooks) {
    if (parent == nullptr) return nullptr;
    if (g.root != nullptr && IsWindow(g.root)) return g.root;
    if (UiSp(UiSpace::Md) <= 0) UiThemeInit();
    g = Ctx{};

    WNDCLASSEXW wc{};
    wc.cbSize = sizeof(wc);
    wc.style = CS_HREDRAW | CS_VREDRAW;
    wc.lpfnWndProc = PageProc;
    wc.hInstance = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
    wc.hCursor = LoadCursorW(nullptr, MAKEINTRESOURCEW(32512));
    wc.hbrBackground = nullptr;
    wc.lpszClassName = kClassName;
    if (RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) return nullptr;

    const HINSTANCE hInst = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
    HWND root = CreateWindowExW(0, kClassName, L"",
                                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS, rc.left,
                                rc.top, std::max(1, static_cast<int>(rc.right - rc.left)),
                                std::max(1, static_cast<int>(rc.bottom - rc.top)), parent,
                                reinterpret_cast<HMENU>(static_cast<INT_PTR>(kIdBase)), hInst,
                                nullptr);
    if (root == nullptr) return nullptr;
    g.root = root;
    g.hooks = hooks;
    g.placeholder = Tr("（没有启动项）", "(no startup entries)");
    g.countBadge = Tr("共 0 项", "Total 0");
    g.disabledBadge = Tr("已禁用 0 项", "Disabled 0");

    HWND list = CreateWindowExW(0, L"LISTBOX", L"",
                                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | LBS_NOTIFY |
                                    LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT,
                                0, 0, 10, 10, root,
                                reinterpret_cast<HMENU>(static_cast<INT_PTR>(kList)), hInst, nullptr);
    if (list != nullptr) {
        if (HFONT font = UiFont(UiFontRole::Body))
            SendMessageW(list, WM_SETFONT, reinterpret_cast<WPARAM>(font), TRUE);
        SubclassControl(list);
    }
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
    RecalcButtonWidths();
    LayoutSelf();
    RefreshList(false);
    SetReceiptQuiet(Tr("就绪：选中一行后可禁用/启用；「恢复全部」按备份一键还原。",
                       "Ready: select a row to disable/enable; Restore All reverts from the backup."),
                    UiTone::Info);
    return root;
}

void PageStartup::SetHooks(HWND page, const Hooks& hooks) {
    if (page == nullptr || page != g.root) return;
    g.hooks = hooks;
    RefreshList(false);
}

void PageStartup::Layout(HWND page, const RECT& rc) {
    if (page == nullptr || page != g.root) return;
    MoveWindow(page, rc.left, rc.top, std::max(1, static_cast<int>(rc.right - rc.left)),
               std::max(1, static_cast<int>(rc.bottom - rc.top)), TRUE);
    LayoutSelf();
}

void PageStartup::FillParent(HWND page) {
    if (page == nullptr || page != g.root) return;
    HWND parent = GetParent(page);
    if (parent == nullptr) return;
    RECT rc{};
    if (!GetClientRect(parent, &rc)) return;
    Layout(page, rc);
}

void PageStartup::Show(HWND page, bool visible) {
    if (page == nullptr || page != g.root) return;
    ShowWindow(page, visible ? SW_SHOW : SW_HIDE);
    if (visible) RefreshList(false);
}

void PageStartup::Refresh(HWND page, bool announce) {
    if (page == nullptr || page != g.root) return;
    RefreshList(announce);
}

void PageStartup::ApplyLabels(HWND page) {
    if (g.root == nullptr) return;
    if (page != nullptr && page != g.root) return;
    for (const ButtonSpec& b : kButtons) {
        HWND btn = GetDlgItem(g.root, b.id);
        if (btn == nullptr) continue;
        SetWindowTextW(btn, ToWide(LabelOf(b.id)).c_str());
    }
    g.placeholder = Tr("（没有启动项）", "(no startup entries)");
    RecalcButtonWidths();
    RefreshList(false);   // 行内「已禁用/已启用」等文案随语言重建
    LayoutSelf();
    InvalidateRect(g.root, nullptr, TRUE);
}

void PageStartup::Destroy(HWND page) {
    if (page == nullptr) return;
    if (page != g.root) {
        DestroyWindow(page);
        return;
    }
    if (IsWindow(page)) DestroyWindow(page);
    g = Ctx{};
}

bool PageStartup::IsPageWindow(HWND h) {
    if (h == nullptr) return false;
    wchar_t cls[64] = {};
    if (GetClassNameW(h, cls, 64) <= 0) return false;
    return lstrcmpW(cls, kClassName) == 0;
}

std::string PageStartup::SelectedName(HWND page) {
    if (page == nullptr || page != g.root) return std::string();
    const int sel = SelectedIndexInternal(GetDlgItem(page, kList));
    return sel < 0 ? std::string() : g.rows[static_cast<size_t>(sel)].baseName;
}

std::string PageStartup::SelectedHive(HWND page) {
    if (page == nullptr || page != g.root) return std::string();
    const int sel = SelectedIndexInternal(GetDlgItem(page, kList));
    return sel < 0 ? std::string() : g.rows[static_cast<size_t>(sel)].hive;
}

// -----------------------------------------------------------------------------
// 自检
// -----------------------------------------------------------------------------
std::string PageStartup::SelfCheck(HWND page) {
    std::string out = "PageStartup self-check\n";
    int failures = 0;

    bool idsUnique = true;
    {
        const int ids[1 + kBtnCount] = {kList,  kButtons[0].id, kButtons[1].id, kButtons[2].id,
                                        kButtons[3].id};
        for (int i = 0; i < 1 + kBtnCount; ++i)
            for (int j = i + 1; j < 1 + kBtnCount; ++j)
                if (ids[i] == ids[j]) idsUnique = false;
    }
    out += std::string("  control ids: count=") + std::to_string(1 + kBtnCount) +
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
        rects.push_back({"listCard", p.listCard});
        rects.push_back({"noteCard", p.noteCard});
        rects.push_back({"receipt", p.receipt});
        for (int i = 0; i < p.buttonCount; ++i)
            rects.push_back({std::string("button#") + std::to_string(kButtons[i].id), p.buttons[i]});

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
               " outOfBounds=" + std::to_string(outOfBounds) +
               (p.sideBySide ? " layout=side" : " layout=stack") + (ok ? " OK" : " FAIL") + "\n";
    }

    if (page != nullptr && page == g.root) {
        RECT client{};
        GetClientRect(page, &client);
        const Plan p = PlanLayout(client.right - client.left, client.bottom - client.top, widths,
                                  kBtnCount);
        std::vector<RECT> live;
        HWND list = GetDlgItem(page, kList);
        if (list != nullptr) {
            RECT r{};
            if (GetWindowRect(list, &r)) {
                POINT tl{r.left, r.top};
                ScreenToClient(page, &tl);
                live.push_back(MakeRect(tl.x, tl.y, tl.x + (r.right - r.left),
                                        tl.y + (r.bottom - r.top)));
            }
        }
        for (int i = 0; i < p.buttonCount; ++i) {
            HWND btn = GetDlgItem(page, kButtons[i].id);
            if (btn == nullptr) continue;
            RECT r{};
            if (!GetWindowRect(btn, &r)) continue;
            POINT tl{r.left, r.top};
            ScreenToClient(page, &tl);
            live.push_back(MakeRect(tl.x, tl.y, tl.x + (r.right - r.left), tl.y + (r.bottom - r.top)));
        }
        int liveOverlaps = 0;
        for (size_t i = 0; i < live.size(); ++i)
            for (size_t j = i + 1; j < live.size(); ++j)
                if (RectsOverlap(live[i], live[j])) ++liveOverlaps;
        out += std::string("  live window: childRects=") + std::to_string(live.size()) +
               " overlaps=" + std::to_string(liveOverlaps) + "\n";
        if (liveOverlaps != 0) ++failures;
    }

    out += failures == 0 ? "  result: PASS\n" : "  result: FAIL\n";
    return out;
}

}  // namespace ui
}  // namespace gopt
