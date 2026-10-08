//! exe 名通配匹配（大小写不敏感，`*` / `?`）。
//!
//! 刻意不引入正则引擎：策略文件里的 `match` 只需要"exe 名通配"这一种能力，
//! 自己实现既避免依赖膨胀，也能保证匹配是线性时间（不会出现灾难性回溯）。

/// 通配匹配：`*` 匹配任意长度（含空），`?` 匹配任意单个字符，大小写不敏感。
///
/// ```
/// use gopt_policy::wildcard_match;
///
/// assert!(wildcard_match("cs2.exe", "CS2.EXE"));
/// assert!(wildcard_match("*.exe", "DeltaForceClient-Win64-Shipping.exe"));
/// assert!(wildcard_match("r?apex.exe", "r5apex.exe"));
/// assert!(!wildcard_match("cs2.exe", "csgo.exe"));
/// ```
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    // 先做 ASCII 小写归一：exe 名是 ASCII，这样比较时不必反复分配。
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();

    let mut p = 0usize;
    let mut t = 0usize;
    let mut star: Option<usize> = None;
    let mut resume = 0usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if let Some(star_index) = star {
            // 回退到上一个 `*`，让它多吞一个字符。
            p = star_index + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// 校验 `match` / `exe_aliases` 里的模式；返回稳定英文错误消息（调用方补文件名/行号）。
///
/// 拒绝：空模式、含路径分隔符（`match` 是 exe 名而不是路径）、含控制字符、以及裸 `*`
/// （那会匹配所有进程，属于危险配置）。
pub fn validate_exe_pattern(pattern: &str) -> Result<(), String> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return Err("exe pattern must not be empty".to_string());
    }
    if trimmed != pattern {
        return Err(format!(
            "exe pattern `{pattern}` must not have leading or trailing whitespace"
        ));
    }
    if pattern.contains(['\\', '/']) {
        return Err(format!(
            "`{pattern}` looks like a path: `match` takes an executable file name (wildcards `*` and `?` allowed)"
        ));
    }
    if pattern.chars().any(char::is_control) {
        return Err(format!(
            "exe pattern `{pattern}` contains a control character"
        ));
    }
    if pattern == "*" {
        return Err(
            "`match = \"*\"` would apply this policy to every running process; name the executable instead"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_case_insensitive() {
        assert!(wildcard_match("cs2.exe", "cs2.exe"));
        assert!(wildcard_match("cs2.exe", "CS2.EXE"));
        assert!(wildcard_match("Cs2.Exe", "cs2.exe"));
        assert!(!wildcard_match("cs2.exe", "cs2.exe.bak"));
        assert!(!wildcard_match("cs2.exe", "csgo.exe"));
    }

    #[test]
    fn wildcards_cover_the_builtin_patterns() {
        assert!(wildcard_match(
            "DeltaForceClient-Win64-Shipping.exe",
            "DeltaForceClient-Win64-Shipping.exe"
        ));
        assert!(wildcard_match("*.exe", "VALORANT-Win64-Shipping.exe"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("r?apex.exe", "r5apex.exe"));
        assert!(wildcard_match("*apex*", "r5apex.exe"));
        assert!(!wildcard_match("r?apex.exe", "r55apex.exe"));
        assert!(!wildcard_match("*.dll", "r5apex.exe"));
    }

    #[test]
    fn matching_is_linear_on_pathological_input() {
        // 经典回溯陷阱：多个 `*` + 长文本。这里的实现是迭代 + 单星回退，不会爆炸。
        let pattern = "*a*a*a*a*a*a*a*a*a*b";
        let text = "a".repeat(400);
        assert!(!wildcard_match(pattern, &text));
        assert!(wildcard_match(pattern, &format!("{}b", "a".repeat(400))));
    }

    #[test]
    fn pattern_validation_is_strict() {
        assert!(validate_exe_pattern("cs2.exe").is_ok());
        assert!(validate_exe_pattern("DeltaForceClient-Win64-Shipping.exe").is_ok());
        assert!(validate_exe_pattern("*.exe").is_ok());
        assert!(validate_exe_pattern("cs?.exe").is_ok());

        assert!(validate_exe_pattern("").is_err());
        assert!(validate_exe_pattern("  ").is_err());
        assert!(validate_exe_pattern(" cs2.exe").is_err());
        assert!(validate_exe_pattern(r"C:\Games\cs2.exe").is_err());
        assert!(validate_exe_pattern("games/cs2.exe").is_err());
        assert!(validate_exe_pattern("*").is_err());
        assert!(validate_exe_pattern("cs\n2.exe").is_err());
    }
}
