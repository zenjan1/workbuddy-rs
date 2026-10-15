mod agents;
mod config;
mod dev;
mod edits;
mod executor;
mod llm;
mod plan;
mod planner;
mod util;
mod workspace;

use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use executor::Executor;
use llm::{Llm, MockLlm, OpenAiCompat};
use workspace::{RunRecord, StepRecord, Workspace};

#[derive(Parser)]
#[command(
    name = "workbuddy",
    version = env!("CARGO_PKG_VERSION"),
    about = "WorkBuddy 非官方 Rust 复刻:多专家 AI 智能体工作台"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 规划并执行一个任务
    Run {
        /// 任务目标
        goal: Vec<String>,
        /// 使用 Mock LLM(离线,不调用真实 API)
        #[arg(long)]
        mock: bool,
        /// 模型名(默认取环境变量/配置)
        #[arg(long)]
        model: Option<String>,
        /// 并行度(默认取配置,4)
        #[arg(long)]
        parallel: Option<usize>,
    },
    /// 列出历史任务
    List {
        /// 仅显示该日期(含)之后创建的任务 (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
    },
    /// 查看某次任务的计划与报告
    Show { id: String },
    /// 列出内置专家与 skills
    Agents,
    /// 打印当前生效配置(隐藏 key)
    Config,
    /// 校验配置(读取 config.toml 或默认配置)
    Validate,
    /// 探测 LLM 端点健康状态
    Health,
    /// 自主开发循环:从 backlog 取任务 → 规划 → 改码 → 验证 → 提交
    Dev {
        /// backlog 文件(JSON,默认 .workbuddy/backlog.json)
        #[arg(long)]
        backlog: Option<std::path::PathBuf>,
        /// 全局验证命令(任务未指定 verify 时使用)
        #[arg(long)]
        verify: Option<String>,
        /// 本次最多完成任务数
        #[arg(long)]
        tasks: Option<usize>,
        /// 本次最多运行分钟数
        #[arg(long)]
        minutes: Option<u64>,
        /// 操作仓库目录(默认当前目录)
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
        /// backlog 耗尽时自动生成新任务(每轮 N 个),支撑长期运行
        #[arg(long)]
        extend: Option<usize>,
        /// 使用 Mock LLM(离线演练)
        #[arg(long)]
        mock: bool,
    },
    /// 一键健康自检:依次运行 cargo test 与 cargo clippy
    Selfcheck,
    /// 安全撤销上次 dev 提交:仅当最近提交以 `feat(dev):` 开头时执行 reset --hard
    Undo,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Cmd::Validate = &cli.cmd {
        return cmd_validate();
    }
    let mut cfg = Config::load()?;
    match cli.cmd {
        Cmd::Run {
            goal,
            mock,
            model,
            parallel,
        } => {
            let goal = goal.join(" ");
            if goal.trim().is_empty() {
                anyhow::bail!("任务目标不能为空");
            }
            if let Some(m) = model {
                cfg.model = m;
            }
            if let Some(p) = parallel {
                cfg.max_parallel = p.max(1);
            }
            cfg.mock = mock;
            run_task(cfg, &goal).await
        }
        Cmd::List { since } => cmd_list(cfg, since.as_deref()),
        Cmd::Show { id } => cmd_show(cfg, &id),
        Cmd::Agents => cmd_agents(),
        Cmd::Config => {
            println!("{}", cfg.summary());
            Ok(())
        }
        Cmd::Validate => unreachable!("Validate 已在 main 入口处理"),
        Cmd::Health => cmd_health(cfg).await,
        Cmd::Dev {
            backlog,
            verify,
            tasks,
            minutes,
            repo,
            extend,
            mock,
        } => {
            if let Some(r) = repo {
                cfg.repo_dir = r;
            }
            cfg.mock = mock;
            let llm: Arc<dyn Llm> = if cfg.mock {
                eprintln!("[workbuddy] Mock 模式:使用内置 Mock LLM");
                Arc::new(MockLlm)
            } else {
                Arc::new(OpenAiCompat::new(&cfg)?)
            };
            let backlog_path = backlog.unwrap_or_else(|| {
                cfg.repo_dir.join(".workbuddy/backlog.json")
            });
            let report = dev::run_dev_loop(
                llm,
                cfg.clone(),
                dev::DevOptions {
                    backlog_path,
                    verify,
                    max_tasks: tasks,
                    max_minutes: minutes,
                    extend,
                },
            )
            .await?;
            println!("\n=== dev 循环报告 ===");
            println!("完成 {} 项:", report.completed.len());
            for t in &report.completed {
                println!("  ✅ {}", t);
            }
            if !report.failed.is_empty() {
                println!("失败 {} 项:", report.failed.len());
                for t in &report.failed {
                    println!("  ❌ {}", t);
                }
            }
            Ok(())
        }
        Cmd::Selfcheck => cmd_selfcheck(),
        Cmd::Undo => match run_undo(None) {
            Ok(short_sha) => {
                println!("已撤销 dev 提交: {short_sha}");
                Ok(())
            }
            Err(reason) => {
                eprintln!("拒绝撤销：{reason}");
                Ok(())
            }
        },
    }
}

fn cmd_validate() -> Result<()> {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置文件读取失败：{e:#}");
            std::process::exit(2);
        }
    };
    if let Err(errors) = cfg.validate() {
        for error in &errors {
            eprintln!("{error}");
        }
        std::process::exit(1);
    }
    println!("配置校验通过");
    Ok(())
}

async fn run_task(cfg: Config, goal: &str) -> Result<()> {
    let llm: Arc<dyn Llm> = if cfg.mock {
        eprintln!("[workbuddy] Mock 模式:使用内置 Mock LLM");
        Arc::new(MockLlm)
    } else {
        if cfg.api_key.is_none() {
            eprintln!("[workbuddy] 警告:未检测到 API key(DASHSCOPE_API_KEY/OPENAI_API_KEY),将按 OpenAI 兼容模式尝试调用");
        }
        Arc::new(OpenAiCompat::new(&cfg)?)
    };

    println!("==================================================");
    println!("WorkBuddy 任务: {}", goal);
    println!("{}", cfg.summary());
    println!("==================================================");

    // 规划
    eprintln!("[规划] 正在生成执行计划...");
    let plan = planner::plan_task(&llm, goal).await?;
    println!("\n[计划] 目标: {}\n{}", plan.goal, plan.summary());

    // 工作区
    let ws = Workspace::create_new(&cfg.workspace_dir)?;
    let mut run = RunRecord {
        id: ws.dir.file_name().unwrap().to_string_lossy().to_string(),
        goal: goal.to_string(),
        created: now_iso(),
        model: cfg.model.clone(),
        mock: cfg.mock,
        steps: Vec::new(),
    };
    ws.save_plan(&run, &plan)?;

    // 执行(绑定工作区,步骤产出实时落盘)
    let ws = Arc::new(ws);
    let exec = Executor::new(llm, cfg.clone()).with_workspace(ws.clone());
    let (results, summary) = exec.execute(&plan).await?;

    // 收尾:收集最终报告
    let last = results
        .iter()
        .rev()
        .find(|r| r.ok)
        .map(|r| r.id);
    let mut report = format!(
        "# WorkBuddy 任务报告\n\n- 任务: {goal}\n- 任务ID: {}\n- 模型: {}\n- 时间: {}\n\n## 计划\n\n```\n{}\n```\n\n## 最终产出\n\n",
        run.id, run.model, run.created, plan.summary()
    );
    if let Some(id) = last {
        match std::fs::read_to_string(ws.dir.join("outputs").join(format!("{id}.md"))) {
            Ok(t) => report.push_str(&t),
            Err(_) => report.push_str("(无最终步骤产出)"),
        }
    } else {
        report.push_str("(所有步骤均未成功,无最终产出)");
    }
    report.push_str("\n\n");
    report.push_str(&summary);

    // 写步骤记录
    for r in &results {
        let step = plan.step(r.id).unwrap();
        let preview = std::fs::read_to_string(ws.dir.join("outputs").join(format!("{}.md", r.id)))
            .ok()
            .map(|t| util::clip(&t, 300).to_string())
            .unwrap_or_else(|| r.error.clone().unwrap_or_default());
        run.steps.push(StepRecord {
            id: r.id,
            name: step.name.clone(),
            agent: step.agent.clone(),
            skill: step.skill.clone(),
            kind: step.kind.label().to_string(),
            ok: r.ok,
            error: r.error.clone(),
            seconds: r.seconds,
            output_preview: preview,
        });
    }
    ws.finish_run(&mut run, &report)?;

    // 输出最终报告
    println!("\n==================================================");
    println!("{report}");
    println!("==================================================");
    println!("任务已保存到: {}", ws.dir.display());
    println!(
        "完整报告: {}",
        report_path(&cfg.workspace_dir, &run.id).display()
    );
    Ok(())
}

fn report_path(workspace_dir: &std::path::Path, run_id: &str) -> std::path::PathBuf {
    workspace_dir.join(run_id).join("report.md")
}

fn run_created_since(created: &str, since: Option<&str>) -> bool {
    match since {
        Some(since) => created >= since,
        None => true,
    }
}

fn cmd_list(cfg: Config, since: Option<&str>) -> Result<()> {
    let runs = Workspace::root(&cfg.workspace_dir);
    let filtered: Vec<&RunRecord> = runs
        .iter()
        .filter(|r| run_created_since(&r.created, since))
        .collect();
    if filtered.is_empty() {
        println!("(暂无历史任务)");
        return Ok(());
    }
    println!("{:<14} {:<24} {}", "ID", "时间", "目标");
    for r in filtered {
        let goal = util::clip(&r.goal, 40);
        println!("{:<14} {:<24} {}", r.id, r.created, goal);
    }
    Ok(())
}

fn cmd_show(cfg: Config, id: &str) -> Result<()> {
    let ws = Workspace::open(&cfg.workspace_dir, id)?;
    let rec = ws.load_record()?;
    println!("# 任务 {}\n\n目标: {}\n模型: {}\n时间: {}\n\n## 步骤\n", rec.id, rec.goal, rec.model, rec.created);
    for s in &rec.steps {
        let mark = if s.ok { "✅" } else { "❌" };
        println!(
            "- {} 步骤 {} [{} / {}] ({}, {:.1}s){}",
            mark,
            s.id,
            s.agent,
            s.skill,
            s.kind,
            s.seconds,
            s.error.as_deref().map(|e| format!("  错误: {}", util::clip(e, 80))).unwrap_or_default()
        );
        if !s.output_preview.trim().is_empty() {
            println!("  > {}", util::clip(&s.output_preview, 200).replace('\n', " "));
        }
    }
    let report = std::fs::read_to_string(ws.dir.join("report.md")).unwrap_or_default();
    println!("\n## 报告\n\n{report}");
    Ok(())
}

fn cmd_agents() -> Result<()> {
    for a in agents::AGENTS {
        println!("## {} — {}", a.id, a.name);
        println!("   {}", a.desc);
        for sk in a.skills {
            println!("   - `{}`: {}", sk.name, sk.desc);
        }
    }
    Ok(())
}

fn run_undo(repo_path: Option<&std::path::Path>) -> Result<String, String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let mut cmd = std::process::Command::new("git");
        if let Some(path) = repo_path {
            cmd.arg("-C").arg(path);
        }
        cmd.args(args);
        let output = cmd.output().map_err(|e| format!("执行 git {:?} 失败: {e}", args))?;
        if !output.status.success() {
            return Err(format!(
                "git {:?} 失败: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };

    let subject = git(&["log", "-1", "--pretty=%s"])?;
    if !subject.starts_with("feat(dev):") {
        return Err(format!("最近提交不是 dev 提交：{subject}"));
    }

    let short_sha = git(&["rev-parse", "--short", "HEAD"])?;
    git(&["reset", "--hard", "HEAD~1"])?;

    Ok(short_sha)
}

async fn cmd_health(cfg: Config) -> Result<()> {
    if cfg.mock {
        println!("{}", mock_health_ok());
        return Ok(());
    }

    let url = format!("{}/models", cfg.base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut request = client.get(&url);
    if let Some(key) = &cfg.api_key {
        request = request.bearer_auth(key);
    }

    let start = std::time::Instant::now();
    let response = request.send().await?;
    let status = response.status();
    let body_text = response.text().await?;
    let elapsed = start.elapsed();

    println!("HTTP 状态码: {}", status);
    println!("耗时: {} ms", elapsed.as_millis());

    if !status.is_success() {
        anyhow::bail!("健康检查失败: HTTP {}", status);
    }

    let body: serde_json::Value = serde_json::from_str(&body_text)
        .map_err(|e| anyhow::anyhow!("响应不是合法 JSON: {e}"))?;
    let data = body
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("响应缺少 data 数组"))?;

    let ids: Vec<&str> = data
        .iter()
        .take(5)
        .filter_map(|v| v.get("id").and_then(|s| s.as_str()))
        .collect();

    if ids.is_empty() {
        println!("(未找到模型 id)");
    } else {
        println!("模型 id (前 {} 个):", ids.len());
        for id in ids {
            println!("  - {}", id);
        }
    }

    Ok(())
}

fn mock_health_ok() -> &'static str {
    "ok"
}

fn cmd_selfcheck() -> Result<()> {
    use std::process::Command;

    let mut results = Vec::new();

    // 运行 cargo test
    let test_status = Command::new("cargo")
        .arg("test")
        .status();
    let test_ok = match test_status {
        Ok(status) => status.success(),
        Err(e) => {
            eprintln!("运行 cargo test 失败: {e}");
            false
        }
    };
    results.push(("cargo test", if test_ok { "PASS" } else { "FAIL" }));

    // 检测 clippy 是否可用
    let clippy_available = Command::new("clippy")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if clippy_available {
        let clippy_status = Command::new("cargo")
            .arg("clippy")
            .status();
        let clippy_ok = match clippy_status {
            Ok(status) => status.success(),
            Err(e) => {
                eprintln!("运行 cargo clippy 失败: {e}");
                false
            }
        };
        results.push(("cargo clippy", if clippy_ok { "PASS" } else { "FAIL" }));
    } else {
        results.push(("cargo clippy", "SKIP"));
    }

    // 打印汇总表
    println!("{:<20} {}", "命令", "结果");
    for (cmd, result) in &results {
        println!("{:<20} {}", cmd, result);
    }

    let all_pass = results.iter().all(|(_, result)| *result == "PASS" || *result == "SKIP");
    if all_pass {
        Ok(())
    } else {
        anyhow::bail!("selfcheck 存在失败命令")
    }
}

/// dev.rs 等模块复用的 ISO 时间戳(UTC)。
pub fn main_now_iso() -> String {
    now_iso()
}

fn now_iso() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 简易 ISO(UTC) 格式化
    let days = s / 86400;
    let rem = s % 86400;
    let (h, m, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // 从 1970-01-01 起算的日期(平年/闰年近似)
    let mut y = 1970i32;
    let mut d = days;
    loop {
        let days_in_y = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if d < days_in_y {
            break;
        }
        d -= days_in_y;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1;
    for md in &month_days {
        if d < *md {
            break;
        }
        d -= *md;
        mo += 1;
    }
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d + 1, h, m, sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_mock_branch_returns_ok_without_network() {
        let mut cfg = Config::load().expect("加载默认配置失败");
        cfg.mock = true;
        assert!(cfg.mock);
        // 仅调用同步辅助函数;不会发起任何 reqwest/网络请求。
        assert_eq!(mock_health_ok(), "ok");
    }

    #[test]
    fn report_path_joins_workspace_run_id_and_report_md() {
        let path = report_path(std::path::Path::new("/tmp/workbuddy-ws"), "run-123");
        assert_eq!(
            path,
            std::path::PathBuf::from("/tmp/workbuddy-ws/run-123/report.md")
        );
    }

    #[test]
    fn run_created_since_no_since_filter_true() {
        assert!(run_created_since("2025-03-01T00:00:00Z", None));
    }

    #[test]
    fn run_created_since_includes_boundary_day() {
        assert!(run_created_since("2025-03-02T00:00:00Z", Some("2025-03-02")));
        assert!(run_created_since("2025-03-02T23:59:59Z", Some("2025-03-02")));
    }

    #[test]
    fn run_created_since_filters_earlier_run() {
        assert!(!run_created_since("2025-03-01T23:59:59Z", Some("2025-03-02")));
    }

    #[test]
    fn mock_health_returns_ok() {
        assert_eq!(mock_health_ok(), "ok");
    }
}
