use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::future::join_all;
use serde::Serialize;

use crate::agents::{self, Agent};
use crate::config::Config;
use crate::llm::Llm;
use crate::plan::{Plan, Step, StepKind};
use crate::util::clip;
use crate::workspace::Workspace;

const DEPS_CONTEXT_MAX: usize = 800;
const SHELL_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub id: usize,
    pub ok: bool,
    pub error: Option<String>,
    pub seconds: f64,
}

struct StepOutcome {
    id: usize,
    ok: bool,
    error: Option<String>,
    seconds: f64,
    output: Option<String>,
}

pub struct Executor {
    llm: Arc<dyn Llm>,
    cfg: Config,
    ws: Option<Arc<Workspace>>,
}

impl Executor {
    pub fn new(llm: Arc<dyn Llm>, cfg: Config) -> Self {
        Self {
            llm,
            cfg,
            ws: None,
        }
    }

    /// 绑定工作区:执行过程中把每个成功步骤的产出落盘。
    pub fn with_workspace(mut self, ws: Arc<Workspace>) -> Self {
        self.ws = Some(ws);
        self
    }

    fn save_output(&self, id: usize, text: &str) {
        if let Some(ws) = &self.ws {
            if let Err(e) = ws.save_step_output(id, text) {
                eprintln!("[执行] 警告: 步骤 {} 产出落盘失败: {}", id, e);
            }
        }
    }

    /// 拓扑并行执行计划:无依赖的步骤并发(受 max_parallel 限制),
    /// 失败时其全部下游标记为 skipped。返回 (按 id 排序的结果, 执行摘要)。
    pub async fn execute(&self, plan: &Plan) -> Result<(Vec<StepResult>, String)> {
        plan.topo_order()?;
        let mut pending: Vec<&Step> = plan.steps.iter().collect();
        let mut outputs: HashMap<usize, String> = HashMap::new();
        let mut results: Vec<StepResult> = Vec::new();
        let mut failed: HashSet<usize> = HashSet::new();
        let mut done: HashSet<usize> = HashSet::new();

        loop {
            if pending.is_empty() {
                break;
            }
            // 就绪判定:依赖全部完成
            let ready: Vec<&Step> = pending
                .iter()
                .copied()
                .filter(|s| s.needs.iter().all(|n| done.contains(n)))
                .collect();
            if ready.is_empty() {
                anyhow::bail!(
                    "执行死锁: 仍有 {} 个未完成步骤但无可执行步骤",
                    pending.len()
                );
            }
            // 就绪步骤分流:依赖失败 -> 跳过;否则 -> 本批执行
            let mut run: Vec<&Step> = Vec::new();
            for s in &ready {
                if let Some(n) = s.needs.iter().find(|n| failed.contains(n)) {
                    eprintln!(
                        "[执行] 步骤 {} [{}] 跳过(依赖 {} 失败)",
                        s.id,
                        s.name,
                        plan.step_name(*n)
                    );
                    results.push(StepResult {
                        id: s.id,
                        ok: false,
                        error: Some(format!("跳过: 依赖步骤 {} 失败", n)),
                        seconds: 0.0,
                    });
                    failed.insert(s.id);
                    done.insert(s.id);
                } else {
                    run.push(s);
                }
            }
            pending.retain(|s| !ready.iter().any(|r| r.id == s.id));
            if run.is_empty() {
                continue;
            }
            // 本批并发执行,每 chunk 内并发度 = max_parallel
            let mut batch: Vec<StepOutcome> = Vec::new();
            for chunk in run.chunks(self.cfg.max_parallel) {
                let futs = chunk.iter().map(|s| {
                    let llm = Arc::clone(&self.llm);
                    let cfg = self.cfg.clone();
                    let snap = outputs.clone();
                    async move { Self::run_step(*s, plan, &snap, &llm, &cfg).await }
                });
                batch.extend(join_all(futs).await);
            }
            for o in batch {
                let mark = if o.ok { "完成" } else { "失败" };
                eprintln!(
                    "[执行] 步骤 {} [{}] {} ({:.1}s)",
                    o.id,
                    plan.step_name(o.id),
                    mark,
                    o.seconds
                );
                if o.ok {
                    if let Some(text) = &o.output {
                        eprintln!(
                            "[执行]   摘要: {}",
                            clip(text, 80).replace('\n', " ")
                        );
                        self.save_output(o.id, text);
                    }
                    if let Some(text) = o.output {
                        outputs.insert(o.id, text);
                    }
                    results.push(StepResult {
                        id: o.id,
                        ok: true,
                        error: None,
                        seconds: o.seconds,
                    });
                } else {
                    if let Some(e) = &o.error {
                        eprintln!("[执行]   错误: {}", clip(e, 120));
                    }
                    failed.insert(o.id);
                    results.push(StepResult {
                        id: o.id,
                        ok: false,
                        error: o.error,
                        seconds: o.seconds,
                    });
                }
                done.insert(o.id);
            }
        }

        let mut summary = String::from("## 执行摘要\n\n");
        for r in &results {
            let mark = if r.ok { "✅" } else { "❌" };
            let err = r
                .error
                .as_deref()
                .map(|e| format!("  ({})", clip(e, 60)))
                .unwrap_or_default();
            summary.push_str(&format!(
                "- {} 步骤 {} [{}]: {:.1}s{}\n",
                mark,
                r.id,
                plan.step_name(r.id),
                r.seconds,
                err
            ));
        }
        let failed_count = results.iter().filter(|r| !r.ok).count();
        summary.push_str(&format!(
            "\n完成 {}/{} 个步骤,{} 个未成功。\n",
            results.len() - failed_count,
            results.len(),
            failed_count
        ));

        results.sort_by_key(|r| r.id);
        Ok((results, summary))
    }

    /// 执行单个步骤:LLM 步骤组装 专家角色+skill+依赖上下文 后调用 LLM,
    /// shell 步骤在 workspace 下运行命令(60s 超时)。
    async fn run_step(
        step: &Step,
        plan: &Plan,
        outputs: &HashMap<usize, String>,
        llm: &Arc<dyn Llm>,
        cfg: &Config,
    ) -> StepOutcome {
        let agent = match agents::by_id(&step.agent) {
            Some(a) => a,
            None => {
                return StepOutcome {
                    id: step.id,
                    ok: false,
                    error: Some(format!("未知专家: {}", step.agent)),
                    seconds: 0.0,
                    output: None,
                }
            }
        };
        let start = std::time::Instant::now();
        let result = match step.kind {
            StepKind::Llm => run_llm_step(step, agent, plan, outputs, llm, cfg).await,
            StepKind::Shell => run_shell_step(step).await,
            StepKind::Read => run_read_step(step, cfg),
            StepKind::Edit => run_edit_step(step, agent, plan, outputs, llm, cfg).await,
        };
        let seconds = start.elapsed().as_secs_f64();
        match result {
            Ok(text) => StepOutcome {
                id: step.id,
                ok: true,
                error: None,
                seconds,
                output: Some(text),
            },
            Err(e) => StepOutcome {
                id: step.id,
                ok: false,
                error: Some(e.to_string()),
                seconds,
                output: None,
            },
        }
    }
}

async fn run_llm_step(
    step: &Step,
    agent: &Agent,
    plan: &Plan,
    outputs: &HashMap<usize, String>,
    llm: &Arc<dyn Llm>,
    _cfg: &Config,
) -> Result<String> {
    let skill = agent
        .skills
        .iter()
        .find(|s| s.name == step.skill)
        .copied()
        .unwrap_or_else(|| agent.skills[0]);

    let mut system = String::new();
    system.push_str(agent.system);
    system.push_str("\n\n当前启用的 Skill:");
    system.push_str(&skill.name);
    system.push_str(" — ");
    system.push_str(skill.desc);
    system.push_str("\n");
    system.push_str(skill.prompt);

    let mut user = format!("总目标: {}\n", plan.goal);
    user.push_str(&format!(
        "你的任务(步骤 {}): {}\n\n{}",
        step.id, step.name, step.prompt
    ));
    if !step.needs.is_empty() {
        user.push_str("\n=== 上游步骤产出(依赖上下文) ===\n");
        for n in &step.needs {
            user.push_str(&format!(
                "\n## 来自步骤 {} [{}]\n",
                n,
                plan.step_name(*n)
            ));
            match outputs.get(n) {
                Some(t) => user.push_str(&clip(t, DEPS_CONTEXT_MAX).to_string()),
                None => user.push_str("(无产出)"),
            }
        }
    }

    llm.chat(&system, &user)
        .await
        .context("LLM 步骤调用失败")
}

/// read 步骤:读取仓库内文件内容(限制 20KB),供下游作为上下文。
fn run_read_step(step: &Step, cfg: &Config) -> Result<String> {
    let path = step.path.as_deref().context("read 步骤缺少 path")?;
    let p = cfg.repo_dir.join(path);
    if !p.is_file() {
        anyhow::bail!("文件不存在: {}", p.display());
    }
    let content = std::fs::read_to_string(&p).with_context(|| format!("读取失败: {}", p.display()))?;
    Ok(format!("# 文件: {path}\n\n{}", clip(&content, 20_000)))
}

/// edit 步骤:LLM 产出 edits JSON(文件操作协议),解析并应用到仓库,
/// 输出应用了哪些动作供下游/审计。
async fn run_edit_step(
    step: &Step,
    agent: &Agent,
    plan: &Plan,
    outputs: &HashMap<usize, String>,
    llm: &Arc<dyn Llm>,
    cfg: &Config,
) -> Result<String> {
    let skill = agent
        .skills
        .iter()
        .find(|s| s.name == step.skill)
        .copied()
        .unwrap_or_else(|| agent.skills[0]);

    let mut system = String::new();
    system.push_str(agent.system);
    system.push_str("\n\n当前启用的 Skill:");
    system.push_str(&skill.name);
    system.push_str(" — ");
    system.push_str(skill.desc);
    system.push_str("\n");
    system.push_str(skill.prompt);
    system.push_str(
        "\n\n== 输出协议(必须遵守) ==\n\
        只输出一个 JSON 对象: {\"edits\": [ ... ]},不要 markdown 围栏、不要额外文字。\n\
        每个 edit: {\"path\": 仓库相对路径, \"old_string\": 要替换的精确原文(为空表示在文件末尾追加;文件不存在则新建), \"new_string\": 替换后内容, \"replace_all\": 布尔}。\n\
        old_string 必须与文件内容逐字符一致(含缩进);不唯一时报错,请给更长上下文或 replace_all=true。\n\
        每次最多输出 5 个 edits,聚焦最小改动。",
    );

    let mut user = format!("总目标: {}\n", plan.goal);
    user.push_str(&format!(
        "你的任务(步骤 {}): {}\n\n{}",
        step.id, step.name, step.prompt
    ));
    if let Some(p) = &step.path {
        if let Ok(content) = std::fs::read_to_string(cfg.repo_dir.join(p)) {
            user.push_str(&format!("\n== 当前文件内容: {p} ==\n{}\n", clip(&content, 12_000)));
        }
    }
    if !step.needs.is_empty() {
        user.push_str("\n=== 上游步骤产出(依赖上下文) ===\n");
        for n in &step.needs {
            user.push_str(&format!(
                "\n## 来自步骤 {} [{}]\n",
                n,
                plan.step_name(*n)
            ));
            match outputs.get(n) {
                Some(t) => user.push_str(&clip(t, DEPS_CONTEXT_MAX).to_string()),
                None => user.push_str("(无产出)"),
            }
        }
    }

    let raw = llm.chat(&system, &user).await.context("edit 步骤 LLM 调用失败")?;
    let ops = crate::edits::parse_edits(&raw).context("解析 edits JSON 失败")?;
    if ops.is_empty() {
        anyhow::bail!("LLM 未返回任何 edit 操作");
    }
    let mut applied = Vec::new();
    for op in &ops {
        let a = crate::edits::apply_op(&cfg.repo_dir, op)?;
        applied.push(a);
    }
    Ok(applied.join("\n"))
}

async fn run_shell_step(step: &Step) -> Result<String> {
    let cmd = step
        .command
        .clone()
        .context("shell 步骤缺少 command")?;
    // 自主开发安全护栏:拒绝危险命令
    crate::edits::check_shell(&cmd)?;
    let out = tokio::time::timeout(
        Duration::from_secs(SHELL_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .output()
        }),
    )
    .await
    .context("shell 步骤执行超时或任务异常")?
    .context("shell 步骤任务异常")?
    .context("运行 shell 命令失败")?;
    if !out.status.success() {
        anyhow::bail!(
            "命令退出码 {:?}: {}",
            out.status.code(),
            clip(&String::from_utf8_lossy(&out.stderr), 200)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockLlm;
    use crate::plan::Plan;
    use std::path::Path;

    const PLAN: &str = r#"{"goal":"mock 端到端","steps":[
        {"id":1,"name":"调研","agent":"researcher","skill":"deep-research","prompt":"调研","kind":"llm","needs":[]},
        {"id":2,"name":"撰写","agent":"writer","skill":"report-writing","prompt":"撰写","kind":"llm","needs":[1]},
        {"id":3,"name":"终审","agent":"reviewer","skill":"final-review","prompt":"终审","kind":"llm","needs":[2]}
    ]}"#;

    fn mock_cfg() -> Config {
        Config {
            api_key: None,
            base_url: "http://mock".into(),
            model: "mock".into(),
            max_parallel: 2,
            workspace_dir: Path::new("/tmp").to_path_buf(),
            repo_dir: Path::new("/tmp").to_path_buf(),
            request_timeout_secs: 5,
            mock: true,
        }
    }

    #[tokio::test]
    async fn mock_end_to_end() {
        let exec = Executor::new(Arc::new(MockLlm), mock_cfg());
        let plan = Plan::from_llm("mock 端到端", PLAN).unwrap();
        let (results, summary) = exec.execute(&plan).await.unwrap();
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.ok), "mock 步骤应全部成功");
        assert!(summary.contains("完成 3/3"));
    }

    #[tokio::test]
    async fn mock_shell_step_runs() {
        let plan = Plan::from_llm(
            "shell",
            r#"{"goal":"shell","steps":[
                {"id":1,"name":"echo","agent":"coder","skill":"code-writing","prompt":"跑命令","kind":"shell","command":"echo hello-wb","needs":[]}
            ]}"#,
        )
        .unwrap();
        let exec = Executor::new(Arc::new(MockLlm), mock_cfg());
        let (results, summary) = exec.execute(&plan).await.unwrap();
        assert!(results[0].ok);
        assert!(summary.contains("完成 1/1"));
    }
}
