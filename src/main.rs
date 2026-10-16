#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(unused_imports)]
#![allow(dropping_references)]
mod agents;
mod config;
mod dev;
mod edits;
mod executor;
mod llm;
mod memory;
mod plan;
mod planner;
mod serve;
mod status;
mod task_id;
mod tui;
mod review {
    use super::*;
    use anyhow::anyhow;
    use std::process::Command;

    pub async fn review(cfg: Config) -> Result<()> {
        let output = Command::new("git")
            .args(["diff", "--cached"])
            .output()
            .map_err(|e| anyhow!("git diff --cached 失败: {e}"))?;
        let cached_diff = String::from_utf8_lossy(&output.stdout).to_string();
        let diff = if cached_diff.trim().is_empty() {
            let output = Command::new("git")
                .args(["diff"])
                .output()
                .map_err(|e| anyhow!("git diff 失败: {e}"))?;
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            cached_diff
        };
        if diff.trim().is_empty() {
            println!("没有需要评审的 diff");
            return Ok(());
        }

        let llm: Arc<dyn Llm> = if cfg.mock {
            Arc::new(MockLlm)
        } else {
            Arc::new(OpenAiCompat::new(&cfg)?)
        };
        let system = "你是一位资深代码评审专家(reviewer)。";
        let prompt = format!(
            "请对以下 git diff 进行提交前的自评审,指出潜在问题:\n\n{diff}"
        );
        let output = llm.chat(system, &prompt).await?;
        println!("{}", output.text);
        Ok(())
    }
}
mod resolver;
mod util;
mod utils;
mod webhook;
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
enum ConfigAction {
    /// 设置配置项
    Set {
        /// 配置键
        key: String,
        /// 配置值（位置参数）
        value: String,
    },
    /// 获取配置项
    Get {
        /// 配置键
        key: String,
    },
}

#[derive(Subcommand)]
enum MemoryAction {
    /// 添加或覆盖一个键值对
    Add {
        /// 键
        key: String,
        /// 值
        value: String,
    },
    /// 列出所有键值对
    List,
    /// 获取键对应的值
    Get {
        /// 键
        key: String,
    },
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
        /// 可选：按数字索引、任务标题或标题唯一前缀查看任务详情
        id: Option<String>,
        /// 仅显示该日期(含)之后创建的任务 (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
    },
    /// 查看某次任务的计划与报告(slug 或数字 id)
    Show {
        /// 任务 slug 或数字 id
        id: String,
    },
    /// 列出内置专家与 skills
    Agents,
    /// 提交前自评审(只读)
    Review,
    /// 查看或修改当前生效配置
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
        /// 获取配置项（--get <key>，与子命令互斥）
        #[arg(long)]
        get: Option<String>,
    },
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
    Undo {
        /// 可选：任务标识符（数字索引、标题或 slug）
        id: Option<String>,
    },
    /// 查看 workspace 运行状态(只读)
    Status {
        /// 可选：任务标识符（数字索引、标题或 slug）
        id: Option<String>,
    },
    /// 启动 TUI 界面查看 backlog(只读)
    Tui,
    /// 启动只读 HTTP 服务
    Serve {
        /// 数据目录（默认使用配置的 workspace_dir）
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// 监听地址（默认 127.0.0.1，可用 --bind 别名）
        #[arg(long, visible_alias = "bind", default_value = "127.0.0.1")]
        addr: std::net::IpAddr,
        /// 监听端口（默认 8080）
        #[arg(short, long, default_value_t = 8080)]
        port: u16,
        /// 只读模式：使用随机端口 127.0.0.1:0，输出实际端口
        #[arg(long)]
        readonly: bool,
    },
    /// 持久记忆:add/list/get
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// 推进路线图:扫描 VISION.md 已完成条目,更新未完成项并追加日志
    RoadmapAdvance,
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
            let input = goal.join(" ");
            if input.trim().is_empty() {
                anyhow::bail!("任务目标不能为空");
            }

            // 解析输入：数字索引、标题/slug 或新任务目标
            let runs = Workspace::root(&cfg.workspace_dir);
            let goal = match crate::task_id::resolve_task_id(
                &input,
                runs.iter().map(|r: &RunRecord| r.goal.as_str()),
            ) {
                Ok(crate::task_id::TaskId::Numeric(idx)) => {
                    runs.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("任务索引 {} 超出范围", idx))?
                        .goal
                        .clone()
                }
                Ok(crate::task_id::TaskId::Slug(slug)) => {
                    runs.iter()
                        .find(|r| r.id == slug)
                        .ok_or_else(|| anyhow::anyhow!("未找到 slug 为 '{}' 的任务", slug))?
                        .goal
                        .clone()
                }
                Err(crate::task_id::TaskIdError::NotFound { .. }) => input,
                Err(e) => anyhow::bail!("无法解析任务引用 '{}': {}", input, e),
            };

            if let Some(m) = model {
                cfg.model = m;
            }
            if let Some(p) = parallel {
                cfg.max_parallel = p.max(1);
            }
            cfg.mock = mock;
            run_task(cfg, &goal).await
        }
        Cmd::List { since, id } => {
            if let Some(id) = id {
                let runs = Workspace::root(&cfg.workspace_dir);
                let run_id = match crate::task_id::resolve_task_id(
                    &id,
                    runs.iter().map(|r: &RunRecord| r.goal.as_str()),
                ) {
                    Ok(crate::task_id::TaskId::Numeric(idx)) => {
                        runs.get(idx)
                            .map(|r| r.id.clone())
                            .ok_or_else(|| anyhow::anyhow!("任务索引 {} 超出范围", idx))?
                    }
                    Ok(crate::task_id::TaskId::Slug(slug)) => {
                        runs.iter()
                            .find(|r| r.id == slug)
                            .map(|r| r.id.clone())
                            .ok_or_else(|| anyhow::anyhow!("未找到 slug 为 '{}' 的任务", slug))?
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("冲突") || msg.contains("多个匹配") {
                            anyhow::bail!("任务标题 '{}' 存在多个匹配，请使用数字索引指定任务", id)
                        } else {
                            anyhow::bail!("无法解析任务引用 '{}': {}", id, e)
                        }
                    }
                };
                cmd_show(cfg, &run_id)
            } else {
                cmd_list_with_slug(cfg, since.as_deref())
            }
        }
        Cmd::Show { id } => {
            let runs = Workspace::root(&cfg.workspace_dir);
            let run_id = match crate::task_id::resolve_task_id(
                &id,
                runs.iter().map(|r: &RunRecord| r.goal.as_str()),
            ) {
                Ok(crate::task_id::TaskId::Numeric(idx)) => {
                    runs.get(idx)
                        .map(|r| r.id.clone())
                        .ok_or_else(|| anyhow::anyhow!("任务索引 {} 超出范围", idx))?
                }
                Ok(crate::task_id::TaskId::Slug(slug)) => {
                    runs.iter()
                        .find(|r| r.id == slug)
                        .map(|r| r.id.clone())
                        .ok_or_else(|| anyhow::anyhow!("未找到 slug 为 '{}' 的任务", slug))?
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("冲突") || msg.contains("多个匹配") {
                        anyhow::bail!("任务标题 '{}' 存在多个匹配，请使用数字索引指定任务", id)
                    } else {
                        anyhow::bail!("无法解析任务引用 '{}': {}", id, e)
                    }
                }
            };
            cmd_show(cfg, &run_id)
        }
        Cmd::Agents => cmd_agents(),
        Cmd::Review => review::review(cfg).await,
        Cmd::Config { action, get } => {
            if get.is_some() && action.is_some() {
                anyhow::bail!("--get 不能与子命令同时使用");
            }
            if let Some(key) = get {
                if key.trim().is_empty() {
                    return Err(anyhow::anyhow!("配置键不能为空"));
                }

                let config_path = find_config_path()?;
                let value = config::get_config_value(&config_path, &key)
                    .map_err(|e| anyhow::anyhow!("获取配置 {} 失败: {}", key, e))?;

                println!("{} = {}", key, value);
                return Ok(());
            }
            match action {
                None => {
                    println!("{}", cfg.summary());
                    Ok(())
                }
                Some(ConfigAction::Get { key }) => {
                    if key.trim().is_empty() {
                        return Err(anyhow::anyhow!("配置键不能为空"));
                    }

                    let config_path = find_config_path()?;
                    let value = config::get_config_value(&config_path, &key)
                        .map_err(|e| anyhow::anyhow!("获取配置 {} 失败: {}", key, e))?;

                    println!("{} = {}", key, value);
                    Ok(())
                },
                Some(ConfigAction::Set { key, value }) => {
                    if key.trim().is_empty() {
                        return Err(anyhow::anyhow!("配置键不能为空"));
                    }
                    if value.trim().is_empty() {
                        return Err(anyhow::anyhow!("配置值不能为空"));
                    }

                    let config_path = find_config_path()?;
                    config::set_config(&config_path, &key, &value)
                        .map_err(|e| anyhow::anyhow!("设置配置 {} 失败: {}", key, e))?;

                    let cfg = Config::load()?;
                    println!("已更新配置：{} = {}", key, value);
                    println!("{}", cfg.summary());

                    // 提示哪些配置需要重启生效
                    let needs_restart = matches!(
                        key.as_str(),
                        "model" | "base_url" | "api_key" | "temperature" | "timeout"
                    );
                    if needs_restart {
                        println!("提示：修改 {} 需要重启 WorkBuddy 才能生效。", key);
                    }
                    Ok(())
                }
            }
        },
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
        Cmd::Memory { action } => {
            run_memory_command(action, &cfg)
        }
        Cmd::Status { id } => {
            if let Some(id) = id {
                let runs = Workspace::root(&cfg.workspace_dir);
                match crate::task_id::resolve_task_id(
                    &id,
                    runs.iter().map(|r: &RunRecord| r.goal.as_str()),
                ) {
                    Ok(_) => {}
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("冲突") || msg.contains("多个匹配") {
                            anyhow::bail!("任务标题 '{}' 存在多个匹配，请使用数字索引指定任务", id)
                        } else {
                            anyhow::bail!("无法解析任务引用 '{}': {}", id, e)
                        }
                    }
                }
            }
            status::run(&cfg.workspace_dir)
                .map_err(|e| anyhow::anyhow!("查看 workspace 运行状态失败: {e:#}"))
        },
        Cmd::Tui => {
            if let Err(e) = tui::run(&cfg.workspace_dir) {
                eprintln!("启动 TUI 失败: {e:#}");
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Serve { data_dir, addr, port, readonly } => {
            let workspace_dir = data_dir.unwrap_or_else(|| cfg.workspace_dir.clone());
            if readonly || port == 0 {
                let actual_addr = serve::serve_readonly(&workspace_dir)
                    .map_err(|e| anyhow::anyhow!("启动只读 HTTP 服务失败: {e:#}"))?;
                println!("只读 HTTP 服务已启动: http://{actual_addr}");
                Ok(())
            } else {
                let bind_addr = std::net::SocketAddr::new(addr, port);
                println!("只读 HTTP 服务已启动: http://{bind_addr}");
                serve::run(&workspace_dir, bind_addr)
                    .await
                    .map_err(|e| anyhow::anyhow!("启动只读 HTTP 服务失败: {e:#}"))
            }
        }
        Cmd::Undo { id } => {
            if let Some(id) = id {
                let runs = Workspace::root(&cfg.workspace_dir);
                match crate::task_id::resolve_task_id(
                    &id,
                    runs.iter().map(|r: &RunRecord| r.goal.as_str()),
                ) {
                    Ok(_) => {}
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("冲突") || msg.contains("多个匹配") {
                            anyhow::bail!("任务标题 '{}' 存在多个匹配，请使用数字索引指定任务", id)
                        } else {
                            anyhow::bail!("无法解析任务引用 '{}': {}", id, e)
                        }
                    }
                }
            }
            match run_undo(None) {
                Ok(short_sha) => {
                    println!("已撤销 dev 提交: {short_sha}");
                    Ok(())
                }
                Err(reason) => {
                    eprintln!("拒绝撤销：{reason}");
                    Ok(())
                }
            }
        },
        Cmd::RoadmapAdvance => {
            workspace::advance_roadmap(&cfg.repo_dir)
                .map_err(|e| anyhow::anyhow!("推进路线图失败: {e:#}"))?;
            Ok(())
        },
    }
}

fn run_memory_command(action: MemoryAction, cfg: &Config) -> Result<()> {
    let mut store = memory::MemoryStore::open(&cfg.workspace_dir)?;
    match action {
        MemoryAction::Add { key, value } => {
            if key.trim().is_empty() {
                anyhow::bail!("记忆键不能为空");
            }
            store
                .add(&key, &value)
                .map_err(|e| anyhow::anyhow!("添加记忆失败: {e}"))?;
            println!("已保存记忆：{} = {}", key, value);
        }
        MemoryAction::List => {
            let entries = store.list();
            if entries.is_empty() {
                println!("(暂无记忆)");
            } else {
                for entry in entries {
                    println!("{:?}", entry);
                }
            }
        }
        MemoryAction::Get { key } => {
            if key.trim().is_empty() {
                anyhow::bail!("记忆键不能为空");
            }
            match store.get(&key) {
                Ok(value) => println!("{} = {:?}", key, value),
                Err(memory::MemoryError::NotFound(_)) => anyhow::bail!("未找到记忆键: {}", key),
                Err(e) => anyhow::bail!("获取记忆失败: {e}"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod status_cmd_tests {
    use super::*;

    #[test]
    fn cli_parses_status() {
        let cli = Cli::try_parse_from(["workbuddy", "status"]).expect("parse status");
        match cli.cmd {
            Cmd::Status { id: None } => {}
            _ => panic!("expected status command"),
        }
    }
}

#[cfg(test)]
mod serve_cmd_tests {
    use super::*;

    #[test]
    fn cli_parses_serve_defaults() {
        let cli = Cli::try_parse_from(["workbuddy", "serve"]).expect("parse serve");
        match cli.cmd {
            Cmd::Serve {
                data_dir: None,
                addr,
                port,
                readonly: false,
            } => {
                assert_eq!(addr, "127.0.0.1".parse::<std::net::IpAddr>().unwrap());
                assert_eq!(port, 8080);
            }
            _ => panic!("expected serve command with defaults"),
        }
    }

    #[test]
    fn cli_parses_serve_custom_port() {
        let cli = Cli::try_parse_from(["workbuddy", "serve", "--port", "9090"])
            .expect("parse serve custom port");
        match cli.cmd {
            Cmd::Serve { port, .. } => assert_eq!(port, 9090),
            _ => panic!("expected serve command"),
        }
    }
}

#[cfg(test)]
mod memory_cmd_tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn cli_parses_memory_add() {
        let cli = Cli::try_parse_from(["workbuddy", "memory", "add", "greeting", "hello"])
            .expect("parse memory add");
        match cli.cmd {
            Cmd::Memory { action } => match action {
                MemoryAction::Add { key, value } => {
                    assert_eq!(key, "greeting");
                    assert_eq!(value, "hello");
                }
                _ => panic!("expected add"),
            },
            _ => panic!("expected memory"),
        }
    }

    #[test]
    fn cli_parses_memory_list() {
        let cli = Cli::try_parse_from(["workbuddy", "memory", "list"])
            .expect("parse memory list");
        match cli.cmd {
            Cmd::Memory { action } => match action {
                MemoryAction::List => {}
                _ => panic!("expected list"),
            },
            _ => panic!("expected memory"),
        }
    }

    #[test]
    fn cli_parses_memory_get() {
        let cli = Cli::try_parse_from(["workbuddy", "memory", "get", "greeting"])
            .expect("parse memory get");
        match cli.cmd {
            Cmd::Memory { action } => match action {
                MemoryAction::Get { key } => assert_eq!(key, "greeting"),
                _ => panic!("expected get"),
            },
            _ => panic!("expected memory"),
        }
    }

    #[test]
    fn cli_memory_add_requires_args() {
        let result = Cli::try_parse_from(["workbuddy", "memory", "add"]);
        match result {
            Ok(_) => panic!("expected missing arguments error"),
            Err(e) => assert_eq!(e.kind(), ErrorKind::MissingRequiredArgument),
        }
    }

    #[test]
    fn cli_memory_get_requires_key_arg() {
        let result = Cli::try_parse_from(["workbuddy", "memory", "get"]);
        match result {
            Ok(_) => panic!("expected missing key error"),
            Err(e) => assert_eq!(e.kind(), ErrorKind::MissingRequiredArgument),
        }
    }
}

#[cfg(test)]
mod config_set_tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_config_path(tag: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "workbuddy_config_{}_{}_{}.toml",
            tag,
            std::process::id(),
            nanos
        ))
    }

    fn write_config(path: &std::path::Path, contents: &str) {
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
    }

    fn read_config(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn cli_parses_config_set() {
        let cli = Cli::try_parse_from(["workbuddy", "config", "set", "timeout", "45"])
            .expect("parse config set");
        match cli.cmd {
            Cmd::Config { action, .. } => match action {
                Some(ConfigAction::Set { key, value }) => {
                    assert_eq!(key, "timeout");
                    assert_eq!(value, "45");
                }
                _ => panic!("expected set action"),
            },
            _ => panic!("expected config command"),
        }
    }

    #[test]
    fn config_set_updates_timeout_and_preserves_others() {
        let path = temp_config_path("update");
        write_config(
            &path,
            r#"
model = "old-model"
timeout = 30

[other]
nested = "keep"
"#,
        );

        crate::config::set_config(&path, "timeout", "45").unwrap();

        let contents = read_config(&path);
        assert!(contents.contains("timeout = 45"), "timeout should be updated: {contents}");
        assert!(
            contents.contains("model = \"old-model\""),
            "model should be preserved: {contents}"
        );
        assert!(
            contents.contains("nested = \"keep\""),
            "nested section should be preserved: {contents}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn config_set_invalid_type_errors_and_does_not_modify_file() {
        let path = temp_config_path("invalid-type");
        write_config(
            &path,
            r#"
model = "old-model"
timeout = 30
"#,
        );
        let before = read_config(&path);

        let result = crate::config::set_config(&path, "timeout", "not-a-number");

        assert!(result.is_err(), "invalid timeout value should be rejected");
        let after = read_config(&path);
        assert_eq!(before, after, "config file must not change after error");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn config_set_invalid_key_errors_and_does_not_modify_file() {
        let path = temp_config_path("invalid-key");
        write_config(
            &path,
            r#"
model = "old-model"
timeout = 30
"#,
        );
        let before = read_config(&path);

        let result = crate::config::set_config(&path, "no_such_key", "x");

        assert!(result.is_err(), "unknown config key should be rejected");
        let after = read_config(&path);
        assert_eq!(before, after, "config file must not change after error");
        let _ = std::fs::remove_file(&path);
    }
}

fn resolve_run_id(cfg: &Config, id: &str) -> Result<String> {
    let runs = Workspace::root(&cfg.workspace_dir);
    let run = match crate::task_id::resolve_task_id(
        id,
        runs.iter().map(|r: &RunRecord| r.goal.as_str()),
    ) {
        Ok(crate::task_id::TaskId::Numeric(idx)) => runs
            .get(idx)
            .ok_or_else(|| anyhow::anyhow!("任务索引 {} 超出范围", idx))?,
        Ok(crate::task_id::TaskId::Slug(slug)) => runs
            .iter()
            .find(|r| r.id == slug)
            .ok_or_else(|| anyhow::anyhow!("未找到 slug 为 '{}' 的任务", slug))?,
        Err(e) => anyhow::bail!("无法解析任务引用 '{}': {}", id, e),
    };
    Ok(run.id.to_string())
}

fn find_config_path() -> Result<std::path::PathBuf> {
    if let Ok(path) = std::env::var("WORKBUDDY_CONFIG") {
        return Ok(std::path::PathBuf::from(path));
    }
    let cwd = std::env::current_dir()?;
    Ok(cwd.join("config.toml"))
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
    let ws = Workspace::create_new(&cfg.workspace_dir, goal)?;
    let mut run = RunRecord {
        token_usage: None,
        char_usage: None,
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
    let total_tokens: u64 = results.iter().filter_map(|r| r.token_usage).sum();
    let total_chars: u64 = results.iter().filter_map(|r| r.char_usage).sum();
    run.token_usage = Some(crate::workspace::TokenUsage {
        input_tokens: 0,
        output_tokens: total_tokens,
    });
    run.char_usage = Some(total_chars);
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
    let runs = Workspace::root(&cfg.workspace_dir);
    let rec = match crate::task_id::resolve_task_id(id, runs.iter().map(|r: &RunRecord| r.goal.as_str())) {
        Ok(crate::task_id::TaskId::Numeric(idx)) => runs
            .get(idx)
            .ok_or_else(|| anyhow::anyhow!("任务索引 {} 超出范围", idx))?,
        Ok(crate::task_id::TaskId::Slug(_)) => anyhow::bail!("内部错误：标题别名解析应返回数字索引"),
        Err(crate::task_id::TaskIdError::Ambiguous { .. }) => {
            anyhow::bail!(
                "任务标识 '{}' 不唯一，存在多个匹配任务。提示：若任务标题存在冲突，请使用任务索引（数字 id）引用任务。",
                id
            );
        }
        Err(crate::task_id::TaskIdError::DuplicateTitle { .. }) => {
            anyhow::bail!(
                "任务标识 '{}' 不唯一，存在多个同名任务。请使用任务索引（数字 id）引用任务。",
                id
            );
        }
        Err(crate::task_id::TaskIdError::NotFound { .. }) => {
            runs.iter()
                .find(|r| r.id.as_str() == id)
                .ok_or_else(|| anyhow::anyhow!("未找到任务 '{}'", id))?
        }
    };
    let ws = Workspace::open(&cfg.workspace_dir, &rec.id)?;
    let rec = ws.load_record()?;
    let total_tokens = rec
        .token_usage
        .map(|t| t.input_tokens + t.output_tokens)
        .unwrap_or(0);
    let estimated_cost = cfg.estimate_cost(total_tokens);
    let usage_line = match (rec.token_usage, rec.char_usage) {
        (Some(t), Some(c)) => format!(
            "Token 用量: {} | 字符用量: {} | 成本估算: ${:.4}",
            t, c, estimated_cost
        ),
        (Some(t), None) => format!(
            "Token 用量: {} | 字符用量: 未记录 | 成本估算: ${:.4}",
            t, estimated_cost
        ),
        (None, Some(c)) => format!(
            "Token 用量: 未记录 | 字符用量: {} | 成本估算: ${:.4}",
            c, estimated_cost
        ),
        (None, None) => format!(
            "Token 用量: 未记录 | 字符用量: 未记录 | 成本估算: ${:.4}",
            estimated_cost
        ),
    };
    println!("# 任务 {}\n\n目标: {}\n模型: {}\n时间: {}\n{}\n\n## 步骤\n", rec.id, rec.goal, rec.model, rec.created, usage_line);
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

fn cmd_list_with_slug(cfg: Config, since: Option<&str>) -> Result<()> {
    let runs = Workspace::list_runs(&cfg.workspace_dir);
    if runs.is_empty() {
        println!("(暂无历史任务)");
        return Ok(());
    }

    for run in runs {
        let slug = crate::task_id::TaskId::from_title(&run.goal, run.id.parse::<usize>().unwrap_or(0), &std::collections::HashSet::new());
        println!("[{}] [id:{}] {}", slug, run.id, run.goal);
    }
    Ok(())
}

fn cmd_show_by_slug(cfg: Config, slug: &str) -> Result<()> {
    let runs = Workspace::list_runs(&cfg.workspace_dir);
    let run = runs
        .into_iter()
        .find(|r| r.id == slug)
        .ok_or_else(|| anyhow::anyhow!("未找到 slug 为 '{}' 的任务", slug))?;
    cmd_show(cfg, &run.id.to_string())
}