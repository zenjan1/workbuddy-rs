use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use crate::llm::LlmClient;
use crate::config::Config;

const REVIEWER_SYSTEM: &str = "You are a rigorous code reviewer. Given a task description and a git diff, produce a structured review in JSON with keys: risk_level (low/medium/high), suggestions (array of strings), key_issues (array of strings).";



/// Configuration for the reviewer agent, extracted from the main Config.
#[derive(Debug, Clone)]
pub struct ReviewerConfig {
    pub agent: String,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl ReviewerConfig {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            agent: "reviewer".to_string(),
            model: Some(cfg.model.clone()),
            base_url: Some(cfg.base_url.clone()),
            api_key: cfg.api_key.clone(),
            temperature: None,
            max_tokens: None,
        }
    }
}

/// Review a git diff using the configured reviewer agent.
/// Returns a structured ReviewResult with summary, suggestions, and blocking flag.
pub async fn review_diff<C: LlmClient>(diff: &str, cfg: &ReviewerConfig, client: &C) -> Result<ReviewResult> {
    if diff.trim().is_empty() {
        return Ok(ReviewResult {
            risk_level: "low".to_string(),
            suggestions: vec![],
            key_issues: vec![],
            summary: "No changes to review.".to_string(),
            blocking: false,
            raw: String::new(),
        });
    }

    let user_prompt = format!(
        "Task: Review the following git diff for code quality, correctness, and potential issues.\n\nDiff:\n```\n{}```",
        diff
    );

    let response = client
        .chat(REVIEWER_SYSTEM, &user_prompt)
        .await
        .context("Failed to call reviewer agent")?;

    // Parse the JSON response
    let review: ReviewResult = serde_json::from_str(&response)
        .context(format!("Failed to parse reviewer response as JSON: {}", response))?;

    // Determine if blocking based on risk level
    let blocking = matches!(review.risk_level.as_str(), "high" | "critical");

    Ok(ReviewResult {
        blocking,
        summary: review.summary.clone(),
        raw: review.raw.clone(),
        risk_level: review.risk_level.clone(),
        suggestions: review.suggestions.clone(),
        key_issues: review.key_issues.clone(),
    })
}



/// ReviewerAgent 封装代码审查逻辑，持有 LLM 客户端与配置
#[derive(Debug)]
pub struct ReviewerAgent<C: LlmClient> {
    client: C,
    run_log_path: std::path::PathBuf,
    timeout_secs: u64,
}

impl<C: LlmClient> ReviewerAgent<C> {
    /// 创建新的 ReviewerAgent
    pub fn new(client: C, run_log_path: impl Into<std::path::PathBuf>, timeout_secs: u64) -> Self {
        Self {
            client,
            run_log_path: run_log_path.into(),
            timeout_secs,
        }
    }

    /// 执行代码审查：读取暂存区 diff，调用 LLM，解析结果，按 severity 分类
    /// 返回 Ok(ReviewResult) 表示审查完成（可能包含 blocker），Err 表示调用/解析失败
    pub async fn run_review(&self, task_description: &str) -> Result<ReviewResult> {
        let diff = get_staged_diff().context("failed to get staged diff")?;
        if diff.trim().is_empty() {
            anyhow::bail!("Git diff is empty");
        }

        let user_prompt = format!(
            "Task description:\n{}\n\nGit diff:\n{}",
            task_description, diff
        );

        let response = match timeout(
            Duration::from_secs(self.timeout_secs),
            self.client.chat(REVIEWER_SYSTEM, &user_prompt),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => anyhow::bail!("LLM reviewer call failed: {}", e),
            Err(_) => anyhow::bail!("LLM reviewer timed out after {} seconds", self.timeout_secs),
        };

        let review = parse_review_output(&response);
        
        // 验证 risk_level
        if review.risk_level != "low" && review.risk_level != "medium" && review.risk_level != "high" {
            anyhow::bail!(
                "LLM reviewer returned invalid risk_level: {:?}",
                review.risk_level
            );
        }
        
        save_review(&self.run_log_path, &review)?;
        Ok(review)
    }

    /// 非交互模式评审：仅评审暂存区 diff，输出简要建议，返回退出码
    /// 返回: Ok(0) = 通过, Ok(1) = 不通过, Err = 错误(退出码 2)
    pub async fn run_review_non_interactive(&self, task_description: &str) -> Result<i32> {
        let diff = get_staged_diff().context("failed to get staged diff")?;
        if diff.trim().is_empty() {
            println!("暂存区为空，跳过评审");
            return Ok(0);
        }

        let user_prompt = format!(
            "Task description:\n{}\n\nGit diff:\n{}",
            task_description, diff
        );

        let response = match timeout(
            Duration::from_secs(self.timeout_secs),
            self.client.chat(REVIEWER_SYSTEM, &user_prompt),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => anyhow::bail!("LLM reviewer call failed: {}", e),
            Err(_) => anyhow::bail!("LLM reviewer timed out after {} seconds", self.timeout_secs),
        };

        let review = parse_review_output(&response);
        
        if review.risk_level != "low" && review.risk_level != "medium" && review.risk_level != "high" {
            anyhow::bail!(
                "LLM reviewer returned invalid risk_level: {:?}",
                review.risk_level
            );
        }
        save_review(&self.run_log_path, &review)?;

        print_brief_review(&review);

        match review.risk_level.as_str() {
            "low" => Ok(0),
            "medium" | "high" => Ok(1),
            _ => Ok(2),
        }
    }
}

pub fn parse_review_output(text: &str) -> ReviewResult {
    let trimmed = text.trim();
    if let Ok(mut parsed) = serde_json::from_str::<ReviewResult>(trimmed) {
        parsed.raw = text.to_string();
        return parsed;
    }

    if let Some(start) = trimmed.find("```json") {
        let after_fence = &trimmed[start + 7..];
        if let Some(end) = after_fence.find("```") {
            let json_text = after_fence[..end].trim();
            if let Ok(mut parsed) = serde_json::from_str::<ReviewResult>(json_text) {
                parsed.raw = text.to_string();
                return parsed;
            }
        }
    } else if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            let json_text = &trimmed[start..=end];
            if let Ok(mut parsed) = serde_json::from_str::<ReviewResult>(json_text) {
                parsed.raw = text.to_string();
                return parsed;
            }
        }
    }

    ReviewResult {
        risk_level: "unknown".to_string(),
        suggestions: vec![],
        key_issues: vec![],
        summary: String::new(),
        blocking: false,
        raw: text.to_string(),
    }
}

pub fn save_review(run_log_path: &Path, review: &ReviewResult) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_log_path)
        .with_context(|| format!("failed to open run log at {}", run_log_path.display()))?;
    let line = serde_json::to_string(review).with_context(|| "failed to serialize review")?;
    writeln!(file, "{}", line).with_context(|| "failed to write review to run log")?;
    Ok(())
}



fn get_staged_diff() -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--staged"])
        .output()
        .context("failed to execute git diff --staged")?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff --staged failed: {}", stderr);
    }
    
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn print_brief_review(review: &ReviewResult) {
    if !review.suggestions.is_empty() {
        println!("建议：{}", review.suggestions.join("; "));
    }
    if !review.key_issues.is_empty() {
        println!("风险：{}", review.key_issues.join("; "));
    }
    if review.suggestions.is_empty() && review.key_issues.is_empty() {
        println!("评审通过，无明显问题");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use anyhow::Result;

    struct MockLlm {
        response: String,
    }

    impl LlmClient for MockLlm {
        fn chat<'a>(
            &'a self,
            _system: &'a str,
            _user: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            let response = self.response.clone();
            Box::pin(async move { Ok(response) })
        }
    }

    #[test]
    fn parse_valid_json_returns_structured_result() {
        let input = r#"{"risk_level":"high","suggestions":["add tests"],"key_issues":["race condition"]}"#;
        let result = parse_review_output(input);
        assert_eq!(result.risk_level, "high");
        assert_eq!(result.suggestions, vec!["add tests"]);
        assert_eq!(result.key_issues, vec!["race condition"]);
        assert_eq!(result.raw, input);
    }

    #[test]
    fn parse_fallback_returns_raw_on_invalid_json() {
        let input = "This is not JSON, but should be kept.";
        let result = parse_review_output(input);
        assert_eq!(result.risk_level, "unknown");
        assert!(result.suggestions.is_empty());
        assert!(result.key_issues.is_empty());
        assert_eq!(result.raw, input);
    }

    #[test]
    fn save_review_appends_json_line() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("xi_reviewer_test_{}.log", unique));
        let _ = std::fs::remove_file(&path);
        let review = ReviewResult {
            risk_level: "medium".into(),
            suggestions: vec!["refactor".into()],
            key_issues: vec!["input validation".into()],
            summary: String::new(),
            blocking: false,
            raw: "raw review output".into(),
        };
        save_review(&path, &review).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: ReviewResult = serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert_eq!(parsed, review);
        let _ = std::fs::remove_file(&path);
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewResult {
    #[serde(default)]
    pub risk_level: String,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub key_issues: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub raw: String,
}
