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
