// GameOptimizer 命令行入口（第 5 步：AppCore 协调层 + CLI，发布版）
#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdarg>
#include <cstdio>
#include <string>
#include <thread>
#include <vector>

#include "config/GameConfig.h"
#include "core/AppCore.h"
#include "hal/HAL.h"
#include "i18n.h"
#include "license/License.h"
#include "tuning/StartupManager.h"
#include "tuning/SystemTuner.h"
#include "version.h"

// K32GetProcessMemoryInfo 由 kernel32 导出（Win7+）：只取结构体，不链接 psapi，也不改构建脚本
#ifndef PSAPI_VERSION
#define PSAPI_VERSION 2
#endif
#include <psapi.h>

using gopt::AppConfig;
using gopt::AppCore;
using gopt::GameConfig;
using gopt::GameId;
using gopt::GameLaunchConfig;
using gopt::Lang;
using gopt::SetLang;
using gopt::T;

static void PrintUsage() {
    std::printf("GameOptimizer CLI v%s (build %d.%d.%d)\n",
                GOPT_VERSION_STR, GOPT_VERSION_MAJOR, GOPT_VERSION_MINOR, GOPT_VERSION_PATCH);
    std::puts(T(
        "\n用法:\n"
        "  gopt_cli status                      查看硬件指纹、预设与授权\n"
        "  gopt_cli report [--out <文件路径>]    诊断报告（可导出为 UTF-8 文本）\n"
        "  gopt_cli apply <game> [选项]         应用优化（自动快照 + 看门狗监控）\n"
        "  gopt_cli rollback                    回滚最近一次优化\n"
        "  gopt_cli rollback-all                回滚全部\n"
        "  gopt_cli list                          显示运行中的支持游戏（优先级/亲和性）\n"
        "  gopt_cli watch [秒数] [--top [N]]     实时监视；--top 显示进程热点榜（CPU%/内存/优先级）\n"
        "  gopt_cli prio <pid> <级别>            设置进程优先级 high|above|normal|below|idle\n"
        "  gopt_cli clean                         清理临时文件（24 小时内文件保留）\n"
        "  gopt_cli fingerprint                 显示本机机器指纹（授权绑定用）\n"
        "  gopt_cli license status              查看授权状态\n"
        "  gopt_cli license activate <code>     安装授权码\n"
        "  gopt_cli license gen <hash> <ed> [expiry]   开发者：为机器指纹生成授权码\n"
        "\n游戏: deltaforce | lol | cs2 | pubg | valorant | apex | dota2 | ow\n"
        "\n选项 (apply):\n"
        "  --game-exe <path>   工具代启动游戏（CREATE_SUSPENDED → 设置 → Resume）\n"
        "  --dry-run           只读预览：显示将应用的优化项（不修改任何设置）\n"
        "  --power             允许切换高性能电源方案（需管理员，默认关闭；功能本身免费）\n"
        "  --lang zh|en        界面语言（默认 zh）\n"
        "\n所有功能免费：电源切换/驱动帧延迟/工作集等全部优化项对所有人开放。\n"
        "安全边界: 无注入、无内核 Hook；优先级上限 HIGH；全部修改可一键回滚。",
        "\nUsage:\n"
        "  gopt_cli status                      Show hardware, presets and license\n"
        "  gopt_cli report [--out <file>]       Diagnostic report (export as UTF-8 text)\n"
        "  gopt_cli apply <game> [options]      Apply optimization (auto snapshot + watchdog)\n"
        "  gopt_cli rollback                    Rollback the last optimization\n"
        "  gopt_cli rollback-all                Rollback everything\n"
        "  gopt_cli list                         Show running supported games (priority/affinity)\n"
        "  gopt_cli watch [seconds] [--top [N]] Live monitor; --top = process hot list (CPU%/RAM/prio)\n"
        "  gopt_cli prio <pid> <level>           Set process priority high|above|normal|below|idle\n"
        "  gopt_cli clean                        Clean temp files (files < 24h are kept)\n"
        "  gopt_cli fingerprint                 Show machine fingerprint (for licensing)\n"
        "  gopt_cli license status              Show license status\n"
        "  gopt_cli license activate <code>     Install a license code\n"
        "  gopt_cli license gen <hash> <ed> [expiry]   Dev: generate a license code\n"
        "\nGames: deltaforce | lol | cs2 | pubg | valorant | apex | dota2 | ow\n"
        "\nOptions (apply):\n"
        "  --game-exe <path>   Tool starts the game (CREATE_SUSPENDED -> settings -> Resume)\n"
        "  --dry-run           Preview only: show what will be applied (no changes)\n"
        "  --power             Allow high-performance power scheme (admin, off by default; free feature)\n"
        "  --lang zh|en        UI language (default zh)\n"
        "\nAll features are free: power scheme / frame latency / working set are open to everyone.\n"
        "Safety: no injection, no kernel hooks; priority capped at HIGH; every change is rollable."));
}

// 解析游戏名（中英文别名），失败返回 false
static bool ParseGame(const char* s, GameId* out) {
    if (!s || !*s) return false;
    std::string v(s);
    for (char& c : v) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    if (v == "deltaforce" || v == "df" || v == "三角洲" || v == "三角洲行动") {
        *out = GameId::DeltaForce;
        return true;
    }
    if (v == "lol" || v == "league" || v == "lol1" || v == "英雄联盟") {
        *out = GameId::LeagueOfLegends;
        return true;
    }
    if (v == "cs2" || v == "cs") {
        *out = GameId::CS2;
        return true;
    }
    if (v == "pubg" || v == "吃鸡" || v == "绝地求生") {
        *out = GameId::PUBG;
        return true;
    }
    if (v == "valorant" || v == "无畏" || v == "无畏契约") {
        *out = GameId::Valorant;
        return true;
    }
    if (v == "apex" || v == "apexlegends") {
        *out = GameId::Apex;
        return true;
    }
    if (v == "dota" || v == "dota2") {
        *out = GameId::Dota2;
        return true;
    }
    if (v == "ow" || v == "ow2" || v == "overwatch" || v == "守望") {
        *out = GameId::Overwatch2;
        return true;
    }
    return false;
}

// ---------- report 子命令辅助（诊断报告：稳定字段对齐 + UTF-8 导出） ----------

// 显示宽度：ASCII 记 1 列，非 ASCII（中日韩等）记 2 列，保证中英文报告都字段对齐
static int DisplayWidth(const std::string& s) {
    int w = 0;
    for (std::size_t i = 0; i < s.size();) {
        const unsigned char c = static_cast<unsigned char>(s[i]);
        if (c < 0x80) { ++w; i += 1; }
        else if ((c & 0xE0) == 0xC0) { w += 2; i += 2; }
        else if ((c & 0xF0) == 0xE0) { w += 2; i += 3; }
        else if ((c & 0xF8) == 0xF0) { w += 2; i += 4; }
        else { ++w; i += 1; }
    }
    return w;
}

// 按显示宽度左侧补空格到 cols 列
static std::string PadDisplay(const std::string& s, int cols) {
    std::string r = s;
    const int w = DisplayWidth(s);
    if (w < cols) r.append(static_cast<std::size_t>(cols - w), ' ');
    return r;
}

static std::string StrFmt(const char* fmt, ...) {
    char buf[512] = {};
    va_list ap;
    va_start(ap, fmt);
    std::vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    return std::string(buf);
}

// 一行 "标签: 值"，标签按显示宽度左对齐
static void AppendField(std::string& out, const std::string& label, const std::string& value,
                        int labelCols = 20) {
    out += PadDisplay(label, labelCols);
    out += ": ";
    out += value;
    out += "\n";
}

// 上次优化时间：%LOCALAPPDATA%\GameOptimizer\savepoints.txt 的最后写入时间（稳定文件 API）
static std::string LastOptimizeText() {
    wchar_t base[MAX_PATH] = {};
    if (GetEnvironmentVariableW(L"LOCALAPPDATA", base, MAX_PATH) <= 0)
        return T("未知（读不到 LOCALAPPDATA）", "unknown (LOCALAPPDATA unavailable)");
    const std::wstring p = std::wstring(base) + L"\\GameOptimizer\\savepoints.txt";
    WIN32_FILE_ATTRIBUTE_DATA fa{};
    if (!GetFileAttributesExW(p.c_str(), GetFileExInfoStandard, &fa))
        return T("从未优化（无快照文件）", "never (no snapshot file)");
    SYSTEMTIME st{};
    if (!FileTimeToSystemTime(&fa.ftLastWriteTime, &st))
        return T("未知（时间戳不可读）", "unknown (timestamp unreadable)");
    return StrFmt("%04u-%02u-%02u %02u:%02u:%02u", st.wYear, st.wMonth, st.wDay,
                  st.wHour, st.wMinute, st.wSecond);
}

// 组装诊断报告（纯文本；字段对齐，中英文均可直接复制粘贴）
static std::string BuildReport() {
    AppCore core;
    std::string out;
    out += "GameOptimizer ";
    out += T("诊断报告", "diagnostic report");
    out += "\n==============================================================\n";

    SYSTEMTIME now{};
    GetLocalTime(&now);
    AppendField(out, T("生成时间", "Generated"),
                StrFmt("%04u-%02u-%02u %02u:%02u:%02u", now.wYear, now.wMonth, now.wDay,
                       now.wHour, now.wMinute, now.wSecond));
    AppendField(out, T("版本", "Version"), GOPT_VERSION_STR);

    // 提权状态：如实显示；未提权时电源切换与系统调优受限
    const bool elevated = gopt::HAL::IsElevated();
    AppendField(out, T("提权状态", "Elevation"),
                elevated ? T("已提权（电源切换/系统调优可用）",
                            "elevated (power scheme / system tuning available)")
                         : T("未提权（电源方案切换与系统调优受限，需以管理员运行）",
                             "not elevated (power scheme switching and system tuning limited; "
                             "run as administrator)"));

    // 当前电源方案：只读查询，失败时输出 <unknown>
    GUID scheme{};
    std::string schemeName = "<unknown>";
    if (gopt::HAL::QueryActivePowerScheme(&scheme)) {
        const std::string n = gopt::HAL::PowerSchemeName(scheme);
        if (!n.empty()) schemeName = n;
    }
    AppendField(out, T("当前电源方案", "Power scheme"), schemeName);

    // 运行中的支持游戏
    std::string gamesText;
    for (const auto& [id, pid] : core.RunningGames()) {
        if (!gamesText.empty()) gamesText += ", ";
        gamesText += StrFmt("%s(pid %u)", gopt::GameIdToString(id).c_str(),
                            static_cast<unsigned>(pid));
    }
    if (gamesText.empty())
        gamesText = T("无（当前没有运行中的支持游戏）", "none (no supported games running)");
    AppendField(out, T("运行中的游戏", "Running games"), gamesText);
    AppendField(out, T("上次优化时间", "Last optimized"), LastOptimizeText());

    out += "\n[";
    out += T("硬件概要", "Hardware");
    out += "]\n";
    out += core.Profile().ToString();
    if (out.empty() || out.back() != '\n') out += "\n";

    out += "\n[";
    out += T("8 款游戏预设概览", "Presets overview (8 games)");
    out += "]\n";
    for (const GameId id : {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                            GameId::PUBG, GameId::Valorant, GameId::Apex,
                            GameId::Dota2, GameId::Overwatch2}) {
        const gopt::GamePreset p = core.ResolvedPreset(id);
        out += "  ";
        out += PadDisplay(gopt::GameIdToString(id), 14);
        out += p.description;
        out += "\n";
    }

    out += "\n[";
    out += T("安全边界", "Safety");
    out += "]\n";
    out += T("无注入、无内核 Hook；优先级上限 HIGH（不使用 REALTIME）；所有修改均可一键回滚。",
             "No injection, no kernel hooks; priority capped at HIGH (never REALTIME); "
             "every change is rollable.");
    out += "\n";
    return out;
}

// 路径转宽字符：优先按 UTF-8（控制台 chcp 65001）；非法 UTF-8 时回退系统 ANSI 代码页
// （Windows 下 CRT 的 argv 默认是 ANSI），保证中文/空格路径都能正确落到文件
static std::wstring PathToWide(const std::string& path) {
    const UINT codepages[2] = {CP_UTF8, CP_ACP};
    for (const UINT cp : codepages) {
        const DWORD flags = (cp == CP_UTF8) ? MB_ERR_INVALID_CHARS : 0;
        const int n = MultiByteToWideChar(cp, flags, path.c_str(), -1, nullptr, 0);
        if (n <= 0) continue;
        std::wstring w(static_cast<std::size_t>(n), L'\0');
        if (MultiByteToWideChar(cp, flags, path.c_str(), -1, &w[0], n) <= 0) continue;
        w.resize(static_cast<std::size_t>(n) - 1);
        return w;
    }
    return std::wstring();
}

// 以 UTF-8（无 BOM）写文本文件；路径经 UTF-16 转换后交给官方 Win32 API
static bool WriteTextUtf8(const std::string& pathUtf8, const std::string& text) {
    if (pathUtf8.empty()) return false;
    const std::wstring wpath = PathToWide(pathUtf8);
    if (wpath.empty()) return false;
    HANDLE h = CreateFileW(wpath.c_str(), GENERIC_WRITE, FILE_SHARE_READ, nullptr, CREATE_ALWAYS,
                           FILE_ATTRIBUTE_NORMAL, nullptr);
    if (h == INVALID_HANDLE_VALUE) return false;
    DWORD written = 0;
    const DWORD want = static_cast<DWORD>(text.size());
    const BOOL ok = WriteFile(h, text.data(), want, &written, nullptr);
    CloseHandle(h);
    return ok != FALSE && written == want;
}

// ---------- watch --top 进程级热点榜 ----------

struct WatchTopRow {
    std::string name;                 // 游戏显示名
    unsigned pid = 0;
    double cpuPct = 0.0;
    bool hasCpu = false;              // 首次 tick 无历史采样 -> false（显示 --）
    unsigned long long memMB = 0;     // 工作集（MB）
    std::string prio = "-";           // 优先级短标签
};

// 优先级类 -> 稳定短标签（安全边界：不使用也不显示 REALTIME）
static const char* PriorityLabel(DWORD cls) {
    switch (cls) {
        case HIGH_PRIORITY_CLASS:         return "HIGH";
        case ABOVE_NORMAL_PRIORITY_CLASS: return "ABOVE";
        case NORMAL_PRIORITY_CLASS:       return "NORMAL";
        case BELOW_NORMAL_PRIORITY_CLASS: return "BELOW";
        case IDLE_PRIORITY_CLASS:         return "IDLE";
        default:                          return "-";
    }
}

// 标准输出是控制台时原地清屏（纯 Win32，不依赖 ANSI 转义/重定向场景）；
// 输出被重定向（管道/文件）时返回 false，调用方改为顺序打印整块，便于复制粘贴。
static bool ClearConsoleIfAvailable() {
    const HANDLE h = GetStdHandle(STD_OUTPUT_HANDLE);
    if (h == nullptr || h == INVALID_HANDLE_VALUE) return false;
    CONSOLE_SCREEN_BUFFER_INFO csbi{};
    if (!GetConsoleScreenBufferInfo(h, &csbi)) return false;
    // 只重绘可见窗口区域（不整块填充回滚缓冲区，避免每秒大范围写控制台）
    const SHORT width = static_cast<SHORT>(csbi.srWindow.Right - csbi.srWindow.Left + 1);
    const SHORT height = static_cast<SHORT>(csbi.srWindow.Bottom - csbi.srWindow.Top + 1);
    const DWORD cells = static_cast<DWORD>(width) * static_cast<DWORD>(height);
    const COORD home{0, csbi.srWindow.Top};
    DWORD written = 0;
    FillConsoleOutputCharacterA(h, ' ', cells, home, &written);
    if (!FillConsoleOutputAttribute(h, csbi.wAttributes, cells, home, &written)) return false;
    return SetConsoleCursorPosition(h, home) != FALSE;
}

// watch --top [N]：每秒刷新运行中支持游戏的进程级指标，按 CPU% 降序取前 N
// CPU% = (kernel+user 差值) / (实际间隔秒 * 逻辑核数)，与 GUI 口径一致；首次 tick 为 --
static int RunWatchTop(AppCore& core, int topN, int maxSecs) {
    if (topN < 1) topN = 5;
    if (topN > 8) topN = 8;  // 只会枚举 8 款支持游戏
    const int logical = core.Profile().logicalCores > 0 ? core.Profile().logicalCores : 1;

    struct ProcSample {
        unsigned pid = 0;
        ULONGLONG kernel = 0;  // 100ns
        ULONGLONG user = 0;    // 100ns
        bool ok = false;
    };
    std::vector<ProcSample> prev;
    ULONGLONG lastMs = GetTickCount64();
    const ULONGLONG t0 = lastMs;
    bool printed = false;

    std::printf(T("进程热点榜 Top %d（每秒刷新，Ctrl+C 退出）\n",
                  "Process hot list top %d (1s refresh, Ctrl+C to exit)\n"), topN);
    for (;;) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1000));
        const ULONGLONG nowMs = GetTickCount64();
        const double intervalSec = static_cast<double>(nowMs - lastMs) / 1000.0;
        lastMs = nowMs;

        std::vector<WatchTopRow> rows;
        std::vector<ProcSample> next;
        for (const auto& [id, pid] : core.RunningGames()) {
            WatchTopRow r;
            r.pid = pid;
            r.name = gopt::GameIdToString(id);
            ProcSample s;
            s.pid = pid;
            HANDLE h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
            if (h != nullptr) {
                FILETIME c{}, e{}, k{}, u{};
                if (GetProcessTimes(h, &c, &e, &k, &u)) {
                    s.kernel = (static_cast<ULONGLONG>(k.dwHighDateTime) << 32) | k.dwLowDateTime;
                    s.user = (static_cast<ULONGLONG>(u.dwHighDateTime) << 32) | u.dwLowDateTime;
                    s.ok = true;
                }
                PROCESS_MEMORY_COUNTERS pmc{};
                pmc.cb = sizeof(pmc);
                if (K32GetProcessMemoryInfo(h, &pmc, sizeof(pmc))) {
                    r.memMB = static_cast<unsigned long long>(
                        pmc.WorkingSetSize / (1024ull * 1024ull));
                }
                r.prio = PriorityLabel(GetPriorityClass(h));
                CloseHandle(h);
                if (s.ok && intervalSec > 0.0) {
                    for (const ProcSample& p : prev) {
                        if (p.pid != pid || !p.ok) continue;
                        const ULONGLONG delta = (s.kernel - p.kernel) + (s.user - p.user);
                        r.cpuPct = static_cast<double>(delta) /
                                   (intervalSec * 10000000.0 * logical) * 100.0;
                        if (r.cpuPct < 0.0) r.cpuPct = 0.0;
                        r.hasCpu = true;
                        break;
                    }
                }
            }
            rows.push_back(r);
            next.push_back(s);
        }
        prev.swap(next);

        // CPU% 降序；无 CPU 数据时按内存降序，保证输出稳定可复现
        std::sort(rows.begin(), rows.end(), [](const WatchTopRow& a, const WatchTopRow& b) {
            if (a.hasCpu != b.hasCpu) return a.hasCpu && !b.hasCpu;
            if (a.cpuPct != b.cpuPct) return a.cpuPct > b.cpuPct;
            if (a.memMB != b.memMB) return a.memMB > b.memMB;
            return a.name < b.name;
        });
        if (static_cast<int>(rows.size()) > topN) rows.resize(static_cast<std::size_t>(topN));

        const bool cleared = ClearConsoleIfAvailable();
        if (!cleared && printed) std::printf("\n");
        printed = true;

        SYSTEMTIME st{};
        GetLocalTime(&st);
        std::printf(T("[%02u:%02u:%02u] 进程热点榜 Top %d（逻辑核 %d）\n",
                      "[%02u:%02u:%02u] process hot list top %d (logical cores %d)\n"),
                    st.wHour, st.wMinute, st.wSecond, topN, logical);

        std::string hdr = "  ";
        hdr += PadDisplay("pid", 7);
        hdr += PadDisplay(T("游戏", "game"), 14);
        hdr += PadDisplay("CPU%", 8);
        hdr += PadDisplay(T("内存MB", "MEM MB"), 10);
        hdr += T("优先级", "PRIO");
        std::printf("%s\n", hdr.c_str());

        if (rows.empty()) {
            std::printf("  %s\n", T("无（当前没有运行中的支持游戏）",
                                    "none (no supported games running)"));
        } else {
            for (const WatchTopRow& r : rows) {
                std::string line = "  ";
                line += PadDisplay(StrFmt("%u", r.pid), 7);
                line += PadDisplay(r.name, 14);
                line += PadDisplay(r.hasCpu ? StrFmt("%.1f%%", r.cpuPct) : std::string("--"), 8);
                line += PadDisplay(StrFmt("%llu", r.memMB), 10);
                line += r.prio;
                std::printf("%s\n", line.c_str());
            }
        }
        std::fflush(stdout);
        if (maxSecs > 0 && (GetTickCount64() - t0) / 1000 >= static_cast<ULONGLONG>(maxSecs)) break;
    }
    std::printf("\n");
    return 0;
}

int main(int argc, char** argv) {
    if (argc < 2) {
        PrintUsage();
        return 0;
    }
    const std::string cmd = argv[1];

    if (cmd == "--version" || cmd == "-v") {
        std::printf("GameOptimizer v%s\n", GOPT_VERSION_STR);
        return 0;
    }

    // 每游戏「优化启动」配置（独立解析，避免与公共选项冲突）
    if (cmd == "game") {
        GameId id = GameId::DeltaForce;
        bool haveGame = false;
        GameLaunchConfig gc;
        bool changed = false;
        for (int i = 2; i < argc; ++i) {
            const std::string a = argv[i];
            if (a == "--exe" && i + 1 < argc) { gc.exePath = argv[++i]; changed = true; }
            else if (a == "--args" && i + 1 < argc) { gc.args = argv[++i]; changed = true; }
            else if (a == "--power") { gc.powerScheme = true; changed = true; }
            else if (a == "--no-power") { gc.powerScheme = false; changed = true; }
            else if (a == "--auto") { gc.optimizedOnLaunch = true; changed = true; }
            else if (a == "--no-auto") { gc.optimizedOnLaunch = false; changed = true; }
            else if (a == "--latency") { gc.frameLatency = true; changed = true; }
            else if (a == "--no-latency") { gc.frameLatency = false; changed = true; }
            else if (a == "--ws") { gc.workingSet = true; changed = true; }
            else if (a == "--no-ws") { gc.workingSet = false; changed = true; }
            else if (!a.empty() && a[0] != '-' && !haveGame) {
                if (ParseGame(a.c_str(), &id)) haveGame = true;
            }
        }
        if (!haveGame) {
            std::puts(T("用法: gopt_cli game <game> [--exe <路径>] [--args \"<参数>\"] "
                        "[--power] [--no-power] [--auto] [--no-auto] [--latency] [--no-latency] [--ws] [--no-ws]",
                        "Usage: gopt_cli game <game> [--exe <path>] [--args \"<...>\"] ..."));
            return 1;
        }
        gc = changed ? (GameConfig::Set(id, gc) ? gc : GameConfig::Get(id))
                     : GameConfig::Get(id);
        std::printf("%s %s:\n"
                    "  exe   : %s\n  args  : %s\n  auto  : %s\n  power : %s\n"
                    "  frame : %s\n  wkset : %s\n  cfg   : %s\n",
                    T("配置", "config"), gopt::GameIdToString(id).c_str(),
                    gc.exePath.empty() ? T("(未设置)", "(unset)") : gc.exePath.c_str(),
                    gc.args.empty() ? T("(无)", "(none)") : gc.args.c_str(),
                    gc.optimizedOnLaunch ? T("开", "on") : T("关", "off"),
                    gc.powerScheme ? T("开", "on") : T("关", "off"),
                    gc.frameLatency ? T("开", "on") : T("关", "off"),
                    gc.workingSet ? T("开", "on") : T("关", "off"),
                    GameConfig::ConfigPath().c_str());
        return 0;
    }

    // 解析公共选项
    std::string gameArg, gameExe, outPath;
    bool dryRun = false;
    bool outMissing = false;
    bool topMode = false;   // watch --top：进程热点榜
    bool topBad = false;    // --top 后跟了非法 N
    int topN = 5;           // 默认显示前 5
    AppConfig cfg;
    for (int i = 2; i < argc; ++i) {
        const std::string a = argv[i];
        if (a == "--game-exe" && i + 1 < argc) {
            gameExe = argv[++i];
        } else if (a == "--out") {
            if (i + 1 < argc) outPath = argv[++i];
            else outMissing = true;
        } else if (a == "--top") {
            topMode = true;
            if (i + 1 < argc) {
                const std::string nxt = argv[i + 1];
                if (!nxt.empty() && nxt.find_first_not_of("0123456789") == std::string::npos) {
                    topN = std::atoi(nxt.c_str());  // --top 8
                    ++i;
                } else if (!nxt.empty() && nxt[0] != '-') {
                    topBad = true;                  // --top abc
                }
            }
        } else if (a == "--power") {
            cfg.allowPowerSchemeSwitch = true;
        } else if (a == "--dry-run") {
            dryRun = true;
        } else if (a == "--lang" && i + 1 < argc) {
            SetLang(std::string(argv[++i]) == "en" ? Lang::En : Lang::Zh);
        } else if (gameArg.empty() && !a.empty() && a[0] != '-') {
            gameArg = a;
        }
    }

    if (cmd == "report") {
        // 诊断报告：一条命令拿到全部状态；可选 --out 导出 UTF-8 文本
        if (outMissing) {
            std::puts(T("错误：--out 需要一个文件路径。", "Error: --out requires a file path."));
            return 1;
        }
        const std::string report = BuildReport();
        std::fputs(report.c_str(), stdout);
        if (!outPath.empty()) {
            if (WriteTextUtf8(outPath, report)) {
                std::printf(T("已写入（UTF-8 无 BOM）: %s\n", "Written (UTF-8, no BOM): %s\n"),
                            outPath.c_str());
            } else {
                std::printf(T("写入失败: %s\n", "Write failed: %s\n"), outPath.c_str());
                return 1;
            }
        }
        return 0;
    }

    if (cmd == "status") {
        AppCore core(cfg);
        std::puts(T("所有功能免费：全部优化项对所有人开放。",
                    "All features free: every optimization enabled for everyone."));
        std::puts(core.Profile().ToString().c_str());
        std::puts(T("\n预设概览：", "\nPresets overview:"));
        for (const GameId id : {GameId::DeltaForce, GameId::LeagueOfLegends, GameId::CS2,
                                GameId::PUBG, GameId::Valorant, GameId::Apex,
                                GameId::Dota2, GameId::Overwatch2}) {
            const gopt::GamePreset p = core.ResolvedPreset(id);
            std::printf("  %-10s %s\n", gopt::GameIdToString(id).c_str(), p.description.c_str());
        }
        GUID scheme{};
        if (gopt::HAL::QueryActivePowerScheme(&scheme)) {
            std::printf("%s: %s\n", T("当前电源方案", "Power scheme"),
                        gopt::HAL::PowerSchemeName(scheme).c_str());
        }
        std::printf("%s: %s\n", T("驱动级帧延迟配置", "Driver frame latency"),
                    gopt::HAL::IsDriverFrameLatencySupported(core.Profile())
                        ? T("支持", "supported")
                        : T("不支持（将降级跳过）", "unsupported (degraded)"));
        std::printf("%s: %s\n", T("提权状态", "Elevation"),
                    gopt::HAL::IsElevated()
                        ? T("已提权（全部功能可用）", "elevated (all features available)")
                        : T("未提权（电源/系统调优等需管理员运行）", "not elevated (power/tune need admin)"));
        return 0;
    }

    if (cmd == "fingerprint") {
        AppCore core(cfg);
        const gopt::MachineFingerprint mf = gopt::ComputeMachineFingerprint(core.Profile());
        std::printf("本机机器指纹:\n  主板序列号 : %s\n  指纹哈希   : %s\n",
                    mf.boardSerial.empty() ? "(未读出)" : mf.boardSerial.c_str(),
                    mf.hash.c_str());
        return 0;
    }

    if (cmd == "license" && argc >= 3) {
        const std::string sub = argv[2];
        AppCore core(cfg);
        if (sub == "status") {
            const gopt::LicenseInfo li = gopt::License::Check(core.Profile());
            if (li.valid) {
                std::printf("授权状态:\n  有效\n  %s\n", li.message.c_str());
            } else {
                std::puts(T("授权状态: 未激活（所有功能免费，无需授权；授权仅可选用途）\n  本机指纹可用 `gopt_cli fingerprint` 查看。\n",
                            "License: not activated (all features are free; license is optional)\n  Use `gopt_cli fingerprint` to see the machine fingerprint.\n"));
            }
            return 0;
        }
        if (sub == "activate" && argc >= 4) {
            const gopt::LicenseInfo li = gopt::License::Activate(argv[3], core.Profile());
            std::printf("激活结果: %s\n", li.message.c_str());
            return li.valid ? 0 : 1;
        }
        if (sub == "gen" && argc >= 5) {
            const std::string code = gopt::License::Generate(
                argv[3], argv[4], argc >= 6 ? argv[5] : "permanent");
            std::printf("授权码:\n%s\n", code.c_str());
            return 0;
        }
    }

    if (cmd == "apply") {
        GameId id;
        if (!ParseGame(gameArg.c_str(), &id)) {
            std::printf("未知游戏: %s（支持 deltaforce / lol / cs2）\n", gameArg.c_str());
            return 1;
        }
        cfg.gameExeOverride = gameExe;
        if (dryRun) {
            // 只读预览：显示解析后的预设与将应用的具体项，不修改任何设置
            AppCore core(cfg);
            const gopt::GamePreset p = core.ResolvedPreset(id);
            std::printf("%s: %s\n", T("目标", "Target"), gopt::GameIdToString(id).c_str());
            std::puts(T("将应用（只读预览，未执行任何修改）：", "Will apply (preview only, nothing modified):"));
            std::printf("  %s\n", p.description.c_str());
            if (p.processPriorityClass)
                std::printf("  %s: %s\n", T("进程优先级", "Priority"),
                            p.processPriorityClass == HIGH_PRIORITY_CLASS ? T("高 (HIGH)", "High")
                                                                          : T("高于正常 (ABOVE_NORMAL)", "AboveNormal"));
            if (p.cpuAffinityMask)
                std::printf("  %s: 0x%llx（%s %d %s）\n", T("CPU 亲和性", "CPU affinity"),
                            static_cast<unsigned long long>(p.cpuAffinityMask),
                            T("保留", "keep"), p.leaveCoresForSystem, T("核给系统", "cores for system"));
            if (p.workingSetMinMB > 0)
                std::printf("  %s: %llu MB\n", T("工作集下限", "Working set min"),
                            static_cast<unsigned long long>(p.workingSetMinMB));
            if (p.gpuMaxFrames > 0)
                std::printf("  %s: %u\n", T("驱动级帧延迟", "Driver frame latency"), p.gpuMaxFrames);
            if (cfg.allowPowerSchemeSwitch && p.switchHighPerformancePower)
                std::printf("  %s: %s\n", T("电源方案", "Power scheme"), T("高性能", "High performance"));
            std::printf("  %s: %s\n", T("快照/看门狗", "Snapshot/watchdog"),
                        T("自动启用（可随时回滚）", "auto enabled (rollable anytime)"));
            return 0;
        }
        AppCore core(cfg);
        std::puts(core.OptimizeForGame(id).c_str());

        if (!core.HasActiveOptimization()) {
            std::puts("\n（未实际应用优化，已跳过监控）");
            return 0;
        }

        std::puts("\n正在监控系统响应（最长 30 秒）...");
        bool stable = true;
        for (int i = 0; i < 60; ++i) {
            std::this_thread::sleep_for(std::chrono::milliseconds(500));
            if (!core.IsStable()) {
                stable = false;
                std::printf("检测到系统响应异常，自动回滚: %s\n", core.Rollback().c_str());
                break;
            }
        }
        if (stable) {
            core.StopWatchdog();
            std::puts("监控结束：系统响应正常，优化保持生效。");
        }
        return 0;
    }

    if (cmd == "optimize") {
        AppCore core(cfg);
        if (dryRun) {
            // 只读预览：显示将优化的运行中游戏及其预设（不修改任何设置）
            const auto running = core.RunningGames();
            if (running.empty()) {
                std::puts(T("预览：无运行中的支持游戏；将执行系统级一键优化（电源方案等，随配置）。",
                            "Preview: no supported games running; system-level one-click will run (power etc., as configured)."));
            } else {
                std::printf("%s:\n", T("预览：将优化以下运行中的支持游戏", "Preview: will optimize these running games"));
                for (const auto& [id, pid] : running) {
                    const gopt::GamePreset p = core.ResolvedPreset(id);
                    std::printf("  %-10s pid=%-7u %s\n",
                                gopt::GameIdToString(id).c_str(), pid, p.description.c_str());
                }
            }
            std::puts(T("（只读预览，未修改任何设置；快照/看门狗将在实际执行时启用）",
                        "(read-only preview, nothing modified; snapshot/watchdog start on real execution)"));
            return 0;
        }
        if (gameArg.empty()) {
            // 一键+并发：批量处理所有运行中的支持游戏（无游戏则系统级）
            std::puts(core.OptimizeAll().c_str());
        } else if (gameArg == "system" || gameArg == "sys") {
            std::puts(core.OptimizeSystem().c_str());
        } else {
            GameId id;
            if (!ParseGame(gameArg.c_str(), &id)) {
                std::printf(T("未知游戏: %s（支持 deltaforce / lol / cs2 等）\n",
                              "Unknown game: %s (deltaforce / lol / cs2 ...)\n"),
                            gameArg.c_str());
                return 1;
            }
            cfg.gameExeOverride = gameExe;
            AppCore core2(cfg);
            std::puts(core2.OptimizeForGame(id).c_str());
        }
        if (!core.HasActiveOptimization()) {
            std::puts(T("\n（未实际应用优化，已跳过监控）", "\n(not applied, skip monitor)"));
            return 0;
        }
        std::puts(T("\n正在监控系统响应（最长 30 秒）...",
                    "\nMonitoring system response (max 30s)..."));
        bool stable = true;
        for (int i = 0; i < 60; ++i) {
            std::this_thread::sleep_for(std::chrono::milliseconds(500));
            if (!core.IsStable()) {
                stable = false;
                std::printf(T("检测到系统响应异常，自动回滚: %s\n",
                              "Abnormal system response, auto rollback: %s\n"),
                            core.Rollback().c_str());
                break;
            }
        }
        if (stable) {
            core.StopWatchdog();
            std::puts(T("监控结束：系统响应正常，优化保持生效。",
                        "Monitor done: system stable, optimization kept."));
        }
        return 0;
    }

    if (cmd == "tune") {
        AppCore core(cfg);
        const std::string sub = gameArg;
        if (sub == "restore") {
            std::puts(core.RestoreTune().c_str());
            return 0;
        }
        if (sub == "status" || sub == "show") {
            // 只读：显示当前电源方案 + 按硬件推荐的档位
            GUID scheme{};
            std::string name = "?";
            if (gopt::HAL::QueryActivePowerScheme(&scheme)) name = gopt::HAL::PowerSchemeName(scheme);
            const bool hi = gopt::SystemTuner::RecommendHighPerf(core.Profile());
            std::printf("%s: %s\n%s: %s\n",
                        T("当前电源方案", "Active power scheme"), name.c_str(),
                        T("本机推荐（按物理核/内存）", "Recommended (cores/RAM)"),
                        hi ? T("高性能档", "high performance") : T("平衡档", "balanced"));
            std::puts(T("提示：tune [high|balanced] 应用；tune restore 恢复。",
                        "Tip: tune [high|balanced] applies; tune restore reverts."));
            return 0;
        }
        bool high = gopt::SystemTuner::RecommendHighPerf(core.Profile());
        if (sub == "high") high = true;
        else if (sub == "balanced" || sub == "balance") high = false;
        else if (sub.empty())
            std::printf("%s\n", high ? T("（依据硬件推荐：高性能档）", "(hardware suggests: high-performance)")
                                     : T("（依据硬件推荐：平衡档）", "(hardware suggests: balanced)"));
        std::puts(core.TuneSystem(high).c_str());
        return 0;
    }

    if (cmd == "list") {
        // Process Lasso 风格：运行中的支持游戏概览（优先级/亲和性）
        AppCore core(cfg);
        const auto running = core.RunningGames();
        if (running.empty()) {
            std::puts(T("没有运行中的支持游戏。", "No supported games running."));
            return 0;
        }
        for (const auto& [id, pid] : running) {
            HANDLE h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
            DWORD pri = 0;
            DWORD_PTR mask = 0, sys = 0;
            if (h != nullptr) {
                pri = GetPriorityClass(h);
                GetProcessAffinityMask(h, &mask, &sys);
                CloseHandle(h);
            }
            std::printf("  %-10s pid=%-7u 优先级=0x%lx 亲和性=0x%llx\n",
                        gopt::GameIdToString(id).c_str(), pid,
                        static_cast<unsigned long>(pri),
                        static_cast<unsigned long long>(mask));
        }
        return 0;
    }

    if (cmd == "prio" && argc >= 4) {
        const unsigned long pid = std::strtoul(argv[2], nullptr, 10);
        const std::string level = argv[3];
        DWORD cls = 0;
        std::string name;
        if (level == "high") { cls = HIGH_PRIORITY_CLASS; name = T("高（High）", "High"); }
        else if (level == "above") { cls = ABOVE_NORMAL_PRIORITY_CLASS; name = T("高于正常（AboveNormal）", "AboveNormal"); }
        else if (level == "normal") { cls = NORMAL_PRIORITY_CLASS; name = T("正常（Normal）", "Normal"); }
        else if (level == "below") { cls = BELOW_NORMAL_PRIORITY_CLASS; name = T("低于正常（BelowNormal）", "BelowNormal"); }
        else if (level == "idle") { cls = IDLE_PRIORITY_CLASS; name = T("空闲（Idle）", "Idle"); }
        else {
            std::puts(T("用法: gopt_cli prio <pid> high|above|normal|below|idle",
                        "Usage: gopt_cli prio <pid> high|above|normal|below|idle"));
            return 1;
        }
        HANDLE h = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if (h == nullptr) {
            std::printf(T("无法打开进程 %lu（可能已退出或无权限）。\n",
                          "Cannot open process %lu (may have exited or no access).\n"), pid);
            return 1;
        }
        const bool ok = gopt::HAL::SetProcessPriority(h, cls);
        CloseHandle(h);
        std::printf("%s %lu -> %s: %s\n", T("进程", "Process"), pid, name.c_str(),
                    ok ? T("成功", "OK") : T("失败", "FAILED"));
        return ok ? 0 : 1;
    }

    if (cmd == "watch") {
        // 实时监视器：CPU% / 内存 / 运行中的支持游戏（1 秒刷新；Ctrl+C 退出）
        // 可选参数：秒数（0/缺省 = 持续运行直到 Ctrl+C）；--top [N] 切换为进程级热点榜
        if (topBad) {
            std::puts(T("错误：--top 需要一个正整数 N（缺省 5）。",
                        "Error: --top requires a positive integer N (default 5)."));
            return 1;
        }
        AppCore core(cfg);
        int maxSecs = 0;
        if (!gameArg.empty()) maxSecs = std::atoi(gameArg.c_str());
        if (topMode) return RunWatchTop(core, topN, maxSecs);
        ULARGE_INTEGER idlePrev{}, kPrev{}, uPrev{};
        {
            FILETIME i{}, k{}, u{};
            if (!GetSystemTimes(&i, &k, &u)) {
                std::puts(T("监视器初始化失败。", "monitor init failed."));
                return 1;
            }
            idlePrev.HighPart = i.dwHighDateTime; idlePrev.LowPart = i.dwLowDateTime;
            kPrev.HighPart = k.dwHighDateTime; kPrev.LowPart = k.dwLowDateTime;
            uPrev.HighPart = u.dwHighDateTime; uPrev.LowPart = u.dwLowDateTime;
        }
        std::puts(T("实时监视（Ctrl+C 退出）：", "live monitor (Ctrl+C to exit):"));
        const ULONGLONG t0 = GetTickCount64();
        for (;;) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1000));
            FILETIME i{}, k{}, u{};
            if (!GetSystemTimes(&i, &k, &u)) break;
            ULARGE_INTEGER idle{}, kernel{}, user{};
            idle.HighPart = i.dwHighDateTime; idle.LowPart = i.dwLowDateTime;
            kernel.HighPart = k.dwHighDateTime; kernel.LowPart = k.dwLowDateTime;
            user.HighPart = u.dwHighDateTime; user.LowPart = u.dwLowDateTime;
            const ULONGLONG dI = idle.QuadPart - idlePrev.QuadPart;
            const ULONGLONG dK = kernel.QuadPart - kPrev.QuadPart;
            const ULONGLONG dU = user.QuadPart - uPrev.QuadPart;
            const ULONGLONG total = dK + dU;
            const int cpuPct = total > 0 ? static_cast<int>((total - dI) * 100 / total) : 0;
            idlePrev = idle; kPrev = kernel; uPrev = user;
            MEMORYSTATUSEX ms{};
            ms.dwLength = sizeof(ms);
            int ramPct = 0;
            if (GlobalMemoryStatusEx(&ms) && ms.ullTotalPhys > 0) {
                ramPct = static_cast<int>((ms.ullTotalPhys - ms.ullAvailPhys) * 100 / ms.ullTotalPhys);
            }
            std::string games;
            for (const auto& [id, pid] : core.RunningGames())
                games += gopt::GameIdToString(id) + "(" + std::to_string(pid) + ") ";
            if (games.empty()) games = T("无", "none");
            SYSTEMTIME st{};
            GetLocalTime(&st);
            std::printf("\r[%02u:%02u:%02u] CPU %3d%%  RAM %u%% (%llu/%llu MB)  %s: %s     ",
                        st.wHour, st.wMinute, st.wSecond, cpuPct, ramPct,
                        static_cast<unsigned long long>((ms.ullAvailPhys) / (1024ull * 1024ull)),
                        static_cast<unsigned long long>((ms.ullTotalPhys) / (1024ull * 1024ull)),
                        T("游戏", "games"), games.c_str());
            std::fflush(stdout);
            if (maxSecs > 0 && (GetTickCount64() - t0) / 1000 >= static_cast<ULONGLONG>(maxSecs)) break;
        }
        std::printf("\n");
        return 0;
    }

    if (cmd == "startup") {
        // Wise Care 365 风格：开机启动项管理（禁用=改名保留，可恢复）
        const std::string sub = gameArg;
        if (sub == "list") {
            const auto items = gopt::StartupManager::List();
            if (items.empty()) {
                std::puts(T("（没有启动项）", "(no startup entries)"));
                return 0;
            }
            std::printf("%s:\n", T("已启用启动项", "Enabled startup entries"));
            for (const auto& e : items) {
                std::printf("  [%s] %s = %s\n", e.hive.c_str(), e.name.c_str(), e.value.c_str());
            }
            return 0;
        }
        if (sub == "restore") {
            std::printf(T("已恢复 %d 个由本工具禁用的启动项。\n", "Restored %d entries.\n"),
                        gopt::StartupManager::RestoreAll());
            return 0;
        }
        if ((sub == "disable" || sub == "enable") && argc >= 4) {
            const bool ok = (sub == "disable") ? gopt::StartupManager::Disable(argv[3])
                                               : gopt::StartupManager::Enable(argv[3]);
            std::printf("%s %s: %s\n", (sub == "disable" ? T("禁用", "Disable") : T("启用", "Enable")),
                        argv[3], ok ? T("成功", "OK")
                                    : T("未找到/失败（可能已在另一 hive 或不存在）", "not found / failed"));
            return 0;
        }
        std::puts(T("用法: gopt_cli startup list | disable <名称> | enable <名称> | restore",
                    "Usage: gopt_cli startup list | disable <name> | enable <name> | restore"));
        return 0;
    }

    if (cmd == "clean") {
        std::puts(gopt::SystemTuner::CleanTemp().c_str());
        return 0;
    }
    if (cmd == "rollback") {
        AppCore core(cfg);
        std::puts(core.Rollback().c_str());
        return 0;
    }

    if (cmd == "rollback-all") {
        AppCore core(cfg);
        std::puts(core.RollbackAll().c_str());
        return 0;
    }

    PrintUsage();
    return 0;
}
