//! 数据目录解析：**一个进程只看一个目录**，测试因此可以完全不碰真实配置。
//!
//! 解析优先级（[`DataPaths::resolve`]）：
//!
//! 1. `--data-dir <路径>`（CLI 显式指定，最高优先级）；
//! 2. `GOPT_DATA_DIR` 环境变量；
//! 3. `%LOCALAPPDATA%\GameOptimizer`（与 C++ 版 `SecurityRollback::SaveFileDirW` 同策略）；
//! 4. `%LOCALAPPDATA%` 缺失时退化为当前目录下的 `GameOptimizer\`。
//!
//! 目录内布局（全部由本模块唯一决定，前端不再自己拼路径）：
//!
//! ```text
//! <data_dir>/
//! ├── journal.jsonl     审计日志（哈希链）
//! ├── policies.d/       用户策略覆盖（*.toml，同 id 覆盖内置）
//! ├── savepoints.txt    C++ v1.1.0 旧格式快照（只读导入）
//! └── games.conf        C++ v1.1.0 每游戏启动配置（只读导入）
//! ```

use std::path::{Path, PathBuf};

use gopt_journal::{DATA_DIR_NAME, JOURNAL_FILE_NAME};

use crate::error::{CoreError, CoreResult};

/// 数据目录环境变量名（验收要求：用环境变量把数据目录指到 %TEMP% 下的测试目录）。
pub const DATA_DIR_ENV: &str = "GOPT_DATA_DIR";

/// 数据目录及其内部布局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPaths {
    root: PathBuf,
    /// 是否来自环境变量/参数（true）还是平台默认（false）——用于提示里区分。
    explicit: bool,
}

impl DataPaths {
    /// 用给定目录构造（测试与 `--data-dir` 用）。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            explicit: true,
        }
    }

    /// 平台默认目录：`%LOCALAPPDATA%\GameOptimizer`，缺失时退化为 `.\GameOptimizer`。
    pub fn platform_default() -> Self {
        match std::env::var_os("LOCALAPPDATA") {
            Some(value) if !value.is_empty() => Self::new(PathBuf::from(value).join(DATA_DIR_NAME)),
            _ => {
                let mut path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                path.push(DATA_DIR_NAME);
                Self::new(path)
            }
        }
    }

    /// 按优先级解析：参数 > `GOPT_DATA_DIR` > 平台默认。
    pub fn resolve(explicit: Option<impl Into<PathBuf>>) -> Self {
        if let Some(path) = explicit {
            return Self::new(path);
        }
        if let Some(value) = std::env::var_os(DATA_DIR_ENV) {
            if !value.is_empty() {
                return Self::new(PathBuf::from(value));
            }
        }
        Self {
            explicit: false,
            ..Self::platform_default()
        }
    }

    /// 数据目录根。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 是否由参数/环境变量指定。
    pub const fn is_explicit(&self) -> bool {
        self.explicit
    }

    /// 审计日志路径 `journal.jsonl`。
    pub fn journal(&self) -> PathBuf {
        self.root.join(JOURNAL_FILE_NAME)
    }

    /// 用户策略目录 `policies.d`。
    pub fn policies_dir(&self) -> PathBuf {
        self.root.join("policies.d")
    }

    /// C++ 旧格式快照 `savepoints.txt`。
    pub fn savepoints(&self) -> PathBuf {
        self.root.join(gopt_journal::SAVEPOINTS_FILE_NAME)
    }

    /// C++ 旧格式游戏配置 `games.conf`。
    pub fn games_conf(&self) -> PathBuf {
        self.root.join(gopt_journal::GAMES_CONF_FILE_NAME)
    }

    /// 创建数据目录（写命令在落盘前调用；只读命令不创建）。
    pub fn ensure_dir(&self) -> CoreResult<()> {
        std::fs::create_dir_all(&self.root).map_err(|err| {
            CoreError::io(
                "DataPaths::ensure_dir",
                format!(
                    "cannot create the data directory {}: {err}",
                    self.root.display()
                ),
            )
        })
    }

    /// 展示用文本（`root` + 来源）。
    pub fn describe(&self) -> String {
        let source = if self.explicit {
            "explicit"
        } else {
            "platform default"
        };
        format!("{} ({source})", self.root.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_centralised() {
        let paths = DataPaths::new(r"C:\tmp\gopt");
        assert_eq!(paths.journal(), PathBuf::from(r"C:\tmp\gopt\journal.jsonl"));
        assert_eq!(
            paths.policies_dir(),
            PathBuf::from(r"C:\tmp\gopt\policies.d")
        );
        assert!(paths.savepoints().ends_with("savepoints.txt"));
        assert!(paths.games_conf().ends_with("games.conf"));
        assert!(paths.is_explicit());
        assert!(paths.describe().contains("explicit"));
    }

    #[test]
    fn explicit_argument_wins_over_environment() {
        std::env::set_var(DATA_DIR_ENV, r"C:\from-env");
        let explicit = DataPaths::resolve(Some(r"C:\from-arg"));
        assert_eq!(explicit.root(), Path::new(r"C:\from-arg"));

        let from_env = DataPaths::resolve(None::<PathBuf>);
        assert_eq!(from_env.root(), Path::new(r"C:\from-env"));

        std::env::remove_var(DATA_DIR_ENV);
        let fallback = DataPaths::resolve(None::<PathBuf>);
        assert!(fallback.root().ends_with(DATA_DIR_NAME));
        // 平台默认一定不是"显式"来源：提示里要能区分。
        assert!(!fallback.is_explicit());
    }
}
