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

const MAX_REPAIRS: usize = 8;
/// --extend 单次运行最多允许的补给轮数(防失控;每轮 1 次 LLM 调用)
const MAX_EXTEND_ROUNDS: usize = 1000;

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
    /// 为 true 时跳过该任务并写入 state.history
    #[serde(default)]
    pub skip: bool,
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
        let skip = obj.get("skip").and_then(|x| x.as_bool()).unwrap_or(false);
        out.push(DevTask {
            title,
            description,
            verify,
            priority,
            skip,
        });
    }
    if out.is_empty() {
        bail!("backlog 为空");
    }
    Ok(out)
}

/// 持久化 backlog(原子写:先写临时文件再 rename)。
fn save_backlog(path: &Path, tasks: &[DevTask]) -> Result<()> {
    let v = serde_json::to_string_pretty(&serde_json::json!({ "tasks": tasks }))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, v)?;
    std::fs::rename(&tmp, path).with_context(|| format!("写回 backlog 失败: {}", path.display()))?;
    Ok(())
}

/// 调用 LLM 生成 N 个新的开发任务(用于 backlog 耗尽时自补给,支撑长期运行)。
async fn generate_tasks(llm: &Arc<dyn Llm>, repo: &Path, n: usize) -> Result<Vec<DevTask>> {
    // 对齐"完整 workbuddy"目标:把仓库根 VISION.md 路线图注入,让 backlog 耗尽后的自补给持续朝目标推进
    let vision = std::fs::read_to_string(repo.join("VISION.md")).unwrap_or_default();
    let base = "你是 WorkBuddy 的任务生成器。为 workbuddy-rs 这个 Rust 多智能体 CLI 项目\
    (模块: main/dev/edits/executor/llm/plan/planner/agents/workspace/config/util)生成持续的开发任务,\
    要求:每个任务小而独立、可验证(用 cargo test 或编译)、不破坏现有功能、优先补测试/文档/CLI 子命令/性能/健壮性,\
    禁止安全敏感操作。\n";
    let vision_note = format!(
        "仓库路线图 VISION.md(请优先推进其中【未完成】且高价值的方向,避免与已完成/已列出的重复):\n{}\n",
        clip(&vision, 4000)
    );
    let system = format!(
        "{}{}{}",
        base,
        vision_note,
        "只输出一个 JSON 对象: {\"tasks\": [{\"title\": \"任务名\", \"description\": \"完整说明\", \"priority\": 1}]},恰好 N 个任务,不要 markdown 围栏、不要额外文字。"
    );
    let user = format!("请生成 {n} 个新任务(避免与 backlog 中已有的重复主题: 文档、clip 测试、--version、summary、CRLF、占位符、workspace 提示、失败报告、dry-run、--since、重试、StepRecord 统计、retry-failed、stats、--json、LLM 预算、skip 字段、提交序号、SHELL_TIMEOUT、report 命令、health、git 状态摘要、undo、hint 字段、agents.toml、worktree 并行、watch、bench、CLI 集成测试、selfcheck)。");
    // 最多 2 次调用:解析失败时重试(容忍模型偶发输出围栏/前后多余文字/截断)
    let mut last_err = None;
    for attempt in 1..=2 {
        let raw = llm.chat(&system, &user).await.context("任务生成 LLM 调用失败")?;
        match extract_tasks_json(&raw) {
            Ok(arr) => return parse_generated_tasks(arr, n),
            Err(e) => {
                eprintln!("[dev] 任务生成解析失败(第 {attempt} 次): {}", clip(&e.to_string(), 120));
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("任务生成失败")))
}

/// 从 LLM 原始输出提取 tasks 数组(容忍 markdown 围栏、前后文字、裸数组)。
fn extract_tasks_json(raw: &str) -> Result<Vec<serde_json::Value>> {
    let start = raw.find('{').or_else(|| raw.find('[')).context("未找到 JSON")?;
    let end = raw
        .rfind('}')
        .or_else(|| raw.rfind(']'))
        .filter(|e| *e > start)
        .ok_or_else(|| anyhow::anyhow!("JSON 不完整"))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw[start..=end]).context("解析生成任务 JSON 失败")?;
    let arr = v
        .get("tasks")
        .and_then(|t| t.as_array())
        .cloned()
        .or_else(|| v.as_array().cloned())
        .context("生成结果缺少 tasks 数组")?;
    Ok(arr)
}

fn parse_generated_tasks(arr: Vec<serde_json::Value>, n: usize) -> Result<Vec<DevTask>> {
    let mut out = Vec::new();
    for (i, t) in arr.into_iter().take(n).enumerate() {
        let obj = match t.as_object() {
            Some(o) => o,
            None => continue,
        };
        let title = match obj.get("title").and_then(|x| x.as_str()) {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => continue,
        };
        let description = obj
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        let priority = obj.get("priority").and_then(|x| x.as_i64()).unwrap_or(1) as i32;
        let _ = i;
        out.push(DevTask {
            title,
            description,
            verify: None,
            priority,
            skip: false,
        });
    }
    if out.is_empty() {
        bail!("生成任务解析后为空");
    }
    Ok(out)
}

// ---------- state ----------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DevState {
    // 各字段带 #[serde(default)]:手写/外部/旧版本 state.json 漏字段时取默认值,不致解析失败而丢进度
    #[serde(default)]
    pub done: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
    #[serde(default)]
    pub llm_calls: u64,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub history: Vec<String>,
}

fn state_path(repo: &Path) -> std::path::PathBuf {
    repo.join(".workbuddy/state.json")
}

fn load_state(repo: &Path) -> DevState {
    let path = state_path(repo);
    if !path.exists() {
        return DevState::default(); // 全新开始:无 state 文件,直接空(不重试)
    }
    // FUSE(NTFS)上偶发瞬读失败/读滞后:文件确在却读不到时重试,避免把真实进度误判为空
    // (曾于 2026-10-14 复现:state.json 确有 done=10,循环却读到空,导致重做已完成任务、烧 token)
    for attempt in 0..8 {
        match std::fs::read_to_string(&path) {
            Ok(t) => match serde_json::from_str::<DevState>(&t) {
                Ok(s) => return s,
                Err(e) => eprintln!("[dev] state 解析失败(第 {} 次): {}", attempt + 1, e),
            },
            Err(e) => eprintln!("[dev] state 读取失败(第 {} 次): {}", attempt + 1, e),
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    eprintln!("[dev] 警告: state 文件存在但多次读取失败,回退空状态(将重做已完成任务)");
    DevState::default()
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
    // 只看 .workbuddy/ 之外的改动:笔记/状态文件不算"源码改动"
    // (否则 LLM 只写 .workbuddy/autodev_note.md 也能骗过假成功守卫)
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain", "--", ".", ":(exclude).workbuddy"])
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// 提交当前改动;无改动返回 Ok(None)。
fn git_commit(repo: &Path, message: &str) -> Result<Option<String>> {
    if !git_has_changes(repo) {
        return Ok(None);
    }
    // .workbuddy/ 下的文件不进 git(状态/笔记噪声,且不能充当"源码改动")
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["add", "-A", "--", ".", ":(exclude).workbuddy"])
        .status();
    let out = git(repo, &["commit", "-m", message])?;
    let sha = git(repo, &["rev-parse", "--short", "HEAD"])?;
    let _ = out;
    Ok(Some(sha))
}

/// 列出仓库内实际文件(供 planner 约束 path 引用,减少幻觉路径);
/// 跳过 .git/target/node_modules/.workbuddy,上限 300 个。
pub fn list_repo_files(repo: &Path) -> String {
    let mut out = Vec::new();
    let mut stack = vec![repo.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if matches!(name, ".git" | "target" | "node_modules" | ".workbuddy") {
                    continue;
                }
                stack.push(p);
            } else if out.len() < 300 {
                if let Some(rel) = p.strip_prefix(repo).ok() {
                    out.push(rel.to_string_lossy().to_string());
                }
            }
        }
    }
    out.sort();
    out.join("\n")
}

// ---------- verify ----------

fn run_verify(repo: &Path, cmd: &str) -> Result<(bool, String)> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(repo)
        .output()
        .with_context(|| format!("运行验证命令失败: {cmd}"))?;
    let ok = out.status.success();
    // 合并 stderr+stdout 作为失败输出:编译错误常在 stderr,测试断言/panic 常在 stdout。
    // 之前只打印不返回,repair 拿不到真正的报错,只能对着 diff --stat 猜。
    let mut output = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !stdout.trim().is_empty() {
        if !output.trim().is_empty() {
            output.push('\n');
        }
        output.push_str(&stdout);
    }
    if !ok {
        eprintln!(
            "[verify] ❌ {} (退出码 {:?})\n{}",
            cmd,
            out.status.code(),
            clip(&output, 2000)
        );
    } else {
        eprintln!("[verify] ✅ {}", cmd);
    }
    Ok((ok, output))
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
    // 关键修复:把改动文件的当前真实内容喂给 LLM。旧行为只给 diff --stat,
    // LLM 看不到真实代码,只能凭空猜 old_string(常拿错文件里的片段)→ old_string 对不上 → 修复落空。
    // 现在:LLM 能看到确切报错 + 每个改动文件的当前全文,据此给出逐字符一致的 old_string。
    let mut files_ctx = String::new();
    let names = git(&cfg.repo_dir, &["diff", "--name-only"]).unwrap_or_default();
    for name in names.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(content) = std::fs::read_to_string(cfg.repo_dir.join(name)) {
            files_ctx.push_str(&format!(
                "\n== 当前文件内容: {} ==\n{}\n",
                name,
                clip(&content, 30_000)
            ));
        }
    }
    if files_ctx.is_empty() {
        // 无未提交改动(如纯测试断言失败):给出最近 diff 供参考
        files_ctx.push_str("\n== 最近改动(git diff) ==\n");
        files_ctx.push_str(&clip(&git(&cfg.repo_dir, &["diff"]).unwrap_or_default(), 8000));
    }
    let system = "你是 WorkBuddy 的修复工程师。验证命令失败了,请找出问题并直接修复代码。\n\
     只输出一个 JSON 对象: {\"edits\": [ ... ]},每个 edit: {\"path\": 仓库相对路径, \"old_string\": 精确原文, \"new_string\": 修复后内容, \"replace_all\": 布尔}。\n\
     old_string 必须与下面\"当前文件内容\"里的真实代码逐字符一致(含缩进);为空表示在文件末尾追加(文件不存在则新建)。不要 markdown 围栏,不要额外文字,最多 5 个 edits。";
    let user = format!(
        "任务: {}\n验证命令: {}\n失败输出(尾部):\n{}\n{}\n请修复:old_string 必须取自上面\"当前文件内容\"里能逐字符匹配的真实代码,不要臆造。",
        task.title,
        verify_cmd,
        clip(failure_output, 4000),
        files_ctx
    );
    let raw = llm.chat(system, &user).await.context("修复 LLM 调用失败")?;
    let ops = edits::parse_edits(&raw).context("解析修复 edits 失败")?;
    if ops.is_empty() {
        return Ok(None);
    }
    let applied = edits::apply_batch(&cfg.repo_dir, &ops)?;
    Ok(Some(applied.join("; ")))
}

// ---------- 主循环 ----------

pub struct DevOptions {
    pub backlog_path: std::path::PathBuf,
    pub verify: Option<String>,
    pub max_tasks: Option<usize>,
    pub max_minutes: Option<u64>,
    /// backlog 耗尽时自动生成 N 个新任务(可重复补给,直到达到该上限),支撑长期运行
    pub extend: Option<usize>,
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
    let mut backlog = load_backlog(&opts.backlog_path)?;
    let mut state = load_state(&cfg.repo_dir);
    if state.started_at.is_none() {
        state.started_at = Some(now_iso());
    }

    // 处理 skip 任务: 不进入正常调度队列, 但在 state.history 中留下跳过记录
    backlog.retain(|task| {
        if task.skip {
            state.history.push(format!("[skipped] {}", task.title));
            eprintln!("[dev] 跳过任务: {} (skip=true)", task.title);
            false
        } else {
            true
        }
    });

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
    let mut extend_rounds = 0usize;
    let mut extend_fail_streak = 0usize;
    // done_set 同时含 done 与 failed:重启后不重复跑已失败任务(省 token);
    // 确需重试时用 --retry-failed 从 failed 移回待办。
    let mut done_set: HashSet<String> = state
        .done
        .iter()
        .chain(state.failed.iter())
        .cloned()
        .collect();

    loop {
        // 每个任务开始前打印 git 状态摘要
        {
            let status = Command::new("git")
                .arg("-C")
                .arg(&cfg.repo_dir)
                .args(["status", "--short"])
                .output();
            match status {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    for line in text.lines().take(5) {
                        eprintln!("[dev] git status: {}", line);
                    }
                }
                Err(e) => eprintln!("[dev] git status 执行失败: {e}"),
            }
        }

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
            // backlog 耗尽:开启 --extend 时自动补给新任务,支撑长期运行
            let Some(per_round) = opts.extend else {
                eprintln!("[dev] backlog 全部处理完毕");
                break;
            };
            if extend_rounds >= MAX_EXTEND_ROUNDS {
                eprintln!("[dev] backlog 耗尽且补给轮数达上限({MAX_EXTEND_ROUNDS}),停止");
                break;
            }
            eprintln!(
                "[dev] backlog 耗尽,第 {} 轮自动生成 {per_round} 个新任务...",
                extend_rounds + 1
            );
            match generate_tasks(&llm, &cfg.repo_dir, per_round).await {
                Ok(new_tasks) if !new_tasks.is_empty() => {
                    backlog.extend(new_tasks);
                    if let Err(e) = save_backlog(&opts.backlog_path, &backlog) {
                        eprintln!(
                            "[dev] 警告:新任务写回 backlog 失败: {}",
                            clip(&e.to_string(), 120)
                        );
                    }
                    eprintln!("[dev] backlog 现为 {} 项", backlog.len());
                    extend_rounds += 1;
                    extend_fail_streak = 0;
                    continue;
                }
                Ok(_) => {
                    eprintln!("[dev] 任务生成返回空,停止");
                    break;
                }
                Err(e) => {
                    // 自补给失败多为瞬时(限流/抖动):等 5 分钟重试,连续失败约 1 小时才停,
                    // 避免一次瞬时错误就终止整个 30 天无人值守。
                    extend_fail_streak += 1;
                    if extend_fail_streak >= 12 {
                        eprintln!(
                            "[dev] 任务生成连续失败 {} 次,停止: {}",
                            extend_fail_streak,
                            clip(&e.to_string(), 120)
                        );
                        break;
                    }
                    eprintln!(
                        "[dev] 任务生成失败,5 分钟后重试(第 {}/12 次): {}",
                        extend_fail_streak,
                        clip(&e.to_string(), 120)
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    continue;
                }
            }
        };
        let task_id = format!("backlog[{}]", i);
        let goal = format!("{}\n{}", task.title, task.description).trim().to_string();
        eprintln!("[dev] === 任务 {}/{}: {} ===", state.done.len() + 1, backlog.len(), task.title);

        let verify_cmd = task
            .verify
            .clone()
            .or_else(|| opts.verify.clone())
            .unwrap_or_else(|| "cargo test".to_string());

        // 规划(最多 3 次,防规划器抖动)
        let filelist = list_repo_files(&cfg.repo_dir);
        let mut plan = None;
        for attempt in 1..=3 {
            match planner::plan_dev_task(&llm, &goal, &filelist).await {
                Ok(p) => {
                    plan = Some(p);
                    break;
                }
                Err(e) => eprintln!("[dev] 规划失败(第 {} 次): {}", attempt, clip(&e.to_string(), 200)),
            }
        }
        let Some(plan) = plan else {
            failed.push(task.title.clone());
            state.failed.push(task_id.clone());
            state.history.push(format!("{} 规划失败,跳过: {}", now_iso(), task.title));
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
        let mut (verify_ok, verify_out) = run_verify(&cfg.repo_dir, &verify_cmd)?;
        let mut repair_count = 0;
        while !verify_ok && repair_count < MAX_REPAIRS {
            repair_count += 1;
            eprintln!("[dev] 验证失败,第 {} 次自动修复...", repair_count);
            match repair(&llm, &cfg, task, &verify_cmd, &verify_out).await {
                Ok(Some(desc)) => {
                    eprintln!("[dev] 修复已应用: {}", clip(&desc, 200));
                    let (v, o) = run_verify(&cfg.repo_dir, &verify_cmd)?;
                    verify_ok = v;
                    verify_out = o;
                }
                Ok(None) => eprintln!("[dev] 修复 LLM 未返回编辑"),
                Err(e) => eprintln!("[dev] 修复失败: {}", clip(&e.to_string(), 200)),
            }
        }

        // 完成判定:验证通过 且 仓库确有源码改动(提交成功)。
        // 防止假成功:步骤全失败/零改动时 cargo test 本来就绿,不能算完成。
        let steps_ok = results.iter().filter(|r| r.ok).count();
        let steps_total = results.len();
        let mut sha = None;
        if verify_ok {
            // 提交(无改动 → 视为未完成任务;防止假成功:步骤全失败/零改动时
            // 验证命令本来就绿,不能算完成)。消息格式必须匹配 pre-commit hook:
            // feat(dev): [N/总数] 任务标题
            let task_no = state.done.len() + 1;
            sha = match git_commit(
                &cfg.repo_dir,
                &format!("feat(dev): [{task_no}/{}] {}", backlog.len(), task.title),
            ) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[dev] 提交失败: {}", clip(&e.to_string(), 200));
                    None
                }
            };
        }
        match sha {
            Some(sha) => {
                eprintln!(
                    "[dev] ✅ 任务完成并已提交: {} ({}) [步骤 {steps_ok}/{steps_total}]",
                    task.title, sha
                );
                completed.push(task.title.clone());
                state.done.push(task_id.clone());
                state
                    .history
                    .push(format!("{} ✅ {} @ {}", now_iso(), task.title, sha));
                done_set.insert(task_id);
            }
            None => {
                let reason = if verify_ok {
                    "验证通过但无源码改动".to_string()
                } else {
                    format!("验证未通过(已尝试 {repair_count} 次修复)")
                };
                mark_task_failed(
                    &cfg.repo_dir,
                    &task.title,
                    &task_id,
                    &reason,
                    steps_ok,
                    steps_total,
                    &mut failed,
                    &mut state,
                    &mut done_set,
                );
            }
        }
        save_state(&cfg.repo_dir, &state)?;
    }

    eprintln!(
        "[dev] 循环结束: 本次完成 {} 项,失败 {} 项",
        completed.len(),
        failed.len()
    );
    Ok(DevReport { completed, failed })
}

/// 标记任务失败:回滚脏改动(避免污染下一个任务)并记录 state。
fn mark_task_failed(
    repo: &Path,
    title: &str,
    task_id: &str,
    reason: &str,
    steps_ok: usize,
    steps_total: usize,
    failed: &mut Vec<String>,
    state: &mut DevState,
    done_set: &mut HashSet<String>,
) {
    eprintln!(
        "[dev] ❌ 任务未完成: {title} ({reason}) [步骤 {steps_ok}/{steps_total}]"
    );
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["checkout", "--", "."])
        .status();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["clean", "-fd", "-e", ".workbuddy"])
        .status();
    failed.push(title.to_string());
    state.failed.push(task_id.to_string());
    state.history.push(format!("{} ❌ {} ({reason})", now_iso(), title));
    done_set.insert(task_id.to_string());
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
                extend: None,
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

    // 捕获提示的 mock LLM:记录收到的 user 提示,并返回一个合法(追加)edit
    struct CapturingLlm {
        users: std::sync::Mutex<Vec<String>>,
    }
    impl crate::llm::Llm for CapturingLlm {
        fn chat<'a>(
            &'a self,
            _system: &'a str,
            user: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>,
        > {
            self.users.lock().unwrap().push(user.to_string());
            let r = r#"{"edits":[{"path":"code.rs","old_string":"","new_string":"x","replace_all":false}]}"#
                .to_string();
            Box::pin(async move { Ok(r) })
        }
    }

    // 回归:自修复提示必须包含改动文件的当前真实内容 + 真实报错,
    // 否则 LLM 只能凭空猜 old_string(对不上)→ 修复落空(2026-10-14 主因)。
    #[tokio::test]
    async fn repair_prompt_includes_changed_file_content_and_error() {
        let dir = std::env::temp_dir().join(format!("wb-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| {
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
        std::fs::write(dir.join("code.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-m", "init"]);
        // 改动 code.rs(未提交)→ 进入 git diff --name-only
        std::fs::write(dir.join("code.rs"), "fn main() {\n    let x = 1;\n    let y = x;\n}\n").unwrap();

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
        let task = DevTask {
            title: "test task".into(),
            description: "mock".into(),
            verify: None,
            priority: 1,
            skip: false,
        };
        let llm: std::sync::Arc<dyn crate::llm::Llm> =
            std::sync::Arc::new(CapturingLlm {
                users: std::sync::Mutex::new(Vec::new()),
            });
        let _ = repair(&llm, &cfg, &task, "cargo test", "编译错误: borrow of moved value")
            .await
            .unwrap();
        let users = llm.users.lock().unwrap();
        assert!(!users.is_empty(), "repair 应调用 LLM");
        let prompt = &users[0];
        assert!(
            prompt.contains("fn main()"),
            "repair 提示应包含改动文件当前内容"
        );
        assert!(
            prompt.contains("let y = x"),
            "repair 提示应包含改动后的真实代码行"
        );
        assert!(
            prompt.contains("borrow of moved value"),
            "repair 提示应包含真实报错"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod dev_skip_tests {
    use super::*;

    #[test]
    fn backlog_parses_skip_field() {
        let dir = std::env::temp_dir().join(format!("workbuddy_devskip_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("backlog.json");
        std::fs::write(&path, r#"{"tasks":[{"title":"skip me","skip":true},{"title":"normal"}]}"#).unwrap();
        let tasks = load_backlog(&path).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks[0].skip);
        assert!(!tasks[1].skip);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skip_true_filters_out_and_records_history() {
        let tasks = vec![
            DevTask { title: "skip".into(), description: String::new(), verify: None, priority: 0, skip: true },
            DevTask { title: "run".into(), description: String::new(), verify: None, priority: 0, skip: false },
        ];
        let mut state = DevState::default();
        let mut active = tasks;
        active.retain(|t| {
            if t.skip {
                state.history.push(format!("[skipped] {}", t.title));
                false
            } else {
                true
            }
        });
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].title, "run");
        assert_eq!(state.history.len(), 1);
        assert!(state.history[0].contains("skip"));
    }
}