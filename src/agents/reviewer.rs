use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
