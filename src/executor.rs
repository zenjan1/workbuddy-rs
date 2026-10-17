use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::future::join_all;
use serde::Serialize;

use crate::agents::{self, Agent};
use crate::config::Config;
use crate::llm::{Llm, Usage};
use crate::plan::{Plan, Step, StepKind};
use crate::util::clip;
use crate::workspace::Workspace;
use crate::status;

const DEPS_CONTEXT_MAX: usize = 800;
const SHELL_TIMEOUT_SECS: u64 = 60;
const MAX_READ_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub id: usize,
    pub ok: bool,
    pub error: Option<String>,
    pub seconds: f64,
    pub token_usage: Option<u64>,
    pub char_usage: Option<u64>,
}

struct StepOutcome {
    id: usize,
    ok: bool,
    error: Option<String>,
    seconds: f64,
    output: Option<String>,
    token_usage: Option<u64>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    char_usage: Option<u64>,
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

    fn notify_webhook_result(
        &self,
        plan: &Plan,
        result: &Result<(Vec<StepResult>, String)>,
        elapsed_ms: u64,
    ) {
        let (status, error) = match result {
            Ok((results, _summary)) => {
                if let Some(first_failed) = results.iter().find(|r| !r.ok) {
                    (
                        "failed",
                        first_failed
                            .error
                            .clone()
                            .or_else(|| Some("任务执行未成功".to_string())),
                    )
                } else {
                    ("completed", None)
                }
            }
            Err(e) => ("failed", Some(format!("执行失败: {:#}", e))),
        };
        crate::webhook::notify_task_finished(
            &self.cfg,
            &plan.goal,
            &plan.goal,
            status,
            elapsed_ms,
            error.as_deref(),
        );
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
        let _ = crate::status::set_running(true);
        let start = std::time::Instant::now();
        let result = self.execute_inner(plan).await;
        let _ = crate::status::set_running(false);
        let elapsed_ms = start.elapsed().as_millis() as u64;
        self.notify_webhook_result(plan, &result, elapsed_ms);
        result
    }

    async fn execute_inner(&self, plan: &Plan) -> Result<(Vec<StepResult>, String)> {
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
                        token_usage: None,
                        char_usage: None,
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
                let step_summary = if o.ok {
                    o.output.as_deref().map(|s| clip(s, 80).replace('\n', " ")).unwrap_or_default()
                } else {
                    o.error.as_deref().map(|s| clip(s, 120).to_string()).unwrap_or_default()
                };
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
                        token_usage: o.token_usage,
                        char_usage: o.char_usage,
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
                        token_usage: o.token_usage,
                        char_usage: o.char_usage,
                    });
                }
                let _ = crate::status::record_step(o.id, o.ok, &step_summary);
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

        let total_tokens: u64 = results.iter().filter_map(|r| r.token_usage).sum();
        let cost = crate::llm::estimate_cost(total_tokens, self.cfg.cost_per_1k_tokens);
        summary.push_str(&format!("\n估算成本: ${:.4} ({} tokens)\n", cost, total_tokens));

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
                    token_usage: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    char_usage: None,
                }
            }
        };
        let start = std::time::Instant::now();
        let (result, usage) = match step.kind {
            StepKind::Llm => {
                match run_llm_step(step, agent, plan, outputs, llm, cfg).await {
                    Ok((text, usage)) => (Ok(text), usage),
                    Err(e) => (Err(e), None),
                }
            }
            StepKind::Shell => (run_shell_step(step).await, None),
            StepKind::Read => (run_read_step(step, cfg), None),
            StepKind::Edit => {
                match run_edit_step(step, agent, plan, outputs, llm, cfg).await {
                    Ok((text, usage)) => (Ok(text), usage),
                    Err(e) => (Err(e), None),
                }
            }
        };
        let seconds = start.elapsed().as_secs_f64();
        match result {
            Ok(text) => {
                let char_usage = Some(text.chars().count() as u64);
                StepOutcome {
                    id: step.id,
                    ok: true,
                    error: None,
                    seconds,
                    output: Some(text),
                    token_usage: usage.as_ref().map(|u| u.total_tokens),
                    prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens),
                    completion_tokens: usage.as_ref().map(|u| u.completion_tokens),
                    char_usage,
                }
            }
            Err(e) => StepOutcome {
                id: step.id,
                ok: false,
                error: Some(e.to_string()),
                seconds,
                output: None,
                token_usage: usage.as_ref().map(|u| u.total_tokens),
                prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens),
                completion_tokens: usage.as_ref().map(|u| u.completion_tokens),
                char_usage: None,
            },
        }
    }
}

fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("webhook_url 仅支持 http://")?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rfind(':') {
        Some(idx) => {
            let host = &authority[..idx];
            let port = authority[idx + 1..]
                .parse::<u16>()
                .map_err(|_| "webhook_url 端口无效")?;
            (host.to_string(), port)
        }
        None => (authority.to_string(), 80),
    };
    Ok((host, port, path.to_string()))
}

fn send_webhook_once(
    cfg: &Config,
    url: &str,
    event: &str,
    goal: &str,
    summary: &str,
) -> Result<(), String> {
    let (host, port, path) = parse_http_url(url)?;
    let timeout = Duration::from_secs(cfg.webhook_timeout_secs.max(1));
    let addrs = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port))
        .map_err(|e| format!("webhook DNS 解析失败: {e}"))?
        .collect::<Vec<_>>();
    let first = addrs.first().ok_or("webhook 地址解析为空")?;
    let mut stream = std::net::TcpStream::connect_timeout(first, timeout)
        .map_err(|e| format!("webhook 连接失败: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("webhook 设置读超时失败: {e}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| format!("webhook 设置写超时失败: {e}"))?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let payload = serde_json::json!({
        "event": event,
        "task": goal,
        "summary": summary,
        "timestamp": timestamp,
    });
    let body = serde_json::to_vec(&payload)
        .map_err(|e| format!("webhook JSON 序列化失败: {e}"))?;
    let path = if path.is_empty() { "/" } else { &path };
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        path, host, port, body.len()
    );
    std::io::Write::write_all(&mut stream, req.as_bytes())
        .map_err(|e| format!("webhook 写请求失败: {e}"))?;
    std::io::Write::write_all(&mut stream, &body)
        .map_err(|e| format!("webhook 写 body 失败: {e}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| format!("webhook 关闭写端失败: {e}"))?;
    let mut resp = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = std::io::Read::read(&mut stream, &mut buf)
            .map_err(|e| format!("webhook 读响应失败: {e}"))?;
        if n == 0 {
            break;
        }
        resp.extend_from_slice(&buf[..n]);
        if resp.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&resp).to_string();
    let status = head.lines().next().unwrap_or("").to_string();
    if status.contains(" 200") || status.contains(" 201") || status.contains(" 204") {
        Ok(())
    } else {
        Err(format!("webhook 返回非成功状态: {}", status))
    }
}

fn send_webhook(
    cfg: &Config,
    url: &str,
    event: &str,
    goal: &str,
    summary: &str,
) -> Result<(), String> {
    let mut last_err = "webhook 未发送".to_string();
    for attempt in 0..=cfg.webhook_retry {
        match send_webhook_once(cfg, url, event, goal, summary) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
                if attempt < cfg.webhook_retry {
                    let backoff_ms = 100u64.saturating_mul(attempt as u64 + 1);
                    std::thread::sleep(Duration::from_millis(backoff_ms));
                }
            }
        }
    }
    Err(last_err)
}

async fn run_llm_step(
    step: &Step,
    agent: &Agent,
    plan: &Plan,
    outputs: &HashMap<usize, String>,
    llm: &Arc<dyn Llm>,
    _cfg: &Config,
) -> Result<(String, Option<Usage>)> {
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

    let resp = llm.chat(&system, &user)
        .await
        .context("LLM 步骤调用失败")?;
    Ok((resp.text, resp.usage))
}

/// 解析模型给出的文件路径:优先原路径;否则按 basename 在仓库里查找唯一匹配
/// (容忍模型漏掉 src/ 等目录前缀)。
fn resolve_repo_path(repo_dir: &std::path::Path, path: &str) -> Result<std::path::PathBuf> {
    let exact = repo_dir.join(path);
    if exact.is_file() {
        return Ok(exact);
    }
    let base = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str());
    let Some(base) = base else {
        anyhow::bail!("文件不存在: {}", path);
    };
    let mut matches = Vec::new();
    let mut stack = vec![repo_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name == "target" || name == ".git" || name == "node_modules" {
                    continue;
                }
                stack.push(p);
            } else if p.file_name().and_then(|s| s.to_str()) == Some(base) {
                matches.push(p);
            }
        }
    }
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => anyhow::bail!("文件不存在: {}(按 {} 也未找到)", path, base),
        n => anyhow::bail!(
            "{} 匹配到 {} 个同名文件,请给完整路径",
            base,
            n
        ),
    }
}

/// 在 workspace/repo_dir 内安全读取文本文件。
/// 拒绝绝对路径、`..` 组件与符号链接逃逸;超过 MAX_READ_BYTES 截断。
fn read_workspace_file(repo_dir: &std::path::Path, path: &str) -> Result<String> {
    let raw = std::path::Path::new(path);
    if raw.is_absolute() {
        anyhow::bail!("路径越界/不允许: 绝对路径: {}", path);
    }
    if raw
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("路径越界/不允许: 包含 ..: {}", path);
    }

    let p = resolve_repo_path(repo_dir, path)?;
    let root = repo_dir
        .canonicalize()
        .context("workspace 根目录 canonicalize 失败")?;
    let cand = p
        .canonicalize()
        .with_context(|| format!("读取文件 canonicalize 失败: {}", p.display()))?;
    if !cand.starts_with(&root) {
        anyhow::bail!("路径越界/不允许: 符号链接逃逸: {}", path);
    }

    let metadata = std::fs::metadata(&cand)
        .with_context(|| format!("读取元数据失败: {}", cand.display()))?;
    let mut file = std::fs::File::open(&cand)
        .with_context(|| format!("读取失败: {}", cand.display()))?;
    let mut buf = Vec::new();
    file.take(MAX_READ_BYTES as u64)
        .read_to_end(&mut buf)
        .with_context(|| format!("读取失败: {}", cand.display()))?;

    let mut content = String::from_utf8_lossy(&buf).to_string();
    if metadata.len() > MAX_READ_BYTES as u64 {
        content.push_str("\n...[truncated]");
    }
    let rel = cand
        .strip_prefix(&root)
        .unwrap_or(&cand)
        .to_string_lossy()
        .to_string();
    Ok(format!("# 文件: {rel}\n\n{}", content))
}

/// read 步骤:读取 workspace 内文件内容(限制 MAX_READ_BYTES),供下游作为上下文。
fn run_read_step(step: &Step, cfg: &Config) -> Result<String> {
    let path = step.path.as_deref().context("read 步骤缺少 path")?;
    read_workspace_file(&cfg.repo_dir, path)
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
) -> Result<(String, Option<Usage>)> {
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
            user.push_str(&format!("\n== 当前文件内容: {p} ==\n{}\n", clip(&content, 20_000)));
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

    let resp = llm.chat(&system, &user).await.context("edit 步骤 LLM 调用失败")?;
    let ops = crate::edits::parse_edits(&resp.text).context("解析 edits JSON 失败")?;
    if ops.is_empty() {
        anyhow::bail!("LLM 未返回任何 edit 操作");
    }
    let applied = crate::edits::apply_batch(&cfg.repo_dir, &ops)?;
    Ok((applied.join("\n"), resp.usage))
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
        Config { pre_commit_review: false,
            price: 0.0,
            max_budget: 0.0,
            api_key: None,
            base_url: "http://mock".into(),
            model: "mock".into(),
            max_parallel: 2,
            workspace_dir: Path::new("/tmp").to_path_buf(),
            repo_dir: Path::new("/tmp").to_path_buf(),
            request_timeout_secs: 5,
            webhook_enabled: false,
            review_enabled: false,
            webhook_url: None,
            webhook_timeout_secs: 1,
            webhook_retry: 0,
            cost_per_1k_tokens: 0.0,
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

    #[test]
    fn webhook_success_posts_json() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_clone = std::sync::Arc::clone(&received);
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut stream, &mut buf).unwrap();
            received_clone.lock().unwrap().push(String::from_utf8_lossy(&buf).to_string());
            std::io::Write::write_all(
                &mut stream,
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        });

        let mut cfg = mock_cfg();
        cfg.webhook_url = Some(format!("http://{}/hook", addr));
        cfg.webhook_retry = 0;
        let url = cfg.webhook_url.clone().unwrap();
        send_webhook(&cfg, &url, "completed", "goal-1", "summary-1").unwrap();

        handle.join().unwrap();
        let reqs = received.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].starts_with("POST /hook HTTP/1.1\r\n"));
        assert!(reqs[0].contains("Content-Type: application/json\r\n"));
        let body = reqs[0].split("\r\n\r\n").nth(1).unwrap();
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["event"], "completed");
        assert_eq!(json["task"], "goal-1");
        assert_eq!(json["summary"], "summary-1");
        assert!(json["timestamp"].is_u64());
    }

    #[test]
    fn webhook_retries_on_500_then_succeeds() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_clone = std::sync::Arc::clone(&received);
        let handle = std::thread::spawn(move || {
            for i in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut stream, &mut buf).unwrap();
                received_clone.lock().unwrap().push(String::from_utf8_lossy(&buf).to_string());
                if i == 0 {
                    std::io::Write::write_all(
                        &mut stream,
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                } else {
                    std::io::Write::write_all(
                        &mut stream,
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                }
            }
        });

        let mut cfg = mock_cfg();
        cfg.webhook_url = Some(format!("http://{}/hook", addr));
        cfg.webhook_retry = 1;
        cfg.webhook_timeout_secs = 2;
        let url = cfg.webhook_url.clone().unwrap();
        send_webhook(&cfg, &url, "failed", "goal-2", "summary-2").unwrap();

        handle.join().unwrap();
        let reqs = received.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        for req in reqs.iter() {
            assert!(req.starts_with("POST /hook HTTP/1.1\r\n"));
        }
    }

    #[test]
    fn webhook_timeout_gives_up() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let _ = std::io::Write::write_all(
                &mut stream,
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });

        let mut cfg = mock_cfg();
        cfg.webhook_url = Some(format!("http://{}/slow", addr));
        cfg.webhook_timeout_secs = 1;
        cfg.webhook_retry = 0;
        let url = cfg.webhook_url.clone().unwrap();
        let err = send_webhook(&cfg, &url, "completed", "goal-3", "summary-3").unwrap_err();

        assert!(err.contains("失败") || err.contains("超时"), "unexpected err: {err}");
        handle.join().unwrap();
    }
}

#[cfg(any())]
mod tests {
    use super::*;
    use crate::agents::Agent;
    use crate::llm::{Llm, LlmResponse, Usage};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct MockLlm {
        text: String,
        usage: Option<Usage>,
    }

    #[async_trait]
    impl Llm for MockLlm {
        async fn chat(&self, _system: &str, _user: &str) -> anyhow::Result<LlmResponse> {
            Ok(LlmResponse {
                text: self.text.clone(),
                usage: self.usage.clone(),
            })
        }
    }

    fn test_plan() -> Plan {
        Plan {
            goal: "test goal".to_string(),
            steps: vec![],
        }
    }

    fn test_step() -> Step {
        Step {
            id: 1,
            name: "test step".to_string(),
            agent: "code-writing".to_string(),
            skill: "test".to_string(),
            kind: StepKind::Llm,
            needs: vec![],
            path: None,
            command: None,
            prompt: "test prompt".to_string(),
        }
    }

    #[tokio::test]
    async fn records_usage_from_llm_response() {
        let mock = Arc::new(MockLlm {
            text: "hello world".to_string(),
            usage: Some(Usage {
                prompt_tokens: 12,
                completion_tokens: 34,
                total_tokens: 46,
            }),
        });
        let cfg = Config::default();
        let plan = test_plan();
        let outputs = HashMap::new();
        let outcome = run_step(&test_step(), &plan, &outputs, &mock, &cfg).await;
        assert_eq!(outcome.token_usage, Some(46));
        assert_eq!(outcome.prompt_tokens, Some(12));
        assert_eq!(outcome.completion_tokens, Some(34));
        assert_eq!(outcome.char_usage, Some(11));
    }

    #[tokio::test]
    async fn records_zero_usage_when_missing() {
        let mock = Arc::new(MockLlm {
            text: "abc".to_string(),
            usage: None,
        });
        let cfg = Config::default();
        let plan = test_plan();
        let outputs = HashMap::new();
        let outcome = run_step(&test_step(), &plan, &outputs, &mock, &cfg).await;
        assert_eq!(outcome.token_usage, None);
        assert_eq!(outcome.prompt_tokens, None);
        assert_eq!(outcome.completion_tokens, None);
        assert_eq!(outcome.char_usage, Some(3));
    }
}

#[cfg(test)]
mod executor_read_tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    fn create_temp_dir(test: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "workbuddy-executor-{}-{}-{}",
            test,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_text(dir: &std::path::Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn executor_read_plain_file_success() {
        let dir = create_temp_dir("plain");
        write_text(&dir, "hello.txt", "hello world");
        let got = read_workspace_file(&dir, "hello.txt").unwrap();
        assert_eq!(got, "# 文件: hello.txt\n\nhello world");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn executor_read_missing_file_errors() {
        let dir = create_temp_dir("missing");
        let err = read_workspace_file(&dir, "nope.txt").unwrap_err();
        assert!(
            err.to_string().contains("文件不存在"),
            "unexpected error: {}",
            err
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn executor_read_path_escape_rejected() {
        let dir = create_temp_dir("escape");
        let abs = if cfg!(windows) {
            r"C:\Windows\win.ini"
        } else {
            "/etc/passwd"
        };
        let err = read_workspace_file(&dir, abs).unwrap_err();
        assert!(
            err.to_string().contains("路径越界/不允许"),
            "absolute path error: {}",
            err
        );

        let err = read_workspace_file(&dir, "../outside.txt").unwrap_err();
        assert!(
            err.to_string().contains("路径越界/不允许"),
            "parent path error: {}",
            err
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = std::env::temp_dir().join(format!(
                "workbuddy-read-outside-{}.txt",
                std::process::id()
            ));
            fs::write(&outside, "outside").unwrap();
            let link = dir.join("linked.txt");
            symlink(&outside, &link).unwrap();
            let err = read_workspace_file(&dir, "linked.txt").unwrap_err();
            assert!(
                err.to_string().contains("路径越界/不允许"),
                "symlink escape error: {}",
                err
            );
            fs::remove_file(&outside).unwrap();
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn executor_read_large_file_truncated() {
        let dir = create_temp_dir("large");
        let body: String = "a".repeat(MAX_READ_BYTES + 100);
        write_text(&dir, "large.txt", &body);
        let got = read_workspace_file(&dir, "large.txt").unwrap();
        let prefix = "# 文件: large.txt\n\n".to_string();
        let content = got.strip_prefix(&prefix).expect("file header missing");
        assert!(
            content.ends_with("\n...[truncated]"),
            "missing truncation marker: {}",
            content.len()
        );
        let expected_prefix_len = MAX_READ_BYTES;
        assert_eq!(
            content.len(),
            expected_prefix_len + "\n...[truncated]".len(),
            "truncated body length mismatch"
        );
        assert!(content.starts_with(&"a".repeat(MAX_READ_BYTES)));
        fs::remove_dir_all(&dir).unwrap();
    }
}