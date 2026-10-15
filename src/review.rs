use std::future::Future;
use std::pin::Pin;
use std::process::Command;

use anyhow::{Context, Result};

use crate::config::Config;

const DEFAULT_REVIEWER_SYSTEM: &str = "You are WorkBuddy's code review expert. Review the given git diff for correctness, readability, performance, security, and maintainability. Provide concise, actionable feedback.";

pub trait LlmClient: Send + Sync {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}

impl<T: crate::llm::Llm> LlmClient for T {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        crate::llm::Llm::chat(self, system, user)
    }
}

pub fn collect_diff() -> Result<String> {
    let commands: &[&[&str]] = &[
        &["diff", "--cached"],
        &["diff"],
        &["diff", "HEAD^", "HEAD"],
    ];

    for args in commands {
        let output = Command::new("git")
            .args(*args)
            .output()
            .with_context(|| format!("failed to run git {}", args.join(" ")))?;
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if !stdout.trim().is_empty() {
                return Ok(stdout);
            }
        }
    }

    let output = Command::new("git")
        .args(["show", "--format=", "--no-color", "--patch", "HEAD"])
        .output()
        .context("failed to run git show")?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !stdout.trim().is_empty() {
            return Ok(stdout);
        }
    }

    Ok(String::new())
}

pub async fn review_changes<C: LlmClient>(
    client: &C,
    cfg: &Config,
    diff: &str,
) -> Result<Option<String>> {
    if diff.trim().is_empty() {
        return Ok(None);
    }

    let _ = cfg;
    let system = DEFAULT_REVIEWER_SYSTEM.to_string();

    let user = format!(
        "Please review the following git diff.\n\n```diff\n{}\n```",
        diff
    );

    let review = client.chat(&system, &user).await?;
    Ok(Some(review))
}

pub async fn run_review<C: LlmClient>(client: &C, cfg: &Config) -> Result<()> {
    let diff = collect_diff()?;
    match review_changes(client, cfg, &diff).await? {
        Some(review) => {
            println!("{}", review);
        }
        None => {
            println!("No changes to review.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockLlm {
        calls: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl MockLlm {
        fn new(calls: Arc<Mutex<Vec<(String, String)>>>) -> Self {
            Self { calls }
        }
    }

    impl LlmClient for MockLlm {
        fn chat<'a>(
            &'a self,
            system: &'a str,
            user: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            let calls = self.calls.clone();
            let system = system.to_string();
            let user = user.to_string();
            Box::pin(async move {
                calls.lock().unwrap().push((system, user));
                Ok("mock review text".to_string())
            })
        }
    }

    fn test_config() -> Config {
        Config {
            api_key: None,
            base_url: "http://localhost".to_string(),
            model: "test-model".to_string(),
            max_parallel: 1,
            workspace_dir: std::env::temp_dir(),
            repo_dir: std::env::temp_dir(),
            request_timeout_secs: 30,
            price: 0.0,
            max_budget: 0.0,
            cost_per_1k_tokens: 0.0,
            mock: true,
            webhook_url: None,
            webhook_retry: 0,
            webhook_timeout_secs: 5,
        }
    }

    #[tokio::test]
    async fn review_changes_non_empty_diff_calls_llm_and_returns_text() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mock = MockLlm::new(calls.clone());
        let cfg = test_config();

        let diff = "diff --git a/foo.rs b/foo.rs\n+// new line";
        let result = review_changes(&mock, &cfg, diff).await.unwrap();
        assert_eq!(result.as_deref(), Some("mock review text"));

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (_, user) = &calls[0];
        assert!(user.contains(diff));
    }

    #[tokio::test]
    async fn review_changes_empty_diff_skips_llm() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mock = MockLlm::new(calls.clone());
        let cfg = test_config();

        let result = review_changes(&mock, &cfg, "").await.unwrap();
        assert!(result.is_none());
        assert_eq!(calls.lock().unwrap().len(), 0);
    }
}

pub async fn review(_cfg: crate::config::Config) -> anyhow::Result<()> {
    println!("review 子命令暂未实现");
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub rule: String,
    pub file: String,
    pub line: Option<usize>,
    pub severity: Severity,
    pub message: String,
}

impl Violation {
    pub fn new(rule: impl Into<String>, file: impl Into<String>, line: Option<usize>, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            rule: rule.into(),
            file: file.into(),
            line,
            severity,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub added_lines: Vec<(usize, String)>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReviewReport {
    pub violations: Vec<Violation>,
}

impl ReviewReport {
    pub fn new() -> Self {
        Self { violations: Vec::new() }
    }

    pub fn add(&mut self, violation: Violation) {
        self.violations.push(violation);
    }

    pub fn extend(&mut self, other: Vec<Violation>) {
        self.violations.extend(other);
    }

    pub fn is_empty(&self) -> bool {
        self.violations.is_empty()
    }
}

pub trait Rule: Send + Sync {
    fn name(&self) -> &str;
    fn check(&self, files: &[FileDiff]) -> Vec<Violation>;
}

pub struct TodoRule;

impl Rule for TodoRule {
    fn name(&self) -> &str { "todo" }

    fn check(&self, files: &[FileDiff]) -> Vec<Violation> {
        let mut violations = Vec::new();
        for file in files {
            for (line_num, content) in &file.added_lines {
                let trimmed = content.trim();
                if trimmed.contains("TODO") || trimmed.contains("FIXME") {
                    violations.push(Violation::new(
                        self.name(),
                        file.path.clone(),
                        Some(*line_num),
                        Severity::Warning,
                        format!("Added TODO/FIXME: {}", trimmed),
                    ));
                }
            }
        }
        violations
    }
}

pub struct MissingTestRule;

impl Rule for MissingTestRule {
    fn name(&self) -> &str { "missing-test" }

    fn check(&self, files: &[FileDiff]) -> Vec<Violation> {
        let mut violations = Vec::new();
        let file_paths: std::collections::HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
        for file in files {
            let path = &file.path;
            if let Some(rest) = path.strip_prefix("src/") {
                if rest.ends_with("_test.rs") || rest.ends_with("test.rs") {
                    continue;
                }
                let expected = expected_test_path(rest);
                if !file_paths.contains(expected.as_str()) {
                    violations.push(Violation::new(
                        self.name(),
                        path.clone(),
                        None,
                        Severity::Warning,
                        format!("新增源文件 {} 但未同时添加对应测试文件 {}", path, expected),
                    ));
                }
            }
        }
        violations
    }
}

fn expected_test_path(rest: &str) -> String {
    let path = std::path::Path::new(rest);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = path.extension().unwrap_or_default().to_string_lossy().to_string();
    let parent = path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let new_file_name = format!("{}_test.{}", stem, ext);
    if parent.is_empty() {
        format!("tests/{}", new_file_name)
    } else {
        format!("tests/{}/{}", parent, new_file_name)
    }
}

pub struct FormatRule;

impl Rule for FormatRule {
    fn name(&self) -> &str { "format" }

    fn check(&self, files: &[FileDiff]) -> Vec<Violation> {
        let mut violations = Vec::new();
        const MAX_LINE_LEN: usize = 100;
        for file in files {
            for (line_num, content) in &file.added_lines {
                if content.len() > MAX_LINE_LEN {
                    violations.push(Violation::new(
                        self.name(),
                        file.path.clone(),
                        Some(*line_num),
                        Severity::Warning,
                        format!("行长度超过 {} 字符 (实际 {} 字符)", MAX_LINE_LEN, content.len()),
                    ));
                }
                if content.ends_with(' ') || content.ends_with('\t') {
                    violations.push(Violation::new(
                        self.name(),
                        file.path.clone(),
                        Some(*line_num),
                        Severity::Warning,
                        "行尾存在空白字符".to_string(),
                    ));
                }
            }
        }
        violations
    }
}

pub fn default_rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(TodoRule),
        Box::new(MissingTestRule),
        Box::new(FormatRule),
    ]
}

pub fn parse_diff(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut new_line = 1usize;

    for raw in diff.lines() {
        if raw.starts_with("+++ b/") {
            let path = raw.strip_prefix("+++ b/").unwrap_or_default().split('\t').next().unwrap_or_default().to_string();
            if let Some(f) = current.replace(FileDiff { path: path.clone(), added_lines: Vec::new() }) {
                files.push(f);
            }
            new_line = 1;
            continue;
        }
        if raw.starts_with("+++ a/") {
            let path = raw.strip_prefix("+++ a/").unwrap_or_default().split('\t').next().unwrap_or_default().to_string();
            if let Some(f) = current.replace(FileDiff { path: path.clone(), added_lines: Vec::new() }) {
                files.push(f);
            }
            new_line = 1;
            continue;
        }
        if raw.starts_with("@@") {
            if let Some(rest) = raw.split('+').nth(1) {
                if let Some(num) = rest.split(',').next().and_then(|s| s.trim_start().parse::<usize>().ok()) {
                    new_line = num.max(1);
                }
            }
            continue;
        }
        if raw.starts_with('+') && !raw.starts_with("+++") {
            if let Some(f) = current.as_mut() {
                let content = raw[1..].to_string();
                f.added_lines.push((new_line, content));
                new_line += 1;
            }
            continue;
        }
        if raw.starts_with('-') && !raw.starts_with("---") {
            continue;
        }
        if raw.starts_with(' ') {
            if current.is_some() {
                new_line += 1;
            }
        }
    }
    if let Some(f) = current {
        files.push(f);
    }
    files
}

pub fn run_rules_against_diff(diff: &str) -> ReviewReport {
    let files = parse_diff(diff);
    run_rules(&files)
}

pub fn run_rules(files: &[FileDiff]) -> ReviewReport {
    let mut report = ReviewReport::new();
    for rule in default_rules() {
        let violations = rule.check(files);
        report.extend(violations);
    }
    report
}

#[cfg(test)]
mod tests_local_rules {
    use super::*;

    fn file(path: &str, lines: Vec<(usize, &str)>) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            added_lines: lines.into_iter().map(|(n, s)| (n, s.to_string())).collect(),
        }
    }

    #[test]
    fn todo_rule_detects_todo_in_added_line() {
        let files = vec![file("src/foo.rs", vec![(1, "fn main() {"), (2, "// TODO: implement")])];
        let rule = TodoRule;
        let violations = rule.check(&files);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "todo");
        assert_eq!(violations[0].file, "src/foo.rs");
        assert_eq!(violations[0].line, Some(2));
        assert_eq!(violations[0].severity, Severity::Warning);
    }

    #[test]
    fn missing_test_rule_flags_src_file_without_test() {
        let files = vec![file("src/foo.rs", vec![(1, "fn main() {}")])];
        let rule = MissingTestRule;
        let violations = rule.check(&files);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "missing-test");
        assert_eq!(violations[0].file, "src/foo.rs");
        assert_eq!(violations[0].line, None);
    }

    #[test]
    fn missing_test_rule_does_not_flag_when_test_exists() {
        let files = vec![
            file("src/foo.rs", vec![(1, "fn main() {}")]),
            file("tests/foo_test.rs", vec![(1, "fn test() {}")]),
        ];
        let rule = MissingTestRule;
        let violations = rule.check(&files);
        assert!(violations.is_empty());
    }

    #[test]
    fn format_rule_detects_trailing_whitespace_and_long_line() {
        let long_line = "a".repeat(101);
        let files = vec![file("src/foo.rs", vec![
            (1, "let x = 1; "),
            (2, &long_line),
        ])];
        let rule = FormatRule;
        let violations = rule.check(&files);
        assert!(violations.len() >= 2);
        assert!(violations.iter().any(|v| v.message.contains("行尾存在空白字符")));
        assert!(violations.iter().any(|v| v.message.contains("行长度超过")));
    }

    #[test]
    fn run_rules_aggregates_violations() {
        let files = vec![
            file("src/foo.rs", vec![(1, "// TODO: fix"), (2, "let x = 1; ")])
        ];
        let report = run_rules(&files);
        assert!(!report.is_empty());
        assert!(report.violations.iter().any(|v| v.rule == "todo"));
        assert!(report.violations.iter().any(|v| v.rule == "format"));
        assert!(report.violations.iter().any(|v| v.rule == "missing-test"));
    }
}