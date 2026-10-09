use anyhow::Result;
use std::sync::Arc;

use crate::agents;
use crate::llm::Llm;
use crate::plan::{Plan, MAX_STEPS};

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
    Plan::from_llm(goal, &raw)
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
}
