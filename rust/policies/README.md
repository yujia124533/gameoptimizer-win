# rust/policies —— 声明式策略目录

这个目录就是 GameOptimizer-RS 的"游戏库"：全部以 TOML 描述，**加游戏 / 改规则不需要重新编译**。
加载器（`gopt-policy` 的 `PolicyLoader`）按两层合并：

```text
内置层：本目录 *.toml                       —— 经 include_str! 编译进二进制
用户层：%LOCALAPPDATA%\GameOptimizer\policies.d\*.toml   —— 可选，同 id 覆盖内置，新 id 新增
```

内置层与磁盘目录由测试 `tests/builtin_policies.rs::embedded_builtin_list_matches_the_repository_directory`
强制一一对应：新增文件必须同时登记到 `crates/gopt-policy/src/builtin.rs` 的 `BUILTIN_FILES`，
否则测试会失败。

## 加一款游戏

1. 复制任意一个文件（例如 `cs2.toml`）为 `<你的游戏>.toml`；
2. 改 `id`（kebab-case）、`name_zh`、`name_en`、`match`（exe 名通配 `*`/`?`，不是路径）；
3. 按需增删 `[[game.rules]]`；
4. 在 `src/builtin.rs` 的 `BUILTIN_FILES` 里加一行 `include_str!`；
5. `cargo test -p gopt-policy`。

不想改代码？直接把文件放进 `%LOCALAPPDATA%\GameOptimizer\policies.d\` 即可生效——
这正是 `example-custom.toml` 演示的路径（它同时作为模板）。

## 覆盖内置策略

用户目录里放一个**同 `id`** 的文件即可整条覆盖（不是合并字段），例如把 CS2 改成保守档：

```toml
[[game]]
id = "cs2"                       # 与内置同 id ⇒ 覆盖
name_zh = "CS2（保守档）"
name_en = "Counter-Strike 2 (conservative)"
match = "cs2.exe"

[[game.rules]]
id = "priority"
action = { priority = { class = "above-normal" } }
```

覆盖会留下一条 warning（`user policy ... overrides the built-in policy ...`），不会静默生效。

## 文件一览

| 文件 | 游戏 | 说明 |
| --- | --- | --- |
| `delta-force.toml` | 三角洲行动 | 与 C++ v1.1.0 `GameId::DeltaForce` 对齐 |
| `league-of-legends.toml` | 英雄联盟 | `GameId::LeagueOfLegends` |
| `cs2.toml` | CS2 | `GameId::CS2`（仅物理核 + 工作集阶梯） |
| `pubg.toml` | 绝地求生 | `GameId::PUBG` |
| `valorant.toml` | 无畏契约 | `GameId::Valorant`（别名"瓦罗兰特"） |
| `apex-legends.toml` | Apex 英雄 | `GameId::Apex` |
| `dota-2.toml` | Dota 2 | `GameId::Dota2` |
| `overwatch-2.toml` | 守望先锋2 | `GameId::Overwatch2` |
| `example-custom.toml` | 永劫无间 / 原神 | 纯数据新增 + 覆盖/掩码/电源/启动项示例 |

字段、动作与条件的完整说明见 [`../crates/gopt-policy/README.md`](../crates/gopt-policy/README.md)。

## 解析失败会怎样

不会 panic，也不会让其它策略失效：加载器为每个文件单独产出诊断，
形如 `<路径>:<行>:<列>: error: <稳定英文原因>`，CLI 会原样展示。
`cargo test -p gopt-policy --test invalid_toml` 里有完整的"错误 → 行号"矩阵。
