# gopt-journal —— 哈希链审计日志 / 事件溯源回滚 / 旧格式兼容

GameOptimizer-RS 的审计与回滚基础设施。把"可回滚 / 可解释"从注释变成**可验证的工程属性**：

* 每一次系统修改都先写一条 **`before` 快照**记录，回滚不是内存里的临时变量，而是从日志**重新推导**出来的逆序计划；
* 每条记录与前一条用 **SHA-256 哈希链**绑定，篡改可检出，且报出**第一处**不一致的 id 与原因；
* C++ 版 v1.1.0 的 `savepoints.txt` / `games.conf` 能被**只读**导入进同一条链（历史不丢、旧文件不动）。

红线（本 crate 的边界）：**不做任何系统调用**（回滚计划是纯数据，执行由 `gopt-core` 经 `gopt-hal::SystemApi` 落地）、**不 panic**、**依赖面只有 4 个**（`gopt-hal` + `serde` + `serde_json` + `sha2`）、**无 async**、`forbid(unsafe_code)`。

---

## 1. 记录 schema（`journal.jsonl`，每行一个对象）

默认路径：`%LOCALAPPDATA%\GameOptimizer\journal.jsonl`（无 `LOCALAPPDATA` 时退化为当前目录下 `GameOptimizer\journal.jsonl`，与 C++ 版 `SecurityRollback::SaveFileDirW` 同策略）。

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `id` | u64 | 链上序号：从 1 起、严格 +1（删行/插行会被检出） |
| `ts_unix_ms` | i64 | UTC 毫秒；旧格式导入沿用原时间戳（允许负值） |
| `kind` | string | `apply` / `rollback` / `imported` |
| `target` | string | 作用对象：`pid:<pid>` / `power-scheme` / `run:<HIVE>:<name>` / `game:<index>` |
| `before` | object \| null | 写入前状态（回滚依据）；`null` = 未知 ⇒ 回滚时显式报"不可执行" |
| `after` | object \| null | 写入后状态（可解释性） |
| `rule_id` | string \| null | 触发修改的策略/规则标识 |
| `prev_hash` | string(64) | 上一条的 `hash`；链首为创世哈希（64 个 `0`） |
| `hash` | string(64) | `SHA256(规范化正文 ‖ prev_hash)` |

示例行（已格式化换行，实际是一整行）：

```json
{"id":1,"ts_unix_ms":1788242158696,"kind":"imported","target":"pid:38760",
 "before":{"pid":38760,"priority":"normal","affinity_mask":"0x000000000000ffff",
           "min_bytes":204800,"max_bytes":1413120,
           "guid":"52521609-efc9-4268-b9ba-67dea73f18b2",
           "legacy":{"source":"savepoints.txt","index":1,"raw":"38760|32|65535|…"}},
 "after":null,"rule_id":"legacy:savepoints.txt",
 "prev_hash":"0000…0000","hash":"a1b2…"}
```

严格性：`#[serde(deny_unknown_fields)]`，未知键直接判为格式异常；必需字段缺失同理。可选字段（`before`/`after`/`rule_id`）缺失时 serde 会补 `null`，但那样它就不再等于规范行、自哈希也对不上，链校验照样报出。

### `before` / `after` 载荷约定（`payload` 模块，编解码唯一实现）

| 域 | 载荷 |
| --- | --- |
| 优先级 | `{"pid":1234,"name":"cs2.exe"\|null,"priority":"high"}` |
| 亲和性 | `{"pid":1234,"name":…,"plan":{…AffinityPlan…}}`，也接受扁平写法 `{"pid":…,"mask":"0x…","total_logical":32,"group":0}` |
| 工作集 | `{"pid":1234,"name":…,"min_bytes":536870912,"max_bytes":2147483648}`（`max_bytes` 缺省 = 不设上限） |
| 电源方案 | `{"guid":"8c5e7fda-…","name":"High performance","is_high_performance":true}` |
| 启动项 | `RunEntry` 直接序列化（`hive`/`value_name`/`name`/`command`/`enabled`/`expandable`） |

解码器**宽容但受校验**：接受多种等价写法，绝不放宽安全边界 —— `"realtime"`、`0x100`、`256` 一律返回"被拒绝"的原因；亲和性计划会用 `AffinityPlan::from_requests` 重新校验一次（堵住 `Deserialize` 绕过构造函数塞进空掩码/越界组号的洞）。

---

## 2. 哈希规范化方案（`canonical` 模块）

```text
hash = SHA256( canonical(record 去掉 hash 字段) ‖ prev_hash )
```

`canonical` 是**自己实现**的规范 JSON 写入器，不用 `serde_json` 的序列化器：

| 值 | 规范写法 |
| --- | --- |
| 对象 | 键按 **UTF-8 字节序**升序，`{"k":v,"k2":v2}`，无空格 |
| 数组 | `[v1,v2]`，无空格 |
| 字符串 | 仅 `"` `\` 用短转义；控制字符 `< 0x20` 写成 `\u00xx`（小写）；`>= 0x20` 原样输出 UTF-8（中文原样保留） |
| 整数 | 十进制 |
| 浮点 | `serde_json` 的最短往返表示（同一 `f64` ⇒ 同一串文本） |
| 其它 | `null` / `true` / `false` |

字段序由 `JournalRecord::body_json()` / `to_canonical_line()` 写死：
`id, ts_unix_ms, kind, target, before, after, rule_id, prev_hash[, hash]`
——**不依赖 `serde` 的字段序，也不依赖任何 map 的迭代顺序**（键序在写完后再显式排一次）。

写盘的行 **= 规范形式 + 恰好一个 `\n`**；读取时重算规范形式做**逐字节**比对，因此"把行重新格式化、加空格、改键序"也会被检出。`prev_hash` 以定长 64 字节文本直接拼在规范正文之后，边界唯一、无歧义。

---

## 3. 校验：`verify_chain()` 报出第一处不一致

每条记录从左到右，检查顺序固定：

1. 行能否按 schema 解析（含"尾部半行且未修复"）；
2. `id` 是否等于期望序号 ⇒ `id_sequence`；
3. 链首 `prev_hash` 是否为创世哈希 ⇒ `genesis_prev_hash`；
4. `prev_hash` 是否等于上一条 `hash` ⇒ `prev_hash_link`；
5. 记录自哈希是否等于存储的 `hash` ⇒ `record_hash`；
6. 行字节是否等于规范形式 ⇒ `line_canonical`。

结果 `ChainReport { total, verified, first_inconsistency: Option<ChainBreak>, anchored }`，
`ChainBreak { index, line_no, id, problem, detail }`，`summary()` 给一行英文摘要（`detail` 里带期望值/实际值，便于 explain）。

| 攻击/损坏方式 | 报出的问题 |
| --- | --- |
| 改字段（不重算哈希） | `record_hash` |
| 改字段 + 重算自己的哈希 | `prev_hash_link`（下一条） |
| 改/删 `id`、删中间整行 | `id_sequence` |
| 注入未知字段 | `malformed_line` |
| 重新格式化（加空格、改键序） | `line_canonical` |
| 改链首 `prev_hash` | `genesis_prev_hash` |
| **删除尾部整行 / 截断文件** | 文件内自洽 ⇒ 需要外部锚点：`anchor_length` |
| 换掉锚点位置的前缀（自洽重写） | `anchor_hash` |

**能力边界（诚实说明）**：单靠文件自身，"尾部整行被删除"不可检出（剩下的前缀仍是一条合法链）。
`Journal::anchor()` 生成 `ChainAnchor { len, last_hash }`，`verify_chain_with_anchor()` 用它做校验；
锚点必须存到日志之外（配置、CI 产物、另一台机器）才有意义。锚点只覆盖前缀：锚点之后**新增**的记录不算篡改。

---

## 4. 崩溃安全

* **提交路径**：一批记录拼成一个缓冲区，**一次 `write_all`**，然后 `flush` + `sync_data`（Windows 上是 `FlushFileBuffers`）。返回即落盘。
* **崩溃残留**：进程在 `write_all` 中途被杀会在文件尾部留下半行。**打开时**把"最后一个换行符之后的残余字节"截掉（`JournalOptions::repair_torn_tail`，默认开启），`Journal::tail_repair()` 报出丢了几个字节。
  * 只要不以 `\n` 结尾就一律丢弃——哪怕它看起来是一条完整 JSON（否则下一条会粘在同一行）。
  * 被丢弃的那条**从未提交**：下一个 id 会复用它，链继续。
* **只读诊断**：`JournalOptions::read_only()` 不截断、不 fsync；尾部残行保留成 `terminated = false` 的行，校验明确报 `malformed_line`，并且**拒绝追加**。
* **不把新记录接在坏链上**：追加前用打开时的校验结果把关，坏链 ⇒ `chain_broken`。
* **外部改写检测**：追加前比对文件长度，发现被别的进程改过 ⇒ `stale`（要求重新打开）。
* **原子替换**：`rewrite_atomic()` / `rewrite_atomic_with(&records)` 走"临时文件 → `sync_all` → `rename`"，用于日志压缩/截断；传入的记录必须先自证是一条合法链，否则拒绝写入（不允许静默丢记录），临时文件会被清理。
* 空行跳过（计数上报），不占行号、不破坏链。

---

## 5. 回滚计划（`rollback` 模块）

`Journal::plan_rollback(to_id)` / `plan_rollback_all()`：覆盖 `id <= to_id` 且 `kind ∈ {apply, imported}` 的记录，按 id **降序**生成 `RollbackStep`：

```rust
RollbackAction::RestorePriority { pid, name, priority }
RollbackAction::RestoreAffinity { pid, name, plan }
RollbackAction::RestoreWorkingSet { pid, name, limits }
RollbackAction::RestorePowerScheme { guid, name }
RollbackAction::RestoreRunEntry { hive, value_name, enabled }
RollbackAction::RestoreLegacySnapshot { process_id, priority, affinity_mask, working_set, power_scheme_guid }
RollbackAction::NotActionable { reason }        // before 缺失 / 形状无法识别 / 被红线拒绝
```

* **纯数据、无系统调用**：`gopt-core` 负责把每一步翻成 HAL 调用。
* **不静默跳过**：无法解读的步骤保留为 `NotActionable` + 原因；`executable_steps()` 拿可执行子集，`not_actionable()` 计数需要人工确认的步骤。
* 链不可信时 `plan_rollback` 直接返回 `chain_broken`（不基于被篡改的日志回滚）。
* 约定：`kind = rollback` 的记录把 `rule_id` 写成 `rollback:<apply_id>`；`undone_apply_ids()` + `plan_rollback_pending()` 用来区分"已撤销过的 apply"。`kind = rollback` 的记录本身**不进计划**。
* `RollbackPlan` / `RollbackStep` / `RollbackAction` / `ChainReport` / `JournalRecord` 全部 `serde` 可往返，直接支撑 CLI `--json`。

---

## 6. 旧格式兼容（只读，`legacy` 模块）

### 格式 A：`savepoints.txt`（对齐 `SecurityRollback::Serialize` / `TryDeserialize`）

```text
processId|priorityClass|affinityMask|workingSetMin|workingSetMax|hasProcessState|hasWorkingSet|hasPowerScheme|powerSchemeGuid|timestampMs|gameName|description
```

逐条对齐 C++ 的行为：

| 规则 | 对齐点 |
| --- | --- |
| 数字字段 | 十进制优先，**失败退回十六进制**（兼容 `0x` 前缀）；必须整串消费；`processId`/`priorityClass` ≤ `u32::MAX`，掩码/工作集 ≤ `u64::MAX` |
| 负数 | 前导 `-` 一律判负（C++ 显式拒绝 `stoul` 回绕） |
| 超长数字串 | 判负（C++ 用 `maxValue` 拦住 `stoull` 回绕），不 panic |
| 前导空白 | 接受（`strtoull` 会跳过；Rust 的 `parse` 不会，这里显式对齐） |
| `has*` 字段 | 只有恰好 `"1"` 才是 `true` |
| `timestampMs` | 唯一允许负值的字段，且只用十进制（C++ 用 `stoll`，不回退十六进制） |
| 第 13+ 字段 | 忽略（C++ 只读 12 次 `getline`）；**整行原文**存进 `legacy.raw`，信息不丢 |
| CRLF / 空行 | 剥 `\r`；空行跳过（计数） |
| 坏行 | 跳过并计数（`bad_lines`），前 3 条原因进 `notes`；C++ 的 UI 路径选择"整文件判损坏"，我们按任务要求逐行跳过 |

导入成 `kind = imported` 的草稿：`target = pid:<processId>`、`ts = timestampMs`、`rule_id = legacy:savepoints.txt`、
`before = {pid, priority?, affinity_mask?, min_bytes/max_bytes?, guid?, legacy:{source,index,raw,game_name,description,has_*,timestamp_ms}}`。

**红线落地**：旧文件里的 `REALTIME_PRIORITY_CLASS (0x100)` 不会进 `priority` 字段，只在
`priority_raw_blocked` 里留痕并在报告里计数（`policy_filtered()`），回滚计划因此永远拿不到 REALTIME。

### 格式 B：`games.conf`（对齐 `GameConfig.cpp` 的 `Store()` / `Persist()`）

```text
<gameIndex>|<exePath>|<args>|<oI:power:frameLatency:workingSet>
```

* `gameIndex` 用 `std::stoi` 的宽松语义（前导空白、正负号、尾部残留字符）；**C++ 在 `stoi` 抛异常时异常会穿透未捕获的 `Store()`** —— Rust 版把这种行记为坏行跳过（不 panic）。
* 第 4 字段不足 4 字符 ⇒ 保持 `GameLaunchConfig` 默认值（`true,false,false,true`，不是全 `false`）。
* `gameIndex < 0` ⇒ 记 `ignored_lines`（C++ 静默忽略）。
* 重复索引 ⇒ 两条都保留进审计链（C++ 后者覆盖前者），并记一条 note。

导入成 `kind = imported`、`target = game:<index>`、`ts = games.conf 的 mtime`、`rule_id = legacy:games.conf`；
这些条目描述的是**启动偏好而不是实时系统状态**，回滚计划里显式报 `NotActionable`（原因："launch preferences … nothing to restore"）。

### 只读保证

`import_legacy()` / `import_legacy_default()` 只做 `fs::read` / `fs::metadata`：不创建目录、不写文件、不改 mtime；
文件不存在/不可读/传入目录/含二进制垃圾都降级成 `SourceReport.notes`（不返回 `Err`、不 panic）。
集成测试逐字节比对文件内容 + mtime + 目录清单来证明这一点。

---

## 7. 文件清单（新增，未改动任何 C++ 源码）

| 文件 | 行数 | 内容 |
| --- | --- | --- |
| `Cargo.toml` | 20 | 依赖：`gopt-hal` + `serde` + `serde_json` + `sha2`（无 dev-dependencies） |
| `src/lib.rs` | 132 | crate 文档（schema / 哈希方案 / 边界）+ lint 面 + 公开导出 |
| `src/canonical.rs` | 221 | 规范 JSON 写入器 + SHA-256 + 链式哈希（含 62 行单测） |
| `src/record.rs` | 364 | `JournalRecord` / `JournalKind` / `JournalDraft`（字段序写死） |
| `src/chain.rs` | 607 | `verify_lines` / `ChainReport` / `ChainBreak` / `ChainAnchor` |
| `src/store.rs` | 508 | `Journal`：追加 + fsync、尾部半行修复、`stale` 检测、原子替换、默认路径 |
| `src/rollback.rs` | 765 | 逆序回滚计划 + 类型化动作 + 形状派发解码 |
| `src/payload.rs` | 415 | `before`/`after` 载荷 schema 的编解码（唯一实现） |
| `src/legacy.rs` | 896 | C++ `savepoints.txt` / `games.conf` 只读解析与导入报告 |
| `src/error.rs` | 288 | `JournalError`（分类 + 路径 + 行号 + Win32 码）+ 转 `HalError` |
| `examples/journal_selfcheck.rs` | 243 | 真机自检：写 → 校验 → 篡改检出 → 半行修复 → 回滚计划 → 旧格式导入 |
| `tests/journal_contract.rs` | 1072 | 6 类端到端场景 + 2 组额外守卫（真实文件系统） |
| `README.md` | 266 | 本文件（schema / 哈希方案 / 对齐表 / 验收命令） |

合计 **5797 行**（`src/` 4196 + `examples/` 243 + `tests/` 1072 + `Cargo.toml` 20 + 本文档 266；
统计口径：`ReadAllLines(UTF-8).Count`，含空行、注释与内联单测）。

---

## 8. 构建 / 测试 / 自检

```powershell
$tc = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"; $env:PATH = "$tc;$env:PATH"
cd rust

cargo build -p gopt-journal                                   # 构建
cargo test  -p gopt-journal                                   # 55 个测试（46 单测 + 8 集成 + 1 doctest）
cargo clippy -p gopt-journal --all-targets -- -D warnings      # 无告警

# 真机自检（默认写 %TEMP%，旧格式导入部分是只读的）
cargo run -p gopt-journal --example journal_selfcheck
```

6 类场景（`tests/journal_contract.rs`）：

1. `scenario_1_clean_chain_appends_and_verifies` —— 正常链：追加 → 校验 → 重开一致 → 跨进程续写 → 锚点；
2. `scenario_2_tamper_detection_reports_first_inconsistency` —— 篡改检出 9 个子例（见 §3 对照表）；
3. `scenario_3_torn_tail_is_truncated_and_recovered` —— 尾部半行修复 + 只读模式拒绝 + 空行 + 原子替换 + `stale`；
4. `scenario_4_rollback_plan_is_reverse_and_typed` —— 逆序、类型化解码、已撤销过滤、坏链拒绝、参数非法；
5. `scenario_5_legacy_import_parses_and_skips_bad_lines` —— 旧格式解析（坏行跳过、红线过滤、入链、计划）；
6. `scenario_6_legacy_import_is_read_only_and_never_panics` —— 只读性（字节/mtime/目录清单）、幂等、二进制垃圾、目录路径、空文件。

真机实测（Windows 10/11 x64，机器上有 C++ 版 v1.1.0 的真实数据目录）：

```text
savepoints: %LOCALAPPDATA%\GameOptimizer\savepoints.txt: exists=true bytes=428 valid=3 bad=0
games.conf: %LOCALAPPDATA%\GameOptimizer\games.conf: exists=true bytes=113 valid=2 bad=0
→ 3 条快照（pid 38760/39708/28284，priorityClass 32 ⇒ normal，掩码 0xffff / 0xffffffff，
   工作集 204800..1413120，电源方案 GUID 原样保留，中文游戏名"三角洲行动"无损）
→ 2 条每游戏配置（含真实 cs2.exe 路径与 -novid 参数）
→ 导入全程只读：目录清单与文件 mtime 不变，未创建 journal.jsonl
```

---

## 9. 给后续任务的接口约定

* **写日志**（`gopt-core` / `gopt-cli`）：`Journal::open_default()` → `append(JournalDraft::now(kind, target).with_before(...).with_after(...).with_rule_id(...))`；
  批量导入用 `append_legacy_import(&import)`。
* **回滚执行**：`journal.plan_rollback_all()` → 遍历 `plan.executable_steps()`，把每个 `RollbackAction` 翻成 HAL 调用，
  每执行完一步追加一条 `kind = rollback` + `rule_id = "rollback:<apply_id>"` 的记录。
* **可解释输出**：`ChainReport::summary()` / `ChainBreak::{problem, detail}` / `RollbackStep::describe()` / `LegacyImport::notes()` 都是稳定英文；
  中文由 CLI 按 `ChainProblem` / `JournalErrorKind` 本地化。
* **审计锚点**：把 `journal.anchor()` 存到日志之外（配置或 CI 产物），下次启动 `verify_chain_with_anchor()`，才能检出"尾部被删"。
