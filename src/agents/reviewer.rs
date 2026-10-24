use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use crate::llm::LlmClient;

const REVIEWER_SYSTEM: &str = "You are a rigorous code reviewer. Given a task description and a git diff, produce a structured review in JSON with keys: risk_level (low/medium/high), suggestions (array of strings), key_issues (array of strings).";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewResult {
    #[serde(default)]
    pub risk_level: String,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub key_issues: Vec<String>,
    #[serde(default)]
    pub raw: String,
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

pub async fn run_reviewer<C: LlmClient + ?Sized>(
    diff: &str,
    task_description: &str,
    client: &C,
    run_log_path: &Path,
) -> Result<ReviewResult> {
    if diff.trim().is_empty() {
        anyhow::bail!("Git diff is empty");
    }

    let user_prompt = format!(
        "Task description:\n{}\n\nGit diff:\n{}",
        task_description, diff
    );
    let response = client
        .chat(REVIEWER_SYSTEM, &user_prompt)
        .await
        .context("LLM reviewer call failed")?;
    let review = parse_review_output(&response);
    if review.risk_level != "low" && review.risk_level != "medium" && review.risk_level != "high" {
        anyhow::bail!(
            "LLM reviewer returned invalid risk_level: {:?}",
            review.risk_level
        );
    }
    save_review(run_log_path, &review)?;
    Ok(review)
}

/// 非交互模式评审：仅评审暂存区 diff，超时 60s，输出简要建议，返回退出码
/// 返回: Ok(0) = 通过, Ok(1) = 不通过, Err = 错误(退出码 2)
pub async fn run_reviewer_non_interactive<C: LlmClient + ?Sized>(
    client: &C,
    run_log_path: &Path,
    task_description: &str,
    timeout_secs: u64,
) -> Result<i32> {
    // 获取暂存区 diff
    let diff = get_staged_diff().context("failed to get staged diff")?;
    if diff.trim().is_empty() {
        println!("暂存区为空，跳过评审");
        return Ok(0);
    }

    let user_prompt = format!(
        "Task description:\n{}\n\nGit diff:\n{}",
        task_description, diff
    );

    // 带超时的 LLM 调用
    let response = match timeout(Duration::from_secs(timeout_secs), client.chat(REVIEWER_SYSTEM, &user_prompt)).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => anyhow::bail!("LLM reviewer call failed: {}", e),
        Err(_) => anyhow::bail!("LLM reviewer timed out after {} seconds", timeout_secs),
    };

    let review = parse_review_output(&response);
    
    // 验证 risk_level
    if review.risk_level != "low" && review.risk_level != "medium" && review.risk_level != "high" {
        anyhow::bail!(
            "LLM reviewer returned invalid risk_level: {:?}",
            review.risk_level
        );
    }
    save_review(run_log_path, &review)?;

    // 输出简要建议
    print_brief_review(&review);

    // 根据风险等级决定退出码
    match review.risk_level.as_str() {
        "low" => Ok(0),
        "medium" | "high" => Ok(1),
        _ => Ok(2),
    }
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
        let path = std::env::temp_dir().join(format!("workbuddy_reviewer_test_{}.log", unique));
        let _ = std::fs::remove_file(&path);
        let review = ReviewResult {
            risk_level: "medium".into(),
            suggestions: vec!["refactor".into()],
            key_issues: vec!["input validation".into()],
            raw: "raw review output".into(),
        };
        save_review(&path, &review).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: ReviewResult = serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert_eq!(parsed, review);
        let _ = std::fs::remove_file(&path);
    }
}
