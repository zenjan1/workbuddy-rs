use anyhow::Context;
use serde::{Deserialize, Serialize};

pub const MAX_STEPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Llm,
    Shell,
    /// 读取仓库内文件,产出作为下游上下文
    Read,
    /// LLM 产出 edits JSON,应用到仓库文件
    Edit,
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
            StepKind::Read => "read",
            StepKind::Edit => "edit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    Edit,
    Shell,
    Read,
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
    /// read/edit 步骤的目标文件(仓库相对路径)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub needs: Vec<usize>,
    /// 关联的工具类型,用于 run 模式直接调用 executor
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub goal: String,
    pub steps: Vec<Step>,
}

impl Plan {
    /// 从 LLM 原始输出解析并校验计划。容忍 markdown 围栏、额外文字,
    /// 以及模型常见的字段格式幻觉(id 为字符串/数组、needs 混入非数字等)。
    /// 解析 LLM 原始输出为计划(不做最终 validate),供 dev 模式先容忍修复再校验。
    pub fn parse_from_llm(goal: &str, raw: &str) -> anyhow::Result<Self> {
        let json_text = extract_json_object(raw)?;
        let v: serde_json::Value =
            serde_json::from_str(&json_text).context("解析计划 JSON 失败")?;

        let steps_v = v
            .get("steps")
            .and_then(|s| s.as_array())
            .context("计划缺少 steps 数组")?;

        let mut steps: Vec<Step> = Vec::new();
        for (i, sv) in steps_v.iter().enumerate() {
            let obj = sv.as_object().with_context(|| format!("步骤 {} 不是 JSON 对象", i + 1))?;
            // id:优先取数字;容忍字符串数字、数组首个元素;否则按序号补齐
            let mut id = obj
                .get("id")
                .and_then(|x| x.as_u64().or_else(|| first_number(x)))
                .map(|n| n as usize);
            if id.is_none() {
                id = Some(i + 1);
            }
            let id = id.unwrap();

            let str_field = |name: &str| {
                obj.get(name)
                    .and_then(|x| x.as_str().or_else(|| x.as_array().and_then(|a| a.first()).and_then(|f| f.as_str())))
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            let name = str_field("name");
            let agent = str_field("agent");
            let skill = str_field("skill");
            let prompt = str_field("prompt");

            let kind = match obj.get("kind").and_then(|k| k.as_str().map(|s| s.to_lowercase())).as_deref().unwrap_or("llm") {
                "shell" => StepKind::Shell,
                "read" => StepKind::Read,
                "edit" => StepKind::Edit,
                _ => StepKind::Llm,
            };
            let command = (kind == StepKind::Shell)
                .then(|| str_field("command"))
                .filter(|c| !c.is_empty());
            let path = (kind == StepKind::Read || kind == StepKind::Edit)
                .then(|| str_field("path"))
                .filter(|c| !c.is_empty());

            // needs:容忍字符串数字/数组嵌套,只保留纯数字项
            let needs: Vec<usize> = obj
                .get("needs")
                .map(|n| {
                    let mut out = Vec::new();
                    if let Some(arr) = n.as_array() {
                        for x in arr {
                            if let Some(num) = x.as_u64().or_else(|| first_number(x)) {
                                out.push(num as usize);
                            }
                        }
                    }
                    out
                })
                .unwrap_or_default();

            steps.push(Step {
                id,
                name,
                agent,
                skill,
                prompt,
                kind,
                command,
                path,
                needs,
                tool: None,
            });
        }

        let mut p = Plan {
            goal: goal.to_string(),
            steps,
        };
        p.steps.sort_by_key(|s| s.id);
        // 未知 skill 回退为该专家的第一个 skill(容忍模型轻微幻觉)
        for s in &mut p.steps {
            if let Some(agent) = crate::agents::by_id(&s.agent) {
                if !agent.skills.iter().any(|sk| sk.name == s.skill) {
                    s.skill = agent.skills[0].name.to_string();
                }
            }
        }
        Ok(p)
    }

    /// 从 LLM 原始输出解析并校验计划(校验失败返回 Err)。
    pub fn from_llm(goal: &str, raw: &str) -> anyhow::Result<Self> {
        let p = Self::parse_from_llm(goal, raw)?;
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
            match s.kind {
                StepKind::Shell => {
                    if s.command.as_deref().map(|c| c.trim().is_empty()).unwrap_or(true) {
                        anyhow::bail!("步骤 {} 是 shell 类型但缺少 command", s.id);
                    }
                }
                StepKind::Read | StepKind::Edit => {
                    if s.path.as_deref().map(|p| p.trim().is_empty()).unwrap_or(true) {
                        anyhow::bail!("步骤 {} 是 {} 类型但缺少 path", s.id, s.kind.label());
                    }
                }
                StepKind::Llm => {}
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

    /// 将用户输入解析为任务在 steps 数组中的下标。
    /// 匹配顺序：
    /// 1) 数字索引(作为 0-based 下标)
    /// 2) 标题精确匹配(区分大小写)
    /// 3) 标题大小写不敏感精确匹配
    /// 4) 标题子串包含匹配(按步骤顺序返回第一个匹配项)
    pub fn resolve_task_id(&self, query: &str) -> Option<usize> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }

        // 1. 数字索引查找(保持原有数字索引行为)
        if let Ok(idx) = query.parse::<usize>() {
            if idx < self.steps.len() {
                return Some(idx);
            }
        }

        // 2. 标题精确匹配(区分大小写)
        for (i, s) in self.steps.iter().enumerate() {
            if s.name == query {
                return Some(i);
            }
        }

        // 3. 标题大小写不敏感精确匹配
        for (i, s) in self.steps.iter().enumerate() {
            if s.name.eq_ignore_ascii_case(query) {
                return Some(i);
            }
        }

        // 4. 标题包含/子串模糊匹配(按顺序确定性,大小写不敏感)
        let query_lower = query.to_lowercase();
        for (i, s) in self.steps.iter().enumerate() {
            if s.name.to_lowercase().contains(&query_lower) {
                return Some(i);
            }
        }

        None
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

/// 从一个 JSON 值里尽力提取一个非负整数:
/// 数字直接用;字符串按数字解析;数组取第一个可解析的元素。
fn first_number(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
        serde_json::Value::Array(a) => a.iter().find_map(first_number),
        _ => None,
    }
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

    #[test]
    fn tolerate_string_and_array_id() {
        // 模型幻觉:id 为字符串、数组,needs 混入字符串数字与无效项
        let raw = r#"{ "goal": "x", "steps": [
            {"id":"1","name":"a","agent":"researcher","skill":"deep-research","prompt":"p","kind":"llm","needs":[]},
            {"id":[2],"name":"b","agent":"writer","skill":"report-writing","prompt":"p","kind":"llm","needs":["1", "oops"]},
            {"name":"c","agent":"reviewer","skill":"final-review","prompt":"p","kind":"llm","needs":[1,2]}
        ]}"#;
        let p = Plan::from_llm("x", raw).unwrap();
        assert_eq!(p.steps.iter().map(|s| s.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(p.steps[1].needs, vec![1]);
        assert_eq!(p.topo_order().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn tolerate_missing_kind_defaults_to_llm() {
        let raw = r#"{ "goal": "x", "steps": [
            {"id":1,"name":"a","agent":"coder","skill":"code-writing","prompt":"p","needs":[]},
            {"id":2,"name":"b","agent":"reviewer","skill":"final-review","prompt":"p","kind":"LLM","needs":[1]}
        ]}"#;
        let p = Plan::from_llm("x", raw).unwrap();
        assert_eq!(p.steps[0].kind, StepKind::Llm);
        assert_eq!(p.steps[1].kind, StepKind::Llm);
    }

    #[test]
    fn resolve_task_id_by_numeric_index() {
        let p = Plan::from_llm("测试", GOOD).unwrap();
        assert_eq!(p.resolve_task_id("0"), Some(0));
        assert_eq!(p.resolve_task_id("1"), Some(1));
        assert_eq!(p.resolve_task_id("2"), None);
    }

    #[test]
    fn resolve_task_id_by_exact_title() {
        let p = Plan::from_llm("测试", GOOD).unwrap();
        assert_eq!(p.resolve_task_id("调研"), Some(0));
        assert_eq!(p.resolve_task_id("终审"), Some(1));
    }

    #[test]
    fn resolve_task_id_case_insensitive_title() {
        // 使用英文标题测试大小写不敏感
        let raw = r#"{ "goal": "x", "steps": [
            {"id":1,"name":"Add Login","agent":"researcher","skill":"deep-research","prompt":"p","kind":"llm","needs":[]},
            {"id":2,"name":"Setup DB","agent":"reviewer","skill":"final-review","prompt":"p","kind":"llm","needs":[1]}
        ]}"#;
        let p = Plan::from_llm("x", raw).unwrap();
        assert_eq!(p.resolve_task_id("add login"), Some(0));
        assert_eq!(p.resolve_task_id("SETUP DB"), Some(1));
    }

    #[test]
    fn resolve_task_id_by_title_substring() {
        // 使用英文标题测试子串包含匹配
        let raw = r#"{ "goal": "x", "steps": [
            {"id":1,"name":"Add Login","agent":"researcher","skill":"deep-research","prompt":"p","kind":"llm","needs":[]},
            {"id":2,"name":"Setup DB","agent":"reviewer","skill":"final-review","prompt":"p","kind":"llm","needs":[1]}
        ]}"#;
        let p = Plan::from_llm("x", raw).unwrap();
        // "login" 是 "Add Login" 的子串，但不是精确匹配
        assert_eq!(p.resolve_task_id("login"), Some(0));
        // "db" 是 "Setup DB" 的子串
        assert_eq!(p.resolve_task_id("db"), Some(1));
    }

    #[test]
    fn resolve_task_id_missing_title_returns_none() {
        let p = Plan::from_llm("测试", GOOD).unwrap();
        assert_eq!(p.resolve_task_id("不存在"), None);
        assert_eq!(p.resolve_task_id(""), None);
    }
}

#[test]
fn parse_tool_steps() {
    let raw = r#"{ "goal": "改码", "steps": [
        {"id":1,"name":"读文件","agent":"coder","skill":"code-writing","prompt":"读 source","kind":"read","path":"src/main.rs","needs":[]},
        {"id":2,"name":"跑测试","agent":"coder","skill":"code-writing","prompt":"跑命令","kind":"shell","command":"cargo test","needs":[1]},
        {"id":3,"name":"改文件","agent":"coder","skill":"code-writing","prompt":"生成 edits","kind":"edit","path":"src/main.rs","needs":[1,2]}
    ]}"#;
    let p = Plan::from_llm("改码", raw).unwrap();
    assert_eq!(p.steps.len(), 3);
    assert_eq!(p.steps[0].kind, StepKind::Read);
    assert_eq!(p.steps[0].path.as_deref(), Some("src/main.rs"));
    assert_eq!(p.steps[0].command, None);
    assert_eq!(p.steps[1].kind, StepKind::Shell);
    assert_eq!(p.steps[1].command.as_deref(), Some("cargo test"));
    assert_eq!(p.steps[1].path, None);
    assert_eq!(p.steps[2].kind, StepKind::Edit);
    assert_eq!(p.steps[2].path.as_deref(), Some("src/main.rs"));
    assert_eq!(p.steps[2].command, None);
}

#[test]
fn reject_tool_step_missing_required_field() {
    let missing_shell_command = r#"{ "goal": "x", "steps": [{"id":1,"name":"a","agent":"coder","skill":"code-writing","prompt":"p","kind":"shell","needs":[]}]}"#;
    assert!(Plan::from_llm("x", missing_shell_command).is_err());

    let missing_edit_path = r#"{ "goal": "x", "steps": [{"id":1,"name":"a","agent":"coder","skill":"code-writing","prompt":"p","kind":"edit","needs":[]}]}"#;
    assert!(Plan::from_llm("x", missing_edit_path).is_err());

    let missing_read_path = r#"{ "goal": "x", "steps": [{"id":1,"name":"a","agent":"coder","skill":"code-writing","prompt":"p","kind":"read","needs":[]}]}"#;
    assert!(Plan::from_llm("x", missing_read_path).is_err());
}


/// 将标题转换为 slug：小写、空格/标点等非字母数字字符转为连字符，
/// 并压缩连续连字符。
fn slugify_title(input: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;

    for ch in input.chars() {
        if ch.is_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.extend(ch.to_lowercase());
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }

    out
}

/// 将用户输入解析为任务索引。
/// - 若输入为十进制整数，将其作为索引（保持向后兼容），越界时报错。
/// - 否则按标题 slug 进行匹配，返回匹配任务的下标。
pub fn resolve_task_id<I, S>(input: &str, task_titles: I) -> anyhow::Result<usize>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let titles: Vec<String> = task_titles
        .into_iter()
        .map(|t| slugify_title(t.as_ref()))
        .collect();

    if let Ok(index) = input.parse::<usize>() {
        if index < titles.len() {
            return Ok(index);
        }
        anyhow::bail!(
            "task index {index} out of range; only {} task(s) available",
            titles.len()
        );
    }

    let needle = slugify_title(input);
    if needle.is_empty() {
        anyhow::bail!("task selector must be a numeric index or a non-empty title slug");
    }

    titles
        .iter()
        .position(|t| *t == needle)
        .ok_or_else(|| anyhow::anyhow!("no task found with title slug '{needle}'"))
}

#[cfg(test)]
mod task_id_resolve_tests {
    use super::*;

    #[test]
    fn resolves_numeric_index() {
        let titles = ["Add Login", "Setup DB"];
        assert_eq!(resolve_task_id("0", titles.iter().copied()).unwrap(), 0);
        assert_eq!(resolve_task_id("1", titles.iter().copied()).unwrap(), 1);
    }

    #[test]
    fn resolves_title_slug() {
        let titles = ["Add Login", "Setup DB"];
        assert_eq!(resolve_task_id("add-login", titles.iter().copied()).unwrap(), 0);
        assert_eq!(resolve_task_id("setup-db", titles.iter().copied()).unwrap(), 1);
        assert_eq!(resolve_task_id("Setup DB", titles.iter().copied()).unwrap(), 1);
    }

    #[test]
    fn rejects_out_of_range_and_unknown_slug() {
        let titles = ["Add Login"];
        assert!(resolve_task_id("2", titles.iter().copied()).is_err());
        assert!(resolve_task_id("missing", titles.iter().copied()).is_err());
        assert!(resolve_task_id("", titles.iter().copied()).is_err());
    }
}