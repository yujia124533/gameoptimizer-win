# GameOptimizer 架构说明 (Architecture)

> 面向贡献者与审阅者的结构视角文档。相关文档：[CONTRIBUTING.md](CONTRIBUTING.md)（流程视角）、[BEFORE_USE_README.txt](BEFORE_USE_README.txt)（用户视角）、[COMMERCIALIZATION.md](COMMERCIALIZATION.md)（路线视角）。
>
> **定位约定**：稳定锚点是 **文件:函数名/符号名**；括号中的行号为 **v1.1.0 工作副本实测快照**，仅作定位参考（改代码后可能漂移，冲突时以函数名为准）。`tools/cli_main.cpp`、`src/gui/gopt_gui.cpp` 以及全部 `src/gui/ui_*.cpp` / `src/gui/page_*.cpp` 处于活跃开发中（行号漂移最快），故这些文件**只给函数/符号名、不给行号**。核心模块（hal / hardware / preset / rollback / config / tuning / license / core）的行号在 v1.1.0 快照中逐个复核过。

## 1. 模块地图 (Module Map)

单进程、无服务、无驱动的原生 Win32 应用：核心逻辑编译为静态库 `gameopt_core`（`CMakeLists.txt:17`），CLI 与 GUI 各自链接（`CMakeLists.txt:43` / `:47`）。

| 模块 | 关键文件 | 职责 | 关键入口 |
| --- | --- | --- | --- |
| 硬件探测 (hardware) | `src/hardware/HardwareDetector.*`、`HardwareProfile.*` | CPU 物理核/逻辑核/频率、GPU（DXGI）、内存、SMBIOS 主板序列号；结果缓存并格式化为可读文本 | `HardwareDetector::Detect()`（`HardwareDetector.cpp:370`，实际探测 `DetectFresh()` `:348`）、`HardwareProfile::ToString()`（`HardwareProfile.cpp:7`） |
| 硬件操作层 (hal) | `src/hal/HAL.*` | 优先级 / 亲和性 / 工作集 / 电源方案 / 代启动 / 帧延迟探测 / 提权检测；统一错误文本 | `HAL::SetProcessPriority`（`HAL.cpp:100`）、`SetProcessAffinity`（`:128`）、`SetProcessWorkingSet`（`:149`）、`ActivatePowerScheme`（`:186`）、`LaunchGameSuspended`（`:233`）、`IsElevated`（`:319`） |
| 预设 (preset) | `src/preset/GamePreset.h`、`GameOptimizationPreset.cpp` | 8 款游戏 id / 显示名 / 可执行名 / 静态策略；按硬件指纹降级并算出亲和性掩码 | `GameOptimizationPreset::GetPreset`（`:50`）、`Resolve`（`:135`）、`ApplyHardwareDegradation`（`:149`）、`ComputeAffinityMask`（`:168`） |
| 回滚 (rollback) | `src/rollback/SecurityRollback.*` | 应用前快照（`SavePoint`）、多级撤销栈、磁盘持久化、心跳看门狗、稳定性判定；**只读快照历史**（解析持久化文件，不建目录、不写文件、错误状态与回滚隔离） | `CreateSavePoint`（`:463`）、`ApplyPreset`（`:507`）、`RollbackToLastSave`（`:553`）、`RollbackAll`（`:617`）、`StartWatchdog`（`:629`）、`IsSystemStable`（`:662`）、`SavepointList` / `RecentSavepoints` / `SavepointListError`、`SavepointFilePath`（`:453`）、`SaveFileDirW`（`:172`）/ `SaveFilePathNoCreate`（`:192`）、只读解析 `LoadSavePointsReadOnly`（`:321`） |
| 配置 (config) | `src/config/GameConfig.*` | 每游戏「优化启动」配置（exe 路径 / 参数 / 开关）持久化到 `%LOCALAPPDATA%\GameOptimizer\games.conf` | `ConfigPath`（`GameConfig.cpp:79`，拼接见 `:90`）、`Get`（`:93`）、`Set`（`:100`）、`Remove`（`:106`） |
| 系统调优 (tuning) | `src/tuning/SystemTuner.*`、`StartupManager.*` | 电源方案 + 处理器性能档（powercfg 官方别名）+ 调度优先级注册表；启动项枚举/禁用/启用/恢复；`%TEMP%` 安全清理 | `SystemTuner::Tune`（`SystemTuner.cpp:164`）、`Restore`（`:222`）、`CleanTemp`（`:254`）、`RecommendHighPerf`（`:130`）；`StartupManager::List`（`StartupManager.cpp:97`）、`Disable`（`:153`）、`Enable`（`:172`）、`RestoreAll`（`:198`） |
| 授权/指纹 (license) | `src/license/License.*`、`sha256.h` | 机器指纹计算与可选授权码校验；**所有功能免费，不做功能门控** | `ComputeMachineFingerprint`（`License.cpp:159`）、`Check`（`:193`）、`Activate`（`:209`）、`Generate`（`:185`） |
| 协调层 (core) | `src/core/AppCore.*` | 唯一业务编排点：探测 → 解析预设 → 快照 → 逐步应用 → 看门狗 → 回滚；向上暴露事件回调供 GUI 动画；暴露**只读快照历史**（薄门面：零逻辑、不缓存、不写文件） | `OptimizeForGame`（`AppCore.cpp:125`）、`OptimizeAuto`（`:274`）、`OptimizeAll`（`:336`）、`OptimizeSystem`（`:311`）、`Rollback`（`:268`）、`RollbackAll`（`:363`）、`IsStable`（`:389`）、`Savepoints`（`:373`）、`RecentSavepoints`（`:377`）、`SavepointsError`（`:381`）、`SavepointsFilePath`（`:385`） |
| 界面外壳 (gui shell) | `src/gui/gopt_gui.cpp` | 只做「外壳」：顶层窗口、5 页导航（`g_pages[5]` / `g_nav[5]`）、页容器（`STATIC` + `PageProc` 转发 `WM_COMMAND`）、底部日志与状态栏、托盘、单实例、中英双语、流程事件回投 | `WndProc`、`ShowPage`、`AddLog`、`UpdateFooter`、`PageProc`（页容器子类化）、托盘 `TrayAdd` / `TrayBalloon` |
| UI 基础层：令牌与主题 | `src/gui/ui_theme.*` | 设计令牌（30 语义色 × 浅/深、7 间距、7 字号、4 圆角、4 控件高度）、跟随系统主题、per-monitor 高 DPI（含降级链）、字体缓存、GDI/文本封装 | `UiThemeInit`、`UiThemeRefresh`、`UiSetThemeMode`、`UiColor` / `UiSp` / `UiFont`、`UiEnablePerMonitorDpi`、`UiOnDpiChanged`、`UiDpiForWindow` |
| UI 基础层：统一自绘控件 | `src/gui/ui_widgets.*` | 纯绘制控件（给 HDC + RECT + 状态就画，不建窗口、不装钩子）：按钮五态、卡片/标题/大号数字/分隔线、列表行（悬停/选中/斑马纹）、进度条、徽标/提示条、双缓冲 Paint、文本省略/换行 | `UiDrawButton`、`UiButtonStateFromDrawItem`、`UiDrawCard`、`UiDrawListRow`、`UiDrawProgress`、`UiDrawBadge`、`UIPaintBufferBegin/End`、`UiWidgetsSelfTest` |
| 页面模块 A | `src/gui/page_dashboard.*`、`src/gui/page_game.*` | 总览页（硬件/实时/曲线三卡 + 一键优化 + 一键回滚 + 只读优化历史）与游戏优化页（代启动表单 / 预设摘要 / 应用 / 保存 / 回滚）；在页容器内自建「页内面板」，就地分发命令 + 转发页容器 | `DashboardPageCreate` / `Layout` / `OnShow` / `Destroy` / `ApplyLanguage` / `ApplyTheme`、`DashboardPageCommand`、`StartBoost` / `StartRollback` / `RefreshHistory` / `ShowAbout`、`ComputeDashLayout` / `DashLayoutSelfCheck`、`GamePageCreate` / `GamePageCommand`、`ComputeGameLayout` / `GameLayoutSelfCheck` |
| 页面模块 B | `src/gui/page_tune.*`、`page_process.*`、`page_startup.*` | 系统调优 / 进程 / 启动项三页，各建自己的子窗口类（`GoptPageTune` / `GoptPageProcess` / `GoptPageStartup`），自处理 `WM_PAINT`/`WM_DRAWITEM`/`WM_COMMAND`/`WM_SIZE`/`WM_DPICHANGED`；系统动作全部经 `Hooks` | `PageTune::Create` / `Layout` / `FillParent` / `Show` / `Refresh` / `ApplyLabels` / `SelfCheck`、`PageProcess::Create` / `Refresh` / `SelectedPid`、`PageStartup::Create` / `Refresh` / `SelectedName` / `SelectedHive`、内部纯几何 `PlanLayout` + `RectsOverlap` |
| CLI (tools) | `tools/cli_main.cpp` | 全部子命令入口与使用说明（命令清单的唯一事实来源是文件内 `PrintUsage`）；`report` 诊断报告；`savepoints` 只读快照历史 | `PrintUsage`、各 `if (cmd == "...")` 分支、`BuildReport` 辅助、`RunSavepointsList` / `RunSavepointsShow` |
| 自检/构建 (tools) | `tools/verify_real.cpp`、`build_release.sh`、`build_w64devkit.ps1`、`check_version.ps1`、`gen_icon.ps1` | 真实进程机制自检（应用+恢复）；MSYS2/MinGW 发布构建与打包；本地便携构建；版本一致性门禁；图标生成 | `verify_real.cpp:36`（默认全流程 `:153` 输出 PASS/FAIL；跨进程两段模式 `save` `:40` / `rollback <pid>` `:70`）；`build_release.sh:31`–`:53` |

依赖方向（单向，无环）：

```
   tools/cli_main.cpp      src/gui/gopt_gui.cpp (外壳)
                            + src/gui/page_*.cpp (页面模块，经 Hooks)
                 \                     /
                  v                   v
                        +---------------------------+
                        |        AppCore (core)     |   唯一编排层
                        +---------------------------+
                          |      |      |      |
              +-----------+      |      |      +----------------+
              v                  v      v                       v
   HardwareDetector/HWProfile  preset  rollback  tuning/license/config
              \                  |      |             /
               +-----------------+------+------------+
                                 v
                          HAL (官方 API 唯一出口)
                                 |
                          Windows 用户态 API
```

> UI 基础层（`ui_theme.*` / `ui_widgets.*`）只提供令牌与绘制，不参与业务依赖链：页面模块引用它，它不反向引用页面模块与 AppCore。

## 2. GUI 分层（外壳 / 令牌与控件 / 页面模块）

GUI 是「薄外壳 + 设计令牌与统一控件 + 自包含页面模块」三层；**页面模块只负责画界面，所有对系统的修改都经 `Hooks` / 工作线程自建 `AppCore` → `HAL`、`SystemTuner`、`StartupManager`**（薄门面隔离；现存少量只读或用户级直连见 2.3 的「可审计例外清单」）：

```
        src/gui/gopt_gui.cpp（外壳：导航 / 页容器 / 日志 / 状态栏 / 托盘 / 单实例 / 语言）
                    |
   +----------------+-------------------------+
   v                                          v
 ui_theme.*（令牌 / 主题 / 高 DPI / 字体）   ui_widgets.*（纯绘制控件 + 双缓冲）
        ^   ^
        |   |
 page_dashboard.* + page_game.*（页面组 A：页内面板就地分发 + 转发页容器）
 page_tune.* / page_process.* / page_startup.*（页面组 B：自包含子窗口，自建窗口类）
        |  Hooks 回调 / 工作线程自建 AppCore（页面修改系统的唯一出口；例外见 2.3）
        v
 AppCore（编排） → HAL（官方 API 唯一出口） → Windows 用户态 API
```

### 2.1 三层职责与文件地图

| 层 | 文件 | 职责 | 直接使用的 API 族 |
| --- | --- | --- | --- |
| 外壳 (shell) | `src/gui/gopt_gui.cpp` | 顶层窗口与标题栏、5 页导航、页容器、日志区、状态栏、托盘、单实例、语言切换、页面装配与流程事件回投 | user32 / shell32 / advapi32 / gdi32 / psapi（只读查询） |
| 令牌与主题 | `src/gui/ui_theme.h` / `.cpp` | 30 个语义色角色 × 浅/深、7 间距（8px 栅格）、7 字号、4 圆角、4 控件高度；跟随系统主题；per-monitor 高 DPI；字体缓存；文本与圆角 GDI 封装 | user32 / gdi32 / advapi32 / kernel32（只读注册表） |
| 统一控件 | `src/gui/ui_widgets.h` / `.cpp` | 纯绘制：按钮五态、卡片/标题/大号数字/分隔线、列表行（悬停/选中/斑马纹）、进度条、徽标/提示条、双缓冲 Paint、文本省略与换行 | gdi32 + user32（尺寸/颜色全部取令牌） |
| 页面模块 A | `src/gui/page_dashboard.h` / `.cpp`、`page_game.h` / `.cpp` | 总览页（硬件/实时/曲线三卡 + 一键优化 + 回滚最近一次 + 只读优化历史）、游戏优化页（代启动表单 / 预设摘要 / 应用 / 保存 / 回滚）。在宿主页容器内再建「页内面板」，面板就地分发命令并把 `WM_COMMAND` 转发给页容器 | user32 / gdi32 / kernel32 / advapi32（advapi32 仅用于 HKCU Run 开机自启）；优化/回滚一律在**工作线程自建 `AppCore`** 执行 |
| 页面模块 B | `src/gui/page_tune.*`、`page_process.*`、`page_startup.*` | 系统调优 / 进程 / 启动项三页，各建自己的子窗口类（`GoptPageTune` / `GoptPageProcess` / `GoptPageStartup`），自处理 `WM_PAINT` / `WM_DRAWITEM` / `WM_COMMAND` / `WM_SIZE` / `WM_DPICHANGED` | user32 / gdi32；进程枚举、优先级、注册表、电源、清理全部经 `Hooks` |

构建入口：上述 2 个基础层文件与 5 个页面实现**只进 GUI 目标**（CLI 与自检程序不链接，避免引入 comctl32 等 UI 依赖），三处都已登记 —— `CMakeLists.txt`（`gopt_gui` 目标源列表）、`tools/build_w64devkit.ps1`（`$UISRCS`，只加在 GUI 链接行）、`tools/build_release.sh`（`UISRCS`，只加在 GUI 编译行）。

### 2.2 不变量 1：页面控件通知必须经页容器转发

v1.0.19 的真实缺陷是**页容器是 `STATIC`，它不转发子控件的 `WM_COMMAND`**，于是「页内按钮点了没反应」。修复与 v1.1.0 的加固构成四层保证，任何重构都不得移除：

1. **页容器层（必须保留）**：外壳把 5 个页容器的窗口过程子类化为 `PageProc`（`SetWindowLongPtrW` 保存原过程 + `CallWindowProcW` 链式调用），任何 `WM_COMMAND` **必须**转发给主窗口（`SendMessageW(GetParent(h), m, w, l)`，`gopt_gui.cpp` 的 `PageProc`）。这是本条不变量的落点。
2. **页面层（v1.1.0 加固）**：页面不再依赖宿主是否记得路由新 ID —— 页面组 A 的页内面板**就地**调用 `DashboardPageCommand` / `GamePageCommand` 完成分发，再把 `WM_COMMAND` 继续转发给页容器，保持既有 `PageProc → 主窗口` 链路可用；页面组 B 的页面子窗口本身就是其控件的直接父窗口，控件通知天然落到该窗口。
3. **幂等保护**：页面组 A 的「面板就地分发 + 宿主路由」可能让同一条 `WM_COMMAND` 走两条路径，面板用**确定性的** `RouteGuard`（先清守卫 → 就地分发 → 置本条 `id`/`code` → 再转发；同一 `id`/`code` 的第二次到达被拦下并消费，不依赖计时）保证只执行一次。
4. **宿主禁止**：外壳**不要**为页面控件 ID（1201–1206 / 1301–1310 / 4001–4004 / 4101–4104 / 4201–4205）写处理分支，否则会与页面就地分发重复执行。

### 2.3 不变量 2：页面只画界面，系统调用走薄门面

页面模块**不直接**修改进程属性、切换电源方案、删除文件、改动启动项：这些动作一律通过 `Hooks` 结构体（页面组 B）或在工作线程自建 `AppCore`（页面组 A）完成，最终落到 `HAL` / `SystemTuner` / `StartupManager`。

| 页面 | 薄门面（唯一出口） | 触发的能力 | 内核落点 |
| --- | --- | --- | --- |
| 总览页 / 游戏优化页 | `PageHostHooks{sharedCore, AppendLog, SetStatus}` + 工作线程自建 `AppCore` | 一键优化、应用预设、回滚 | `AppCore::OptimizeAll` / `OptimizeForGame` / `Rollback` → `HAL::SetProcessPriority` 等 |
| 系统调优页 | `PageTune::Hooks{tune, restoreTune, cleanTemp, activePowerSchemeName, isElevated}` | 电源方案 / 处理器档 / 调度优先级 / `%TEMP%` 清理 | `SystemTuner::Tune` / `Restore` / `CleanTemp` |
| 进程页 | `PageProcess::Hooks{snapshot, setPriority, receipt}` | 进程枚举 + 优先级调整（上限 HIGH） | 宿主 `OpenProcess` + `HAL::SetProcessPriority` |
| 启动项页 | `PageStartup::Hooks{list, disable, enable, restoreAll, receipt}` | Run 键枚举 / 禁用 / 启用 / 还原 | `StartupManager::List` / `Disable` / `Enable` / `RestoreAll` |

红线因此是**单点集中**的：优先级白名单、快照、24 小时保留、启动项备份都在 HAL / SystemTuner / StartupManager 内；`page_tune` / `page_process` / `page_startup` 三页**没有任何**直连系统调用（文件头显式声明）。

**可审计例外清单**（页面模块里现存的直连调用，全部为只读查询或用户级、可逆项；新增直连必须在此登记）：

| 位置（文件:符号） | 直连调用 | 性质 | 可接受的理由 |
| --- | --- | --- | --- |
| `page_dashboard.cpp` `ShowAbout` | `HAL::IsElevated` / `HAL::QueryActivePowerScheme` / `HAL::PowerSchemeName` / `AppCore::SavepointsFilePath` | 只读查询 | 不改任何系统状态；查询失败降级为「未知 / unknown」，不影响优化与回滚 |
| `page_dashboard.cpp` `AutoStartExists` / `AutoStartSet` | `RegGetValueW` / `RegCreateKeyExW` / `RegSetValueExW` / `RegDeleteValueW`（`HKCU\...\Run`） | 用户级、有显式勾选开关、可逆 | 官方 advapi32；只写当前用户 Run 项，取消勾选即删除；与 v1.0.19 行为一致（功能本身免费、可回滚） |
| `page_game.cpp` `LoadConfigToUI` / `SaveSettings` / `StartApply` | `GameConfig::Get` / `GameConfig::Set` | 用户级配置文件（`%LOCALAPPDATA%\GameOptimizer\games.conf`） | 不写注册表、不改系统设置；与 v1.0.19 行为一致 |
| `page_dashboard.cpp` `StartBoost` / `StartRollback`、`page_game.cpp` `StartApply` / `StartRollback` | `new AppCore(cfg)`（工作线程内） | 业务编排（非系统调用本体） | 不跨线程共享实例；UI 线程只用 `hooks.sharedCore` 做只读查询 |

### 2.4 不变量 3：布局纯整数、无重叠、可自检

- 每页布局都由**纯整数函数**算出，输入只有「客户区宽高 + 令牌快照 `PageMetrics`」，不依赖窗口句柄与 DC，因而可单测、不随 DPI 漂移：页面组 A 用 `ComputeDashLayout` / `ComputeGameLayout`，页面组 B 用内部 `PlanLayout(w, h, btnW[], btnN)`。
- 空间不足时把次要控件置为**空矩形**并 `ShowWindow(SW_HIDE)`，**绝不用重叠换空间**；`DashLayoutSelfCheck` / `GameLayoutSelfCheck` 做「同层两两不重叠 + 落在容器内 + 非负宽高」自检，失败时把首个冲突写进日志。页面组 B 的 `SelfCheck` 在多种分辨率下用 `RectsOverlap` 复算同一约束。
- 自检入口：`UiWidgetsSelfTest`（全部控件绘制路径）、`PageTune::SelfCheck` / `PageProcess::SelfCheck` / `PageStartup::SelfCheck`、`DashLayoutSelfCheck` / `GameLayoutSelfCheck`。

### 2.5 装配契约（外壳收口清单）

| 时机 | 页面组 A（总览 / 游戏优化） | 页面组 B（系统调优 / 进程 / 启动项） |
| --- | --- | --- |
| `WM_CREATE` | `DashboardPageCreate(g_pages[0], hooks)` / `GamePageCreate(g_pages[1], hooks)` | `PageTune::Create` / `PageProcess::Create` / `PageStartup::Create`（parent = 页容器或主窗口） |
| `WM_SIZE` | `DashboardPageLayout()` / `GamePageLayout()` | `FillParent(hwnd)` 或 `Layout(hwnd, rc)` |
| 切换页面 | `DashboardPageOnShow()` / `GamePageOnShow()` | `Show(hwnd, on)` |
| 语言切换 | `DashboardPageApplyLanguage()` / `GamePageApplyLanguage()` | `ApplyLabels(hwnd)` |
| 主题 / 高 DPI | `DashboardPageApplyTheme()` / `GamePageApplyTheme()` | `Refresh(hwnd)` / `ApplyLabels(hwnd)` + 重绘；DPI 变化由页面自己处理 `WM_DPICHANGED`（`UiOnDpiChanged()` 后重排） |
| `WM_DESTROY` | `DashboardPageDestroy()` / `GamePageDestroy()` | `Destroy(hwnd)` |

启动期（创建任何窗口之前）外壳还应调用 `UiEnablePerMonitorDpi()` 与 `UiThemeInit()`；系统主题变化后调用 `UiThemeRefresh()`，并按广播消息 `UiThemeChangedMessage()` 重绘。页面组 A 的面板带 1 秒定时器自愈重排：即使宿主漏调 `WM_SIZE`，也会在 1 秒内补齐面板尺寸。

### 2.6 线程模型：界面不阻塞

一键优化 / 应用优化 / 回滚都在**工作线程**执行：UI 线程只做「取配置 → 校验 → 置 busy 并禁用按钮 → 起 `std::thread` → 返回」；工作线程自建 `AppCore`（不跨线程共享实例，避免 mutable 错误字段与快照栈的数据竞争），通过 `PostMessage` 把 `FlowEvent` 与结果文本回投页面窗口；页面销毁时用 `PeekMessage` 抽干残留载荷并释放（不泄漏）。UI 线程只用 `hooks.sharedCore` 做只读查询。

## 3. 数据流 (Data Flow)

### 3.1 优化主链路（GUI 与 CLI 共用）

```
AppCore::OptimizeForGame(gameId)                       AppCore.cpp:125
  ├─ 探测硬件：HardwareDetector::Detect()（带缓存）      HardwareDetector.cpp:370
  ├─ 解析预设：GameOptimizationPreset::Resolve(id, hw)   GameOptimizationPreset.cpp:135
  │     └─ 硬件降级：核 ≤2 / 内存小 → 取消亲和性或降档    :149
  ├─ 定位目标进程：RunningGames() 或 --game-exe 代启动    AppCore.cpp:327 / HAL.cpp:233
  ├─ 快照：SecurityRollback::CreateSavePoint(pid)        AppCore.cpp:215 → SecurityRollback.cpp:463
  │     └─ 持久化到 %LOCALAPPDATA%\GameOptimizer\savepoints.txt（跨进程回滚）SaveFilePath :185
  ├─ 应用：SecurityRollback::ApplyPreset(pid, preset)    每步可解释（StepItem label/ok/elapsedMs）
  │     ├─ 优先级      HAL::SetProcessPriority            HAL.cpp:100（白名单校验 :33）
  │     ├─ CPU 亲和性  HAL::SetProcessAffinity            HAL.cpp:128
  │     ├─ 工作集      HAL::SetProcessWorkingSet          HAL.cpp:149
  │     ├─ 电源方案    HAL::ActivatePowerScheme           HAL.cpp:186（需管理员；默认关闭）
  │     └─ 帧延迟      HAL::SetDriverFrameLatency         HAL.cpp:307（未集成厂商 SDK → 降级跳过）
  ├─ 看门狗：StartWatchdog(WatchdogConfig)               AppCore.cpp:256 → SecurityRollback.cpp:629
  └─ 返回人类可读结果（失败/降级逐条说明）
```

设计要点：**任何一步失败都不中止整条链路**——失败项记入 `ApplyReport::failures` 并继续执行剩余步骤（`SecurityRollback.h:61-67`）；因此「部分生效 + 可回滚」是合法状态：GUI 页面用页内进度与步骤明细显示真实耗时与结果，日志区保留完整明细。

### 3.2 实时监视与系统调优链路

- 整机 CPU：`GetSystemTimes` 差值；内存：`GlobalMemoryStatusEx`；GUI 总览页每秒采样写入 48 秒环形缓冲（页面模块 `page_dashboard.cpp` 的 `SampleLive` / `DrawSpark`，宿主侧旧路径见 `gopt_gui.cpp` 的 `RefreshCpuLoad` / `UpdateDashboard`）；CLI 对应 `gopt_cli watch [秒数]`。
- 每进程 CPU%：`GetProcessTimes` 差值；内存：`GetProcessMemoryInfo`（页面组 B 的 `page_process.cpp` 只做展示，采样由宿主经 `Hooks::snapshot` 提供；宿主实现见 `gopt_gui.cpp` 的 `RefreshProcList`）。
- 系统调优：`AppCore::TuneSystem(bool)`（`AppCore.cpp:355`）→ `SystemTuner::Tune`（`SystemTuner.cpp:164`）：快照当前方案与 `Win32PrioritySeparation`（`:134`）→ 应用电源方案与处理器档（powercfg 官方别名 `:67`/`:69`）→ `Restore()`（`:222`）可还原。GUI 入口是 `PageTune::Hooks{tune, restoreTune}`。
- 临时清理：`SystemTuner::CleanTemp`（`:254`）递归 `%TEMP%`，**24 小时内修改的文件一律保留**（`kKeepRecent100ns`，`SystemTuner.cpp:77`）、占用/锁定项跳过，报告「已清理/跳过/保留」。GUI 入口是 `PageTune::Hooks::cleanTemp`，且页面侧额外要求**二次确认**（`ArmClean` / `DoCleanStep`）。
- 只读快照历史：`AppCore::RecentSavepoints(n)`（`AppCore.cpp:377`）→ `SecurityRollback::RecentSavepoints` → 只读解析 `LoadSavePointsReadOnly`（`SecurityRollback.cpp:321`，不建目录）；GUI 总览页每 5 秒刷新历史列表（`page_dashboard.cpp` 的 `RefreshHistory`），CLI 用 `RunSavepointsList` / `RunSavepointsShow`。

### 3.3 回滚与看门狗链路

```
看门狗线程（每 sleepMs 心跳） SecurityRollback.cpp:629
  └─ 抖动超阈值且宽限期内连续命中 → systemStable_ = false
       └─ AppCore::IsStable() == false  AppCore.cpp:389
            └─ 调用方执行 AppCore::Rollback()  AppCore.cpp:268（CLI apply/optimize 轮询 30s 自动回滚）
                 └─ SecurityRollback::RollbackToLastSave()  SecurityRollback.cpp:553（逆序恢复，单步失败继续）
GUI「回滚最近一次优化」：总览页控件 1205 → page_dashboard.cpp 的 StartRollback（工作线程）→ AppCore::Rollback()
GUI「回滚」：游戏优化页控件 1310 → page_game.cpp 的 StartRollback（工作线程）→ AppCore::Rollback()
CLI：gopt_cli rollback / rollback-all → RollbackToLastSave / RollbackAll（SecurityRollback.cpp:617）
```

只读历史与回滚是两条**互不影响**的路径：`SavepointList` / `RecentSavepoints` 只解析持久化文件，错误写在独立的 `savepointsError_`，既不触碰回滚错误状态，也不修改内存撤销栈。

### 3.4 GUI / CLI 共用入口

两者都不直接调用 HAL，只通过 `AppCore`（`AppCore.h:23`）：
- GUI 在后台线程调用 `OptimizeAll` 并通过 `FlowCallback` 更新流程面板，UI 不阻塞（`AppCore.h:36`）；
- CLI 在 `apply` / `optimize` 后轮询 `IsStable()` 最多 30 秒，异常则自动回滚；
- 配置开关统一由 `AppConfig` 承载（`AppCore.h:16-20`），其中 `allowPowerSchemeSwitch` **默认 false**（红线：电源切换需显式 `--power` + 管理员）。

## 4. 红线落点表 (Safety Red Lines → Code)

| # | 红线 | 代码落点（文件:函数/行号） | 机制 |
| --- | --- | --- | --- |
| R1 | 仅官方用户态 API | `src/hal/HAL.cpp:106` `SetPriorityClass`、`:140` `SetProcessAffinityMask`、`:159` `SetProcessWorkingSetSize`、`:195` `PowerSetActiveScheme`、`:248` `CreateProcessW`；`src/hardware/HardwareDetector.cpp:164` `GetLogicalProcessorInformationEx`、`:240` `CreateDXGIFactory1`、`:297` `GlobalMemoryStatusEx`、`:303` `GetSystemFirmwareTable`；`src/tuning/SystemTuner.cpp:67/69` powercfg 官方别名、`:150` `Win32PrioritySeparation` 注册表 | 无第三方 SDK 硬依赖；厂商库仅在 `HAL::IsDriverFrameLatencySupported`（`HAL.cpp:285`）用 `LoadLibraryW`（`:289`/`:297`）**探测是否存在**后立即 `FreeLibrary`，实际写入接口未集成、一律降级跳过（`:307-315`） |
| R2 | 无注入 | 全仓库检索 `CreateRemoteThread` / `WriteProcessMemory` / `VirtualAllocEx` / `QueueUserAPC` / `SetThreadContext` → **0 命中**（复核命令见第 6 节）；进程句柄只经 `OpenProcess`（`AppCore.cpp` / `SecurityRollback.cpp` 内），属性修改只经官方 `Set*` API（`HAL.cpp:100-172`） | 只改内核已暴露的调度/内存属性，不写目标进程内存、不创建远程线程；`HAL.h:5` 显式声明禁止；UI 层连 `OpenProcess` 都不出现——页面模块经 `Hooks` 交宿主执行（见 2.3 表） |
| R3 | 无内核 Hook / 无驱动 | 全仓库检索 `SetWindowsHookEx` / `NtLoadDriver` / `OpenSCManager` / `CreateService` → **0 命中**；`*.sys` / `*.inf` **0 个文件**；构建目标仅 `gameopt_core`/`gopt_cli`/`gopt_gui`（`CMakeLists.txt:17/43/47`） | 纯用户态 EXE，安装包只复制文件并创建快捷方式（`resources/installer.cpp`） |
| R4 | 优先级上限 `HIGH_PRIORITY_CLASS` | `src/hal/HAL.cpp:33-44` `IsValidPriorityClass` 白名单（`:42` 显式拒绝 REALTIME）→ 强制入口 `HAL::SetProcessPriority`（`:100-111`）；声明 `src/hal/HAL.h:6`；预设字段 `src/preset/GamePreset.h:21`，实际取值 `GameOptimizationPreset.cpp:54/64/74/85/95/105/114/123`（仅 HIGH / ABOVE_NORMAL）；CLI `gopt_cli prio` 仅五档 | 采用**白名单**而非黑名单：未知值与 REALTIME 一律返回 false 并记录原因 |
| R5 | 可回滚（快照 + 看门狗） | 快照 `SecurityRollback::CreateSavePoint`（`SecurityRollback.cpp:463`）、逆序恢复 `RollbackToLastSave`（`:553`）、全量 `RollbackAll`（`:617`）、可写持久化 `SaveFilePath`（`:185`，文件名 `savepoints.txt`，会建目录）/ 只读同址路径 `SaveFilePathNoCreate`（`:192`，查询零副作用）、看门狗 `StartWatchdog`（`:629`）/ `IsSystemStable`（`:662`）；编排 `AppCore.cpp:215/256/270/363`；入口 `gopt_cli rollback` / `rollback-all`、GUI 总览页「回滚最近一次优化」（页面控件 ID 1205）/ 游戏优化页「回滚」（1310） | 先快照后修改；快照落盘故跨进程有效；单步回滚失败继续恢复剩余步骤；看门狗异常自动回滚；只读历史查询（`SavepointList` / `SavepointListError`）与回滚路径的错误状态互相隔离，查询永不改变快照文件 |

配套约束（同属红线族，但非独立一行）：
- **所有变更必须可解释**——`SecurityRollback::ApplyReport`（`SecurityRollback.h:61-67`）逐项记录 `label/ok/elapsedMs` 与失败原因；`AppCore::FlowEvent`（`AppCore.h:28-35`）把每步真实耗时上报 GUI 流程面板与 CLI 日志。
- **UI 与系统调用用薄门面隔离**：页面模块不直接修改进程属性、切换电源、删除文件或改动启动项，一律经 `Hooks` 交宿主（页面组 B）或在工作线程自建 `AppCore`（页面组 A）；红线（优先级白名单、快照、24h 保留、启动项备份）因此单点集中在 HAL / SystemTuner / StartupManager。页面模块里现存的少量直连（诊断只读查询、`HKCU\...\Run` 开机自启、用户级 `games.conf`）全部登记在 2.3 的**可审计例外清单**中，新增直连必须同时登记。

## 5. 扩展清单 (Extension Checklist)

### 5.1 新增一款游戏预设

1. `src/preset/GamePreset.h:11` `GameId` 枚举追加 id；
2. `src/preset/GameOptimizationPreset.cpp` 补 3 处：`GameIdToString`（`:24` 起）、`GameExeName`（`:38` 起）、`GetPreset` 的 `case`（`:53` 起，priority/affinity/workingSet 等字段）；
3. 若新游戏对硬件敏感，检查 `ApplyHardwareDegradation`（`:149`）是否需要新规则；
4. 对外可见性：CLI 帮助里的游戏列表（`tools/cli_main.cpp` 的 `PrintUsage`）与 GUI 游戏下拉（页面组 A 的 `page_game.cpp` 内 `kGames[8]`，被 `ComboRebuild` / `CurrentGameIndex` 使用）；外壳 `gopt_gui.cpp` 的同名数组只服务旧路径，收口后不再需要维护；
5. 自测：`gopt_cli apply <新游戏> --dry-run` 与 `gopt_cli status` 的预设概览。

### 5.2 新增一个 CLI 子命令

1. `tools/cli_main.cpp` 的 `PrintUsage` 补一行用法（中英双语同一函数内用 `T(zh, en)`）；
2. 在公共选项解析（`--game-exe` / `--power` / `--dry-run` / `--lang`）之后追加 `if (cmd == "xxx") { ... return 0; }` 分支，位置参照既有 `if (cmd == ...)` 链；
3. 业务逻辑写进 `AppCore`（`AppCore.h`）而不是 CLI，这样 GUI 也能复用；纯展示/聚合逻辑（如 `report` 的 `BuildReport`、`savepoints` 的 `RunSavepointsList`）可留在 CLI 层；
4. 涉及新的只读数据源时，先在 `AppCore` 上加**薄门面**（零逻辑、不缓存、不写文件），CLI 与 GUI 共用（参考 `Savepoints` / `RecentSavepoints` / `SavepointsError` / `SavepointsFilePath`）；
5. `docs/CONTRIBUTING.md` 与 README 的 CLI 表格同步；需要版本行为时读 `src/version.h`，不要硬编码。

### 5.3 新增一个 GUI 页面 / 页面模块

优先按**页面模块**（`src/gui/page_*.cpp`）落地，而不是继续往外壳里堆控件 —— 外壳只保留导航、页容器与装配：

1. 新建 `src/gui/page_xxx.h` / `.cpp`：`namespace gopt::ui`，自带窗口类名（如 `GoptPageXxx`）与控件 ID 段（避开已占用区间：外壳 201–205 / 301–311 / 401–404 / 501–605 / 700–701，页面组 A 1201–1206 / 1301–1310，页面组 B 4001–4004 / 4101–4104 / 4201–4205），只暴露 `Create` / `Layout`（或 `FillParent`）/ `Show` / `Refresh` / `ApplyLabels` / `Destroy` / `IsPageWindow` / `SelfCheck`；
2. **不碰**系统 API：需要的能力在 `Hooks` 结构体里声明 `std::function` 字段，由宿主接线到 `AppCore` / `HAL` / `SystemTuner` / `StartupManager`（薄门面隔离，见 2.3）；
3. 布局写纯整数函数（输入 `w, h` + `PageMetrics`）；间距/颜色/字号/圆角/控件高度**全部**取 `UiSp` / `UiColor` / `UiFont` / `UiRadiusPx` / `UiControlHeight`，不得出现硬编码像素或 RGB；`SelfCheck` 里复算「无重叠 + 不越界 + ID 唯一」；
4. 文案一律 `T(zh, en)`，`ApplyLabels` 负责语言切换后刷新静态文案；
5. 登记构建源列表：UI 源码**只进 GUI 目标** —— `CMakeLists.txt` 的 `gopt_gui` 源列表、`tools/build_w64devkit.ps1` 的 `$UISRCS`（只加在 GUI 链接行）、`tools/build_release.sh` 的 `UISRCS`（只加在 GUI 编译行）三处都要加，缺一处则 CI 或本地构建失败；
6. 外壳装配：`WM_CREATE` 创建 → `WM_SIZE` 布局 → 切页 `Show` → 语言 `ApplyLabels` → 主题/DPI `Refresh` → `WM_DESTROY` 销毁（时机表见 2.5）；
7. 通知路径：若页面挂在宿主提供的页容器（`STATIC` + `PageProc`）下，**不得依赖宿主识别你的新 ID** —— 页面要能就地分发命令，并保留「把 `WM_COMMAND` 转发给页容器」这一步；两条路径可能重复到达时用确定性的 `(id, code)` 守卫去重（参考页面组 A 的 `RouteGuard`，不依赖 `GetMessageTime` 之类的计时判据）；
8. 长任务放工作线程：UI 线程只置 busy / 禁用按钮，结果用 `PostMessage` 回投页面窗口，页面销毁前 `PeekMessage` 抽干并释放载荷（参考 `StartBoost` / `StartApply` / `StartRollback`）；
9. 自测：`-fsyntax-only` 编译新文件（命令见第 6 节）→ `UiWidgetsSelfTest` → 页面 `SelfCheck` → **真实点击**一遍（按钮 / 列表双击 / 语言切换 / 150% 缩放）。

## 6. 版本与校验锚点 (Version & Verification)

- 版本唯一来源：`src/version.h:7` `GOPT_VERSION_STR`；三处必须同步（`src/version.h`、`resources/resource.rc`、`resources/gui_resource.rc`），由 `tools/check_version.ps1` 门禁（CI 第一步，见 `.github/workflows/build-release.yml`）。
- 机制自检：`build\gopt_verify.exe`（`tools/verify_real.cpp:36`）默认模式打印 `RESULT: PASS/FAIL`（`:153`）；跨进程两段模式 `gopt_verify.exe save`（`:40`）→ `gopt_verify.exe rollback <pid>`（`:70`）验证快照落盘后的跨进程回滚（`:90`）。
- UI 层自检（不启动窗口即可跑的是语法检查；其余为运行期自检）：
  - 单文件语法/告警门禁（w64devkit 工具链，输出为空即通过）：
    ```bat
    g++ -std=c++17 -Wall -Wextra -fsyntax-only -Isrc -mwindows src\gui\ui_theme.cpp src\gui\ui_widgets.cpp
    g++ -std=c++17 -Wall -Wextra -fsyntax-only -Isrc -mwindows src\gui\page_dashboard.cpp src\gui\page_game.cpp
    g++ -std=c++17 -Wall -Wextra -fsyntax-only -Isrc -mwindows src\gui\page_tune.cpp src\gui\page_process.cpp src\gui\page_startup.cpp
    ```
  - 运行期：`UiWidgetsSelfTest(HDC)`（全部控件绘制路径出图）、`PageTune::SelfCheck` / `PageProcess::SelfCheck` / `PageStartup::SelfCheck`（多分辨率矩形与 ID 唯一性）、`DashLayoutSelfCheck` / `GameLayoutSelfCheck`（页面组 A 的无重叠自检，失败会把首个冲突写进日志）。
- 红线复核命令（PowerShell，任一有输出即为回归）：

```powershell
# R2/R3：注入与内核 Hook API 应 0 命中
Get-ChildItem src,tools -Recurse -Include *.cpp,*.h |
  Select-String -Pattern 'CreateRemoteThread|WriteProcessMemory|VirtualAllocEx|QueueUserAPC|SetThreadContext|SetWindowsHookEx|NtLoadDriver|OpenSCManager|CreateService'
# R3：不应存在驱动/安装信息文件
Get-ChildItem . -Recurse -Include *.sys,*.inf
# R4：优先级取值只允许白名单五档
Select-String -Path src\hal\HAL.cpp -Pattern 'case (IDLE|BELOW_NORMAL|NORMAL|ABOVE_NORMAL|HIGH)_PRIORITY_CLASS'
# UI 薄门面：页面组 B 三页不应有任何直连系统调用（只匹配真实调用，注释行不算）
Select-String -Path src\gui\page_tune.cpp,src\gui\page_process.cpp,src\gui\page_startup.cpp `
  -Pattern '(OpenProcess|SetPriorityClass|SetProcessAffinityMask|RegCreateKeyExW|RegSetValueExW|RegDeleteValueW|DeleteFile|PowerSetActiveScheme)\('
# 页面组 A 的直连应只有 2.3「可审计例外清单」里登记的那几处（诊断只读查询 / HKCU Run 开机自启 / games.conf）
Select-String -Path src\gui\page_dashboard.cpp,src\gui\page_game.cpp `
  -Pattern '(RegCreateKeyExW|RegSetValueExW|RegDeleteValueW|DeleteFile|PowerSetActiveScheme|OpenProcess)\('
```

- CI 产物校验：构建后断言 `build/gopt_cli.exe`、`build/gopt_gui.exe`、`build/GameOptimizer-setup.exe`、`release/GameOptimizer-portable.zip` 存在且非空，并以 `gopt_cli --version` 输出对照 `src/version.h`（不硬编码版本号）。
