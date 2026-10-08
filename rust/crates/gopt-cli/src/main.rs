//! `gopt` —— GameOptimizer-RS 命令行前端（单内核多前端里的 CLI 前端）。
//!
//! ```text
//! gopt [--json] [--lang zh|en] [--data-dir DIR] <命令> [参数] [选项]
//! ```
//!
//! 退出码（与内核 [`gopt_core::CoreError::exit_code`] 一致）：
//!
//! | 码 | 含义 |
//! | --- | --- |
//! | 0 | 成功（含"预演成功"：没有 `--yes` 时只打印计划） |
//! | 1 | 用法错误（未知命令/选项、缺参数、取值非法） |
//! | 2 | 环境不满足（目标不存在、未提权、系统不支持、文件不可写…） |
//! | 3 | 审计链校验失败（日志被篡改/损坏） |
//!
//! 输出约定：成功与运行期失败的**文本**都走 stdout（失败时是"错误 + 建议"，不是堆栈）；
//! 纯用法错误走 stderr（那里没有结构化结果可给）。`--json` 一律走 stdout。
//!
//! 红线落地：本 crate 不含任何系统调用（不依赖 `windows`、不依赖 `gopt-hal`），
//! 只有 `gopt-core` 一个依赖 —— "前端绕过内核直接改系统"在依赖图上就不可能。

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
// 测试代码允许 unwrap/expect/panic：测试失败必须显式炸出来。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod args;
mod run;

use std::io::Write as _;

use gopt_core::{pick, CoreError, Lang};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // 先按"猜测语言"解析一次，以便用法错误也是双语的（真正的语言在解析成功后确定）。
    let code = match args::parse(&argv) {
        Ok(invocation) => {
            let output = run::execute(invocation);
            if invocation_json(&argv) {
                println!("{}", output.json.trim_end());
            } else {
                print!("{}", output.text);
            }
            output.code
        }
        Err(error) => {
            let lang = guess_lang(&argv);
            let text = format!(
                "{}: {}\n\n{}",
                pick(lang, "用法错误", "usage error"),
                error.message,
                args::usage(lang, error.topic.as_deref())
            );
            if wants_json(&argv) {
                let core_error = CoreError::usage("gopt::parse_args", error.message.clone());
                let outcome: gopt_core::Outcome<()> =
                    gopt_core::Outcome::failed("usage", lang, core_error);
                println!("{}", outcome.to_json_pretty());
            } else {
                let _ = std::io::stderr().write_all(text.as_bytes());
            }
            1
        }
    };
    std::process::exit(code);
}

/// `--json` 是否出现（用法错误路径需要在没有 `Invocation` 的情况下判断）。
fn wants_json(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "--json" || arg == "-j")
}

/// 解析成功时用的 JSON 判断（与 `run::execute` 内部保持一致）。
fn invocation_json(argv: &[String]) -> bool {
    wants_json(argv)
}

/// 从命令行里猜语言（用法错误发生在 `Lang` 解析成功之前）。
fn guess_lang(argv: &[String]) -> Lang {
    let mut index = 0usize;
    while index < argv.len() {
        let token = &argv[index];
        if let Some(value) = token.strip_prefix("--lang=") {
            if let Some(lang) = Lang::parse(value) {
                return lang;
            }
        } else if token == "--lang" {
            if let Some(value) = argv.get(index + 1).and_then(|value| Lang::parse(value)) {
                return value;
            }
        }
        index += 1;
    }
    Lang::default_lang()
}
