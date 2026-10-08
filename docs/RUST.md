# GameOptimizer-RS 设计说明（Rust 重写 · Phase 1）

> **定位：C++ v1.1.0 仍是正式发布版。** 本文档描述的是仓库内新增的 Rust 实现（`rust/`，workspace 版本 `0.1.0`）：
> 它是 Phase 1 的**内核 + CLI 重写**，与 C++ 版**并存、互不依赖**，不参与 C++ 的发布链路，也不改任何 C++ 源码。
>
> 相关文档：[`../rust/README.md`](../rust/README.md)（怎么构建 / 怎么跑 / 策略怎么写）、
> [ARCHITECTURE.md](ARCHITECTURE.md)（C++ v1.1.0 架构）、[CONTRIBUTING.md](CONTRIBUTING.md)、
> 各 crate 自己的 README（`rust/crates/*/README.md`，细节最全）。

**Abstract (EN).** GameOptimizer-RS is an in-progress Rust rewrite of the same product surface: a declarative
TOML policy engine, a SHA-256 hash-chained audit journal with event-sourced rollback, a `SystemApi` HAL with
real Win32 and Mock backends, and a single core with pluggable front-ends (the CLI exposes `--json` everywhere).
The rewrite does not touch the C++ sources and does not replace the C++ v1.1.0 release; its goal is to turn
"reversible / explainable / official-API-only" from promises into engineering properties that CI can check.

---

## 1. 为什么重写（不只是换语言）

C++ v1.1.0 的红线是靠**约定 + 复核命令**守住的：优先级白名单在 `HAL::IsValidPriorityClass`，回滚靠
`SecurityRollback` 快照，红线复核靠 `Select-String`（见 [ARCHITECTURE.md §4/§6](ARCHITECTURE.md)）。
这些做法有效，但都是"事后扫一遍"。

Rust 版把同一批红线**往类型系统和数据结构里搬**，让它们变成可编译期拒绝、可单测、可在 CI 上一条命令复现的属性：

| 工程属性 | 在 Rust 版里变成什么 | 靠什么证明 |
| --- | --- | --- |
| 只用稳定官方 API | 全仓库只有 `gopt-hal` **一个 crate** 依赖 `windows` crate（平台细节收口在 `src/win32/`）；`gopt-policy`/`gopt-core`/`gopt-cli`/`gopt-journal`/`gopt-verify` 的依赖面里都没有它 | 依赖图（`cargo tree` / 各 crate 的 `Cargo.toml`）+ 内核与 CLI 的 `forbid(unsafe_code)` |
| 优先级上限 HIGH | `PriorityClass` 枚举**没有 REALTIME 成员**，`from_raw(0x100)` / `parse("realtime")` 直接 `PolicyDenied` | 编译期不可表达 + 单测（`hal_contract`、`invalid_toml`） |
| 一切可回滚 | 每条写入前先落审计记录（含 `before` 载荷），回滚计划从日志**重新推导** | `gopt-journal` 的 `plan_rollback*` 纯数据 + 真机往返自检 `gopt-verify` |
| 可检出篡改 / 可解释 | `journal.jsonl` 每行 `hash = SHA256(canonical(body) ‖ prev_hash)`；`verify-journal` 报第一处不一致 | 篡改一行 ⇒ `verify-journal` 退出码 3，且**所有写命令拒绝执行**（fail-closed） |
| 加游戏 / 改规则无需重编译 | 策略是 TOML：内置 10 款（`include_str!`）+ 用户 `policies.d/*.toml` 覆盖/新增 | 改一个 `.toml` 重启 `gopt` 即生效（`gopt list games`） |
| 无需管理员、无需游戏即可单测 | `SystemApi` trait + `MockApi`（可注入失败、可断言调用序列） | `cargo test --workspace` 298 个用例，全部不需要提权/不需要真实游戏 |

> 一句话：**C++ 版把红线写在注释和复核脚本里，Rust 版把红线写进类型和测试里。**

---

## 2. 架构总览

### 2.1 六个 crate（单向依赖，无环）

```text
  gopt-cli ──► gopt-core ──┬──► gopt-policy ──► gopt-hal
  （CLI 前端）  （编排内核） ├──► gopt-journal
                           └──► gopt-hal ─────────────────────► Windows 官方用户态 API
  gopt-verify ──► gopt-hal / gopt-journal
  （真机往返自检：不经内核，自己写入 → 读回 → 还原）
```

| crate | `.rs` 文件 | `.rs` 行数 | 职责 | 依赖 |
| --- | ---: | ---: | --- | --- |
| `gopt-hal` | 15 | 4,586 | `SystemApi` trait + `Win32Api` 真实后端 + `MockApi`；结构化错误；不 panic | `windows` 0.58、`serde` |
| `gopt-policy` | 19 | 6,507 | TOML schema 校验、条件求值、`Plan` 生成、用户覆盖 | `gopt-hal`、`serde`、`toml` |
| `gopt-journal` | 11 | 4,496 | 哈希链审计日志、崩溃安全追加、事件溯源回滚、旧格式只读解析 | `serde` |
| `gopt-core` | 12 | 5,803 | 编排（`Session`）、默认安全三层、DTO、i18n、文本渲染 | 上列三个 |
| `gopt-cli` | 4 | 2,189 | `gopt.exe`：参数解析、命令分发、`--json` | 只依赖 `gopt-core` |
| `gopt-verify` | 1 | 1,040 | `gopt-verify.exe`：7 项真机往返自检 | `gopt-hal`、`gopt-journal` |

合计 `rust/` 下 88 个文件 / 26,679 行（含 `Cargo.lock`、`.md`、`.toml`；不含 `target/`）。
作为参照：C++ 版 `src/` 38 个文件 10,565 行 + `tools/` 7 个文件 1,433 行。

### 2.2 依赖方向就是"红线"本身

* `gopt-cli` **不依赖** `gopt-hal` / `gopt-journal`：前端在依赖图上不可能绕过内核直接改系统；
* `gopt-core` 是唯一同时看到策略、HAL、日志的地方，也是唯一允许发起写入的地方；
* `windows` crate 只出现在 `gopt-hal` 的 `src/win32/`；`gopt-verify` 原先自带一个只读工作集探针
  （本 crate 唯一的 `unsafe` + 唯一的 `windows` 依赖），现在改为经 `SystemApi::get_working_set` 读取，
  于是**全仓库的 `unsafe` 与平台依赖都只剩 `gopt-hal` 一处**。

---

## 3. 设计要点 A：HAL trait 化（真实 Win32 + Mock 双实现）

`gopt-hal` 是"唯一被允许触碰系统"的入口：一个对象安全（object-safe）的 trait，14 个方法，全部 `&self`。

```text
读出类：get_priority / get_affinity / get_working_set / query_power_scheme
        list_run_entries / list_processes / is_elevated / hardware
写入类：set_priority / set_affinity / set_working_set / set_power_scheme / set_run_entry_enabled
身份类：backend_name
```

> 注：`get_working_set`（官方 `GetProcessWorkingSetSize`，只读）是后来补上的第 14 个方法 ——
> 它让"工作集写入前状态"从**不可知**变成**真值**，见 §6 末尾。HAL 的写入方法数量没变，
> 因此"写入一律返回回滚信息"这条契约不受影响。

三条契约要点：

1. **写入方法一律返回"回滚所需信息"**：`set_priority` 返回旧优先级、`set_power_scheme` 返回
   `{previous, current}`、`set_run_entry_enabled` 返回改名前后的 `RunEntry`。调用方要回滚，不需要再猜状态；
2. **失败没有第二条路径**：所有错误都是 `HalError { kind, operation, message(稳定英文), win32_code }`，
   `kind ∈ {invalid_argument, unsupported, not_found, access_denied, policy_denied, win32, internal}`。
   crate 内 `deny(clippy::unwrap_used / expect_used / panic / todo / unimplemented)`（仅测试豁免），
   `Mutex` 中毒走恢复路径 —— 于是"错误路径不 panic"是**可被 CI 检查**的，而不是承诺；
3. **红线在类型层**：`PriorityClass` 枚举里没有 REALTIME，唯一入口 `from_raw` 对 `0x100` 返回
   `PolicyDenied`；亲和性请求 `AffinityRequest` 字段私有 ⇒ 空掩码在类型上不可表达。

`MockApi` 不是"打桩返回固定值"，它带处理器拓扑、可注入失败、记录调用序列
（`MockApi::calls()` 可直接断言"策略 → 到底调了哪些系统 API"）。这就是为什么 **298 个测试可以在没有管理员、
没有游戏、没有改动机器的前提下跑完**，而真机部分交给 `gopt-verify`（见 §4.3）。

真机自检：`cargo run -p gopt-hal --example hal_selfcheck`（只读，**23 项通过**）与
`-- --apply-writes`（**28 项通过**，只对**当前进程**做"写入 → 读回 → 还原"，含工作集往返）。

---

## 4. 设计要点 B / C / D

### 4.1 声明式策略引擎（TOML）

```text
内置层：rust/policies/*.toml                （include_str! 进二进制，10 款游戏）
用户层：<数据目录>\policies.d\*.toml        （同 id 覆盖内置，新 id 新增；--data-dir / GOPT_DATA_DIR 可改）
```

* 一款游戏 = 一个 `[[game]]`（`id` / `name_zh` / `name_en` / `match` 通配 exe 名 / 别名 / 描述）；
  规则 = `[[game.rules]]`（可选 `id`、可选 `when`、必填 `action`）；
* `action` 六选一：`priority` / `affinity` / `working_set` / `power_scheme` / `run_entries` / `skip`
  （`skip` 是显式空动作，用来把"因硬件降级而跳过"写进 `Plan.skipped` 并带中英双语理由）；
* `when` 是数据而不是代码：`logical_cores` / `physical_cores` / `ram_gb` / `ram_mb` 支持
  `gt/gte/lt/lte/eq/ne`（同字段可给上下界），`gpu_vendor` 只允许 `eq/ne`，`is_elevated` 只允许布尔比较，多字段 AND；
* **加载期严格校验**：`deny_unknown_fields` 全局生效，类型不匹配 / 空区间 / `eq`+`ne` 矛盾 /
  `realtime` / `0x100` 一律带 **文件:行:列** 报错，且**不 panic、不影响其它策略**；
* 求值不失败：要么产生步骤，要么产生**可解释的跳过**（`condition_not_met` / `degraded` / `explicit_skip`）。

C++ 版对应物是 `GameOptimizationPreset.cpp` 里的编译期常量表 + `ApplyHardwareDegradation` 的 if 链：
加一款游戏要改 3 处代码并重编译；Rust 版改一个 `.toml` 就够（`rust/policies/example-custom.toml` 就是
"纯数据新增 2 款游戏 + 覆盖内置"的活样本）。

### 4.2 哈希链审计日志 + 事件溯源回滚

```text
journal.jsonl（每行一个对象，字段序固定）
  { id, ts_unix_ms, kind(apply|rollback|imported), target, before, after, rule_id, prev_hash, hash }
  hash = SHA256( canonical(除 hash 外的整条记录) ‖ prev_hash )
```

* **规范化（canonical）是自实现的 JSON 写入器**：对象键按 UTF-8 字节序、无空白、控制字符写 `\u00xx`、
  `>=0x20` 原样输出 UTF-8（中文原样）、浮点用最短往返表示。写盘那行 == 规范形式 + 恰好一个 `\n`；
  读取时重算规范形式做**逐字节**比对 —— 于是"重新格式化 / 加空格 / 改键序"同样被检出；
* `verify_chain()` 按固定顺序查：`malformed_line → id_sequence → genesis_prev_hash → prev_hash_link →
  record_hash → line_canonical`，报**第一处**不一致（`ChainBreak { index, line_no, id, problem, detail }`）；
* **能力边界写得诚实**：只靠文件自身，**尾部整行被删除**仍然自洽（剩余前缀是一条完整的链）
  ⇒ 需要外部锚点 `ChainAnchor { len, last_hash }`（`--anchor-len/--anchor-hash`），且锚点只覆盖前缀；
* **崩溃安全**：追加 = 一次 `write_all` + `flush` + `FlushFileBuffers`（fsync）；打开时截断最后一个 `\n`
  之后的残余字节并报出丢弃字节数（被丢弃的记录从未提交，下一个 id 复用它）；只读模式不截断、拒绝追加；
  追加前比对文件长度，长度变了 ⇒ `stale`；
* **事件溯源回滚**：`plan_rollback(to_id)` / `plan_rollback_all()` 只处理 `kind ∈ {apply, imported}`、
  按 id **降序**，产出类型化动作（`RestorePriority` / `RestoreAffinity` / `RestorePowerScheme` /
  `RestoreRunEntry` / `RestoreLegacySnapshot` / `NotActionable`）—— **纯数据、零系统调用**，由内核再翻成 HAL 调用；
  链不可信 ⇒ 拒绝出计划；
* **旧格式兼容是只读的**：`savepoints.txt`（12 字段）与 `games.conf` 的解析逐条对齐 C++
  `ParseField` / `TryDeserialize`（十进制优先、十六进制回退、超长判负不回绕……），导入以
  `kind=imported` 入链，绝不改写旧文件。

与 C++ 版的差别：C++ 的快照（`savepoints.txt`）是"应用前的状态转储"，能回滚但**无法自证没被动过**；
Rust 版把它换成"每次修改一条、带前向哈希"的日志，于是回滚变成"把 `before` 载荷按逆序再喂给 HAL"，
并且篡改可检出。代价是日志会随使用增长——这也是 `--limit` / `plan_rollback_pending()` 存在的原因。

### 4.3 单内核多前端（CLI 全量 `--json`）

* 前端只做两件事：**选语言**和**选格式**；文本渲染在 `gopt-core::report`，中英双语措辞只有一份；
* `--json` 的输出形状统一为 `Outcome<T>`：

```json
{
  "schema_version": 1,
  "ok": true,
  "command": "plan",
  "lang": "zh",
  "data": { "...": "命令相关 DTO" },
  "error": null,
  "notices": [{ "level": "warning", "zh": "…", "en": "…" }]
}
```

* 失败时 `ok=false`，`error.kind` 是稳定分类（`usage` / `invalid_argument` / `not_found` /
  `access_denied` / `policy_denied` / `unsupported` / `io` / `hal` / `journal` / `audit_chain_broken` / `internal`），
  `data` **仍可能带部分结果**（例如"执行到一半失败"时已应用的步骤）——脚本可以据此判断"到底改了什么"；
* 退出码只有四个，且在**二进制边界**上被测试钉死：`0` 成功（含预演成功）/ `1` 用法错误 /
  `2` 环境不满足 / `3` 审计链校验失败。

### 4.4 默认安全的三层（不是靠前端自觉）

1. `plan` **一行都不写**：不写系统、不写日志（Mock 的调用计数断言过）；
2. `apply` / `rollback` / `tune` / `startup` / `prio` / `import-legacy` 没有 `--yes` 一律**预演**：
   照常读当前值、算出前后差异，但不写系统、不建日志文件；
3. **fail-closed**：真正执行前先校验哈希链，链不可信 ⇒ 系统零改动（退出码 3）；
   执行中日志写不进去 ⇒ 立即停手并如实报出"已发生的改动"。幂等：已是目标值 ⇒ 不写、不记、状态 `unchanged`。

---

## 5. 红线落地对照表

| # | 红线（与 C++ 版一致） | Rust 版落点 | 可验证方式 |
| --- | --- | --- | --- |
| R1 | 仅官方 Win32 API | `gopt-hal/src/win32/*`（`windows` 0.58 白名单 feature）——**全仓库唯一**依赖 `windows`、唯一含 `unsafe` 的地方 | `cargo tree` / 各 crate 的 `Cargo.toml`；其余五个 crate 的依赖面里都没有 `windows` |
| R2 | 无注入 | 全仓库不出现 `CreateRemoteThread` / `WriteProcessMemory` / `VirtualAllocEx` / `QueueUserAPC` | 源码检索（0 命中）；写入路径只经 `SystemApi` |
| R3 | 无内核 Hook / 无驱动 | 无 `SetWindowsHookEx` / 驱动 / 服务相关调用；产物只有 EXE | 源码检索（0 命中）；`cargo build` 产物清单 |
| R4 | 优先级上限 HIGH | `PriorityClass` 枚举无 REALTIME；`from_raw`/`parse` 对 `0x100`/`realtime` 返回 `PolicyDenied` | 单测：`hal_contract::realtime_is_rejected_before_any_backend_call`、`invalid_toml` 的加载期拒绝 |
| R5 | 一切可回滚 | 每次写入前落 `before` 载荷；回滚计划由日志逆序推导 | `gopt-verify` 第 2/3/4/7 项真机往返；`cargo test -p gopt-core --test mock_end_to_end` |
| R6 | 全部功能免费 | 授权模块未移植（C++ 版本身也不做功能门控）；CLI 无 licence 命令 | 命令清单里没有授权/付费入口 |
| R7 | 中英双语 | `Lang` / `Text` / `pick` + `GOPT_LANG` / `--lang`；`render_*` 在核心层 | `gopt status --lang en` 与 `--lang zh` 对照；报告渲染单测 |
| R8 | 可解释 | `Plan.steps[].reason{zh,en}` / `Plan.skipped[].cause` / `explain --game \| --rule \| --journal-id` | `gopt explain --game cs2` 输出规则、行号、动作、HAL 操作 |

复核命令（PowerShell，与 [ARCHITECTURE.md §6](ARCHITECTURE.md) 的 C++ 版复核思路一致；在本机实跑过，输出如下）：

```powershell
cd rust
# R2/R3：注入 / Hook / 驱动 / 服务 API —— 期望 0 命中
(Get-ChildItem crates -Recurse -Include *.rs |
  Select-String -Pattern 'CreateRemoteThread|WriteProcessMemory|VirtualAllocEx|QueueUserAPC|SetThreadContext|SetWindowsHookEx|NtLoadDriver|OpenSCManager|CreateService' |
  Measure-Object).Count                                   # -> 0

# R1：`windows` crate 只应出现在 gopt-hal 一个 Cargo.toml 里
Get-ChildItem crates -Recurse -Filter Cargo.toml |
  Select-String -Pattern '^windows'                       # -> 只有 crates\gopt-hal\Cargo.toml

# R1：第三方厂商 SDK（NVAPI/ADL/NVML）—— 期望只剩"为什么不做"的注释
(Get-ChildItem crates -Recurse -Include *.rs |
  Select-String -Pattern 'nvapi|NVAPI|atiadlxx|nvml|LoadLibrary' |
  Measure-Object).Count                                   # -> 2（两处都是注释）
```

R4 不适合用检索复核（`realtime` 这个词在**拒绝路径**、错误消息与测试里本来就该大量出现，本机检索到 90 处）；
它由测试与退出码钉死：`hal_contract::realtime_is_rejected_before_any_backend_call`、
`invalid_toml` 的加载期拒绝矩阵、以及 CLI 边界上的
`gopt prio --pid <pid> --set realtime` → **退出码 1**（`用法错误: invalid --set: 'realtime' is rejected by policy: gopt never raises a process above HIGH_PRIORITY_CLASS`）。

---

## 6. 与 C++ v1.1.0 能力对照

> 结论先说：**C++ v1.1.0 覆盖的功能面仍然更宽（GUI/托盘/打包），Rust 版在"可验证性"上更强。**
> 下表是逐项对照，不含"谁更好"的判断。

| 维度 | C++ v1.1.0（正式发布） | GameOptimizer-RS（Phase 1） |
| --- | --- | --- |
| 语言 / 构建 | C++17 + CMake + MinGW（MSYS2 / w64devkit） | Rust 2021 + cargo workspace（6 crate，`lto=true`、`codegen-units=1`） |
| 产物 | `gopt_cli.exe` / `gopt_gui.exe` / `gopt_verify.exe` + 安装包 / 便携包 | `gopt.exe`（CLI）/ `gopt-verify.exe`；无 GUI、无安装包 |
| 游戏策略 | `GameOptimizationPreset.cpp` 编译期常量表（8 款），加游戏要改 3 处代码 | TOML 声明式：内置 10 款 + 用户 `policies.d` 覆盖/新增，**免重编译** |
| 硬件降级 | `ApplyHardwareDegradation` 的 if 链（编译期） | `when` 条件（数据），不满足时进 `Plan.skipped` 并给出双语理由 |
| 优先级 | `HAL::IsValidPriorityClass` 白名单五档，运行时拒绝 REALTIME | `PriorityClass` 枚举无 REALTIME 成员（类型级）+ 解析期拒绝 |
| 亲和性 | `ComputeAffinityMask`；逻辑核 > 64 直接放弃（返回 0） | 处理器组模型 + 逐组掩码；>64 核给逐组计划并逐线程 `SetThreadGroupAffinity` |
| 工作集 | `SetProcessWorkingSetSize`（无读回） | 读前值（`get_working_set`，官方 `GetProcessWorkingSetSize`）→ 写 → 读回，`before` 是**系统真值** |
| 回滚 | 应用前快照 `SavePoint` + `savepoints.txt` 持久化 + 心跳看门狗 | 哈希链审计日志 `journal.jsonl` + 事件溯源逆序回滚（从 `before` 载荷重新推导） |
| 篡改检测 | 无（快照文件被改动无法发现） | `verify_chain` 六序检查 + 外部锚点；链不可信 ⇒ fail-closed（退出码 3，系统零改动） |
| 崩溃安全 | 文件写入（无 fsync 语义保证） | 追加 + fsync；打开时修复尾部残行；只读模式不写 |
| 旧数据 | 写 `savepoints.txt` / `games.conf` | **只读**解析并导入（`kind=imported`），不改写旧文件 |
| 前端 | CLI + 原生 Win32 GUI（5 页 / 主题 / 托盘 / 单实例） | 仅 CLI，但**全量 `--json`** 且退出码/形状稳定，便于脚本与将来的 GUI 复用 |
| 默认安全 | `apply` 直接执行；`--dry-run` 显式预演 | 无 `--yes` **一律预演**；`plan` 零副作用；fail-closed |
| i18n | `T(zh, en)` 运行时切换（CLI + GUI） | `Lang` / `Text` / `pick` + `GOPT_LANG`；渲染在内核，前端只选语言/格式 |
| 依赖面 | Win32 SDK + 自研工具；厂商库仅用于"探测是否存在" | `windows` 0.58 + `serde` + `toml`；无 async / 日志框架 / GUI 框架 |
| 测试 | 真机 `gopt_verify.exe` 自检 + 人工点击验证 | 298 个 `cargo test`（Mock 后端，**不需要管理员、不需要游戏**）+ `gopt-verify` 7 项真机往返 |
| 发布 | `.github/workflows/build-release.yml`：版本一致性门禁 → 产物校验 → tag 发布 | 独立 `Rust CI`（非阻断，见 §8）；产物 `gopt.exe` 作为 artifact |
| 文档 | README / ARCHITECTURE / CONTRIBUTING / BEFORE_USE | `rust/README.md` + 本文 + 每个 crate 的 README |

**两处有意保留的差异（不是遗漏）：**

1. **驱动级帧延迟不实现**：C++ 版用 `LoadLibraryW` 探测厂商库（NVAPI/ADL）后降级跳过；Rust 版不做这件事，
   因为"仅官方 Win32 API"是红线，宁可不做也不引入第三方 SDK 硬依赖；
2. **工作集的"不可回滚"是读不到的兜底，而不是常态**：`get_working_set`（官方
   `GetProcessWorkingSetSize`）读出前值后，工作集步骤和其它步骤一样可回滚；
   只有两种情况退回 `before=null` + `not_actionable`：
   * 读前值失败（目标进程受保护 / 已退出）；
   * 系统报告 `min_bytes = 0` —— HAL 的写路径拒绝 `min = 0`，这个前值**写不回去**。

   两种情况都**不记录假的 `before`**：宁可让审计记录和回滚计划显式说"这一步撤不了"，
   也不写一条看起来可回滚、执行时才发现还原失败的记录。
   （`gopt-verify` 的 `working_set_round_trip` 会打印真实前后值，可在真机上复核。）

---

## 7. 迁移路线（Phase 1 → 2 → 3）

| 阶段 | 目标 | 已完成 / 待办 | 出口条件 |
| --- | --- | --- | --- |
| **Phase 1**（本轮） | 内核 + CLI + 审计链 + HAL 双实现；建立可验证性 | ✅ `gopt-hal` / `gopt-policy` / `gopt-journal` / `gopt-core` / `gopt-cli` / `gopt-verify`；✅ `cargo fmt/clippy/test/build` 全绿；✅ 真机 7 项 `RESULT: PASS` | 内核层功能与 C++ 版**策略语义**逐字段对齐（8 款游戏 + 降级路径）；CLI 的 `--json` 形状与退出码被测试钉死 |
| **Phase 2** | 前端对等 + 补齐其余契约缺口 | 待办：GUI / 托盘（复用 `gopt-core`，不再新增系统调用路径）；SMBIOS 内存频率 / CPUID 睿频；看门狗（C++ 版有，Rust 版尚未移植）；性能与内存基线对比。已完成：`SystemApi::get_working_set`（工作集读路径，见 §3/§6） | GUI 功能面覆盖 C++ 版 5 页；每个前端都只经 `gopt-core`；回滚计划里不再出现"因为读不到前值而不可撤"的工作集项 |
| **Phase 3** | 切换正式发布 | 待办：并行发布一个版本周期（C++ v1.1.0 与 RS 同时出包）；灰度/回退预案；安装包与签名；把 Rust CI 提升为发布门禁（`build-release.yml` 的 `build` job 加 `needs: rust`） | 连续一个版本周期内 RS 版无阻断缺陷；`gopt-verify` 在目标机型矩阵上 `RESULT: PASS`；发布链路（版本一致性 + 产物校验）对 RS 产物同样适用 |

**切换前的硬性前提**：在 Phase 3 之前，**C++ v1.1.0 始终是唯一正式发布版**，
`docs/`、`README.md`、版本号（`src/version.h` = 1.1.0）都不因 Rust 版而改变。

---

## 8. 工具链、PATH 坑与 CI

### 8.1 本机的 PATH 坑：`~/.cargo/bin` 里是**副本**而不是 rustup shim

本机 `%USERPROFILE%\.cargo\bin` 里放着 **rustc / cargo 的真实副本**（`cargo.exe` 约 30 MB、
`rustc.exe` + `rustc_driver-*.dll`），而不是 rustup 生成的转发 shim。**rustc 用"自己的 exe 所在目录的上一级"
推导 sysroot**，于是这个副本的 sysroot 落在 `C:\Users\<you>\.cargo`，而那里**没有 `lib\rustlib`**
（`~/.cargo` 下只有 `bin/`、`registry/`，没有 `lib/`）：

```text
$ %USERPROFILE%\.cargo\bin\rustc.exe --print sysroot
C:\Users\<you>\.cargo                                    <-- 错：这里没有 lib\rustlib

$ %USERPROFILE%\.cargo\bin\rustc.exe hello.rs            <-- 实测
error[E0463]: can't find crate for `std`
  |
  = note: the `x86_64-pc-windows-msvc` target may not be installed
  = help: consider downloading the target with `rustup target add x86_64-pc-windows-msvc`
（rustc 退出码 1）

$ %USERPROFILE%\.cargo\bin\cargo.exe build               <-- 同一个 crate，经 cargo 一样失败
error[E0463]: can't find crate for `std`
error: could not compile `probe` (bin "probe") due to 1 previous error
（cargo 退出码 101）
```

注意那句 `rustup target add x86_64-pc-windows-msvc` 是**误导性提示**：target 装着，只是 sysroot 找错了。
另一个同源现象：`cargo fmt` / `cargo clippy` 这类**子命令**只存在于工具链的 bin 目录
（`cargo-fmt.exe` / `cargo-clippy.exe`），`~/.cargo/bin` 里没有（历史上那里还放过 9 个 **0 字节占位文件**，
cargo 查找子命令时会优先命中同目录的坏文件；现已清理）。于是"只用 `~/.cargo/bin`"时：

```text
$ %USERPROFILE%\.cargo\bin\cargo.exe fmt --all -- --check   # 只有 ~/.cargo/bin 在 PATH 上时
error: no such command: `fmt`
（退出码 101；`cargo clippy` 同样是 `no such command: 'clippy'`）
```

**正确做法**：把 **rustup 工具链自己的 bin** 放在 PATH 最前（不要把它加进机器 PATH 的尾部、
也不要用 `~/.cargo/bin` 里的副本）：

```powershell
$tc = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"
$env:PATH = "$tc;$env:PATH"      # 只影响当前会话，不改机器配置

rustc --version                  # rustc 1.98.0 (...)
rustc --print sysroot            # C:\Users\<you>\.rustup\toolchains\stable-x86_64-pc-windows-msvc
cargo fmt --all --check          # 这样才会命中工具链里的真 cargo-fmt
cargo clippy --workspace --all-targets -- -D warnings
```

（无法改本机环境时的替代写法：`$env:RUSTUP_TOOLCHAIN` 不解决问题，因为 rustc 不是经由 rustup 启动的；
显式 `--sysroot` 也可以，但每条命令都要带，不如上面一行 PATH 干净。）

> **一次性开发机修复（不在仓库内，因为会改变机器状态）**：本机 `~/.cargo/bin` 里曾有 9 个 0 字节占位文件
> （`cargo-fmt.exe` / `cargo-clippy.exe` / `rustdoc.exe` / `rust-analyzer.exe` …），会让
> `cargo fmt` / `cargo clippy` 报"不是有效的 Win32 应用程序"或 `no such command`；这些文件已于
> 2026-10-08 清理（`cargo.exe` / `rustc.exe` 两个**副本**仍在，因此 §8.1 的 E0463 依然成立，
> PATH 前置工具链 bin 仍是必须的）。CI 上不存在这个问题：GitHub runner 由 rustup 安装，
> `~/.cargo/bin` 是正常的转发 shim。

### 8.2 CI：独立的、非阻断的 `Rust CI`

`.github/workflows/rust-ci.yml`（windows-latest）依次跑：

| 步骤 | 命令 | 为什么需要 |
| --- | --- | --- |
| 工具链自检 | `rustup show` / `rustup component add rustfmt clippy` / `rustc --version` | 固定住"是哪个工具链在跑"，并把 fmt/clippy 组件显式补齐 |
| 格式 | `cargo fmt --all -- --check` | 免争论的格式门禁（本地同样一条命令） |
| 静态检查 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | 把 `unwrap/expect/panic` 类错误路径当**警告即错误**处理 |
| 单元 + 集成 | `cargo test --workspace --locked` | 298 个用例；Mock 后端 ⇒ 不需要管理员、不需要游戏 |
| 发布构建 | `cargo build --release --workspace --locked` | 验证 `lto=true` 的发布档能构建 |
| 冒烟 | `gopt.exe --version` / `help` / `GOPT_BACKEND=mock plan cs2` / `mock apply` / `verify-journal` | 证明**产物真的能跑**、策略→计划→审计链在干净 runner 上闭环 |
| 产物 | `actions/upload-artifact`：`rust/target/release/gopt.exe` + `gopt-verify.exe` | 下载即可在真机上跑（名字 `gopt-rs-windows-x86_64`，与 C++ 产物名不冲突） |
| 真机往返（advisory） | `gopt-verify.exe`，`continue-on-error: true` | runner 的 CPU 拓扑/电源方案不可控，因此只作为参考日志，**不让它把 job 变红** |

**为什么是独立 workflow，而不是往 `build-release.yml` 里塞一个 job：**

* `build-release.yml` **逐字节未改动** ⇒ C++ 的版本一致性门禁（`tools/check_version.ps1`）、产物校验、
  tag → GitHub Release 的链路行为零变化；
* Rust 侧红或绿**都不影响 Release 发布**——这正是"Rust 默认不阻断发布"的落地方式，
  同时保留"红就是红"的可见性（而不是用 `continue-on-error` 把失败涂成绿色）；
* 想把它变成发布门禁：在 `build-release.yml` 的 `build` job 上加一行 `needs: rust`
  （要求 job 名与 `rust-ci.yml` 中的 `rust` job 对应），这属于 Phase 3 的决策，不在本轮。

**触发条件**：`push`（`main`/`master` 与 `v*` tag）、`pull_request`（仅当改动涉及 `rust/**` 或本 workflow 文件，
避免纯 C++ PR 白跑）、`workflow_dispatch`。

### 8.3 本地等价命令（与 CI 逐步对齐）

```powershell
$tc = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"
$env:PATH = "$tc;$env:PATH"
cd rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --workspace --locked
```

---

## 9. 已知边界与风险（诚实清单）

**能力边界**

1. 没有 GUI / 托盘：`gopt-core` 已经为多前端准备好（渲染在核心层），但 Phase 1 只交付 CLI；
2. 工作集的"前值"在两种情况下读不到（读失败 / 系统报告 `min = 0`），此时审计 `before=null`、
   回滚计划报 `NotActionable`（见 §6 末尾）；真实进程上正常可回滚；
3. 跨处理器组亲和性只对"逐组"语义成立：单组计划可直接 `SetProcessAffinityMask`，
   多组计划需要策略层显式 `per_group()` 拆分，否则报 `Unsupported`；
4. 尾部整行删除 / 截断只靠文件自身检测不出来，需要外部锚点；
5. 物理核 ≤ 2 的机器上亲和性规则不触发（与 C++ 版一致），但会留下可解释的跳过说明；
6. 硬件画像缺 SMBIOS 内存频率 / CPUID 最大睿频（宁缺毋滥，不提供恒为 0 的字段）。

**工程风险**

| 风险 | 现状 | 缓解 |
| --- | --- | --- |
| CI 时长 | 本地增量 release 构建 26.1 s；CI 冷启动要编译 `windows` 0.58 等依赖，预计 3–6 分钟；`lto=true` + `codegen-units=1` 会放大 release 档耗时 | 用 `actions/cache@v4` 缓存 `~/.cargo/registry`、`~/.cargo/git`、`rust/target`（key 基于 `Cargo.lock` 哈希）；job `timeout-minutes: 30` |
| 缓存体积 | `rust/target` 含 debug + release 两套产物，可能接近 1 GB | 缓存 key 绑定 `Cargo.lock`，`restore-keys` 做前缀回退；仓库缓存上限 10 GB，必要时只缓存 registry |
| 真机自检在 CI 不可控 | runner 的 CPU 拓扑 / 电源方案与开发机不同 | `gopt-verify` 步骤设为 advisory（`continue-on-error: true`），正式判定在开发机上手跑 |
| 与 C++ 版语义漂移 | 两边各有一份策略语义（C++ 常量表 vs TOML） | `gopt-policy` 的 `tests/builtin_policies.rs` 断言内置策略与 C++ `GameId` 逐款对齐；新增游戏两边都要改，Phase 2 考虑加"对等性"测试 |
| 双实现并存 | 仓库同时存在 C++ 与 Rust 两套构建 | 依赖面完全隔离（Rust 不链接 C++ 产物），CI 也分开跑；发布链路只认 C++ |

---

## 10. 本轮验证记录（实跑，2026-10-08 · 本机 Ryzen 9 7945HX / 15C32T / 1 处理器组）

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 退出码 0（无输出） |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 退出码 0（无告警） |
| `cargo test --workspace --locked` | 退出码 0：19 个测试目标全部 `ok`，**298 passed / 0 failed**（含 8 个 doctest） |
| `cargo build --release --workspace --locked` | 退出码 0，`Finished release profile [optimized] target(s) in 26.08s` |
| `cargo run -p gopt-hal --example hal_selfcheck` | 退出码 0：**23 项通过 / 0 项失败** |
| `cargo run -p gopt-hal --example hal_selfcheck -- --apply-writes` | 退出码 0：**28 项通过 / 0 项失败**（含工作集 64/128 MiB 写入 → 读回 → 还原） |
| `cargo run -p gopt-policy --example policy_selfcheck` | 退出码 0：加载 10 款策略（0 错误）、真机画像、进程匹配、计划生成全部正常 |
| `gopt --version` | `gopt 0.1.0 (GameOptimizer-RS Phase 1) — C++ v1.1.0 remains the official release` |
| `gopt status`（真实后端，`--data-dir %TEMP%`） | 退出码 0：识别 15C/32T、16064 MiB、RTX 4060 Laptop、1 个处理器组、策略 10 款、链状态完好 |
| `gopt status --lang en` | 退出码 0：同一份数据输出英文（`HAL backend: win32 (elevated)`） |
| `GOPT_BACKEND=mock gopt plan cs2` | 退出码 0：3 步计划 + 3 条可解释跳过（含中英双语理由与规则行号） |
| `GOPT_BACKEND=mock gopt apply cs2 --yes` → `journal` → `verify-journal` → `rollback --all --yes` | 退出码全 0：3 条 `apply` 记录入链（priority / affinity / 工作集，**三条都能读到写入前真值**）、链校验 3/3、回滚 **3 可执行 / 3 已执行 / 0 不可撤** 并追加 3 条 `rollback` 记录 |
| 篡改 `journal.jsonl` 一个字节后 `verify-journal` | 退出码 3，报出第一处不一致（`record_hash`，含期望/实际哈希），并给出双语建议 |
| 篡改后 `apply --yes` / `rollback --all --yes` | 均退出码 3（fail-closed：系统零改动） |
| `gopt import-legacy`（旧文件不存在） | 退出码 0：明确报"文件不存在"，不伪造条目 |
| `gopt-verify` | 7/7 PASS，最后一行 `RESULT: PASS`（详见 `rust/crates/gopt-verify/README.md`） |

> 逐条命令与完整输出见 [`../rust/README.md`](../rust/README.md) §3 与 §6。

**关于"工作集可回滚"（本轮的最后一个真缺口）**：验证时先用 Mock 后端跑完整闭环，
三条 `apply` 记录的 `before` 都是真值，`rollback --all --yes` 报
`3 steps (3 executable, 0 not actionable)`；再用 `hal_selfcheck --apply-writes` 与
`gopt-verify` 在**真机**上复核"写入 → 读回 → 还原"（工作集 `64/128 MiB → 67108864/134217728 bytes → 还原`）。
降级路径（读失败 / `min = 0`）由单元测试覆盖，不在真机自检里制造。
