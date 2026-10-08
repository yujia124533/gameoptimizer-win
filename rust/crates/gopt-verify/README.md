# gopt-verify —— 真机往返自检

一句话：**在不改动机器长期状态的前提下，逐项证明"写进去 → 读回来 → 还原"这条链在真机上是通的。**

```powershell
gopt-verify                    # 中文
gopt-verify --lang en          # 英文
gopt-verify --json             # 结构化报告（含每项 before/after）
gopt-verify --keep-temp        # 保留审计链往返用的临时目录
```

只碰**自己的进程**（`GetCurrentProcess` / `std::process::id()`）与 `%TEMP%` 下的临时目录：
不需要管理员，跑完不留任何系统改动（优先级、亲和性、工作集全部还原成原值）。

## 1. 检查项（最后一行固定为 `RESULT: PASS` / `RESULT: FAIL`）

| # | id | 内容 | 读回方式 |
| --- | --- | --- | --- |
| 1 | `hardware` | CPU / 内存 / GPU / 处理器组（策略条件求值的输入） | 只读 |
| 2 | `priority_round_trip` | 自身进程 HIGH → 读回 → 还原 | `SystemApi::get_priority` |
| 3 | `affinity_round_trip` | 自身进程改掩码（严格子集）→ 读回 → 还原 | `SystemApi::get_affinity` |
| 4 | `working_set_round_trip` | 自身进程设 64/128 MiB → 读回 → 还原原值 → 再读回（**逐字节精确比对**） | `SystemApi::get_working_set` |
| 5 | `power_scheme_query` | 当前电源方案（连查两次，确认查询不改状态） | 只读 |
| 6 | `run_entries_read` | HKCU/HKLM 启动项（含"同根显示名唯一"校验） | 只读 |
| 7 | `journal_round_trip` | `%TEMP%` 里建链 → 校验 → 回滚计划 → **篡改检出** → 还原字节 → 锚点 | `gopt-journal` |

每项都打印**前后值**与（失败时）原因，例如：

```text
 2. [PASS] priority_round_trip    (   0 ms) 优先级 HIGH → 读回 → 还原
      前: normal
      后: raise=Ok("high") read_back=Ok("high") restore=Ok("high") read_back=Ok("normal")
```

## 2. 为什么这里没有 `unsafe`、也没有 `windows` 依赖

工作集读回过去只能靠 `src/probe.rs` 里的只读探针，因为 `gopt-hal::SystemApi` **只有写没有读**。
现在 `SystemApi` 有第 14 个方法 `get_working_set`（官方 `GetProcessWorkingSetSize`），于是：

* 读回完全走 HAL：`api.get_working_set(pid)`，与写操作同一条契约、同一套错误分类；
* `src/probe.rs` 与 `[target.'cfg(windows)'.dependencies] windows` 一起删除；
* 本 crate 现在是 `#![forbid(unsafe_code)]` —— "零 unsafe"不是承诺，是编译器保证；
* 比较用**精确相等**（不是 1 MiB 容忍区间）：写入 64/128 MiB 后系统就报这两个数，
  还原原值后必须逐字节回到原值，任何"差不多"都说明读路径在骗人；
* 系统报告 `min = 0` 时（HAL 写路径不接受 `min = 0`，无法精确还原）该项明确报 `SKIP` 并说明，
  **不假装通过**。

## 3. 退出码

| 码 | 含义 |
| --- | --- |
| `0` | 全部 PASS（`SKIP` 不影响退出码，但会在报告里显示 `skipped` 计数） |
| `1` | 有 FAIL（真实往返失败，必须当问题处理） |
| `2` | 参数错误 / 用法错误（`--help` 走 0） |

## 4. 本机实测（Ryzen 9 7945HX / RTX 4060 Laptop / 32 逻辑核 / 1 处理器组）

```text
gopt-verify 0.1.0 (GameOptimizer-RS Phase 1) — 真机往返自检（只碰自己的进程与 %TEMP%）
pid 31304 / backend win32 / 提权: yes

 1. [PASS] hardware               (   9 ms) 硬件画像（只读）
      前: 15C/32T, 16064 MiB RAM
      后: AMD Ryzen 9 7945HX with Radeon Graphics / NVIDIA GeForce RTX 4060 Laptop GPU (nvidia, 7956 MiB) = 1 / 15 groups
 2. [PASS] priority_round_trip    (   0 ms) 优先级 HIGH → 读回 → 还原
      前: normal
      后: raise=Ok("high") read_back=Ok("high") restore=Ok("high") read_back=Ok("normal")
 3. [PASS] affinity_round_trip    (   0 ms) 亲和性掩码 → 读回 → 还原
      前: group 0 mask 0x00000000ffffffff (32 of 32 logical)
      后: target 0x000000007fffffff -> read back 0x000000007fffffff -> restored 0x00000000ffffffff
 4. [PASS] working_set_round_trip (   0 ms) 工作集设上下限 → 读回 → 还原
      前: min 204800 / max 1413120 bytes
      后: set 64 MiB/128 MiB -> read back 67108864 / 134217728 bytes -> restored 204800 / 1413120 bytes
      说明: 读回用 HAL 的 SystemApi::get_working_set（官方 GetProcessWorkingSetSize），逐字节精确比对: 204800 / 1413120 bytes
 5. [PASS] power_scheme_query     (   0 ms) 电源方案查询（只读）
 6. [PASS] run_entries_read       (   0 ms) 开机启动项读取（只读）
      后: 9 entries (9 enabled)
 7. [PASS] journal_round_trip     (   3 ms) 审计链建链 → 校验 → 篡改检出 → 还原
      后: appended 2 records | 2 records verified | rollback plan: ... 2 steps (2 executable, 0 not actionable) |
          tampering detected: #1 (line 1) [record_hash] | restored bytes verify again | anchor ok (len 2)

汇总: 7 通过 / 0 失败 / 0 跳过 / 15 ms
RESULT: PASS
```

## 5. 文件清单

| 文件 | 内容 |
| --- | --- |
| `src/main.rs` | 7 个检查项 + 报告渲染（文本/JSON）+ 退出码；`Check` 结构统一 before/after/detail |
| `Cargo.toml` | 依赖只有 `gopt-hal` / `gopt-journal` / `serde_json`（无 `windows`、无 `unsafe`） |
