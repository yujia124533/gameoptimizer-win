// =============================================================================
// GameOptimizer v1.1.0 — 页面组 A：总览页（含优化历史 / 一键回滚）实现
// -----------------------------------------------------------------------------
// 【通知路径】页容器（宿主创建的 STATIC，PageProc 子类化保持不动）
//       └── 页内面板（本文件注册的窗口类 gopt_ui_page_dash，自绘整页背景/卡片）
//             ├── BS_OWNERDRAW 按钮 → WM_DRAWITEM 到面板（t1 令牌绘制 + 悬停态）
//             ├── LBS_OWNERDRAWFIXED 历史列表 → WM_DRAWITEM 到面板（UiDrawListRow）
//             └── 原生复选框 / EDIT 等 → WM_COMMAND / WM_CTLCOLOR* 到面板
//   面板就地分发命令（DashboardPageCommand），并把 WM_COMMAND 再转发给页容器，
//   保持 v1.0.19 的 PageProc → 主窗口 链路可用。=> 按钮不会是死代码，也不依赖
//   宿主是否记得路由新 ID。同一 WM_COMMAND 若经两条路径到达，GetMessageTime 幂等
//   保护保证只执行一次。
//
// 【线程模型（验收项 d）】
//   * 一键性能优化 / 一键回滚都**不在 UI 线程执行**：UI 线程只做「取配置 → 置 busy
//     → 禁用按钮 → 起 std::thread → 返回」，线程体内部 new AppCore（含硬件探测）
//     并在结束后 PostMessage 回面板窗口；UI 线程在消息处理里更新按钮/状态行/历史。
//   * 线程间不共享 AppCore/SecurityRollback 实例（AppCore 的 mutable 错误字段与快照栈
//     都不是线程安全的）：工作线程自建实例，UI 线程只用 hooks.sharedCore 做只读查询。
//   * 跨线程只传「堆分配的消息载荷 + HWND」；投递前 IsWindow() 守卫，页面销毁时
//     PeekMessage 抽干并释放残留载荷（不泄漏）。
//
// 【布局（验收项 c）】全部由 ComputeDashLayout(w,h,PageMetrics) 纯整数算出：
//   三列 34%/30%/36%（硬件 / 实时 / 曲线）+ 底部一行（左动作区 + 右历史卡），
//   间距一律取令牌（8px 栅格：2/4/8/16/24/32/48），控件高度取 UiControlH 令牌。
//   空间不足时把次要控件置为「空矩形」并隐藏（ShowWindow SW_HIDE），永不用重叠换
//   空间；DashLayoutSelfCheck 做同层两两不重叠 + 容器包含自检。
//
// 【红线】仅官方 API：user32/gdi32/advapi32/kernel32；无注入、无 Hook、无第三方库。
// =============================================================================

#include "gui/page_dashboard.h"

#include <atomic>
#include <cmath>
#include <cstdio>
#include <string>
#include <thread>
#include <vector>

#include "core/AppCore.h"
#include "hal/HAL.h"
#include "i18n.h"
#include "license/License.h"
#include "version.h"

namespace gopt {
namespace ui {
namespace {

// ---------- 页内消息 / 计时器（投递给本页面板，宿主无需认识） ----------
constexpr UINT kMsgFlow      = WM_APP + 21;  // 后台优化流程事件（AppCore::FlowEvent）
constexpr UINT kMsgBoostDone = WM_APP + 22;  // 一键优化结束（载荷 = 结果文本）
constexpr UINT kMsgRollback  = WM_APP + 23;  // 一键回滚结束（载荷 = 结果文本）
constexpr UINT_PTR kTimerLive = 2001;        // 1 秒：实时采样 + 自愈重排 + 历史轮询

constexpr int kHistMax = 40;                 // 历史列表最多展示条数
constexpr int kHistRefreshTicks = 5;         // 每 5 秒只读刷新一次历史（空闲时）

const wchar_t kPanelClass[] = L"gopt_ui_page_dash";

// ---------- 跨线程载荷（谁投递谁 new，UI 线程 delete） ----------
struct FlowPayload {
    AppCore::FlowEvent e;
};
struct TextPayload {
    std::string text;
};

// ---------- 页内状态（单窗口单实例） ----------
struct FlowRow {
    std::string label;
    bool ok = false;
    bool fail = false;
    int elapsedMs = 0;
};

struct DashState {
    HWND container = nullptr;
    HWND panel = nullptr;
    HWND btnBoost = nullptr;
    HWND chkAutostart = nullptr;
    HWND btnRollback = nullptr;
    HWND btnAbout = nullptr;
    HWND btnHistRefresh = nullptr;
    HWND lstHist = nullptr;
    PageHostHooks hooks{};
    AppCore* ownCore = nullptr;   // 宿主未提供 sharedCore 时的自建只读实例（懒加载）
    DashLayout L{};
    PageMetrics m{};
    UIPaintBuffer buf{};
    HBRUSH brSurface = nullptr;

    // 硬件信息（创建/刷新时取一次，避免每帧探测）
    std::string hwCpu, hwCpu2, hwGpu, hwRam;

    // 实时采样
    ULARGE_INTEGER idlePrev{}, kernPrev{}, userPrev{};
    bool cpuPrevValid = false;
    int cpuPct = -1, ramPct = -1;
    float cpuHist[48] = {}, ramHist[48] = {};
    int histPos = 0, histCount = 0;

    // 优化历史（只读查询结果）
    std::vector<SavepointInfo> hist;
    std::string histError;
    int histSel = -1;

    // 一键优化流程（只用于页内进度显示）
    bool flowActive = false;
    double flowProgress = 0.0;
    int flowStep = 0, flowTotal = 0;
    std::string flowLabel;
    bool flowFail = false;
    std::vector<FlowRow> flowRows;

    // 页内状态行（可见反馈）
    std::string status;
    UiTone statusTone = UiTone::Accent;

    bool busy = false;            // 后台任务进行中（按钮禁用 + 历史轮询暂停）
    int rollbackBefore = 0;       // 回滚前的历史条数（判定回滚是否真的生效）
    int ticks = 0;
    bool created = false;
    std::string lastCheckDetail;
} g_d;

// 同一 WM_COMMAND 经「面板就地分发 + 转发宿主」两条路径到达时的幂等守卫。
// 判据是确定性的、不依赖计时：面板在 WM_COMMAND 里先清守卫 → 就地分发 → 置守卫(本条 id/code)
// → 再转发给页容器（PageProc → 主窗口）。宿主要是在同一个消息里又调了一次
// DashboardPageCommand（同一 id/code），就会被守卫拦下并消费掉；下一次真实点击会先清守卫，
// 因此不会被误吞。
struct RouteGuard {
    bool valid = false;
    int id = -1;
    int code = 0;
} g_routeGuard;

// 自绘按钮悬停登记表（页面组共用；每个窗口 1 项，销毁时自动释放）
struct HotEntry {
    HWND hwnd = nullptr;
    WNDPROC old = nullptr;
    bool hot = false;
    bool primary = false;
};
constexpr int kHotMax = 32;
HotEntry g_hot[kHotMax];

HotEntry* FindHot(HWND h) {
    for (int i = 0; i < kHotMax; ++i)
        if (g_hot[i].hwnd == h && h != nullptr) return &g_hot[i];
    return nullptr;
}
HotEntry* AllocHot(HWND h) {
    for (int i = 0; i < kHotMax; ++i)
        if (g_hot[i].hwnd == nullptr) { g_hot[i].hwnd = h; return &g_hot[i]; }
    return nullptr;
}

// 只补「悬停」态：WM_MOUSEMOVE 首次进入 → 登记 hot + TrackMouseEvent(TME_LEAVE)，
// WM_MOUSELEAVE → 清 hot。其余消息原样链回原窗口过程（不改变按钮语义）。
LRESULT CALLBACK HoverSubProc(HWND h, UINT m, WPARAM w, LPARAM l) {
    HotEntry* e = FindHot(h);
    WNDPROC old = (e != nullptr) ? e->old : nullptr;
    if (m == WM_MOUSEMOVE) {
        if (e != nullptr && !e->hot) {
            e->hot = true;
            InvalidateRect(h, nullptr, FALSE);
            TRACKMOUSEEVENT tme{};
            tme.cbSize = sizeof(tme);
            tme.dwFlags = TME_LEAVE;
            tme.hwndTrack = h;
            TrackMouseEvent(&tme);
        }
    } else if (m == WM_MOUSELEAVE) {
        if (e != nullptr && e->hot) {
            e->hot = false;
            InvalidateRect(h, nullptr, FALSE);
        }
    }
    const LRESULT r = (old != nullptr) ? CallWindowProcW(old, h, m, w, l) : DefWindowProcW(h, m, w, l);
    if (m == WM_NCDESTROY && e != nullptr) {  // 窗口已销毁：释放登记项
        e->hwnd = nullptr;
        e->old = nullptr;
        e->hot = false;
        e->primary = false;
    }
    return r;
}

// ---------- 反馈 ----------
void Log(const std::string& s) {
    if (g_d.hooks.AppendLog != nullptr) g_d.hooks.AppendLog(s.c_str());
}
void SetBanner(const std::string& text, UiTone tone) {
    g_d.status = text;
    g_d.statusTone = tone;
    if (g_d.hooks.SetStatus != nullptr) g_d.hooks.SetStatus(text.c_str());
    if (g_d.panel != nullptr) {
        if (PageRectEmpty(g_d.L.statusLine)) InvalidateRect(g_d.panel, nullptr, FALSE);
        else InvalidateRect(g_d.panel, &g_d.L.statusLine, FALSE);
    }
}

// ---------- 历史/摘要文案（双语；核心返回的中文概要用 8 款游戏的 EN 名做前缀替换） ----------
bool IsAsciiText(const std::string& s) {
    if (s.empty()) return false;
    for (size_t i = 0; i < s.size(); ++i) {
        const unsigned char c = static_cast<unsigned char>(s[i]);
        if (c < 0x20 || c > 0x7E) return false;
    }
    return true;
}
const char* GameNameEn(size_t idx) {
    static const char* kEn[8] = {"Delta Force", "League of Legends", "Counter-Strike 2", "PUBG",
                                 "VALORANT", "Apex Legends", "Dota 2", "Overwatch 2"};
    return idx < 8 ? kEn[idx] : nullptr;
}
// SavepointInfo::processSummary 由核心生成（中文游戏名）；EN 模式下把 8 款已知游戏的
// 中文前缀换成英文，其余原样保留 —— 页面不编造翻译。
std::string LocalizedSummary(const SavepointInfo& sp) {
    std::string s = sp.processSummary.empty() ? sp.gameName : sp.processSummary;
    if (CurrentLang() == Lang::En) {
        static const GameId kIds[8] = {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                                       GameId::PUBG, GameId::Valorant, GameId::Apex, GameId::Dota2,
                                       GameId::Overwatch2};
        for (size_t i = 0; i < 8; ++i) {
            const std::string zh = GameIdToString(kIds[i]);
            const char* en = GameNameEn(i);
            if (en == nullptr || zh.empty() || s.size() < zh.size()) continue;
            if (s.compare(0, zh.size(), zh) == 0) {
                s = std::string(en) + s.substr(zh.size());
                break;
            }
        }
    }
    if (s.empty()) s = T("（未记录进程）", "(no process recorded)");
    return s;
}
std::string HistRowText(size_t i) {
    if (i >= g_d.hist.size()) return std::string();
    const SavepointInfo& sp = g_d.hist[i];
    std::string s;
    if (sp.isLatest) s += "\xE2\x98\x85 ";  // ★ 最新一条（回滚目标）
    s += std::to_string(sp.index) + ". " + LocalizedSummary(sp) + " · " + std::to_string(sp.entryCount)
       + T(" 项", " items");
    return s;
}
// timeText 无法可靠换算时核心给的是中文说明文本；EN 下换成英文，可读时间戳原样显示。
std::string HistRightText(size_t i) {
    if (i >= g_d.hist.size()) return std::string();
    const std::string& t = g_d.hist[i].timeText;
    if (IsAsciiText(t)) return t;
    return T("较早一次运行（时间未知）", "earlier run (time unknown)");
}

// ---------- 只读核心 ----------
AppCore* CoreForQuery() {
    if (g_d.hooks.sharedCore != nullptr) return g_d.hooks.sharedCore;
    if (g_d.ownCore == nullptr) g_d.ownCore = new AppCore();  // 宿主未给：懒加载自备只读实例
    return g_d.ownCore;
}

// ---------- 开机自启动（HKCU Run 键；仅官方注册表 API） ----------
const wchar_t kRunKey[] = L"Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const wchar_t kRunValue[] = L"GameOptimizer";
bool AutoStartExists() {
    DWORD cb = 0;
    return RegGetValueW(HKEY_CURRENT_USER, kRunKey, kRunValue, RRF_RT_REG_SZ, nullptr, nullptr, &cb) ==
           ERROR_SUCCESS;
}
bool AutoStartSet(bool on) {
    HKEY k = nullptr;
    if (RegCreateKeyExW(HKEY_CURRENT_USER, kRunKey, 0, nullptr, 0, KEY_SET_VALUE, nullptr, &k,
                        nullptr) != ERROR_SUCCESS)
        return false;
    bool ok = false;
    if (on) {
        wchar_t exe[MAX_PATH] = {};
        if (GetModuleFileNameW(nullptr, exe, MAX_PATH) > 0) {
            const std::wstring cmd = L"\"" + std::wstring(exe) + L"\"";
            ok = RegSetValueExW(k, kRunValue, 0, REG_SZ, reinterpret_cast<const BYTE*>(cmd.c_str()),
                                static_cast<DWORD>((cmd.size() + 1) * sizeof(wchar_t))) == ERROR_SUCCESS;
        }
    } else {
        const LONG r = RegDeleteValueW(k, kRunValue);
        ok = (r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND);
    }
    RegCloseKey(k);
    return ok;
}

// ---------- 实时采样（GetSystemTimes / GlobalMemoryStatusEx，官方 API） ----------
void SampleLive() {
    FILETIME idleFt{}, kernFt{}, userFt{};
    if (GetSystemTimes(&idleFt, &kernFt, &userFt)) {
        ULARGE_INTEGER idle{}, kern{}, user{};
        idle.LowPart = idleFt.dwLowDateTime;  idle.HighPart = idleFt.dwHighDateTime;
        kern.LowPart = kernFt.dwLowDateTime;  kern.HighPart = kernFt.dwHighDateTime;
        user.LowPart = userFt.dwLowDateTime;  user.HighPart = userFt.dwHighDateTime;
        if (g_d.cpuPrevValid) {
            const ULONGLONG dI = idle.QuadPart - g_d.idlePrev.QuadPart;
            const ULONGLONG dK = kern.QuadPart - g_d.kernPrev.QuadPart;
            const ULONGLONG dU = user.QuadPart - g_d.userPrev.QuadPart;
            const ULONGLONG total = dK + dU;
            if (total > 0) {
                const int pct = static_cast<int>((total - dI) * 100 / total);
                g_d.cpuPct = PageClampI(pct, 0, 100);
            }
        }
        g_d.idlePrev = idle;
        g_d.kernPrev = kern;
        g_d.userPrev = user;
        g_d.cpuPrevValid = true;
    }
    MEMORYSTATUSEX ms{};
    ms.dwLength = sizeof(ms);
    if (GlobalMemoryStatusEx(&ms) && ms.ullTotalPhys > 0) {
        const int pct = static_cast<int>((ms.ullTotalPhys - ms.ullAvailPhys) * 100 / ms.ullTotalPhys);
        g_d.ramPct = PageClampI(pct, 0, 100);
    }
    // 48 秒环形缓冲（与 t1 曲线卡配套）
    g_d.cpuHist[g_d.histPos] = static_cast<float>(g_d.cpuPct < 0 ? 0 : g_d.cpuPct);
    g_d.ramHist[g_d.histPos] = static_cast<float>(g_d.ramPct < 0 ? 0 : g_d.ramPct);
    g_d.histPos = (g_d.histPos + 1) % 48;
    if (g_d.histCount < 48) ++g_d.histCount;
}

// ---------- 硬件信息卡文本 ----------
void RefreshHardwareText() {
    AppCore* core = CoreForQuery();
    if (core == nullptr) return;
    const HardwareProfile p = core->Profile();
    g_d.hwCpu = p.cpuModel.empty() ? T("未知", "unknown") : p.cpuModel;
    g_d.hwCpu2 = std::to_string(p.physicalCores) + T(" 物理核 / ", " cores / ")
               + std::to_string(p.logicalCores) + T(" 逻辑", " threads")
               + (p.cpuBaseFreqMHz > 0 ? (" @ " + std::to_string(p.cpuBaseFreqMHz) + " MHz") : "");
    g_d.hwGpu = (p.gpuVendor + " " + p.gpuModel);
    if (p.gpuModel.empty()) g_d.hwGpu = T("未知", "unknown");
    if (p.vramMB > 0) g_d.hwGpu += " (" + std::to_string(p.vramMB / 1024) + " GB)";
    g_d.hwRam = std::to_string(p.systemRamMB / 1024) + " GB" + T("（可用 ", " (free ")
              + std::to_string(p.availableRamMB / 1024) + " GB)";
    const int ramPct = p.systemRamMB > 0
                           ? static_cast<int>((p.systemRamMB - p.availableRamMB) * 100 / p.systemRamMB)
                           : 0;
    if (g_d.ramPct < 0) g_d.ramPct = PageClampI(ramPct, 0, 100);
}

// ---------- 优化历史（只读查询；核心保证不建目录、不写文件） ----------
void RefreshHistory() {
    AppCore* core = CoreForQuery();
    if (core == nullptr) {
        g_d.hist.clear();
        g_d.histError = T("核心不可用", "core unavailable");
    } else {
        g_d.hist = core->RecentSavepoints(static_cast<size_t>(kHistMax));  // 最新在前
        g_d.histError = core->SavepointsError();
    }
    if (g_d.lstHist != nullptr) {
        SendMessageW(g_d.lstHist, WM_SETREDRAW, FALSE, 0);
        SendMessageW(g_d.lstHist, LB_RESETCONTENT, 0, 0);
        if (g_d.hist.empty()) {
            const std::wstring emptyRow =
                PageToWide(T("（暂无快照：尚未优化过，或快照文件异常）",
                             "(no snapshots: never optimized, or savepoint file abnormal)"));
            SendMessageW(g_d.lstHist, LB_ADDSTRING, 0, reinterpret_cast<LPARAM>(emptyRow.c_str()));
        } else {
            for (size_t i = 0; i < g_d.hist.size(); ++i) {
                const std::wstring row = PageToWide(HistRowText(i));
                const LRESULT idx = SendMessageW(g_d.lstHist, LB_ADDSTRING, 0,
                                                 reinterpret_cast<LPARAM>(row.c_str()));
                if (idx >= 0) SendMessageW(g_d.lstHist, LB_SETITEMDATA, idx, 1);
            }
            SendMessageW(g_d.lstHist, LB_SETCURSEL, 0, 0);
            g_d.histSel = 0;
        }
        SendMessageW(g_d.lstHist, WM_SETREDRAW, TRUE, 0);
        InvalidateRect(g_d.lstHist, nullptr, TRUE);
    }
    if (g_d.btnRollback != nullptr)
        EnableWindow(g_d.btnRollback, !g_d.hist.empty() && !g_d.busy);
    if (g_d.panel != nullptr) InvalidateRect(g_d.panel, nullptr, FALSE);
}

// ---------- 历史条数/降级说明（历史卡底部一行） ----------
std::string HistStatusText() {
    if (!g_d.histError.empty()) return g_d.histError;  // 核心给的降级/失败文案（含路径与坏行数）
    if (g_d.hist.empty()) {
        return std::string(T("暂无快照：尚未优化过（查询不建目录、不写文件）",
                             "No snapshots: never optimized (query creates nothing)"));
    }
    return std::string(T("共 ", "total ")) + std::to_string(g_d.hist.size()) + T(" 条（最新在前，★=回滚目标）",
                                                                                " entries (newest first, ★=rollback target)")
         + " · " + HistRightText(0);
}
UiTone HistStatusTone() {
    if (!g_d.histError.empty()) return UiTone::Warning;
    return g_d.hist.empty() ? UiTone::Neutral : UiTone::Accent;
}

// ---------- 一键性能优化（工作线程 + PostMessage 回 UI） ----------
void StartBoost() {
    if (g_d.busy) {
        SetBanner(T("上一次操作仍在进行中，请稍候…", "Previous operation still running…"), UiTone::Warning);
        return;
    }
    AppConfig cfg;
    cfg.allowPowerSchemeSwitch = PageSharedFlags().allowPowerSchemeSwitch;
    cfg.autoRollbackOnUnstable = true;
    const HWND target = g_d.panel;

    g_d.busy = true;
    g_d.flowActive = true;
    g_d.flowProgress = 0.0;
    g_d.flowStep = 0;
    g_d.flowTotal = 0;
    g_d.flowFail = false;
    g_d.flowRows.clear();
    g_d.flowLabel = T("已提交后台线程…", "dispatched to worker thread…");
    EnableWindow(g_d.btnBoost, FALSE);
    EnableWindow(g_d.btnRollback, FALSE);
    SetBanner(std::string(T("一键性能优化：后台执行中（界面不阻塞）",
                            "One-click boost running in background (UI stays responsive)")),
              UiTone::Info);
    Log(std::string(T("== 一键性能优化 ==\n", "== One-click boost ==\n")));

    std::thread([cfg, target]() {
        AppCore* core = new AppCore(cfg);
        const std::string result = core->OptimizeAll(
            [target](const AppCore::FlowEvent& e) {
                if (target != nullptr && IsWindow(target)) {
                    PostMessageW(target, kMsgFlow, 0, reinterpret_cast<LPARAM>(new FlowPayload{e}));
                }
            },
            0);
        delete core;
        if (target != nullptr && IsWindow(target)) {
            PostMessageW(target, kMsgBoostDone, 0, reinterpret_cast<LPARAM>(new TextPayload{result}));
        }
    }).detach();
}

// ---------- 一键回滚最近一次（工作线程 + PostMessage 回 UI） ----------
void StartRollback() {
    if (g_d.busy) {
        SetBanner(T("上一次操作仍在进行中，请稍候…", "Previous operation still running…"), UiTone::Warning);
        return;
    }
    if (g_d.hist.empty()) {
        SetBanner(T("没有可回滚的快照（历史为空）", "Nothing to roll back (history empty)"), UiTone::Warning);
        Log(std::string(T("回滚被忽略：优化历史为空。\n\n", "Rollback skipped: history is empty.\n\n")));
        return;
    }
    const HWND target = g_d.panel;
    g_d.busy = true;
    g_d.rollbackBefore = static_cast<int>(g_d.hist.size());
    EnableWindow(g_d.btnBoost, FALSE);
    EnableWindow(g_d.btnRollback, FALSE);
    SetBanner(T("正在后台回滚最近一次优化（界面不阻塞）…",
                "Rolling back the last optimization in background (UI stays responsive)…"),
              UiTone::Info);
    Log(std::string(T("== 回滚最近一次优化 ==\n", "== Rollback last optimization ==\n")));

    std::thread([target]() {
        AppCore* core = new AppCore();
        const std::string result = core->Rollback();
        delete core;
        if (target != nullptr && IsWindow(target)) {
            PostMessageW(target, kMsgRollback, 0, reinterpret_cast<LPARAM>(new TextPayload{result}));
        }
    }).detach();
}

// ---------- 诊断 / 关于（只读信息汇总；MessageBoxW 官方 API） ----------
void ShowAbout() {
    std::string s;
    s += std::string("GameOptimizer v") + GOPT_VERSION_STR + "\n";
    AppCore* core = CoreForQuery();
    if (core != nullptr) {
        const HardwareProfile p = core->Profile();
        s += std::string(T("CPU：", "CPU: ")) + p.cpuModel + "\n";
        s += std::string(T("物理核：", "Physical cores: ")) + std::to_string(p.physicalCores)
           + T("  逻辑核：", "  Logical cores: ") + std::to_string(p.logicalCores) + "\n";
        s += std::string(T("GPU：", "GPU: ")) + p.gpuVendor + " " + p.gpuModel + "（"
           + std::to_string(p.vramMB / 1024) + " GB / " + p.gpuDriverVersion + "）\n";
        s += std::string(T("内存：", "RAM: ")) + std::to_string(p.systemRamMB / 1024) + " GB"
           + T("（可用 ", " (free ") + std::to_string(p.availableRamMB / 1024) + " GB）\n";
        const LicenseInfo li = core->License();
        s += std::string(T("授权：", "License: "))
           + (li.edition.empty() ? std::string(T("免费版", "Free")) : li.edition)
           + T("（全部功能免费）", " (all features free)") + "\n";
    }
    s += std::string(T("提权状态：", "Elevation: "))
       + (HAL::IsElevated() ? T("已提权（管理员）", "elevated (admin)")
                            : T("未提权（部分功能需管理员）", "not elevated (some need admin)"))
       + "\n";
    GUID scheme{};
    std::string power = T("未知", "unknown");
    if (HAL::QueryActivePowerScheme(&scheme)) power = HAL::PowerSchemeName(scheme);
    s += std::string(T("当前电源方案：", "Active power scheme: ")) + power + "\n";
    s += std::string(T("快照文件：", "Savepoint file: ")) + AppCore::SavepointsFilePath() + "\n";
    s += std::string(T("可回滚快照：", "Rollback points: ")) + std::to_string(g_d.hist.size()) + "\n";
    s += std::string(T("安全声明：无注入 · 无内核 Hook · 优先级上限 HIGH · 每次优化自动快照可回滚",
                       "Safety: no injection, no kernel hooks, priority capped at HIGH, auto snapshot + rollback"));
    MessageBoxW(g_d.panel, PageToWide(s).c_str(),
                PageToWide(T("诊断 / 关于 - GameOptimizer", "Diagnostics / About - GameOptimizer")).c_str(),
                MB_OK | MB_ICONINFORMATION);
}

// ---------- 选中行的详情（列表双击 → 只读详情） ----------
void ShowSavepointDetail() {
    const int sel = g_d.lstHist != nullptr
                        ? static_cast<int>(SendMessageW(g_d.lstHist, LB_GETCURSEL, 0, 0))
                        : -1;
    if (sel < 0 || sel >= static_cast<int>(g_d.hist.size())) {
        SetBanner(T("请先在列表中选择一条快照", "Select a snapshot row first"), UiTone::Warning);
        return;
    }
    const SavepointInfo& sp = g_d.hist[static_cast<size_t>(sel)];
    std::string s = std::string("#") + std::to_string(sp.index) + "  " + LocalizedSummary(sp) + "\n";
    s += std::string(T("时间：", "Time: ")) + HistRightText(static_cast<size_t>(sel)) + "\n";
    s += std::string(T("可恢复条目：", "Restorable entries: ")) + std::to_string(sp.entryCount) + "\n";
    s += (sp.isLatest ? T("（这是回滚目标：一键回滚作用于它）", "(rollback target: one-click rollback uses it)")
                      : T("（历史条目，仅供查看）", "(history entry, read-only)")) + std::string("\n\n");
    for (size_t i = 0; i < sp.entries.size(); ++i) s += "  - " + sp.entries[i] + "\n";
    MessageBoxW(g_d.panel, PageToWide(s).c_str(),
                PageToWide(T("快照详情（只读）", "Snapshot detail (read-only)")).c_str(),
                MB_OK | MB_ICONINFORMATION);
}

}  // namespace

// =============================================================================
// 导出：UTF-8 工具 / 悬停支持 / 共享开关 / 度量令牌
// =============================================================================
std::wstring PageToWide(const std::string& utf8) {
    if (utf8.empty()) return std::wstring();
    const int need = MultiByteToWideChar(CP_UTF8, 0, utf8.c_str(), -1, nullptr, 0);
    if (need <= 1) return std::wstring();
    std::wstring out(static_cast<size_t>(need), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8.c_str(), -1, &out[0], need);
    out.resize(static_cast<size_t>(need) - 1);  // 去掉结尾 NUL
    return out;
}
std::string PageFromWide(const wchar_t* w) {
    if (w == nullptr || w[0] == L'\0') return std::string();
    const int need = WideCharToMultiByte(CP_UTF8, 0, w, -1, nullptr, 0, nullptr, nullptr);
    if (need <= 1) return std::string();
    std::string out(static_cast<size_t>(need), '\0');
    WideCharToMultiByte(CP_UTF8, 0, w, -1, &out[0], need, nullptr, nullptr);
    out.resize(static_cast<size_t>(need) - 1);
    return out;
}
std::string PageGetTextUtf8(HWND h) {
    if (h == nullptr) return std::string();
    const int len = GetWindowTextLengthW(h);
    if (len <= 0) return std::string();
    std::vector<wchar_t> buf(static_cast<size_t>(len) + 1, L'\0');
    GetWindowTextW(h, &buf[0], len + 1);
    return PageFromWide(&buf[0]);
}
void PageSetTextUtf8(HWND h, const std::string& s) {
    if (h == nullptr) return;
    SetWindowTextW(h, PageToWide(s).c_str());
}
SharedUiFlags& PageSharedFlags() {
    static SharedUiFlags f;
    return f;
}
PageMetrics MakePageMetrics() {
    PageMetrics m;
    m.dpi = UiDpi();
    for (int i = 0; i < 7; ++i) m.sp[i] = UiSp(static_cast<UiSpace>(i));
    for (int i = 0; i < 4; ++i) m.ctrlH[i] = UiControlHeight(static_cast<UiControlH>(i));
    for (int i = 0; i < 4; ++i) m.radius[i] = UiRadiusPx(static_cast<UiRadius>(i));
    return m;
}
void PageDrawStatusLine(HDC dc, const RECT& rc, UiTone tone, const std::string& textUtf8,
                        const PageMetrics& m, UiFontRole role) {
    if (dc == nullptr || PageRectEmpty(rc)) return;
    const int dot = PageMaxI(6, m.space(2));
    const int cy = rc.top + (rc.bottom - rc.top) / 2;
    HBRUSH b = CreateSolidBrush(UiToneFg(tone));
    if (b != nullptr) {
        HGDIOBJ ob = SelectObject(dc, b);
        HGDIOBJ op = SelectObject(dc, GetStockObject(NULL_PEN));
        Ellipse(dc, rc.left, cy - dot / 2, rc.left + dot, cy + dot / 2);
        if (op != nullptr) SelectObject(dc, op);
        SelectObject(dc, ob);
        DeleteObject(b);
    }
    RECT tr = rc;
    tr.left = rc.left + dot + m.space(2);
    UiDrawTextClamped(dc, textUtf8, tr, DT_LEFT | DT_VCENTER, UiToneFg(tone), role);
}
bool PageEnableButtonHover(HWND button, bool primary) {
    if (button == nullptr) return false;
    HotEntry* e = FindHot(button);
    if (e == nullptr) e = AllocHot(button);
    if (e == nullptr) return false;
    e->primary = primary;
    if (e->old == nullptr) {
        const LONG_PTR prev =
            SetWindowLongPtrW(button, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(HoverSubProc));
        if (prev == 0) return false;
        e->old = reinterpret_cast<WNDPROC>(prev);
    }
    return true;
}
void PageDisableAllButtonHover() {
    for (int i = 0; i < kHotMax; ++i) {
        if (g_hot[i].hwnd != nullptr && !IsWindow(g_hot[i].hwnd)) {
            g_hot[i].hwnd = nullptr;
            g_hot[i].old = nullptr;
            g_hot[i].hot = false;
            g_hot[i].primary = false;
        }
    }
}
bool PageIsButtonHot(HWND button) {
    const HotEntry* e = FindHot(button);
    return e != nullptr && e->hot;
}
bool PageIsButtonPrimary(HWND button) {
    const HotEntry* e = FindHot(button);
    return e != nullptr && e->primary;
}
UiButtonState PageButtonStateFromDrawItem(const DRAWITEMSTRUCT* di) {
    UiButtonState st = UiButtonStateFromDrawItem(di);
    if (di != nullptr && st == UiButtonState::Normal && PageIsButtonHot(di->hwndItem))
        st = UiButtonState::Hot;
    return st;
}

// =============================================================================
// 布局：纯整数（可脱离窗口/DC 单测）
// =============================================================================
DashLayout ComputeDashLayout(int w, int h, const PageMetrics& m) {
    DashLayout L;
    PageSetRect(&L.panel, 0, 0, PageMaxI(0, w), PageMaxI(0, h));
    if (w <= 0 || h <= 0) {
        L.note = "empty panel";
        return L;
    }
    const int pad = m.px(16), gap = m.px(16), in8 = m.px(8), in4 = m.px(4);
    const int innerW = w - 2 * pad, innerH = h - 2 * pad;
    L.valid = true;  // 先假设「全部控件都能放下」，下面的降级分支按需置 false 并写明原因
    if (innerW < m.px(240) || innerH < m.px(120)) {
        L.note = "content area too small";
        return L;
    }

    // ---- 紧凑模式判定：内容区小于建议最小面积（640x300 逻辑）时，曲线卡让位，
    //      把底部整行切成「左动作区 / 右历史卡」两列，保证优化历史与一键回滚始终可达 ----
    const bool compact = (w < m.px(640) || h < m.px(300));
    L.compact = compact;

    // ---- 三列宽（34% / 30% / 36%，带最小宽；不足时按最小宽重排并标记降级） ----
    const int usable = innerW - 2 * gap;
    const int min1 = m.px(180), min2 = m.px(168), min3 = m.px(200);
    int c1 = 0, c2 = 0, c3 = 0;
    if (compact) {
        c1 = usable * 46 / 100;  // 硬件卡 / 左动作区
        c2 = usable - c1;        // 实时卡 / 历史卡
        c3 = 0;                  // 曲线卡让位
        L.valid = false;
        L.note = "compact: curve hidden, history + actions kept";
    } else {
        c1 = usable * 34 / 100;
        c2 = usable * 30 / 100;
        c3 = usable - c1 - c2;
        if (c1 < min1 || c2 < min2 || c3 < min3) {
            const int maxC1 = PageMaxI(m.px(120), usable - min2 - m.px(120));
            c1 = PageMinI(PageMaxI(min1, c1), maxC1);
            const int maxC2 = PageMaxI(m.px(120), usable - c1 - m.px(120));
            c2 = PageMinI(PageMaxI(min2, c2), maxC2);
            c3 = PageMaxI(0, usable - c1 - c2);
            L.valid = false;
            L.note = (c3 < min3) ? "narrow: history column compressed" : "narrow: columns re-flowed";
        }
    }
    // ---- 两行高（上排卡片 / 下排动作+历史） ----
    const int minA = compact ? m.px(96) : m.px(132);
    const int minB = compact ? m.px(56) : m.px(96);
    int hA = innerH * (compact ? 50 : 46) / 100;
    if (hA < minA) hA = minA;
    if (innerH - hA - gap < minB) hA = innerH - gap - minB;
    if (hA < m.px(80)) {
        hA = PageClampI(innerH * 50 / 100, m.px(56), minA);
        L.valid = false;
        if (L.note.empty() || !compact) L.note = "short: card heights compressed";
    }
    int hB = innerH - hA - gap;
    if (hB < 0) hB = 0;

    const int x0 = pad;
    const int top = pad;
    const int yB = pad + hA + gap;

    // ---- 顶部三卡 + 底部动作区/历史卡 ----
    PageSetRect(&L.cardHw, x0, top, x0 + c1, top + hA);
    PageSetRect(&L.cardLive, x0 + c1 + gap, top, x0 + c1 + gap + c2, top + hA);
    if (c3 > 0) PageSetRect(&L.cardCurve, x0 + c1 + gap + c2 + gap, top, w - pad, top + hA);
    if (c3 <= 0) {
        // 紧凑模式：底行复用上排两列（左=动作区，右=历史卡），曲线卡整块退场
        PageSetRect(&L.areaAct, x0, yB, x0 + c1, yB + hB);
        PageSetRect(&L.cardHist, x0 + c1 + gap, yB, w - pad, yB + hB);
    } else {
        PageSetRect(&L.areaAct, x0, yB, x0 + c1 + gap + c2, yB + hB);
        PageSetRect(&L.cardHist, x0 + c1 + gap + c2 + gap, yB, w - pad, yB + hB);
    }

    // ---- 卡1：硬件信息（标题 + 4 行） ----
    if (!PageRectEmpty(L.cardHw)) {
        PageSetRect(&L.hwTitle, L.cardHw.left + in8, L.cardHw.top + in8, L.cardHw.right - in8,
                    L.cardHw.top + in8 + m.px(20));
        const int rowH = m.px(18);
        for (int i = 0; i < 4; ++i) {
            const int t = L.hwTitle.bottom + in4 + i * rowH;
            if (t + rowH > L.cardHw.bottom - in4) {
                PageClearRect(&L.hwLine[i]);
                continue;
            }
            PageSetRect(&L.hwLine[i], L.cardHw.left + in8, t, L.cardHw.right - in8, t + rowH);
        }
    }

    // ---- 卡2：实时（标题 + CPU 大号数字/进度 + 内存大号数字/进度 + 页脚/状态沉底） ----
    if (!PageRectEmpty(L.cardLive)) {
        PageSetRect(&L.liveTitle, L.cardLive.left + in8, L.cardLive.top + in8, L.cardLive.right - in8,
                    L.cardLive.top + in8 + m.px(20));
        const int metricW = m.px(80);
        const int metricH = compact ? m.px(26) : m.px(32);  // 紧凑：让出页脚（状态行沉底位）
        const int rowY = L.liveTitle.bottom + in4;
        PageSetRect(&L.metricCpu, L.cardLive.left + in8, rowY, L.cardLive.left + in8 + metricW,
                    rowY + metricH);
        PageSetRect(&L.barCpu, L.metricCpu.right + in8, rowY + m.px(11), L.cardLive.right - in8,
                    rowY + m.px(21));
        const bool keepRam = (L.cardLive.bottom - in8 - rowY) >= m.px(72);
        if (keepRam) {
            const int rowY2 = rowY + m.px(36);
            PageSetRect(&L.metricRam, L.cardLive.left + in8, rowY2, L.metricCpu.right, rowY2 + metricH);
            PageSetRect(&L.barRam, L.metricRam.right + in8, rowY2 + m.px(11), L.cardLive.right - in8,
                        rowY2 + m.px(21));
            PageSetRect(&L.liveFoot, L.cardLive.left + in8, rowY2 + metricH + in4, L.cardLive.right - in8,
                        L.cardLive.bottom - in8);
        } else {
            PageSetRect(&L.liveFoot, L.cardLive.left + in8, L.metricCpu.bottom + in4,
                        L.cardLive.right - in8, L.cardLive.bottom - in8);
            L.valid = false;
            if (L.note.empty()) L.note = "short: live RAM row hidden";
        }
        if (L.liveFoot.bottom - L.liveFoot.top < m.px(16)) PageClearRect(&L.liveFoot);
    }

    // ---- 卡3：曲线（标题 + 图例 + 曲线区） ----
    if (!PageRectEmpty(L.cardCurve)) {
        PageSetRect(&L.curveTitle, L.cardCurve.left + in8, L.cardCurve.top + in8,
                    L.cardCurve.right - in8, L.cardCurve.top + in8 + m.px(20));
        PageSetRect(&L.curveLegend, L.cardCurve.left + in8, L.curveTitle.bottom + in4,
                    L.cardCurve.right - in8, L.curveTitle.bottom + in4 + m.px(16));
        PageSetRect(&L.curveArea, L.cardCurve.left + in8, L.curveLegend.bottom + in4,
                    L.cardCurve.right - in8, L.cardCurve.bottom - in8);
        if (L.curveArea.bottom - L.curveArea.top < m.px(28)) PageClearRect(&L.curveArea);
    }

    // ---- 左下动作区（一键优化 / 自启动 / 回滚 / 诊断 + 状态行） ----
    if (!PageRectEmpty(L.areaAct)) {
        const int rowH = compact ? m.height(1) : m.height(2);  // 紧凑：32（Lg=40 太占高）
        const int btnW = PageClampI((L.areaAct.right - L.areaAct.left) * (compact ? 58 : 45) / 100,
                                    m.px(120), m.px(240));
        PageSetRect(&L.btnBoost, L.areaAct.left, L.areaAct.top, L.areaAct.left + btnW,
                    L.areaAct.top + rowH);
        const int chkH = m.px(22);
        PageSetRect(&L.chkAutostart, L.btnBoost.right + in4, L.areaAct.top + (rowH - chkH) / 2,
                    L.areaAct.right, L.areaAct.top + (rowH - chkH) / 2 + chkH);
        const bool keepRow2 = hB >= (rowH + in4 + rowH);
        if (keepRow2) {
            const int row2 = L.areaAct.top + rowH + in4;
            PageSetRect(&L.btnRollback, L.areaAct.left, row2, L.areaAct.left + btnW, row2 + rowH);
            PageSetRect(&L.btnAbout, L.btnRollback.right + in4, row2 + (rowH - m.px(26)) / 2,
                        PageMinI(L.btnRollback.right + in4 + m.px(160), L.areaAct.right),
                        row2 + (rowH - m.px(26)) / 2 + m.px(26));
            const bool keepStatus = hB >= (rowH + in4 + rowH + m.px(22) + in4);
            if (keepStatus) {
                PageSetRect(&L.statusLine, L.areaAct.left, L.areaAct.bottom - m.px(22),
                            L.areaAct.right, L.areaAct.bottom);
            } else {
                L.valid = false;
                if (L.note.empty()) L.note = "short: page status line hidden";
            }
        } else {
            L.valid = false;
            if (L.note.empty()) L.note = "short: rollback/about buttons hidden";
        }
    }

    // ---- 右下历史卡（标题 + 刷新按钮 + 列表 + 状态行） ----
    if (!PageRectEmpty(L.cardHist)) {
        const int btnW = m.px(64);
        PageSetRect(&L.histTitle, L.cardHist.left + in8, L.cardHist.top + in8,
                    L.cardHist.right - in8 - btnW - in8, L.cardHist.top + in8 + m.px(20));
        PageSetRect(&L.btnHistRefresh, L.cardHist.right - in8 - btnW, L.cardHist.top + m.px(6),
                    L.cardHist.right - in8, L.cardHist.top + m.px(6) + m.px(24));
        PageSetRect(&L.histStatus, L.cardHist.left + in8, L.cardHist.bottom - in8 - m.px(20),
                    L.cardHist.right - in8, L.cardHist.bottom - in8);
        PageSetRect(&L.lstHist, L.cardHist.left + in8, L.histTitle.bottom + in8,
                    L.cardHist.right - in8, L.histStatus.top - m.px(6));
        if (L.lstHist.bottom - L.lstHist.top < m.px(24)) {  // 状态行让位给列表
            PageClearRect(&L.histStatus);
            PageSetRect(&L.lstHist, L.cardHist.left + in8, L.histTitle.bottom + in8,
                        L.cardHist.right - in8, L.cardHist.bottom - in8);
            L.valid = false;
            if (L.note.empty()) L.note = "short: history status line hidden";
            if (L.lstHist.bottom - L.lstHist.top < m.px(24)) {
                PageClearRect(&L.lstHist);
                if (L.note.empty()) L.note = "short: history list hidden";
            }
        }
    }

    // 归一化：任何「不在所属容器内」的非空矩形一律清空 —— 与 DashLayoutSelfCheck 的
    // 包含性判据同源，保证「容器退场时其内部矩形一并退场」，且自检恒真。
    struct Fit {
        RECT* r;
        const RECT* container;
    };
    const Fit fits[] = {
        {&L.cardHw, &L.panel},        {&L.cardLive, &L.panel},
        {&L.cardCurve, &L.panel},     {&L.areaAct, &L.panel},
        {&L.cardHist, &L.panel},      {&L.hwTitle, &L.cardHw},
        {&L.hwLine[0], &L.cardHw},    {&L.hwLine[1], &L.cardHw},
        {&L.hwLine[2], &L.cardHw},    {&L.hwLine[3], &L.cardHw},
        {&L.liveTitle, &L.cardLive},  {&L.metricCpu, &L.cardLive},
        {&L.barCpu, &L.cardLive},     {&L.metricRam, &L.cardLive},
        {&L.barRam, &L.cardLive},     {&L.liveFoot, &L.cardLive},
        {&L.curveTitle, &L.cardCurve}, {&L.curveLegend, &L.cardCurve},
        {&L.curveArea, &L.cardCurve}, {&L.btnBoost, &L.areaAct},
        {&L.chkAutostart, &L.areaAct}, {&L.btnRollback, &L.areaAct},
        {&L.btnAbout, &L.areaAct},    {&L.statusLine, &L.areaAct},
        {&L.histTitle, &L.cardHist},  {&L.btnHistRefresh, &L.cardHist},
        {&L.lstHist, &L.cardHist},    {&L.histStatus, &L.cardHist},
    };
    for (size_t i = 0; i < sizeof(fits) / sizeof(fits[0]); ++i) {
        if (!PageRectEmpty(*fits[i].r) && !PageRectInside(*fits[i].r, *fits[i].container))
            PageClearRect(fits[i].r);
    }

    if (L.note.empty() && L.valid) L.note = "ok";
    return L;
}

bool DashLayoutSelfCheck(const DashLayout& L, std::string* detail) {
    struct Named {
        const char* name;
        const RECT* rc;
    };
    const Named topCards[] = {{"cardHw", &L.cardHw}, {"cardLive", &L.cardLive}, {"cardCurve", &L.cardCurve}};
    const Named bottom[] = {{"areaAct", &L.areaAct}, {"cardHist", &L.cardHist}};
    const Named live[] = {{"metricCpu", &L.metricCpu}, {"barCpu", &L.barCpu},
                          {"metricRam", &L.metricRam}, {"barRam", &L.barRam}, {"liveFoot", &L.liveFoot}};
    const Named acts[] = {{"btnBoost", &L.btnBoost},   {"chkAutostart", &L.chkAutostart},
                          {"btnRollback", &L.btnRollback}, {"btnAbout", &L.btnAbout},
                          {"statusLine", &L.statusLine}};
    const Named hist[] = {{"histTitle", &L.histTitle}, {"btnHistRefresh", &L.btnHistRefresh},
                          {"lstHist", &L.lstHist},     {"histStatus", &L.histStatus}};

    struct Group {
        const char* title;
        const Named* items;
        int count;
        const RECT* container;
    };
    const Group groups[] = {
        {"top-cards", topCards, 3, &L.panel},
        {"bottom", bottom, 2, &L.panel},
        {"live-inner", live, 5, &L.cardLive},
        {"action-area", acts, 5, &L.areaAct},
        {"history-card", hist, 4, &L.cardHist},
    };
    for (size_t g = 0; g < sizeof(groups) / sizeof(groups[0]); ++g) {
        const Group& grp = groups[g];
        for (int i = 0; i < grp.count; ++i) {
            const RECT& a = *grp.items[i].rc;
            if (PageRectEmpty(a)) continue;  // 空矩形 = 已隐藏（降级），不参与重叠判定
            if (!PageRectInside(a, *grp.container)) {
                if (detail != nullptr) {
                    *detail = std::string("group[") + grp.title + "] " + grp.items[i].name +
                              " escapes its container";
                }
                return false;
            }
            for (int j = i + 1; j < grp.count; ++j) {
                if (PageRectOverlap(a, *grp.items[j].rc)) {
                    if (detail != nullptr) {
                        *detail = std::string("group[") + grp.title + "] overlap: " +
                                  grp.items[i].name + " vs " + grp.items[j].name;
                    }
                    return false;
                }
            }
        }
    }
    if (detail != nullptr) *detail = "ok";
    return true;
}

// =============================================================================
// 总览页：绘制
// =============================================================================
namespace {

void DrawCardLabel(HDC dc, const RECT& rc, const std::string& label, COLORREF color,
                   UiFontRole role) {
    UiDrawTextClamped(dc, label, rc, DT_LEFT | DT_VCENTER, color, role);
}

// 状态行：左侧色调圆点 + 文本（与 t1 的 HintBanner 同色系，但更紧凑以省高度）
void DrawStatusLine(HDC dc, const RECT& rc, UiTone tone, const std::string& text) {
    PageDrawStatusLine(dc, rc, tone, text, g_d.m, UiFontRole::Caption);
}

// 48 秒曲线：Divider 网格 + CPU(Info) / 内存(Success) 两条折线
void DrawSpark(HDC dc, const RECT& rc) {
    if (PageRectEmpty(rc)) return;
    HBRUSH bg = CreateSolidBrush(UiColor(UiColorRole::SurfaceAlt));
    if (bg != nullptr) { FillRect(dc, &rc, bg); DeleteObject(bg); }
    HBRUSH grid = CreateSolidBrush(UiColor(UiColorRole::Divider));
    if (grid != nullptr) {
        for (int i = 1; i <= 3; ++i) {
            const int y = rc.top + (rc.bottom - rc.top) * i / 4;
            RECT line{rc.left, y, rc.right, y + PageMaxI(1, UiScale(1))};
            FillRect(dc, &line, grid);
        }
        DeleteObject(grid);
    }
    if (g_d.histCount <= 0 || rc.right - rc.left < 8) return;
    for (int pass = 0; pass < 2; ++pass) {
        const float* hist = (pass == 0) ? g_d.cpuHist : g_d.ramHist;
        const COLORREF color = (pass == 0) ? UiColor(UiColorRole::Info) : UiColor(UiColorRole::Success);
        HPEN pen = CreatePen(PS_SOLID, PageMaxI(1, UiScale(2)), color);
        if (pen == nullptr) continue;
        HGDIOBJ op = SelectObject(dc, pen);
        for (int i = 0; i < g_d.histCount; ++i) {
            float v = hist[i];
            if (!(v >= 0.0f)) v = 0.0f;
            if (v > 100.0f) v = 100.0f;
            const int x = rc.left + i * (rc.right - rc.left - 1) / 47;
            const int y = rc.bottom - 1 - static_cast<int>(v * (rc.bottom - rc.top - 2) / 100.0f);
            if (i == 0) MoveToEx(dc, x, y, nullptr);
            else LineTo(dc, x, y);
        }
        SelectObject(dc, op);
        DeleteObject(pen);
    }
}

void DrawCurveLegend(HDC dc, const RECT& rc) {
    if (PageRectEmpty(rc)) return;
    const int y = rc.top + (rc.bottom - rc.top) / 2;
    const int seg = PageMaxI(8, g_d.m.space(3));
    int x = rc.left;
    HPEN cpuPen = CreatePen(PS_SOLID, PageMaxI(1, UiScale(2)), UiColor(UiColorRole::Info));
    if (cpuPen != nullptr) {
        HGDIOBJ op = SelectObject(dc, cpuPen);
        MoveToEx(dc, x, y, nullptr);
        LineTo(dc, x + seg, y);
        SelectObject(dc, op);
        DeleteObject(cpuPen);
    }
    x += seg + g_d.m.space(1);
    x += UiDrawTextAt(dc, x, y - g_d.m.space(3), "CPU", UiColor(UiColorRole::TextSecondary),
                      UiFontRole::Caption);
    x += g_d.m.space(3);
    HPEN ramPen = CreatePen(PS_SOLID, PageMaxI(1, UiScale(2)), UiColor(UiColorRole::Success));
    if (ramPen != nullptr) {
        HGDIOBJ op = SelectObject(dc, ramPen);
        MoveToEx(dc, x, y, nullptr);
        LineTo(dc, x + seg, y);
        SelectObject(dc, op);
        DeleteObject(ramPen);
    }
    x += seg + g_d.m.space(1);
    x += UiDrawTextAt(dc, x, y - g_d.m.space(3), T("内存", "RAM"), UiColor(UiColorRole::TextSecondary),
                      UiFontRole::Caption);
    RECT tr = rc;
    tr.left = x + g_d.m.space(2);
    UiDrawTextClamped(dc, T("48 秒 · 每秒采样", "48s · 1s sampling"), tr, DT_LEFT | DT_VCENTER,
                      UiColor(UiColorRole::TextMuted), UiFontRole::Caption);
}

void DashPaint(HWND h) {
    PAINTSTRUCT ps{};
    HDC dc = BeginPaint(h, &ps);
    RECT rc{};
    GetClientRect(h, &rc);
    UIPaintBufferBegin(g_d.buf, dc, rc);
    HDC d = (g_d.buf.ready && g_d.buf.dc != nullptr) ? g_d.buf.dc : dc;
    // 页面底：WindowBg（令牌）；卡片用 Surface，两层不同色 → 浅色/深色都分明
    HBRUSH page = CreateSolidBrush(UiColor(UiColorRole::WindowBg));
    if (page != nullptr) { FillRect(d, &rc, page); DeleteObject(page); }

    const DashLayout& L = g_d.L;
    // ---- 卡1 硬件信息 ----
    if (!PageRectEmpty(L.cardHw)) {
        UiDrawCard(d, L.cardHw);
        UiDrawCardTitle(d, L.hwTitle, T("硬件信息", "Hardware"));
        static const char* kZh[4] = {"CPU", "", "GPU", "内存"};
        static const char* kEn[4] = {"CPU", "", "GPU", "RAM"};
        const std::string values[4] = {g_d.hwCpu, g_d.hwCpu2, g_d.hwGpu, g_d.hwRam};
        for (int i = 0; i < 4; ++i) {
            if (PageRectEmpty(L.hwLine[i])) continue;
            RECT lr = L.hwLine[i];
            lr.right = lr.left + g_d.m.px(38);
            DrawCardLabel(d, lr, T(kZh[i], kEn[i]), UiColor(UiColorRole::TextMuted), UiFontRole::Caption);
            RECT vr = L.hwLine[i];
            vr.left = lr.right;
            DrawCardLabel(d, vr, values[i], UiColor(UiColorRole::TextSecondary), UiFontRole::Body);
        }
    }
    // ---- 卡2 实时 ----
    if (!PageRectEmpty(L.cardLive)) {
        UiDrawCard(d, L.cardLive);
        UiDrawCardTitle(d, L.liveTitle, T("实时负载", "Live load"));
        const std::string cpuTxt = (g_d.cpuPct >= 0) ? (std::to_string(g_d.cpuPct) + "%") : "--%";
        const std::string ramTxt = (g_d.ramPct >= 0) ? (std::to_string(g_d.ramPct) + "%") : "--%";
        if (!PageRectEmpty(L.metricCpu))
            UiDrawMetric(d, L.metricCpu, cpuTxt, UiColor(UiColorRole::Info));
        if (!PageRectEmpty(L.barCpu))
            UiDrawProgress(d, L.barCpu, g_d.cpuPct > 0 ? g_d.cpuPct / 100.0 : 0.0, UiTone::Info);
        if (!PageRectEmpty(L.metricRam))
            UiDrawMetric(d, L.metricRam, ramTxt, UiColor(UiColorRole::Success));
        if (!PageRectEmpty(L.barRam))
            UiDrawProgress(d, L.barRam, g_d.ramPct > 0 ? g_d.ramPct / 100.0 : 0.0, UiTone::Success);
        if (!PageRectEmpty(L.liveFoot)) {
            if (g_d.flowActive && g_d.flowTotal > 0) {
                RECT bar = L.liveFoot;
                bar.bottom = bar.top + g_d.m.px(8);
                UiDrawProgress(d, bar, g_d.flowProgress, g_d.flowFail ? UiTone::Warning : UiTone::Accent);
                RECT tr = L.liveFoot;
                tr.top = bar.bottom + g_d.m.space(1);
                const std::string stepText =
                    std::string(T("第 ", "step ")) + std::to_string(g_d.flowStep) + "/" +
                    std::to_string(g_d.flowTotal) + " · " + g_d.flowLabel;
                UiDrawTextClamped(d, stepText, tr, DT_LEFT | DT_VCENTER,
                                  UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);
            } else if (PageRectEmpty(L.statusLine)) {
                // 紧凑模式：底部状态行放不下时，状态文案沉到实时卡页脚（保证可见反馈不丢）
                PageDrawStatusLine(d, L.liveFoot, g_d.statusTone, g_d.status, g_d.m, UiFontRole::Caption);
            } else {
                UiDrawTextClamped(d, T("CPU / 内存 每秒采样 · 曲线 48 秒", "CPU / RAM sampled every 1s · curve 48s"),
                                  L.liveFoot, DT_LEFT | DT_VCENTER, UiColor(UiColorRole::TextMuted),
                                  UiFontRole::Caption);
            }
        }
    }
    // ---- 卡3 曲线 ----
    if (!PageRectEmpty(L.cardCurve)) {
        UiDrawCard(d, L.cardCurve);
        UiDrawCardTitle(d, L.curveTitle, T("实时曲线（48 秒）", "Live curve (48s)"));
        DrawCurveLegend(d, L.curveLegend);
        DrawSpark(d, L.curveArea);
    }
    // ---- 左下动作区状态行（无卡片底，直接画在页面上） ----
    if (!PageRectEmpty(L.statusLine))
        DrawStatusLine(d, L.statusLine, g_d.statusTone, g_d.status);
    // ---- 历史卡 ----
    if (!PageRectEmpty(L.cardHist)) {
        UiDrawCard(d, L.cardHist);
        UiDrawCardTitle(d, L.histTitle, T("优化历史（只读）", "Optimization history (read-only)"));
        if (!PageRectEmpty(L.histStatus)) DrawStatusLine(d, L.histStatus, HistStatusTone(), HistStatusText());
    }
    UIPaintBufferEnd(g_d.buf);
    EndPaint(h, &ps);
}

// ---------- 自绘：按钮 + 历史列表行 ----------
bool DashDrawItem(const DRAWITEMSTRUCT* di) {
    if (di == nullptr) return false;
    if (di->CtlType == ODT_BUTTON) {
        const int id = static_cast<int>(di->CtlID);
        if (id < kDashCtlFirst || id > kDashCtlLast) return false;
        std::string label;
        bool primary = false;
        switch (id) {
            case IDC_DASH_BOOST:        label = T("一键性能优化", "One-click Boost"); primary = true; break;
            case IDC_DASH_ROLLBACK:     label = T("回滚最近一次优化", "Rollback Last"); break;
            case IDC_DASH_ABOUT:        label = T("诊断 / 关于", "Diagnostics / About"); break;
            case IDC_DASH_HIST_REFRESH: label = T("刷新", "Refresh"); break;
            default: return false;
        }
        const UiButtonState st = PageButtonStateFromDrawItem(di);
        RECT rc = di->rcItem;
        if (UiDrawButton(di->hDC, rc, st, label, primary)) return true;
        return false;
    }
    if (di->CtlType == ODT_LISTBOX && static_cast<int>(di->CtlID) == IDC_DASH_HIST_LIST) {
        const UINT idx = di->itemID;
        RECT rc = di->rcItem;
        const bool selected = (di->itemState & ODS_SELECTED) != 0;
        const bool disabled = (di->itemState & ODS_DISABLED) != 0;
        const UiRowState rowState = disabled ? UiRowState::Disabled
                                             : (selected ? UiRowState::Selected : UiRowState::Normal);
        const bool zebra = (idx % 2u) == 1u;
        if (g_d.hist.empty()) {
            UiDrawListRow(di->hDC, rc, rowState,
                          T("（暂无快照：尚未优化过，或快照文件异常）",
                            "(no snapshots: never optimized, or savepoint file abnormal)"),
                          zebra);
            return true;
        }
        if (idx >= g_d.hist.size()) return false;
        UiDrawListRow(di->hDC, rc, rowState, HistRowText(idx), zebra);
        UiDrawListRowRight(di->hDC, rc, HistRightText(idx), rowState);
        if (selected && (di->itemState & ODS_FOCUS) != 0) {
            RECT fr = rc;
            InflateRect(&fr, -1, -1);
            UiStrokeRoundRect(di->hDC, fr, g_d.m.radius[0], UiColor(UiColorRole::FocusRing), 1);
        }
        return true;
    }
    return false;
}

// ---------- 控件创建 ----------
HWND MakeCtl(HWND parent, const wchar_t* cls, const wchar_t* text, DWORD style, DWORD exStyle,
             const RECT& rc, int id, UiFontRole fontRole) {
    HINSTANCE hi = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
    HWND h = CreateWindowExW(exStyle, cls, text, style | WS_CHILD | WS_VISIBLE, rc.left, rc.top,
                             PageMaxI(0, rc.right - rc.left), PageMaxI(0, rc.bottom - rc.top), parent,
                             reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), hi, nullptr);
    if (h != nullptr) {
        HFONT f = UiFont(fontRole);
        if (f != nullptr) SendMessageW(h, WM_SETFONT, reinterpret_cast<WPARAM>(f), TRUE);
    }
    return h;
}

// 把布局矩形应用到真实控件（空矩形 → 隐藏，绝不裁切/重叠）
void ApplyRect(HWND h, const RECT& rc) {
    if (h == nullptr) return;
    if (PageRectEmpty(rc)) {
        ShowWindow(h, SW_HIDE);
        return;
    }
    ShowWindow(h, SW_SHOWNA);
    SetWindowPos(h, nullptr, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top,
                 SWP_NOZORDER | SWP_NOACTIVATE);
}

void LayoutControls() {
    const DashLayout& L = g_d.L;
    ApplyRect(g_d.btnBoost, L.btnBoost);
    ApplyRect(g_d.chkAutostart, L.chkAutostart);
    ApplyRect(g_d.btnRollback, L.btnRollback);
    ApplyRect(g_d.btnAbout, L.btnAbout);
    ApplyRect(g_d.btnHistRefresh, L.btnHistRefresh);
    ApplyRect(g_d.lstHist, L.lstHist);
    if (g_d.lstHist != nullptr)
        SendMessageW(g_d.lstHist, LB_SETITEMHEIGHT, 0, PageMaxI(g_d.m.height(0), g_d.m.px(20)));
}

// 面板尺寸 = 页容器客户区；宿主 WM_SIZE 调 DashboardPageLayout()，定时器里再自愈一次
void Relayout() {
    if (g_d.panel == nullptr || g_d.container == nullptr) return;
    RECT cr{};
    GetClientRect(g_d.container, &cr);
    const int w = PageMaxI(0, cr.right - cr.left);
    const int h = PageMaxI(0, cr.bottom - cr.top);
    if (w > 0 && h > 0) {
        RECT pr{};
        GetWindowRect(g_d.panel, &pr);
        if (pr.right - pr.left != w || pr.bottom - pr.top != h)
            SetWindowPos(g_d.panel, nullptr, 0, 0, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
    g_d.m = MakePageMetrics();
    g_d.L = ComputeDashLayout(w, h, g_d.m);
    std::string detail;
    if (!DashLayoutSelfCheck(g_d.L, &detail)) {
        g_d.lastCheckDetail = detail;
        Log(std::string(T("[总览页布局自检] 失败：", "[dashboard layout self-check] FAIL: ")) + detail + "\n");
    } else if (g_d.L.valid) {
        g_d.lastCheckDetail = "ok";
    } else {
        g_d.lastCheckDetail = "ok (degraded: " + g_d.L.note + ")";
    }
    LayoutControls();
    InvalidateRect(g_d.panel, nullptr, TRUE);
}

// ---------- 命令分发（页面内部；面板就地调用 + 转发宿主，靠 GetMessageTime 幂等） ----------
void DispatchCommand(int id, int code) {
    switch (id) {
        case IDC_DASH_BOOST:
            if (code == BN_CLICKED) StartBoost();
            return;
        case IDC_DASH_ROLLBACK:
            if (code == BN_CLICKED) StartRollback();
            return;
        case IDC_DASH_AUTOSTART:
            if (code == BN_CLICKED) {
                const bool on = SendMessageW(g_d.chkAutostart, BM_GETCHECK, 0, 0) == BST_CHECKED;
                const bool ok = AutoStartSet(on);
                const std::string msg =
                    std::string(T("开机自启动：", "Start with Windows: ")) +
                    (on ? T("已开启", "enabled") : T("已关闭", "disabled")) +
                    (ok ? "" : T("（写入注册表失败，可能被策略限制）", " (registry write failed)"));
                SetBanner(msg, ok ? UiTone::Success : UiTone::Warning);
                Log(msg + "\n\n");
            }
            return;
        case IDC_DASH_ABOUT:
            if (code == BN_CLICKED) ShowAbout();
            return;
        case IDC_DASH_HIST_REFRESH:
            if (code == BN_CLICKED) {
                RefreshHistory();
                const std::string msg =
                    std::string(T("已刷新优化历史：", "History refreshed: ")) +
                    std::to_string(g_d.hist.size()) + T(" 条", " entries") +
                    (g_d.histError.empty() ? "" : (T("（", " (") + g_d.histError + ")"));
                SetBanner(msg, g_d.histError.empty() ? UiTone::Success : UiTone::Warning);
                Log(msg + "\n");
            }
            return;
        case IDC_DASH_HIST_LIST:
            if (code == LBN_SELCHANGE) {
                g_d.histSel = static_cast<int>(SendMessageW(g_d.lstHist, LB_GETCURSEL, 0, 0));
                if (g_d.histSel >= 0 && g_d.histSel < static_cast<int>(g_d.hist.size())) {
                    const SavepointInfo& sp = g_d.hist[static_cast<size_t>(g_d.histSel)];
                    SetBanner(std::string(T("已选中 #", "selected #")) + std::to_string(sp.index) + " · " +
                                  LocalizedSummary(sp) + " · " + std::to_string(sp.entryCount) +
                                  T(" 项（双击看详情）", " items (double-click for detail)"),
                              UiTone::Accent);
                }
            } else if (code == LBN_DBLCLK) {
                ShowSavepointDetail();
            }
            return;
        default:
            return;
    }
}

// ---------- 面板窗口过程 ----------
LRESULT CALLBACK DashPanelProc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_ERASEBKGND:
            return 1;  // 全部自绘（双缓冲），避免闪烁
        case WM_PAINT:
            DashPaint(h);
            return 0;
        case WM_SIZE:
            Relayout();
            return 0;
        case WM_TIMER:
            if (wp == kTimerLive) {
                Relayout();  // 自愈：宿主 WM_SIZE 未调用时 1 秒内补齐
                if (IsWindowVisible(h)) {
                    SampleLive();
                    if (!g_d.busy && (++g_d.ticks % kHistRefreshTicks) == 0) RefreshHistory();
                    if (g_d.hwRam.empty()) RefreshHardwareText();
                    InvalidateRect(h, nullptr, FALSE);
                }
            }
            return 0;
        case WM_DRAWITEM:
            if (DashDrawItem(reinterpret_cast<const DRAWITEMSTRUCT*>(lp))) return TRUE;
            break;
        case WM_MEASUREITEM: {
            auto* mi = reinterpret_cast<MEASUREITEMSTRUCT*>(lp);
            if (mi != nullptr && mi->CtlType == ODT_LISTBOX &&
                static_cast<int>(mi->CtlID) == IDC_DASH_HIST_LIST) {
                mi->itemHeight = static_cast<UINT>(PageMaxI(g_d.m.height(0), g_d.m.px(20)));
                return TRUE;
            }
            break;
        }
        case WM_COMMAND: {
            const int id = LOWORD(wp);
            const int code = HIWORD(wp);
            g_routeGuard.valid = false;     // 新消息：先清上一条的幂等守卫
            DashboardPageCommand(id, code);  // 就地分发（自包含：按钮绝不会是死代码）
            if (id >= kDashCtlFirst && id <= kDashCtlLast) {
                g_routeGuard.valid = true;   // 同一条消息若被宿主再路由 → 拦下
                g_routeGuard.id = id;
                g_routeGuard.code = code;
            }
            // 保持 v1.0.19 的 PageProc → 主窗口 链路（宿主不识别这些 ID 时走 default）
            HWND parent = GetParent(h);
            if (parent != nullptr) SendMessageW(parent, WM_COMMAND, wp, lp);
            return 0;
        }
        case WM_CTLCOLORSTATIC:
        case WM_CTLCOLOREDIT:
        case WM_CTLCOLORLISTBOX: {
            HDC dc = reinterpret_cast<HDC>(wp);
            if (dc != nullptr) {
                SetTextColor(dc, UiColor(UiColorRole::TextPrimary));
                SetBkColor(dc, UiColor(UiColorRole::Surface));
                SetBkMode(dc, TRANSPARENT);
            }
            if (g_d.brSurface == nullptr)
                g_d.brSurface = CreateSolidBrush(UiColor(UiColorRole::Surface));
            return reinterpret_cast<LRESULT>(g_d.brSurface);
        }
        case WM_THEMECHANGED:
        case WM_SYSCOLORCHANGE:
            if (g_d.brSurface != nullptr) { DeleteObject(g_d.brSurface); g_d.brSurface = nullptr; }
            InvalidateRect(h, nullptr, TRUE);
            return 0;
        case kMsgFlow: {
            auto* p = reinterpret_cast<FlowPayload*>(lp);
            if (p != nullptr) {
                const AppCore::FlowEvent& e = p->e;
                if (e.kind == AppCore::FlowEvent::Info) {
                    if (!e.text.empty()) Log(e.text + "\n");
                } else if (e.kind == AppCore::FlowEvent::StepStart) {
                    g_d.flowActive = true;
                    g_d.flowStep = e.step;
                    g_d.flowTotal = e.total;
                    g_d.flowLabel = e.text;
                    if (static_cast<int>(g_d.flowRows.size()) < e.step) g_d.flowRows.resize(e.step);
                    g_d.flowRows[e.step - 1].label = e.text;
                    Log("  " + std::to_string(e.step) + ") " + e.text + ": " +
                        T("处理中...", "running...") + "\n");
                } else {
                    const bool ok = (e.kind == AppCore::FlowEvent::StepOk);
                    if (!ok) g_d.flowFail = true;
                    if (e.step >= 1 && e.step <= static_cast<int>(g_d.flowRows.size())) {
                        FlowRow& r = g_d.flowRows[e.step - 1];
                        r.ok = ok;
                        r.fail = !ok;
                        r.elapsedMs = e.elapsedMs;
                    }
                    g_d.flowLabel = e.text;
                    Log("  " + std::to_string(e.step) + ") " + e.text + ": " +
                        (ok ? T("成功", "OK") : T("失败", "FAIL")) + "（" + std::to_string(e.elapsedMs) +
                        " ms）\n");
                }
                if (e.total > 0) g_d.flowProgress = static_cast<double>(e.step) / e.total;
                delete p;
                InvalidateRect(h, nullptr, FALSE);
            }
            return 0;
        }
        case kMsgBoostDone: {
            auto* p = reinterpret_cast<TextPayload*>(lp);
            if (p != nullptr) {
                g_d.busy = false;
                g_d.flowActive = false;
                g_d.flowProgress = 0.0;
                EnableWindow(g_d.btnBoost, TRUE);
                EnableWindow(g_d.btnRollback, g_d.hist.empty() ? FALSE : TRUE);
                Log("\n" + p->text + "\n\n");
                SetBanner(g_d.flowFail
                              ? T("一键性能优化结束（存在失败项，可一键回滚）",
                                  "Boost finished with failures (rollback available)")
                              : T("一键性能优化完成（详情见日志，可一键回滚）",
                                  "Boost finished (see log; rollback available)"),
                          g_d.flowFail ? UiTone::Warning : UiTone::Success);
                delete p;
                RefreshHistory();
                InvalidateRect(h, nullptr, FALSE);
            }
            return 0;
        }
        case kMsgRollback: {
            auto* p = reinterpret_cast<TextPayload*>(lp);
            if (p != nullptr) {
                g_d.busy = false;
                EnableWindow(g_d.btnBoost, TRUE);
                Log("\n" + p->text + "\n\n");
                delete p;
                RefreshHistory();
                const int after = static_cast<int>(g_d.hist.size());
                const bool effective = after < g_d.rollbackBefore;
                SetBanner(std::string(T("回滚", "Rollback ")) +
                              (effective ? T("已生效", "applied") : T("未改变快照历史", "did not change history")) +
                              T("：历史 ", ": history ") + std::to_string(g_d.rollbackBefore) + " → " +
                              std::to_string(after) + T(" 条（详情见日志）", " entries (see log)"),
                          effective ? UiTone::Success : UiTone::Danger);
                EnableWindow(g_d.btnRollback, g_d.hist.empty() ? FALSE : TRUE);
                InvalidateRect(h, nullptr, FALSE);
            }
            return 0;
        }
        default:
            break;
    }
    return DefWindowProcW(h, msg, wp, lp);
}

void DrainPostedPayloads() {
    if (g_d.panel == nullptr) return;
    MSG msg{};
    while (PeekMessageW(&msg, g_d.panel, kMsgFlow, kMsgBoostDone, PM_REMOVE)) {
        if (msg.message == kMsgFlow) delete reinterpret_cast<FlowPayload*>(msg.lParam);
        else delete reinterpret_cast<TextPayload*>(msg.lParam);
    }
    while (PeekMessageW(&msg, g_d.panel, kMsgRollback, kMsgRollback, PM_REMOVE))
        delete reinterpret_cast<TextPayload*>(msg.lParam);
}

}  // namespace

// =============================================================================
// 导出：总览页装配接口
// =============================================================================
bool DashboardPageCreate(HWND pageContainer, const PageHostHooks& hooks) {
    if (pageContainer == nullptr || g_d.created) return false;
    HINSTANCE hi = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));

    WNDCLASSW wc{};
    wc.lpfnWndProc = DashPanelProc;
    wc.hInstance = hi;
    wc.lpszClassName = kPanelClass;
    wc.style = CS_HREDRAW | CS_VREDRAW;
    // 注意：本模块在未定义 UNICODE 的目标里也要能编译（IDC_ARROW 是 ANSI 宏），
    // 与 gopt_gui.cpp 的做法一致，显式转成 LPCWSTR。
    wc.hCursor = LoadCursorW(nullptr, reinterpret_cast<LPCWSTR>(IDC_ARROW));
    wc.hbrBackground = nullptr;  // 自绘
    if (RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) return false;

    g_d.container = pageContainer;
    g_d.hooks = hooks;
    g_d.panel = CreateWindowExW(WS_EX_CONTROLPARENT, kPanelClass, L"",
                               WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN, 0, 0, 10, 10, pageContainer,
                               nullptr, hi, nullptr);
    if (g_d.panel == nullptr) return false;

    RECT z{0, 0, 10, 10};
    g_d.btnBoost = MakeCtl(g_d.panel, L"BUTTON", L"", BS_OWNERDRAW, 0, z, IDC_DASH_BOOST,
                           UiFontRole::BodyBold);
    g_d.btnRollback = MakeCtl(g_d.panel, L"BUTTON", L"", BS_OWNERDRAW, 0, z, IDC_DASH_ROLLBACK,
                              UiFontRole::BodyBold);
    g_d.btnAbout = MakeCtl(g_d.panel, L"BUTTON", L"", BS_OWNERDRAW, 0, z, IDC_DASH_ABOUT,
                           UiFontRole::Body);
    g_d.btnHistRefresh = MakeCtl(g_d.panel, L"BUTTON", L"", BS_OWNERDRAW, 0, z, IDC_DASH_HIST_REFRESH,
                                 UiFontRole::Body);
    g_d.chkAutostart = MakeCtl(g_d.panel, L"BUTTON", L"", BS_AUTOCHECKBOX, 0, z, IDC_DASH_AUTOSTART,
                               UiFontRole::Body);
    g_d.lstHist = MakeCtl(g_d.panel, L"LISTBOX", L"",
                          LBS_OWNERDRAWFIXED | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | WS_VSCROLL |
                              WS_TABSTOP | WS_BORDER,
                          WS_EX_CLIENTEDGE, z, IDC_DASH_HIST_LIST, UiFontRole::Body);
    if (g_d.btnBoost == nullptr || g_d.btnRollback == nullptr || g_d.btnAbout == nullptr ||
        g_d.btnHistRefresh == nullptr || g_d.chkAutostart == nullptr || g_d.lstHist == nullptr)
        return false;

    PageEnableButtonHover(g_d.btnBoost, true);
    PageEnableButtonHover(g_d.btnRollback, false);
    PageEnableButtonHover(g_d.btnAbout, false);
    PageEnableButtonHover(g_d.btnHistRefresh, false);

    PageSetTextUtf8(g_d.chkAutostart, T("开机自启动", "Start with Windows"));
    SendMessageW(g_d.chkAutostart, BM_SETCHECK, AutoStartExists() ? BST_CHECKED : BST_UNCHECKED, 0);

    g_d.created = true;
    g_d.status = T("就绪：一键优化 / 一键回滚都在后台线程执行，界面不阻塞",
                   "Ready: boost and rollback run on worker threads; UI never blocks");
    g_d.statusTone = UiTone::Accent;
    g_d.m = MakePageMetrics();
    RefreshHardwareText();
    SampleLive();
    RefreshHistory();
    Relayout();
    SetTimer(g_d.panel, kTimerLive, 1000, nullptr);
    Log(std::string("GameOptimizer v") + GOPT_VERSION_STR +
        T("  总览页就绪（硬件信息 / 实时负载 / 实时曲线 / 优化历史与一键回滚）\n",
          "  dashboard ready (hardware / live load / curve / history + rollback)\n"));
    return true;
}

void DashboardPageDestroy() {
    if (g_d.panel != nullptr) {
        KillTimer(g_d.panel, kTimerLive);
        DrainPostedPayloads();  // 释放未处理载荷（不泄漏）
        DestroyWindow(g_d.panel);
        g_d.panel = nullptr;
    }
    UIPaintBufferFree(g_d.buf);
    if (g_d.brSurface != nullptr) { DeleteObject(g_d.brSurface); g_d.brSurface = nullptr; }
    if (g_d.ownCore != nullptr) { delete g_d.ownCore; g_d.ownCore = nullptr; }
    PageDisableAllButtonHover();  // 清理已销毁窗口的悬停登记（活着的窗口不受影响）
    g_d = DashState{};
}

void DashboardPageLayout() { Relayout(); }

void DashboardPageOnShow() {
    g_d.m = MakePageMetrics();
    RefreshHardwareText();
    SampleLive();
    RefreshHistory();
    Relayout();
    InvalidateRect(g_d.panel, nullptr, TRUE);
    SetBanner(T("总览页已刷新（硬件 / 实时 / 优化历史）", "Dashboard refreshed (hardware / live / history)"),
              UiTone::Accent);
}

bool DashboardPageCommand(int id, int code) {
    if (id < kDashCtlFirst || id > kDashCtlLast) return false;
    // 幂等守卫：同一条 WM_COMMAND 若既被面板就地分发、又被宿主路由一次，第二次被拦下并消费
    if (g_routeGuard.valid && g_routeGuard.id == id && g_routeGuard.code == code) {
        g_routeGuard.valid = false;
        return true;
    }
    DispatchCommand(id, code);
    return true;
}

void DashboardPageApplyLanguage() {
    if (g_d.chkAutostart != nullptr)
        PageSetTextUtf8(g_d.chkAutostart, T("开机自启动", "Start with Windows"));
    if (g_d.lstHist != nullptr) InvalidateRect(g_d.lstHist, nullptr, TRUE);
    if (g_d.panel != nullptr) InvalidateRect(g_d.panel, nullptr, TRUE);
    SetBanner(T("语言已切换：总览页文案已同步", "Language switched: dashboard text synced"), UiTone::Accent);
}

void DashboardPageApplyTheme() {
    g_d.m = MakePageMetrics();
    if (g_d.brSurface != nullptr) { DeleteObject(g_d.brSurface); g_d.brSurface = nullptr; }
    HWND ctrls[6] = {g_d.btnBoost, g_d.chkAutostart, g_d.btnRollback,
                     g_d.btnAbout, g_d.btnHistRefresh, g_d.lstHist};
    for (int i = 0; i < 6; ++i) {
        if (ctrls[i] == nullptr) continue;
        const UiFontRole role = (ctrls[i] == g_d.btnBoost || ctrls[i] == g_d.btnRollback)
                                    ? UiFontRole::BodyBold
                                    : UiFontRole::Body;
        HFONT f = UiFont(role);
        if (f != nullptr) SendMessageW(ctrls[i], WM_SETFONT, reinterpret_cast<WPARAM>(f), TRUE);
    }
    Relayout();
    if (g_d.lstHist != nullptr) InvalidateRect(g_d.lstHist, nullptr, TRUE);
}

void DashboardPageRefresh() {
    RefreshHardwareText();
    SampleLive();
    RefreshHistory();
    InvalidateRect(g_d.panel, nullptr, FALSE);
}

}  // namespace ui
}  // namespace gopt
