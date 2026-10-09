//! dev 模式:自主开发循环。
//!
//! 从 backlog(任务清单 JSON)取任务 → LLM 规划 → 执行(可 edit/shell/read)→
//! 验证(verify 命令)→ 失败自动修复(最多 N 次)→ git 提交 → 下一个任务。
//! 支持预算(任务数/时长)与中断恢复(state.json 记录进度)。

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::edits;
use crate::executor::Executor;
use crate::llm::Llm;
use crate::planner;
use crate::util::clip;

const MAX_REPAIRS: usize = 3;

// ---------- backlog ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevTask {
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// 验证命令(缺省用全局 verify)
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub priority: i32,
}

pub fn load_backlog(path: &Path) -> Result<Vec<DevTask>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("读取 backlog 失败: {}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).context("backlog 不是合法 JSON")?;
    let arr = v
        .get("tasks")
        .and_then(|t| t.as_array())
        .cloned()
        .or_else(|| v.as_array().cloned())
        .context("backlog 需要 {\"tasks\":[...]} 或数组")?;
    let mut out = Vec::new();
    for (i, t) in arr.iter().enumerate() {
        let obj = t.as_object().with_context(|| format!("backlog[{}] 不是对象", i))?;
        let title = obj
            .get("title")
            .and_then(|x| x.as_str())
            .context("backlog 任务缺少 title")?
            .to_string();
        let description = obj.get("description").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let verify = obj.get("verify").and_then(|x| x.as_str()).map(|s| s.to_string());
        let priority = obj.get("priority").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
        out.push(DevTask {
            title,
            description,
            verify,
            priority,
        });
    }
    if out.is_empty() {
        bail!("backlog 为空");
    }
    Ok(out)
}

// ---------- state ----------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DevState {
    pub done: Vec<String>,
    pub failed: Vec<String>,
    pub llm_calls: u64,
    pub started_at: Option<String>,
    pub history: Vec<String>,
}

fn state_path(repo: &Path) -> std::path::PathBuf {
    repo.join(".workbuddy/state.json")
}

fn load_state(repo: &Path) -> DevState {
    std::fs::read_to_string(state_path(repo))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_state(repo: &Path, s: &DevState) -> Result<()> {
    let path = state_path(repo);
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    std::fs::write(path, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

// ---------- git helpers ----------

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("git 命令执行失败")?;
    if !out.status.success() {
        bail!(
            "git {} 失败: {}",
            args.join(" "),
            clip(&String::from_utf8_lossy(&out.stderr), 300)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_has_changes(repo: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// 提交当前改动;无改动返回 Ok(None)。
fn git_commit(repo: &Path, message: &str) -> Result<Option<String>> {
    if !git_has_changes(repo) {
        return Ok(None);
    }
    // .workbuddy/ 状态文件不进 git(避免每次提交都带状态噪声)
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["add", "-A", "--", ".", ":(exclude).workbuddy/state.json"])
        .status();
    let out = git(repo, &["commit", "-m", message])?;
    let sha = git(repo, &["rev-parse", "--short", "HEAD"])?;
    let _ = out;
    Ok(Some(sha))
}

// ---------- verify ----------

fn run_verify(repo: &Path, cmd: &str) -> Result<bool> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(repo)
        .output()
        .with_context(|| format!("运行验证命令失败: {cmd}"))?;
    let ok = out.status.success();
    if !ok {
        eprintln!(
            "[verify] ❌ {} (退出码 {:?})\n{}",
            cmd,
            out.status.code(),
            clip(
                &String::from_utf8_lossy(&out.stderr),
                2000
            )
        );
    } else {
        eprintln!("[verify] ✅ {}", cmd);
    }
    Ok(ok)
}

// ---------- 修复 ----------

/// 验证失败时:把失败输出喂给 LLM,让它直接产出 edits JSON 修复,应用后重试。
async fn repair(
    llm: &Arc<dyn Llm>,
    cfg: &Config,
    task: &DevTask,
    verify_cmd: &str,
    failure_output: &str,
) -> Result<Option<String>> {
    let system = "你是 WorkBuddy 的修复工程师。验证命令失败了,请找出问题并直接修复代码。\n\
    只输出一个 JSON 对象: {\"edits\": [ ... ]},每个 edit: {\"path\": 仓库相对路径, \"old_string\": 精确原文, \"new_string\": 修复后内容, \"replace_all\": 布尔}。\n\
    old_string 为空表示在文件末尾追加(文件不存在则新建)。不要 markdown 围栏,不要额外文字,最多 5 个 edits。";
    let user = format!(
        "任务: {}\n验证命令: {}\n失败输出(尾部):\n{}\n\n请修复。",
        task.title,
        verify_cmd,
        clip(failure_output, 4000)
    );
    let raw = llm.chat(system, &user).await.context("修复 LLM 调用失败")?;
    let ops = edits::parse_edits(&raw).context("解析修复 edits 失败")?;
    if ops.is_empty() {
        return Ok(None);
    }
    let mut applied = Vec::new();
    for op in &ops {
        applied.push(edits::apply_op(&cfg.repo_dir, op)?);
    }
    Ok(Some(applied.join("; ")))
}

// ---------- 主循环 ----------

pub struct DevOptions {
    pub backlog_path: std::path::PathBuf,
    pub verify: Option<String>,
    pub max_tasks: Option<usize>,
    pub max_minutes: Option<u64>,
}

pub struct DevReport {
    pub completed: Vec<String>,
    pub failed: Vec<String>,
}

pub async fn run_dev_loop(
    llm: Arc<dyn Llm>,
    cfg: Config,
    opts: DevOptions,
) -> Result<DevReport> {
    if !cfg.repo_dir.join(".git").exists() {
        bail!("{} 不是 git 仓库(dev 模式需要 git 用于提交)", cfg.repo_dir.display());
    }
    let backlog = load_backlog(&opts.backlog_path)?;
    let mut state = load_state(&cfg.repo_dir);
    if state.started_at.is_none() {
        state.started_at = Some(now_iso());
    }

    eprintln!(
        "[dev] 仓库: {} | backlog {} 项 | 已完成 {} | 预算: 任务 {} 分钟数 {}",
        cfg.repo_dir.display(),
        backlog.len(),
        state.done.len(),
        opts.max_tasks.map(|n| n.to_string()).unwrap_or_else(|| "∞".into()),
        opts.max_minutes.map(|n| n.to_string()).unwrap_or_else(|| "∞".into()),
    );

    let start = Instant::now();
    let mut completed = Vec::new();
    let mut failed = Vec::new();
    let mut done_set: HashSet<String> = state.done.iter().cloned().collect();

    let mut idx = 0usize;
    loop {
        // 预算:任务数
        if let Some(m) = opts.max_tasks {
            if completed.len() >= m {
                break;
            }
        }
        // 预算:时长
        if let Some(mins) = opts.max_minutes {
            if start.elapsed() >= Duration::from_secs(mins * 60) {
                eprintln!("[dev] 达到时长预算 {} 分钟,优雅停止", mins);
                break;
            }
        }
        // 取下一个未完成任务(按 priority 降序,原顺序稳定)
        let next = backlog
            .iter()
            .enumerate()
            .filter(|(i, _)| !done_set.contains(&format!("backlog[{}]", i)))
            .max_by_key(|(i, t)| (t.priority, -(*i as i32)))
            .map(|(i, t)| (i, t));
        let Some((i, task)) = next else {
            eprintln!("[dev] backlog 全部处理完毕");
            break;
        };
        let task_id = format!("backlog[{}]", i);
        let goal = format!("{}\n{}", task.title, task.description).trim().to_string();
        eprintln!("[dev] === 任务 {}/{}: {} ===", state.done.len() + 1, backlog.len(), task.title);

        let verify_cmd = task
            .verify
            .clone()
            .or_else(|| opts.verify.clone())
            .unwrap_or_else(|| "echo no-verify".to_string());

        // 规划(最多 2 次,防规划器抖动)
        let mut plan = None;
        for attempt in 1..=2 {
            match planner::plan_dev_task(&llm, &goal).await {
                Ok(p) => {
                    plan = Some(p);
                    break;
                }
                Err(e) => eprintln!("[dev] 规划失败(第 {} 次): {}", attempt, clip(&e.to_string(), 200)),
            }
        }
        let Some(plan) = plan else {
            failed.push(task.title.clone());
            state.failed.push(task.title.clone());
            state.history.push(format!("{} 规划失败,跳过", now_iso()));
            done_set.insert(task_id.clone());
            save_state(&cfg.repo_dir, &state)?;
            continue;
        };

        // 执行
        let ws = crate::workspace::Workspace::create_new(&cfg.workspace_dir)?;
        let ws = Arc::new(ws);
        let mut run = crate::workspace::RunRecord {
            id: ws.dir.file_name().unwrap().to_string_lossy().to_string(),
            goal: goal.clone(),
            created: now_iso(),
            model: cfg.model.clone(),
            mock: cfg.mock,
            steps: Vec::new(),
        };
        ws.save_plan(&run, &plan)?;
        let exec = Executor::new(Arc::clone(&llm), cfg.clone()).with_workspace(ws.clone());
        let (results, summary) = exec
            .execute(&plan)
            .await
            .context("执行计划异常")?;
        for r in &results {
            let step = plan.step(r.id).unwrap();
            run.steps.push(crate::workspace::StepRecord {
                id: r.id,
                name: step.name.clone(),
                agent: step.agent.clone(),
                skill: step.skill.clone(),
                kind: step.kind.label().to_string(),
                ok: r.ok,
                error: r.error.clone(),
                seconds: r.seconds,
                output_preview: String::new(),
            });
        }
        ws.finish_run(&mut run, &summary)?;

        // 验证 + 自修复
        let mut verify_ok = run_verify(&cfg.repo_dir, &verify_cmd)?;
        let mut repair_count = 0;
        while !verify_ok && repair_count < MAX_REPAIRS {
            repair_count += 1;
            eprintln!("[dev] 验证失败,第 {} 次自动修复...", repair_count);
            let failure = git(&cfg.repo_dir, &["diff", "--stat"]).unwrap_or_default();
            match repair(&llm, &cfg, task, &verify_cmd, &failure).await {
                Ok(Some(desc)) => {
                    eprintln!("[dev] 修复已应用: {}", clip(&desc, 200));
                    verify_ok = run_verify(&cfg.repo_dir, &verify_cmd)?;
                }
                Ok(None) => eprintln!("[dev] 修复 LLM 未返回编辑"),
                Err(e) => eprintln!("[dev] 修复失败: {}", clip(&e.to_string(), 200)),
            }
        }

        if verify_ok {
            // 提交
            let sha = match git_commit(&cfg.repo_dir, &format!("feat: {}", task.title)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[dev] 提交失败: {}", clip(&e.to_string(), 200));
                    None
                }
            };
            let sha_str = sha.as_deref().unwrap_or("(无改动/提交失败)");
            eprintln!("[dev] ✅ 任务完成并已提交: {} ({})", task.title, sha_str);
            completed.push(task.title.clone());
            state.done.push(task_id.clone());
            state.history.push(format!(
                "{} ✅ {} @ {}",
                now_iso(),
                task.title,
                sha_str
            ));
            done_set.insert(task_id);
        } else {
            eprintln!("[dev] ❌ 任务失败(验证未通过,已尝试 {} 次修复)", repair_count);
            // 回滚脏改动,避免污染下一个任务
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&cfg.repo_dir)
                .args(["checkout", "--", "."])
                .status();
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&cfg.repo_dir)
                .args(["clean", "-fd", "--", ":!.workbuddy"])
                .status();
            failed.push(task.title.clone());
            state.failed.push(task_id.clone());
            state.history.push(format!("{} ❌ {}", now_iso(), task.title));
            done_set.insert(task_id);
        }
        save_state(&cfg.repo_dir, &state)?;
        idx += 1;
        let _ = idx;
    }

    eprintln!(
        "[dev] 循环结束: 本次完成 {} 项,失败 {} 项",
        completed.len(),
        failed.len()
    );
    Ok(DevReport { completed, failed })
}

fn now_iso() -> String {
    crate::main_now_iso()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_backlog_accepts_object_and_array() {
        let dir = std::env::temp_dir().join(format!("wb-dev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let p = dir.join("b1.json");
        std::fs::write(&p, r#"{"tasks":[{"title":"t1","description":"d","priority":2}]}"#).unwrap();
        let tasks = load_backlog(&p).unwrap();
        assert_eq!(tasks[0].title, "t1");
        assert_eq!(tasks[0].priority, 2);

        let p = dir.join("b2.json");
        std::fs::write(&p, r#"[{"title":"t2"},{"title":"t3","verify":"cargo test"}]"#).unwrap();
        let tasks = load_backlog(&p).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].verify.as_deref(), Some("cargo test"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wb-dev2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut s = DevState::default();
        s.done.push("backlog[0]".into());
        save_state(&dir, &s).unwrap();
        let s2 = load_state(&dir);
        assert_eq!(s2.done, vec!["backlog[0]"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mock_dev_loop_end_to_end() {
        // 临时 git 仓库 + 2 个 mock 任务,离线跑完整 dev 循环
        let dir = std::env::temp_dir().join(format!("wb-devloop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join(".workbuddy")).unwrap();
        let g = |args: &[&str]| -> std::process::Output {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap()
        };
        g(&["init", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(dir.join("README.md"), "init\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-m", "init"]);

        std::fs::write(
            dir.join(".workbuddy/backlog.json"),
            r#"{"tasks":[{"title":"任务一","description":"mock"},{"title":"任务二","description":"mock"}]}"#,
        )
        .unwrap();

        let cfg = Config {
            api_key: None,
            base_url: "http://mock".into(),
            model: "mock".into(),
            max_parallel: 1,
            workspace_dir: dir.join("ws"),
            repo_dir: dir.clone(),
            request_timeout_secs: 5,
            mock: true,
        };
        let llm: std::sync::Arc<dyn Llm> = std::sync::Arc::new(crate::llm::MockLlm);
        let report = run_dev_loop(
            llm,
            cfg,
            DevOptions {
                backlog_path: dir.join(".workbuddy/backlog.json"),
                verify: Some("echo verify-ok".into()),
                max_tasks: Some(2),
                max_minutes: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(report.completed.len(), 2, "mock dev 循环应完成 2 项");
        assert!(report.failed.is_empty());
        // state 已记录
        let st = load_state(&dir);
        assert_eq!(st.done.len(), 2);
        // 仓库里应产生提交(mock edits 追加到 autodev_note)
        let log = g(&["log", "--oneline"]);
        assert!(String::from_utf8_lossy(&log.stdout).contains("任务一"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
