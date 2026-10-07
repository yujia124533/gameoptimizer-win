# GameOptimizer 贡献指南 (Contributing)

感谢参与！本项目是纯 C++17 + Win32 原生实现的 Windows 游戏优化工具，
红线（设计约束，PR 必须遵守）：

- 只用官方 Win32 API（user32 / kernel32 / shell32 / advapi32 等）
- 无注入、无内核 Hook（反作弊游戏安全）
- 优先级上限 `HIGH_PRIORITY_CLASS`，REALTIME 一律拒绝
- 所有改动必须可回滚/可解释（快照 + 看门狗）
- UI 与系统调用用薄门面隔离：页面模块不直接修改进程属性、切换电源、删除文件或改动启动项，一律经 `Hooks` 交宿主（页面组 B），或在工作线程自建 `AppCore`（页面组 A）；页面里现存的少量直连（诊断只读查询、`HKCU\...\Run` 开机自启、用户级 `games.conf`）已在 `docs/ARCHITECTURE.md` 第 2.3 节的「可审计例外清单」登记，新增直连必须同时登记

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
   GUI 游戏下拉在页面组 A 的 `src/gui/page_game.cpp` 内 `kGames[8]` 数组补上（外壳 `gopt_gui.cpp` 的同名数组只服务旧路径，收口后无需维护）。

## UI 改动检查清单 (UI Checklist)

界面改动（新页面、新控件、改布局、改文案、改颜色）逐条自检，全部满足才算完成：

1. **令牌取用**：颜色只用 `UiColor(UiColorRole::...)`，间距只用 `UiSp(UiSpace::...)`，字号只用 `UiFont(UiFontRole::...)` / `UiFontPx`，圆角只用 `UiRadiusPx(UiRadius::...)`，控件高度只用 `UiControlHeight(UiControlH::...)`。**不得出现硬编码 RGB 或魔法像素值**（只有两处例外：`ui_theme.cpp` 里的令牌表本身，以及 `ui_widgets.cpp` 中 `UiWidgetsSelfTest` 用 `RGB(1,1,1)` 作「是否真的出图」的哨兵色）。核对命令：`Select-String -Path src\gui\page_*.cpp,src\gui\ui_widgets.cpp -Pattern 'RGB\(' -Encoding UTF8` —— 命中只应出现在 `UiWidgetsSelfTest` 里。
2. **8px 栅格**：间距取值来自 2 / 4 / 8 / 16 / 24 / 32 / 48 这套栅格；新布局必须由纯整数函数算出（输入 `w, h` + `PageMetrics`），并保证**同层矩形两两不重叠、全部落在容器内、宽高非负**（页面组 A 用 `DashLayoutSelfCheck` / `GameLayoutSelfCheck`，页面组 B 用 `SelfCheck` 复算 `RectsOverlap`）；空间不足时把次要控件置空矩形并隐藏，绝不用重叠换空间。
3. **双语**：所有面向用户的文案都过 `T(zh, en)`（或页面的 `Tr(zh, en)`），切换语言后由 `ApplyLabels` / `ApplyLanguage` 能刷新到；新增文案时同步检查中英两列，不要只写中文。
4. **每个入口都有反馈**：按钮/列表操作/双击都要有可见回执——页内状态行（`PageDrawStatusLine` / `SetBanner`）、`receipt` 回执、进度条或日志，三者至少一处；失败必须给出可读原因（不允许静默 return）。长任务放工作线程，UI 线程只置 busy + 禁用按钮，结束后 `PostMessage` 回投。
5. **高 DPI 与主题**：不写死像素（用 `UiScale*` / `PageMetrics::px` / DPI 令牌换算）；`WM_DPICHANGED` 里调用 `UiOnDpiChanged()`，按 lParam 给出的 RECT 重排并 `InvalidateRect(TRUE)`；主题变化（`WM_THEMECHANGED` / `UiThemeChangedMessage()`）后重建画刷并重绘；125% / 150% 缩放下确认无裁切、无重叠、无错位。
6. **构建入口**：新增 UI 源文件要登记到三处，且**只进 GUI 目标**（CLI 与自检不链接 UI 源，避免引入 comctl32 等依赖）：`CMakeLists.txt` 的 `gopt_gui` 源列表、`tools/build_w64devkit.ps1` 的 `$UISRCS`、`tools/build_release.sh` 的 `UISRCS`。
7. **真实点击验证**：至少真机点一遍改动涉及的入口（按钮点击、列表选中与双击、复选框、下拉、输入框校验失败路径），并在 150% 缩放下再点一遍；只看代码或只跑语法检查不算验证。
8. **通知路径**：页面控件通知必须能被页内页面（就地分发）与页容器（`PageProc` 转发）两条路径正确处理，同一条 `WM_COMMAND` 重复到达时用确定性的 `(id, code)` 守卫去重（不依赖 `GetMessageTime` 等计时判据）；不要要求宿主为你新增的 ID 写分支（见 `docs/ARCHITECTURE.md` 第 2.2 节）。

UI 改动的语法门禁（不启动窗口，输出为空即通过；工具链路径见 `tools\build_w64devkit.ps1`）：

```bat
g++ -std=c++17 -Wall -Wextra -fsyntax-only -Isrc -mwindows src\gui\ui_theme.cpp src\gui\ui_widgets.cpp
g++ -std=c++17 -Wall -Wextra -fsyntax-only -Isrc -mwindows src\gui\page_dashboard.cpp src\gui\page_game.cpp
g++ -std=c++17 -Wall -Wextra -fsyntax-only -Isrc -mwindows src\gui\page_tune.cpp src\gui\page_process.cpp src\gui\page_startup.cpp
```

运行期自检：`UiWidgetsSelfTest(HDC)`、`PageTune::SelfCheck` / `PageProcess::SelfCheck` / `PageStartup::SelfCheck`、`DashLayoutSelfCheck` / `GameLayoutSelfCheck`。

## 自测（提交前必做）

```bat
build\gopt_verify.exe                  :: 机制自检（优先级/亲和性 应用+恢复，PASS）
build\gopt_cli.exe apply <game> --dry-run :: 预览（只读，不修改）
build\gopt_cli.exe status
build\gopt_cli.exe savepoints          :: 只读查看快照历史（不建目录、不写文件）
build\gopt_cli.exe clean               :: 临时清理（24h 内文件保留）
```

## 版本与发布

- 发布版本须同步三处：`src/version.h`、`resources/resource.rc`、`resources/gui_resource.rc`；
  运行 `powershell -File tools\check_version.ps1` 校验一致性。
- README 顶部更新日志追加条目（`## 🎉 vX.Y.Z 更新日志`）。
- 打标签 `vX.Y.Z` 推送后 GitHub Actions 自动构建并发布 Release（无需手动上传）。

## 发布检查清单 (Release Checklist)

按顺序执行，任一步失败即停止并修复（步骤 2–5 与 CI 的前几步一致）：

1. **同步版本号三处 + 更新日志**：`src/version.h` 的 `GOPT_VERSION_STR`、`resources/resource.rc`、`resources/gui_resource.rc`（`FILEVERSION` 与 `FileVersion`/`ProductVersion` 字符串）；README 顶部追加 `## 🎉 vX.Y.Z 更新日志`。

2. **版本一致性门禁**：

   ```bat
   powershell -NoProfile -ExecutionPolicy Bypass -File tools\check_version.ps1
   :: 期望输出 RESULT: PASS (all version markers = vX.Y.Z)，退出码 0
   ```

3. **构建全部产物**（MSYS2 MinGW64 / w64devkit 环境，产出 CLI + GUI + 自检 + 安装包 + 便携包）：

   ```bat
   bash tools/build_release.sh
   :: 期望：build\gopt_cli.exe  build\gopt_gui.exe  build\gopt_verify.exe  build\GameOptimizer-setup.exe
   ::       release\GameOptimizer-setup.exe  release\GameOptimizer-portable.zip
   ```

4. **机制自检（真实进程 优先级/亲和性 应用 + 恢复）**：

   ```bat
   build\gopt_verify.exe
   :: 期望 RESULT: PASS
   :: 可选：跨进程两段验证（快照落盘后由新进程回滚）
   build\gopt_verify.exe save
   build\gopt_verify.exe rollback <上一步输出的 pid>
   ```

5. **CLI 冒烟**：

   ```bat
   build\gopt_cli.exe --version   :: 期望 GameOptimizer vX.Y.Z
   build\gopt_cli.exe report      :: 诊断报告（可加 --out release\report.txt）
   build\gopt_cli.exe status      :: 硬件 / 预设 / 提权状态
   ```

6. **GUI 冒烟（真机点击，不接受"编译通过就算过"）**：

   ```bat
   build\gopt_gui.exe
   :: 逐页点一遍入口：总览（一键性能优化 / 开机自启动 / 一键回滚 / 诊断·关于 / 优化历史刷新与双击明细）
   ::                   游戏优化（选游戏 / 浏览路径 / 保存 / 应用 / 回滚）
   ::                   系统调优（高性能 / 平衡 / 恢复 / 清理临时文件二次确认）
   ::                   进程（刷新 / 提升 / 恢复正常 / 列表双击提升）
   ::                   启动项（刷新 / 禁用 / 启用 / 恢复全部）
   :: 再切一次 中文/English（文案全部刷新），并在 150% 缩放下重复关键入口（无裁切/重叠）
   ```

   期望：每个入口都有可见反馈（状态行 / 回执 / 进度 / 日志），无"点了没反应"。

7. **确认便携包内容**：`release\GameOptimizer-portable.zip` 内应含 `gopt_cli.exe`、`gopt_gui.exe`、`gopt_verify.exe`、`BEFORE_USE_README.txt`、`自检.cmd`（由 `tools\build_release.sh` 自动打包）。

8. **提交并推送**：

   ```bat
   git add -A
   git commit -m "release: vX.Y.Z ..."
   git push origin main
   ```

9. **打 tag 并推送**（tag 推送即触发 CI 构建与发布）：

   ```bat
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

10. **在 GitHub Release 页确认两个资产**：`GameOptimizer-setup.exe`、`GameOptimizer-portable.zip`。
    CI 步骤顺序：版本一致性校验 → 构建（MSYS2）→ **产物存在性 + 版本校验** → 上传 artifact → 创建 Release。

11. **失败处理**：若门禁或产物校验失败，修正后删除远端 tag 重打：

    ```bat
    git tag -d vX.Y.Z
    git push origin :refs/tags/vX.Y.Z
    ```

## 提交规范

- 每个提交聚焦一个改动；描述说明"为什么安全/可回滚"。
- 保留全部免费原则：不要在核心功能上加授权门控。
