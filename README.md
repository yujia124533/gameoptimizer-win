# GameOptimizer

> 硬件无感的 Windows 游戏优化工具 · 仅用官方 WinAPI，无注入、无内核 Hook，一键安全回滚

[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![C++](https://img.shields.io/badge/C%2B%2B-17-00599c.svg)](#)
[![Windows](https://img.shields.io/badge/Windows-x64-0078d4.svg)](#)

自动识别 CPU/GPU/内存 → 按游戏预设应用进程优先级/CPU 亲和性/工作集/电源策略 → 每次修改自动快照，秒级回滚。

## 特性

- **硬件无感**：自动识别 AMD/Intel CPU、NVIDIA/AMD/Intel GPU、内存（DXGI / GetLogicalProcessorInformationEx / SMBIOS / 注册表）
- **支持 8 款游戏**：三角洲行动、英雄联盟、CS2、绝地求生、无畏契约、Apex Legends、Dota 2、守望先锋2
- **安全**：只用官方 WinAPI（`SetPriorityClass` / `SetProcessAffinityMask` / `SetProcessWorkingSetSize` / `PowerSetActiveScheme`），**无注入、无内核 Hook**（反作弊游戏放心用，与 Process Lasso 同类操作）
- **一键回滚**：快照持久化到磁盘，`apply` 与 `rollback` 跨进程可用，多级撤销
- **看门狗**：应用后监控系统调度，异常自动回滚
- **免费版 + Pro**：免费版 = 优先级 + CPU 亲和性；Pro 增加 电源方案/驱动帧延迟/工作集（机器指纹绑定的签名授权码）
- **双界面**：原生 Win32 **GUI**（选游戏/代启动路径/Pro 开关）+ **CLI**

## 界面

```bat
:: GUI（推荐）
gopt_gui.exe

:: CLI
gopt_cli status                        查看硬件/预设/授权
gopt_cli apply cs2                     优化 cs2（游戏运行中 attach）
gopt_cli apply cs2 --game-exe "<路径>"  代启动游戏
gopt_cli rollback / rollback-all       回滚
gopt_cli fingerprint                   本机机器指纹（授权绑定）
gopt_cli license status / activate <码> 授权
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
build\gopt_cli.exe
```

GUI 构建：把 `src\gui\gopt_gui.cpp` 与核心源码一并编译（`-mwindows`），或用脚本/CMake 增加该源文件。

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
- 电源切换默认关闭（需 `--power` / Pro + 管理员）
- 三角洲行动(ACE)、CS2(VAC)、英雄联盟(Riot) 均带反作弊——本工具不用注入/Hook，可放心使用

## 授权

免费版仅优先级+亲和性；Pro 增加电源/帧延迟/工作集。授权码由机器指纹绑定 + HMAC-SHA256 签名。
开发者为指定机器生成：`gopt_cli license gen <本机指纹> Pro [到期]`。

## 许可

[MIT](LICENSE)。注意：游戏名称/商标归各自厂商；本工具不包含任何游戏素材。

## 开源协作

欢迎 PR：新游戏预设（`src\preset\GameOptimizationPreset.cpp` 加一行）、UI 增强、更多硬件适配。
商业变现路线见 [docs/COMMERCIALIZATION.md](docs/COMMERCIALIZATION.md)。
