# GameOptimizer

> 硬件无感的 Windows 游戏优化工具 · 仅用官方 WinAPI，无注入、无内核 Hook，一键安全回滚

[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![C++](https://img.shields.io/badge/C%2B%2B-17-00599c.svg)](#)
[![Windows](https://img.shields.io/badge/Windows-x64-0078d4.svg)](#)
[![Build](https://github.com/yujia124533/gameoptimizer-win/actions/workflows/build-release.yml/badge.svg)](https://github.com/yujia124533/gameoptimizer-win/actions)
[![Release](https://img.shields.io/github/v/release/yujia124533/gameoptimizer-win?color=green)](https://github.com/yujia124533/gameoptimizer-win/releases)
[![Stars](https://img.shields.io/github/stars/yujia124533/gameoptimizer-win?color=yellow)](https://github.com/yujia124533/gameoptimizer-win)
[![GitHub](https://img.shields.io/badge/GitHub-repo-24292e)](https://github.com/yujia124533/gameoptimizer-win)

自动识别 CPU/GPU/内存 → 按游戏预设应用进程优先级/CPU 亲和性/工作集/电源策略 → 每次修改自动快照，秒级回滚。

## English (summary)

A hardware-agnostic Windows game optimizer built on official Win32 APIs only:

- **Safe**: no injection, no kernel hooks (fine with anti-cheat: ACE / VAC / Riot); priority capped at HIGH
- **Official APIs only**: `SetPriorityClass` / `SetProcessAffinityMask` / `SetProcessWorkingSetSize` / `PowerSetActiveScheme` (+ powercfg / registry)
- **8 supported games** (Delta Force, LoL, CS2, PUBG, Valorant, Apex, Dota 2, Overwatch 2) with per-game launch config
- **Rollback-first**: every apply persists snapshots (`%LOCALAPPDATA%\GameOptimizer`), cross-process rollback + watchdog auto-rollback
- **All features are free** (MIT); native Win32 GUI (zh/en) + CLI `gopt_cli` (status / apply / optimize / rollback / tune / startup / prio / clean ...)
- CI builds standalone binaries on every tag; releases on GitHub Releases

Contributions welcome: add a game preset in `src/preset/GameOptimizationPreset.cpp` (one line per game), UI polish, more hardware coverage.

## 🎉 v1.0.19 更新日志

- **修复（重要）：页内控件点击此前完全无效**——页容器是 `STATIC`，它不转发子控件的 `WM_COMMAND`，导致「一键优化 / 应用优化 / 系统调优三键 / 清理临时文件 / 进程提升 / 启动项」等页内按钮的点击通知根本到不了主窗口（这正是"点了没反应"的根因）。现已通过页容器子类化（保存原过程 + `CallWindowProcW` 链式调用）把 `WM_COMMAND` 转发给主窗口，所有页内按钮与列表交互恢复可用
- **`gopt_cli watch --top [N]`**：进程热点榜——每秒按 CPU% 降序列出运行中支持游戏的 pid / CPU% / 内存 MB / 优先级（首次采样显示 `--`；`--top` 可与秒数共存）
- **进程页排序 + 双击提升**：列表按 CPU% 降序（选中项按 pid 保持，不再选错行）；双击某行等价于「提升优先级」
- **文档**：新增 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)（模块地图 / 数据流 / 红线落点表 / 扩展清单）；[docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) 增加「发布检查清单」10 步
- **CI 加严**：构建后新增产物校验步骤（`gopt_cli`/`gopt_gui`/安装包/便携包均存在且非空，且 `gopt_cli --version` 与 `src/version.h` 一致）

## 🎉 v1.0.18 更新日志

- **诊断报告**：`gopt_cli report [--out <文件路径>]`——一条命令导出全部状态（版本 / 硬件 / 提权 / 当前电源方案 / 运行中游戏 / 上次优化时间 / 8 款预设概览 / 安全边界），支持 UTF-8 无 BOM 导出，字段按显示宽度对齐（中英文均可直接复制粘贴）
- **GUI「诊断 / 关于」**：总览页新增按钮，弹窗展示版本、CPU/GPU/内存、提权状态、当前电源方案、上次优化时间与安全声明（中英双语）
- **托盘「清理临时文件」**：托盘右键菜单可直接执行临时清理，结果写入界面日志
- **README 功能矩阵 + FAQ**：GUI/CLI 能力对照表（12 项）与 6 条常见问题（反作弊安全 / 管理员权限 / 回滚 / 电源方案 unknown / 清理会不会误删 / 如何新增游戏）
- **CI 版本一致性校验**：构建前运行 `tools/check_version.ps1`，`version.h` 与双 RC / README 更新日志不一致时直接中断发布

## 🎉 v1.0.17 更新日志

- **CPU/内存实时曲线**：总览页新增 48 秒实时迷你图（深色面板，CPU 青色 / 内存绿色双曲线，纯 GDI 绘制、每秒采样）——优化前后趋势一目了然

## 🎉 v1.0.16 更新日志

- **进程内存占用**：进程页列表显示每个游戏进程的当前内存占用（`GetProcessMemoryInfo`，psapi 稳定 API），与优先级/CPU% 同屏
- **README CLI 命令表刷新**：补齐 `watch / optimize / tune / startup / prio / --dry-run / clean / license` 等全部命令与说明

## 🎉 v1.0.15 更新日志

- **贡献指南**：新增 [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md)——红线、构建、添加游戏预设三步、自测、发布流程与提交规范
- **版本一致性检查**：`tools/check_version.ps1` 校验 version.h / 双 RC / README 更新日志版本一致（发布前必跑）
- **启动配置摘要**：游戏优化页提示区显示已保存的代启动配置（exe 路径/参数/电源开关；未设置时明确提示）

## 🎉 v1.0.14 更新日志

- **RAM 实时化**：总览页内存卡片每秒刷新「当前占用 X%」（GlobalMemoryStatusEx 稳定 API，与 CPU 负载同步）
- **当前电源方案可见**：系统调优页显示当前电源方案（只读查询）
- **托盘一键优化**：托盘右键菜单新增「一键优化」——恢复窗口并立即执行（复用完整流程与通知）

## 🎉 v1.0.13 更新日志

- **实时监视器**：`gopt_cli watch [秒数]`——命令行实时刷新 CPU 占用 / 内存占用 / 运行中的支持游戏（GetSystemTimes + GlobalMemoryStatusEx 稳定 API；Ctrl+C 或按秒数自动退出），观察优化前后效果最直观

## 🎉 v1.0.12 更新日志

- **英文概览（README）**：新增 English summary——安全边界、官方 API 清单、快照/看门狗、八大游戏、全免费与贡献指引（开源协作友好）
- **授权状态文案修正**：`license status` 未激活时明确提示"所有功能免费，无需授权"（消除"无效/未激活"误导）
- **`optimize --dry-run`**：一键优化也有只读预览——显示将优化的运行中游戏及其预设
- **自检程序增强**：`gopt_verify.exe` 默认自检追加环境报告（提权状态/电源方案/快照文件存在性）

## 🎉 v1.0.11 更新日志

- **优化预览（只读）**：`gopt_cli apply <game> --dry-run` 显示将应用的每项优化（预设/优先级/亲和性掩码/工作集/帧延迟/电源/快照看门狗），不修改任何设置——动手前先看清会做什么
- **上次优化时间**：总览页显示最近一次优化的时间（基于 savepoints 快照文件时间戳；从未优化则显示提示）

## 🎉 v1.0.10 更新日志

- **单实例保护**（CreateMutexW）：重复启动激活已有窗口而非叠加多份（避免多个后台监控/看门狗相互干扰）
- **开机自启动**：总览页新增「开机自启动」勾选（HKCU Run 注册表，写入/移除当前路径；默认关闭）
- **档位推荐可见**：系统调优页与 `gopt_cli tune status` 显示当前电源方案 + 按硬件（物理核/内存）推荐的档位（只读）

## 🎉 v1.0.9 更新日志

- **系统托盘（稳定 API：Shell_NotifyIcon）**：关闭窗口最小化到托盘（后台看门狗继续），首次气泡提示；双击恢复、右键菜单「打开主界面 / 退出」；优化完成后台通知
- **进程页实时 CPU 占用**（稳定 API：GetProcessTimes + GetTickCount64）：每个游戏进程显示当前 CPU%，1 秒刷新且保留列表选中项

## 🎉 v1.0.8 更新日志

- **发布一致性**：安装包卸载注册表版本与安装完成提示统一为 1.0.8；NSIS 脚本同步
- **使用说明全面更新**（`docs/BEFORE_USE_README.txt`，随安装包/便携包分发）：移除过时 Pro 门控说明，补齐 系统调优/启动项/进程优先级/临时清理/科技感流程面板 等全部新功能与 CLI 命令

## 🎉 v1.0.7 更新日志

- **科技感优化流程面板**：一键/应用优化时显示深色霓虹覆盖面板——步骤节点链（✓ 完成 / 当前高亮 / 待执行）、每步实测耗时、大号百分比、扫描线与旋转指示动画；结束后日志保留完整明细（优先级值/掩码/核数等）

## 🎉 v1.0.6 更新日志

- **实时 CPU 负载**：总览页 CPU 卡片每秒刷新「当前负载 X%」（GetSystemTimes 官方 API）——优化前后占用变化一目了然
- **游戏预设即时预览**：「游戏优化」页选择游戏即显示该游戏的预设摘要（优先级/CPU 核数/帧延迟等）

## 🎉 v1.0.5 更新日志

- **进程优先级管理（Process Lasso 风格）**：GUI「进程」页新增「提升优先级 / 恢复正常」按钮，列表实时显示每个游戏进程当前优先级；CLI 新增 `gopt_cli prio <pid> high|above|normal|below|idle`
- **帮助文案修正**：CLI 使用说明移除过时的 Pro 门控表述，统一为"所有功能免费"

## 🎉 v1.0.4 更新日志

- **清理更安全**：临时清理保留 **24 小时内**修改的文件（防误删写入中的临时文件），报告"已清理/跳过/保留"三类统计
- **提权状态一目了然**：GUI 底部状态栏与 `gopt_cli status` 显示当前是否已提权，未提权时明确提示"部分功能需管理员"

## 🎉 v1.0.3 更新日志

- **垃圾清理**：`gopt_cli clean` 与 GUI「系统调优」页「清理临时文件」按钮——安全清理 `%TEMP%`（占用/锁定项自动跳过），Wise 365 风格
- **运行中游戏概览**：`gopt_cli list` 显示已运行的受支持游戏的优先级/亲和性
- **所有功能免费**：电源/帧延迟/工作集等全部优化项对所有人开放，无任何授权限制
- **一键性能优化**：**无需先启动游戏**——自动检测运行中的游戏并优化；无游戏时做系统级性能优化
- **每款游戏「优化启动」配置**：保存 exe 路径/启动参数/电源等开关，持久化到 `games.conf`
- **现代化主题 GUI**：微软雅黑 UI 字体、顶部渐变标题栏、系统主题控件、一键大按钮
- **中英双语界面**：GUI 下拉切换 / CLI `--lang en`
- 8 款游戏：三角洲行动、英雄联盟、CS2、绝地求生、无畏契约、Apex Legends、Dota 2、守望先锋2

## 特性

- **硬件无感**：自动识别 AMD/Intel CPU、NVIDIA/AMD/Intel GPU、内存（DXGI / GetLogicalProcessorInformationEx / SMBIOS / 注册表）
- **支持 8 款游戏**：三角洲行动、英雄联盟、CS2、绝地求生、无畏契约、Apex Legends、Dota 2、守望先锋2
- **安全**：只用官方 WinAPI（`SetPriorityClass` / `SetProcessAffinityMask` / `SetProcessWorkingSetSize` / `PowerSetActiveScheme`），**无注入、无内核 Hook**（反作弊游戏放心用，与 Process Lasso 同类操作）
- **一键回滚**：快照持久化到磁盘，`apply` 与 `rollback` 跨进程可用，多级撤销
- **看门狗**：应用后监控系统调度，异常自动回滚
- **所有功能免费**：电源方案/驱动帧延迟/工作集等全部优化项对所有人开放，无授权门控（仅受系统能力限制：管理员/厂商库）
- **双界面**：原生 Win32 **GUI**（一键优化/游戏选择/代启动路径/每游戏设置）+ **CLI**

## 功能矩阵

GUI 五大页（总览 / 游戏优化 / 系统调优 / 进程 / 启动项）与 CLI 命令的对应关系（√ = 有入口，— = 无）：

| 功能 | GUI | CLI | 说明 |
| --- | :---: | :---: | --- |
| 一键优化 | √ | √ | GUI 总览页「一键性能优化」；CLI `gopt_cli optimize [游戏 / system]`（未检测到游戏时做系统级优化） |
| 游戏预设 | √ | √ | GUI「游戏优化」页选游戏后点「应用优化」；CLI `gopt_cli apply <game>`（优先级 / 亲和性 / 工作集 / 驱动帧延迟） |
| 系统调优 | √ | √ | GUI「系统调优」页 高性能档 / 平衡档 / 恢复调优；CLI `tune high / balanced / restore / status`（需管理员） |
| 临时清理 | √ | √ | GUI「清理临时文件」；CLI `gopt_cli clean`（`%TEMP%`，24 小时内修改的文件保留、占用/锁定项跳过） |
| 进程优先级 | √ | √ | GUI「进程」页 提升优先级 / 恢复正常；CLI `prio <pid> high / above / normal / below / idle`（上限 HIGH） |
| 进程 CPU·内存 | √ | — | GUI「进程」页每个进程显示 优先级 + CPU% + 内存 MB（1 秒刷新）；CLI `list` 只有优先级/亲和性、`watch` 只有整机 CPU/内存 |
| 启动项管理 | √ | √ | GUI「启动项」页 禁用 / 启用 / 恢复全部；CLI `startup list / disable / enable / restore`（禁用=改名保留，可还原） |
| 实时监视 | √ | √ | GUI 总览页 CPU/内存卡每秒刷新 + 48 秒实时曲线；CLI `gopt_cli watch [秒数]` |
| 干跑预览 dry-run | — | √ | CLI `apply <game> --dry-run` / `optimize --dry-run` 只读预览将执行的每一项；GUI 无 dry-run 开关（实际执行时由流程面板逐步显示） |
| 系统托盘 | √ | — | GUI：关闭窗口最小化到托盘、双击恢复、右键菜单「打开主界面 / 一键优化 / 退出」（含单实例保护） |
| 开机自启 | √ | — | GUI 总览页「开机自启动」勾选（HKCU Run 写入当前路径，默认关闭） |
| 回滚 | √ | √ | GUI「回滚」按钮；CLI `rollback` / `rollback-all`（快照持久化到 `%LOCALAPPDATA%\GameOptimizer`，跨进程有效） |

## 界面

```bat
:: GUI（推荐）
gopt_gui.exe

:: CLI
gopt_cli status                        硬件/预设/提权/授权状态
gopt_cli watch [秒数]                  实时监视 CPU/内存/运行中游戏（Ctrl+C 退出）
gopt_cli apply cs2                     优化 cs2（游戏运行中 attach；--dry-run 只读预览）
gopt_cli apply cs2 --game-exe "<路径>"  代启动游戏
gopt_cli optimize [游戏|system]        一键：优化全部运行中的支持游戏（--dry-run 预览）
gopt_cli rollback / rollback-all       回滚（跨进程，秒级）
gopt_cli tune [high|balanced]          系统调优；tune status 只读查看；tune restore 恢复
gopt_cli startup list|disable|enable|restore   开机启动项管理
gopt_cli prio <pid> high|above|normal|below|idle  进程优先级（上限 HIGH）
gopt_cli list                          运行中的游戏概览（优先级/亲和性）
gopt_cli clean                         清理临时文件（%TEMP%，24h 内保留）
gopt_cli fingerprint / license status  机器指纹 / 授权（所有功能免费，授权可选）
gopt_cli --version                     版本
```

## FAQ 常见问题

**① 带反作弊的游戏（ACE / VAC / Riot 等）能安全用吗？**
可以。本工具只用官方 Win32 API（`SetPriorityClass` / `SetProcessAffinityMask` / `SetProcessWorkingSetSize` / `PowerSetActiveScheme`），**不注入进程、不加载驱动、不做内核 Hook**，操作类型与 Process Lasso 同类；优先级上限为 `HIGH_PRIORITY_CLASS`，从不使用 REALTIME。它不读写游戏内存、不改动游戏文件。

**② 需要管理员权限吗？**
分功能：进程优先级 / 亲和性 / 工作集 / 一键优化**不需要**管理员（但若游戏本身以管理员运行，本工具也需以管理员运行才能打开该进程）；**系统调优（电源方案、处理器频率、调度优先级）需要**管理员。GUI 底部状态栏与 `gopt_cli status` 都会显示当前是否已提权，未提权时明确提示「部分功能需管理员」。

**③ 怎么回滚？**
每次优化前都会把原值快照持久化到磁盘（`%LOCALAPPDATA%\GameOptimizer`），所以回滚不依赖发起优化的那个进程：GUI 点「回滚」，或在任意终端执行 `gopt_cli rollback`（撤销最近一次）/ `gopt_cli rollback-all`（撤销全部）。跨进程、秒级生效；应用后看门狗发现系统响应异常也会自动回滚。

**④ 为什么界面显示「电源方案 未知 / unknown / `<unknown>`」？**
这是只读查询失败时的正常降级，不影响优化与回滚：当前会话权限不足或系统未返回活动电源方案时，GUI「系统调优」页显示「未知 / unknown」，底部状态栏对应位置显示 `Power ?`；查询成功但方案友好名读不出时（`HAL::PowerSchemeName`）显示 `<unknown>`。想看到具体名称可尝试以管理员运行。

**⑤ 清理临时文件会删掉我正在用的文件吗？**
不会。`%TEMP%` 清理按「时间 + 可删除性」双重保护：**24 小时内修改过的文件一律保留**，被占用 / 锁定的项自动跳过；结果报告分别给出「已清理 / 跳过 / 保留」三类数量。

**⑥ 支持哪些游戏？怎么添加新游戏？**
内置 8 款：三角洲行动、英雄联盟、CS2、绝地求生、无畏契约、Apex Legends、Dota 2、守望先锋2。新增预设只需在 `src/preset/GameOptimizationPreset.cpp` 加一条（详见 [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md)），无需改动界面代码。

## 构建

需要 CMake ≥ 3.16（MSVC/Mingw）或便携工具链 w64devkit。

```bat
:: MSVC / CMake
cmake -S . -B build -G "Visual Studio 17 2022"
cmake --build build --config Release
build\Release\gopt_cli.exe

:: 便携 GCC（w64devkit）
powershell -ExecutionPolicy Bypass -File tools\build_w64devkit.ps1
build\gopt_cli.exe     CLI（脚本同时产出 build\gopt_gui.exe 原生 GUI）
```

GUI 构建：CMake 已含 `gopt_gui` 目标（含独立版本资源 `resources\gui_resource.rc`）；便携脚本 `tools\build_w64devkit.ps1` 同样产出 GUI。

## 目录结构

```
src/
  hardware/   硬件探测           hal/   统一硬件操作(HAL)
  preset/     8 款游戏预设         rollback/  快照/回滚/看门狗
  license/    机器指纹 + 授权      core/  AppCore 协调层
  gui/        原生 GUI 界面
tools/        构建脚本 / 验证程序 / CLI
resources/    图标 / 安装脚本 / 版本资源
docs/         商业化方案（含合规风险）
```

## 安全边界（设计约束）

- 无注入、无内核 Hook；只用官方用户态 API
- 优先级上限 `HIGH_PRIORITY_CLASS`（REALTIME 禁用）
- 电源切换默认关闭（需 `--power` + 管理员权限；功能本身免费）
- 三角洲行动(ACE)、CS2(VAC)、英雄联盟(Riot) 均带反作弊——本工具不用注入/Hook，可放心使用

## 授权（全部免费）

所有功能免费：无需授权即可使用全部优化项（电源/帧延迟/工作集等）。
仅保留可选的机器指纹命令（`fingerprint`，用于潜在的企业/捐赠场景），不影响功能。
（旧版 free/Pro 门控已移除；如未来需要付费档，可重启该门控。）

## 许可

[MIT](LICENSE)。注意：游戏名称/商标归各自厂商；本工具不包含任何游戏素材。

## 开源协作

欢迎 PR：新游戏预设（`src\preset\GameOptimizationPreset.cpp` 加一行）、UI 增强、更多硬件适配。
完整贡献指南（构建/加游戏/自测/发布流程）见 [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md)。
商业变现路线见 [docs/COMMERCIALIZATION.md](docs/COMMERCIALIZATION.md)。
