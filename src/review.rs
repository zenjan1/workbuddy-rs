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

    let system = cfg
        .reviewer_agent
        .clone()
        .unwrap_or_else(|| DEFAULT_REVIEWER_SYSTEM.to_string());

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
            mock: true,
            reviewer_agent: None,
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