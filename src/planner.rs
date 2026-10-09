use anyhow::Result;
use std::sync::Arc;

use crate::agents;
use crate::llm::Llm;
use crate::plan::{Plan, StepKind, MAX_STEPS};
use crate::util::clip;

pub const PLANNER_SYSTEM: &str = "你是 WorkBuddy 的总规划师(Planner),负责把用户目标拆解为可执行的多智能体协作计划。\n\
只输出一个 JSON 对象(不要 markdown 代码块、不要任何额外文字),结构如下:\n\
{\n  \"goal\": \"一句话任务目标\",\n  \"steps\": [\n    {\"id\": 1, \"name\": \"步骤名\", \"agent\": \"专家id\", \"skill\": \"skill名\", \"prompt\": \"给该专家的完整任务说明\", \"kind\": \"llm\", \"needs\": []}\n  ]\n}\n\n\
规则:\n\
- steps 数量 2 到 {MAX_STEPS},id 从 1 连续递增。\n\
- agent 只能取自下方专家目录,skill 只能取自该专家的 skill 列表。\n\
- kind 默认 \"llm\";只有当步骤本质是运行命令时才用 \"shell\",且必须给出 command 字段。\n\
- needs 列出该步骤依赖的其他步骤 id(上游产出会作为上下文提供),必须引用更小的 id;无依赖给空数组。\n\
- 最后一个步骤应由 reviewer 的 final-review 负责综合所有上游产出,形成对用户的直接答复。\n\n\
专家目录:\n{catalog}";

/// 调用 LLM 规划器,产出并校验一份执行计划。
pub async fn plan_task(llm: &Arc<dyn Llm>, goal: &str) -> Result<Plan> {
    let system = PLANNER_SYSTEM
        .replace("{MAX_STEPS}", &MAX_STEPS.to_string())
        .replace("{catalog}", &agents::catalog_text());
    let user = format!("用户目标:\n{}\n\n请输出 JSON 计划。", goal);
    let raw = llm.chat(&system, &user).await?;
    Plan::from_llm(goal, &raw)
}

/// dev 模式规划提示词:针对"在代码仓库里完成一个开发任务"。
/// 步骤类型允许 read/edit/shell,要求先读后改、最后验证。
pub const DEV_PLANNER_SYSTEM: &str = "你是 WorkBuddy 的总规划师(Planner),负责把【开发任务】拆解为可在当前代码仓库中执行的多智能体计划。\n\
只输出一个 JSON 对象(不要 markdown 代码块、不要任何额外文字),结构如下:\n\
{\n  \"goal\": \"一句话任务目标\",\n  \"steps\": [\n    {\"id\": 1, \"name\": \"步骤名\", \"agent\": \"专家id\", \"skill\": \"skill名\", \"prompt\": \"给该专家的完整任务说明\", \"kind\": \"llm\", \"needs\": []}\n  ]\n}\n\n\
规则:\n\
- steps 数量 2 到 {MAX_STEPS},id 从 1 连续递增。\n\
- agent 只能取自下方专家目录,skill 只能取自该专家的 skill 列表。\n\
- kind 取值:\n\
  - \"llm\": 纯推理/分析/设计,产出文字结论;\n\
  - \"read\": 读取仓库文件(path=仓库相对路径),产出文件内容作为下游上下文;\n\
  - \"edit\": 修改/新建仓库文件(path=目标文件相对路径)。该步骤会先自动读取 path 文件内容,再由 LLM 输出 edits JSON 应用。prompt 里必须写明要改什么、为什么;\n\
  - \"shell\": 运行命令(command),例如 `cargo build`、`cargo test`、`git status`。只允许只读/构建/测试/本地 git 提交类命令,禁止 push/force/删除/格式化。\n\
- needs 列出依赖的其他步骤 id,必须引用更小的 id;无依赖给空数组。\n\
- 编排原则:先用 read 步骤查看要改的文件(或让 edit 步骤自带文件内容),再 edit,最后必须用 shell 步骤运行验证命令(如 `cargo test`)确认改动可用。\n\
- 最后一个步骤应为 shell 验证或 reviewer 综合,确保改动被验证。\n\
- 每个 edit 步骤聚焦最小改动,不要重写整个文件。\n\n\
专家目录:\n{catalog}";

/// dev 模式:规划一个"在仓库里完成开发任务"的计划。
pub async fn plan_dev_task(llm: &Arc<dyn Llm>, goal: &str) -> Result<Plan> {
    let system = DEV_PLANNER_SYSTEM
        .replace("{MAX_STEPS}", &MAX_STEPS.to_string())
        .replace("{catalog}", &agents::catalog_text());
    let user = format!("开发任务(在当前代码仓库中完成):\n{}\n\n请输出 JSON 计划。", goal);
    let raw = llm.chat(&system, &user).await?;
    let mut plan = Plan::parse_from_llm(goal, &raw)?;
    repair_dev_plan(&mut plan)?;
    Ok(plan)
}

/// 容忍修复 dev 计划:模型偶发漏给 read/edit 步骤 path。
/// - read 缺 path:尝试从 prompt 提取文件路径;提取不到则降级为 llm 步骤(read 本就只是上下文辅助);
/// - edit 缺 path:必须能提取,否则报错触发重新规划。
fn repair_dev_plan(plan: &mut Plan) -> Result<()> {
    // 未知专家回退:模型常把 kind 值(shell/read/edit)幻觉进 agent 字段,按类型映射到合适专家
    for s in plan.steps.iter_mut() {
        if crate::agents::by_id(&s.agent).is_none() {
            s.agent = match s.kind {
                StepKind::Llm => "reviewer".to_string(),
                _ => "coder".to_string(),
            };
        }
    }
    for s in plan.steps.iter_mut() {
        if s.path.as_ref().is_some_and(|p| !p.trim().is_empty()) {
            continue;
        }
        match s.kind {
            StepKind::Read => match extract_path_from_prompt(&s.prompt) {
                Some(p) => s.path = Some(p),
                None => s.kind = StepKind::Llm,
            },
            StepKind::Edit => {
                s.path = Some(extract_path_from_prompt(&s.prompt).ok_or_else(|| {
                    anyhow::anyhow!(
                        "edit 步骤 {} 缺少 path 且无法从 prompt 提取文件路径(名称: {})",
                        s.id,
                        clip(&s.name, 40)
                    )
                })?);
            }
            _ => {}
        }
    }
    // shell 缺 command:从 prompt/name 提取命令;提取不到则降级为 llm(该步骤仅产出文字)
    for s in plan.steps.iter_mut() {
        if s.kind != StepKind::Shell {
            continue;
        }
        if s.command.as_ref().is_some_and(|c| !c.trim().is_empty()) {
            continue;
        }
        let text = format!("{}\n{}", s.name, s.prompt);
        if let Some(cmd) = extract_shell_command(&text) {
            s.command = Some(cmd);
        } else {
            s.kind = StepKind::Llm;
            s.command = None;
        }
    }
    plan.validate()
}

/// 从自由文本中提取一个 shell 命令:优先 `...` 或 "命令:" 之后的片段,
/// 否则取以已知命令动词开头的 token 序列。
fn extract_shell_command(text: &str) -> Option<String> {
    // 1) 反引号包裹
    if let Some(start) = text.find('`') {
        if let Some(end) = text[start + 1..].find('`') {
            let c = text[start + 1..start + 1 + end].trim();
            if !c.is_empty() && c.chars().count() <= 120 {
                return Some(c.to_string());
            }
        }
    }
    // 2) "命令: xxx" 形式
    for marker in ["命令:", "命令:", "command:", "command:"] {
        if let Some(i) = text.find(marker) {
            let rest = text[i + marker.len()..].trim();
            let line = rest.lines().next().unwrap_or("").trim().trim_end_matches('。').trim();
            if !line.is_empty() && line.chars().count() <= 120 && !line.contains(' ') {
                return Some(line.to_string());
            }
            if !line.is_empty() && line.chars().count() <= 120 {
                return Some(line.to_string());
            }
        }
    }
    // 3) 已知命令动词开头的行
    let verbs = [
        "cargo", "rustc", "git", "echo", "ls", "grep", "sh", "bash", "python", "make",
    ];
    for line in text.lines() {
        let l = line.trim().trim_start_matches(|c| c == '-' || c == '*').trim();
        if l.is_empty() {
            continue;
        }
        let head = l.split_whitespace().next().unwrap_or("");
        if verbs.contains(&head) {
            let c: String = l.chars().take(120).collect();
            if c.split_whitespace().count() >= 1 && c.split_whitespace().count() <= 8 {
                return Some(c);
            }
        }
    }
    None
}

/// 从自由文本中提取一个看起来像仓库文件路径的 token(含已知扩展名或含 '/')。
fn extract_path_from_prompt(text: &str) -> Option<String> {
    for tok in text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '/' || c == '.' || c == '_' || c == '-'))
    {
        let mut t = tok.trim_matches('/').to_string();
        while t.ends_with('.') || t.ends_with('-') {
            t.pop();
        }
        if t.len() < 3 || t.is_empty() {
            continue;
        }
        let known_ext = [
            ".rs", ".toml", ".md", ".json", ".py", ".ts", ".js", ".lock", ".sh", ".yml", ".yaml",
        ]
        .iter()
        .any(|e| t.ends_with(e));
        if known_ext || (t.contains('/') && t.matches('/').count() <= 3) {
            return Some(t);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockLlm;

    #[tokio::test]
    async fn mock_planner_produces_valid_plan() {
        let llm: Arc<dyn Llm> = Arc::new(MockLlm);
        let plan = plan_task(&llm, "写一份调研简报").await.unwrap();
        assert!(!plan.steps.is_empty());
        assert!(plan.goal.contains("调研简报"));
        // 最后一步应为 reviewer 终审
        let last = plan.steps.last().unwrap();
        assert_eq!(last.agent, "reviewer");
    }

    #[tokio::test]
    async fn mock_dev_planner_produces_valid_dev_plan() {
        let llm: Arc<dyn Llm> = Arc::new(MockLlm);
        let plan = plan_dev_task(&llm, "给项目加一个文档").await.unwrap();
        assert_eq!(plan.steps.len(), 3);
        let kinds: Vec<_> = plan.steps.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&crate::plan::StepKind::Read));
        assert!(kinds.contains(&crate::plan::StepKind::Edit));
        assert!(kinds.contains(&crate::plan::StepKind::Shell));
    }

    #[tokio::test]
    async fn test_dev_planner_system_replacement() {
        let system = DEV_PLANNER_SYSTEM
            .replace("{MAX_STEPS}", &MAX_STEPS.to_string())
            .replace("{catalog}", &agents::catalog_text());
        assert!(!system.contains("{MAX_STEPS}"));
        assert!(!system.contains("{catalog}"));
    }

    #[test]
    fn extract_path_from_prompt_finds_file_tokens() {
        assert_eq!(
            extract_path_from_prompt("先读 src/llm.rs 了解结构"),
            Some("src/llm.rs".to_string())
        );
        assert_eq!(
            extract_path_from_prompt("修改 README.md 增加一节"),
            Some("README.md".to_string())
        );
        assert_eq!(
            extract_path_from_prompt("在 Cargo.toml 里加依赖"),
            Some("Cargo.toml".to_string())
        );
        assert_eq!(extract_path_from_prompt("做一个整体设计"), None);
    }

    /// 测试辅助:直接反序列化 Plan(不走 from_llm 的 validate),
    /// 以便构造"缺 path"这种 from_llm 会拒绝的中间态来测试修复函数。
    /// 入参可为完整对象或裸步骤数组。
    fn raw_plan(json: &str) -> Plan {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let v = if v.is_array() {
            serde_json::json!({"goal": "t", "steps": v})
        } else {
            v
        };
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn repair_dev_plan_heals_missing_path() {
        // read 缺 path 但 prompt 含路径 → 补上
        let raw = r#"[
            {"id":1,"name":"读代码","agent":"coder","skill":"code-writing","prompt":"读 src/llm.rs 的结构","kind":"read","needs":[]},
            {"id":2,"name":"总结","agent":"reviewer","skill":"final-review","prompt":"总结","kind":"llm","needs":[1]}
        ]"#;
        let mut p = raw_plan(raw);
        assert!(p.steps[0].path.is_none());
        repair_dev_plan(&mut p).unwrap();
        assert_eq!(p.steps[0].path.as_deref(), Some("src/llm.rs"));

        // read 缺 path 且提取不到 → 降级为 llm,计划仍合法
        let raw2 = r#"[
            {"id":1,"name":"分析","agent":"coder","skill":"code-writing","prompt":"分析整体架构风格","kind":"read","needs":[]}
        ]"#;
        let mut p2 = raw_plan(raw2);
        repair_dev_plan(&mut p2).unwrap();
        assert_eq!(p2.steps[0].kind, StepKind::Llm);

        // edit 缺 path 且提取不到 → 报错(触发重新规划)
        let raw3 = r#"[
            {"id":1,"name":"改","agent":"coder","skill":"code-writing","prompt":"做一处修改","kind":"edit","needs":[]}
        ]"#;
        let mut p3 = raw_plan(raw3);
        assert!(repair_dev_plan(&mut p3).is_err());
    }
}
