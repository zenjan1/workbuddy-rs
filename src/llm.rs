use std::future::Future;
use std::pin::Pin;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::config::Config;
use crate::util::clip;

/// LLM 抽象:OpenAI 兼容接口 + 离线 Mock。
/// 手动装箱为 dyn Future 以保证 trait 对象安全(无需 async-trait 依赖)。
pub trait Llm: Send + Sync {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}

/// OpenAI 兼容聊天客户端(DashScope compatible-mode 等)。
pub struct OpenAiCompat {
    client: reqwest::Client,
    cfg: Config,
}

impl OpenAiCompat {
    pub fn new(cfg: &Config) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(cfg.request_timeout_secs))
            .build()
            .context("创建 HTTP 客户端失败")?;
        Ok(Self {
            client,
            cfg: cfg.clone(),
        })
    }
}

impl Llm for OpenAiCompat {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let key = self
                .cfg
                .api_key
                .clone()
                .context("缺少 API key")?;
            let url = format!(
                "{}/chat/completions",
                self.cfg.base_url.trim_end_matches('/')
            );
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&key)
                .json(&json!({
                    "model": self.cfg.model,
                    "temperature": 0.3,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user}
                    ]
                }))
                .send()
                .await
                .with_context(|| format!("LLM 请求失败: {url}"))?;

            let status = resp.status();
            let text = resp.text().await.context("读取响应体失败")?;
            if !status.is_success() {
                bail!("LLM API 返回 {}: {}", status, clip(&text, 300));
            }
            let v: serde_json::Value =
                serde_json::from_str(&text).context("解析 LLM 响应 JSON 失败")?;
            v.pointer("/choices/0/message/content")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow::anyhow!("响应中缺少 choices[0].message.content"))
        })
    }
}

/// 离线 Mock LLM:规划请求返回固定合法计划,其余返回占位输出。
pub struct MockLlm;

const MOCK_PLAN: &str = r#"{"goal":"(mock) 任务目标","steps":[
  {"id":1,"name":"调研","agent":"researcher","skill":"deep-research","prompt":"(mock) 调研相关信息","kind":"llm","needs":[]},
  {"id":2,"name":"撰写","agent":"writer","skill":"report-writing","prompt":"(mock) 撰写报告","kind":"llm","needs":[1]},
  {"id":3,"name":"终稿审核","agent":"reviewer","skill":"final-review","prompt":"(mock) 审核并总结","kind":"llm","needs":[2]}
]}"#;

impl Llm for MockLlm {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        _user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            if system.contains("规划师") || system.contains("Planner") {
                Ok(MOCK_PLAN.to_string())
            } else {
                let head = _user
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("done");
                Ok(format!("[mock] {}", clip(head, 60)))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_planner_returns_parseable_json() {
        let llm = MockLlm;
        let raw = llm.chat("你是规划师", "任务").await.unwrap();
        assert!(raw.contains("\"steps\""));
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["steps"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn mock_step_returns_placeholder() {
        let llm = MockLlm;
        let out = llm.chat("专家", "第一步调研").await.unwrap();
        assert!(out.starts_with("[mock]"));
    }
}
