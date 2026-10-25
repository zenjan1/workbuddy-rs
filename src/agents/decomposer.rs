//! Decomposer Agent: 从高级目标生成结构化 backlog 任务

use crate::llm::{LlmClient, LlmMessage};
use crate::task_runner::Task;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 任务分解器 Agent
pub struct DecomposerAgent {
    llm_client: LlmClient,
    prompt_template: String,
}

impl DecomposerAgent {
    /// 创建新的分解器实例
    pub fn new(llm_client: LlmClient, prompt_template: String) -> Self {
        Self {
            llm_client,
            prompt_template,
        }
    }

    /// 从 agents.toml 配置创建分解器
    pub fn from_config(llm_client: LlmClient, config: &DecomposerConfig) -> Result<Self> {
        let prompt_template = config.prompt.clone().context("decomposer prompt missing in config")?;
        Ok(Self::new(llm_client, prompt_template))
    }

    /// 将自然语言目标分解为结构化任务列表
    /// 
    /// # 参数
    /// - `goal`: 高级目标描述，如 "实现用户认证"
    /// - `count`: 期望生成的任务数量
    /// 
    /// # 返回
    /// 结构化的 Task 列表，可直接用于 `wb dev`
    pub async fn decompose(&self, goal: &str, count: usize) -> Result<Vec<Task>> {
        let prompt = self.build_prompt(goal, count);
        
        let messages = vec![
            LlmMessage::system("你是一个资深软件架构师，擅长将高级目标拆解为可执行的开发任务。"),
            LlmMessage::user(prompt),
        ];

        let response = self.llm_client.chat(messages).await
            .context("LLM 调用失败")?;

        let tasks = self.parse_response(&response)?;
        Ok(tasks)
    }

    /// 构建发送给 LLM 的提示词
    fn build_prompt(&self, goal: &str, count: usize) -> String {
        self.prompt_template
            .replace("{{goal}}", goal)
            .replace("{{count}}", &count.to_string())
    }

    /// 解析 LLM 响应为 Task 列表
    fn parse_response(&self, response: &str) -> Result<Vec<Task>> {
        // 尝试直接解析 JSON 数组
        if let Ok(tasks) = serde_json::from_str::<Vec<Task>>(response) {
            return Ok(tasks);
        }

        // 尝试从 markdown 代码块中提取 JSON
        if let Some(json_str) = extract_json_from_markdown(response) {
            if let Ok(tasks) = serde_json::from_str::<Vec<Task>>(&json_str) {
                return Ok(tasks);
            }
        }

        // 尝试解析包含 tasks 字段的对象
        #[derive(Deserialize)]
        struct TasksWrapper {
            tasks: Vec<Task>,
        }
        if let Ok(wrapper) = serde_json::from_str::<TasksWrapper>(response) {
            return Ok(wrapper.tasks);
        }
        if let Some(json_str) = extract_json_from_markdown(response) {
            if let Ok(wrapper) = serde_json::from_str::<TasksWrapper>(&json_str) {
                return Ok(wrapper.tasks);
            }
        }

        anyhow::bail!("无法解析 LLM 响应为任务列表: {}", response)
    }
}

/// 从 agents.toml 读取的分解器配置
#[derive(Debug, Deserialize, Serialize)]
pub struct DecomposerConfig {
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl Default for DecomposerConfig {
    fn default() -> Self {
        Self {
            prompt: None,
            model: None,
            temperature: Some(0.3),
            max_tokens: Some(4000),
        }
    }
}

/// 从 markdown 代码块中提取 JSON 字符串
fn extract_json_from_markdown(text: &str) -> Option<String> {
    // 匹配 ```json ... ``` 或 ``` ... ```
    let re = regex::Regex::new(r"```(?:json)?\s*(\{.*?\}|\[.*?\])\s*```").ok()?;
    let captures = re.captures(text)?;
    captures.get(1).map(|m| m.as_str().to_string())
}

/// 交互式细化：基于用户反馈重新生成任务
pub async fn refine_tasks(
    agent: &DecomposerAgent,
    original_goal: &str,
    current_tasks: &[Task],
    feedback: &str,
    count: usize,
) -> Result<Vec<Task>> {
    let current_json = serde_json::to_string_pretty(current_tasks)?;
    let refine_prompt = format!(
        r#"原始目标: {}

当前任务列表:
{}

用户反馈: {}

请根据反馈调整任务列表，生成 {} 个任务。输出 JSON 数组。"#,
        original_goal, current_json, feedback, count
    );

    let messages = vec![
        LlmMessage::system("你是一个资深软件架构师，根据用户反馈调整任务分解。"),
        LlmMessage::user(refine_prompt),
    ];

    let response = agent.llm_client.chat(messages).await
        .context("LLM 调用失败")?;

    agent.parse_response(&response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmClient, LlmMessage, LlmResponse};
    use crate::task_runner::{Task, TaskDependency};
    use async_trait::async_trait;
    use std::sync::Arc;

    /// Mock LLM 客户端用于测试
    struct MockLlmClient {
        response: String,
    }

    #[async_trait]
    impl LlmClient for MockLlmClient {
        async fn chat(&self, _messages: Vec<LlmMessage>) -> Result<LlmResponse> {
            Ok(LlmResponse {
                content: self.response.clone(),
                usage: None,
            })
        }

        async fn chat_stream(&self, _messages: Vec<LlmMessage>) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            unimplemented!()
        }
    }

    fn create_test_tasks_json() -> String {
        serde_json::to_string(&vec![
            Task {
                id: "task-1".to_string(),
                title: "设计用户认证数据模型".to_string(),
                description: "设计用户表、角色表、权限表等数据模型".to_string(),
                dependencies: vec![],
                estimated_tools: vec!["sql".to_string(), "db-migration".to_string()],
                status: "pending".to_string(),
                metadata: HashMap::new(),
            },
            Task {
                id: "task-2".to_string(),
                title: "实现用户注册 API".to_string(),
                description: "实现 POST /api/auth/register 接口，包含密码加密、邮箱验证".to_string(),
                dependencies: vec![TaskDependency { task_id: "task-1".to_string(), type_: "blocks".to_string() }],
                estimated_tools: vec!["axum".to_string(), "bcrypt".to_string(), "validator".to_string()],
                status: "pending".to_string(),
                metadata: HashMap::new(),
            },
            Task {
                id: "task-3".to_string(),
                title: "实现用户登录 API".to_string(),
                description: "实现 POST /api/auth/login 接口，签发 JWT token".to_string(),
                dependencies: vec![TaskDependency { task_id: "task-1".to_string(), type_: "blocks".to_string() }],
                estimated_tools: vec!["axum".to_string(), "jsonwebtoken".to_string()],
                status: "pending".to_string(),
                metadata: HashMap::new(),
            },
            Task {
                id: "task-4".to_string(),
                title: "实现 JWT 中间件与权限校验".to_string(),
                description: "实现 JWT 验证中间件、基于角色的访问控制 (RBAC)".to_string(),
                dependencies: vec![TaskDependency { task_id: "task-3".to_string(), type_: "blocks".to_string() }],
                estimated_tools: vec!["axum".to_string(), "jsonwebtoken".to_string()],
                status: "pending".to_string(),
                metadata: HashMap::new(),
            },
            Task {
                id: "task-5".to_string(),
                title: "实现密码重置与邮件通知".to_string(),
                description: "实现忘记密码、重置密码流程，集成邮件发送服务".to_string(),
                dependencies: vec![TaskDependency { task_id: "task-2".to_string(), type_: "blocks".to_string() }],
                estimated_tools: vec!["lettre".to_string(), "tera".to_string()],
                status: "pending".to_string(),
                metadata: HashMap::new(),
            },
            Task {
                id: "task-6".to_string(),
                title: "编写认证模块单元测试与集成测试".to_string(),
                description: "覆盖注册、登录、权限校验、Token 刷新等核心流程".to_string(),
                dependencies: vec![
                    TaskDependency { task_id: "task-2".to_string(), type_: "blocks".to_string() },
                    TaskDependency { task_id: "task-3".to_string(), type_: "blocks".to_string() },
                    TaskDependency { task_id: "task-4".to_string(), type_: "blocks".to_string() },
                ],
                estimated_tools: vec!["tokio-test".to_string(), "reqwest".to_string()],
                status: "pending".to_string(),
                metadata: HashMap::new(),
            },
        ]).unwrap()
    }

    #[tokio::test]
    async fn test_decompose_parses_json_array() {
        let mock_client = MockLlmClient {
            response: create_test_tasks_json(),
        };
        let agent = DecomposerAgent::new(mock_client, "{{goal}} {{count}}".to_string());

        let tasks = agent.decompose("实现用户认证", 5).await.unwrap();

        assert!(tasks.len() >= 5, "应至少生成 5 个任务，实际: {}", tasks.len());
        assert_eq!(tasks[0].title, "设计用户认证数据模型");
        assert_eq!(tasks[1].title, "实现用户注册 API");
        assert!(!tasks[1].dependencies.is_empty());
        assert!(tasks[1].dependencies[0].task_id == "task-1");
    }

    #[tokio::test]
    async fn test_decompose_parses_markdown_json() {
        let json = create_test_tasks_json();
        let markdown_response = format!("```json
{}
```", json);
        
        let mock_client = MockLlmClient {
            response: markdown_response,
        };
        let agent = DecomposerAgent::new(mock_client, "{{goal}} {{count}}".to_string());

        let tasks = agent.decompose("实现用户认证", 5).await.unwrap();

        assert!(tasks.len() >= 5);
        assert_eq!(tasks[0].title, "设计用户认证数据模型");
    }

    #[tokio::test]
    async fn test_decompose_parses_wrapper_object() {
        let json = create_test_tasks_json();
        let wrapper = format!("{{\"tasks\": {}}}", json);
        
        let mock_client = MockLlmClient {
            response: wrapper,
        };
        let agent = DecomposerAgent::new(mock_client, "{{goal}} {{count}}".to_string());

        let tasks = agent.decompose("实现用户认证", 5).await.unwrap();

        assert!(tasks.len() >= 5);
    }

    #[tokio::test]
    async fn test_decompose_chinese_goal_produces_5plus_tasks() {
        let mock_client = MockLlmClient {
            response: create_test_tasks_json(),
        };
        let agent = DecomposerAgent::new(mock_client, "{{goal}} {{count}}".to_string());

        let tasks = agent.decompose("实现用户认证", 5).await.unwrap();

        // 验证生成至少 5 个任务
        assert!(tasks.len() >= 5, "中文目标 '实现用户认证' 应生成至少 5 个任务");
        
        // 验证任务结构完整性
        for task in &tasks {
            assert!(!task.id.is_empty(), "任务 ID 不能为空");
            assert!(!task.title.is_empty(), "任务标题不能为空");
            assert!(!task.description.is_empty(), "任务描述不能为空");
            assert!(!task.estimated_tools.is_empty(), "预估工具不能为空");
            assert_eq!(task.status, "pending", "新任务状态应为 pending");
        }
        
        // 验证依赖关系
        let has_dependencies = tasks.iter().any(|t| !t.dependencies.is_empty());
        assert!(has_dependencies, "应包含任务依赖关系");
    }

    #[tokio::test]
    async fn test_refine_tasks() {
        let json = create_test_tasks_json();
        let mock_client = MockLlmClient {
            response: json,
        };
        let agent = DecomposerAgent::new(mock_client, "{{goal}} {{count}}".to_string());

        let original_tasks = vec![
            Task {
                id: "old-1".to_string(),
                title: "旧任务".to_string(),
                description: "旧描述".to_string(),
                dependencies: vec![],
                estimated_tools: vec![],
                status: "pending".to_string(),
                metadata: HashMap::new(),
            }
        ];

        let refined = refine_tasks(&agent, "实现用户认证", &original_tasks, "增加 OAuth2 支持", 5).await.unwrap();

        assert!(refined.len() >= 5);
        assert_eq!(refined[0].title, "设计用户认证数据模型");
    }
}
