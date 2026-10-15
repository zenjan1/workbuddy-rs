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
            let payload = json!({
                "model": self.cfg.model,
                "temperature": 0.3,
                // 推理模型(如 deepseek-v4-pro)需足量预算:先思考(reasoning)再产出,
                // 太小会耗尽预算导致 content 为空(见下方回退兜底)。
                "max_tokens": 32768,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user}
                ]
            });

            // 校园网到 DeepSeek 不稳定:读响应体失败/超时/连接重置时有发生。
            // 旧逻辑只对 429/5xx 状态码重试一次,传输错误直接返回 → 网络一抖整轮 LLM 调用全废。
            // 现在:任何错误(传输失败 / 可重试状态码 / 解析失败 / 内容为空)都重试,指数退避,最多 5 次。
            const MAX_ATTEMPTS: u32 = 5;
            let mut last_err = String::new();
            for attempt in 0..MAX_ATTEMPTS {
                if attempt > 0 {
                    let backoff = std::cmp::min(60u64, 1u64 << attempt);
                    eprintln!("[llm] 第 {attempt} 次重试,等待 {backoff}s...");
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                }
                // 1) 发送(传输错误可重试)
                let resp = match self
                    .client
                    .post(&url)
                    .bearer_auth(&key)
                    .json(&payload)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        last_err = format!("LLM 请求失败(传输): {e}");
                        eprintln!(
                            "[llm] 第 {} 次请求失败: {}",
                            attempt + 1,
                            clip(&last_err, 120)
                        );
                        continue;
                    }
                };
                let status = resp.status();
                // 2) 读响应体(传输错误可重试)
                let text = match resp.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        last_err = format!("读取响应体失败(传输): {e}");
                        eprintln!(
                            "[llm] 第 {} 次读体失败: {}",
                            attempt + 1,
                            clip(&last_err, 120)
                        );
                        continue;
                    }
                };
                // 3) 非 2xx:429/5xx 可重试,其余(4xx 业务错误,如 401/400)直接返回
                if !status.is_success() {
                    if matches!(status.as_u16(), 429 | 500 | 502 | 503) {
                        last_err = format!("LLM API 返回 {status}: {}", clip(&text, 200));
                        eprintln!("[llm] 第 {} 次可重试状态码 {status}", attempt + 1);
                        continue;
                    }
                    bail!("LLM API 返回 {}: {}", status, clip(&text, 300));
                }
                // 4) 解析 JSON(解析错误可重试)
                let v: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        last_err = format!("解析 LLM 响应 JSON 失败: {e}");
                        eprintln!(
                            "[llm] 第 {} 次解析失败: {}",
                            attempt + 1,
                            clip(&last_err, 120)
                        );
                        continue;
                    }
                };
                // 5) 取 content,空则回退 reasoning_content(内容空可重试)
                let content = v
                    .pointer("/choices/0/message/content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                if !content.trim().is_empty() {
                    return Ok(content);
                }
                let reasoning = v
                    .pointer("/choices/0/message/reasoning_content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                if !reasoning.trim().is_empty() {
                    eprintln!(
                        "[llm] content 为空,回退使用 reasoning_content({} 字符)",
                        reasoning.len()
                    );
                    return Ok(reasoning);
                }
                last_err = "响应中 content 与 reasoning_content 均为空".to_string();
                eprintln!("[llm] 第 {} 次内容为空,重试...", attempt + 1);
            }
            bail!(
                "LLM 调用失败(已重试 {MAX_ATTEMPTS} 次): {}",
                clip(&last_err, 200)
            )
        })
    }
}

/// 离线 Mock LLM:规划请求返回固定合法计划,dev 规划返回含 edit/shell 的计划,
/// edits 请求返回安全的 no-op 编辑,其余返回占位输出。
pub struct MockLlm;

const MOCK_PLAN: &str = r#"{"goal":"(mock) 任务目标","steps":[
  {"id":1,"name":"调研","agent":"researcher","skill":"deep-research","prompt":"(mock) 调研相关信息","kind":"llm","needs":[]},
  {"id":2,"name":"撰写","agent":"writer","skill":"report-writing","prompt":"(mock) 撰写报告","kind":"llm","needs":[1]},
  {"id":3,"name":"终稿审核","agent":"reviewer","skill":"final-review","prompt":"(mock) 审核并总结","kind":"llm","needs":[2]}
]}"#;

const MOCK_DEV_PLAN: &str = r#"{"goal":"(mock) 开发任务","steps":[
  {"id":1,"name":"查看构建","agent":"coder","skill":"code-writing","prompt":"(mock) 查看构建文件","kind":"read","path":"README.md","needs":[]},
  {"id":2,"name":"安全改动","agent":"coder","skill":"code-writing","prompt":"(mock) 做一处安全的最小改动","kind":"edit","path":"autodev_note.md","needs":[1]},
  {"id":3,"name":"验证","agent":"reviewer","skill":"final-review","prompt":"(mock) 运行验证","kind":"shell","command":"echo mock-verify-ok","needs":[2]}
]}"#;

/// Mock 的 edits 输出:在仓库根的 autodev_note.md 追加一行(始终唯一/存在)。
/// 注意:必须写在 .workbuddy/ 之外——该目录下的文件不算"源码改动"(假成功守卫)。
const MOCK_EDITS: &str = r#"{"edits":[{"path":"autodev_note.md","old_string":"","new_string":"(mock) 自主开发步骤完成","replace_all":false}]}"#;

/// Mock 的任务生成输出:一个可重复执行的安全 mock 任务。
const MOCK_GEN_TASKS: &str = r#"{"tasks":[{"title":"(mock) 生成任务:补充文档注释","description":"为某个模块补充一行注释","priority":1}]}"#;

impl Llm for MockLlm {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        _user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            if system.contains("输出协议") {
                Ok(MOCK_EDITS.to_string())
            } else if system.contains("任务生成器") {
                Ok(MOCK_GEN_TASKS.to_string())
            } else if system.contains("开发任务") {
                Ok(MOCK_DEV_PLAN.to_string())
            } else if system.contains("规划师") || system.contains("Planner") {
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
