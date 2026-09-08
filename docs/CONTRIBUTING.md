# GameOptimizer 贡献指南 (Contributing)

感谢参与！本项目是纯 C++17 + Win32 原生实现的 Windows 游戏优化工具，
红线（设计约束，PR 必须遵守）：

- 只用官方 Win32 API（user32 / kernel32 / shell32 / advapi32 等）
- 无注入、无内核 Hook（反作弊游戏安全）
- 优先级上限 `HIGH_PRIORITY_CLASS`，REALTIME 一律拒绝
- 所有改动必须可回滚/可解释（快照 + 看门狗）

## 构建（只需 w64devkit）

```bat
powershell -ExecutionPolicy Bypass -File tools\build_w64devkit.ps1
:: 产出 build\gopt_cli.exe（CLI）与 build\gopt_gui.exe（GUI）
:: 完整发布构建含自检/安装包/便携包：tools\build_release.sh
```

CMake（MSVC / MinGW）也能构建：`cmake -S . -B build -G "Visual Studio 17 2022"`。

## 添加一款新游戏预设（最常见贡献）

1. 在 `src/preset/GamePreset.h` 的 `GameId` 枚举追加新游戏 id；
2. 在 `src/preset/GameOptimizationPreset.cpp` 三处各加一行：
   - `GameIdToString`：显示名（中文）
   - `GameExeName`：目标进程可执行文件名（如 `cs2.exe`；区分启动器/主进程）
   - `GetPreset`：预设 `GamePreset` 字段：
     - `processPriorityClass`：`ABOVE_NORMAL_PRIORITY_CLASS` 或 `HIGH_PRIORITY_CLASS`（最高）
     - `leaveCoresForSystem`：保留给系统的物理/逻辑核数（推荐 >=1）
     - `bindPhysicalOnly`：是否仅绑物理核（HT 场景建议 true）
     - `gpuMaxFrames`：驱动级帧延迟 1~3（0=不启用；无 NVIDIA/AMD 库会自动降级跳过）
     - `workingSetMinMB/MaxMB`：工作集（0=不设置）
     - `switchHighPerformancePower`：建议是否切高性能电源（实际受 AppCore 配置控制）
     - `description`：一句话预设说明（必填，UI/日志展示）
3. 硬件降级逻辑集中在 `ApplyHardwareDegradation`（核少/内存小自动降低档位）；
4. CLI 支持的游戏列表行 `"游戏: deltaforce | lol | cs2 | ..."` 补上；
   GUI 游戏下拉在 `gopt_gui.cpp` 的 `kGames[]` 数组补上。

## 自测（提交前必做）

```bat
build\gopt_verify.exe                  :: 机制自检（优先级/亲和性 应用+恢复，PASS）
build\gopt_cli.exe apply <game> --dry-run :: 预览（只读，不修改）
build\gopt_cli.exe status
build\gopt_cli.exe clean               :: 临时清理（24h 内文件保留）
```

## 版本与发布

- 发布版本须同步三处：`src/version.h`、`resources/resource.rc`、`resources/gui_resource.rc`；
  运行 `powershell -File tools\check_version.ps1` 校验一致性。
- README 顶部更新日志追加条目（`## 🎉 vX.Y.Z 更新日志`）。
- 打标签 `vX.Y.Z` 推送后 GitHub Actions 自动构建并发布 Release（无需手动上传）。

## 提交规范

- 每个提交聚焦一个改动；描述说明"为什么安全/可回滚"。
- 保留全部免费原则：不要在核心功能上加授权门控。
