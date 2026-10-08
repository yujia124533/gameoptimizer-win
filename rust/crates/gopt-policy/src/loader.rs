//! 加载：内置策略（编译进二进制）+ 用户覆盖目录，分层合并成 [`PolicySet`]。
//!
//! # 分层与优先规则
//!
//! 1. **内置层**：仓库 `rust/policies/*.toml`，经 `include_str!` 进入二进制；
//! 2. **用户层**：`%LOCALAPPDATA%\GameOptimizer\policies.d\*.toml`（可用
//!    [`PolicyLoader::with_user_dir`] 换成任意目录，测试就这么做）；
//! 3. 用户层**优先**：同 `id` 覆盖内置（不是并存），新 `id` 直接新增；
//!    同名 id 在用户层内重复时，文件名排序靠后的赢，并给出一条 warning；
//! 4. `PolicySet::games()` 的顺序 = 用户层（文件名字典序 + 文件内声明序）在前、
//!    内置层（[`crate::builtin::BUILTIN_FILES`] 顺序）在后：**查找与匹配都是用户优先**；
//! 5. 任一文件出错只影响该文件：错误进 [`PolicyLoadOutcome::diagnostics`]（带文件名与行号），
//!    其余策略照常可用；目录不存在不算错误（全新安装就应该是这样）。

use std::fs;
use std::path::{Path, PathBuf};

use gopt_hal::ProcessInfo;

use crate::builtin::BUILTIN_FILES;
use crate::error::{PolicyDiagnostic, PolicyLoadErrors, PolicyOrigin};
use crate::eval::EvalInput;
use crate::model::GamePolicy;
use crate::plan::Plan;
use crate::validate::parse_policy_file;

/// 策略加载器。
///
/// ```
/// use gopt_policy::PolicyLoader;
///
/// // 内置策略（8 款 C++ 兼容预设 + 示例文件）
/// let outcome = PolicyLoader::builtin_only().load();
/// assert!(!outcome.has_errors());
/// let set = outcome.into_set();
/// assert!(set.get("cs2").is_some());
/// assert!(set.get("CS2").is_some()); // id 查找大小写不敏感
///
/// // 内置 + 默认用户目录（%LOCALAPPDATA%\GameOptimizer\policies.d）
/// let loader = PolicyLoader::new();
/// assert!(loader.user_dirs().len() <= 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyLoader {
    builtin: bool,
    user_dirs: Vec<PathBuf>,
}

impl Default for PolicyLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyLoader {
    /// 内置策略 + 默认用户覆盖目录（若 `%LOCALAPPDATA%` 存在）。
    pub fn new() -> Self {
        let mut loader = Self {
            builtin: true,
            user_dirs: Vec::new(),
        };
        if let Some(dir) = Self::default_user_dir() {
            loader.user_dirs.push(dir);
        }
        loader
    }

    /// 只要内置策略（CLI 的 `--no-user-policies` 与绝大多数测试用这个）。
    pub fn builtin_only() -> Self {
        Self {
            builtin: true,
            user_dirs: Vec::new(),
        }
    }

    /// 追加一个用户覆盖目录（可多次调用，按追加顺序处理）。
    #[must_use]
    pub fn with_user_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.user_dirs.push(dir.into());
        self
    }

    /// 不带内置层（只用用户目录；测试"用户新增游戏"时用）。
    pub fn user_only() -> Self {
        Self {
            builtin: false,
            user_dirs: Vec::new(),
        }
    }

    /// 默认用户覆盖目录：`%LOCALAPPDATA%\GameOptimizer\policies.d`。
    ///
    /// 环境变量缺失时返回 `None`（策略引擎不会因此失败，只是没有用户层）。
    pub fn default_user_dir() -> Option<PathBuf> {
        let base = std::env::var_os("LOCALAPPDATA")?;
        let mut path = PathBuf::from(base);
        path.push("GameOptimizer");
        path.push("policies.d");
        Some(path)
    }

    /// 是否加载内置层。
    pub const fn loads_builtin(&self) -> bool {
        self.builtin
    }

    /// 用户覆盖目录列表。
    pub fn user_dirs(&self) -> &[PathBuf] {
        &self.user_dirs
    }

    /// 执行加载：**永不失败**，所有问题都变成 [`PolicyDiagnostic`]。
    pub fn load(&self) -> PolicyLoadOutcome {
        let mut diagnostics: Vec<PolicyDiagnostic> = Vec::new();
        let mut user_games: Vec<GamePolicy> = Vec::new();
        let mut builtin_games: Vec<GamePolicy> = Vec::new();

        for dir in &self.user_dirs {
            for (path, text) in read_policy_dir(dir, &mut diagnostics) {
                let origin = PolicyOrigin::user(path.display().to_string());
                match parse_policy_file(&text, origin.clone()) {
                    Ok(games) => {
                        for game in games {
                            let id = game.id().to_string();
                            if let Some(existing) =
                                user_games.iter_mut().find(|existing| existing.id() == id)
                            {
                                diagnostics.push(PolicyDiagnostic::warning(
                                    origin.clone(),
                                    None,
                                    None,
                                    format!("game `{id}` is defined twice in the user layer; the later file wins"),
                                ));
                                *existing = game;
                            } else {
                                user_games.push(game);
                            }
                        }
                    }
                    Err(diagnostic) => diagnostics.push(diagnostic),
                }
            }
        }

        if self.builtin {
            for file in BUILTIN_FILES {
                let origin = PolicyOrigin::builtin(file.name);
                match parse_policy_file(file.text, origin.clone()) {
                    Ok(games) => builtin_games.extend(games),
                    Err(diagnostic) => diagnostics.push(diagnostic),
                }
            }
        }

        // 用户层覆盖内置层：剔除被覆盖的 id，并留下一条可见的 warning。
        let mut games = user_games;
        for game in builtin_games {
            match games.iter().find(|existing| existing.id() == game.id()) {
                Some(existing) => diagnostics.push(PolicyDiagnostic::warning(
                    existing.origin().clone(),
                    None,
                    None,
                    format!(
                        "user policy `{}` overrides the built-in policy from {}",
                        existing.id(),
                        game.origin().display_path()
                    ),
                )),
                None => games.push(game),
            }
        }

        // exe 模式冲突：只有先被匹配到的那个会生效，必须让用户看得见。
        for (index, game) in games.iter().enumerate() {
            for other in games.iter().skip(index + 1) {
                for pattern in game.patterns() {
                    if other
                        .patterns()
                        .any(|candidate| candidate.eq_ignore_ascii_case(pattern))
                    {
                        diagnostics.push(PolicyDiagnostic::warning(
                            other.origin().clone(),
                            None,
                            None,
                            format!(
                                "exe pattern `{pattern}` is claimed by both `{}` and `{}`; `{}` wins (user layer first)",
                                game.id(),
                                other.id(),
                                game.id()
                            ),
                        ));
                    }
                }
            }
        }

        PolicyLoadOutcome::new(PolicySet { games }, diagnostics)
    }
}

/// 读取一个目录下的全部 `*.toml`（按文件名字典序）；目录不存在时静默返回空列表。
fn read_policy_dir(dir: &Path, diagnostics: &mut Vec<PolicyDiagnostic>) -> Vec<(PathBuf, String)> {
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            // 目录不存在是正常情况（全新安装还没有用户策略）；其它 IO 错误要让用户看到。
            if err.kind() != std::io::ErrorKind::NotFound {
                diagnostics.push(PolicyDiagnostic::warning(
                    PolicyOrigin::user(dir.display().to_string()),
                    None,
                    None,
                    format!("cannot read the user policy directory: {err}"),
                ));
            }
            return files;
        }
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_toml(path))
        .collect();
    paths.sort();

    for path in paths {
        let origin = PolicyOrigin::user(path.display().to_string());
        match fs::read_to_string(&path) {
            Ok(text) => files.push((path, text)),
            Err(err) => diagnostics.push(PolicyDiagnostic::error(
                origin,
                None,
                None,
                format!("cannot read the policy file: {err}"),
            )),
        }
    }
    files
}

fn is_toml(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("toml"))
        .unwrap_or(false)
}

/// 加载结果：可用策略集合 + 全部诊断（错误 + 警告）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyLoadOutcome {
    set: PolicySet,
    diagnostics: Vec<PolicyDiagnostic>,
}

impl PolicyLoadOutcome {
    /// 构造。
    pub fn new(set: PolicySet, diagnostics: Vec<PolicyDiagnostic>) -> Self {
        Self { set, diagnostics }
    }

    /// 已加载的策略集合（坏文件被跳过，不影响这里）。
    pub const fn set(&self) -> &PolicySet {
        &self.set
    }

    /// 取走策略集合。
    pub fn into_set(self) -> PolicySet {
        self.set
    }

    /// 全部诊断（错误在前、警告在后？——不，保持**产生顺序**，便于对照文件）。
    pub fn diagnostics(&self) -> &[PolicyDiagnostic] {
        &self.diagnostics
    }

    /// 只取错误。
    pub fn errors(&self) -> Vec<&PolicyDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|item| item.is_error())
            .collect()
    }

    /// 只取警告。
    pub fn warnings(&self) -> Vec<&PolicyDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|item| !item.is_error())
            .collect()
    }

    /// 是否有文件解析失败。
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(PolicyDiagnostic::is_error)
    }

    /// 严格模式：有任何错误就返回 [`PolicyLoadErrors`]（给 `?` 用）。
    pub fn into_result(self) -> Result<PolicySet, PolicyLoadErrors> {
        let errors: Vec<PolicyDiagnostic> = self
            .diagnostics
            .into_iter()
            .filter(PolicyDiagnostic::is_error)
            .collect();
        if errors.is_empty() {
            Ok(self.set)
        } else {
            Err(PolicyLoadErrors::new(errors))
        }
    }
}

/// 合并后的策略集合：用户层在前、内置层在后，查找与匹配都是用户优先。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicySet {
    games: Vec<GamePolicy>,
}

impl PolicySet {
    /// 由已排序的游戏策略列表构造。
    pub fn new(games: Vec<GamePolicy>) -> Self {
        Self { games }
    }

    /// 全部策略（用户层在前）。
    pub fn games(&self) -> &[GamePolicy] {
        &self.games
    }

    /// 策略数量。
    pub fn len(&self) -> usize {
        self.games.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    /// 全部游戏 id（展示顺序稳定）。
    pub fn game_ids(&self) -> Vec<&str> {
        self.games.iter().map(GamePolicy::id).collect()
    }

    /// 按 id 精确查找（大小写不敏感，与 CLI 输入习惯一致）。
    pub fn get(&self, id: &str) -> Option<&GamePolicy> {
        self.games
            .iter()
            .find(|game| game.id().eq_ignore_ascii_case(id.trim()))
    }

    /// 宽松查找：id / 中英文名 / 名称别名 / exe 模式。
    pub fn find(&self, query: &str) -> Option<&GamePolicy> {
        self.games.iter().find(|game| game.matches_query(query))
    }

    /// 宽松查找的全部命中。
    pub fn find_all(&self, query: &str) -> Vec<&GamePolicy> {
        self.games
            .iter()
            .filter(|game| game.matches_query(query))
            .collect()
    }

    /// 按进程 exe 名匹配策略（用户层优先；`exe_aliases` 也参与匹配）。
    pub fn match_process_name(&self, exe_name: &str) -> Option<&GamePolicy> {
        self.games.iter().find(|game| game.matches_exe(exe_name))
    }

    /// 为一个进程生成计划（不匹配任何策略时为 `None`）。
    pub fn plan_for_process(&self, process: &ProcessInfo, input: &EvalInput) -> Option<Plan> {
        self.match_process_name(&process.name)
            .map(|game| game.plan(input, process.pid))
    }

    /// 为一批进程生成计划：按传入顺序，每个匹配到的进程一份计划（同款游戏多开 = 多份计划）。
    pub fn plans_for(&self, input: &EvalInput, processes: &[ProcessInfo]) -> Vec<Plan> {
        processes
            .iter()
            .filter_map(|process| self.plan_for_process(process, input))
            .collect()
    }
}
