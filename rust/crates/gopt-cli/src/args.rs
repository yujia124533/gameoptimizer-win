//! 手写参数解析（**刻意不引入 clap**）：解析结果就是类型化的 [`Invocation`]，非法输入给出用法错误。
//!
//! 设计要点：
//!
//! * 全局选项（`--json` / `--lang` / `--data-dir` / `--help` / `--version`）可以出现在命令行任何位置；
//! * 需要取值的选项既可以写 `--pid 1234`，也可以写 `--pid=1234`；
//! * 未知选项、缺少参数、多余的参数**一律报用法错误**（退出码 1），并打印对应命令的用法；
//! * 解析层不认识"业务语义"（比如某个游戏是否存在），只保证形状正确——语义错误由内核返回。

use std::collections::BTreeMap;
use std::path::PathBuf;

use gopt_core::{pick, Lang, PowerSchemeChoice, PriorityClass, RunHive};

/// 需要取值的长选项（其余长选项都是布尔开关）。
const VALUE_FLAGS: &[&str] = &[
    "lang",
    "data-dir",
    "pid",
    "exe",
    "set",
    "to",
    "limit",
    "kind",
    "interval",
    "duration",
    "out",
    "savepoints",
    "games-conf",
    "anchor-len",
    "anchor-hash",
    "rule",
    "game",
    "journal-id",
    "hive",
    "power-scheme",
];

/// 全部已知的长选项（未知选项**必须是用法错误**，而不是被当成"未知布尔开关"悄悄忽略）。
const KNOWN_FLAGS: &[&str] = &[
    "json",
    "lang",
    "data-dir",
    "help",
    "version",
    "yes",
    "all",
    "pending",
    "strict",
    "once",
    "pid",
    "exe",
    "set",
    "to",
    "limit",
    "kind",
    "interval",
    "duration",
    "out",
    "savepoints",
    "games-conf",
    "anchor-len",
    "anchor-hash",
    "rule",
    "game",
    "journal-id",
    "hive",
    "power-scheme",
];

/// 用法错误（退出码 1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError {
    /// 稳定英文消息。
    pub message: String,
    /// 相关命令（用于打印该命令的用法）。
    pub topic: Option<String>,
}

impl UsageError {
    fn new(message: impl Into<String>, topic: Option<String>) -> Self {
        Self {
            message: message.into(),
            topic,
        }
    }
}

/// `gopt list` 的列举对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    /// 可用策略（默认）。
    Games,
    /// 运行中的进程。
    Processes,
    /// 开机启动项。
    Startup,
}

/// `gopt startup` 的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAction {
    /// 列出启动项。
    List,
    /// 启用（把 `[disabled] ` 前缀去掉）。
    Enable(String),
    /// 禁用（改名迁移）。
    Disable(String),
}

/// `gopt watch` 选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchArgs {
    /// 轮询间隔（秒）。
    pub interval: u64,
    /// 总时长（秒，`0` = 一直跑）。
    pub duration: u64,
    /// 只跑一轮。
    pub once: bool,
}

/// 解析后的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `help [命令]`
    Help {
        /// 主题。
        topic: Option<String>,
    },
    /// `--version`
    Version,
    /// `status`
    Status,
    /// `list [games|processes|startup]`
    List(ListKind),
    /// `plan <游戏> [--pid N]`
    Plan {
        /// 游戏 id / 名称 / exe / pid。
        query: String,
        /// 显式进程。
        pid: Option<u32>,
    },
    /// `apply <游戏> [--pid N] [--yes]`
    Apply {
        /// 游戏 id / 名称 / exe / pid。
        query: String,
        /// 显式进程。
        pid: Option<u32>,
        /// 真正执行（无此开关时只预演）。
        yes: bool,
    },
    /// `rollback [--to N] [--all] [--pending] [--yes]`
    Rollback {
        /// 回滚到哪条记录（含）。
        to: Option<u64>,
        /// 撤销全部。
        all: bool,
        /// 只撤销尚未撤销的。
        pending: bool,
        /// 真正执行。
        yes: bool,
    },
    /// `journal [--kind K] [--limit N]`
    Journal {
        /// 只看某类记录。
        kind: Option<String>,
        /// 最多显示多少条。
        limit: Option<usize>,
    },
    /// `explain --game X | --rule X | --journal-id N`
    Explain {
        /// 游戏查询。
        game: Option<String>,
        /// 规则 id。
        rule: Option<String>,
        /// 审计记录 id。
        journal_id: Option<u64>,
    },
    /// `verify-journal [--anchor-len N --anchor-hash H] [--strict]`
    VerifyJournal {
        /// 锚点长度。
        anchor_len: Option<u64>,
        /// 锚点哈希。
        anchor_hash: Option<String>,
        /// 把"可修复的尾部半行"也算失败。
        strict: bool,
    },
    /// `watch [--interval S] [--duration S] [--once] [--yes]`
    Watch {
        /// 选项。
        watch: WatchArgs,
        /// 真正执行（无此开关时只报告）。
        yes: bool,
    },
    /// `prio [--pid N | --exe X] [--set CLASS] [--yes]`
    Prio {
        /// 进程 id。
        pid: Option<u32>,
        /// 进程名。
        exe: Option<String>,
        /// 目标优先级（缺省 = 只查询）。
        set: Option<PriorityClass>,
        /// 真正执行。
        yes: bool,
    },
    /// `tune [--power-scheme high|balanced] [--yes]`
    Tune {
        /// 目标方案（缺省 = 只查询）。
        scheme: Option<PowerSchemeChoice>,
        /// 真正执行。
        yes: bool,
    },
    /// `startup [list|enable NAME|disable NAME] [--hive hkcu|hklm] [--yes]`
    Startup {
        /// 动作。
        action: StartupAction,
        /// 注册表根。
        hive: RunHive,
        /// 真正执行。
        yes: bool,
    },
    /// `report [--out FILE]`
    Report {
        /// 文本报告落盘路径。
        out: Option<PathBuf>,
    },
    /// `import-legacy [--savepoints P] [--games-conf P] [--yes]`
    ImportLegacy {
        /// 旧快照路径。
        savepoints: Option<PathBuf>,
        /// 旧游戏配置路径。
        games_conf: Option<PathBuf>,
        /// 真正写入日志。
        yes: bool,
    },
}

/// 一次完整调用：命令 + 全局选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// 命令。
    pub command: Command,
    /// 是否输出 JSON。
    pub json: bool,
    /// 输出语言。
    pub lang: Lang,
    /// 数据目录（`--data-dir`）。
    pub data_dir: Option<PathBuf>,
}

/// 命令名（用于 `--json` 的 `command` 字段与用法提示）。
impl Command {
    /// 稳定命令名。
    pub fn name(&self) -> &'static str {
        match self {
            Command::Help { .. } => "help",
            Command::Version => "version",
            Command::Status => "status",
            Command::List(_) => "list",
            Command::Plan { .. } => "plan",
            Command::Apply { .. } => "apply",
            Command::Rollback { .. } => "rollback",
            Command::Journal { .. } => "journal",
            Command::Explain { .. } => "explain",
            Command::VerifyJournal { .. } => "verify-journal",
            Command::Watch { .. } => "watch",
            Command::Prio { .. } => "prio",
            Command::Tune { .. } => "tune",
            Command::Startup { .. } => "startup",
            Command::Report { .. } => "report",
            Command::ImportLegacy { .. } => "import-legacy",
        }
    }
}

/// 原始解析结果（第一遍扫描）。
#[derive(Debug, Default)]
struct Raw {
    flags: BTreeMap<String, Vec<String>>,
    booleans: BTreeMap<String, bool>,
    positionals: Vec<String>,
}

impl Raw {
    fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .get(name)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    fn has(&self, name: &str) -> bool {
        self.booleans.get(name).copied().unwrap_or(false)
    }
}

/// 解析命令行（`args` 不含程序名）。
pub fn parse(args: &[String]) -> Result<Invocation, UsageError> {
    let raw = scan(args)?;

    let lang = match raw.value("lang") {
        None => Lang::default_lang(),
        Some(text) => Lang::parse(text).ok_or_else(|| {
            UsageError::new(
                format!("`{text}` is not a language; expected zh or en"),
                None,
            )
        })?,
    };
    let json = raw.has("json");
    let data_dir = raw.value("data-dir").map(PathBuf::from);

    // `--version` 优先于命令；`--help` 打印用法。
    if raw.has("version") {
        return Ok(Invocation {
            command: Command::Version,
            json,
            lang,
            data_dir,
        });
    }
    if raw.has("help") {
        let topic = raw.positionals.first().cloned();
        return Ok(Invocation {
            command: Command::Help { topic },
            json,
            lang,
            data_dir,
        });
    }

    let mut positionals = raw.positionals.clone();
    let (name, rest) = match positionals.split_first() {
        Some((name, rest)) => (name.clone(), rest.to_vec()),
        None => {
            return Err(UsageError::new(
                "no command given; run `gopt --help` for the command list",
                None,
            ))
        }
    };
    positionals = rest;

    let command = match name.as_str() {
        "help" => Command::Help {
            topic: positionals.first().cloned(),
        },
        "version" => Command::Version,
        "status" => {
            require_no_positional(&name, &positionals)?;
            Command::Status
        }
        "list" => {
            let what = parse_list_kind(positionals.first().map(String::as_str))?;
            if positionals.len() > 1 {
                return Err(UsageError::new(
                    "too many arguments for `list`",
                    Some("list".to_string()),
                ));
            }
            Command::List(what)
        }
        "plan" => {
            let (query, pid) = single_query("plan", &positionals, &raw)?;
            Command::Plan { query, pid }
        }
        "apply" => {
            let (query, pid) = single_query("apply", &positionals, &raw)?;
            Command::Apply {
                query,
                pid,
                yes: raw.has("yes"),
            }
        }
        "rollback" => {
            require_no_positional("rollback", &positionals)?;
            Command::Rollback {
                to: parse_opt_u64("rollback", raw.value("to"))?,
                all: raw.has("all"),
                pending: raw.has("pending"),
                yes: raw.has("yes"),
            }
        }
        "journal" => {
            require_no_positional("journal", &positionals)?;
            Command::Journal {
                kind: raw.value("kind").map(str::to_string),
                limit: match raw.value("limit") {
                    Some(text) => Some(text.parse::<usize>().map_err(|_| {
                        UsageError::new(
                            format!("`{text}` is not a record count"),
                            Some("journal".to_string()),
                        )
                    })?),
                    None => None,
                },
            }
        }
        "explain" => {
            require_no_positional("explain", &positionals)?;
            Command::Explain {
                game: raw.value("game").map(str::to_string),
                rule: raw.value("rule").map(str::to_string),
                journal_id: parse_opt_u64("explain", raw.value("journal-id"))?,
            }
        }
        "verify-journal" | "verify" => {
            require_no_positional("verify-journal", &positionals)?;
            Command::VerifyJournal {
                anchor_len: parse_opt_u64("verify-journal", raw.value("anchor-len"))?,
                anchor_hash: raw.value("anchor-hash").map(str::to_string),
                strict: raw.has("strict"),
            }
        }
        "watch" => {
            require_no_positional("watch", &positionals)?;
            Command::Watch {
                watch: WatchArgs {
                    interval: parse_opt_u64("watch", raw.value("interval"))?
                        .unwrap_or(2)
                        .max(1),
                    duration: parse_opt_u64("watch", raw.value("duration"))?.unwrap_or(0),
                    once: raw.has("once"),
                },
                yes: raw.has("yes"),
            }
        }
        "prio" | "priority" => {
            require_no_positional("prio", &positionals)?;
            Command::Prio {
                pid: parse_opt_u64("prio", raw.value("pid"))?.map(|value| value as u32),
                exe: raw.value("exe").map(str::to_string),
                set: match raw.value("set") {
                    Some(text) => Some(PriorityClass::parse(text).map_err(|error| {
                        UsageError::new(
                            format!("invalid --set: {}", error.message()),
                            Some("prio".to_string()),
                        )
                    })?),
                    None => None,
                },
                yes: raw.has("yes"),
            }
        }
        "tune" => {
            require_no_positional("tune", &positionals)?;
            Command::Tune {
                scheme: match raw.value("power-scheme") {
                    Some(text) => Some(PowerSchemeChoice::parse(text).ok_or_else(|| {
                        UsageError::new(
                            format!("`{text}` is not a power scheme; use high or balanced"),
                            Some("tune".to_string()),
                        )
                    })?),
                    None => None,
                },
                yes: raw.has("yes"),
            }
        }
        "startup" => {
            let action = match positionals.first().map(String::as_str) {
                None | Some("list") => StartupAction::List,
                Some("enable") => {
                    StartupAction::Enable(positionals.get(1).cloned().ok_or_else(|| {
                        UsageError::new(
                            "`startup enable` needs an entry name",
                            Some("startup".to_string()),
                        )
                    })?)
                }
                Some("disable") => {
                    StartupAction::Disable(positionals.get(1).cloned().ok_or_else(|| {
                        UsageError::new(
                            "`startup disable` needs an entry name",
                            Some("startup".to_string()),
                        )
                    })?)
                }
                Some(other) => {
                    return Err(UsageError::new(
                        format!("`{other}` is not a startup action; use list, enable or disable"),
                        Some("startup".to_string()),
                    ))
                }
            };
            if positionals.len() > 2 {
                return Err(UsageError::new(
                    "too many arguments for `startup`",
                    Some("startup".to_string()),
                ));
            }
            Command::Startup {
                action,
                hive: match raw.value("hive") {
                    Some(text) => RunHive::parse(text).map_err(|error| {
                        UsageError::new(
                            format!("invalid --hive: {}", error.message()),
                            Some("startup".to_string()),
                        )
                    })?,
                    None => RunHive::CurrentUser,
                },
                yes: raw.has("yes"),
            }
        }
        "report" => {
            require_no_positional("report", &positionals)?;
            Command::Report {
                out: raw.value("out").map(PathBuf::from),
            }
        }
        "import-legacy" => {
            require_no_positional("import-legacy", &positionals)?;
            Command::ImportLegacy {
                savepoints: raw.value("savepoints").map(PathBuf::from),
                games_conf: raw.value("games-conf").map(PathBuf::from),
                yes: raw.has("yes"),
            }
        }
        other => return Err(UsageError::new(format!("unknown command `{other}`"), None)),
    };

    Ok(Invocation {
        command,
        json,
        lang,
        data_dir,
    })
}

/// 第一遍扫描：把所有 `--flag` 收集起来。
fn scan(args: &[String]) -> Result<Raw, UsageError> {
    let mut raw = Raw::default();
    let mut index = 0usize;
    while index < args.len() {
        let token = &args[index];
        if let Some(body) = token.strip_prefix("--") {
            let (name, inline) = match body.split_once('=') {
                Some((name, value)) => (name.to_string(), Some(value.to_string())),
                None => (body.to_string(), None),
            };
            if name.is_empty() {
                return Err(UsageError::new("`--` is not an option", None));
            }
            if !KNOWN_FLAGS.contains(&name.as_str()) {
                return Err(UsageError::new(
                    format!("unknown option `--{name}`; `gopt --help` lists every option"),
                    None,
                ));
            }
            if VALUE_FLAGS.contains(&name.as_str()) {
                let value = match inline {
                    Some(value) => value,
                    None => {
                        index += 1;
                        let Some(next) = args.get(index) else {
                            return Err(UsageError::new(format!("`--{name}` needs a value"), None));
                        };
                        if next.starts_with("--") {
                            return Err(UsageError::new(format!("`--{name}` needs a value"), None));
                        }
                        next.clone()
                    }
                };
                raw.flags.entry(name).or_default().push(value);
            } else {
                if inline.is_some() {
                    return Err(UsageError::new(
                        format!("`--{name}` does not take a value"),
                        None,
                    ));
                }
                raw.booleans.insert(name, true);
            }
        } else if token.starts_with('-') && token.len() > 1 {
            for ch in token.chars().skip(1) {
                let flag = match ch {
                    'h' => "help",
                    'V' | 'v' => "version",
                    'j' => "json",
                    'y' => "yes",
                    other => {
                        return Err(UsageError::new(
                            format!("unknown short option `-{other}`; long options are listed in `gopt --help`"),
                            None,
                        ))
                    }
                };
                raw.booleans.insert(flag.to_string(), true);
            }
        } else {
            raw.positionals.push(token.clone());
        }
        index += 1;
    }
    Ok(raw)
}

/// 解析 `list` 的列举对象（单独的辅助函数：让分支各自返回 `Result`，
/// 避免"长 `let` 绑定里再 `return`"那种 rustfmt 无法稳定的写法）。
fn parse_list_kind(what: Option<&str>) -> Result<ListKind, UsageError> {
    match what {
        None | Some("games") | Some("policies") => Ok(ListKind::Games),
        Some("processes") | Some("procs") => Ok(ListKind::Processes),
        Some("startup") => Ok(ListKind::Startup),
        Some(other) => Err(UsageError::new(
            format!("`{other}` is not something to list; use games, processes or startup"),
            Some("list".to_string()),
        )),
    }
}

/// `plan` / `apply` 共用的位置参数 + `--pid` 解析。
fn single_query(
    topic: &str,
    positionals: &[String],
    raw: &Raw,
) -> Result<(String, Option<u32>), UsageError> {
    if positionals.len() > 1 {
        return Err(UsageError::new(
            format!("`{topic}` takes exactly one game id / exe name / pid"),
            Some(topic.to_string()),
        ));
    }
    let query = match positionals.first() {
        Some(query) => query.clone(),
        None => match raw.value("game") {
            Some(query) => query.to_string(),
            None => {
                return Err(UsageError::new(
                    format!("`{topic}` needs a game id, an exe name or a pid"),
                    Some(topic.to_string()),
                ))
            }
        },
    };
    let pid = parse_opt_u64(topic, raw.value("pid"))?.map(|value| value as u32);
    Ok((query, pid))
}

/// 解析可选的无符号整数选项。
fn parse_opt_u64(topic: &str, value: Option<&str>) -> Result<Option<u64>, UsageError> {
    match value {
        None => Ok(None),
        Some(text) => text.parse::<u64>().map(Some).map_err(|_| {
            UsageError::new(
                format!("`{text}` is not a non-negative integer"),
                Some(topic.to_string()),
            )
        }),
    }
}

/// 该命令不接受位置参数。
fn require_no_positional(topic: &str, positionals: &[String]) -> Result<(), UsageError> {
    match positionals.first() {
        None => Ok(()),
        Some(extra) => Err(UsageError::new(
            format!("`{topic}` does not take the argument `{extra}`"),
            Some(topic.to_string()),
        )),
    }
}

/// 全局用法（`gopt --help`）。
pub fn usage(lang: Lang, topic: Option<&str>) -> String {
    if let Some(topic) = topic {
        return command_usage(lang, topic);
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n\n",
        pick(
            lang,
            "GameOptimizer-RS —— 游戏进程优化（默认只读，一切可回滚）",
            "GameOptimizer-RS — game process optimization (read-only by default, everything is reversible)"
        )
    ));
    out.push_str(&format!(
        "{} gopt [{}] <{}> [{}] [{}...]\n\n",
        pick(lang, "用法:", "usage:"),
        pick(lang, "全局选项", "global options"),
        pick(lang, "命令", "command"),
        pick(lang, "参数", "arguments"),
        pick(lang, "选项", "options")
    ));
    out.push_str(&format!("{}\n", pick(lang, "全局选项:", "global options:")));
    out.push_str(&format!(
        "  --json                 {}\n  --lang zh|en           {}\n  --data-dir <{}>     {}\n  -h, --help             {}\n  -V, --version          {}\n\n",
        pick(lang, "结构化 JSON 输出（含 schema_version）", "structured JSON output (includes schema_version)"),
        pick(lang, "输出语言（默认跟随 GOPT_LANG）", "output language (defaults to GOPT_LANG)"),
        pick(lang, "目录", "dir"),
        pick(
            lang,
            "数据目录（默认 %LOCALAPPDATA%\\GameOptimizer，也可用 GOPT_DATA_DIR）",
            "data directory (default %LOCALAPPDATA%\\GameOptimizer, or GOPT_DATA_DIR)"
        ),
        pick(lang, "本帮助", "this help"),
        pick(lang, "版本", "version")
    ));
    out.push_str(&format!("{}\n", pick(lang, "命令:", "commands:")));
    let rows: [(&str, &str, &str); 15] = [
        (
            "status",
            "机器/策略/审计链总览",
            "machine, policies and audit chain at a glance",
        ),
        (
            "list [games|processes|startup]",
            "列出策略、进程或启动项",
            "list policies, processes or startup entries",
        ),
        (
            "plan <游戏|exe|pid>",
            "只读预览执行计划（不改系统）",
            "read-only preview of the plan (changes nothing)",
        ),
        (
            "apply <游戏|exe|pid> [--yes]",
            "执行计划（无 --yes 时只预演）",
            "apply the plan (dry-run without --yes)",
        ),
        (
            "rollback [--to N|--all|--pending]",
            "从审计日志逆序撤销",
            "undo from the audit journal, newest first",
        ),
        (
            "journal [--kind K] [--limit N]",
            "查看审计记录",
            "show audit records",
        ),
        (
            "explain --game|--rule|--journal-id",
            "解释某个游戏/规则/记录",
            "explain a game, a rule or a record",
        ),
        (
            "verify-journal [--strict]",
            "校验哈希链（篡改可检出）",
            "verify the hash chain (detects tampering)",
        ),
        (
            "watch [--interval S] [--once]",
            "监控新出现的游戏进程",
            "watch for newly started game processes",
        ),
        (
            "prio [--pid N|--exe X] [--set C]",
            "读/改进程优先级（上限 high）",
            "read or set a process priority (capped at high)",
        ),
        (
            "tune [--power-scheme high|balanced]",
            "查询/切换电源方案",
            "query or switch the power scheme",
        ),
        (
            "startup [list|enable|disable NAME]",
            "启用/禁用开机启动项",
            "enable or disable a startup entry",
        ),
        (
            "report [--out FILE]",
            "生成体检报告",
            "generate a health report",
        ),
        (
            "import-legacy [--yes]",
            "只读导入 C++ v1.1.0 旧格式",
            "import the C++ v1.1.0 legacy format (read-only)",
        ),
        ("help [命令]", "命令用法", "usage of one command"),
    ];
    for (name, zh, en) in rows {
        out.push_str(&format!("  {:<38} {}\n", name, pick(lang, zh, en)));
    }
    out.push_str(&format!(
        "\n{}\n  {} — 0 {} / 1 {} / 2 {} / 3 {}\n",
        pick(
            lang,
            "默认安全: apply / rollback / tune / startup / prio / import-legacy 没有 --yes 时只预演。",
            "safe by default: apply / rollback / tune / startup / prio / import-legacy only dry-run without --yes."
        ),
        pick(lang, "退出码", "exit codes"),
        pick(lang, "成功", "ok"),
        pick(lang, "用法错误", "usage"),
        pick(lang, "环境不满足", "environment"),
        pick(lang, "审计链校验失败", "audit chain")
    ));
    out
}

/// 单个命令的用法。
pub fn command_usage(lang: Lang, topic: &str) -> String {
    let (usage_line, zh, en) = match topic {
        "status" => ("gopt status [--json]", "机器/策略/审计链总览", "machine, policies and audit chain"),
        "list" => (
            "gopt list [games|processes|startup] [--json]",
            "列出策略、进程或启动项",
            "list policies, processes or startup entries",
        ),
        "plan" => (
            "gopt plan <游戏|exe|pid> [--pid N] [--json]",
            "只读预览：显示会执行哪些步骤、为什么",
            "read-only preview: shows the steps and why",
        ),
        "apply" => (
            "gopt apply <游戏|exe|pid> [--pid N] [--yes] [--json]",
            "执行计划；无 --yes 时只预演，并说明会改什么",
            "apply the plan; without --yes it only previews",
        ),
        "rollback" => (
            "gopt rollback [--to N] [--all] [--pending] [--yes] [--json]",
            "从审计日志逆序撤销；无 --yes 时只生成计划",
            "undo newest-first from the audit journal; without --yes it only plans",
        ),
        "journal" => (
            "gopt journal [--kind apply|rollback|imported] [--limit N] [--json]",
            "查看审计记录（只读）",
            "show audit records (read-only)",
        ),
        "explain" => (
            "gopt explain (--game ID | --rule ID | --journal-id N) [--json]",
            "解释某个游戏/规则/审计记录",
            "explain a game, a rule or an audit record",
        ),
        "verify-journal" => (
            "gopt verify-journal [--anchor-len N --anchor-hash H] [--strict] [--json]",
            "校验哈希链；退出码 3 表示链被篡改或损坏",
            "verify the hash chain; exit code 3 means it is broken",
        ),
        "watch" => (
            "gopt watch [--interval S] [--duration S] [--once] [--yes] [--json]",
            "监控新出现的游戏进程并按策略优化（无 --yes 时只报告）",
            "watch for game processes and optimize them (reports only without --yes)",
        ),
        "prio" => (
            "gopt prio [--pid N | --exe NAME] [--set idle|below-normal|normal|above-normal|high] [--yes]",
            "读取或设置进程优先级（红线：上限 high，永不 REALTIME）",
            "read or set a process priority (capped at high, never REALTIME)",
        ),
        "tune" => (
            "gopt tune [--power-scheme high|balanced] [--yes]",
            "查询或切换电源方案",
            "query or switch the power scheme",
        ),
        "startup" => (
            "gopt startup [list | enable NAME | disable NAME] [--hive hkcu|hklm] [--yes]",
            "列出/启用/禁用开机启动项（禁用 = 改名迁移，可回滚）",
            "list/enable/disable startup entries (disable renames the value, reversible)",
        ),
        "report" => (
            "gopt report [--out FILE] [--json]",
            "生成体检报告（含硬件、策略、日志）",
            "generate a health report (hardware, policies, journal)",
        ),
        "import-legacy" => (
            "gopt import-legacy [--savepoints P] [--games-conf P] [--yes]",
            "只读解析 C++ v1.1.0 的 savepoints.txt / games.conf，--yes 时入链",
            "read the C++ v1.1.0 savepoints.txt / games.conf; with --yes it appends to the chain",
        ),
        "help" => ("gopt help [命令]", "帮助", "help"),
        "version" => ("gopt --version", "版本", "version"),
        other => {
            return format!(
                "unknown command `{other}`\n\n{}",
                usage(lang, None)
            )
        }
    };
    format!("{usage_line}\n  {}\n", pick(lang, zh, en))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Invocation, UsageError> {
        let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        parse(&owned)
    }

    #[test]
    fn global_options_can_appear_anywhere() {
        let invocation = parse_args(&["--json", "status"]).expect("status");
        assert!(invocation.json);
        assert_eq!(invocation.command, Command::Status);
        assert_eq!(invocation.lang, Lang::Zh);

        let invocation = parse_args(&["status", "--json", "--lang=en"]).expect("status en");
        assert!(invocation.json);
        assert_eq!(invocation.lang, Lang::En);
    }

    #[test]
    fn default_safety_flags_are_off() {
        let invocation = parse_args(&["apply", "cs2"]).expect("apply");
        match invocation.command {
            Command::Apply { yes, query, pid } => {
                assert!(!yes);
                assert_eq!(query, "cs2");
                assert_eq!(pid, None);
            }
            other => panic!("unexpected {other:?}"),
        }
        let invocation =
            parse_args(&["apply", "cs2", "--yes", "--pid", "1234"]).expect("apply yes");
        match invocation.command {
            Command::Apply { yes, pid, .. } => {
                assert!(yes);
                assert_eq!(pid, Some(1234));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(
            parse_args(&["version"]).expect("v").command,
            Command::Version
        );
        assert_eq!(
            parse_args(&["--version"]).expect("v").command,
            Command::Version
        );
        assert_eq!(parse_args(&["-V"]).expect("v").command, Command::Version);
        assert!(matches!(
            parse_args(&["help", "apply"]).expect("help").command,
            Command::Help { topic: Some(topic) } if topic == "apply"
        ));
        assert_eq!(
            parse_args(&["list"]).expect("list").command,
            Command::List(ListKind::Games)
        );
        assert_eq!(
            parse_args(&["list", "startup"]).expect("list").command,
            Command::List(ListKind::Startup)
        );
        assert!(matches!(
            parse_args(&["plan", "cs2"]).expect("plan").command,
            Command::Plan { .. }
        ));
        assert!(matches!(
            parse_args(&["rollback", "--to", "3", "-y"])
                .expect("rollback")
                .command,
            Command::Rollback {
                to: Some(3),
                yes: true,
                ..
            }
        ));
        assert!(matches!(
            parse_args(&["rollback", "--pending"])
                .expect("rollback")
                .command,
            Command::Rollback { pending: true, .. }
        ));
        assert!(matches!(
            parse_args(&["journal", "--kind", "apply", "--limit=5"]).expect("journal").command,
            Command::Journal { kind: Some(kind), limit: Some(5) } if kind == "apply"
        ));
        assert!(matches!(
            parse_args(&["explain", "--rule", "priority"]).expect("explain").command,
            Command::Explain { rule: Some(rule), .. } if rule == "priority"
        ));
        assert!(matches!(
            parse_args(&[
                "verify-journal",
                "--strict",
                "--anchor-len",
                "3",
                "--anchor-hash",
                "aa"
            ])
            .expect("verify")
            .command,
            Command::VerifyJournal {
                strict: true,
                anchor_len: Some(3),
                ..
            }
        ));
        assert!(matches!(
            parse_args(&["watch", "--once", "--interval", "1"])
                .expect("watch")
                .command,
            Command::Watch {
                watch: WatchArgs {
                    once: true,
                    interval: 1,
                    ..
                },
                yes: false
            }
        ));
        assert!(matches!(
            parse_args(&["prio", "--set", "high", "--exe", "cs2.exe"])
                .expect("prio")
                .command,
            Command::Prio {
                set: Some(PriorityClass::High),
                ..
            }
        ));
        assert!(matches!(
            parse_args(&["tune", "--power-scheme", "high", "--yes"])
                .expect("tune")
                .command,
            Command::Tune {
                scheme: Some(PowerSchemeChoice::High),
                yes: true
            }
        ));
        assert!(matches!(
            parse_args(&["startup", "disable", "Discord", "--yes"]).expect("startup").command,
            Command::Startup { action: StartupAction::Disable(name), yes: true, .. } if name == "Discord"
        ));
        assert!(matches!(
            parse_args(&["report", "--out", "r.txt"]).expect("report").command,
            Command::Report { out: Some(path) } if path.to_string_lossy() == "r.txt"
        ));
        assert!(matches!(
            parse_args(&["import-legacy", "--yes"])
                .expect("import")
                .command,
            Command::ImportLegacy { yes: true, .. }
        ));
        assert!(matches!(
            parse_args(&["--data-dir", "C:\\tmp\\gopt", "status"])
                .expect("dir")
                .command,
            Command::Status
        ));
    }

    #[test]
    fn realtime_priority_is_rejected_at_parse_time() {
        let error = parse_args(&["prio", "--set", "realtime"]).expect_err("realtime");
        assert!(error.message.contains("realtime"), "{}", error.message);
    }

    #[test]
    fn usage_errors_are_actionable() {
        assert!(parse_args(&[])
            .expect_err("no command")
            .message
            .contains("no command"));
        assert!(parse_args(&["nope"])
            .expect_err("unknown")
            .message
            .contains("unknown command"));
        assert!(
            parse_args(&["plan"])
                .expect_err("no query")
                .topic
                .as_deref()
                == Some("plan")
        );
        assert!(parse_args(&["plan", "a", "b"])
            .expect_err("too many")
            .message
            .contains("exactly one"));
        assert!(parse_args(&["status", "extra"])
            .expect_err("extra")
            .message
            .contains("does not take"));
        assert!(parse_args(&["--lang", "de", "status"])
            .expect_err("lang")
            .message
            .contains("zh or en"));
        assert!(parse_args(&["--json=1", "status"])
            .expect_err("value")
            .message
            .contains("does not take a value"));
        assert!(parse_args(&["-Z", "status"])
            .expect_err("short")
            .message
            .contains("unknown short option"));
        assert!(parse_args(&["journal", "--limit", "x"])
            .expect_err("limit")
            .message
            .contains("record count"));
        assert!(parse_args(&["--pid"])
            .expect_err("missing value")
            .message
            .contains("needs a value"));
    }

    #[test]
    fn usage_texts_are_bilingual_and_list_every_command() {
        let zh = usage(Lang::Zh, None);
        let en = usage(Lang::En, None);
        assert!(zh.contains("默认安全"));
        assert!(en.contains("safe by default"));
        for command in [
            "status",
            "list",
            "plan",
            "apply",
            "rollback",
            "journal",
            "explain",
            "verify-journal",
            "watch",
            "prio",
            "tune",
            "startup",
            "report",
            "import-legacy",
        ] {
            assert!(zh.contains(command), "help must mention {command}");
            assert!(!command_usage(Lang::Zh, command).is_empty());
        }
        assert!(command_usage(Lang::En, "apply").contains("gopt apply"));
        assert!(command_usage(Lang::Zh, "nope").contains("unknown command"));
    }
}
