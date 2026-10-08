# GameOptimizer-RS（`rust/`）

Rust 重写（Phase 1）：**声明式 TOML 策略引擎 + 哈希链审计日志/事件溯源回滚 + `SystemApi` HAL（真实 Win32 + Mock）
+ 单内核多前端（CLI 全量 `--json`）**。

> **状态：C++ v1.1.0 仍是正式发布版。** 本目录是与之并存的重写实现，不参与 C++ 的发布链路，
> 也**不改动任何 C++ 源码**。设计说明、与 C++ 的能力对照、迁移路线见 [`../docs/RUST.md`](../docs/RUST.md)。
>
> 工具链：rustc / cargo 1.98.0（`rust-version = 1.80`）· edition 2021 · release 档 `lto = true` + `codegen-units = 1`。

---

## 1. 六个 crate

| crate | 是什么 | 依赖谁 |
| --- | --- | --- |
| [`gopt-hal`](crates/gopt-hal/README.md) | `SystemApi` trait（14 个方法）+ `Win32Api` 真实后端 + `MockApi`；结构化错误、不 panic；**全仓库唯一依赖 `windows` crate（也是唯一有 `unsafe`）的地方** | `windows` 0.58、`serde` |
| [`gopt-policy`](crates/gopt-policy/README.md) | TOML 策略：加载、严格校验、条件求值、`Plan` 生成、用户覆盖 | `gopt-hal`、`serde`、`toml` |
| [`gopt-journal`](crates/gopt-journal/README.md) | 哈希链审计日志、崩溃安全追加、事件溯源回滚、旧格式只读解析 | `serde` |
| [`gopt-core`](crates/gopt-core/README.md) | 编排内核 `Session`：detect → plan → apply → rollback；DTO、i18n、文本渲染 | 上列三个 |
| [`gopt-cli`](crates/gopt-cli/README.md) | `gopt.exe`：手写参数解析 + 全量 `--json` | **只**依赖 `gopt-core` |
| [`gopt-verify`](crates/gopt-verify/README.md) | `gopt-verify.exe`：7 项真机往返自检（`RESULT: PASS`）；经 HAL 读工作集，无 `unsafe`、无平台依赖 | `gopt-hal`、`gopt-journal` |

数据流一句话：`detect → load policy（内置 + 用户 policies.d）→ plan（只读）→ apply（写审计日志）→ rollback（从日志逆序）`。

---

## 2. 构建与测试

### 2.1 先读这一段：工具链 PATH

本机 `%USERPROFILE%\.cargo\bin` 里放的是 **rustc / cargo 的二进制副本**（不是 rustup 转发 shim），
而 rustc 用"自己所在目录的上一级"推导 sysroot ⇒ sysroot 会落到 `C:\Users\<you>\.cargo`（那里**没有** `lib\rustlib`），
于是**任何编译**都会失败：

```text
error[E0463]: can't find crate for `std`
  = note: the `x86_64-pc-windows-msvc` target may not be installed      <-- 误导：target 是装着的
```

正确做法是把 **rustup 工具链自己的 bin** 前置到 PATH（只影响当前会话，不动机器配置）：

```powershell
$tc = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"
$env:PATH = "$tc;$env:PATH"

rustc --version           # rustc 1.98.0 (...)
rustc --print sysroot     # C:\Users\<you>\.rustup\toolchains\stable-x86_64-pc-windows-msvc （必须指向工具链）
```

完整实测（报错原文、`cargo fmt`/`cargo clippy` 的 "no such command" 现象、替代方案）见
[`../docs/RUST.md`](../docs/RUST.md) §8.1。

### 2.2 常用命令（本机实跑结果）

```powershell
cd rust
cargo fmt --all -- --check                                              # 格式门禁
cargo clippy --workspace --all-targets --locked -- -D warnings           # 静态检查（告警即错误）
cargo test  --workspace --locked                                        # 单测 + 集成 + doctest
cargo build --release --workspace --locked                              # 产物：target/release/gopt.exe
```

| 命令 | 本机结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 退出码 0（无输出） |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 退出码 0（无告警） |
| `cargo test --workspace --locked` | 退出码 0：19 个测试目标全部 `ok`，**298 passed / 0 failed**（含 8 个 doctest） |
| `cargo build --release --workspace --locked` | 退出码 0，`Finished release profile [optimized] target(s) in 26.08s` |

`--locked` 表示"严格按 `Cargo.lock` 解析依赖"（该文件随仓库提交，保证可复现构建）；
只想快速迭代时可以去掉。

**测试为什么不需要管理员、也不需要游戏**：所有系统交互都走 `SystemApi`，测试用 `MockApi`
（带处理器拓扑、可注入失败、记录调用序列）。真机部分交给第 6 节的 `gopt-verify`。

### 2.3 只读自检（都不用管理员）

```powershell
cargo run -p gopt-hal    --example hal_selfcheck        # HAL 真机只读自检：23 项通过
cargo run -p gopt-hal    --example hal_selfcheck -- --apply-writes   # 追加"写入→读回→还原"（只碰当前进程）：28 项通过
cargo run -p gopt-policy --example policy_selfcheck     # 策略加载 + 真机画像 + 进程匹配 + 生成计划
cargo run -p gopt-cli    -- --version                   # 直接跑 CLI（等价于 target/debug/gopt.exe --version）
```

---

## 3. CLI 速查

`rust/target/release/gopt.exe`（或 `cargo run -p gopt-cli -- <参数>`）。

### 3.1 全局选项

| 选项 | 作用 |
| --- | --- |
| `--json` | 结构化输出（`Outcome<T>`，含 `schema_version`）；`watch --json` 是 JSONL |
| `--lang zh\|en` | 输出语言（默认跟随 `GOPT_LANG`） |
| `--data-dir <目录>` | 数据目录（优先级高于 `GOPT_DATA_DIR`） |
| `-y`, `--yes` | 真正执行写操作（不加就是预演） |
| `-h`, `--help` / `-V`, `--version` | 帮助 / 版本 |

选项可以出现在命令行任何位置（`gopt --json status` ≡ `gopt status --json`）；
取值支持 `--pid 1234` 与 `--pid=1234`；**未知选项一律是用法错误**（不会被静默忽略）。

### 3.2 命令与退出码

实测的 `gopt --help` 输出：

```text
GameOptimizer-RS —— 游戏进程优化（默认只读，一切可回滚）

用法: gopt [全局选项] <命令> [参数] [选项.]

全局选项:
  --json                 结构化 JSON 输出（含 schema_version）
  --lang zh|en           输出语言（默认跟随 GOPT_LANG）
  --data-dir <目录>     数据目录（默认 %LOCALAPPDATA%\GameOptimizer，也可用 GOPT_DATA_DIR）
  -h, --help             本帮助
  -V, --version          版本

命令:
  status                                 机器/策略/审计链总览
  list [games|processes|startup]         列出策略、进程或启动项
  plan <游戏|exe|pid>                      只读预览执行计划（不改系统）
  apply <游戏|exe|pid> [--yes]             执行计划（无 --yes 时只预演）
  rollback [--to N|--all|--pending]      从审计日志逆序撤销
  journal [--kind K] [--limit N]         查看审计记录
  explain --game|--rule|--journal-id     解释某个游戏/规则/记录
  verify-journal [--strict]              校验哈希链（篡改可检出）
  watch [--interval S] [--once]          监控新出现的游戏进程
  prio [--pid N|--exe X] [--set C]       读/改进程优先级（上限 high）
  tune [--power-scheme high|balanced]    查询/切换电源方案
  startup [list|enable|disable NAME]     启用/禁用开机启动项
  report [--out FILE]                    生成体检报告
  import-legacy [--yes]                  只读导入 C++ v1.1.0 旧格式
  help [命令]                              命令用法

默认安全: apply / rollback / tune / startup / prio / import-legacy 没有 --yes 时只预演。
  退出码 — 0 成功 / 1 用法错误 / 2 环境不满足 / 3 审计链校验失败
```

| 退出码 | 含义 | 典型场景 |
| ---: | --- | --- |
| 0 | 成功 | 含"预演成功"（没有 `--yes` 时只打印计划） |
| 1 | 用法错误 | 未知命令/选项、缺参数、`--lang de`、`--set realtime`（红线） |
| 2 | 环境不满足 | 游戏没在运行、进程已退出、未提权、系统不支持、文件不可写 |
| 3 | 审计链校验失败 | 日志被篡改/损坏；此时**所有写命令都拒绝执行**（fail-closed） |

### 3.3 环境变量

| 变量 | 作用 |
| --- | --- |
| `GOPT_DATA_DIR` | 数据目录（等价 `--data-dir`，命令行优先）：`journal.jsonl` / `policies.d/` / `savepoints.txt` / `games.conf` |
| `GOPT_LANG` | 默认语言 `zh` / `en` |
| `GOPT_BACKEND=mock` | 用 Mock 后端跑（**仅供测试/CI/文档**：不碰真实系统）；不设置时永远是真实 Win32 后端 |

把数据目录指到 `%TEMP%`，整条链路（含审计日志）就完全不碰用户配置——本文档所有示例都这么做。
下文贴出的实测输出里，路径中的用户名统一脱敏成 `C:\Users\<you>`（其余逐字节照抄）。

### 3.4 一次真实会话（Mock 后端，不碰系统）

```powershell
$g = ".\target\release\gopt.exe"
$env:GOPT_DATA_DIR = "$env:TEMP\gopt-demo"
$env:GOPT_BACKEND  = "mock"          # 让 cs2.exe(pid 1234) 等进程"存在"，无需真的启动游戏

& $g status            # 环境 / 审计链 / 策略 / 正在运行的游戏 总览
& $g list games        # 策略清单
& $g plan cs2          # 只读预览：会改什么、为什么、调哪个 HAL 操作
& $g apply cs2 --yes   # 真执行（Mock 后端：只动内存里的假进程 + %TEMP% 里的日志）
& $g journal           # 看审计记录
& $g rollback --all    # 不带 --yes：预演回滚
& $g rollback --all --yes
```

`plan` 的真实输出（节选）：

```text
计划 cs2 — CS2 / Counter-Strike 2 (运行中 pid 1234)
策略来源: <builtin>/cs2.toml | 步骤: 3 | 跳过: 3 (只读，不改系统)
候选: cs2
  1. [set_priority] 把 CS2 的进程优先级设为「高」（本工具上限 HIGH，绝不使用 REALTIME）
     规则: priority:26 — set_priority
  2. [set_affinity] 绑定 CPU 亲和性：仅物理核；保留全局序号最小的 1 个物理核给系统；实际选中 14 / 16 个逻辑处理器
     规则: affinity:31 — set_affinity
  3. [set_working_set] 设置工作集下限 256 MB（无上限；可被系统随时回收，不是内存预分配）
     规则: working-set:42 — set_working_set
  - affinity-skipped-too-few-cores [条件不满足]: 条件不满足：物理核 ≤ 2
  - working-set-halved [条件不满足]: 条件不满足：内存 ≥ 8 GB 且 < 16 GB
  - working-set-skipped-low-memory [条件不满足]: 条件不满足：内存 < 8 GB
```

注意三点：**步骤是可解释的**（规则 id + 行号 + 中英双语理由 + 对应的 HAL 操作）；
**条件是数据**（同一份策略在不同硬件上自动走不同分支）；**被跳过的也说明理由**（不是静默忽略）。

`--json` 的形状（`gopt --json status` 的开头）：

```json
{
  "schema_version": 1,
  "ok": true,
  "command": "status",
  "lang": "zh",
  "data": {
    "product": "GameOptimizer-RS",
    "version": "0.1.0",
    "cpp_release": "1.1.0",
    "backend": "win32",
    "journal_records": 5,
    "chain_ok": true,
    "chain_summary": "hash chain verified: 5 of 5 records",
    "policies_total": 12,
    "policies_builtin": 10,
    "policies_user": 2,
    "policy_errors": 0,
    "hardware": { "physical_cores": 15, "logical_cores": 32, "...": "…" }
  },
  "error": null,
  "notices": []
}
```

---

## 4. 策略文件怎么写

### 4.1 两层加载

```text
内置层：rust/policies/*.toml                             —— include_str! 编译进二进制（10 款游戏）
用户层：<数据目录>\policies.d\*.toml                      —— 可选；同 id 覆盖内置，新 id 新增
       默认数据目录 = %LOCALAPPDATA%\GameOptimizer
```

**加游戏 / 改规则不需要重新编译**：把 TOML 丢进 `policies.d\` 即可（`--data-dir` / `GOPT_DATA_DIR` 可改位置）。

### 4.2 一个可以直接抄的完整示例

存成 `<数据目录>\policies.d\my-game.toml`：

```toml
schema = 1

[[game]]
id = "my-game"
name_zh = "我的游戏"
name_en = "My Game"
match = "MyGame.exe"                            # exe 名通配（* ?），不是路径
exe_aliases = ["MyGame-Win64-Shipping.exe"]     # 同一游戏多个可执行文件
name_aliases = ["mygame", "我的游戏"]            # plan/apply 时也接受这些名字
description_zh = "High 优先级；仅物理核并保留 2 个；16GB 以上把工作集下限抬到 512MB"
description_en = "High priority; physical cores only with 2 reserved; 512MB working-set floor above 16GB"

[[game.rules]]
id = "priority"
action = { priority = { class = "high" } }      # 五档白名单：idle/below-normal/normal/above-normal/high

[[game.rules]]
id = "affinity"
when = { physical_cores = { gte = 4 } }         # 物理核 < 4 的机器上这条不生效（可解释地跳过）
action = { affinity = { physical_only = true, reserve_cores = 2 } }

[[game.rules]]
id = "working-set"
when = { ram_gb = { gte = 16 } }
action = { working_set = { min_mb = 512 } }     # 只设下限（max 缺省 = 无上限）

[[game.rules]]
id = "power-high-performance"
when = { is_elevated = { eq = true } }          # 未提权时不触发（电源方案需要管理员）
action = { power_scheme = { scheme = "high" } }

# 文档演示用：match 命中 Mock 后端里"一直在运行"的 steam.exe，
# 因此不需要真的启动游戏就能看到计划生成（去掉这段不影响上面的 my-game）。
[[game]]
id = "steam-boost"
name_zh = "Steam 演示（Mock 后端）"
name_en = "Steam demo (Mock backend)"
match = "steam.exe"

[[game.rules]]
id = "priority"
action = { priority = { class = "above-normal" } }

[[game.rules]]
id = "affinity"
when = { logical_cores = { gte = 8 } }
action = { affinity = { reserve_cores = 1 } }

[[game.rules]]
id = "working-set"
action = { working_set = { min_mb = 256 } }
```

生效后 `gopt list games` 会多出两款（实测输出，节选）：

```text
可用策略: 12
  my-game                我的游戏 / My Game                 MyGame.exe       4 条规则 / 未运行
    来源: C:\Users\<you>\AppData\Local\Temp\gopt-demo\policies.d\my-game.toml — set_priority, set_affinity, set_working_set, set_power_scheme
  steam-boost            Steam 演示（Mock 后端） / Steam demo (Mock backend) steam.exe        3 条规则 / 运行中 pid 5678
    来源: C:\Users\<you>\AppData\Local\Temp\gopt-demo\policies.d\my-game.toml — set_priority, set_affinity, set_working_set
  cs2                    CS2 / Counter-Strike 2         cs2.exe          6 条规则 / 运行中 pid 1234
    来源: <builtin>/cs2.toml — set_priority, set_affinity, set_working_set
  ...
```

计划生成的实跑证据（`steam-boost` 匹配 Mock 后端的 `steam.exe`，所以不需要真实游戏）：

```text
计划 steam-boost — Steam 演示（Mock 后端） / Steam demo (Mock backend) (运行中 pid 5678)
策略来源: C:\Users\<you>\AppData\Local\Temp\gopt-demo\policies.d\my-game.toml | 步骤: 3 | 跳过: 0 (只读，不改系统)
候选: steam-boost
  1. [set_priority] 把 Steam 演示（Mock 后端） 的进程优先级设为「高于正常」（本工具上限 HIGH，绝不使用 REALTIME）
     规则: priority:40 — set_priority
  2. [set_affinity] 绑定 CPU 亲和性：保留全局序号最小的 1 个逻辑核给系统；实际选中 15 / 16 个逻辑处理器
     规则: affinity:44 — set_affinity
  3. [set_working_set] 设置工作集下限 256 MB（无上限；可被系统随时回收，不是内存预分配）
     规则: working-set:49 — set_working_set
```

`gopt explain --game steam-boost` 逐条讲清"条件 / 动作 / 调用"（含行号）：

```text
游戏 steam-boost — Steam 演示（Mock 后端） / Steam demo (Mock backend) (...\policies.d\my-game.toml)
  exe 匹配: steam.exe / 名称别名: -
== 规则 ==
  - priority (行:40): 无条件（总是生效）
    动作: 优先级 = 高于正常（above-normal）
    调用: set_priority  [条件满足]
  - affinity (行:44): 逻辑核 ≥ 8
    动作: CPU 亲和性 = 保留全局序号最小的 1 个逻辑核给系统
    调用: set_affinity  [条件满足]
  - working-set (行:49): 无条件（总是生效）
    动作: 工作集下限 = 256 MB
    调用: set_working_set  [条件满足]
```

### 4.3 字段速查

| 位置 | 字段 | 说明 |
| --- | --- | --- |
| 文件 | `schema` | 可选，目前只接受 `1`；其它版本报错 |
| `[[game]]` | `id` | kebab-case，唯一（用户层同 id = 覆盖内置） |
| | `name_zh` / `name_en` | 双语显示名（必填） |
| | `match` | exe 名通配 `*` / `?`，大小写不敏感；**拒路径与裸 `*`** |
| | `exe_aliases` / `name_aliases` | 额外的 exe 名 / 查询别名 |
| | `description_zh` / `description_en` | 可选描述 |
| `[[game.rules]]` | `id` | 可选（缺省按 `游戏id.动作` 推导；同种动作第 k 条加 `-k`） |
| | `when` | 可选（缺省 = 无条件） |
| | `action` | 必填，六选一：`priority` / `affinity` / `working_set` / `power_scheme` / `run_entries` / `skip` |
| 动作 | `priority { class }` | `idle` / `below-normal` / `normal` / `above-normal` / `high`（`realtime` 等一律加载期拒绝） |
| | `affinity { reserve_cores \| mask, reserve_from, physical_only }` | `reserve_from` 默认 `"first"`，可 `"last"`；`mask` 只用于组 0（十进制或 `"0x..."`） |
| | `working_set { min_mb, max_mb }` | `max_mb` 缺省 / 0 = 无上限 |
| | `power_scheme { scheme }` | `"high"` / `"balanced"`（系统级、需管理员） |
| | `run_entries { hive, disable = [名称], ignore_missing }` | `hive = hkcu \| hklm \| both`；禁用 = 改名迁移 `Foo` → `[disabled] Foo` |
| | `skip { reason_zh, reason_en }` | 显式空动作：把"这里故意不做"写进 `Plan.skipped` |
| 条件 | `logical_cores` / `physical_cores` / `ram_gb` / `ram_mb` | `gt/gte/lt/lte/eq/ne`，或标量简写；同字段可给上下界（`gte = 8, lt = 16`） |
| | `gpu_vendor` | 只允许 `eq`/`ne`：`nvidia` / `amd` / `intel` / `microsoft` / `unknown` / `none` |
| | `is_elevated` | 只允许布尔 `eq`/`ne` |
| | 组合 | 多个字段是 **AND** |

### 4.4 写错了会怎样（不 panic、不影响其它策略）

故意写一份把优先级设成红线值 `realtime` 的 `policies.d\typo.toml`：

```toml
schema = 1

# 故意写错：优先级用了红线里的 REALTIME
[[game]]
id = "typo"
name_zh = "写错的策略"
name_en = "Typo policy"
match = "typo.exe"

[[game.rules]]
id = "priority"
action = { priority = { class = "realtime" } }
```

`gopt status` 实测输出（**文件:行:列 + 稳定英文原因**，其余策略照常可用）：

```text
! 策略文件加载失败: ...\policies.d\typo.toml:12:33: error: priority: `realtime` is rejected by policy: gopt never raises a process above HIGH_PRIORITY_CLASS
...
== 策略 ==
总数: 12 (内置 10 + 用户 2) / 错误: 1 / 警告: 0
  ! 有 1 个策略文件加载失败（其余策略照常可用，见 policy_diagnostics）
```

同一份文件里若还有别的错误（例如未知字段、`gt` 与 `gte` 并存），修好第一处后再跑就会报下一处
（实测：`error: unknown field 'priority_class', expected one of 'id', 'name_zh', ...`）。
`--json` 下诊断在 `data.policy_diagnostics[]`（`severity` / `layer` / `file` / `line` / `column` / `message`）。

### 4.5 覆盖内置策略

用户目录里放**同 `id`** 的文件即可整条覆盖（注意是覆盖，不是字段合并）：

```toml
[[game]]
id = "cs2"                       # 与内置同 id ⇒ 覆盖内置的 cs2
name_zh = "CS2（保守档）"
name_en = "Counter-Strike 2 (conservative)"
match = "cs2.exe"

[[game.rules]]
id = "priority"
action = { priority = { class = "above-normal" } }

[[game.rules]]
id = "affinity"
action = { affinity = { reserve_cores = 4 } }
```

覆盖会留下一条 warning（不会静默生效）。内置的 `policies/example-custom.toml` 里还有
"显式掩码"、"`reserve_from = "last"`"、"释放 HKCU 启动项"的可运行示例。

---

## 5. 审计日志与回滚

### 5.1 位置与格式

```text
<数据目录>\journal.jsonl          默认 %LOCALAPPDATA%\GameOptimizer\journal.jsonl
```

每行一个对象（`deny_unknown_fields`，字段序固定）：

```json
{"id":1,"ts_unix_ms":1791443583478,"kind":"apply","target":"pid:1234",
 "before":{"name":"cs2.exe","pid":1234,"priority":"normal"},
 "after":{"name":"cs2.exe","pid":1234,"priority":"high"},
 "rule_id":"policy:game/cs2/priority",
 "prev_hash":"0000...0000",
 "hash":"90b5724f349d2e62202e3e82effdf7b28e0ac174ec1e6c82322973e66b4ed004"}
```

* `kind ∈ apply | rollback | imported`；`target` 约定 `pid:<pid>` / `power-scheme` / `run:<HIVE>:<name>` / `game:<index>`；
* `hash = SHA256(canonical(除 hash 外的整条记录) ‖ prev_hash)`：**改一个字节、加一个空格、换一次键序都会被检出**；
* 写入是"一次 `write_all` + `flush` + `fsync`"；打开时若发现尾部残行会截断并报出丢弃字节数。

### 5.2 查看 / 校验 / 回滚

```powershell
& $g journal                             # 记录 + 链状态 + 待撤销/已撤销
& $g journal --kind apply --limit 20     # 过滤
& $g verify-journal                      # 校验哈希链：0 = 完好，3 = 被篡改/损坏
& $g verify-journal --anchor-len 3 --anchor-hash <H>   # 带外部锚点（能发现"尾部整行被删"）
& $g explain --journal-id 3              # 这条记录做了什么、能不能撤、怎么撤
& $g rollback --pending                  # 预演：只撤销还没撤过的
& $g rollback --all --yes                # 真正执行逆序撤销
```

`journal` 实测输出：

```text
C:\Users\<you>\AppData\Local\Temp\gopt-demo\journal.jsonl
记录: 3 / 链: 完好 / 待撤销: 3
  hash chain verified: 3 of 3 records
  #1    apply     pid:1234                           policy:game/cs2/priority
        before {"name":"cs2.exe","pid":1234,"priority":"normal"}
        after  {"name":"cs2.exe","pid":1234,"priority":"high"}
  #2    apply     pid:1234                           policy:game/cs2/affinity
        before {"name":"cs2.exe","pid":1234,"plan":{"requests":[{"group":0,"mask":65535}],"total_logical":16}}
        after  {"name":"cs2.exe","pid":1234,"plan":{"requests":[{"group":0,"mask":65532}],"total_logical":16}}
  #3    apply     pid:1234                           policy:game/cs2/working-set
        before {"max_bytes":8796093022208,"min_bytes":204800,"name":"cs2.exe","pid":1234}
        after  {"max_bytes":8796093022208,"min_bytes":268435456,"name":"cs2.exe","pid":1234}
```

回滚是**事件溯源**：内核不记得"改过什么"，而是把日志里 `id <= to_id` 的 `apply`/`imported` 记录
按 id **降序**重新推导成撤销动作，再经 HAL 落地；每条撤销动作自己也会追加一条
`kind=rollback` + `rule_id="rollback:<apply_id>"` 的记录。

`rollback --all --yes` 实测输出（三条记录全部可撤，含工作集）：

```text
回滚 (to_id=3)
  rollback plan to #3: 3 steps (3 executable, 0 not actionable) over 3 records
  #3 [applied] pid:1234 — #3 pid:1234 → restore working set of pid 1234 to min 204800 bytes / max 8796093022208 bytes
     撤销记录 #4
  #2 [applied] pid:1234 — #2 pid:1234 → restore affinity of pid 1234 to 16 of 16 logical processors [g0=0x000000000000ffff]
     撤销记录 #5
  #1 [applied] pid:1234 — #1 pid:1234 → restore priority of pid 1234 (cs2.exe) to normal
     撤销记录 #6
汇总: 3 计划 / 3 可执行 / 3 已执行 / 0 失败
```

> **什么时候会出现 `not_actionable`？** 只有"写入前状态读不到"时：目标进程受保护/已退出导致读失败，
> 或系统报告工作集 `min = 0`（HAL 的写路径拒绝 `min = 0`，这个前值写不回去）。
> 这时内核**不记录假的 `before`**，而是让审计记录与回滚计划显式说"这一步撤不了"——
> 宁可少一条可回滚的记录，也不写一条执行时才发现还原失败的记录。

### 5.3 篡改会被检出，而且会 fail-closed

把日志里一个字节改掉（例如把 `"priority":"high"` 改成 `"priority":"HIGH"`）后：

```text
$ gopt verify-journal
审计链校验失败
文件存在: yes / 记录: 6 / 锚点: no
  hash chain broken after 0 of 6 records — #1 (line 1) [record_hash]: the stored hash does not match the record content — stored hash c0a0ef38…86a01 != recomputed hash 781e55f7…870d71
  第一处不一致: line 1 — record_hash (the stored hash does not match the record content)
（退出码 3）

$ gopt apply cs2 --yes          # 写操作同样被拒绝：系统零改动
错误 [audit_chain_broken]: the audit chain in ...\journal.jsonl does not verify, so gopt will not modify the system
  建议: 审计链校验失败：日志被篡改或损坏，gopt 拒绝基于它继续修改系统；…
（退出码 3）
```

**尾部整行被删除**这种情况，只靠文件自身检测不出来（剩下的前缀仍是一条自洽的链），
需要外部锚点 `--anchor-len/--anchor-hash`（锚点只覆盖当时的前缀，其后新增不算篡改）：

```text
$ gopt verify-journal --anchor-len 6 --anchor-hash d75b1514…74f9e    # 锚点对得上
审计链完好
文件存在: yes / 记录: 6 / 锚点: yes
  hash chain verified: 6 of 6 records (anchor matched)
（退出码 0）

$ gopt verify-journal --anchor-len 6 --anchor-hash 0000…0000         # 锚点对不上
审计链校验失败
  hash chain broken after 6 of 6 records — #6 (line 6) [anchor_hash]: the anchored record hash does not match
  第一处不一致: line 6 — anchor_hash (the anchored record hash does not match)
（退出码 3）
```

锚点的用法是"把 `(记录条数, 最后一条的 hash)` 记在**日志之外**"（例如备份清单、发布记录、
或另一台机器上的副本），这才是完整的防删改方案。
（`gopt journal` 不带参数时会打印待撤销/已撤销；`--kind apply --limit 20` 可过滤，实测 `[已撤销]` 会标在对应记录上。）

### 5.4 旧格式（只读）

`import-legacy` 只读解析 C++ v1.1.0 的 `savepoints.txt` / `games.conf`（不改写旧文件），
不加 `--yes` 只解析、加了 `--yes` 才以 `kind=imported` 入链：

```text
$ gopt import-legacy
旧格式导入（只读解析） — 预演，未写日志
  savepoints.txt: ...\gopt-demo\savepoints.txt: exists=false bytes=0 valid=0 bad=0 blank=0 ignored=0 / games.conf: ...: exists=false bytes=0 valid=0 bad=0 blank=0 ignored=0
解析条目: 0 / 已入链: 0 / 红线过滤: 0 / 入链 id: -
  - the file does not exist (never optimized with the C++ release?)
（退出码 0）
```

---

## 6. 真机往返自检 `gopt-verify`

只碰**自己的进程**与 `%TEMP%`，不需要管理员，跑完不留系统改动：

```powershell
.\target\release\gopt-verify.exe            # 中文
.\target\release\gopt-verify.exe --lang en  # 英文
.\target\release\gopt-verify.exe --json     # 结构化（含每项 before/after）
```

7 项：`hardware` / `priority_round_trip` / `affinity_round_trip` / `working_set_round_trip` /
`power_scheme_query` / `run_entries_read` / `journal_round_trip`（建链 → 校验 → 篡改检出 → 还原 → 锚点）。
最后一行固定 `RESULT: PASS`（退出码 0；有 FAIL 则 1）。本机实测：

```text
gopt-verify 0.1.0 (GameOptimizer-RS Phase 1) — 真机往返自检（只碰自己的进程与 %TEMP%）
pid 13436 / backend win32 / 提权: yes

 1. [PASS] hardware               (   9 ms) 硬件画像（只读）
      后: AMD Ryzen 9 7945HX with Radeon Graphics / NVIDIA GeForce RTX 4060 Laptop GPU (nvidia, 7956 MiB) = 1 / 15 groups
 7. [PASS] journal_round_trip     (   2 ms) 审计链建链 → 校验 → 篡改检出 → 还原
      后: appended 2 records | 2 records verified | rollback plan: ... 2 steps (2 executable, 0 not actionable) ... | tampering detected: #1 (line 1) [record_hash] ... | restored bytes verify again | anchor ok (len 2)

汇总: 7 通过 / 0 失败 / 0 跳过 / 13 ms
RESULT: PASS
```

---

## 7. 目录结构

```text
rust/
├─ Cargo.toml            # workspace：resolver=2 / edition 2021 / 显式 members / release lto + codegen-units=1
├─ Cargo.lock            # 依赖锁定（建议随仓库提交，保证可复现构建）
├─ .gitignore            # target/ 等
├─ policies/             # 内置策略（*.toml）+ 加游戏的说明；example-custom.toml 是模板兼示例
│  └─ README.md
└─ crates/
   ├─ gopt-hal/          # src/{lib,api,error,types,affinity,hardware,mock}.rs + src/win32/{mod,handle,process,power,startup,system}.rs
   │                     # examples/hal_selfcheck.rs + tests/hal_contract.rs
   ├─ gopt-policy/       # src/{lib,error,condition,matching,raw,validate,model,affinity,plan,eval,loader,builtin}.rs
   │                     # examples/policy_selfcheck.rs + tests/{builtin_policies,loader_override,invalid_toml,condition_matrix,plan_and_execution}.rs
   ├─ gopt-journal/      # src/{lib,chain,record,canonical,payload,rollback,legacy,store,error}.rs
   │                     # examples/journal_selfcheck.rs + tests/journal_contract.rs
   ├─ gopt-core/         # src/{lib,engine,apply,exec,inspect,model,report,paths,outcome,error,i18n}.rs + tests/mock_end_to_end.rs
   ├─ gopt-cli/          # src/{main,args,run}.rs + tests/cli_contract.rs
   └─ gopt-verify/       # src/main.rs（7 项真机往返自检；工作集经 SystemApi::get_working_set 读回）
```

`target/` 是 cargo 产物目录，已被 `rust/.gitignore` 与仓库根 `.gitignore` 忽略，**不入库**。

---

## 8. 常见问题

**Q：`error[E0463]: can't find crate for 'std'` 怎么办？**
PATH 里命中了 `~/.cargo/bin` 里的 cargo/rustc **副本**。按 §2.1 把 rustup 工具链 bin 前置即可。
（`cargo fmt` / `cargo clippy` 报 "no such command" 是同一个原因的另一面：子命令只在工具链 bin 目录里。）

**Q：`cargo clippy` 报"不是有效的 Win32 应用程序"或 "no such command: \`clippy\`"？**
`~/.cargo/bin` 里曾经有 9 个 0 字节占位文件（`cargo-fmt.exe` / `cargo-clippy.exe` / …），
而 cargo 会优先在 `$CARGO_HOME/bin` 找子命令；占位文件现已清理，但**子命令只存在于工具链 bin 目录**，
所以仍然要把工具链 bin 前置到 PATH（见 §2.1）。

**Q：没有游戏 / 没有管理员，怎么验证功能？**
`GOPT_BACKEND=mock` 跑 CLI（Mock 后端内置 `cs2.exe(1234)`、`steam.exe(5678)` 等假进程）；
`cargo test --workspace` 的 298 个用例全部走 Mock；真机往返用 `gopt-verify`。

**Q：`gopt plan <游戏>` 报"目标不存在"（退出码 2）？**
游戏没在运行。先启动游戏，或直接用 `gopt plan <游戏> --pid <pid>`；`GOPT_BACKEND=mock` 下不需要真实游戏。

**Q：日志被改坏了怎么办？**
`gopt verify-journal` 报第一处不一致（文件、行号、记录 id、原因）。此时**所有写命令都会拒绝执行**
（fail-closed，系统零改动）。用备份恢复该文件，或保留证据后手动移除（会失去对应的回滚能力）。

**Q：为什么 `apply` 不加 `--yes` 什么都没发生？**
这是默认安全：没有 `--yes` 一律只预演（读当前值、算差异、打印会改什么），不写系统、不建日志文件。
