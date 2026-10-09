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
}
