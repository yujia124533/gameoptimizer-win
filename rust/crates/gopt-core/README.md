# gopt-core —— 编排内核（单内核多前端）

`detect → load policy → plan（默认只读）→ apply（写审计日志）→ rollback（逆序撤销）`。

前端（CLI、将来的 GUI/托盘）**只依赖本 crate**：它们不拼 HAL 调用、不写审计日志，
于是"某个前端绕过内核直接改系统"在依赖图上就不可能出现。

```
                       ┌──────────── gopt-policy（TOML 声明式策略）
detect ──► EvalInput ─┤
  │                   └──► Plan（有序、可解释：rule_id / 行号 / 中英双语理由 / 降级说明）
  │
  └──► apply ──► gopt-hal::SystemApi（唯一被允许触碰系统的入口）
           │
           └──► gopt-journal（每条修改先写"写入前状态"，SHA-256 哈希链绑定）
                    │
                    └──► rollback：从日志**重新推导**逆序撤销计划，再经 HAL 落地
```

---

## 1. 公开 API（前端只需要这些）

| 类型 / 方法 | 作用 |
| --- | --- |
| `Session::new(api, DataPaths, Lang)` | 采集硬件 + 提权状态（`EvalInput`），加载内置 + 用户策略 |
| `Session::status()` | 环境 / 策略 / 审计链 / 正在运行的游戏 的完整快照（`StatusReport`） |
| `Session::games()` | 全部可用策略摘要（`GameSummary`） |
| `Session::plan(query, pid)` | **只读**：按 id / 中英文名 / 别名 / exe / pid 找到策略并求值成 `PlanReport` |
| `Session::plans_for_running()` | 全部"正在运行且命中策略"的进程的计划 |
| `Session::apply(&Plan, ApplyOptions)` | 执行（`dry_run` 时只读）→ `ApplyReport`（逐步骤前后值 + 入链 id） |
| `Session::rollback(RollbackOptions)` | 从日志逆序撤销 → `RollbackReport`（含 `NotActionable` 说明） |
| `Session::journal_view(JournalFilter)` | 审计记录视图（链状态 + 待撤销/已撤销） |
| `Session::verify_journal(VerifyOptions)` | 链校验（可带外部锚点、`strict`）→ `VerifyReport` |
| `Session::explain_game/explain_rule/explain_journal` | 可解释性入口（规则条件/动作/HAL 操作/撤销方式） |
| `Session::priority / tune / startup_list / startup_set` | 单点操作（读/写，写时入链） |
| `Session::report()` | 体检报告数据（`ReportData`） |
| `Session::import_legacy(LegacyImportOptions)` | 只读解析 C++ v1.1.0 旧格式，`--yes` 时以 `kind=imported` 入链 |
| `Session::watch_tick(&mut WatchState, WatchOptions)` | 监控的一轮（CLI 自己 sleep，内核不引入线程/async） |
| `Outcome<T>` + `CoreError` | `--json` 的顶层形状与**退出码唯一来源**（0/1/2/3） |
| `report::render_*` | 文本渲染（CLI 与将来的 GUI 共用同一套措辞与信息层次） |

数据目录只有一个入口：`DataPaths::resolve(--data-dir > GOPT_DATA_DIR > %LOCALAPPDATA%\GameOptimizer)`。
把 `GOPT_DATA_DIR` 指向临时目录，整个内核（含审计日志、用户策略、旧格式导入）就完全在临时目录里跑。

---

## 2. 默认安全（三层）

1. **plan 只读**：`gopt plan` 一行日志都不写、一个写调用都不发（集成测试用 Mock 的调用计数断言）。
2. **apply 需要显式确认**：CLI 没有 `--yes` 时走 `ApplyOptions::preview()`——照常读当前值、算出前后差异，
   但**不写系统、不写日志**（连日志文件都不会被创建）。
3. **fail-closed**：真正执行前先打开审计日志并校验哈希链；链不可信 ⇒ `AuditChainBroken`（退出码 3），
   **系统零改动**。执行过程中日志写不进去 ⇒ 立刻停手并把已发生的改动如实报出（`journal_blocked`）。

## 3. 可解释 / 可回滚

* 每个 `AppliedStep` 都带 `before` / `after` 两个 JSON 载荷（与审计日志同一套 schema）；
* 已经是目标值 ⇒ 不写、不记，状态列 `unchanged`（幂等，重复执行不会污染审计链）；
* 单步失败 ⇒ 其余步骤照常执行，失败原因结构化（`kind` + 稳定英文消息 + 中英双语建议），
  **只有成功的步骤进链**；
* `rollback` 计划完全来自 `gopt-journal`（纯数据），内核只负责把它翻成 HAL 调用，
  并为每条执行过的撤销动作追加 `kind=rollback` + `rule_id="rollback:<apply_id>"` 的记录。

### 能力边界（诚实说明）

| 项 | 现状 | 原因 |
| --- | --- | --- |
| 工作集写入前状态 | **可回滚**：`before` 是官方读回来的真值，回滚按字节还原 | 需要 `gopt-hal` 的工作集读方法（`get_working_set` → 官方 `GetProcessWorkingSetSize`） |
| 工作集前值读不到 | 该步 `before = null` ⇒ 回滚报 `NotActionable`（附原因） | 进程受保护/已退出时读不到；或系统报告 `min = 0`，而 HAL 写路径不接受 `min = 0` |
| 跨处理器组亲和性 | 按 `PlanStep::affinity_batches()` 逐组应用（非主组逐线程） | Win32 没有"整体绑定多组"的语义 |
| 旧格式快照回滚 | 逐字段尝试（缺失字段不动），带"需人工确认"说明 | 旧 `savepoints.txt` 不记录当时实际应用了哪些项 |
| 尾部整行被删除 | 需要外部锚点（`--anchor-len/--anchor-hash`） | 只靠文件自身，剩余前缀仍然自洽 |

工作集那一项曾是**上游 HAL 契约的缺口**，现已补齐：`SystemApi` 增加第 14 个方法
`get_working_set`（官方 `GetProcessWorkingSetSize`），内核在写之前读一次当前上下限，
把它作为 `before` 写进审计记录，于是工作集步骤 `reversible = true`、回滚能逐字节还原。
只有"读不到前值"或"前值写不回去（min = 0）"两种情况才会退回不可回滚，并在 note 里说明原因——
内核宁可每次都说"这一步撤不了"，也不写一条假的可回滚记录。

---

## 4. 用 Mock 后端做完整单测（不需要管理员）

```powershell
$tc = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"; $env:PATH = "$tc;$env:PATH"
cd rust
cargo test -p gopt-core                       # 35 单测 + 9 集成 + 2 doctest
cargo clippy -p gopt-core --all-targets -- -D warnings
```

`tests/mock_end_to_end.rs` 覆盖 9 个场景：

| 用例 | 证明什么 |
| --- | --- |
| `plan_apply_journal_rollback_round_trip` | 计划 → 系统 → 审计链 → 逆序撤销，全链路闭环 |
| `dry_run_writes_nothing` | 默认安全：预演既不写系统也不写日志 |
| `apply_fails_closed_on_a_broken_chain` | 链不可信 ⇒ 一个写调用都不发（fail-closed） |
| `apply_keeps_going_after_a_step_failure` | 单步失败不拖垮整份计划，且只有成功的步骤入链 |
| `working_set_round_trips_back_to_the_previous_limits` | 工作集可回滚：`before` 是真值，回滚按字节还原（断言具体数值） |
| `working_set_without_a_readable_before_is_still_honest` | 读不到前值时不写假的 `before`，如实报 `NotActionable` |
| `legacy_import_then_rollback` | 旧格式导入 → 逆序恢复旧快照，且旧文件只读 |
| `watch_applies_once_per_process` | 监控模式下同一进程只优化一次 |
| `json_output_is_parseable_for_every_report` | 每类报告都能序列化成 JSON 对象 |

---

## 5. 文件清单

| 文件 | 内容 |
| --- | --- |
| `src/lib.rs` | crate 文档（数据流 / 红线 / 能力边界）+ lint 面 + 公开导出 |
| `src/i18n.rs` | `Lang` / `Text` / `pick`（中英双语，`GOPT_LANG`） |
| `src/error.rs` | `CoreError`（分类 + 稳定英文 + 双语建议 + 退出码；内层错误装箱保持 <= 128B） |
| `src/outcome.rs` | `Outcome<T>` / `Notice` / 退出码常量（`--json` 顶层形状） |
| `src/paths.rs` | `DataPaths`（数据目录解析与布局，唯一决定日志/策略/旧文件路径） |
| `src/model.rs` | 全部 DTO（status / plan / apply / rollback / journal / verify / explain / prio / tune / startup / report / watch） |
| `src/exec.rs` | 步骤执行原语（预演与执行同一段代码）、审计草稿生成 |
| `src/engine.rs` | `Session`（detect / status / games / plan / watch） |
| `src/apply.rs` | 写路径（apply / rollback / prio / tune / startup） |
| `src/inspect.rs` | 只读视图（journal / verify-journal / explain / report / import-legacy） |
| `src/report.rs` | 文本渲染（中英双语） |
| `tests/mock_end_to_end.rs` | Mock 后端端到端 9 场景 |
