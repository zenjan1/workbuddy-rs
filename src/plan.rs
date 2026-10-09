use anyhow::Context;
use serde::{Deserialize, Serialize};

pub const MAX_STEPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Llm,
    Shell,
}

impl Default for StepKind {
    fn default() -> Self {
        StepKind::Llm
    }
}

impl StepKind {
    pub fn label(self) -> &'static str {
        match self {
            StepKind::Llm => "llm",
            StepKind::Shell => "shell",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: usize,
    pub name: String,
    pub agent: String,
    pub skill: String,
    pub prompt: String,
    #[serde(default)]
    pub kind: StepKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub needs: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub goal: String,
    pub steps: Vec<Step>,
}

impl Plan {
    /// 从 LLM 原始输出解析并校验计划(容忍 markdown 围栏等额外文字)。
    pub fn from_llm(goal: &str, raw: &str) -> anyhow::Result<Self> {
        let json_text = extract_json_object(raw)?;
        let mut p: Plan =
            serde_json::from_str(&json_text).context("解析计划 JSON 失败")?;
        // 计划目标以用户原始目标为准,LLM 的 goal 字段仅作文本参考
        p.goal = goal.to_string();
        p.steps.sort_by_key(|s| s.id);
        // 未知 skill 回退为该专家的第一个 skill(容忍模型轻微幻觉)
        for s in &mut p.steps {
            if let Some(agent) = crate::agents::by_id(&s.agent) {
                if !agent.skills.iter().any(|sk| sk.name == s.skill) {
                    s.skill = agent.skills[0].name.to_string();
                }
            }
        }
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.steps.is_empty() {
            anyhow::bail!("计划没有步骤");
        }
        if self.steps.len() > MAX_STEPS {
            anyhow::bail!("步骤过多({}),上限 {MAX_STEPS}", self.steps.len());
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.steps {
            if s.id == 0 {
                anyhow::bail!("步骤 id 从 1 开始");
            }
            if !seen.insert(s.id) {
                anyhow::bail!("步骤 id 重复: {}", s.id);
            }
            if crate::agents::by_id(&s.agent).is_none() {
                anyhow::bail!("步骤 {} 引用未知专家: {}", s.id, s.agent);
            }
            if s.prompt.trim().is_empty() {
                anyhow::bail!("步骤 {} 缺少 prompt", s.id);
            }
            for n in &s.needs {
                if *n >= s.id {
                    anyhow::bail!(
                        "步骤 {} 的依赖 {} 必须引用更小的 id",
                        s.id,
                        n
                    );
                }
            }
        }
        let ids: std::collections::HashSet<_> =
            self.steps.iter().map(|s| s.id).collect();
        for s in &self.steps {
            for n in &s.needs {
                if !ids.contains(n) {
                    anyhow::bail!("步骤 {} 依赖不存在的步骤 {}", s.id, n);
                }
            }
        }
        Ok(())
    }

    pub fn step(&self, id: usize) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn step_name(&self, id: usize) -> &str {
        self.step(id).map(|s| s.name.as_str()).unwrap_or("?")
    }

    /// 拓扑序。validate 已保证 needs 只引用更小的 id,故按 id 排序即拓扑序。
    pub fn topo_order(&self) -> anyhow::Result<Vec<usize>> {
        let mut order: Vec<usize> = self.steps.iter().map(|s| s.id).collect();
        order.sort_unstable();
        Ok(order)
    }

    /// 人类可读的计划摘要。
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for s in &self.steps {
            let needs = if s.needs.is_empty() {
                "无".to_string()
            } else {
                s.needs
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            out.push_str(&format!(
                "  {}. [{}] {} ({} / {})\n     依赖: {}\n",
                s.id, s.agent, s.name, s.skill, s.kind.label(), needs
            ));
        }
        out
    }
}

fn extract_json_object(raw: &str) -> anyhow::Result<String> {
    let start = raw.find('{').context("响应中未找到 JSON 对象")?;
    let end = raw
        .rfind('}')
        .filter(|e| *e > start)
        .context("JSON 对象不完整")?;
    Ok(raw[start..=end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{ "goal": "测试", "steps": [
        {"id":1,"name":"调研","agent":"researcher","skill":"deep-research","prompt":"查资料","kind":"llm","needs":[]},
        {"id":2,"name":"终审","agent":"reviewer","skill":"final-review","prompt":"总结","kind":"llm","needs":[1]}
    ]}"#;

    #[test]
    fn parse_good_plan() {
        let p = Plan::from_llm("测试", GOOD).unwrap();
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.topo_order().unwrap(), vec![1, 2]);
    }

    #[test]
    fn parse_plan_with_markdown_fence() {
        let raw = format!("好的,计划如下:\n```json\n{GOOD}\n```\n以上。");
        let p = Plan::from_llm("测试", &raw).unwrap();
        assert_eq!(p.steps.len(), 2);
    }

    #[test]
    fn reject_unknown_agent() {
        let bad = r#"{ "goal": "x", "steps": [
            {"id":1,"name":"a","agent":"ghost","skill":"s","prompt":"p","kind":"llm","needs":[]}
        ]}"#;
        assert!(Plan::from_llm("x", bad).is_err());
    }

    #[test]
    fn reject_forward_dependency() {
        let bad = r#"{ "goal": "x", "steps": [
            {"id":1,"name":"a","agent":"writer","skill":"report-writing","prompt":"p","kind":"llm","needs":[2]},
            {"id":2,"name":"b","agent":"reviewer","skill":"final-review","prompt":"p","kind":"llm","needs":[]}
        ]}"#;
        assert!(Plan::from_llm("x", bad).is_err());
    }

    #[test]
    fn reject_missing_dependency() {
        let bad = r#"{ "goal": "x", "steps": [
            {"id":2,"name":"b","agent":"reviewer","skill":"final-review","prompt":"p","kind":"llm","needs":[1]}
        ]}"#;
        assert!(Plan::from_llm("x", bad).is_err());
    }

    #[test]
    fn fallback_unknown_skill_to_first() {
        let raw = r#"{ "goal": "x", "steps": [
            {"id":1,"name":"a","agent":"coder","skill":"不存在的技能","prompt":"p","kind":"llm","needs":[]}
        ]}"#;
        let p = Plan::from_llm("x", raw).unwrap();
        assert_eq!(p.steps[0].skill, "code-writing");
    }

    #[test]
    fn reject_empty_steps() {
        let bad = r#"{ "goal": "x", "steps": [] }"#;
        assert!(Plan::from_llm("x", bad).is_err());
    }
}
