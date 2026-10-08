# gopt-policy —— GameOptimizer-RS 声明式策略引擎

`gopt-policy` 把"给哪款游戏做什么优化"从 C++ 代码搬进 **TOML 数据**：加游戏、改规则、
按硬件分级降级都不需要重新编译，解析错误带 `文件:行:列`，求值产物是一份带中英双语理由、
按顺序排列、可回滚的 **Plan**。

本 crate **`forbid(unsafe_code)`**，不直接触碰系统：策略只描述"要做什么"，
执行永远经 `gopt-hal` 的 `SystemApi`（真实 Win32 / Mock 双后端）。

---

## 1. 快速开始

```powershell
# 关键：本机 %USERPROFILE%\.cargo\bin 里是 rustc/cargo 的二进制副本，直接用它会导致
#      sysroot 落回 ~\.cargo（缺少 lib\rustlib → E0463）。必须把 rustup 工具链 bin 放最前。
$tc = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"
$env:PATH = "$tc;$env:PATH"

cd rust
cargo build -p gopt-policy
cargo test  -p gopt-policy                 # 95 个测试（单元 + 集成 + 文档测试）
cargo clippy -p gopt-policy --all-targets -- -D warnings
```

依赖面只有三个：`gopt-hal`（唯一系统入口）、`serde`（序列化）、`toml`（声明式策略解析，
用它的 `Spanned` 拿行号）。没有 async、没有正则引擎（通配匹配自己实现，线性时间）。

---

## 2. TOML schema

```toml
schema = 1                                  # 可选；本构建只认 1，其它版本直接报错

[[game]]
id = "cs2"                                  # kebab-case、文件内唯一；用户层同 id ⇒ 覆盖内置
name_zh = "CS2"                             # 中英双语名都是必填（双语红线）
name_en = "Counter-Strike 2"
match = "cs2.exe"                           # 必填：exe 名通配（* 与 ?，大小写不敏感，不接受路径）
exe_aliases = ["csgo.exe"]                  # 可选：同一款游戏的其它可执行文件
name_aliases = ["反恐精英2"]                 # 可选：搜索/展示别名（"瓦罗兰特"就挂在这里）
description_zh = "..."                      # 可选
description_en = "..."

[[game.rules]]                              # 规则按声明顺序求值
id = "working-set-halved"                   # 可选；缺省按 `游戏id.动作` 推导（同种动作第 k 条加 -k）
when = { ram_gb = { gte = 8, lt = 16 } }     # 可选；缺省/空表 = 无条件
action = { working_set = { min_mb = 128 } }  # 必填；恰好一种动作
```

### 动作（六选一）

| 动作 | 写法 | 落到 HAL | 需管理员 | 危险动作 |
| --- | --- | --- | :--: | :--: |
| `priority` | `{ class = "high" }`（`idle`/`below-normal`/`normal`/`above-normal`/`high`） | `set_priority` | — | — |
| `affinity` | `{ reserve_cores = 1, reserve_from = "first", physical_only = true }` 或 `{ mask = "0xffff" }` | `set_affinity` | — | — |
| `working_set` | `{ min_mb = 256, max_mb = 0 }`（`max_mb` 缺省/0 = 无上限） | `set_working_set` | — | — |
| `power_scheme` | `{ scheme = "high" }` / `{ scheme = "balanced" }` | `set_power_scheme` | ✔ | ✔ |
| `run_entries` | `{ hive = "hkcu"\|"hklm"\|"both", disable = ["Discord"], ignore_missing = true }` | `set_run_entry_enabled` | HKLM 需要 | ✔ |
| `skip` | `{ reason_zh = "...", reason_en = "..." }` | 不执行（进 `Plan::skipped` 解释为什么） | — | — |

* `priority` 的上限由 HAL 类型保证：`PriorityClass` 里没有 REALTIME，
  写 `class = "realtime"`（或 `0x100`）会在**加载期**被拒绝并报出行号。
* `affinity` 的 `reserve_cores` 与 `mask` 互斥；`reserve_from` 缺省 `"first"`
  （保留全局序号最小的 N 个核，与 C++ 版 `leaveCoresForSystem` 逐位等价），
  `"last"` 则保留序号最大的 N 个核。`physical_only = true` 时按物理核计数，
  并把每个物理核的 SMT 兄弟核一起选中。
* `run_entries` 只提供 `disable`（这是"优化"语义）；启用/还原由 CLI 的回滚路径负责。

### 条件（`when`）

| 字段 | 类型 | 允许的算子 |
| --- | --- | --- |
| `logical_cores` / `physical_cores` / `ram_gb` / `ram_mb` | 整数 | `gt` `gte` `lt` `lte` `eq` `ne`，并可写成标量简写（`ram_gb = 16` ⇒ `eq`） |
| `gpu_vendor` | 文本 | `eq` / `ne`；取值 `nvidia` `amd` `intel` `microsoft` `unknown` `none` |
| `is_elevated` | 布尔 | `eq` / `ne` |

* 同一字段可以同时给上下界（`{ gte = 8, lt = 16 }`）——C++ 的"8~16GB 工作集减半"
  就是这一条；
* 多个字段之间是 **AND**；不写 `when` 或写 `when = {}` 表示无条件；
* 语义校验在加载期完成并带行号：类型不匹配、`gt`+`gte` 同时给、区间矛盾（`gte = 16, lt = 8`）、
  `eq`+`ne` 互相矛盾、`gpu_vendor` 不支持大于小于……全部拒绝；
* `none`（没有探测到适配器）与 `unknown`（适配器存在但认不出厂商）刻意区分开。

### 严格校验清单

文件级：`schema` 必须是 1；至少一个 `[[game]]`；未知键一律报错。
游戏级：`id` kebab-case 且文件内唯一；`name_zh` / `name_en` 非空；`match` 必须是 exe 名
（不允许路径、不允许裸 `*`）；`exe_aliases` 不重复且不与主模式重复。
规则级：`id` 非空且游戏内唯一；`action` 恰好一种。
动作级：`working_set.min_mb > 0` 且 `max_mb >= min_mb`；`affinity` 至少一种绑定意图
（`reserve_cores` / `mask` / `physical_only = true`）；掩码非 0 且必须能解析；
`run_entries.disable` 非空且无重复；`skip` 必须中英双语都有。

---

## 3. Plan 结构

```text
Plan {
  game_id, game_name_zh, game_name_en, pid, policy_origin,
  steps:   [ PlanStep { order, rule_id, rule_line, pid, reason { zh, en },
                        action, requires_elevation, is_dangerous } ],
  skipped: [ PlanSkip { rule_id, rule_line,
                        cause: condition_not_met | degraded | explicit_skip,
                        reason { zh, en } } ]
}

PlanAction = Priority { class }
           | Affinity { plan: AffinityPlan, spec: AffinitySpec }
           | WorkingSet { limits }
           | PowerScheme { scheme, selector }
           | RunEntry { hive, name, enabled, ignore_missing }
```

不变量（有测试守着）：

1. `order` 从 1 连续递增，等于在 `steps` 中的下标 + 1；
2. `steps` 顺序 = 规则声明顺序；`run_entries` 规则按 (hive, name) 展开（HKCU 在前）；
3. 每个 `action` 都能经 `PlanStep::hal_op()` 一对一映射到 HAL 操作
   （`SetPriority` / `SetAffinity` / `SetWorkingSet` / `SetPowerScheme` / `SetRunEntryEnabled`）；
4. **求值不失败**：条件不满足 → `condition_not_met`；算不出来（掩码超出组容量、
   保留核数超过核数）→ `degraded`；策略里写 `skip` → `explicit_skip`。三种都带中英双语理由，
   没有"静默跳过"；
5. 跨处理器组（逻辑核 > 64）时亲和性计划会是"每组一段掩码"，
   `PlanStep::affinity_batches()` 给出逐组批次（非主组走逐线程 `SetThreadGroupAffinity`）。

---

## 4. 加载与用户覆盖

```text
内置层：rust/policies/*.toml                     （include_str! 进二进制，单 exe 分发也能用）
用户层：%LOCALAPPDATA%\GameOptimizer\policies.d\*.toml
```

* 用户层**优先**：同 `id` 覆盖内置（并给出一条 warning，让用户知道自己盖掉了什么），
  新 `id` 直接新增；
* 查找与 exe 匹配都是"用户层在前"，列表顺序 = 用户层（文件名字典序 + 文件内声明序）+ 内置层；
* 一个文件坏掉只影响它自己：错误进 `PolicyLoadOutcome::diagnostics`（带 `文件:行:列`），
  其余策略照常可用；`into_result()` 给 CLI 用的严格模式；
* 目录不存在不是错误（全新安装就是这样）；非 UTF-8 / 读不了的文件变成诊断而不是 panic；
* 两个游戏抢同一个 exe 模式时给出 warning（先匹配到的赢），不静默隐藏。

---

## 5. 与 C++ 版 v1.1.0 的关系

内置的 8 款游戏与 `src/preset/GameOptimizationPreset.cpp` 的 `GameId` 一一对应，
`priority` / `affinity` / `working_set` 语义逐字段对齐（含降级路径，只是它们现在是 `when` 条件）：

| 内置 id | exe | 优先级 | 亲和性 | 工作集下限 |
| --- | --- | --- | --- | --- |
| `delta-force` | `DeltaForceClient-Win64-Shipping.exe` | high | 全逻辑核，保留 1 | — |
| `league-of-legends` | `League of Legends.exe` | above-normal | 全逻辑核，保留 1 | — |
| `cs2` | `cs2.exe`（`csgo.exe`） | high | 仅物理核，保留 1 | 256 / 128（<16GB）/ —（<8GB） |
| `pubg` | `TslGame.exe` | high | 仅物理核，保留 1 | 512 / 256 / — |
| `valorant` | `VALORANT-Win64-Shipping.exe` | high | 全逻辑核，保留 1 | 256 / 128 / — |
| `apex-legends` | `r5apex.exe` | above-normal | 全逻辑核，保留 1 | — |
| `dota-2` | `dota2.exe` | above-normal | 全逻辑核，保留 1 | — |
| `overwatch-2` | `Overwatch.exe` | high | 全逻辑核，保留 1 | 256 / 128 / — |

物理核 ≤ 2 的机器上所有亲和性规则都不触发（与 C++ 一致），并且留下可解释的跳过说明。

`rust/policies/example-custom.toml` 额外以**纯数据**新增了两款游戏
（`naraka-bladepoint` 永劫无间、`genshin-impact` 原神），并演示了覆盖内置、显式掩码、
`reserve_from = "last"`、电源方案与启动项动作——这就是"加游戏不重编译"的证据。

两处**有意**的偏差：

1. 驱动级帧延迟（C++ 的 `gpuMaxFrames`）不实现：它需要 NVAPI/ADL 等第三方厂商库，
   违反"仅官方 Win32 API"红线；
2. 逻辑核 > 64 时不再放弃亲和性（C++ 在 `ComputeAffinityMask` 里直接返回 0），
   而是给出逐组计划，由 HAL 的 `AffinityPlan::per_group` + 逐线程组亲和性执行。

---

## 6. 文件清单（行数）

| 文件 | 行数 | 说明 |
| --- | ---: | --- |
| `rust/Cargo.toml` | +2 | workspace：登记 `crates/gopt-policy`，新增 `toml = "0.8"` workspace 依赖 |
| `rust/Cargo.lock` | — | 由 cargo 生成（新增 `toml`/`toml_edit`/`winnow`/`indexmap` 等传递依赖的锁定；该文件同时被兄弟 crate 共享） |
| `crates/gopt-policy/Cargo.toml` | 18 | 依赖面：`gopt-hal` + `serde` + `toml` |
| `src/lib.rs` | 154 | crate 文档（schema/Plan/覆盖规则全文）、lint 纪律（`forbid(unsafe_code)`、`deny(missing_docs)`、`deny(clippy::unwrap_used/panic/...)`）、导出 |
| `src/error.rs` | 324 | `PolicyOrigin` / `PolicyLayer` / `PolicyDiagnostic`（文件:行:列）/ `PolicyLoadErrors` |
| `src/condition.rs` | 1124 | 条件字段、算子、区间校验、求值、中英双语 describe |
| `src/matching.rs` | 133 | exe 名通配匹配（线性时间）+ 模式校验 |
| `src/raw.rs` | 408 | TOML 字面量镜像（`toml::Spanned` 保留行号）、标量/算子表两种写法 |
| `src/validate.rs` | 751 | 严格校验（唯一下诊断的地方）、`parse_policy_file` |
| `src/model.rs` | 911 | 受约束模型：`GamePolicy` / `Rule` / `Action` / `AffinitySpec` / `PowerSchemeChoice` / `RunHiveSpec` |
| `src/affinity.rs` | 374 | 策略层亲和性解析（first/last、physical_only、显式掩码、跨组、降级理由） |
| `src/plan.rs` | 404 | `Plan` / `PlanStep` / `PlanSkip` / `PlanAction` / `Reason` / `SkipCause` |
| `src/eval.rs` | 641 | `EvalInput`（硬件画像 + 提权）+ 求值 + 中英双语理由生成 |
| `src/loader.rs` | 383 | `PolicyLoader` / `PolicyLoadOutcome` / `PolicySet`（分层合并、查找、按进程生成计划） |
| `src/builtin.rs` | 79 | 内置文件清单（`include_str!`）与 C++ 兼容文件表 |
| `tests/builtin_policies.rs` | 362 | 内置清单与磁盘一一对应、C++ 语义对齐、示例文件 |
| `tests/loader_override.rs` | 280 | 用户覆盖内置、新增游戏、坏文件隔离、默认用户目录 |
| `tests/invalid_toml.rs` | 388 | 语法/语义错误的行号矩阵、恶意输入不 panic、非 UTF-8 文件 |
| `tests/condition_matrix.rs` | 309 | 算子矩阵、内存/厂商/提权边界、多组亲和性 |
| `tests/plan_and_execution.rs` | 391 | Plan 不变量、用 MockApi 逐步执行计划、回滚依据、跨组逐批执行 |
| `tests/common/mod.rs` | 110 | 临时目录与硬件画像工具（不引入 tempfile） |
| `examples/policy_selfcheck.rs` | 213 | 真机只读自检：加载策略 → 真机画像 → 进程匹配 → 打印计划（不需要管理员） |
| `README.md` | 233 | 本文件 |
| `rust/policies/*.toml` | 471 | 8 款 C++ 兼容策略 + `example-custom.toml`（共 10 款游戏定义） |
| `rust/policies/README.md` | 64 | 加游戏 / 覆盖内置策略的操作指南 |
| **合计** | **8525** | 新增（不含 `rust/target`，该目录已被 `rust/.gitignore` 忽略；含 `rust/policies/README.md` 与本文档） |

> 统计口径：对每个文件取 `ReadAllLines(UTF-8).Count`（含文档、注释与内联单测的空行），
> 与 t7 复核时一致；`rust/Cargo.lock`（跨 crate 共享）与 `rust/target` 不计入。

### 真机自检输出（本机 Ryzen 9 7945HX / RTX 4060 Laptop / 32 逻辑核 / 16GB）

```text
内置策略：9 个文件；用户覆盖目录：C:\Users\<you>\AppData\Local\GameOptimizer\policies.d
已加载 10 款游戏策略（错误 0 / 警告 0）
硬件画像：AMD Ryzen 9 7945HX with Radeon Graphics / 15 物理核 / 32 逻辑核（SMT） / 16064 MiB 内存 / GPU NVIDIA GeForce RTX 4060 Laptop GPU (7956 MiB)
求值输入：32 逻辑核 / 15 物理核 / 15 GiB 内存 / 显卡厂商 Some(Nvidia) / 已提权 = true
```

注意 `16064 MiB → 15 GiB` 落在"8~16GB 减半"档：CS2 在这台机器上会拿到工作集下限 128MB
（而不是 256MB），与 C++ 版 `systemRamMB < 16384` 的判定完全一致。

---

## 7. 已知边界

* Plan 是"意图 + 解析后的参数"，不是执行结果：真机上是否成功由 `gopt-hal` 报告，
  由 `gopt-core` 决定重试/回滚/记录审计链；
* 策略层不做 I/O 判断（例如"启动项是否存在"）：`run_entries` 带 `ignore_missing`，
  执行方按 HAL 返回的 `NotFound` 决定是软跳过还是报错；
* 同一款游戏的多个实例（多开）会各自得到一份 Plan，`PolicySet::plans_for` 保持进程顺序。
