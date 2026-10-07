// =============================================================================
// GameOptimizer v1.1.0 — 页面组 A：游戏优化页（选游戏 / 预设摘要 / 代启动 / 应用 / 回滚）
// -----------------------------------------------------------------------------
// 【通知路径】与总览页一致：页容器（宿主 STATIC，PageProc 不动）→ 页内面板
//   （本文件注册窗口类 gopt_ui_page_game）→ 面板就地分发命令 + 转发 WM_COMMAND
//   给页容器（保持 PageProc → 主窗口 链路；宿主不必认识 1301..1310）。
//
// 【线程模型】「应用优化」与「回滚」都在工作线程执行：UI 线程只做「取配置 → 校验
//   → 存盘（应用前同步 GameConfig，保证所见即所应用）→ 置 busy/禁用按钮 → 起线程」，
//   线程体自建 AppCore（不与他人共享实例，避免 mutable 错误字段/快照栈的数据竞争），
//   结束后 PostMessage 回面板；UI 线程更新进度、步骤、状态行与日志，界面全程不阻塞。
//
// 【输入校验】路径：可空（=优化正在运行的游戏）；非空则要求 ≤MAX_PATH、无引号、
//   .exe 结尾、文件存在且不是目录。参数：≤512 字符、无换行、双引号成对。
//   校验失败 → 页内状态行（Danger 色调） + 日志，并**不**执行优化。
//
// 【布局】ComputeGameLayout(w,h,PageMetrics) 纯整数：左卡（代启动表单）52% + 右卡
//   （预设摘要）；底部动作区（应用/保存/回滚 + 进度条 + 状态行 + 步骤明细）。
//   行高与间距全部取令牌；空间不足时置空矩形并隐藏，永不重叠/裁切。
//
// 【红线】仅官方 API：user32/gdi32/advapi32/kernel32/comdlg32；无注入、无 Hook。
// =============================================================================

#include "gui/page_game.h"

#include <commdlg.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

#include "config/GameConfig.h"
#include "core/AppCore.h"
#include "i18n.h"
#include "license/License.h"
#include "version.h"

namespace gopt {
namespace ui {
namespace {

constexpr UINT kMsgFlow    = WM_APP + 31;  // 优化流程事件
constexpr UINT kMsgApply   = WM_APP + 32;  // 应用优化结束（结果文本）
constexpr UINT kMsgRollback = WM_APP + 33; // 回滚结束（结果文本）
constexpr UINT_PTR kTimerLive = 2101;      // 1 秒：布局自愈 + 空闲重绘

const wchar_t kPanelClass[] = L"gopt_ui_page_game";

struct FlowPayload {
    AppCore::FlowEvent e;
};
struct TextPayload {
    std::string text;
};

struct FlowRow {
    std::string label;
    bool ok = false;
    bool fail = false;
    int elapsedMs = 0;
};

struct GameState {
    HWND container = nullptr;
    HWND panel = nullptr;
    HWND cmbGame = nullptr;
    HWND edtPath = nullptr;
    HWND btnBrowse = nullptr;
    HWND edtArgs = nullptr;
    HWND chkPower = nullptr;
    HWND chkFrame = nullptr;
    HWND chkWork = nullptr;
    HWND btnApply = nullptr;
    HWND btnSave = nullptr;
    HWND btnRollback = nullptr;
    PageHostHooks hooks{};
    AppCore* ownCore = nullptr;
    GameLayout L{};
    PageMetrics m{};
    UIPaintBuffer buf{};
    HBRUSH brSurface = nullptr;

    GamePreset preset{};         // 当前游戏 + 当前硬件下的最终参数
    std::string edition;         // 授权版本文本（免费/Pro）
    std::string presetDesc;      // 预设说明（可能为空）
    std::string param[4];        // 摘要四行

    bool flowActive = false;
    double flowProgress = 0.0;
    int flowStep = 0, flowTotal = 0;
    std::string flowLabel;
    bool flowFail = false;
    std::vector<FlowRow> flowRows;

    std::string status;
    UiTone statusTone = UiTone::Accent;
    bool busy = false;
    int rollbackBefore = 0;
    bool dirty = false;  // 表单有未保存修改（提示用）
    bool created = false;
} g_g;

// 同一 WM_COMMAND 经「面板就地分发 + 转发宿主」两条路径到达时的幂等守卫（判据同总览页，
// 确定性、不依赖计时：面板就地分发后置 (id,code) 守卫，宿主的重复路由会被消费掉）。
struct RouteGuard {
    bool valid = false;
    int id = -1;
    int code = 0;
} g_routeGuard;

// ---------- 游戏名（双语；中文名来自核心 GameIdToString） ----------
const GameId kGames[8] = {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2, GameId::PUBG,
                          GameId::Valorant, GameId::Apex, GameId::Dota2, GameId::Overwatch2};
const char* kGameEn[8] = {"Delta Force", "League of Legends", "Counter-Strike 2", "PUBG",
                          "VALORANT", "Apex Legends", "Dota 2", "Overwatch 2"};
std::string GameNameLocalized(GameId id) {
    for (int i = 0; i < 8; ++i) {
        if (kGames[i] == id) {
            if (CurrentLang() == Lang::En && kGameEn[i] != nullptr) return std::string(kGameEn[i]);
            return GameIdToString(id);
        }
    }
    return GameIdToString(id);
}
int CurrentGameIndex() {
    if (g_g.cmbGame == nullptr) return 0;
    const int sel = static_cast<int>(SendMessageW(g_g.cmbGame, CB_GETCURSEL, 0, 0));
    return (sel >= 0 && sel < 8) ? sel : 0;
}
GameId CurrentGameId() { return kGames[CurrentGameIndex()]; }

// ---------- 反馈 ----------
void Log(const std::string& s) {
    if (g_g.hooks.AppendLog != nullptr) g_g.hooks.AppendLog(s.c_str());
}
void SetBanner(const std::string& text, UiTone tone) {
    g_g.status = text;
    g_g.statusTone = tone;
    if (g_g.hooks.SetStatus != nullptr) g_g.hooks.SetStatus(text.c_str());
    if (g_g.panel != nullptr) {
        if (PageRectEmpty(g_g.L.statusLine)) InvalidateRect(g_g.panel, nullptr, FALSE);
        else InvalidateRect(g_g.panel, &g_g.L.statusLine, FALSE);
    }
}
AppCore* CoreForQuery() {
    if (g_g.hooks.sharedCore != nullptr) return g_g.hooks.sharedCore;
    if (g_g.ownCore == nullptr) g_g.ownCore = new AppCore();
    return g_g.ownCore;
}

// ---------- 表单 ←→ 配置（GameConfig 持久化） ----------
void ComboRebuild(int keepIndex) {
    if (g_g.cmbGame == nullptr) return;
    SendMessageW(g_g.cmbGame, CB_RESETCONTENT, 0, 0);
    for (int i = 0; i < 8; ++i) {
        const std::wstring name = PageToWide(GameNameLocalized(kGames[i]));
        SendMessageW(g_g.cmbGame, CB_ADDSTRING, 0, reinterpret_cast<LPARAM>(name.c_str()));
    }
    const int idx = (keepIndex >= 0 && keepIndex < 8) ? keepIndex : 0;
    SendMessageW(g_g.cmbGame, CB_SETCURSEL, idx, 0);
}
void LoadConfigToUI() {
    const GameLaunchConfig gc = GameConfig::Get(CurrentGameId());
    PageSetTextUtf8(g_g.edtPath, gc.exePath);
    PageSetTextUtf8(g_g.edtArgs, gc.args);
    SendMessageW(g_g.chkPower, BM_SETCHECK, gc.powerScheme ? BST_CHECKED : BST_UNCHECKED, 0);
    SendMessageW(g_g.chkFrame, BM_SETCHECK, gc.frameLatency ? BST_CHECKED : BST_UNCHECKED, 0);
    SendMessageW(g_g.chkWork, BM_SETCHECK, gc.workingSet ? BST_CHECKED : BST_UNCHECKED, 0);
    PageSharedFlags().allowPowerSchemeSwitch = gc.powerScheme;
    g_g.dirty = false;
}
void CollectFormIntoConfig(GameLaunchConfig* gc) {
    if (gc == nullptr) return;
    *gc = GameConfig::Get(CurrentGameId());
    gc->exePath = PageGetTextUtf8(g_g.edtPath);
    gc->args = PageGetTextUtf8(g_g.edtArgs);
    gc->powerScheme = SendMessageW(g_g.chkPower, BM_GETCHECK, 0, 0) == BST_CHECKED;
    gc->frameLatency = SendMessageW(g_g.chkFrame, BM_GETCHECK, 0, 0) == BST_CHECKED;
    gc->workingSet = SendMessageW(g_g.chkWork, BM_GETCHECK, 0, 0) == BST_CHECKED;
}

// ---------- 输入校验 ----------
struct Validation {
    bool ok = true;      // false = 不能执行优化
    bool warn = false;   // true = 可以执行但需要注意
    std::string msg;     // 双语可读说明
};
bool EndsWithNoCase(const std::string& s, const char* suffix) {
    const size_t n = strlen(suffix);
    if (s.size() < n) return false;
    for (size_t i = 0; i < n; ++i) {
        char a = s[s.size() - n + i];
        char b = suffix[i];
        if (a >= 'A' && a <= 'Z') a = static_cast<char>(a - 'A' + 'a');
        if (b >= 'A' && b <= 'Z') b = static_cast<char>(b - 'A' + 'a');
        if (a != b) return false;
    }
    return true;
}
Validation ValidateLaunch(const std::string& path, const std::string& args) {
    Validation v;
    if (!path.empty()) {
        if (path.size() >= MAX_PATH) {
            v.ok = false;
            v.msg = T("路径过长（≥260 字符），请缩短或改用短路径", "path too long (>=260 chars)");
            return v;
        }
        if (path.find('"') != std::string::npos) {
            v.ok = false;
            v.msg = T("路径不能包含引号（填写纯路径即可）", "path must not contain quotes");
            return v;
        }
        if (!EndsWithNoCase(path, ".exe")) {
            v.ok = false;
            v.msg = T("路径必须以 .exe 结尾（可点「浏览…」选择）",
                      "path must end with .exe (use Browse…)");
            return v;
        }
        const DWORD attr = GetFileAttributesW(PageToWide(path).c_str());
        if (attr == INVALID_FILE_ATTRIBUTES) {
            v.ok = false;
            v.msg = T("路径不存在或不可访问，请检查后重试（或留空以优化运行中的游戏）",
                      "path not found/unreadable (leave empty to optimize a running game)");
            return v;
        }
        if ((attr & FILE_ATTRIBUTE_DIRECTORY) != 0) {
            v.ok = false;
            v.msg = T("该路径是目录，不是可执行文件", "path is a directory, not an executable");
            return v;
        }
    }
    if (args.size() > 512) {
        v.ok = false;
        v.msg = T("启动参数过长（>512 字符）", "args too long (>512 chars)");
        return v;
    }
    if (args.find('\n') != std::string::npos || args.find('\r') != std::string::npos) {
        v.ok = false;
        v.msg = T("启动参数不能包含换行", "args must not contain line breaks");
        return v;
    }
    int quotes = 0;
    for (size_t i = 0; i < args.size(); ++i)
        if (args[i] == '"') ++quotes;
    if ((quotes % 2) != 0) {
        v.ok = false;
        v.msg = T("启动参数中的双引号不成对", "unbalanced quotes in args");
        return v;
    }
    if (path.empty()) {
        v.warn = true;
        v.msg = T("未设置代启动路径：应用优化将作用于正在运行的游戏（找不到时给出提示）",
                  "No launch path: will optimize a running game (with a hint if none found)");
    } else {
        v.msg = T("路径与参数校验通过", "path & args validated");
    }
    return v;
}

// ---------- 预设摘要 ----------
const char* PriorityText(uint32_t cls) {
    switch (cls) {
        case 0:                     return T("不设置", "not set");
        case HIGH_PRIORITY_CLASS:   return T("高 (0x80)", "High (0x80)");
        case ABOVE_NORMAL_PRIORITY_CLASS: return T("高于正常 (0x8000)", "AboveNormal (0x8000)");
        case NORMAL_PRIORITY_CLASS: return T("正常 (0x20)", "Normal (0x20)");
        case BELOW_NORMAL_PRIORITY_CLASS: return T("低于正常 (0x4000)", "BelowNormal (0x4000)");
        case IDLE_PRIORITY_CLASS:   return T("空闲 (0x40)", "Idle (0x40)");
        default:                    return T("不设置", "not set");
    }
}
void BuildPresetLines() {
    char buf[256] = {};
    // 0) 进程优先级（上限 HIGH：红线，不使用 REALTIME）
    g_g.param[0] = std::string(T("优先级：", "Priority: ")) + PriorityText(g_g.preset.processPriorityClass);
    // 1) 亲和性
    if (g_g.preset.cpuAffinityMask == 0) {
        g_g.param[1] = std::string(T("亲和性：不设置（不绑定核心）", "Affinity: not set"));
    } else {
        std::snprintf(buf, sizeof(buf), "0x%016llX", static_cast<unsigned long long>(g_g.preset.cpuAffinityMask));
        g_g.param[1] = std::string(T("亲和性：掩码 ", "Affinity: mask ")) + buf;
        if (g_g.preset.bindPhysicalOnly) g_g.param[1] += T("（仅物理核）", " (physical only)");
        if (g_g.preset.leaveCoresForSystem >= 0)
            g_g.param[1] += std::string(T("（保留 ", " (reserve ")) +
                            std::to_string(g_g.preset.leaveCoresForSystem) + T(" 核给系统）", " cores)");
    }
    // 2) 工作集
    if (g_g.preset.workingSetMinMB == 0 && g_g.preset.workingSetMaxMB == 0) {
        g_g.param[2] = std::string(T("工作集：不设置", "Working set: not set"));
    } else {
        g_g.param[2] = std::string(T("工作集：", "Working set: ")) +
                       std::to_string(g_g.preset.workingSetMinMB) + " ~ " +
                       std::to_string(g_g.preset.workingSetMaxMB) + " MB";
    }
    // 3) 电源
    g_g.param[3] = g_g.preset.switchHighPerformancePower
                       ? std::string(T("电源：应用时切高性能（需勾选 + 管理员）",
                                       "Power: switch to High performance (needs checkbox + admin)"))
                       : std::string(T("电源：不改动", "Power: unchanged"));
}
void RefreshPresetSummary() {
    AppCore* core = CoreForQuery();
    if (core != nullptr) {
        g_g.preset = core->ResolvedPreset(CurrentGameId());
        const LicenseInfo li = core->License();
        g_g.edition = li.edition.empty() ? T("免费版", "Free") : li.edition;
    }
    g_g.presetDesc = g_g.preset.description;
    BuildPresetLines();
    if (g_g.panel != nullptr) InvalidateRect(g_g.panel, nullptr, FALSE);
}

// ---------- 应用优化（工作线程 + PostMessage 回 UI） ----------
void StartApply() {
    if (g_g.busy) {
        SetBanner(T("上一次操作仍在进行中，请稍候…", "Previous operation still running…"), UiTone::Warning);
        return;
    }
    const std::string path = PageGetTextUtf8(g_g.edtPath);
    const std::string args = PageGetTextUtf8(g_g.edtArgs);
    const Validation v = ValidateLaunch(path, args);
    if (!v.ok) {
        SetBanner(std::string(T("校验失败：", "Validation failed: ")) + v.msg, UiTone::Danger);
        Log(std::string(T("[校验失败] ", "[validation failed] ")) + v.msg + "\n");
        return;
    }
    // 所见即所应用：先把表单落盘到 GameConfig（OptimizeForGame 会读它决定帧延迟/工作集/电源）
    GameLaunchConfig gc;
    CollectFormIntoConfig(&gc);
    const bool saved = GameConfig::Set(CurrentGameId(), gc);
    g_g.dirty = false;
    const GameId id = CurrentGameId();
    AppConfig cfg;
    cfg.gameExeOverride = gc.exePath;
    cfg.allowPowerSchemeSwitch = gc.powerScheme;
    cfg.autoRollbackOnUnstable = true;
    PageSharedFlags().allowPowerSchemeSwitch = gc.powerScheme;

    const HWND target = g_g.panel;
    g_g.busy = true;
    g_g.flowActive = true;
    g_g.flowProgress = 0.0;
    g_g.flowStep = 0;
    g_g.flowTotal = 0;
    g_g.flowFail = false;
    g_g.flowRows.clear();
    g_g.flowLabel = T("已提交后台线程…", "dispatched to worker thread…");
    EnableWindow(g_g.btnApply, FALSE);
    EnableWindow(g_g.btnSave, FALSE);
    EnableWindow(g_g.btnRollback, FALSE);
    SetBanner(std::string(T("应用优化：后台执行中（界面不阻塞）", "Apply running in background (UI responsive)")),
              UiTone::Info);
    Log(std::string(T("== 应用优化 ", "== Apply ")) + GameIdToString(id) + " ==\n");
    if (!v.msg.empty()) Log(std::string(T("  ", "  ")) + v.msg + "\n");
    Log(saved ? std::string(T("  启动配置已保存。\n", "  launch config saved.\n"))
              : std::string(T("  启动配置保存失败（仅本次生效）。\n", "  launch config save failed (this run only).\n")));

    std::thread([cfg, id, target]() {
        AppCore* core = new AppCore(cfg);
        const std::string result = core->OptimizeForGame(
            id,
            [target](const AppCore::FlowEvent& e) {
                if (target != nullptr && IsWindow(target))
                    PostMessageW(target, kMsgFlow, 0, reinterpret_cast<LPARAM>(new FlowPayload{e}));
            },
            0);
        delete core;
        if (target != nullptr && IsWindow(target))
            PostMessageW(target, kMsgApply, 0, reinterpret_cast<LPARAM>(new TextPayload{result}));
    }).detach();
}

// ---------- 回滚最近一次（工作线程 + PostMessage 回 UI） ----------
void StartRollback() {
    if (g_g.busy) {
        SetBanner(T("上一次操作仍在进行中，请稍候…", "Previous operation still running…"), UiTone::Warning);
        return;
    }
    AppCore* q = CoreForQuery();
    g_g.rollbackBefore = (q != nullptr) ? static_cast<int>(q->Savepoints().size()) : 0;
    if (g_g.rollbackBefore == 0) {
        SetBanner(T("没有可回滚的快照（快照历史为空）", "Nothing to roll back (history empty)"), UiTone::Warning);
        Log(std::string(T("回滚被忽略：快照历史为空。\n", "Rollback skipped: no savepoints.\n")));
        return;
    }
    const HWND target = g_g.panel;
    g_g.busy = true;
    EnableWindow(g_g.btnApply, FALSE);
    EnableWindow(g_g.btnSave, FALSE);
    EnableWindow(g_g.btnRollback, FALSE);
    SetBanner(T("正在后台回滚最近一次优化（界面不阻塞）…",
                "Rolling back the last optimization in background (UI responsive)…"),
              UiTone::Info);
    Log(std::string(T("== 回滚最近一次优化 ==\n", "== Rollback last optimization ==\n")));
    std::thread([target]() {
        AppCore* core = new AppCore();
        const std::string result = core->Rollback();
        delete core;
        if (target != nullptr && IsWindow(target))
            PostMessageW(target, kMsgRollback, 0, reinterpret_cast<LPARAM>(new TextPayload{result}));
    }).detach();
}

// ---------- 浏览 exe ----------
void BrowseExe() {
    OPENFILENAMEW ofn{};
    wchar_t file[MAX_PATH] = {};
    const std::string cur = PageGetTextUtf8(g_g.edtPath);
    if (!cur.empty()) lstrcpynW(file, PageToWide(cur).c_str(), MAX_PATH);
    ofn.lStructSize = sizeof(ofn);
    ofn.hwndOwner = g_g.panel;
    ofn.lpstrFilter = L"可执行文件 (*.exe)\0*.exe\0所有文件 (*.*)\0*.*\0";
    ofn.lpstrFile = file;
    ofn.nMaxFile = MAX_PATH;
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;
    if (GetOpenFileNameW(&ofn)) {
        PageSetTextUtf8(g_g.edtPath, PageFromWide(file));
        g_g.dirty = true;
        const Validation v = ValidateLaunch(PageGetTextUtf8(g_g.edtPath), PageGetTextUtf8(g_g.edtArgs));
        SetBanner(std::string(T("已选择路径：", "Path selected: ")) + v.msg,
                  v.ok ? UiTone::Success : UiTone::Danger);
        Log(std::string(T("已选择代启动路径：", "Launch path selected: ")) + PageGetTextUtf8(g_g.edtPath) + "\n");
        InvalidateRect(g_g.panel, nullptr, FALSE);
    } else {
        SetBanner(T("已取消选择（路径未改变）", "Browse cancelled (path unchanged)"), UiTone::Neutral);
    }
}

// ---------- 保存设置 ----------
void SaveSettings() {
    const Validation v = ValidateLaunch(PageGetTextUtf8(g_g.edtPath), PageGetTextUtf8(g_g.edtArgs));
    if (!v.ok) {
        SetBanner(std::string(T("未保存：", "Not saved: ")) + v.msg, UiTone::Danger);
        Log(std::string(T("[保存被拒绝] ", "[save rejected] ")) + v.msg + "\n");
        return;
    }
    GameLaunchConfig gc;
    CollectFormIntoConfig(&gc);
    // 防止误清空：表单路径为空时保留已存在的非空路径（用户误点「保存」不应丢配置）
    if (gc.exePath.empty()) {
        const GameLaunchConfig prev = GameConfig::Get(CurrentGameId());
        if (!prev.exePath.empty()) {
            gc.exePath = prev.exePath;
            Log(std::string(T("[提示] 路径为空，已保留原有路径：", "[note] empty path kept: ")) + prev.exePath + "\n");
        }
    }
    const bool ok = GameConfig::Set(CurrentGameId(), gc);
    g_g.dirty = false;
    PageSharedFlags().allowPowerSchemeSwitch = gc.powerScheme;
    const std::string msg = std::string(T("已保存 ", "Saved ")) + GameIdToString(CurrentGameId()) +
                            T(" 的优化启动配置", " launch config") +
                            (ok ? "" : T("（写入失败，可能无权限）", " (write failed)"));
    SetBanner(msg, ok ? UiTone::Success : UiTone::Warning);
    Log(msg + "\n");
}

// ---------- 绘制 ----------
void GamePaint(HWND h) {
    PAINTSTRUCT ps{};
    HDC dc = BeginPaint(h, &ps);
    RECT rc{};
    GetClientRect(h, &rc);
    UIPaintBufferBegin(g_g.buf, dc, rc);
    HDC d = (g_g.buf.ready && g_g.buf.dc != nullptr) ? g_g.buf.dc : dc;
    HBRUSH page = CreateSolidBrush(UiColor(UiColorRole::WindowBg));
    if (page != nullptr) { FillRect(d, &rc, page); DeleteObject(page); }

    const GameLayout& L = g_g.L;
    // ---- 左卡：代启动配置（卡片底 + 标题 + 表单标签；输入控件是子窗口，自绘不覆盖） ----
    if (!PageRectEmpty(L.cardLaunch)) {
        UiDrawCard(d, L.cardLaunch);
        UiDrawCardTitle(d, L.launchTitle, T("代启动配置", "Launch configuration"));
        UiDrawTextClamped(d, T("游戏", "Game"), L.lblGame, DT_LEFT | DT_VCENTER,
                          UiColor(UiColorRole::TextSecondary), UiFontRole::Body);
        UiDrawTextClamped(d, T("路径", "Path"), L.lblPath, DT_LEFT | DT_VCENTER,
                          UiColor(UiColorRole::TextSecondary), UiFontRole::Body);
        UiDrawTextClamped(d, T("参数", "Args"), L.lblArgs, DT_LEFT | DT_VCENTER,
                          UiColor(UiColorRole::TextSecondary), UiFontRole::Body);
    }
    // ---- 右卡：预设摘要 ----
    if (!PageRectEmpty(L.cardPreset)) {
        UiDrawCard(d, L.cardPreset);
        UiDrawCardTitle(d, L.presetTitle, T("预设摘要（按硬件降级后的真实参数）",
                                            "Preset summary (real params after HW degradation)"));
        if (!PageRectEmpty(L.presetBadge)) {
            const std::string badge = g_g.edition.empty() ? std::string(T("免费版", "Free"))
                                                          : (g_g.edition + T(" · 全部功能免费",
                                                                             " · all features free"));
            const int w = UiBadgeWidth(d, badge);
            int x = L.presetBadge.right - w;
            if (x < L.presetBadge.left) x = L.presetBadge.left;
            UiDrawBadge(d, x, L.presetBadge.top, badge, UiTone::Accent);
        }
        if (!PageRectEmpty(L.presetDesc)) {
            const std::string desc = g_g.presetDesc.empty()
                                         ? std::string(T("（该游戏预设无额外说明）", "(no description)"))
                                         : g_g.presetDesc;
            UiDrawTextWrap(d, desc, L.presetDesc, UiColor(UiColorRole::TextSecondary), UiFontRole::Body);
        }
        for (int i = 0; i < 4; ++i) {
            if (PageRectEmpty(L.presetParam[i])) continue;
            UiDrawTextClamped(d, g_g.param[i], L.presetParam[i], DT_LEFT | DT_VCENTER,
                              UiColor(UiColorRole::TextPrimary), UiFontRole::Caption);
        }
    }
    // ---- 底部：进度条 / 步骤文案 / 状态行 / 步骤明细 ----
    if (!PageRectEmpty(L.progress)) {
        const bool active = g_g.flowActive;
        UiDrawProgress(d, L.progress, active ? g_g.flowProgress : 0.0,
                       active ? (g_g.flowFail ? UiTone::Warning : UiTone::Accent) : UiTone::Neutral);
    }
    if (!PageRectEmpty(L.progressLabel)) {
        std::string label;
        if (g_g.flowActive) {
            label = std::string(T("第 ", "step ")) + std::to_string(g_g.flowStep) + "/" +
                    std::to_string(g_g.flowTotal) + " · " + g_g.flowLabel;
        } else {
            label = T("未执行优化（点「应用优化」开始）", "idle (press Apply to start)");
        }
        UiDrawTextClamped(d, label, L.progressLabel, DT_LEFT | DT_VCENTER,
                          UiColor(UiColorRole::TextSecondary), UiFontRole::Caption);
    }
    if (!PageRectEmpty(L.statusLine))
        PageDrawStatusLine(d, L.statusLine, g_g.statusTone, g_g.status, g_g.m, UiFontRole::Caption);
    for (int i = 0; i < 3; ++i) {
        if (PageRectEmpty(L.flowLine[i])) continue;
        std::string line;
        if (i < static_cast<int>(g_g.flowRows.size())) {
            const FlowRow& r = g_g.flowRows[static_cast<size_t>(i)];
            line = std::to_string(i + 1) + ") " + r.label + " · " +
                   (r.ok ? T("成功", "OK") : (r.fail ? T("失败", "FAIL") : T("等待", "pending")));
            if (r.ok || r.fail) line += "（" + std::to_string(r.elapsedMs) + " ms）";
        }
        if (line.empty()) continue;
        const COLORREF c = (i < static_cast<int>(g_g.flowRows.size()) &&
                            g_g.flowRows[static_cast<size_t>(i)].fail)
                               ? UiColor(UiColorRole::Danger)
                               : UiColor(UiColorRole::TextMuted);
        UiDrawTextClamped(d, line, L.flowLine[i], DT_LEFT | DT_VCENTER, c, UiFontRole::Caption);
    }
    UIPaintBufferEnd(g_g.buf);
    EndPaint(h, &ps);
}

bool GameDrawItem(const DRAWITEMSTRUCT* di) {
    if (di == nullptr || di->CtlType != ODT_BUTTON) return false;
    const int id = static_cast<int>(di->CtlID);
    if (id < kGameCtlFirst || id > kGameCtlLast) return false;
    std::string label;
    bool primary = false;
    switch (id) {
        case IDC_GAME_APPLY:    label = T("应用优化", "Apply"); primary = true; break;
        case IDC_GAME_SAVE:     label = T("保存游戏设置", "Save Settings"); break;
        case IDC_GAME_ROLLBACK: label = T("回滚最近一次", "Rollback Last"); break;
        case IDC_GAME_BROWSE:   label = T("浏览…", "Browse…"); break;
        default: return false;
    }
    return UiDrawButton(di->hDC, di->rcItem, PageButtonStateFromDrawItem(di), label, primary);
}

// ---------- 控件/布局 ----------
HWND MakeCtl(HWND parent, const wchar_t* cls, DWORD style, DWORD exStyle, const RECT& rc, int id,
             UiFontRole fontRole) {
    HINSTANCE hi = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));
    HWND h = CreateWindowExW(exStyle, cls, L"", style | WS_CHILD | WS_VISIBLE, rc.left, rc.top,
                             PageMaxI(0, rc.right - rc.left), PageMaxI(0, rc.bottom - rc.top), parent,
                             reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), hi, nullptr);
    if (h != nullptr) {
        HFONT f = UiFont(fontRole);
        if (f != nullptr) SendMessageW(h, WM_SETFONT, reinterpret_cast<WPARAM>(f), TRUE);
    }
    return h;
}
void ApplyRect(HWND h, const RECT& rc) {
    if (h == nullptr) return;
    if (PageRectEmpty(rc)) { ShowWindow(h, SW_HIDE); return; }
    ShowWindow(h, SW_SHOWNA);
    SetWindowPos(h, nullptr, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top,
                 SWP_NOZORDER | SWP_NOACTIVATE);
}
void LayoutControls() {
    const GameLayout& L = g_g.L;
    ApplyRect(g_g.cmbGame, L.cmbGame);
    ApplyRect(g_g.edtPath, L.edtPath);
    ApplyRect(g_g.btnBrowse, L.btnBrowse);
    ApplyRect(g_g.edtArgs, L.edtArgs);
    ApplyRect(g_g.chkPower, L.chkPower);
    ApplyRect(g_g.chkFrame, L.chkFrame);
    ApplyRect(g_g.chkWork, L.chkWork);
    ApplyRect(g_g.btnApply, L.btnApply);
    ApplyRect(g_g.btnSave, L.btnSave);
    ApplyRect(g_g.btnRollback, L.btnRollback);
}
void Relayout() {
    if (g_g.panel == nullptr || g_g.container == nullptr) return;
    RECT cr{};
    GetClientRect(g_g.container, &cr);
    const int w = PageMaxI(0, cr.right - cr.left);
    const int h = PageMaxI(0, cr.bottom - cr.top);
    if (w > 0 && h > 0) {
        RECT pr{};
        GetWindowRect(g_g.panel, &pr);
        if (pr.right - pr.left != w || pr.bottom - pr.top != h)
            SetWindowPos(g_g.panel, nullptr, 0, 0, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
    g_g.m = MakePageMetrics();
    g_g.L = ComputeGameLayout(w, h, g_g.m);
    std::string detail;
    if (!GameLayoutSelfCheck(g_g.L, &detail))
        Log(std::string(T("[游戏优化页布局自检] 失败：", "[game page layout self-check] FAIL: ")) + detail + "\n");
    LayoutControls();
    InvalidateRect(g_g.panel, nullptr, TRUE);
}

// ---------- 命令分发 ----------
void DispatchCommand(int id, int code) {
    switch (id) {
        case IDC_GAME_COMBO:
            if (code == CBN_SELCHANGE) {
                LoadConfigToUI();
                RefreshPresetSummary();
                const GameLaunchConfig gc = GameConfig::Get(CurrentGameId());
                SetBanner(std::string(T("已选择：", "Selected: ")) + GameNameLocalized(CurrentGameId()) +
                              (gc.exePath.empty() ? T("（未设置代启动路径）", " (no launch path set)")
                                                  : T("（已载入保存的启动配置）", " (saved config loaded)")),
                          UiTone::Accent);
                InvalidateRect(g_g.panel, nullptr, FALSE);
            }
            return;
        case IDC_GAME_BROWSE:
            if (code == BN_CLICKED) BrowseExe();
            return;
        case IDC_GAME_SAVE:
            if (code == BN_CLICKED) SaveSettings();
            return;
        case IDC_GAME_APPLY:
            if (code == BN_CLICKED) StartApply();
            return;
        case IDC_GAME_ROLLBACK:
            if (code == BN_CLICKED) StartRollback();
            return;
        case IDC_GAME_POWERCHK:
        case IDC_GAME_FRAMECHK:
        case IDC_GAME_WORKCHK:
            if (code == BN_CLICKED) {
                const bool power = SendMessageW(g_g.chkPower, BM_GETCHECK, 0, 0) == BST_CHECKED;
                const bool frame = SendMessageW(g_g.chkFrame, BM_GETCHECK, 0, 0) == BST_CHECKED;
                const bool work = SendMessageW(g_g.chkWork, BM_GETCHECK, 0, 0) == BST_CHECKED;
                PageSharedFlags().allowPowerSchemeSwitch = power;  // 总览页一键优化同步生效
                g_g.dirty = true;
                const std::string msg =
                    std::string(T("开关已更新：电源 ", "Flags: power ")) + (power ? "ON" : "OFF") +
                    T(" / 帧延迟 ", " / frame latency ") + (frame ? "ON" : "OFF") +
                    T(" / 工作集 ", " / working set ") + (work ? "ON" : "OFF") +
                    T("（「保存游戏设置」落盘，「应用优化」会自动先保存）",
                      " (Save persists; Apply saves first automatically)");
                SetBanner(msg, UiTone::Info);
                Log(msg + "\n");
                RefreshPresetSummary();
            }
            return;
        case IDC_GAME_PATH:
        case IDC_GAME_ARGS:
            if (code == EN_CHANGE && !g_g.busy) {
                g_g.dirty = true;
                const Validation v = ValidateLaunch(PageGetTextUtf8(g_g.edtPath), PageGetTextUtf8(g_g.edtArgs));
                SetBanner(std::string(T("输入提示：", "Input hint: ")) + v.msg,
                          v.ok ? (v.warn ? UiTone::Warning : UiTone::Accent) : UiTone::Danger);
            }
            return;
        default:
            return;
    }
}

// ---------- 面板窗口过程 ----------
LRESULT CALLBACK GamePanelProc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_ERASEBKGND:
            return 1;
        case WM_PAINT:
            GamePaint(h);
            return 0;
        case WM_SIZE:
            Relayout();
            return 0;
        case WM_TIMER:
            if (wp == kTimerLive) {
                Relayout();  // 自愈：宿主 WM_SIZE 未调用时 1 秒内补齐
                if (IsWindowVisible(h)) InvalidateRect(h, nullptr, FALSE);
            }
            return 0;
        case WM_DRAWITEM:
            if (GameDrawItem(reinterpret_cast<const DRAWITEMSTRUCT*>(lp))) return TRUE;
            break;
        case WM_COMMAND: {
            const int id = LOWORD(wp);
            const int code = HIWORD(wp);
            g_routeGuard.valid = false;    // 新消息：先清上一条的幂等守卫
            GamePageCommand(id, code);     // 就地分发（自包含：按钮绝不会是死代码）
            if (id >= kGameCtlFirst && id <= kGameCtlLast) {
                g_routeGuard.valid = true;  // 同一条消息若被宿主再路由 → 拦下
                g_routeGuard.id = id;
                g_routeGuard.code = code;
            }
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
            if (g_g.brSurface == nullptr) g_g.brSurface = CreateSolidBrush(UiColor(UiColorRole::Surface));
            return reinterpret_cast<LRESULT>(g_g.brSurface);
        }
        case WM_THEMECHANGED:
        case WM_SYSCOLORCHANGE:
            if (g_g.brSurface != nullptr) { DeleteObject(g_g.brSurface); g_g.brSurface = nullptr; }
            InvalidateRect(h, nullptr, TRUE);
            return 0;
        case kMsgFlow: {
            auto* p = reinterpret_cast<FlowPayload*>(lp);
            if (p != nullptr) {
                const AppCore::FlowEvent& e = p->e;
                if (e.kind == AppCore::FlowEvent::Info) {
                    if (!e.text.empty()) Log(e.text + "\n");
                } else if (e.kind == AppCore::FlowEvent::StepStart) {
                    g_g.flowActive = true;
                    g_g.flowStep = e.step;
                    g_g.flowTotal = e.total;
                    g_g.flowLabel = e.text;
                    if (static_cast<int>(g_g.flowRows.size()) < e.step) g_g.flowRows.resize(e.step);
                    g_g.flowRows[e.step - 1].label = e.text;
                    Log("  " + std::to_string(e.step) + ") " + e.text + ": " + T("处理中...", "running...") + "\n");
                } else {
                    const bool ok = (e.kind == AppCore::FlowEvent::StepOk);
                    if (!ok) g_g.flowFail = true;
                    if (e.step >= 1 && e.step <= static_cast<int>(g_g.flowRows.size())) {
                        FlowRow& r = g_g.flowRows[e.step - 1];
                        r.ok = ok;
                        r.fail = !ok;
                        r.elapsedMs = e.elapsedMs;
                    }
                    g_g.flowLabel = e.text;
                    Log("  " + std::to_string(e.step) + ") " + e.text + ": " +
                        (ok ? T("成功", "OK") : T("失败", "FAIL")) + "（" + std::to_string(e.elapsedMs) +
                        " ms）\n");
                }
                if (e.total > 0) g_g.flowProgress = static_cast<double>(e.step) / e.total;
                delete p;
                InvalidateRect(h, nullptr, FALSE);
            }
            return 0;
        }
        case kMsgApply: {
            auto* p = reinterpret_cast<TextPayload*>(lp);
            if (p != nullptr) {
                g_g.busy = false;
                g_g.flowActive = false;
                g_g.flowProgress = 0.0;
                EnableWindow(g_g.btnApply, TRUE);
                EnableWindow(g_g.btnSave, TRUE);
                EnableWindow(g_g.btnRollback, TRUE);
                Log("\n" + p->text + "\n\n");
                delete p;
                SetBanner(g_g.flowFail
                              ? T("应用优化结束（存在失败项，可回滚最近一次）",
                                  "Apply finished with failures (rollback available)")
                              : T("应用优化完成（详情见日志，可回滚最近一次）",
                                  "Apply finished (see log; rollback available)"),
                          g_g.flowFail ? UiTone::Warning : UiTone::Success);
                RefreshPresetSummary();
                InvalidateRect(h, nullptr, FALSE);
            }
            return 0;
        }
        case kMsgRollback: {
            auto* p = reinterpret_cast<TextPayload*>(lp);
            if (p != nullptr) {
                g_g.busy = false;
                EnableWindow(g_g.btnApply, TRUE);
                EnableWindow(g_g.btnSave, TRUE);
                EnableWindow(g_g.btnRollback, TRUE);
                Log("\n" + p->text + "\n\n");
                delete p;
                int after = g_g.rollbackBefore;
                AppCore* q = CoreForQuery();
                if (q != nullptr) after = static_cast<int>(q->Savepoints().size());
                const bool changed = after < g_g.rollbackBefore;
                SetBanner(std::string(T("回滚", "Rollback ")) +
                              (changed ? T("已生效", "applied") : T("未改变快照历史", "did not change history")) +
                              T("：历史 ", ": history ") + std::to_string(g_g.rollbackBefore) + " → " +
                              std::to_string(after) + T(" 条（详情见日志）", " entries (see log)"),
                          changed ? UiTone::Success : UiTone::Danger);
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
    if (g_g.panel == nullptr) return;
    MSG msg{};
    while (PeekMessageW(&msg, g_g.panel, kMsgFlow, kMsgApply, PM_REMOVE)) {
        if (msg.message == kMsgFlow) delete reinterpret_cast<FlowPayload*>(msg.lParam);
        else delete reinterpret_cast<TextPayload*>(msg.lParam);
    }
    while (PeekMessageW(&msg, g_g.panel, kMsgRollback, kMsgRollback, PM_REMOVE))
        delete reinterpret_cast<TextPayload*>(msg.lParam);
}

}  // namespace

// =============================================================================
// 布局：纯整数（可脱离窗口/DC 单测）
// =============================================================================
GameLayout ComputeGameLayout(int w, int h, const PageMetrics& m) {
    GameLayout L;
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

    // ---- 紧凑模式：内容区 < 640x300 逻辑时，表单与动作按钮优先，状态行移入预设卡底部 ----
    const bool compact = (w < m.px(640) || h < m.px(300));
    L.compact = compact;
    if (compact) {
        L.valid = false;
        if (L.note.empty()) L.note = "compact: status line moved into preset card";
    }

    // ---- 左右两卡（左：表单；右：预设摘要） ----
    int leftW = PageClampI(innerW * 52 / 100, m.px(300), m.px(620));
    int rightW = innerW - gap - leftW;
    if (rightW < m.px(240)) {
        leftW = PageMaxI(m.px(200), innerW - gap - m.px(240));
        rightW = PageMaxI(0, innerW - gap - leftW);
        L.valid = false;
        L.note = "narrow: preset column compressed";
    }
    // ---- 上下两行（上：两卡；下：动作区） ----
    const int rowH = compact ? m.height(1) : m.height(2);   // 紧凑：32，否则 40
    const int progH = m.px(22);
    const int statusH = m.px(22);
    // coreB = 底部必须保住的固定高度：宽松模式含「进度行 + 状态行」，紧凑模式只保动作按钮
    //（紧凑模式的状态行住在预设卡里，进度条与步骤明细改为按剩余空间机会式显示）
    const int coreB = compact ? (rowH + in8) : (rowH + in8 + progH + in4 + statusH);
    int hA = PageClampI(innerH * (compact ? 64 : 55) / 100, m.px(96), m.px(230));  // 上排：两卡
    if (innerH - hA - gap < coreB) hA = innerH - gap - coreB;
    if (hA < m.px(80)) {
        hA = PageClampI(innerH * 55 / 100, m.px(56), m.px(96));
        L.valid = false;
        L.note = "short: form rows compressed";
    }
    int hB = innerH - hA - gap;
    if (hB < 0) hB = 0;

    PageSetRect(&L.cardLaunch, pad, pad, pad + leftW, pad + hA);
    PageSetRect(&L.cardPreset, L.cardLaunch.right + gap, pad, w - pad, pad + hA);
    PageSetRect(&L.areaRun, pad, pad + hA + gap, w - pad, h - pad);
    if (rightW <= 0) {
        PageClearRect(&L.cardPreset);
        L.valid = false;
        if (L.note.empty()) L.note = "narrow: preset card hidden";
    }
    if (hB <= 0) {
        PageClearRect(&L.areaRun);
        L.valid = false;
        if (L.note.empty()) L.note = "short: action area hidden";
    }

    // ---- 左卡：表单（标题 → 游戏 → 路径+浏览 → 参数 → 三个开关） ----
    if (!PageRectEmpty(L.cardLaunch)) {
        const RECT C = L.cardLaunch;
        const int titleH = compact ? m.px(18) : m.px(20);
        PageSetRect(&L.launchTitle, C.left + in8, C.top + in8, C.right - in8, C.top + in8 + titleH);
        const int labelW = m.px(48);
        const int rowH2 = compact ? m.px(22) : m.height(0);  // 表单行高（Sm 令牌 / 紧凑 22）
        int y = C.top + in8 + titleH + m.px(2);
        const int btnW = m.px(64);
        // 行1：游戏
        PageSetRect(&L.lblGame, C.left + in8, y, C.left + in8 + labelW, y + rowH2);
        PageSetRect(&L.cmbGame, L.lblGame.right, y, C.right - in8, y + rowH2);
        // 行2：路径 + 浏览…
        y += rowH2 + in4;
        PageSetRect(&L.lblPath, C.left + in8, y, C.left + in8 + labelW, y + rowH2);
        PageSetRect(&L.edtPath, L.lblPath.right, y, C.right - in8 - btnW - in4, y + rowH2);
        PageSetRect(&L.btnBrowse, C.right - in8 - btnW, y, C.right - in8, y + rowH2);
        // 行3：参数
        y += rowH2 + in4;
        PageSetRect(&L.lblArgs, C.left + in8, y, C.left + in8 + labelW, y + rowH2);
        PageSetRect(&L.edtArgs, L.lblArgs.right, y, C.right - in8, y + rowH2);
        // 行4：三个开关（均分）
        y += rowH2 + in4;
        const int innerL = C.left + in8;
        const int innerR = C.right - in8;
        const int third = PageMaxI(m.px(80), (innerR - innerL) / 3);
        const int limit = C.bottom - in8;
        PageSetRect(&L.chkPower, innerL, y, PageMinI(innerL + third - in4, innerR), y + rowH2);
        PageSetRect(&L.chkFrame, L.chkPower.right + in4, y,
                    PageMinI(L.chkPower.right + in4 + third - in4, innerR), y + rowH2);
        PageSetRect(&L.chkWork, L.chkFrame.right + in4, y, innerR, y + rowH2);
        // 空间不足：整行退场（不重叠、不裁切；缺省值仍保留在配置里）
        if (L.chkWork.bottom > limit || (L.chkWork.right - L.chkWork.left) < m.px(48)) {
            PageClearRect(&L.chkPower);
            PageClearRect(&L.chkFrame);
            PageClearRect(&L.chkWork);
            L.valid = false;
            if (L.note.empty()) L.note = "short: option checkboxes hidden";
        }
        if (L.edtArgs.bottom > limit) {
            PageClearRect(&L.lblArgs);
            PageClearRect(&L.edtArgs);
            L.valid = false;
            if (L.note.empty()) L.note = "short: args row hidden";
        }
        if (L.edtPath.bottom > limit) {
            PageClearRect(&L.lblPath);
            PageClearRect(&L.edtPath);
            PageClearRect(&L.btnBrowse);
            L.valid = false;
            if (L.note.empty()) L.note = "short: path row hidden";
        }
        if (L.cmbGame.bottom > limit) {
            PageClearRect(&L.lblGame);
            PageClearRect(&L.cmbGame);
            L.valid = false;
            if (L.note.empty()) L.note = "short: game selector hidden";
        }
    }

    // ---- 右卡：预设摘要（标题 + 徽标 + 说明 + 参数 + 紧凑模式下的状态行） ----
    if (!PageRectEmpty(L.cardPreset)) {
        const RECT C = L.cardPreset;
        const int badgeW = m.px(96);
        const int titleH = compact ? m.px(18) : m.px(20);
        // 徽标高度 = UiControlH::Sm(24)：说明文字从其下方开始，避免与徽标相压
        PageSetRect(&L.presetTitle, C.left + in8, C.top + in8, C.right - in8 - badgeW,
                    C.top + in8 + titleH);
        PageSetRect(&L.presetBadge, C.right - in8 - badgeW, C.top + in8, C.right - in8,
                    C.top + in8 + m.px(24));
        // 说明文字起点 = 标题行与徽标（24 高）二者更低者的下方 + 4：不可能与徽标相压
        const int descTop = PageMaxI(C.top + in8 + titleH, C.top + in8 + m.px(24)) + m.px(4);
        int descH = compact ? m.px(26) : m.px(34);
        if (compact) {  // 紧凑：底部状态行放在预设卡最后一行（保证可见反馈不丢）
            PageSetRect(&L.statusLine, C.left + in8, C.bottom - in8 - statusH, C.right - in8,
                        C.bottom - in8);
            const int room = L.statusLine.top - in4 - descTop;  // 说明最多占到这里
            if (descH > room) descH = room;
        }
        if (descH < m.px(14)) {
            PageClearRect(&L.presetDesc);
            L.valid = false;
            if (L.note.empty()) L.note = "short: preset description hidden";
        } else {
            PageSetRect(&L.presetDesc, C.left + in8, descTop, C.right - in8, descTop + descH);
        }
        const int limit = compact ? (L.statusLine.top - in4) : (C.bottom - in8);
        int y = descTop + descH + m.px(4);
        for (int i = 0; i < 4; ++i) {
            if (y + m.px(18) > limit) { PageClearRect(&L.presetParam[i]); continue; }
            PageSetRect(&L.presetParam[i], C.left + in8, y, C.right - in8, y + m.px(18));
            y += m.px(18);
        }
        if (PageRectEmpty(L.presetParam[0])) {
            L.valid = false;
            if (L.note.empty()) L.note = "short: preset params hidden";
        }
    }

    // ---- 底部动作区（应用/保存/回滚 + 进度 + 状态行 + 步骤明细） ----
    if (!PageRectEmpty(L.areaRun)) {
        const RECT A = L.areaRun;
        PageSetRect(&L.btnApply, A.left, A.top, A.left + m.px(168), A.top + rowH);
        PageSetRect(&L.btnSave, L.btnApply.right + in8, A.top + in4, L.btnApply.right + in8 + m.px(132),
                    A.top + in4 + m.px(32));
        PageSetRect(&L.btnRollback, L.btnSave.right + in8, A.top, L.btnSave.right + in8 + m.px(140),
                    A.top + rowH);
        const int progTop = A.top + rowH + in8;
        const int progW = (A.right - A.left) * 66 / 100;
        PageSetRect(&L.progress, A.left, progTop + m.px(6), A.left + progW - in8, progTop + m.px(16));
        PageSetRect(&L.progressLabel, L.progress.right + in8, progTop, A.right, progTop + progH);
        const int statusTop = progTop + m.px(26);
        if (!compact) PageSetRect(&L.statusLine, A.left, statusTop, A.right, statusTop + statusH);
        // 步骤明细：状态行下方还有空间时逐条显示（宽松模式 0~3 行；紧凑模式进度条下方）
        const int flowTop = compact ? (progTop + progH + in4) : (statusTop + statusH + in4);
        const int avail = (A.bottom - in4) - flowTop;
        const int lines = PageClampI(avail / m.px(16), 0, 3);
        for (int i = 0; i < lines; ++i) {
            PageSetRect(&L.flowLine[i], A.left, flowTop + i * m.px(16), A.right,
                        flowTop + i * m.px(16) + m.px(16));
        }
        if (hB < rowH + in8 + progH) {
            PageClearRect(&L.progress);
            PageClearRect(&L.progressLabel);
            L.valid = false;
            if (L.note.empty()) L.note = "short: progress row hidden";
        }
        if (!compact && hB < coreB) {
            PageClearRect(&L.statusLine);
            L.valid = false;
            if (L.note.empty()) L.note = "short: page status line hidden";
        }
        if (compact && hB < rowH) {
            L.valid = false;
            if (L.note.empty()) L.note = "short: action buttons may be clipped";
        }
    }

    // 归一化：任何「不在所属容器内」的非空矩形一律清空 —— 与 GameLayoutSelfCheck 的
    // 包含性判据同源，保证「容器退场时其内部矩形一并退场」，且自检恒真。
    struct Fit {
        RECT* r;
        const RECT* container;
    };
    const Fit fits[] = {
        {&L.cardLaunch, &L.panel},   {&L.cardPreset, &L.panel},
        {&L.areaRun, &L.panel},      {&L.launchTitle, &L.cardLaunch},
        {&L.lblGame, &L.cardLaunch}, {&L.cmbGame, &L.cardLaunch},
        {&L.lblPath, &L.cardLaunch}, {&L.edtPath, &L.cardLaunch},
        {&L.btnBrowse, &L.cardLaunch}, {&L.lblArgs, &L.cardLaunch},
        {&L.edtArgs, &L.cardLaunch}, {&L.chkPower, &L.cardLaunch},
        {&L.chkFrame, &L.cardLaunch}, {&L.chkWork, &L.cardLaunch},
        {&L.presetTitle, &L.cardPreset}, {&L.presetBadge, &L.cardPreset},
        {&L.presetDesc, &L.cardPreset},  {&L.presetParam[0], &L.cardPreset},
        {&L.presetParam[1], &L.cardPreset}, {&L.presetParam[2], &L.cardPreset},
        {&L.presetParam[3], &L.cardPreset}, {&L.btnApply, &L.areaRun},
        {&L.btnSave, &L.areaRun},    {&L.btnRollback, &L.areaRun},
        {&L.progress, &L.areaRun},   {&L.progressLabel, &L.areaRun},
        {&L.statusLine, compact ? &L.cardPreset : &L.areaRun}, {&L.flowLine[0], &L.areaRun},
        {&L.flowLine[1], &L.areaRun}, {&L.flowLine[2], &L.areaRun},
    };
    for (size_t i = 0; i < sizeof(fits) / sizeof(fits[0]); ++i) {
        if (!PageRectEmpty(*fits[i].r) && !PageRectInside(*fits[i].r, *fits[i].container))
            PageClearRect(fits[i].r);
    }

    if (L.note.empty() && L.valid) L.note = "ok";
    return L;
}

bool GameLayoutSelfCheck(const GameLayout& L, std::string* detail) {
    struct Named {
        const char* name;
        const RECT* rc;
    };
    const Named cards[] = {{"cardLaunch", &L.cardLaunch}, {"cardPreset", &L.cardPreset}};
    const Named form[] = {{"lblGame", &L.lblGame},   {"cmbGame", &L.cmbGame},
                          {"lblPath", &L.lblPath},   {"edtPath", &L.edtPath},
                          {"btnBrowse", &L.btnBrowse}, {"lblArgs", &L.lblArgs},
                          {"edtArgs", &L.edtArgs},   {"chkPower", &L.chkPower},
                          {"chkFrame", &L.chkFrame}, {"chkWork", &L.chkWork}};
    const Named preset[] = {{"presetTitle", &L.presetTitle}, {"presetBadge", &L.presetBadge},
                            {"presetDesc", &L.presetDesc},   {"presetParam0", &L.presetParam[0]},
                            {"presetParam1", &L.presetParam[1]}, {"presetParam2", &L.presetParam[2]},
                            {"presetParam3", &L.presetParam[3]}};
    // 紧凑模式下状态行住在预设卡里，必须和参数行一起做重叠判定
    const Named presetCompact[] = {{"presetTitle", &L.presetTitle}, {"presetBadge", &L.presetBadge},
                                   {"presetDesc", &L.presetDesc},   {"presetParam0", &L.presetParam[0]},
                                   {"presetParam1", &L.presetParam[1]}, {"presetParam2", &L.presetParam[2]},
                                   {"presetParam3", &L.presetParam[3]}, {"statusLine", &L.statusLine}};
    const Named run[] = {{"btnApply", &L.btnApply}, {"btnSave", &L.btnSave},
                         {"btnRollback", &L.btnRollback}, {"progress", &L.progress},
                         {"progressLabel", &L.progressLabel}};
    const Named runWide[] = {{"btnApply", &L.btnApply}, {"btnSave", &L.btnSave},
                             {"btnRollback", &L.btnRollback}, {"progress", &L.progress},
                             {"progressLabel", &L.progressLabel}, {"statusLine", &L.statusLine}};

    struct Group {
        const char* title;
        const Named* items;
        int count;
        const RECT* container;
    };
    const Group groupsCompact[] = {
        {"cards", cards, 2, &L.panel},
        {"launch-form", form, 10, &L.cardLaunch},
        {"preset-card(+status)", presetCompact, 8, &L.cardPreset},
        {"run-area", run, 5, &L.areaRun},
    };
    const Group groupsWide[] = {
        {"cards", cards, 2, &L.panel},
        {"launch-form", form, 10, &L.cardLaunch},
        {"preset-card", preset, 7, &L.cardPreset},
        {"run-area(+status)", runWide, 6, &L.areaRun},
    };
    const Group* groups = L.compact ? groupsCompact : groupsWide;
    const size_t groupCount = L.compact ? (sizeof(groupsCompact) / sizeof(groupsCompact[0]))
                                        : (sizeof(groupsWide) / sizeof(groupsWide[0]));
    for (size_t g = 0; g < groupCount; ++g) {
        const Group& grp = groups[g];
        if (PageRectEmpty(*grp.container)) continue;
        for (int i = 0; i < grp.count; ++i) {
            const RECT& a = *grp.items[i].rc;
            if (PageRectEmpty(a)) continue;  // 空矩形 = 已降级隐藏
            if (!PageRectInside(a, *grp.container)) {
                if (detail != nullptr)
                    *detail = std::string("group[") + grp.title + "] " + grp.items[i].name +
                              " escapes its container";
                return false;
            }
            for (int j = i + 1; j < grp.count; ++j) {
                if (PageRectOverlap(a, *grp.items[j].rc)) {
                    if (detail != nullptr)
                        *detail = std::string("group[") + grp.title + "] overlap: " + grp.items[i].name +
                                  " vs " + grp.items[j].name;
                    return false;
                }
            }
        }
    }
    if (detail != nullptr) *detail = "ok";
    return true;
}

// =============================================================================
// 导出：游戏优化页装配接口
// =============================================================================
bool GamePageCreate(HWND pageContainer, const PageHostHooks& hooks) {
    if (pageContainer == nullptr || g_g.created) return false;
    HINSTANCE hi = reinterpret_cast<HINSTANCE>(GetModuleHandleW(nullptr));

    WNDCLASSW wc{};
    wc.lpfnWndProc = GamePanelProc;
    wc.hInstance = hi;
    wc.lpszClassName = kPanelClass;
    wc.style = CS_HREDRAW | CS_VREDRAW;
    wc.hCursor = LoadCursorW(nullptr, reinterpret_cast<LPCWSTR>(IDC_ARROW));
    wc.hbrBackground = nullptr;
    if (RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) return false;

    g_g.container = pageContainer;
    g_g.hooks = hooks;
    g_g.panel = CreateWindowExW(WS_EX_CONTROLPARENT, kPanelClass, L"",
                               WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN, 0, 0, 10, 10, pageContainer,
                               nullptr, hi, nullptr);
    if (g_g.panel == nullptr) return false;

    RECT z{0, 0, 10, 10};
    g_g.cmbGame = MakeCtl(g_g.panel, L"COMBOBOX", CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP, 0, z,
                          IDC_GAME_COMBO, UiFontRole::Body);
    g_g.edtPath = MakeCtl(g_g.panel, L"EDIT", ES_AUTOHSCROLL | WS_TABSTOP, WS_EX_CLIENTEDGE, z,
                          IDC_GAME_PATH, UiFontRole::Body);
    g_g.btnBrowse = MakeCtl(g_g.panel, L"BUTTON", BS_OWNERDRAW, 0, z, IDC_GAME_BROWSE, UiFontRole::Body);
    g_g.edtArgs = MakeCtl(g_g.panel, L"EDIT", ES_AUTOHSCROLL | WS_TABSTOP, WS_EX_CLIENTEDGE, z,
                          IDC_GAME_ARGS, UiFontRole::Body);
    g_g.chkPower = MakeCtl(g_g.panel, L"BUTTON", BS_AUTOCHECKBOX | WS_TABSTOP, 0, z, IDC_GAME_POWERCHK,
                           UiFontRole::Body);
    g_g.chkFrame = MakeCtl(g_g.panel, L"BUTTON", BS_AUTOCHECKBOX | WS_TABSTOP, 0, z, IDC_GAME_FRAMECHK,
                           UiFontRole::Body);
    g_g.chkWork = MakeCtl(g_g.panel, L"BUTTON", BS_AUTOCHECKBOX | WS_TABSTOP, 0, z, IDC_GAME_WORKCHK,
                          UiFontRole::Body);
    g_g.btnApply = MakeCtl(g_g.panel, L"BUTTON", BS_OWNERDRAW, 0, z, IDC_GAME_APPLY, UiFontRole::BodyBold);
    g_g.btnSave = MakeCtl(g_g.panel, L"BUTTON", BS_OWNERDRAW, 0, z, IDC_GAME_SAVE, UiFontRole::Body);
    g_g.btnRollback = MakeCtl(g_g.panel, L"BUTTON", BS_OWNERDRAW, 0, z, IDC_GAME_ROLLBACK,
                              UiFontRole::BodyBold);
    if (g_g.cmbGame == nullptr || g_g.edtPath == nullptr || g_g.btnBrowse == nullptr ||
        g_g.edtArgs == nullptr || g_g.chkPower == nullptr || g_g.chkFrame == nullptr ||
        g_g.chkWork == nullptr || g_g.btnApply == nullptr || g_g.btnSave == nullptr ||
        g_g.btnRollback == nullptr)
        return false;

    PageEnableButtonHover(g_g.btnApply, true);
    PageEnableButtonHover(g_g.btnSave, false);
    PageEnableButtonHover(g_g.btnRollback, false);
    PageEnableButtonHover(g_g.btnBrowse, false);

    PageSetTextUtf8(g_g.chkPower, T("启用电源方案切换", "Enable power scheme switch"));
    PageSetTextUtf8(g_g.chkFrame, T("允许驱动级帧延迟", "Allow driver frame latency"));
    PageSetTextUtf8(g_g.chkWork, T("允许工作集策略", "Allow working set policy"));

    g_g.created = true;
    g_g.m = MakePageMetrics();
    ComboRebuild(0);
    LoadConfigToUI();
    RefreshPresetSummary();
    g_g.status = T("就绪：选游戏 → 配置代启动（可留空）→ 保存 → 应用优化；随时可回滚",
                   "Ready: pick a game → set launch config (optional) → Save → Apply; rollback anytime");
    g_g.statusTone = UiTone::Accent;
    Relayout();
    SetTimer(g_g.panel, kTimerLive, 1000, nullptr);
    Log(std::string("GameOptimizer v") + GOPT_VERSION_STR +
        T("  游戏优化页就绪（选游戏 / 预设摘要 / 代启动 / 应用 / 回滚）\n",
          "  game tune page ready (select / preset / launch / apply / rollback)\n"));
    return true;
}

void GamePageDestroy() {
    if (g_g.panel != nullptr) {
        KillTimer(g_g.panel, kTimerLive);
        DrainPostedPayloads();
        DestroyWindow(g_g.panel);
        g_g.panel = nullptr;
    }
    UIPaintBufferFree(g_g.buf);
    if (g_g.brSurface != nullptr) { DeleteObject(g_g.brSurface); g_g.brSurface = nullptr; }
    if (g_g.ownCore != nullptr) { delete g_g.ownCore; g_g.ownCore = nullptr; }
    PageDisableAllButtonHover();
    g_g = GameState{};
}

void GamePageLayout() { Relayout(); }

void GamePageOnShow() {
    g_g.m = MakePageMetrics();
    RefreshPresetSummary();
    Relayout();
    SetBanner(std::string(T("游戏优化页已刷新：", "Game page refreshed: ")) +
                  GameNameLocalized(CurrentGameId()),
              UiTone::Accent);
}

bool GamePageCommand(int id, int code) {
    if (id < kGameCtlFirst || id > kGameCtlLast) return false;
    // 幂等守卫：同一条 WM_COMMAND 若被面板就地分发 + 宿主再路由一次，第二次被拦下并消费
    if (g_routeGuard.valid && g_routeGuard.id == id && g_routeGuard.code == code) {
        g_routeGuard.valid = false;
        return true;
    }
    DispatchCommand(id, code);
    return true;
}

void GamePageApplyLanguage() {
    const int keep = CurrentGameIndex();
    ComboRebuild(keep);  // 游戏名按语言重建（保持选中项）
    PageSetTextUtf8(g_g.chkPower, T("启用电源方案切换", "Enable power scheme switch"));
    PageSetTextUtf8(g_g.chkFrame, T("允许驱动级帧延迟", "Allow driver frame latency"));
    PageSetTextUtf8(g_g.chkWork, T("允许工作集策略", "Allow working set policy"));
    RefreshPresetSummary();
    if (g_g.panel != nullptr) InvalidateRect(g_g.panel, nullptr, TRUE);
    SetBanner(T("语言已切换：游戏优化页文案已同步", "Language switched: game page text synced"),
              UiTone::Accent);
}

void GamePageApplyTheme() {
    g_g.m = MakePageMetrics();
    if (g_g.brSurface != nullptr) { DeleteObject(g_g.brSurface); g_g.brSurface = nullptr; }
    HWND ctrls[10] = {g_g.cmbGame, g_g.edtPath, g_g.btnBrowse, g_g.edtArgs, g_g.chkPower,
                      g_g.chkFrame, g_g.chkWork, g_g.btnApply, g_g.btnSave, g_g.btnRollback};
    for (int i = 0; i < 10; ++i) {
        if (ctrls[i] == nullptr) continue;
        const UiFontRole role = (ctrls[i] == g_g.btnApply || ctrls[i] == g_g.btnRollback)
                                    ? UiFontRole::BodyBold
                                    : UiFontRole::Body;
        HFONT f = UiFont(role);
        if (f != nullptr) SendMessageW(ctrls[i], WM_SETFONT, reinterpret_cast<WPARAM>(f), TRUE);
    }
    Relayout();
}

void GamePageRefresh() {
    RefreshPresetSummary();
    LoadConfigToUI();
    InvalidateRect(g_g.panel, nullptr, FALSE);
}

}  // namespace ui
}  // namespace gopt
