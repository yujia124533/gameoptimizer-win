// =============================================================================
// GameOptimizer v1.1.0 — 页面组 B：进程页实现（page_process.cpp）
// -----------------------------------------------------------------------------
// API 白名单：user32 + gdi32（经 ui_theme/ui_widgets）。
// 本文件不调用 OpenProcess / SetPriorityClass / ReadProcessMemory 等任何进程 API，
// 也不装任何全局钩子；进程数据来自 Hooks::snapshot，优先级修改来自 Hooks::setPriority。
//
// 采样口径（与 v1.0.19 gopt_gui.cpp 完全一致，避免语义回退）：
//   CPU% = 100 * Δ(cpuTicks) / (Δms * 10000 * logicalCores)   // ticks 为 100ns
//   排序 = std::stable_sort(CPU% 降序)                          // 同值/首轮无采样保持枚举顺序
//   选中 = 先记 pid，重排后按 pid 找回落点，找不到回退第 0 行      // 空列表清空选择
// =============================================================================

#include "gui/page_process.h"

#include <algorithm>
#include <cstdio>
#include <map>
#include <string>
#include <utility>
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

std::string Fmt1(double v) {
    char buf[32] = {};
    std::snprintf(buf, sizeof(buf), "%.1f", v);
    return std::string(buf);
}

// -----------------------------------------------------------------------------
// 按钮 / 文案
// -----------------------------------------------------------------------------
struct ButtonSpec {
    int id;
    const char* zh;
    const char* en;
};

const ButtonSpec kButtons[PageProcess::kBtnCount] = {
    {PageProcess::kBtnRefresh, "刷新",       "Refresh"},
    {PageProcess::kBtnBoost,   "提升优先级", "Boost Priority"},
    {PageProcess::kBtnNormal,  "恢复正常",   "Reset Normal"},
};

std::string LabelOf(int id) {
    for (const ButtonSpec& b : kButtons)
        if (b.id == id) return Tr(b.zh, b.en);
    return std::string();
}

const char* PriorityName(uint32_t cls) {
    switch (cls) {
        case IDLE_PRIORITY_CLASS:         return Tr("空闲", "Idle");
        case BELOW_NORMAL_PRIORITY_CLASS: return Tr("低于正常", "BelowNormal");
        case NORMAL_PRIORITY_CLASS:       return Tr("正常", "Normal");
        case ABOVE_NORMAL_PRIORITY_CLASS: return Tr("高于正常", "AboveNormal");
        case HIGH_PRIORITY_CLASS:         return Tr("高", "High");
        case REALTIME_PRIORITY_CLASS:     return Tr("实时(不使用)", "Realtime(unused)");
        default:                          return Tr("未知", "unknown");
    }
}

std::string PageTitle() { return Tr("进程", "Processes"); }
std::string PageSubtitle() {
    return Tr("按 CPU% 降序；选中项按 PID 保持；双击行 = 提升优先级（上限 HIGH，不使用 REALTIME）。",
              "Sorted by CPU% desc; selection kept by PID; double-click a row = boost priority (capped at HIGH, never REALTIME).");
}

// -----------------------------------------------------------------------------
// 布局计划（纯几何）
// -----------------------------------------------------------------------------
struct Plan {
    RECT title{};
    RECT subtitle{};
    RECT metric[3]{};
    RECT listCard{};
    RECT list{};
    RECT receipt{};
    RECT buttons[PageProcess::kBtnCount]{};
    int  buttonCount = 0;
    bool sideBySide = false;
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

    const int metricH = UiControlHeight(UiControlH::Xl) + UiSp(UiSpace::Md);
    const int colW = ((right - left) - 2 * gap) / 3;
    if (colW > 0) {
        int mx = left;
        for (int i = 0; i < 3; ++i) {
            p.metric[i] = MakeRect(mx, y, mx + colW, y + metricH);
            mx += colW + gap;
        }
        p.metric[2].right = right;
    }
    y += metricH + gap;

    const int recH = UiSp(UiSpace::Xl) + UiSp(UiSpace::Md);
    const int bottomLimit = h - pad - recH;
    if (bottomLimit > y) {
        p.receipt = MakeRect(left, bottomLimit, right, bottomLimit + recH);
    } else {
        // 空间不足：省略回执卡（回执仍进宿主日志/状态栏），绝不与内容区重叠
        p.receipt = MakeRect(0, 0, 0, 0);
    }
    int contentBottom = ValidRect(p.receipt) ? (p.receipt.top - gap2) : (h - pad);
    if (contentBottom < y + UiControlHeight(UiControlH::Lg)) {
        p.receipt = MakeRect(0, 0, 0, 0);          // 贴底回执会挤掉内容区 → 放弃回执，内容区吃满
        contentBottom = std::max(y + 1, h - pad);
    }

    const int bH = UiControlHeight(UiControlH::Lg);
    int maxBtnW = 0;
    for (int i = 0; i < btnN; ++i)
        maxBtnW = std::max(maxBtnW, btnW != nullptr ? btnW[i] : 0);
    const int btnColW = std::max(UiControlHeight(UiControlH::Xl),
                                 std::min(maxBtnW, std::max(1, (right - left) / 3)));
    const int vBtnTotal = btnN > 0 ? btnN * bH + (btnN - 1) * gap : 0;
    const bool canSide = vBtnTotal > 0 &&
                         (right - left - gap2 - btnColW) >= UiScaleAt(360, UiDpi()) &&
                         (contentBottom - y) >= vBtnTotal;

    if (canSide) {
        p.sideBySide = true;
        p.list = MakeRect(left, y, right - gap2 - btnColW, contentBottom);
        int by = y;
        for (int i = 0; i < btnN; ++i) {
            int bot = by + bH;
            if (bot > contentBottom) bot = contentBottom;
            int top = by;
            if (bot - top < 1) { top = std::max(0, contentBottom - 1); bot = top + 1; }
            p.buttons[i] = MakeRect(right - btnColW, top, right, bot);
            ++p.buttonCount;
            by += bH + gap;
        }
    } else {
        // 横排按钮：先按文本宽度算行数；若「多行」在可用高度内放不下，退化为**单行等宽**
        // （单行时所有按钮共享同一 y、x 互不重叠 → 结构上不可能重叠）
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

    p.listCard = p.list;
    if (ValidRect(p.listCard)) InflateRect(&p.listCard, UiSp(UiSpace::Xs), UiSp(UiSpace::Xs));
    return p;
}

// -----------------------------------------------------------------------------
// 页面上下文（单实例）
// -----------------------------------------------------------------------------
struct Row {
    std::string name;
    uint32_t pid = 0;
    uint32_t priorityClass = 0;
    bool accessible = false;
    double cpuPct = 0.0;
    uint64_t memMB = 0;
    std::string left;
    std::string right;
};

struct Ctx {
    HWND root = nullptr;
    PageProcess::Hooks hooks;
    std::vector<Row> rows;
    std::string placeholder;
    int hoverId = 0;
    int hoverItem = -1;
    std::string receiptText;
    UiTone receiptTone = UiTone::Neutral;
    Plan plan{};
    int btnW[PageProcess::kBtnCount] = {};
    std::map<uint32_t, std::pair<uint64_t, ULONGLONG>> prev;  // pid → (累计 tick, 采样时刻)
    UIPaintBuffer buf{};
    HBRUSH brushPanel = nullptr;
    HBRUSH brushSurface = nullptr;
    std::map<HWND, WNDPROC> oldProc;
};

Ctx g;

constexpr UINT_PTR kTimerLive = 2;     // 每秒自动刷新（与 v1.0.19 的 IDT_LIVE 行为对齐）
constexpr UINT kTimerLiveMs = 1000;

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

void DrawMetricCard(HDC dc, const RECT& rc, const std::string& caption, const std::string& value,
                    COLORREF valueColor, bool big) {
    if (!ValidRect(rc)) return;
    UiDrawCard(dc, rc, true);
    RECT inner = rc;
    InflateRect(&inner, -UiSp(UiSpace::Md), -UiSp(UiSpace::Sm));
    RECT capRc = inner;
    capRc.bottom = inner.top + UiSp(UiSpace::Lg);
    RECT valRc = inner;
    valRc.top = capRc.bottom;
    if (!ValidRect(valRc)) return;
    UiDrawTextClamped(dc, caption, capRc, DT_LEFT | DT_VCENTER,
                      UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);
    if (big) {
        UiDrawMetric(dc, valRc, value, valueColor);
    } else {
        UiDrawTextClamped(dc, value, valRc, DT_LEFT | DT_VCENTER, valueColor, UiFontRole::Subtitle);
    }
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
// 列表/指标刷新
// -----------------------------------------------------------------------------
uint32_t SelectedPidInternal(HWND list) {
    if (list == nullptr) return 0;
    const int sel = static_cast<int>(SendMessageW(list, LB_GETCURSEL, 0, 0));
    if (sel < 0 || sel >= static_cast<int>(g.rows.size())) return 0;
    return g.rows[static_cast<size_t>(sel)].pid;
}

// 指标文案缓存（WM_PAINT 直接取用；数据在 RefreshProcList/选中变化时更新）
struct MetricText {
    std::string cap[3];
    std::string val[3];
};
MetricText g_metricText;

void SetMetricText(const std::string& countText, const std::string& pidText,
                   const std::string& cap2, const std::string& cpuMemText) {
    g_metricText.cap[0] = Tr("支持游戏进程数", "Game processes");
    g_metricText.val[0] = countText;
    g_metricText.cap[1] = cap2;
    g_metricText.val[1] = pidText;
    g_metricText.cap[2] = Tr("选中 CPU%｜内存", "Selected CPU% | RAM");
    g_metricText.val[2] = cpuMemText;
}

void UpdateMetrics() {
    if (g.root == nullptr) return;
    HWND list = GetDlgItem(g.root, PageProcess::kList);
    const int sel = list != nullptr ? static_cast<int>(SendMessageW(list, LB_GETCURSEL, 0, 0)) : -1;
    const bool hasSel = sel >= 0 && sel < static_cast<int>(g.rows.size());
    const Row* row = hasSel ? &g.rows[static_cast<size_t>(sel)] : nullptr;

    const std::string countText = std::to_string(g.rows.size());
    const std::string pidText = row != nullptr ? std::to_string(row->pid) : Tr("—", "-");
    const std::string cap2 = row != nullptr
        ? std::string(Tr("选中进程 PID｜优先级 ", "Selected PID | priority ")) +
              PriorityName(row->priorityClass)
        : Tr("选中进程 PID｜优先级", "Selected PID | priority");
    const std::string cpuMemText = row != nullptr
        ? (Fmt1(row->cpuPct) + "% · " + std::to_string(row->memMB) + " MB")
        : Tr("—", "-");

    SetMetricText(countText, pidText, cap2, cpuMemText);
}

void RefreshProcList(bool announce) {
    if (g.root == nullptr) return;
    HWND list = GetDlgItem(g.root, PageProcess::kList);
    if (list == nullptr) return;

    // 1) 重排前记下选中行的 pid（只能用 pid 恢复，索引会因重排变化）
    const uint32_t pidBefore = SelectedPidInternal(list);

    // 2) 采集一帧原始样本（进程访问全部由宿主完成）
    PageProcess::ProcSnapshot snap;
    if (g.hooks.snapshot) snap = g.hooks.snapshot();
    int cores = snap.logicalCores;
    if (cores < 1) cores = 1;
    const ULONGLONG now = GetTickCount64();

    std::vector<Row> rows;
    rows.reserve(snap.items.size());
    for (const PageProcess::ProcSample& it : snap.items) {
        Row r;
        r.name = it.name;
        r.pid = it.pid;
        r.priorityClass = it.priorityClass;
        r.accessible = it.accessible;
        double cpuPct = 0.0;
        const auto prev = g.prev.find(it.pid);
        if (prev != g.prev.end()) {
            const uint64_t dTicks = it.cpuTicks >= prev->second.first ? it.cpuTicks - prev->second.first
                                                                      : 0;
            const ULONGLONG dMs = now - prev->second.second;
            if (dMs > 50)
                cpuPct = 100.0 * static_cast<double>(dTicks) /
                         (static_cast<double>(dMs) * 10000.0 * static_cast<double>(cores));
        }
        r.cpuPct = cpuPct;
        r.memMB = it.memBytes / (1024ull * 1024ull);
        g.prev[it.pid] = std::make_pair(it.cpuTicks, now);

        r.left = it.name + "  (pid " + std::to_string(it.pid) + ")";
        if (it.accessible) {
            r.right = std::string(Tr("优先级 ", "Prio ")) + PriorityName(it.priorityClass) +
                      " · CPU " + Fmt1(cpuPct) + "%" + " · " +
                      Tr("内存 ", "RAM ") + std::to_string(r.memMB) + " MB";
        } else {
            r.right = Tr("指标不可用（无权限或已退出）", "metrics unavailable");
        }
        rows.push_back(r);
    }
    // 3) 清理已退出进程的采样缓存
    for (auto it = g.prev.begin(); it != g.prev.end();) {
        bool alive = false;
        for (const PageProcess::ProcSample& s : snap.items)
            if (s.pid == it->first) { alive = true; break; }
        if (!alive) it = g.prev.erase(it);
        else ++it;
    }
    // 4) 按 CPU% 降序，stable：相同值 / 首轮无采样保持宿主枚举顺序
    std::stable_sort(rows.begin(), rows.end(),
                     [](const Row& a, const Row& b) { return a.cpuPct > b.cpuPct; });
    g.rows.swap(rows);

    // 5) 重填列表（暂停重绘消闪），并按 pid 恢复选中
    SendMessageW(list, WM_SETREDRAW, FALSE, 0);
    SendMessageW(list, LB_RESETCONTENT, 0, 0);
    if (g.rows.empty()) {
        g.placeholder = Tr("（没有运行中的支持游戏）", "(no supported games running)");
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
            if (g.rows[i].pid == pidBefore) { restore = static_cast<int>(i); break; }
        SendMessageW(list, LB_SETCURSEL, static_cast<WPARAM>(restore), 0);
    }
    SendMessageW(list, WM_SETREDRAW, TRUE, 0);
    InvalidateRect(list, nullptr, TRUE);

    UpdateMetrics();
    InvalidatePage();

    if (announce) {
        PushReceipt(std::string(Tr("已刷新：", "Refreshed: ")) + std::to_string(g.rows.size()) +
                        Tr(" 个进程，按 CPU% 降序（选中项按 PID 保持）。",
                           " processes, sorted by CPU% desc (selection kept by PID)."),
                    UiTone::Info);
    }
}

// -----------------------------------------------------------------------------
// 动作
// -----------------------------------------------------------------------------
void DoBoost(bool high) {
    if (g.root == nullptr) return;
    HWND list = GetDlgItem(g.root, PageProcess::kList);
    const uint32_t pid = SelectedPidInternal(list);
    if (pid == 0) {
        PushReceipt(Tr("请先在列表中选择一个游戏进程。", "Select a game process first."),
                    UiTone::Warning);
        return;
    }
    if (!g.hooks.setPriority) {
        PushReceipt(Tr("未接线：修改优先级需要宿主提供 Hooks::setPriority。",
                       "Not wired: Hooks::setPriority is required."),
                    UiTone::Warning);
        return;
    }
    const PageProcess::OpResult r = g.hooks.setPriority(pid, high);
    std::string text = r.text;
    if (text.empty()) {
        text = std::string(Tr("进程 ", "Process ")) + std::to_string(pid) + " -> " +
               (high ? Tr("高优先级", "High priority") : Tr("正常优先级", "Normal priority")) + ": " +
               (r.ok ? "OK" : "FAIL");
    }
    PushReceipt(text, r.ok ? UiTone::Success : UiTone::Danger);
    RefreshProcList(false);
}

// -----------------------------------------------------------------------------
// 布局应用
// -----------------------------------------------------------------------------
void RecalcButtonWidths() {
    HDC dc = CreateCompatibleDC(nullptr);
    for (int i = 0; i < PageProcess::kBtnCount; ++i) {
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
    g.plan = PlanLayout(rc.right - rc.left, rc.bottom - rc.top, g.btnW, PageProcess::kBtnCount);

    HWND list = GetDlgItem(g.root, PageProcess::kList);
    if (list != nullptr && ValidRect(g.plan.list)) {
        MoveWindow(list, g.plan.list.left, g.plan.list.top, g.plan.list.right - g.plan.list.left,
                   g.plan.list.bottom - g.plan.list.top, TRUE);
    }
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
    if (ValidRect(p.title)) {
        const std::string cap = Tr("上限 HIGH · 无注入", "Cap HIGH · no injection");
        const int bw = UiBadgeWidth(dc, cap);
        const int bh = UiControlHeight(UiControlH::Sm);
        if (bw + UiSp(UiSpace::Md) < (p.title.right - p.title.left)) {
            const int by = p.title.top +
                           std::max(0, static_cast<int>(((p.title.bottom - p.title.top) - bh) / 2));
            UiDrawBadge(dc, p.title.right - bw, by, cap, UiTone::Info);
        }
    }
    UiDrawTextClamped(dc, PageSubtitle(), p.subtitle, DT_LEFT | DT_VCENTER,
                      UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);

    DrawMetricCard(dc, p.metric[0], g_metricText.cap[0], g_metricText.val[0],
                   UiColor(UiColorRole::Accent), true);
    DrawMetricCard(dc, p.metric[1], g_metricText.cap[1], g_metricText.val[1],
                   UiColor(UiColorRole::TextPrimary), true);
    DrawMetricCard(dc, p.metric[2], g_metricText.cap[2], g_metricText.val[2],
                   UiColor(UiColorRole::TextPrimary), false);

    // 列表外的圆角边框卡片（列表控件自身撑满内部，仅露 4px 边框环）
    if (ValidRect(p.listCard)) UiDrawCard(dc, p.listCard, true);

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
// 子类化：hover（按钮高亮 + 列表行悬停）
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
        if (h == GetDlgItem(g.root, PageProcess::kList)) {
            const int x = static_cast<short>(LOWORD(lp));
            const int y = static_cast<short>(HIWORD(lp));
            const DWORD r = static_cast<DWORD>(SendMessageW(
                h, LB_ITEMFROMPOINT, 0,
                MAKELPARAM(static_cast<WORD>(x), static_cast<WORD>(y))));
            const int idx = HIWORD(r) != 0 ? -1 : static_cast<int>(LOWORD(r));
            if (idx != g.hoverItem) { g.hoverItem = idx; InvalidateRect(h, nullptr, FALSE); }
        } else {
            const int id = GetDlgCtrlID(h);
            if (id != g.hoverId) { g.hoverId = id; InvalidateRect(h, nullptr, TRUE); }
        }
    } else if (msg == WM_MOUSELEAVE) {
        if (h == GetDlgItem(g.root, PageProcess::kList)) {
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
                mis->CtlID == static_cast<UINT>(PageProcess::kList)) {
                mis->itemHeight = static_cast<UINT>(RowHeight());
                return TRUE;
            }
        } break;
        case WM_DRAWITEM: {
            auto* di = reinterpret_cast<DRAWITEMSTRUCT*>(lp);
            if (di == nullptr) break;
            if (di->CtlType == ODT_LISTBOX && di->CtlID == static_cast<UINT>(PageProcess::kList)) {
                const int idx = static_cast<int>(di->itemID);
                const bool empty = idx < 0 || idx >= static_cast<int>(g.rows.size());
                UiRowState state = (di->itemState & ODS_SELECTED) != 0 ? UiRowState::Selected
                                                                        : UiRowState::Normal;
                if ((di->itemState & ODS_DISABLED) != 0 || empty) state = UiRowState::Disabled;
                else if (idx == g.hoverItem) state = UiRowState::Hover;
                const RECT rc = di->rcItem;
                RECT leftRc = rc;
                leftRc.right = rc.left + (rc.right - rc.left) * 48 / 100;
                RECT rightRc = rc;
                rightRc.left = leftRc.right;
                const std::string leftText = empty ? g.placeholder : g.rows[idx].left;
                const std::string rightText = empty ? std::string() : g.rows[idx].right;
                const bool zebra = (idx % 2) == 1;
                UiDrawListRow(di->hDC, rc, state, leftText, zebra);
                if (!rightText.empty()) UiDrawListRowRight(di->hDC, rightRc, rightText, state);
                return TRUE;
            }
            if (di->CtlType == ODT_BUTTON && di->CtlID >= static_cast<UINT>(PageProcess::kIdBase) &&
                di->CtlID <= static_cast<UINT>(PageProcess::kIdLast)) {
                UiButtonState state = UiButtonStateFromDrawItem(di);
                if (state == UiButtonState::Normal && g.hoverId == static_cast<int>(di->CtlID))
                    state = UiButtonState::Hot;
                if (UiDrawButton(di->hDC, di->rcItem, state, LabelOf(static_cast<int>(di->CtlID)),
                                 di->CtlID == static_cast<UINT>(PageProcess::kBtnBoost)))
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
            if (id == PageProcess::kList) {
                if (code == LBN_DBLCLK) {
                    DoBoost(true);   // 双击行 = 提升优先级（与按钮同一条代码路径）
                    return 0;
                }
                if (code == LBN_SELCHANGE) {
                    UpdateMetrics();
                    InvalidatePage();
                    return 0;
                }
                return 0;
            }
            if (code != BN_CLICKED) return 0;
            switch (id) {
                case PageProcess::kBtnRefresh: RefreshProcList(true); return 0;
                case PageProcess::kBtnBoost:   DoBoost(true);         return 0;
                case PageProcess::kBtnNormal:  DoBoost(false);        return 0;
                default: break;
            }
        } break;
        case WM_TIMER:
            if (wp == static_cast<WPARAM>(kTimerLive)) {
                // 仅在本页可见时自动刷新（与 v1.0.19 每秒刷新等价，但省掉隐藏页的开销）
                if (IsWindowVisible(hwnd)) RefreshProcList(false);
                return 0;
            }
            break;
        case WM_DESTROY:
            KillTimer(hwnd, kTimerLive);
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
HWND PageProcess::Create(HWND parent, const RECT& rc, const Hooks& hooks) {
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
    g.placeholder = Tr("（没有运行中的支持游戏）", "(no supported games running)");

    // 列表：LBS_OWNERDRAWFIXED（WM_MEASUREITEM 定行高）+ LBS_NOTIFY（双击/选中通知）
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
    UpdateMetrics();
    SetReceiptQuiet(Tr("就绪：列表每秒自动刷新；双击某行可提升该进程优先级。",
                       "Ready: the list auto-refreshes every second; double-click a row to boost it."),
                    UiTone::Info);
    RefreshProcList(false);
    SetTimer(root, kTimerLive, kTimerLiveMs, nullptr);
    return root;
}

void PageProcess::SetHooks(HWND page, const Hooks& hooks) {
    if (page == nullptr || page != g.root) return;
    g.hooks = hooks;
    RefreshProcList(false);
}

void PageProcess::Layout(HWND page, const RECT& rc) {
    if (page == nullptr || page != g.root) return;
    MoveWindow(page, rc.left, rc.top, std::max(1, static_cast<int>(rc.right - rc.left)),
               std::max(1, static_cast<int>(rc.bottom - rc.top)), TRUE);
    LayoutSelf();
}

void PageProcess::FillParent(HWND page) {
    if (page == nullptr || page != g.root) return;
    HWND parent = GetParent(page);
    if (parent == nullptr) return;
    RECT rc{};
    if (!GetClientRect(parent, &rc)) return;
    Layout(page, rc);
}

void PageProcess::Show(HWND page, bool visible) {
    if (page == nullptr || page != g.root) return;
    ShowWindow(page, visible ? SW_SHOW : SW_HIDE);
    if (visible) RefreshProcList(false);
}

void PageProcess::Refresh(HWND page, bool announce) {
    if (page == nullptr || page != g.root) return;
    RefreshProcList(announce);
}

void PageProcess::ApplyLabels(HWND page) {
    if (g.root == nullptr) return;
    if (page != nullptr && page != g.root) return;
    for (const ButtonSpec& b : kButtons) {
        HWND btn = GetDlgItem(g.root, b.id);
        if (btn == nullptr) continue;
        SetWindowTextW(btn, ToWide(LabelOf(b.id)).c_str());
    }
    g.placeholder = Tr("（没有运行中的支持游戏）", "(no supported games running)");
    RecalcButtonWidths();
    RefreshProcList(false);   // 刷新会重建行文案（含优先级/内存标签）
    LayoutSelf();
    InvalidateRect(g.root, nullptr, TRUE);
}

void PageProcess::Destroy(HWND page) {
    if (page == nullptr) return;
    if (page != g.root) {
        DestroyWindow(page);
        return;
    }
    if (IsWindow(page)) DestroyWindow(page);
    g = Ctx{};
}

bool PageProcess::IsPageWindow(HWND h) {
    if (h == nullptr) return false;
    wchar_t cls[64] = {};
    if (GetClassNameW(h, cls, 64) <= 0) return false;
    return lstrcmpW(cls, kClassName) == 0;
}

uint32_t PageProcess::SelectedPid(HWND page) {
    if (page == nullptr || page != g.root) return 0;
    return SelectedPidInternal(GetDlgItem(page, kList));
}

// -----------------------------------------------------------------------------
// 自检
// -----------------------------------------------------------------------------
std::string PageProcess::SelfCheck(HWND page) {
    std::string out = "PageProcess self-check\n";
    int failures = 0;

    bool idsUnique = true;
    {
        const int ids[1 + kBtnCount] = {kList, kButtons[0].id, kButtons[1].id, kButtons[2].id};
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
        for (int i = 0; i < 3; ++i)
            rects.push_back({std::string("metric") + std::to_string(i), p.metric[i]});
        rects.push_back({"listCard", p.listCard});
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
        int liveOverlaps = 0;
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
