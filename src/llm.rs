use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::config::Config;
fn clip(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        let mut truncated: String = chars[..max_chars].iter().collect();
        truncated.push_str("...");
        truncated
    }
}

/// 成本估算:根据 token 用量和每千 token 单价估算成本。
/// 计算公式: tokens * cost_per_1k_tokens / 1000.0
/// 返回 f64,保留浮点精度,不在此处格式化展示。
pub fn estimate_cost(tokens: u64, cost_per_1k_tokens: f64) -> f64 {
    tokens as f64 * cost_per_1k_tokens / 1000.0
}

/// LLM 每次调用的 token 与字符用量。
///
/// 新字段(input_tokens/output_tokens/input_chars/output_chars)为权威统计口径；
/// 旧的 prompt_tokens/completion_tokens/character_count 保留用于向后兼容，并与新字段同步。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LlmUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub input_chars: usize,
    pub output_chars: usize,
    // 向后兼容旧字段（由 extract_usage_with_input 和 merge 同步维护）
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub character_count: usize,
}

impl LlmUsage {
    /// 累加另一个 usage 的全部字段，供 executor 汇总多次 LLM 调用。
    pub fn merge(&mut self, other: &LlmUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
        self.input_chars += other.input_chars;
        self.output_chars += other.output_chars;
        // 同步旧字段，保证向后兼容的读取结果与权威字段一致。
        self.prompt_tokens = self.input_tokens;
        self.completion_tokens = self.output_tokens;
        self.character_count = self.output_chars;
    }
}

/// 兼容别名，便于外部使用 `Usage`。
pub type Usage = LlmUsage;

/// 可序列化/反序列化的 token/字符用量统计。
/// 字段均为可选，旧响应缺少 usage 或 token 统计时 estimated 为 true。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub input_chars: Option<usize>,
    pub output_chars: Option<usize>,
    /// true 表示 token 字段缺失，字符数为按输入/输出文本长度估算。
    pub estimated: bool,
}

/// LLM 响应:文本 + 可选用量(若上游未提供 usage 则为 None/默认零)。
#[derive(Debug)]
pub struct LlmResponse {
    pub text: String,
    pub usage: Option<Usage>,
    /// 新用量结构：token 字段为 Option，均可选且可序列化。
    pub token_usage: Option<TokenUsage>,
}

/// 从 OpenAI 兼容响应的 JSON 中提取 token 用量与字符数。
/// 兼容旧调用：输入字符数记 0。
pub fn extract_usage(v: &serde_json::Value, text: &str) -> LlmUsage {
    extract_usage_with_input(v, text, 0)
}

/// 从 OpenAI 兼容响应的 JSON 中提取可序列化的 TokenUsage。
/// 优先解析 token 统计字段；若缺失则基于输入/输出文本长度估算字符数并标记 estimated=true。
pub fn extract_token_usage_with_input(v: &serde_json::Value, text: &str, input_chars: usize) -> TokenUsage {
    let output_chars = text.chars().count();
    match v.get("usage") {
        Some(usage) => {
            let input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64());
            let output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64());
            let total_tokens = usage.get("total_tokens").and_then(|v| v.as_u64());
            if input_tokens.is_some() || output_tokens.is_some() || total_tokens.is_some() {
                TokenUsage {
                    input_tokens,
                    output_tokens,
                    total_tokens,
                    input_chars: Some(input_chars),
                    output_chars: Some(output_chars),
                    estimated: false,
                }
            } else {
                TokenUsage {
                    input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                    input_chars: Some(input_chars),
                    output_chars: Some(output_chars),
                    estimated: true,
                }
            }
        }
        None => TokenUsage {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            input_chars: Some(input_chars),
            output_chars: Some(output_chars),
            estimated: true,
        },
    }
}

/// 从 OpenAI 兼容响应的 JSON 中提取 token 用量与字符数。
/// `input_chars` 为本次请求的输入字符数（system + user 的字符数之和）。
/// 若响应没有 `usage` 字段，则 token 记 0，但字符数仍会统计。
pub fn extract_usage_with_input(v: &serde_json::Value, text: &str, input_chars: usize) -> LlmUsage {
    let output_chars = text.chars().count();
    match v.get("usage") {
        Some(usage) => {
            let input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let total_tokens = usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            LlmUsage {
                input_tokens,
                output_tokens,
                total_tokens,
                input_chars,
                output_chars,
                // 同步旧字段，保持向后兼容
                prompt_tokens: input_tokens,
                completion_tokens: output_tokens,
                character_count: output_chars,
            }
        }
        None => LlmUsage {
            input_chars,
            output_chars,
            character_count: output_chars,
            ..Default::default()
        },
    }
}

/// LLM 抽象:OpenAI 兼容接口 + 离线 Mock。
/// 手动装箱为 dyn Future 以保证 trait 对象安全(无需 async-trait 依赖)。
pub trait Llm: Send + Sync {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>>;
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
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>> {
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
            let input_chars = system.chars().count() + user.chars().count();

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
                    let usage = extract_usage_with_input(&v, &content, input_chars);
                    let token_usage = extract_token_usage_with_input(&v, &content, input_chars);
                    return Ok(LlmResponse {
                        text: content,
                        usage: Some(usage),
                        token_usage: Some(token_usage),
                    });
                }
                let reasoning = v
                    .pointer("/choices/0/message/reasoning_content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                if !reasoning.trim().is_empty() {
                    let usage = extract_usage_with_input(&v, &reasoning, input_chars);
                    let token_usage = extract_token_usage_with_input(&v, &reasoning, input_chars);
                    eprintln!(
                        "[llm] content 为空,回退使用 reasoning_content({} 字符)",
                        reasoning.len()
                    );
                    return Ok(LlmResponse {
                        text: reasoning,
                        usage: Some(usage),
                        token_usage: Some(token_usage),
                    });
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
#[derive(Default)]
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

/// 构造 MockLlm 的标准离线响应:按 system 提示选择固定 JSON,其余返回占位文本。
fn mock_response(system: &str, user: &str) -> LlmResponse {
    let input_chars = system.chars().count() + user.chars().count();
    let output = if system.contains("输出协议") {
        MOCK_EDITS.to_string()
    } else if system.contains("任务生成器") {
        MOCK_GEN_TASKS.to_string()
    } else if system.contains("开发任务") {
        MOCK_DEV_PLAN.to_string()
    } else if system.contains("规划师") || system.contains("Planner") {
        MOCK_PLAN.to_string()
    } else {
        let head = user
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("done");
        format!("[mock] {}", clip(head, 60))
    };
    let output_chars = output.chars().count();
    let usage = LlmUsage {
        input_chars,
        output_chars,
        character_count: output_chars,
        ..Default::default()
    };
    let token_usage = TokenUsage {
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        input_chars: Some(input_chars),
        output_chars: Some(output_chars),
        estimated: true,
    };
    LlmResponse { text: output, usage: Some(usage), token_usage: Some(token_usage) }
}

/// 脚本化单次调用结果:`Ok(())` 走正常 Mock 成功响应,`Err(message)` 返回错误。
pub type MockOutcome = std::result::Result<(), String>;

impl MockLlm {
    /// 创建默认全成功 Mock,与单元值 `MockLlm` 行为一致。
    pub fn new() -> Self {
        Self
    }

    /// 按调用顺序脚本化返回 Ok/Err,供 bench 等测试确定性注入失败。
    pub fn with_outcomes<I>(outcomes: I) -> ScriptedMockLlm
    where
        I: IntoIterator<Item = MockOutcome>,
    {
        ScriptedMockLlm {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }
}

/// 带结果脚本的离线 Mock LLM。
///
/// 队列为空(包括 `Default::default()`/`new`)时,所有调用均回退为普通 Mock 成功响应;
/// 内部使用 `Mutex<VecDeque<_>>`,可安全跨并发任务共享(`Send + Sync`)。
#[derive(Default)]
pub struct ScriptedMockLlm {
    outcomes: Mutex<VecDeque<MockOutcome>>,
}

impl ScriptedMockLlm {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Llm for ScriptedMockLlm {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>> {
        Box::pin(async move {
            let outcome = {
                let mut queue = self
                    .outcomes
                    .lock()
                    .expect("ScriptedMockLlm outcomes mutex poisoned");
                queue.pop_front()
            };
            match outcome {
                Some(Err(message)) => bail!("{message}"),
                None | Some(Ok(())) => Ok(mock_response(system, user)),
            }
        })
    }
}

impl Llm for MockLlm {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>> {
        Box::pin(async move { Ok(mock_response(system, user)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_planner_returns_parseable_json() {
        let llm = MockLlm;
        let raw = llm.chat("你是规划师", "任务").await.unwrap();
        assert!(raw.text.contains("\"steps\""));
        let v: serde_json::Value = serde_json::from_str(&raw.text).unwrap();
        assert_eq!(v["steps"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn mock_step_returns_placeholder() {
        let llm = MockLlm;
        let out = llm.chat("专家", "第一步调研").await.unwrap();
        assert!(out.text.starts_with("[mock]"));
    }

    #[test]
    fn estimate_cost_zero_tokens_returns_zero() {
        assert_eq!(estimate_cost(0, 10.0), 0.0);
    }

    #[test]
    fn estimate_cost_regular_tokens_and_prices() {
        let cost = estimate_cost(1000, 10.0);
        assert!((cost - 10.0).abs() < 1e-12, "cost={cost}");
        let cost = estimate_cost(2000, 0.5);
        assert!((cost - 1.0).abs() < 1e-12, "cost={cost}");
        let cost = estimate_cost(1234, 7.5);
        assert!((cost - 1234.0 * 7.5 / 1000.0).abs() < 1e-12, "cost={cost}");
    }

    #[test]
    fn estimate_cost_small_token_count_keeps_precision() {
        let cost = estimate_cost(1, 10.0);
        assert!((cost - 0.01).abs() < 1e-12, "cost={cost}");
        let cost = estimate_cost(5, 2.0);
        assert!((cost - 0.01).abs() < 1e-12, "cost={cost}");
    }

    #[test]
    fn estimate_cost_with_high_price() {
        let cost = estimate_cost(100, 1000.0);
        assert!((cost - 100.0).abs() < 1e-12, "cost={cost}");
    }

    #[test]
    fn extract_usage_extracts_tokens_and_chars() {
        let v = serde_json::json!({
            "usage": {
                "prompt_tokens": 120,
                "completion_tokens": 30,
                "total_tokens": 150
            }
        });
        let text = "你好，世界";
        let usage = extract_usage_with_input(&v, text, 8);
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.input_chars, 8);
        assert_eq!(usage.output_chars, text.chars().count());
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 30);
        assert_eq!(usage.character_count, text.chars().count());
    }

    #[test]
    fn extract_usage_without_usage_counts_chars_only() {
        let v = serde_json::json!({});
        let text = "abc";
        let usage = extract_usage_with_input(&v, text, 10);
        assert_eq!(usage.input_chars, 10);
        assert_eq!(usage.output_chars, 3);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(usage.character_count, 3);
    }

    #[test]
    fn usage_merge_accumulates_all_fields() {
        let mut a = LlmUsage {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
            input_chars: 100,
            output_chars: 200,
            prompt_tokens: 10,
            completion_tokens: 20,
            character_count: 200,
        };
        let b = LlmUsage {
            input_tokens: 5,
            output_tokens: 7,
            total_tokens: 12,
            input_chars: 40,
            output_chars: 60,
            prompt_tokens: 5,
            completion_tokens: 7,
            character_count: 60,
        };
        a.merge(&b);
        assert_eq!(a.input_tokens, 15);
        assert_eq!(a.output_tokens, 27);
        assert_eq!(a.total_tokens, 42);
        assert_eq!(a.input_chars, 140);
        assert_eq!(a.output_chars, 260);
        assert_eq!(a.prompt_tokens, 15);
        assert_eq!(a.completion_tokens, 27);
        assert_eq!(a.character_count, 260);
    }

    #[tokio::test]
    async fn mock_llm_returns_usage_with_character_counts() {
        let llm = MockLlm;
        let system = "测试系统提示";
        let user = "测试用户输入";
        let out = llm.chat(system, user).await.unwrap();
        let usage = out.usage.expect("mock usage 应该存在");
        assert_eq!(usage.input_chars, system.chars().count() + user.chars().count());
        assert_eq!(usage.output_chars, out.text.chars().count());
        assert_eq!(usage.input_tokens, 0);
    }

    #[tokio::test]
    async fn scripted_mock_replays_outcomes_in_order() {
        let llm = MockLlm::with_outcomes([
            Ok(()),
            Err("mock failure".to_string()),
            Ok(()),
        ]);

        let ok = llm.chat("system", "u1").await.unwrap();
        assert!(ok.text.starts_with("[mock]"));

        let err = llm
            .chat("system", "u2")
            .await
            .expect_err("第二个脚本化请求应当失败");
        assert!(err.to_string().contains("mock failure"));

        assert!(llm.chat("system", "u3").await.is_ok());
        // 脚本耗尽后回退为默认全成功行为。
        assert!(llm.chat("system", "u4").await.is_ok());
    }

    #[tokio::test]
    async fn scripted_mock_default_succeeds() {
        let llm = ScriptedMockLlm::default();
        for _ in 0..3 {
            assert!(llm.chat("system", "user").await.is_ok());
        }
    }

    #[test]
    fn scripted_mock_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScriptedMockLlm>();
    }
}

pub trait LlmClient {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>>;
}