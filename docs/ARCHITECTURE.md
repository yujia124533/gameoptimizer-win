# GameOptimizer 架构说明 (Architecture)

> 面向贡献者与审阅者的结构视角文档。相关文档：[CONTRIBUTING.md](CONTRIBUTING.md)（流程视角）、[BEFORE_USE_README.txt](BEFORE_USE_README.txt)（用户视角）、[COMMERCIALIZATION.md](COMMERCIALIZATION.md)（路线视角）。
>
> **定位约定**：稳定锚点是 **文件:函数名/符号名**；括号中的行号为 **v1.0.18 工作副本实测快照**，仅作定位参考。`tools/cli_main.cpp` 与 `src/gui/gopt_gui.cpp` 处于活跃开发中（行号漂移最快），故这两处只给函数/符号名。核心模块（hal / hardware / preset / rollback / config / tuning / license / core）在 v1.0.18 快照中行号稳定。

## 1. 模块地图 (Module Map)

单进程、无服务、无驱动的原生 Win32 应用：核心逻辑编译为静态库 `gameopt_core`（`CMakeLists.txt:17`），CLI 与 GUI 各自链接（`CMakeLists.txt:43` / `:47`）。

| 模块 | 关键文件 | 职责 | 关键入口 |
| --- | --- | --- | --- |
| 硬件探测 (hardware) | `src/hardware/HardwareDetector.*`、`HardwareProfile.*` | CPU 物理核/逻辑核/频率、GPU（DXGI）、内存、SMBIOS 主板序列号；结果缓存并格式化为可读文本 | `HardwareDetector::Detect()`（`HardwareDetector.cpp:370`，实际探测 `DetectFresh()` `:348`）、`HardwareProfile::ToString()`（`HardwareProfile.cpp:7`） |
| 硬件操作层 (hal) | `src/hal/HAL.*` | 优先级 / 亲和性 / 工作集 / 电源方案 / 代启动 / 帧延迟探测 / 提权检测；统一错误文本 | `HAL::SetProcessPriority`（`HAL.cpp:100`）、`SetProcessAffinity`（`:128`）、`SetProcessWorkingSet`（`:149`）、`ActivatePowerScheme`（`:186`）、`LaunchGameSuspended`（`:233`）、`IsElevated`（`:319`） |
| 预设 (preset) | `src/preset/GamePreset.h`、`GameOptimizationPreset.cpp` | 8 款游戏 id / 显示名 / 可执行名 / 静态策略；按硬件指纹降级并算出亲和性掩码 | `GameOptimizationPreset::GetPreset`（`:50`）、`Resolve`（`:135`）、`ApplyHardwareDegradation`（`:149`）、`ComputeAffinityMask`（`:168`） |
| 回滚 (rollback) | `src/rollback/SecurityRollback.*` | 应用前快照（`SavePoint`）、多级撤销栈、磁盘持久化、心跳看门狗、稳定性判定 | `CreateSavePoint`（`:173`）、`ApplyPreset`、`RollbackToLastSave`（`:263`）、`RollbackAll`（`:327`）、`StartWatchdog`（`:339`）、`IsSystemStable`（`:372`） |
| 配置 (config) | `src/config/GameConfig.*` | 每游戏「优化启动」配置（exe 路径 / 参数 / 开关）持久化到 `%LOCALAPPDATA%\GameOptimizer\games.conf` | `ConfigPath`（`GameConfig.cpp:79`，拼接见 `:90`）、`Get`（`:93`）、`Set`（`:100`）、`Remove`（`:106`） |
| 系统调优 (tuning) | `src/tuning/SystemTuner.*`、`StartupManager.*` | 电源方案 + 处理器性能档（powercfg 官方别名）+ 调度优先级注册表；启动项枚举/禁用/启用/恢复；`%TEMP%` 安全清理 | `SystemTuner::Tune`（`SystemTuner.cpp:164`）、`Restore`（`:222`）、`CleanTemp`（`:254`）、`RecommendHighPerf`（`:130`）；`StartupManager::List`（`StartupManager.cpp:97`）、`Disable`（`:153`）、`Enable`（`:172`）、`RestoreAll`（`:198`） |
| 授权/指纹 (license) | `src/license/License.*`、`sha256.h` | 机器指纹计算与可选授权码校验；**所有功能免费，不做功能门控** | `ComputeMachineFingerprint`（`License.cpp:159`）、`Check`（`:193`）、`Activate`（`:209`）、`Generate`（`:185`） |
| 协调层 (core) | `src/core/AppCore.*` | 唯一业务编排点：探测 → 解析预设 → 快照 → 逐步应用 → 看门狗 → 回滚；向上暴露事件回调供 GUI 动画 | `OptimizeForGame`（`AppCore.cpp:125`）、`OptimizeAuto`（`:274`）、`OptimizeAll`（`:336`）、`OptimizeSystem`（`:311`）、`Rollback`（`:268`）、`RollbackAll`（`:363`）、`IsStable`（`:370`） |
| 界面 (gui) | `src/gui/gopt_gui.cpp` | 原生 Win32 GUI：五页导航（当前 `g_pages[5]` / `g_nav[5]`）、实时卡片与 48 秒曲线、进程/启动项列表、托盘、单实例、中英双语 | `ShowPage`、`RefreshCpuLoad`、`RefreshProcList`、`RefreshStartupList`、`UpdateDashboard`、`UpdateFooter`、`LastOptimizeTime`、`WM_COMMAND` 分发 |
| CLI (tools) | `tools/cli_main.cpp` | 全部子命令入口与使用说明（命令清单的唯一事实来源是文件内 `PrintUsage`）；`report` 诊断报告 | `PrintUsage`、各 `if (cmd == "...")` 分支、`BuildReport` 辅助 |
| 自检/构建 (tools) | `tools/verify_real.cpp`、`build_release.sh`、`build_w64devkit.ps1`、`check_version.ps1`、`gen_icon.ps1` | 真实进程机制自检（应用+恢复）；MSYS2/MinGW 发布构建与打包；本地便携构建；版本一致性门禁；图标生成 | `verify_real.cpp:36`（默认全流程 `:153` 输出 PASS/FAIL；跨进程两段模式 `save` `:40` / `rollback <pid>` `:70`）；`build_release.sh:31`–`:53` |

依赖方向（单向，无环）：

```
                 tools/cli_main.cpp        src/gui/gopt_gui.cpp
                          \                        /
                           v                      v
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

## 2. 数据流 (Data Flow)

### 2.1 优化主链路（GUI 与 CLI 共用）

```
AppCore::OptimizeForGame(gameId)                       AppCore.cpp:125
  ├─ 探测硬件：HardwareDetector::Detect()（带缓存）      HardwareDetector.cpp:370
  ├─ 解析预设：GameOptimizationPreset::Resolve(id, hw)   GameOptimizationPreset.cpp:135
  │     └─ 硬件降级：核 ≤2 / 内存小 → 取消亲和性或降档    :149
  ├─ 定位目标进程：RunningGames() 或 --game-exe 代启动    AppCore.cpp:327 / HAL.cpp:233
  ├─ 快照：SecurityRollback::CreateSavePoint(pid)        AppCore.cpp:215 → SecurityRollback.cpp:173
  │     └─ 持久化到 %LOCALAPPDATA%\GameOptimizer\savepoints.txt（跨进程回滚）SecurityRollback.cpp:104
  ├─ 应用：SecurityRollback::ApplyPreset(pid, preset)    每步可解释（StepItem label/ok/elapsedMs）
  │     ├─ 优先级      HAL::SetProcessPriority            HAL.cpp:100（白名单校验 :33）
  │     ├─ CPU 亲和性  HAL::SetProcessAffinity            HAL.cpp:128
  │     ├─ 工作集      HAL::SetProcessWorkingSet          HAL.cpp:149
  │     ├─ 电源方案    HAL::ActivatePowerScheme           HAL.cpp:186（需管理员；默认关闭）
  │     └─ 帧延迟      HAL::SetDriverFrameLatency         HAL.cpp:307（未集成厂商 SDK → 降级跳过）
  ├─ 看门狗：StartWatchdog(WatchdogConfig)               AppCore.cpp:256 → SecurityRollback.cpp:339
  └─ 返回人类可读结果（失败/降级逐条说明）
```

设计要点：**任何一步失败都不中止整条链路**——失败项记入 `ApplyReport::failures` 并继续执行剩余步骤（`SecurityRollback.h:39`、`:45-51`）；因此「部分生效 + 可回滚」是合法状态，GUI 用流程面板逐步显示真实耗时与结果。

### 2.2 实时监视与系统调优链路

- 整机 CPU：`GetSystemTimes` 差值；内存：`GlobalMemoryStatusEx`；GUI 每秒采样写入 48 秒环形缓冲（`gopt_gui.cpp` 的 `RefreshCpuLoad`，快照行 `:295`/`:315`/`:320`；`UpdateDashboard` 渲染）；CLI 对应 `gopt_cli watch [秒数]`。
- 每进程 CPU%：`GetProcessTimes` 差值；内存：`GetProcessMemoryInfo`（`RefreshProcList`，快照行 `:358`/`:366`）。
- 系统调优：`AppCore::TuneSystem(bool)`（`AppCore.cpp:355`）→ `SystemTuner::Tune`（`SystemTuner.cpp:164`）：快照当前方案与 `Win32PrioritySeparation`（`:134`）→ 应用电源方案与处理器档（powercfg 官方别名 `:67`/`:69`）→ `Restore()`（`:222`）可还原。
- 临时清理：`SystemTuner::CleanTemp`（`:254`）递归 `%TEMP%`，**24 小时内修改的文件一律保留**、占用/锁定项跳过，报告「已清理/跳过/保留」。

### 2.3 回滚与看门狗链路

```
看门狗线程（每 sleepMs 心跳） SecurityRollback.cpp:339
  └─ 抖动超阈值且宽限期内连续命中 → systemStable_ = false
       └─ AppCore::IsStable() == false  AppCore.cpp:370
            └─ 调用方执行 AppCore::Rollback()  AppCore.cpp:268（CLI apply/optimize 轮询 30s 自动回滚）
                 └─ SecurityRollback::RollbackToLastSave()  SecurityRollback.cpp:263（逆序恢复，单步失败继续）
GUI「回滚」：IDC_ROLLBACK（创建于页面控件段、处理于 WM_COMMAND 的 `id == IDC_ROLLBACK`）→ AppCore::Rollback()
CLI：gopt_cli rollback / rollback-all → RollbackToLastSave / RollbackAll（SecurityRollback.cpp:327）
```

### 2.4 GUI / CLI 共用入口

两者都不直接调用 HAL，只通过 `AppCore`（`AppCore.h:23`）：
- GUI 在后台线程调用 `OptimizeAll` 并通过 `FlowCallback` 更新流程面板，UI 不阻塞（`AppCore.h:36`）；
- CLI 在 `apply` / `optimize` 后轮询 `IsStable()` 最多 30 秒，异常则自动回滚；
- 配置开关统一由 `AppConfig` 承载（`AppCore.h:16-20`），其中 `allowPowerSchemeSwitch` **默认 false**（红线：电源切换需显式 `--power` + 管理员）。

## 3. 红线落点表 (Safety Red Lines → Code)

| # | 红线 | 代码落点（文件:函数/行号） | 机制 |
| --- | --- | --- | --- |
| R1 | 仅官方用户态 API | `src/hal/HAL.cpp:106` `SetPriorityClass`、`:140` `SetProcessAffinityMask`、`:159` `SetProcessWorkingSetSize`、`:195` `PowerSetActiveScheme`、`:248` `CreateProcessW`；`src/hardware/HardwareDetector.cpp:164` `GetLogicalProcessorInformationEx`、`:240` `CreateDXGIFactory1`、`:297` `GlobalMemoryStatusEx`、`:303` `GetSystemFirmwareTable`；`src/tuning/SystemTuner.cpp:67/69` powercfg 官方别名、`:150` `Win32PrioritySeparation` 注册表 | 无第三方 SDK 硬依赖；厂商库仅在 `HAL::IsDriverFrameLatencySupported`（`HAL.cpp:285`）用 `LoadLibraryW`（`:289`/`:297`）**探测是否存在**后立即 `FreeLibrary`，实际写入接口未集成、一律降级跳过（`:307-315`） |
| R2 | 无注入 | 全仓库检索 `CreateRemoteThread` / `WriteProcessMemory` / `VirtualAllocEx` / `QueueUserAPC` / `SetThreadContext` → **0 命中**（复核命令见第 5 节）；进程句柄只经 `OpenProcess`（`AppCore.cpp` / `SecurityRollback.cpp` 内），属性修改只经官方 `Set*` API（`HAL.cpp:100-172`） | 只改内核已暴露的调度/内存属性，不写目标进程内存、不创建远程线程；`HAL.h:5` 显式声明禁止 |
| R3 | 无内核 Hook / 无驱动 | 全仓库检索 `SetWindowsHookEx` / `NtLoadDriver` / `OpenSCManager` / `CreateService` → **0 命中**；`*.sys` / `*.inf` **0 个文件**；构建目标仅 `gameopt_core`/`gopt_cli`/`gopt_gui`（`CMakeLists.txt:17/43/47`） | 纯用户态 EXE，安装包只复制文件并创建快捷方式（`resources/installer.cpp`） |
| R4 | 优先级上限 `HIGH_PRIORITY_CLASS` | `src/hal/HAL.cpp:33-44` `IsValidPriorityClass` 白名单（`:42` 显式拒绝 REALTIME）→ 强制入口 `HAL::SetProcessPriority`（`:100-111`）；声明 `src/hal/HAL.h:6`；预设字段 `src/preset/GamePreset.h:21`，实际取值 `GameOptimizationPreset.cpp:54/64/74/85/95/105/114/123`（仅 HIGH / ABOVE_NORMAL）；CLI `gopt_cli prio` 仅五档 | 采用**白名单**而非黑名单：未知值与 REALTIME 一律返回 false 并记录原因 |
| R5 | 可回滚（快照 + 看门狗） | 快照 `SecurityRollback::CreateSavePoint`（`SecurityRollback.cpp:173`）、逆序恢复 `RollbackToLastSave`（`:263`）、全量 `RollbackAll`（`:327`）、持久化 `SaveFilePath`（`:93`，文件名 `:104`）、看门狗 `StartWatchdog`（`:339`）/`IsSystemStable`（`:372`）；编排 `AppCore.cpp:215/256/268/363`；入口 `gopt_cli rollback` / `rollback-all`、GUI `IDC_ROLLBACK`（`gopt_gui.cpp:103` 定义） | 先快照后修改；快照落盘故跨进程有效；单步回滚失败继续恢复剩余步骤；看门狗异常自动回滚 |

配套约束（同属红线族，但非独立一行）：**所有变更必须可解释**——`SecurityRollback::ApplyReport`（`SecurityRollback.h:45-51`）逐项记录 `label/ok/elapsedMs` 与失败原因；`AppCore::FlowEvent`（`AppCore.h:28-35`）把每步真实耗时上报 GUI 流程面板与 CLI 日志。

## 4. 扩展清单 (Extension Checklist)

### 4.1 新增一款游戏预设

1. `src/preset/GamePreset.h:11` `GameId` 枚举追加 id；
2. `src/preset/GameOptimizationPreset.cpp` 补 3 处：`GameIdToString`（`:24` 起）、`GameExeName`（`:38` 起）、`GetPreset` 的 `case`（`:53` 起，priority/affinity/workingSet 等字段）；
3. 若新游戏对硬件敏感，检查 `ApplyHardwareDegradation`（`:149`）是否需要新规则；
4. 对外可见性：CLI 帮助里的游戏列表（`tools/cli_main.cpp` 的 `PrintUsage`）与 GUI 游戏下拉（`gopt_gui.cpp` 的 `kGames[]`，被 `CurrentGame()` / `RefreshGameList()` 使用）；
5. 自测：`gopt_cli apply <新游戏> --dry-run` 与 `gopt_cli status` 的预设概览。

### 4.2 新增一个 CLI 子命令

1. `tools/cli_main.cpp` 的 `PrintUsage` 补一行用法（中英双语同一函数内用 `T(zh, en)`）；
2. 在公共选项解析（`--game-exe` / `--power` / `--dry-run` / `--lang`）之后追加 `if (cmd == "xxx") { ... return 0; }` 分支，位置参照既有 `if (cmd == ...)` 链；
3. 业务逻辑写进 `AppCore`（`AppCore.h`）而不是 CLI，这样 GUI 也能复用；纯展示/聚合逻辑（如 `report` 的 `BuildReport`）可留在 CLI 层；
4. `docs/CONTRIBUTING.md` 与 README 的 CLI 表格同步；需要版本行为时读 `src/version.h`，不要硬编码。

### 4.3 新增一个 GUI 页面

1. 数组扩容：文件顶部 `g_pages[5]` → `[6]`、`g_nav[5]` → `[6]`；
2. 控件 id：`IDC_NAV0..IDC_NAV4` 追加 `IDC_NAV5`，并同步自绘高亮（`WM_DRAWITEM` 中 `CtlID` 范围判断）与 `WM_COMMAND` 点击判断；
3. 创建：导航按钮循环（`IDC_NAV0 + i`）与页面容器循环自动覆盖新索引（`ShowPage` 负责显隐），页内控件按 `p = g_pages[5]` 追加；
4. 双语：`navZh[]` / `navEn[]` 扩为 6 项，控件文案统一用 `Label(...)` / `T(zh, en)`；
5. 布局：窗口缩放的 `MoveWindow` 遍历循环已按索引处理，通常无需改动；
6. 业务动作在 `WM_COMMAND` 内调用 `g_core->...`；长任务放到后台线程并通过 `FlowCallback` 回投 UI（参考 `IDC_BIGOPT` 与 `IDC_APPLY` 的现有写法）。

## 5. 版本与校验锚点 (Version & Verification)

- 版本唯一来源：`src/version.h:7` `GOPT_VERSION_STR`；三处必须同步（`src/version.h`、`resources/resource.rc`、`resources/gui_resource.rc`），由 `tools/check_version.ps1` 门禁（CI 第一步，见 `.github/workflows/build-release.yml`）。
- 机制自检：`build\gopt_verify.exe`（`tools/verify_real.cpp:36`）默认模式打印 `RESULT: PASS/FAIL`（`:153`）；跨进程两段模式 `gopt_verify.exe save`（`:40`）→ `gopt_verify.exe rollback <pid>`（`:70`）验证快照落盘后的跨进程回滚（`:90`）。
- 红线复核命令（PowerShell，任一有输出即为回归）：

```powershell
# R2/R3：注入与内核 Hook API 应 0 命中
Get-ChildItem src,tools -Recurse -Include *.cpp,*.h |
  Select-String -Pattern 'CreateRemoteThread|WriteProcessMemory|VirtualAllocEx|QueueUserAPC|SetThreadContext|SetWindowsHookEx|NtLoadDriver|OpenSCManager|CreateService'
# R3：不应存在驱动/安装信息文件
Get-ChildItem . -Recurse -Include *.sys,*.inf
# R4：优先级取值只允许白名单五档
Select-String -Path src\hal\HAL.cpp -Pattern 'case (IDLE|BELOW_NORMAL|NORMAL|ABOVE_NORMAL|HIGH)_PRIORITY_CLASS'
```

- CI 产物校验：构建后断言 `build/gopt_cli.exe`、`build/gopt_gui.exe`、`build/GameOptimizer-setup.exe`、`release/GameOptimizer-portable.zip` 存在且非空，并以 `gopt_cli --version` 输出对照 `src/version.h`（不硬编码版本号）。
