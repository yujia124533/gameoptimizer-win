# gopt-cli —— 命令行前端（手写参数解析 + 全量 `--json`）

单内核多前端里的 **CLI 前端**：只依赖 `gopt-core`（不依赖 `gopt-hal` / `gopt-journal`、不依赖 `windows`），
所以"前端绕过内核直接改系统"在依赖图上就不可能。

```
gopt [全局选项] <命令> [参数] [选项]
```

* 全局选项：`--json`、`--lang zh|en`、`--data-dir <目录>`、`-h/--help`、`-V/--version`、`-y/--yes`、`-j`；
  它们可以出现在命令行的任何位置（`gopt --json status` 与 `gopt status --json` 等价）。
* 需要取值的选项支持 `--pid 1234` 与 `--pid=1234` 两种写法；**未知选项一律是用法错误**（不会被静默忽略）。
* 刻意不引入 `clap`：解析层只保证形状正确，语义（游戏是否存在、进程是否在跑）由内核判断并给出结构化错误。

---

## 1. 命令清单

| 命令 | 作用 | 默认安全 |
| --- | --- | --- |
| `status` | 机器 / 策略 / 审计链 / 正在运行的游戏总览 | 只读 |
| `list [games\|processes\|startup]` | 列出策略、进程或开机启动项 | 只读 |
| `plan <游戏\|exe\|pid> [--pid N]` | 只读预览执行计划（为什么这么做、调用哪个 HAL 操作） | 只读 |
| `apply <游戏\|exe\|pid> [--pid N] [--yes]` | 执行计划 | **无 `--yes` 只预演** |
| `rollback [--to N] [--all] [--pending] [--yes]` | 从审计日志逆序撤销 | **无 `--yes` 只预演** |
| `journal [--kind apply\|rollback\|imported] [--limit N]` | 查看审计记录（含链状态、待撤销/已撤销） | 只读 |
| `explain (--game ID \| --rule ID \| --journal-id N)` | 解释某款游戏 / 某条规则 / 某条审计记录 | 只读 |
| `verify-journal [--anchor-len N --anchor-hash H] [--strict]` | 校验哈希链（可检出篡改） | 只读 |
| `watch [--interval S] [--duration S] [--once] [--yes]` | 监控新出现的游戏进程 | **无 `--yes` 只报告** |
| `prio [--pid N \| --exe NAME] [--set CLASS] [--yes]` | 读取 / 设置进程优先级（上限 `high`，永不 REALTIME） | **无 `--yes` 只预演** |
| `tune [--power-scheme high\|balanced] [--yes]` | 查询 / 切换电源方案 | **无 `--yes` 只预演** |
| `startup [list \| enable NAME \| disable NAME] [--hive hkcu\|hklm] [--yes]` | 列出 / 启用 / 禁用启动项（禁用 = 改名迁移） | **无 `--yes` 只预演** |
| `report [--out FILE]` | 生成体检报告（文本落盘 + `--json`） | 只读 |
| `import-legacy [--savepoints P] [--games-conf P] [--yes]` | 只读解析 C++ v1.1.0 旧格式；`--yes` 时以 `kind=imported` 入链 | **无 `--yes` 只解析** |
| `help [命令]` / `--version` | 用法 / 版本（含"仍是正式发布版"的 C++ v1.1.0） | — |

## 2. 退出码

| 码 | 含义 | 典型场景 |
| --- | --- | --- |
| `0` | 成功 | 含"预演成功"（没有 `--yes` 时只打印计划） |
| `1` | 用法错误 | 未知命令/选项、缺参数、`--lang de`、`--set realtime`（红线） |
| `2` | 环境不满足 | 游戏没在运行、进程已退出、未提权、系统不支持、文件不可写 |
| `3` | 审计链校验失败 | 日志被篡改/损坏；此时**所有写命令都拒绝执行**（fail-closed） |

## 3. 输出约定

* **文本**：成功与运行期失败都走 stdout；失败时打印"错误 + 分类 + 可执行建议"（中英双语），**不打印堆栈**；
* **用法错误**走 stderr（那时没有结构化结果可给），并附带该命令的用法；
* `--json` 一律走 stdout，形状是 `Outcome<T>`：

```json
{
  "schema_version": 1,
  "ok": true,
  "command": "plan",
  "lang": "zh",
  "data": { "...": "命令相关（见 gopt-core/README.md 的 DTO 表）" },
  "error": null,
  "notices": [ { "level": "warning", "zh": "…", "en": "…" } ]
}
```

* 失败时 `ok = false` 且 `error.kind ∈ {usage, invalid_argument, not_found, access_denied, policy_denied,
  unsupported, io, hal, journal, audit_chain_broken, internal}`；`data` 仍可能带**部分结果**
  （例如"执行到一半失败"的已应用步骤），脚本可以据此判断"到底改了什么"。
* `watch --json` 是 JSONL：每轮一行 `Outcome<WatchTick>`（流式命令不做一次性大 JSON）。

## 4. 环境变量

| 变量 | 作用 |
| --- | --- |
| `GOPT_DATA_DIR` | 数据目录（等价于 `--data-dir`，优先级低于命令行）：`journal.jsonl` / `policies.d/` / `savepoints.txt` / `games.conf` 全在这里 |
| `GOPT_LANG` | 默认语言（`zh` / `en`，`--lang` 优先） |
| `GOPT_BACKEND=mock` | 用 Mock HALL 后端跑（**仅供测试/CI**：不碰真实系统，缺陷复现用）；不设置时永远是真实 Win32 后端 |

```powershell
# 只读诊断：把数据目录指到临时目录，不污染真实配置
$env:GOPT_DATA_DIR = "$env:TEMP\gopt-demo"
gopt status --json
gopt plan cs2 --json
gopt verify-journal            # 退出码 0 = 链完好；3 = 被篡改/损坏
```

## 5. 默认安全的三条证据（`tests/cli_contract.rs`）

* `apply_without_yes_only_previews`：`apply cs2`（无 `--yes`）⇒ `dry_run=true`、`applied=0`、
  **日志文件不存在**、`notices` 里有 dry-run 说明；
* `exit_code_3_when_the_audit_chain_does_not_verify`：篡改一行后 `verify-journal` ⇒ 3，
  `apply --yes` 与 `rollback --yes` 同样 ⇒ 3（拒绝在被污染的链上改系统）；
* `exit_code_1_for_usage_errors` / `exit_code_2_when_the_environment_is_not_satisfied`：
  退出码语义在进程边界上被钉死。

## 6. 文件清单

| 文件 | 内容 |
| --- | --- |
| `src/main.rs` | 入口：解析 → 执行 → 打印（文本/JSON）→ `std::process::exit(code)`；用法错误走 stderr |
| `src/args.rs` | 手写参数解析（`Command` / `Invocation` / 已知选项白名单）+ 中英双语用法文本 |
| `src/run.rs` | 命令实现：造 `Session`、调内核、选渲染、组装 `Outcome`（不出现任何 HAL/journal 调用） |
| `tests/cli_contract.rs` | 二进制级契约测试：`--json` 可解析 + 退出码 0/1/2/3 + 默认安全（11 个用例） |
