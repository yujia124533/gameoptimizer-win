# gopt-hal —— GameOptimizer-RS 硬件抽象层（HAL）

`gopt-hal` 是 GameOptimizer-RS（Rust 重写 Phase 1）唯一被允许触碰系统的入口：一个
`SystemApi` trait + 两个实现（真实 Win32 后端 `Win32Api` / 测试后端 `MockApi`），
所有失败都走结构化错误，所有写入都返回回滚所需信息。

本 crate **不改动任何 C++ 源码**，也不依赖 C++ 版产物，可与 v1.1.0 并存。

---

## 1. 快速开始

```powershell
# 关键：本机 %USERPROFILE%\.cargo\bin 里是 rustc/cargo 的二进制副本，直接用它会导致
#      sysroot 落回 ~\.cargo（缺少 lib\rustlib → E0463）。必须把 rustup 工具链 bin 放最前。
$tc = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"
$env:PATH = "$tc;$env:PATH"
rustc --version           # 期望：rustc 1.98.0 (...)
rustc --print sysroot     # 期望：C:\Users\<you>\.rustup\toolchains\stable-x86_64-pc-windows-msvc

cd rust
cargo build -p gopt-hal
cargo test  -p gopt-hal
cargo clippy -p gopt-hal -- -D warnings

# 真机自检（默认只读，不需要管理员）
cargo run -p gopt-hal --example hal_selfcheck
# 含"写入往返 + 立即回滚"自检（只针对当前进程，可逆）
cargo run -p gopt-hal --example hal_selfcheck -- --apply-writes
```

> 环境备注（一次性的开发机修复，不在仓库内）：
> `cargo clippy` 需要 `clippy` 组件（`rustup component add clippy`）；
> 本机 `%USERPROFILE%\.cargo\bin\cargo-clippy.exe` 原本是 **0 字节占位文件**，cargo 又优先在
> `$CARGO_HOME/bin` 查找子命令，因此该文件已被删除，让 cargo 回落到工具链里的真 `cargo-clippy`。
> 若换机器后 `cargo clippy` 报 "不是有效的 Win32 应用程序"，按同样方式处理即可。

---

## 2. 文件清单（行数）

| 文件 | 行数 | 说明 |
| --- | ---: | --- |
| `rust/Cargo.toml` | 37 | workspace 骨架：resolver=2、edition 2021、release `lto=true` + `codegen-units=1` |
| `rust/.gitignore` | 6 | 忽略 `target/` |
| `rust/Cargo.lock` | 458 | 由 cargo 生成（六个 crate 共享的依赖锁定，建议随仓库提交以保证可复现构建） |
| `crates/gopt-hal/Cargo.toml` | 24 | 依赖面：`windows` 0.58（feature 白名单）+ `serde` |
| `src/lib.rs` | 87 | crate 文档、lint 纪律（`deny(missing_docs)`、`deny(clippy::unwrap_used/panic/...)`）、导出 |
| `src/api.rs` | 214 | `SystemApi` trait 与 `HalOp` 操作标识 |
| `src/error.rs` | 271 | `HalError` / `HalErrorKind` / `HalResult` |
| `src/types.rs` | 750 | 值类型：`Guid`、`PriorityClass`、`WorkingSetLimits`（含 `observed`/`is_restorable`）、`ProcessInfo`、`RunEntry`、`PowerScheme*` |
| `src/affinity.rs` | 529 | 处理器组模型、掩码计算、`AffinityPlan`/`AffinityRequest` |
| `src/hardware.rs` | 133 | `HardwareInfo`、`GpuInfo`、`GpuVendor`、`CoreLayout` |
| `src/mock.rs` | 983 | `MockApi`：调用序列、失败注入、拓扑/权限配置、`seed_working_set` |
| `src/win32/mod.rs` | 249 | `Win32Api`、trait 实现、宽字符/GUID/错误码转换 |
| `src/win32/handle.rs` | 135 | RAII：`OwnedHandle` / `ProcessHandle` / `OwnedRegKey` / `LocalAllocGuard` |
| `src/win32/process.rs` | 438 | 优先级 / 亲和性 / 工作集读（`GetProcessWorkingSetSize`）与写 / 进程枚举 |
| `src/win32/power.rs` | 220 | 电源方案查询、枚举、切换 |
| `src/win32/startup.rs` | 347 | HKCU/HKLM Run 启动项（改名迁移，可回滚） |
| `src/win32/system.rs` | 548 | 提权检测、CPU/内存/DXGI 硬件画像 |
| `examples/hal_selfcheck.rs` | 453 | 真机自检入口（只读 / 可选写入往返） |
| `tests/hal_contract.rs` | 262 | trait 契约集成测试（Mock + 真机只读） |
| `README.md` | 312 | 本文档 |
| **合计** | **6456** | 20 个文件，其中 Rust 源码 5619 行（非空行 5107） |

> 统计口径：`ReadAllLines(UTF-8).Count`（含空行与注释），与 t7 复核时一致。

---

## 3. `SystemApi` trait 契约

```rust
pub trait SystemApi {
    fn backend_name(&self) -> &'static str;                                  // "win32" / "mock"

    // 进程优先级（写入返回"写入前的值"）
    fn get_priority(&self, pid: u32) -> HalResult<PriorityClass>;
    fn set_priority(&self, pid: u32, class: PriorityClass) -> HalResult<PriorityClass>;

    // CPU 亲和性
    fn get_affinity(&self, pid: u32) -> HalResult<AffinityInfo>;
    fn set_affinity(&self, pid: u32, plan: &AffinityPlan) -> HalResult<AffinityApplied>;

    // 工作集（读回系统真值；写目标可被系统回收，不锁内存）
    fn get_working_set(&self, pid: u32) -> HalResult<WorkingSetLimits>;
    fn set_working_set(&self, pid: u32, limits: WorkingSetLimits) -> HalResult<()>;

    // 电源方案（写入返回"切换前 / 切换后"两个方案）
    fn query_power_scheme(&self) -> HalResult<PowerScheme>;
    fn set_power_scheme(&self, target: &PowerSchemeSelector) -> HalResult<PowerSchemeChange>;

    // 开机启动项（HKCU/HKLM Run 键）
    fn list_run_entries(&self) -> HalResult<Vec<RunEntry>>;
    fn set_run_entry_enabled(&self, hive: RunHive, name: &str, enabled: bool) -> HalResult<RunEntry>;

    // 进程枚举 / 权限 / 硬件
    fn list_processes(&self) -> HalResult<Vec<ProcessInfo>>;
    fn is_elevated(&self) -> HalResult<bool>;
    fn hardware(&self) -> HalResult<HardwareInfo>;
}
```

契约规则（两个实现都必须遵守，`tests/hal_contract.rs` 会同时验证）：

1. **只读优先、写入可回滚**：每个写入方法都返回回滚所需信息——`set_priority` 返回旧优先级、
   `set_power_scheme` 返回 `{previous, current}`、`set_run_entry_enabled` 返回变更后的条目
   （禁用是"改名迁移"，把 `Foo` 改成 `[disabled] Foo`，内容与类型原样保留）。
2. **失败即 `HalError`，永不 panic**：crate 内 `deny(clippy::unwrap_used/expect_used/panic/todo/unimplemented)`
   （仅测试豁免），因此"不会 panic"是 CI 可检查的属性；锁中毒也走恢复路径而不是 panic。
3. **同一输入 → 同一错误分类**：例如"目标进程不存在"在两个后端都是 `NotFound`；
   校验顺序也一致（先校验计划本身，再做与目标进程相关的 I/O）。
4. **不支持就显式报 `Unsupported`**，不允许静默降级或猜测。
5. **对象安全**：全部方法 `&self`、无泛型，可直接 `Box<dyn SystemApi>` / `Arc<dyn SystemApi + Send + Sync>`
   （`Win32Api` 是无状态 `Copy` 类型，`MockApi` 内部用 `Mutex`，均为 `Send + Sync`）。

### 3.1 工作集的读与写（"一切可回滚"的那一环）

| 方向 | 方法 | 官方 API | 语义 |
| --- | --- | --- | --- |
| 读 | `get_working_set(pid)` | `GetProcessWorkingSetSize` | 忠实返回系统真值（`min_bytes` 可能为 0） |
| 写 | `set_working_set(pid, limits)` | `SetProcessWorkingSetSize` | 拒绝 `min_bytes == 0`（不产生"零下限"的写入请求） |

* `WorkingSetLimits::new`（写路径）要求 `min > 0`；`WorkingSetLimits::observed`（读路径）允许 `min == 0`；
* `WorkingSetLimits::is_restorable()` 回答"这个观测值能不能当写入目标"：为 `false` 时**不要**把它
  记成可回滚的 `before`（`gopt-core` 据此把该步骤标为不可回滚，而不是让回滚在运行时才发现还原不了）；
* `max_bytes` 的哨兵语义只属于**写**路径：`0` / `NO_UPPER_BOUND`（8 TiB）表示"不设上限"；
  读回来的是真实值（本机 1413120 字节），不要拿它跟哨兵比较；
* Mock 未显式安排时返回声明式默认值 `MockApi::DEFAULT_WORKING_SET`（200 KiB / 无上限），
  可用 `seed_working_set`（**不记录调用**）覆盖成测试需要的"写入前状态"。

## 4. 错误模型

```rust
pub struct HalError { kind: HalErrorKind, operation: &'static str, message: String, win32_code: Option<u32> }
pub type HalResult<T> = Result<T, HalError>;
```

| `HalErrorKind` | 触发场景 | 调用方应有的反应 |
| --- | --- | --- |
| `invalid_argument` | 空掩码、`min > max`、不存在的处理器组、重复启动项名 | 修正输入后重试 |
| `unsupported` | 跨处理器组亲和性、系统不支持的能力 | 按策略降级/拆分（如 `AffinityPlan::per_group()`） |
| `not_found` | 目标进程已退出、注册表值不存在、未安装的电源方案 | 跳过并记录 |
| `access_denied` | 未提权写 HKLM、切换电源方案、受保护进程 | 提示提权或跳过（`win32_code == 5`） |
| `policy_denied` | **REALTIME 优先级（安全红线）** | 立即停止，不得降级重试 |
| `win32` | 其它系统调用失败，`win32_code` 带原始错误码 | 按错误码诊断 |
| `internal` | 系统返回自相矛盾的数据 | 记录并上报 |

设计要点：

* `operation` 是失败时正在执行的官方 API/操作名（`SetPriorityClass`、`RegOpenKeyExW`…），
  直接进审计日志，无需解析字符串。
* `win32_code` 是**原始 Win32 错误码**：windows-rs 会把 `BOOL` 失败包装成
  `HRESULT_FROM_WIN32(code)`，HAL 统一还原成低 16 位（`error_from` / `win32_code_of`）。
* `message` 是稳定的**英文**诊断文本，便于哈希链审计日志跨版本比对；
  CLI/GUI 面向用户的中文由前端按 `kind` + `operation` 本地化（中英双语红线在前端落地）。
* 所有字段私有、只经访问器读取，序列化格式稳定；`HalError: Display + std::error::Error + Serialize`。

## 5. 亲和性模型（含 >64 逻辑核）

* Windows 的亲和性掩码是"每组 64 位"。逻辑核 >64 的机器会被划分为多个**处理器组**；
  `SetProcessAffinityMask` 只作用于进程主组，跨组必须逐线程 `SetThreadGroupAffinity`。
* `AffinityPlan` 是"一个或多个处理器组上的掩码"：
  * `AffinityPlan::full(total_logical)`：全核；
  * `AffinityPlan::reserve_last_n_cores(total_logical, n)`：保留**全局序号最大**的 n 个核给系统；
  * `AffinityPlan::single(total_logical, group, mask)`：单组显式掩码；
  * `plan.per_group()`：拆成若干"单组计划"（>64 核机器上逐组应用）。
* 掩码计算规则：第 g 组持有全局序号 `[g*64, g*64+64)`，组内第 i 个逻辑处理器对应第 i 位；
  某组被整组保留时该组会从计划中消失（不会产生"掩码为 0"的非法请求）。
  例：96 逻辑核保留最后 4 核 → `g0=0xffffffffffffffff`、`g1=0x00000000ffffffff`（28 个核）。
* 应用策略（`set_affinity`）：
  * `group == 0` 单组计划 → `SetProcessAffinityMask`，`method = process_affinity_mask`；
  * `group != 0` 单组计划 → 逐线程 `SetThreadGroupAffinity`，`method = thread_group_affinity`
    并返回 `threads_updated`；
  * 多组计划 → `Unsupported`（一个线程只能属于一个组，"整体绑定到多组"在 Win32 里无对应语义）。
* `AffinityRequest` 字段私有、只能经 `new()` 构造，因此"掩码为 0"在类型层面不可表达。

## 6. 安全红线的落地方式

| 红线 | 落地方式（可验证） |
| --- | --- |
| 仅官方 Win32 API | 依赖只有 `windows` 0.58 + `serde`；`Cargo.toml` 里的 feature 列表就是允许触碰的全部 API 面（kernel32 / advapi32 / powrprof / dxgi / 注册表） |
| 无注入、无内核 Hook | 没有任何 `OpenProcessToken` 之外的提权调用、没有 `LoadLibrary` 第三方 DLL、没有 `WriteProcessMemory`/`CreateRemoteThread` |
| 优先级上限 HIGH | `PriorityClass` 枚举里没有 REALTIME 这一档；`PriorityClass::from_raw(0x100)` / `parse("realtime")` / `parse("0x100")` 全部返回 `PolicyDenied`（单测 + 集成测试覆盖） |
| 一切可回滚 | 写入返回旧值；工作集有官方读路径（`get_working_set` → `GetProcessWorkingSetSize`），前值是真值；启动项禁用=改名迁移（`[disabled] ` 前缀与 C++ 版逐字节一致，备份文件可互操作）；改名前先查重名，删旧值失败时回滚新值 |
| 全部功能免费 | 无任何许可证/在线校验调用 |
| 中英双语 | 错误 `message` 为稳定英文；`HalErrorKind` 供前端本地化 |

## 7. Mock 后端（无需管理员即可单测）

```rust
use gopt_hal::{HalOp, MockApi, PriorityClass, SystemApi};

let api = MockApi::sample_workstation();          // 16 逻辑核/8 物理核 + 3 进程 + 3 启动项 + 3 电源方案
let previous = api.set_priority(1234, PriorityClass::High)?;   // -> Normal
assert_eq!(api.last_call().map(|call| call.op), Some(HalOp::SetPriority));

api.fail_next(HalOp::SetAffinity, gopt_hal::HalError::win32_from_code("SetProcessAffinityMask", 5));
let err = api
    .set_affinity(1234, &gopt_hal::AffinityPlan::full(16)?)
    .expect_err("injected failure");
assert!(!err.is_policy_denial() && err.win32_code() == Some(5));   // 复现"设置失败"的降级路径
# Ok::<(), gopt_hal::HalError>(())
```

* `calls()` / `call_count(op)` / `last_call()`：断言"策略引擎到底调了哪些 API、参数是什么"。
* `fail_next(op, err)` / `fail_always(op, err)`：注入失败；**失败发生在记录调用之后**，调用序列永远完整。
* `with_topology(physical, logical)` / `with_cores(logical)`：造 >64 核多组拓扑。
* `push_process/push_run_entry/push_power_scheme/set_active_power_scheme/set_elevated`：搭状态。
* Mock 与 Win32 **保持同样的错误分类与校验顺序**（包括跨组 → `Unsupported`）。

## 8. 真机自检入口

```powershell
cargo run -p gopt-hal --example hal_selfcheck                 # 只读，23 项检查
cargo run -p gopt-hal --example hal_selfcheck -- --apply-writes  # 28 项，含写入往返 + 回滚
```

本机实测（Windows，AMD Ryzen 9 7945HX / RTX 4060 Laptop / 32 逻辑核 / 1 个处理器组）：

```text
[ OK ] is_elevated() = true
[ OK ] hardware(): AMD Ryzen 9 7945HX with Radeon Graphics | 15C/32T | 核布局 15 条 | 处理器组 1 | 内存 16064 MiB（可用 8464 MiB）| 大页 false
[ OK ] GPU: NVIDIA GeForce RTX 4060 Laptop GPU (nvidia, vendor 0x10de, device 0x28e0, 显存 7956 MiB, 驱动 32.0.16.1088, 硬件 = true)
[ OK ] AffinityPlan::full() = 32 of 32 logical processors [g0=0x00000000ffffffff]
[ OK ] query_power_scheme() = 电脑调试 (73cfc528-bce2-4a2a-bf32-407223d12ac3)
[WARN] 未安装高性能方案：PowerSchemeSelector::HighPerformance 会返回 NotFound
[ OK ] list_processes(): 279 个进程，其中 275 个可读到完整路径（其余为受保护进程，exe_path = None）
[ OK ] list_run_entries(): 9 个 Run 启动项
[ OK ] get_working_set(10884) = min 204800 / max 1413120 bytes（可精确还原）
[ OK ] 红线生效：PriorityClass::from_raw(0x100) → policy_denied
自检结束：23 项通过，0 项失败
```

`--apply-writes` 会在**当前进程**上做可逆往返，并打印三段证据：

```text
[ OK ] set_priority(10884, high) 返回写入前的值 normal（期望 normal）
[ OK ] 回滚优先级：high → normal
[ OK ] 回滚后读回 = normal
[ OK ] set_working_set(10884, 64/128 MiB) → 读回 min 67108864 / max 134217728 bytes（期望 67108864 / 134217728）
[ OK ] 还原工作集 → 读回 min 204800 / max 1413120 bytes（期望 204800 / 1413120）
自检结束：28 项通过，0 项失败
```

退出码：任一检查失败即 `exit 1`（不 panic），可直接用于冒烟脚本与 CI。

## 9. 与 C++ v1.1.0 的行为差异（有意为之）

| 主题 | C++ v1.1.0 | gopt-hal | 原因 |
| --- | --- | --- | --- |
| 找不到"高性能"电源方案 | 盲目激活内置 GUID | `NotFound`，由前端提示/让用户选方案 | 可解释优先于碰运气；避免在 OEM 机器上激活一个不存在的方案 |
| 电源方案友好名 | 传全零 GUID 指针 | 显式传 NULL | 本机实测：传零 GUID 时 `PowerReadFriendlyName` 拿不到名字（只能用 GUID 文本兜底），传 NULL 后正常返回"电脑调试"等本地化名称 |
| >64 逻辑核亲和性 | 直接返回 false 降级跳过 | 计算逐组掩码 + 逐线程 `SetThreadGroupAffinity`，多组计划显式 `Unsupported` | 让 >64 核机器真正可用，同时不掩盖语义限制 |
| 内存频率 / 最大睿频 | SMBIOS / CPUID 探测 | 未提供字段 | 需要 SMBIOS 解析与 CPUID，留待后续任务；宁缺毋滥，不提供恒为 0 的字段 |

## 10. 依赖面

```toml
[dependencies]
serde  = { version = "1", features = ["derive"] }
windows = { version = "0.58", features = [
    "Win32_Foundation", "Win32_Security", "Win32_System_Threading",
    "Win32_System_SystemInformation", "Win32_System_Diagnostics_ToolHelp",
    "Win32_System_Registry", "Win32_System_Power", "Win32_Graphics_Dxgi",
] }
```

没有 async runtime、没有日志框架、没有 GUI 框架、没有第三方 Win32 封装。
公开 API 不暴露 `windows` 类型（`Guid` 等自有值类型 + `win32/mod.rs` 内部转换），
因此前端与策略引擎只依赖 `gopt_hal` 本身。

## 11. 验证结果（本机 rustc 1.98.0 / cargo 1.98.0）

| 命令 | 结果 |
| --- | --- |
| `cargo build -p gopt-hal` | exit 0 |
| `cargo test -p gopt-hal` | exit 0：库单测 **65 passed**，`tests/hal_contract.rs` **13 passed**，doctest **1 passed** |
| `cargo clippy -p gopt-hal -- -D warnings` | exit 0，无告警 |
| `cargo clippy -p gopt-hal --all-targets -- -D warnings` | exit 0，无告警（含示例与集成测试） |
| `cargo run -p gopt-hal --example hal_selfcheck` | exit 0，23 项通过 |
| `cargo run -p gopt-hal --example hal_selfcheck -- --apply-writes` | exit 0，28 项通过（含优先级与工作集的写入→读回→还原） |

必测项覆盖：**REALTIME 拒绝**（`types::tests::realtime_is_hard_rejected_from_raw`、
`win32::tests::realtime_priority_is_rejected_by_the_only_raw_entry_point`、
`hal_contract::realtime_is_rejected_before_any_backend_call`）、
**亲和性掩码计算 / >64 核分组**（`affinity::tests::reserve_last_n_cores_spans_multiple_groups`、
`processor_groups_split_at_64`、`mock::tests::topology_spreads_logical_cores_across_groups`）、
**目标进程不存在**（`win32::process::tests::missing_process_is_reported_as_not_found`、
`mock::tests::unknown_process_is_not_found_and_still_recorded`）、
**工作集读的成/败两条路径**（`win32::process::tests::own_working_set_is_readable`、
`working_set_read_rejects_missing_and_invalid_targets`、
`working_set_round_trip_on_the_current_process`、
`mock::tests::working_set_read_reports_default_applied_and_observed_values`、
`working_set_read_fails_like_the_real_backend`）。

## 12. 给下游 crate 的接口说明

* 新增 crate 需登记到 `rust/Cargo.toml` 的 `members`（刻意用显式列表：兄弟 crate 的编译错误
  不会影响 `cargo build -p gopt-hal` 这一类单包命令）。
* 内核/策略层请只依赖 `gopt_hal` 的公开类型与 trait，不要直接依赖 `windows`：
  HAL 已经把所有平台细节收口，`cargo test` 才能在没有管理员、没有游戏的 CI 上跑通全部逻辑。
* `MockApi::calls()` 可直接用于"策略 → API 调用"的断言；`Call`/`CallArgs` 都实现了 `Serialize`，
  可以直接进测试报告或 JSON 输出。

## 13. 已知限制 / 后续任务

* 未实现：SMBIOS 内存频率、CPUID 最大睿频、CPU 型号的 CPUID 兜底（注册表取不到时显示 `Unknown CPU`）。
* 启动项仅覆盖注册表 Run 键；`RunOnce`、启动文件夹、计划任务、服务未纳入（与 C++ 版范围一致）。
* 单组计划之外（>64 核多组）需要策略层显式调用 `per_group()` 逐组应用——这是 Win32 的语义限制，
  不是实现取巧。
* `--apply-writes` 自检只覆盖优先级与工作集；电源方案切换与启动项改名会改动系统状态，
  因此不在自检里自动执行（由 CLI 在用户确认后调用，回滚信息已由 trait 返回）。
