//! 内置策略文件清单（`include_str!` 编译进二进制）。
//!
//! 为什么是"编译进来"而不是"运行时去磁盘找目录"：
//!
//! * 安装后的 gopt 是单个 exe，用户机器上不一定有 `policies/` 目录；内置策略必须随二进制走；
//! * 内置文件仍然在仓库里以 TOML 存在（`rust/policies/*.toml`），改策略的流程与用户策略完全一致，
//!   "加游戏 / 改规则"这件事**在数据层**完成，编译只是把数据打包。
//!
//! `tests/builtin_files.rs` 会比对磁盘目录与这份清单：两边必须一一对应，
//! 因此不会出现"改了 TOML 却忘了登记"或"清单里有文件但目录里没有"的情况。

/// 一个内置策略文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinFile {
    /// 文件名（与 `rust/policies/` 下的文件同名，也是诊断里的来源标识）。
    pub name: &'static str,
    /// 文件全文。
    pub text: &'static str,
}

/// 全部内置策略文件（顺序 = 加载顺序 = `PolicySet::games()` 里内置段的顺序）。
///
/// 前 8 个与 C++ 版 v1.1.0 的 `GameId` 一一对应（语义见各自文件头注释），
/// 最后一个是"加游戏不重编译"的示例（永劫无间 / 原神），同样以纯数据方式生效。
pub const BUILTIN_FILES: &[BuiltinFile] = &[
    BuiltinFile {
        name: "delta-force.toml",
        text: include_str!("../../../policies/delta-force.toml"),
    },
    BuiltinFile {
        name: "league-of-legends.toml",
        text: include_str!("../../../policies/league-of-legends.toml"),
    },
    BuiltinFile {
        name: "cs2.toml",
        text: include_str!("../../../policies/cs2.toml"),
    },
    BuiltinFile {
        name: "pubg.toml",
        text: include_str!("../../../policies/pubg.toml"),
    },
    BuiltinFile {
        name: "valorant.toml",
        text: include_str!("../../../policies/valorant.toml"),
    },
    BuiltinFile {
        name: "apex-legends.toml",
        text: include_str!("../../../policies/apex-legends.toml"),
    },
    BuiltinFile {
        name: "dota-2.toml",
        text: include_str!("../../../policies/dota-2.toml"),
    },
    BuiltinFile {
        name: "overwatch-2.toml",
        text: include_str!("../../../policies/overwatch-2.toml"),
    },
    BuiltinFile {
        name: "example-custom.toml",
        text: include_str!("../../../policies/example-custom.toml"),
    },
];

/// 按文件名取内置策略文件。
pub fn builtin_file(name: &str) -> Option<&'static BuiltinFile> {
    BUILTIN_FILES.iter().find(|file| file.name == name)
}

/// 内置策略文件里与 C++ 版 v1.1.0 一一对应的 8 款游戏（用于兼容性测试与文档）。
pub const CPP_PARITY_FILES: [&str; 8] = [
    "delta-force.toml",
    "league-of-legends.toml",
    "cs2.toml",
    "pubg.toml",
    "valorant.toml",
    "apex-legends.toml",
    "dota-2.toml",
    "overwatch-2.toml",
];
