#pragma once
// =============================================================================
// GameOptimizer v1.1.0 — 页面组 A：游戏优化页装配接口（src/gui/page_game.h）
// -----------------------------------------------------------------------------
// 复用 page_dashboard.h 里的页组 A 公共契约（PageHostHooks / 页内共享开关 /
// PageMetrics 几何令牌 / UTF-8 工具 / 自绘按钮悬停 / kPageMin*）。
//
// 【宿主装配清单】（与总览页同一套时机）
//   WM_CREATE : GamePageCreate(g_pages[1], hooks);      // hooks.sharedCore = g_core
//   WM_SIZE   : GamePageLayout();
//   切页      : GamePageOnShow();
//   语言切换  : GamePageApplyLanguage();
//   主题/DPI  : GamePageApplyTheme();
//   WM_DESTROY: GamePageDestroy();
//
// 【通知路径】与总览页完全一致：页容器 STATIC（PageProc 保持不动）→ 页内面板
//   （本页注册窗口类 gopt_ui_page_game）→ 面板就地分发命令，并把 WM_COMMAND 再转发
//   给页容器，保持既有 PageProc → 主窗口 链路。宿主不要为 1301..1310 写处理分支。
//
// 【本页功能】选游戏（8 款，双语名）、预设摘要（硬件降级后的真实参数 + 授权）、
//   代启动配置（exe 路径 + 参数 + 电源/帧延迟/工作集开关，含输入校验与提示）、
//   保存（GameConfig 持久化）、应用优化（工作线程 + 进度/步骤）、回滚（工作线程）。
// =============================================================================

#include "gui/page_dashboard.h"  // 页组 A 公共契约（同时带入 windows.h / ui_widgets.h）

namespace gopt {
namespace ui {

// -----------------------------------------------------------------------------
// 控件 ID（页面组 A 独占区间 1301..1310，与总览页 1201..1206 不重叠，
// 也不与既有 201-205/301-311/401-404/501-605/700-701 冲突）
// -----------------------------------------------------------------------------
enum GameCtlId {
    IDC_GAME_COMBO      = 1301,  // 选游戏（CBS_DROPDOWNLIST）
    IDC_GAME_PATH       = 1302,  // 代启动 exe 路径（EDIT）
    IDC_GAME_BROWSE     = 1303,  // 浏览…（GetOpenFileNameW）
    IDC_GAME_ARGS       = 1304,  // 启动参数（EDIT）
    IDC_GAME_POWERCHK   = 1305,  // 允许电源方案切换（与 AppConfig/GameConfig 同步）
    IDC_GAME_FRAMECHK   = 1306,  // 允许驱动级帧延迟
    IDC_GAME_WORKCHK    = 1307,  // 允许工作集策略
    IDC_GAME_SAVE       = 1308,  // 保存游戏设置（GameConfig::Set）
    IDC_GAME_APPLY      = 1309,  // 应用优化（主按钮，工作线程）
    IDC_GAME_ROLLBACK   = 1310,  // 回滚最近一次（工作线程）
};
constexpr int kGameCtlFirst = 1301;
constexpr int kGameCtlLast  = 1310;

// -----------------------------------------------------------------------------
// 游戏优化页布局（纯整数；空矩形 = 已降级隐藏，永不重叠/裁切）
// -----------------------------------------------------------------------------
struct GameLayout {
    RECT panel{};       // 面板整块（页容器客户区）
    RECT cardLaunch{};  // 左卡：代启动配置（表单：选游戏/路径/参数/开关）
    RECT cardPreset{};  // 右卡：预设摘要（只读）
    RECT areaRun{};     // 底部：动作按钮 + 进度 + 状态行 + 流程步骤

    RECT launchTitle{};
    RECT lblGame{}, cmbGame{};          // 控件 1301
    RECT lblPath{}, edtPath{}, btnBrowse{};  // 控件 1302 / 1303
    RECT lblArgs{}, edtArgs{};          // 控件 1304
    RECT chkPower{}, chkFrame{}, chkWork{};  // 控件 1305 / 1306 / 1307

    RECT presetTitle{}, presetBadge{};  // presetBadge：徽标右对齐锚区（绘制用）
    RECT presetDesc{};                  // 预设说明（自动换行）
    RECT presetParam[4]{};              // 优先级/亲和性/工作集/电源

    RECT btnApply{}, btnSave{}, btnRollback{};  // 控件 1309 / 1308 / 1310
    RECT progress{}, progressLabel{};   // 进度条 + 右侧步骤文案
    RECT statusLine{};                  // 页内状态行（可见反馈）
    RECT flowLine[3]{};                 // 最近步骤明细（空间不足时逐条退场）

    bool        valid = false;  // false = 内容区小于建议值，已隐藏次要控件
    bool        compact = false;  // true = 紧凑模式（<640x300 逻辑：状态行移入预设卡底部）
    std::string note;           // 降级说明（可读）
};
GameLayout ComputeGameLayout(int w, int h, const PageMetrics& m);
// 无重叠自检：同层两两不重叠 + 落在所属容器内 + 非负宽高
bool GameLayoutSelfCheck(const GameLayout& L, std::string* detail);

// -----------------------------------------------------------------------------
// 游戏优化页装配接口（宿主按文件头「装配清单」调用）
// -----------------------------------------------------------------------------
bool GamePageCreate(HWND pageContainer, const PageHostHooks& hooks);
void GamePageDestroy();
void GamePageLayout();
void GamePageOnShow();
bool GamePageCommand(int id, int code);  // 页面内部使用（宿主**不要**调用）
void GamePageApplyLanguage();
void GamePageApplyTheme();
void GamePageRefresh();

}  // namespace ui
}  // namespace gopt
