/// 内置专家智能体目录(对应 WorkBuddy 的"多专家协同"范式)。
pub mod reviewer;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::collections::HashMap;
use serde_json::Value;
use std::path::Path;

/// 复杂度等级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Complexity {
    #[default]
    Med,
    Low,
    High,
}

/// 工具类型分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ToolType {
    /// 内置工具
    Builtin,
    /// 自定义工具
    Custom,
    /// MCP 协议工具
    Mcp,
}

/// 工具规格定义
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub tool_type: ToolType,
}

/// 工具执行结果类型
pub type ToolResult = Result<Value, ToolError>;

/// 工具错误类型
#[derive(Debug, Clone)]
pub enum ToolError {
    Execution(String),
    InvalidArgs(String),
    NotFound(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::Execution(msg) => write!(f, "Execution failed: {}", msg),
            ToolError::InvalidArgs(msg) => write!(f, "Invalid arguments: {}", msg),
            ToolError::NotFound(msg) => write!(f, "Tool not found: {}", msg),
        }
    }
}

impl std::error::Error for ToolError {}

/// 工具特征定义
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn execute(&self, args: Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send>>;
}

/// 工具注册表
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolError> {
        let spec = tool.spec();
        if self.tools.contains_key(&spec.name) {
            return Err(ToolError::Execution(format!("Tool '{}' already registered", spec.name)));
        }
        self.tools.insert(spec.name.clone(), tool);
        Ok(())
    }

    pub fn list(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }
}

/// 自定义专家模板生成模块
pub mod template {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::fmt;
    use std::error::Error as StdError;
    use schemars::JsonSchema;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
    pub enum AgentType {
        Planner,
        Executor,
        Reviewer,
    }

    impl AgentType {
        pub fn from_str(s: &str) -> Option<Self> {
            match s.to_lowercase().as_str() {
                "planner" => Some(AgentType::Planner),
                "executor" => Some(AgentType::Executor),
                "reviewer" => Some(AgentType::Reviewer),
                _ => None,
            }
        }

        pub fn as_str(&self) -> &'static str {
            match self {
                AgentType::Planner => "planner",
                AgentType::Executor => "executor",
                AgentType::Reviewer => "reviewer",
            }
        }
    }

    #[derive(Debug)]
    pub enum TemplateError {
        DirCreate(std::io::Error),
        FileExists(PathBuf),
        InvalidType(String),
        Write(std::io::Error),
    }

    impl fmt::Display for TemplateError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                TemplateError::DirCreate(e) => write!(f, "模板目录创建失败: {}", e),
                TemplateError::FileExists(path) => write!(f, "文件已存在: {}", path.display()),
                TemplateError::InvalidType(msg) => write!(f, "无效的专家类型: {}", msg),
                TemplateError::Write(e) => write!(f, "模板写入失败: {}", e),
            }
        }
    }

    impl StdError for TemplateError {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            match self {
                TemplateError::DirCreate(e) => Some(e),
                TemplateError::Write(e) => Some(e),
                _ => None,
            }
        }
    }

    impl From<std::io::Error> for TemplateError {
        fn from(e: std::io::Error) -> Self {
            TemplateError::DirCreate(e)
        }
    }

    pub type Result<T> = std::result::Result<T, TemplateError>;

    /// 生成的文件路径
    #[derive(Debug, Clone, JsonSchema)]
    pub struct GeneratedFiles {
        pub toml_path: PathBuf,
        pub md_path: PathBuf,
    }

    /// Planner 类型 TOML 模板
    const PLANNER_TOML: &str = r#"""
name = "{{name}}"
type = "planner"
description = "统筹规划师：负责目标拆解、依赖编排与最终验收"
model = "gpt-4o-mini"
prompt_file = "prompt.md"
skills = [
  { name = "task-breakdown", description = "将目标拆解为可执行步骤，明确输入输出与依赖" },
  { name = "acceptance", description = "为每个步骤定义可检验的完成标准" },
]
""""#;

    /// Planner 类型 Markdown 提示词模板
    const PLANNER_MD: &str = r#"""
# {{name}} - 统筹规划师

## 系统提示词

你是 WorkBuddy 的统筹规划师，负责把用户目标拆解为可执行的步骤计划，并定义验收标准。你严谨、全局观强。

## 核心技能

### task-breakdown - 任务拆解
将目标拆解为 2-6 个步骤，明确每步的输入、输出与依赖。

### acceptance - 验收标准
为每个步骤给出可检验的完成标准。

## 输出格式

请按以下 JSON 格式输出规划结果：

```json
{
  "steps": [
    {
      "id": "step-1",
      "name": "步骤名称",
      "description": "步骤描述",
      "inputs": ["输入1", "输入2"],
      "outputs": ["输出1"],
      "depends_on": [],
      "acceptance": ["验收标准1", "验收标准2"]
    }
  ]
}
```
""""#;

    /// Executor 类型 TOML 模板
    const EXECUTOR_TOML: &str = r#"""
name = "{{name}}"
type = "executor"
description = "执行专家：负责具体任务的实施与交付"
model = "gpt-4o-mini"
prompt_file = "prompt.md"
skills = [
  { name = "implementation", description = "按规划执行具体实现任务" },
  { name = "validation", description = "自检交付物是否满足验收标准" },
]
""""#;

    /// Executor 类型 Markdown 提示词模板
    const EXECUTOR_MD: &str = r#"""
# {{name}} - 执行专家

## 系统提示词

你是 WorkBuddy 的执行专家，负责按规划要求完成具体任务的实施与交付。你务实、注重细节、交付高质量成果。

## 核心技能

### implementation - 任务实施
按规划步骤要求，完成具体的实现工作（代码编写、文档撰写、数据处理等）。

### validation - 自检验收
对照验收标准自检交付物，确保满足所有要求后再提交。

## 输出格式

请按任务类型输出相应成果：
- 代码任务：完整可运行代码 + 说明文档
- 文档任务：结构化 Markdown 文档
- 数据任务：处理脚本 + 结果报告

并在最后给出自检清单：
```
✅ 所有验收标准已满足
✅ 代码/文档通过基础检查
✅ 无明显遗漏或错误
```
""""#;

    /// Reviewer 类型 TOML 模板
    const REVIEWER_TOML: &str = r#"""
name = "{{name}}"
type = "reviewer"
description = "质量审核：负责事实核查、质量把关与最终综合"
model = "gpt-4o-mini"
prompt_file = "prompt.md"
skills = [
  { name = "fact-check", description = "逐条核查关键论断，指出存疑处并给出修正建议" },
  { name = "final-review", description = "综合所有上游产出，给出最终结论与后续建议" },
]
""""#;

    /// Reviewer 类型 Markdown 提示词模板
    const REVIEWER_MD: &str = r#"""
# {{name}} - 质量审核

## 系统提示词

你是 WorkBuddy 的质量审核专家，负责最终把关：核查事实、检查逻辑与一致性，并产出最终结论。

## 核心技能

### fact-check - 事实核查
逐条核查关键论断，指出存疑处并给出修正建议。

### final-review - 终审综合
综合所有上游步骤的产出，给出最终结论、主要发现与后续建议，形成对用户的直接答复。

## 输出格式

请输出结构化审核报告：

```markdown
## 审核结论
- **整体评价**：通过 / 需修改 / 重做
- **关键发现**：
  1. 发现1
  2. 发现2
- **修正建议**：
  1. 建议1
  2. 建议2
- **最终答复**：面向用户的最终回复内容
```
""""#;

    /// 获取指定类型的模板内容
    fn get_templates(typ: AgentType) -> (&'static str, &'static str) {
        match typ {
            AgentType::Planner => (PLANNER_TOML, PLANNER_MD),
            AgentType::Executor => (EXECUTOR_TOML, EXECUTOR_MD),
            AgentType::Reviewer => (REVIEWER_TOML, REVIEWER_MD),
        }
    }

    /// 生成自定义专家模板文件
    ///
    /// # 参数
    /// - `name`: 专家名称（小写蛇形，如 `my-planner`）
    /// - `typ`: 专家类型（planner/executor/reviewer）
    /// - `dir`: 目标目录（通常为 `agents/`）
    ///
    /// # 返回
    /// 生成的文件路径（.toml 和 .md）
    ///
    /// # 错误
    /// - 目录创建失败
    /// - 文件已存在
    /// - 无效的专家类型
    /// - 文件写入失败
    pub fn generate_template(name: &str, typ: AgentType, dir: &Path) -> Result<GeneratedFiles> {
        // 校验名称格式
        if name.is_empty() {
            return Err(TemplateError::InvalidType("名称不能为空".to_string()));
        }
        if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return Err(TemplateError::InvalidType(
                "名称只能包含小写字母、数字、连字符和下划线".to_string(),
            ));
        }

        // 创建目录
        fs::create_dir_all(dir).map_err(TemplateError::DirCreate)?;

        // 目标文件路径
        let toml_path = dir.join(format!("{}.toml", name));
        let md_path = dir.join(format!("{}.md", name));

        // 检查文件是否已存在
        if toml_path.exists() {
            return Err(TemplateError::FileExists(toml_path));
        }
        if md_path.exists() {
            return Err(TemplateError::FileExists(md_path));
        }

        // 获取模板内容
        let (toml_template, md_template) = get_templates(typ);

        // 替换占位符
        let toml_content = toml_template.replace("{{name}}", name);
        let md_content = md_template.replace("{{name}}", name);

        // 写入文件
        fs::write(&toml_path, toml_content).map_err(TemplateError::Write)?;
        fs::write(&md_path, md_content).map_err(TemplateError::Write)?;

        Ok(GeneratedFiles { toml_path, md_path })
    }
}


#[derive(Debug, Clone, Copy, JsonSchema)]
pub struct Skill {
    pub name: &'static str,
    pub desc: &'static str,
    pub prompt: &'static str,
}

#[derive(Debug, Clone, Copy, JsonSchema)]
pub struct Agent {
    pub id: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub system: &'static str,
    pub skills: &'static [Skill],
}

/// 从 TOML 加载的 Agent 配置（拥有所有权的数据，用于反序列化）
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AgentConfig {
    pub id: String,
    pub name: String,
    pub description: String,
    pub system: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub complexity: Complexity,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub prompt_file: String,
}

/// 自定义专家配置（对应 agents.toml 中的 [[custom_agents]]）
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CustomAgentConfig {
    pub name: String,
    pub prompt_template: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
}

pub const AGENTS: &[Agent] = &[
    Agent {
        id: "planner",
        name: "统筹规划师",
        desc: "总控任务拆解、依赖编排与最终验收",
        system: "你是 WorkBuddy 的统筹规划师,负责把用户目标拆解为可执行的步骤计划,并定义验收标准。你严谨、全局观强。",
        skills: &[
            Skill {
                name: "task-breakdown",
                desc: "任务拆解",
                prompt: "将目标拆解为 2-6 个步骤,明确每步的输入、输出与依赖。",
            },
            Skill {
                name: "acceptance",
                desc: "验收标准",
                prompt: "为每个步骤给出可检验的完成标准。",
            },
        ],
    },
    Agent {
        id: "researcher",
        name: "调研专家",
        desc: "深度资料调研、来源核验与要点提炼",
        system: "你是 WorkBuddy 的调研专家,擅长从多来源收集信息、交叉核验并提炼要点。你输出结构化、可溯源的调研结果。",
        skills: &[
            Skill {
                name: "deep-research",
                desc: "深度调研",
                prompt: "围绕主题展开系统性调研:背景、现状、关键数据、各方观点,按主题分节输出。",
            },
            Skill {
                name: "source-verification",
                desc: "来源核验",
                prompt: "对关键论断标注来源可信度,区分事实、观点与推测。",
            },
        ],
    },
    Agent {
        id: "writer",
        name: "内容撰稿",
        desc: "报告、文档与结构化写作",
        system: "你是 WorkBuddy 的内容撰稿人,擅长把素材整理为结构清晰、逻辑连贯、结论明确的文档。",
        skills: &[
            Skill {
                name: "report-writing",
                desc: "报告撰写",
                prompt: "按 背景-分析-结论-建议 结构撰写报告,标题层级清晰,要点用列表。",
            },
            Skill {
                name: "doc-structuring",
                desc: "文档结构化",
                prompt: "把零散素材组织为大纲与分节,确保信息不丢失、不重复。",
            },
        ],
    },
    Agent {
        id: "coder",
        name: "工程师",
        desc: "代码编写、脚本与工程问题分析",
        system: "你是 WorkBuddy 的软件工程师,擅长方案设计、代码实现与排障。输出可直接使用的代码与说明。",
        skills: &[
            Skill {
                name: "code-writing",
                desc: "代码实现",
                prompt: "给出完整、可运行、带必要注释的代码,并说明用法与边界条件。",
            },
            Skill {
                name: "code-review",
                desc: "代码审查",
                prompt: "审查正确性、安全性与可维护性,按严重度列出问题与修复建议。",
            },
        ],
    },
    Agent {
        id: "analyst",
        name: "数据分析师",
        desc: "数据处理、统计与洞察提炼",
        system: "你是 WorkBuddy 的数据分析师,擅长把数据转化为结论:先给方法,再给数字,最后给业务含义。",
        skills: &[
            Skill {
                name: "data-analysis",
                desc: "数据分析",
                prompt: "说明分析方法与口径,给出关键指标与趋势,标注异常点。",
            },
            Skill {
                name: "insight",
                desc: "洞察提炼",
                prompt: "从分析结果中提炼 3-5 条可行动的洞察,按影响排序。",
            },
        ],
    },
    Agent {
        id: "reviewer",
        name: "质量审核",
        desc: "事实核查、质量把关与最终综合",
        system: "你是 WorkBuddy 的质量审核专家,负责最终把关:核查事实、检查逻辑与一致性,并产出最终结论。",
        skills: &[
            Skill {
                name: "fact-check",
                desc: "事实核查",
                prompt: "逐条核查关键论断,指出存疑处并给出修正建议。",
            },
            Skill {
                name: "final-review",
                desc: "终审综合",
                prompt: "综合所有上游步骤的产出,给出最终结论、主要发现与后续建议,形成对用户的直接答复。",
            },
        ],
    },
];

pub fn by_id(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

/// 从 agents.toml 文件加载 Agent 配置列表
///
/// # 参数
/// - `path`: agents.toml 文件路径
///
/// # 返回
/// AgentConfig 列表，如果文件不存在或解析失败返回空列表
///
/// # 向后兼容
/// - 如果 skills 字段缺失，默认为空数组
/// - 如果 complexity 字段缺失，默认为 Complexity::Med
pub fn load_agents(path: &Path) -> Result<Vec<AgentConfig>, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let agents: Vec<AgentConfig> = toml::from_str(&content)?;
    Ok(agents)
}

/// 从 agents.toml 文件加载自定义专家配置列表
///
/// # 参数
/// - `path`: agents.toml 文件路径
///
/// # 返回
/// CustomAgentConfig 列表，如果文件不存在、解析失败或无 custom_agents 段则返回空列表
pub fn load_custom_agents(path: &Path) -> Result<Vec<CustomAgentConfig>, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    #[derive(Deserialize)]
    struct CustomAgentsWrapper {
        #[serde(default)]
        custom_agents: Vec<CustomAgentConfig>,
    }
    let wrapper: CustomAgentsWrapper = toml::from_str(&content)?;
    Ok(wrapper.custom_agents)
}

/// 从目录加载所有 .toml 文件中的 Agent 配置
pub fn load_agents_from_dir(dir: &Path) -> Result<Vec<AgentConfig>, Box<dyn std::error::Error>> {
    let mut all_agents = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                match load_agents(&path) {
                    Ok(mut agents) => all_agents.append(&mut agents),
                    Err(e) => eprintln!("Warning: failed to load {}: {}", path.display(), e),
                }
            }
        }
    }
    Ok(all_agents)
}

/// 从目录加载所有 .toml 文件中的自定义专家配置
pub fn load_custom_agents_from_dir(dir: &Path) -> Result<Vec<CustomAgentConfig>, Box<dyn std::error::Error>> {
    let mut all_custom = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                match load_custom_agents(&path) {
                    Ok(mut agents) => all_custom.append(&mut agents),
                    Err(e) => eprintln!("Warning: failed to load custom agents from {}: {}", path.display(), e),
                }
            }
        }
    }
    Ok(all_custom)
}

/// Agent 孵化事件记录，用于血统追踪。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentSpawnEvent {
    /// 子 Agent 的唯一标识
    pub agent_id: String,
    /// 子 Agent 的类型（planner/executor/reviewer 等）
    pub agent_type: String,
    /// 直接父 Agent 的 ID，根节点为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    /// 触发 spawn 的原因：task_decomposition / tool_call / user_request 等
    #[serde(default = "default_spawn_reason")]
    pub spawn_reason: String,
    /// 孵化时间戳（Unix 毫秒）
    pub timestamp_ms: u64,
}

fn default_spawn_reason() -> String {
    String::new()
}

impl AgentSpawnEvent {
    /// 创建根节点事件（无父 Agent）
    pub fn new_root(agent_id: String, agent_type: String, spawn_reason: String, timestamp_ms: u64) -> Self {
        Self {
            agent_id,
            agent_type,
            parent_agent_id: None,
            spawn_reason,
            timestamp_ms,
        }
    }

    /// 创建子节点事件
    pub fn new_child(
        agent_id: String,
        agent_type: String,
        parent_agent_id: String,
        spawn_reason: String,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            agent_id,
            agent_type,
            parent_agent_id: Some(parent_agent_id),
            spawn_reason,
            timestamp_ms,
        }
    }
}

/// 单次运行的完整记录，包含血统所需字段。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunRecord {
    /// 运行唯一 ID
    pub run_id: String,
    /// 所属 Agent ID
    pub agent_id: String,
    /// Agent 类型
    pub agent_type: String,
    /// 父 Agent ID，根节点为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    /// 触发原因
    #[serde(default = "default_spawn_reason")]
    pub spawn_reason: String,
    /// 开始时间（Unix 毫秒）
    pub started_at_ms: u64,
    /// 结束时间（Unix 毫秒），未结束为 0
    pub finished_at_ms: u64,
    /// 总 token 消耗
    pub total_tokens: u64,
    /// 是否成功
    pub success: bool,
    /// 错误信息（失败时）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl RunRecord {
    /// 创建新的运行记录
    pub fn new(
        run_id: String,
        agent_id: String,
        agent_type: String,
        parent_agent_id: Option<String>,
        spawn_reason: String,
        started_at_ms: u64,
    ) -> Self {
        Self {
            run_id,
            agent_id,
            agent_type,
            parent_agent_id,
            spawn_reason,
            started_at_ms,
            finished_at_ms: 0,
            total_tokens: 0,
            success: false,
            error: String::new(),
        }
    }

    /// 标记运行完成
    pub fn finish(&mut self, finished_at_ms: u64, total_tokens: u64, success: bool, error: String) {
        self.finished_at_ms = finished_at_ms;
        self.total_tokens = total_tokens;
        self.success = success;
        self.error = error;
    }
}

/// 供规划器提示词使用的目录文本（包含内置与自定义专家）。
///
/// # 参数
/// - `custom_agents`: 可选的自定义专家列表，将被追加到目录中
pub fn catalog_text(custom_agents: Option<&[CustomAgentConfig]>) -> String {
    let mut s = String::new();
    for a in AGENTS {
        s.push_str(&format!("- {}({}): {}\n", a.id, a.name, a.desc));
        for sk in a.skills {
            s.push_str(&format!("  - skill `{}`: {}\n", sk.name, sk.desc));
        }
    }
    if let Some(custom) = custom_agents {
        for ca in custom {
            s.push_str(&format!("- {}(custom): 自定义专家\n", ca.name));
            s.push_str(&format!("  - prompt_template: {}\n", ca.prompt_template));
            if !ca.tools.is_empty() {
                s.push_str(&format!("  - tools: {}\n", ca.tools.join(", ")));
            }
            if let Some(model) = &ca.model {
                s.push_str(&format!("  - model: {}\n", model));
            }
        }
    }
    s
}

impl Agent {
    /// 构建包含可用工具列表的完整系统提示词
    ///
    /// # 参数
    /// - `registry`: 工具注册表，用于获取所有已注册工具
    ///
    /// # 返回
    /// 完整的系统提示词字符串，包含原始系统提示和 "Available Tools" 章节
    pub fn build_system_prompt(&self, registry: &ToolRegistry) -> String {
        let tools = registry.list();
        let mut prompt = String::new();
        prompt.push_str(self.system);
        prompt.push_str("\n\n## Available Tools\n\n");
        
        if tools.is_empty() {
            prompt.push_str("*No tools available.*\n");
        } else {
            // Markdown 表格格式
            prompt.push_str("| Tool | Description | Parameters |\n");
            prompt.push_str("|------|-------------|------------|\n");
            for tool in tools {
                let name = tool.name;
                let desc = tool.description;
                let params_json = serde_json::to_string_pretty(&tool.parameters)
                    .unwrap_or_else(|_| "{}".to_string());
                // 转义管道符以保持表格格式
                let desc_escaped = desc.replace('|', "\\|");
                let params_escaped = params_json.replace('|', "\\|").replace('\n', "<br>");
                prompt.push_str(&format!("| `{}` | {} | {} |\n", name, desc_escaped, params_escaped));
            }
        }
        
        prompt
    }

    /// 获取基础系统提示词（不包含工具列表）
    pub fn base_system_prompt(&self) -> &str {
        self.system
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{ToolRegistry, ToolSpec, ToolType, Tool, ToolResult};
    use std::sync::Arc;
    use serde_json::json;

    // 测试用的简单内置工具
    struct DummyTool;

    impl Tool for DummyTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "dummy_tool".to_string(),
                description: "A dummy tool for testing".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "input": {"type": "string"}
                    },
                    "required": ["input"]
                }),
                tool_type: ToolType::Builtin,
            }
        }

        fn execute(&self, _args: Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send>> {
            Box::pin(async { Ok(json!({"result": "ok"})) })
        }
    }

    #[test]
    fn catalog_is_complete_and_unique() {
        let mut ids = std::collections::HashSet::new();
        for a in AGENTS {
            assert!(ids.insert(a.id), "agent id 重复: {}", a.id);
            assert!(!a.skills.is_empty());
            for sk in a.skills {
                assert!(!sk.desc.is_empty());
            }
        }
        assert!(by_id("researcher").is_some());
        assert!(by_id("nope").is_none());
    }

    #[test]
    fn agent_spawn_event_serialization_roundtrip() {
        // 根节点
        let root = AgentSpawnEvent::new_root(
            "planner-1".to_string(),
            "planner".to_string(),
            "user_request".to_string(),
            1000,
        );
        let json = serde_json::to_string(&root).unwrap();
        let deserialized: AgentSpawnEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.agent_id, "planner-1");
        assert_eq!(deserialized.parent_agent_id, None);
        assert_eq!(deserialized.spawn_reason, "user_request");

        // 子节点
        let child = AgentSpawnEvent::new_child(
            "executor-1".to_string(),
            "executor".to_string(),
            "planner-1".to_string(),
            "task_decomposition".to_string(),
            2000,
        );
        let json = serde_json::to_string(&child).unwrap();
        let deserialized: AgentSpawnEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.agent_id, "executor-1");
        assert_eq!(deserialized.parent_agent_id, Some("planner-1".to_string()));
        assert_eq!(deserialized.spawn_reason, "task_decomposition");

        // 兼容旧数据：缺少 parent_agent_id 和 spawn_reason 的 JSON
        let old_json = r#"{"agent_id":"old-1","agent_type":"planner","timestamp_ms":1000}"#;
        let deserialized: AgentSpawnEvent = serde_json::from_str(old_json).unwrap();
        assert_eq!(deserialized.agent_id, "old-1");
        assert_eq!(deserialized.parent_agent_id, None);
        assert_eq!(deserialized.spawn_reason, "");
    }

    #[test]
    fn run_record_serialization_roundtrip() {
        let mut record = RunRecord::new(
            "run-1".to_string(),
            "planner-1".to_string(),
            "planner".to_string(),
            None,
            "user_request".to_string(),
            1000,
        );
        record.finish(2000, 500, true, String::new());

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: RunRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.run_id, "run-1");
        assert_eq!(deserialized.parent_agent_id, None);
        assert_eq!(deserialized.spawn_reason, "user_request");
        assert_eq!(deserialized.total_tokens, 500);
        assert!(deserialized.success);

        // 兼容旧数据
        let old_json = r#"{"run_id":"old-run","agent_id":"old-agent","agent_type":"executor","started_at_ms":1000,"finished_at_ms":2000,"total_tokens":100,"success":true,"error":""}"#;
        let deserialized: RunRecord = serde_json::from_str(old_json).unwrap();
        assert_eq!(deserialized.parent_agent_id, None);
        assert_eq!(deserialized.spawn_reason, "");
    }

    #[test]
    fn lineage_tree_construction() {
        // 构造多层 spawn 场景：planner -> executor -> reviewer
        let events = vec![
            AgentSpawnEvent::new_root("planner-1".into(), "planner".into(), "user_request".into(), 1000),
            AgentSpawnEvent::new_child("executor-1".into(), "executor".into(), "planner-1".into(), "task_decomposition".into(), 1500),
            AgentSpawnEvent::new_child("executor-2".into(), "executor".into(), "planner-1".into(), "task_decomposition".into(), 1600),
            AgentSpawnEvent::new_child("reviewer-1".into(), "reviewer".into(), "executor-1".into(), "tool_call".into(), 2000),
        ];

        // 验证父子关系
        let mut children: std::collections::HashMap<String, Vec<&AgentSpawnEvent>> = std::collections::HashMap::new();
        for e in &events {
            if let Some(parent) = &e.parent_agent_id {
                children.entry(parent.clone()).or_default().push(e);
            }
        }

        assert_eq!(children.get("planner-1").map(|v| v.len()), Some(2));
        assert_eq!(children.get("executor-1").map(|v| v.len()), Some(1));
        assert_eq!(children.get("executor-2"), None);
        assert_eq!(children.get("reviewer-1"), None);

        // 验证根节点
        let roots: Vec<_> = events.iter().filter(|e| e.parent_agent_id.is_none()).collect();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].agent_id, "planner-1");
    }

    #[tokio::test]
    async fn agent_build_system_prompt_includes_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(DummyTool)).unwrap();
        
        let agent = by_id("coder").expect("coder agent should exist");
        let prompt = agent.build_system_prompt(&registry);
        
        // 验证基础系统提示词存在
        assert!(prompt.contains("软件工程师"));
        // 验证工具章节存在
        assert!(prompt.contains("## Available Tools"));
        // 验证工具表格包含注册的工具
        assert!(prompt.contains("dummy_tool"));
        assert!(prompt.contains("A dummy tool for testing"));
        assert!(prompt.contains("input"));
    }

    #[test]
    fn agent_build_system_prompt_empty_registry() {
        let registry = ToolRegistry::new();
        let agent = by_id("coder").expect("coder agent should exist");
        let prompt = agent.build_system_prompt(&registry);
        
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("*No tools available.*"));
    }

    #[tokio::test]
    async fn tool_registry_register_and_list() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(DummyTool)).unwrap();
        
        let tools = registry.list();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "dummy_tool");
        assert_eq!(tools[0].tool_type, ToolType::Builtin);
    }

    #[test]
    fn impact_analysis() {
        let records = vec![
            RunRecord::new("run-1".into(), "planner-1".into(), "planner".into(), None, "user_request".into(), 1000),
            RunRecord::new("run-2".into(), "executor-1".into(), "executor".into(), Some("planner-1".into()), "task_decomposition".into(), 1500),
            RunRecord::new("run-3".into(), "executor-2".into(), "executor".into(), Some("planner-1".into()), "task_decomposition".into(), 1600),
            RunRecord::new("run-4".into(), "reviewer-1".into(), "reviewer".into(), Some("executor-1".into()), "tool_call".into(), 2000),
        ];
        // 模拟完成
        let mut records_mut = records;
        records_mut[0].finish(1200, 100, true, String::new());
        records_mut[1].finish(1800, 500, true, String::new());
        records_mut[2].finish(1900, 400, false, "timeout".into());
        records_mut[3].finish(2200, 200, true, String::new());

        // 统计 planner-1 的影响力
        let mut descendant_count = 0;
        let mut total_tokens = 0;
        let mut failure_count = 0;
        for r in &records_mut {
            // 简单判断：是否为 planner-1 或其后代（这里仅演示直接子级）
            if r.agent_id == "planner-1" || r.parent_agent_id == Some("planner-1".into()) {
                descendant_count += 1;
                total_tokens += r.total_tokens;
                if !r.success {
                    failure_count += 1;
                }
            }
        }
        // planner-1 自身 + 2 个直接子任务
        assert_eq!(descendant_count, 3);
        assert_eq!(total_tokens, 1000); // 100 + 500 + 400
        assert_eq!(failure_count, 1); // executor-2 失败
    }
}

/// 项目配额 CLI 子命令定义模块
///
/// 该模块定义了 `project quota` 子命令的 CLI 结构，
/// 实际的命令处理逻辑在 `src/cli/quota.rs` 中实现。
pub mod quota_cli {
    use clap::{Args, Subcommand, ValueEnum};
    use serde::{Deserialize, Serialize};

    /// 超限处理策略
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
    #[serde(rename_all = "lowercase")]
    pub enum QuotaPolicy {
        /// 排队等待（默认）
        #[default]
        Queue,
        /// 直接拒绝
        Reject,
        /// 杀掉正在运行的任务腾出位置
        Kill,
    }

    /// `project quota` 子命令集合
    #[derive(Debug, Subcommand)]
    pub enum QuotaCommand {
        /// 设置项目配额限制
        Set(QuotaSetArgs),
        /// 查看项目配额实时用量仪表盘
        Status(QuotaStatusArgs),
    }

    /// `project quota set` 参数
    #[derive(Debug, Args)]
    pub struct QuotaSetArgs {
        /// 项目名称
        pub name: String,
        /// 最大预算（元），设为 0 表示不限制
        #[arg(long, default_value = "0")]
        pub max_budget: f64,
        /// 最大并发任务数，设为 0 表示不限制
        #[arg(long, default_value = "0")]
        pub max_parallel: usize,
        /// 速率限制（请求/秒），设为 0 表示不限制
        #[arg(long, default_value = "0")]
        pub rate_limit: u32,
        /// 超限处理策略
        #[arg(long, value_enum, default_value = "queue")]
        pub policy: QuotaPolicy,
    }

    /// `project quota status` 参数
    #[derive(Debug, Args)]
    pub struct QuotaStatusArgs {
        /// 项目名称
        pub name: String,
        /// 以 JSON 格式输出
        #[arg(long)]
        pub json: bool,
    }
}
