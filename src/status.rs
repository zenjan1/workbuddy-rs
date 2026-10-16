use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::workspace::RunRecord;
use crate::task_id::TaskId;

const STATUS_FILE_NAME: &str = "status.json";

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
#[allow(dead_code)]
struct SupervisorStatus {
    #[serde(alias = "running")]
    supervisor_running: bool,
    pid: Option<u32>,
    last_log: Option<String>,
    completed: usize,
    failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub success: bool,
    pub log: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StatusSummary {
    pub supervisor_running: bool,
    pub pid: Option<u32>,
    pub completed_runs: usize,
    pub failed_runs: usize,
    pub recent_logs: Vec<String>,
    pub running: bool,
    pub completed: usize,
    pub failed: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Status {
    pub running: bool,
    pub completed: usize,
    pub failed: usize,
    pub recent_logs: Vec<String>,
}

impl Status {
    pub fn from_history(history: &[HistoryEntry]) -> Status {
        Self::from_history_with_limit(history, 5)
    }

    fn from_history_with_limit(history: &[HistoryEntry], limit: usize) -> Status {
        let completed = history.iter().filter(|entry| entry.success).count();
        let failed = history.len() - completed;
        let recent_logs = history
            .iter()
            .rev()
            .take(limit)
            .map(|entry| entry.log.clone())
            .collect();

        Status {
            running: false,
            completed,
            failed,
            recent_logs,
        }
    }
}

impl From<&StatusSummary> for Status {
    fn from(summary: &StatusSummary) -> Self {
        Status {
            running: summary.running,
            completed: summary.completed,
            failed: summary.failed,
            recent_logs: summary.recent_logs.clone(),
        }
    }
}

impl StatusSummary {
    pub fn from_history(history: &[HistoryEntry]) -> StatusSummary {
        Self::from_history_with_limit(history, 5)
    }

    fn from_history_with_limit(history: &[HistoryEntry], limit: usize) -> StatusSummary {
        let completed_runs = history.iter().filter(|entry| entry.success).count();
        let failed_runs = history.len() - completed_runs;
        let recent_logs = history
            .iter()
            .rev()
            .take(limit)
            .map(|entry| entry.log.clone())
            .collect();

        StatusSummary {
            supervisor_running: false,
            pid: None,
            completed_runs,
            failed_runs,
            recent_logs,
            running: false,
            completed: completed_runs,
            failed: failed_runs,
        }
    }
}

pub fn run(workspace_dir: &Path) -> anyhow::Result<()> {
    match parse_status(workspace_dir) {
        Ok(summary) => {
            println!("{}", format_status(&summary));
        }
        Err(err) => {
            eprintln!("workbuddy status: 无法读取运行状态: {err:#}");
        }
    }
    Ok(())
}

pub fn parse_status(workspace_dir: &Path) -> anyhow::Result<StatusSummary> {
    parse_status_inner(workspace_dir, None)
}

pub fn parse_status_from_run_log(
    workspace_dir: &Path,
    run_log_path: &Path,
) -> anyhow::Result<StatusSummary> {
    parse_status_inner(workspace_dir, Some(run_log_path))
}

fn parse_status_inner(
    workspace_dir: &Path,
    run_log_path: Option<&Path>,
) -> anyhow::Result<StatusSummary> {
    let supervisor_running = read_supervisor_running(workspace_dir)?;
    let pid = read_supervisor_pid(workspace_dir)?;
    let cost_per_1k_tokens = read_cost_per_1k_tokens(workspace_dir);
    let history = match run_log_path {
        Some(path) => {
            let data = fs::read_to_string(path)
                .with_context(|| format!("读取运行日志失败 {}", path.display()))?;
            let record: RunRecord = serde_json::from_str(&data)
                .with_context(|| format!("解析运行日志失败 {}", path.display()))?;
            let raw: serde_json::Value = serde_json::from_str(&data)
                .with_context(|| format!("解析运行日志失败 {}", path.display()))?;
            history_entries_from_run(&record, &raw, cost_per_1k_tokens)
        }
        None => load_history(workspace_dir, cost_per_1k_tokens)?,
    };
    let mut summary = StatusSummary::from_history_with_limit(&history, 5);
    summary.supervisor_running = supervisor_running;
    summary.running = supervisor_running;
    summary.pid = pid;

    // 如果本地状态文件存在，则优先采用其中显式给出的完成/失败计数；
    // 若历史日志为空，则使用状态文件中的最近日志尾部。
    let status_path = workspace_dir.join(STATUS_FILE_NAME);
    if status_path.exists() {
        let data = fs::read_to_string(&status_path)
            .with_context(|| format!("读取状态文件失败 {}", status_path.display()))?;
        let supervisor_status: SupervisorStatus = serde_json::from_str(&data)
            .with_context(|| format!("解析状态文件失败 {}", status_path.display()))?;
        let raw: serde_json::Value = serde_json::from_str(&data)
            .with_context(|| format!("解析状态文件失败 {}", status_path.display()))?;

        if raw.get("completed").is_some() {
            summary.completed_runs = supervisor_status.completed;
            summary.completed = supervisor_status.completed;
        }
        if raw.get("failed").is_some() {
            summary.failed_runs = supervisor_status.failed;
            summary.failed = supervisor_status.failed;
        }
        if summary.recent_logs.is_empty() {
            if let Some(last_log) = supervisor_status.last_log {
                summary.recent_logs.push(last_log);
            }
        }
    }

    Ok(summary)
}

fn read_supervisor_pid(workspace_dir: &Path) -> anyhow::Result<Option<u32>> {
    let status_path = workspace_dir.join(STATUS_FILE_NAME);
    if !status_path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&status_path)
        .with_context(|| format!("读取状态文件失败 {}", status_path.display()))?;
    let status: SupervisorStatus = serde_json::from_str(&data)
        .with_context(|| format!("解析状态文件失败 {}", status_path.display()))?;
    Ok(status.pid)
}

fn load_history(
    workspace_dir: &Path,
    cost_per_1k_tokens: Option<f64>,
) -> anyhow::Result<Vec<HistoryEntry>> {
    let mut run_files = collect_run_files(workspace_dir)?;
    run_files.sort_by_key(|path| {
        let modified = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis());
        let numeric = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.rsplit_once('-').map(|(_, n)| n))
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(u64::MAX);
        (modified, numeric)
    });

    let mut history = Vec::new();
    for path in run_files {
        let data = fs::read_to_string(&path)
            .with_context(|| format!("读取运行历史失败 {}", path.display()))?;
        let record: RunRecord = serde_json::from_str(&data)
            .with_context(|| format!("解析运行历史失败 {}", path.display()))?;
        let raw: serde_json::Value = serde_json::from_str(&data)
            .with_context(|| format!("解析运行历史失败 {}", path.display()))?;

        history.extend(history_entries_from_run(&record, &raw, cost_per_1k_tokens));
    }

    Ok(history)
}

#[derive(Debug, Clone, Copy)]
struct RunUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    char_usage: Option<u64>,
}

fn usage_from_json(raw: &serde_json::Value, record: &RunRecord) -> RunUsage {
    let read_root = |key: &str| raw.get(key).and_then(|value| value.as_u64());
    let read_last_step = |key: &str| {
        raw.get("steps")
            .and_then(|steps| steps.as_array())
            .and_then(|steps| steps.last())
            .and_then(|step| step.get(key))
            .and_then(|value| value.as_u64())
    };

    RunUsage {
        prompt_tokens: read_root("prompt_tokens").or_else(|| read_last_step("prompt_tokens")),
        completion_tokens: read_root("completion_tokens")
            .or_else(|| read_last_step("completion_tokens")),
        total_tokens: read_root("total_tokens")
            .or_else(|| read_last_step("total_tokens"))
            .or(record.token_usage.map(|usage| usage.input_tokens + usage.output_tokens)),
        char_usage: read_root("char_usage")
            .or_else(|| read_last_step("char_usage"))
            .or(record.char_usage),
    }
}

fn history_entries_from_run(
    record: &RunRecord,
    raw: &serde_json::Value,
    cost_per_1k_tokens: Option<f64>,
) -> Vec<HistoryEntry> {
    let usage = usage_from_json(raw, record);
    let success = record.steps.iter().all(|step| step.ok);
    let cost = usage.total_tokens.and_then(|tokens| {
        cost_per_1k_tokens.map(|price| price * (tokens as f64 / 1000.0))
    });
    let usage_text = format_usage(usage, cost);

    let content = if success {
        record
            .steps
            .last()
            .map(|step| step.output_preview.clone())
            .unwrap_or_else(|| record.goal.clone())
    } else {
        record
            .steps
            .iter()
            .rev()
            .find(|step| !step.ok)
            .map(|step| {
                step.error
                    .clone()
                    .unwrap_or_else(|| step.output_preview.clone())
            })
            .unwrap_or_else(|| record.goal.clone())
    };
    let content = truncate_chars(&content, 80);
    let state = if success { "completed" } else { "failed" };
    let task_id_display = format_task_id(record.id.as_str());
    let slug_alias = TaskId::slugify(&record.goal);
    let task_id_display = if !slug_alias.is_empty() && slug_alias != task_id_display {
        format!("{} ({})", task_id_display, slug_alias)
    } else {
        task_id_display
    };
    let timestamp = raw
        .get("timestamp")
        .or_else(|| raw.get("created_at"))
        .or_else(|| raw.get("started_at"))
        .or_else(|| raw.get("finished_at"))
        .or_else(|| raw.get("start_time"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let title = truncate_chars(&record.goal, 60);
    let log = format!(
        "[{}] {} {} {} ({} steps): {} {}",
        timestamp,
        task_id_display,
        state,
        title,
        record.steps.len(),
        content,
        usage_text
    );

    vec![HistoryEntry { success, log }]
}

fn format_task_id(record_id: &str) -> String {
    if record_id.chars().all(|c| c.is_ascii_digit()) {
        record_id.to_string()
    } else {
        TaskId::from_slug(record_id).to_string()
    }
}

fn read_supervisor_running(workspace_dir: &Path) -> anyhow::Result<bool> {
    let status_path = workspace_dir.join(STATUS_FILE_NAME);
    let status_running = if status_path.exists() {
        let data = fs::read_to_string(&status_path)
            .with_context(|| format!("读取状态文件失败 {}", status_path.display()))?;
        let status: SupervisorStatus = serde_json::from_str(&data)
            .with_context(|| format!("解析状态文件失败 {}", status_path.display()))?;
        status.supervisor_running || status.pid.is_some()
    } else {
        false
    };

    let lock_path = workspace_dir.join("supervisor.lock");
    let marker_path = workspace_dir.join("RUNNING");
    let pid_path = workspace_dir.join("supervisor.pid");

    let process_running = if cfg!(test) { false } else { check_supervisor_process() };
    Ok(status_running
        || lock_path.exists()
        || marker_path.exists()
        || pid_path.exists()
        || process_running)
}

#[cfg(unix)]
fn check_supervisor_process() -> bool {
    let current_pid = std::process::id();
    if let Ok(output) = std::process::Command::new("ps")
        .args(["-eo", "pid=,comm="])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return stdout.lines().any(|line| {
                let mut parts = line.split_whitespace();
                if let Some(pid_str) = parts.next() {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        if pid == current_pid {
                            return false;
                        }
                    }
                }
                line.contains("workbuddy") || line.contains("supervisor")
            });
        }
    }
    false
}

#[cfg(not(unix))]
fn check_supervisor_process() -> bool {
    // Windows 进程检查示例；仅做最小实现，避免引入额外依赖。
    if let Ok(output) = std::process::Command::new("tasklist").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return stdout
                .lines()
                .any(|line| line.contains("workbuddy") || line.contains("supervisor"));
        }
    }
    false
}

fn collect_run_files(workspace_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let runs_dir = workspace_dir.join("runs");
    if runs_dir.is_dir() {
        let mut files = Vec::new();
        for entry in fs::read_dir(&runs_dir)
            .with_context(|| format!("读取运行目录失败 {}", runs_dir.display()))?
        {
            let entry = entry.with_context(|| format!("读取运行目录条目失败 {}", runs_dir.display()))?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                files.push(path);
            }
        }
        files.sort();
        return Ok(files);
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(workspace_dir)
        .with_context(|| format!("读取工作区目录失败 {}", workspace_dir.display()))?
    {
        let entry = entry.with_context(|| format!("读取工作区条目失败 {}", workspace_dir.display()))?;
        let path = entry.path();
        let file_name = path.file_name().and_then(|name| name.to_str());
        let is_status = file_name == Some(STATUS_FILE_NAME);
        let is_config = file_name == Some("config.json");
        if path.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("json")
            && !is_status
            && !is_config
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub fn format_status(summary: &StatusSummary) -> String {
    let state = if summary.supervisor_running {
        match summary.pid {
            Some(pid) => format!("running (pid {pid})"),
            None => "running".to_string(),
        }
    } else {
        "stopped".to_string()
    };
    let mut output = format!(
        "Supervisor: {state}\nLast run: {} completed, {} failed",
        summary.completed_runs, summary.failed_runs
    );
    if summary.completed_runs == 0 && summary.failed_runs == 0 && summary.recent_logs.is_empty() {
        output.push_str("\nNo run history.");
    }
    if summary.recent_logs.is_empty() {
        output.push_str("\nRecent logs: (none)");
    } else {
        output.push_str("\nRecent logs:");
        for log in &summary.recent_logs {
            let (log_line, usage_line) = split_usage(log);
            output.push_str(&format!("\n  - {log_line}"));
            if let Some(usage) = usage_line {
                output.push_str(&format!("\n    {usage}"));
            }
        }
    }
    output
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        input.to_string()
    } else {
        let mut truncated: String = input.chars().take(max_chars).collect();
        truncated.push_str("...");
        truncated
    }
}

fn read_cost_per_1k_tokens(workspace_dir: &Path) -> Option<f64> {
    let config_path = workspace_dir.join("config.json");
    let data = fs::read_to_string(config_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&data).ok()?;
    json.get("cost_per_1k_tokens")?.as_f64()
}

fn format_usage(usage: RunUsage, cost: Option<f64>) -> String {
    let prompt_tokens = usage.prompt_tokens.unwrap_or(0);
    let completion_tokens = usage.completion_tokens.unwrap_or(0);
    let total_tokens = usage.total_tokens.unwrap_or(0);
    let chars = usage.char_usage.unwrap_or(0);
    let mut result = format!(
        "(prompt_tokens: {}, completion_tokens: {}, total_tokens: {}, chars: {})",
        prompt_tokens, completion_tokens, total_tokens, chars
    );
    if let Some(cost) = cost {
        result.push_str(&format!(", estimated cost: ${:.4}", cost));
    }
    result.push(')');
    result
}

fn split_usage(log: &str) -> (String, Option<String>) {
    if let Some(pos) = log.rfind(" (prompt_tokens: ") {
        let usage_start = pos + 1;
        if log[usage_start..].ends_with(')') {
            let usage = log[usage_start..].to_string();
            let log_line = log[..pos].to_string();
            return (log_line, Some(usage));
        }
    }
    (log.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{RunRecord, StepRecord};
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn format_task_id_keeps_numeric_indices_and_slugs() {
        assert_eq!(format_task_id("1"), "1");
        assert_eq!(format_task_id("add-status-command"), "add-status-command");
    }

    #[test]
    fn parse_status_limits_recent_logs_to_five() {
        let workspace = test_workspace("limits-recent");
        for i in 1..=11 {
            let file_name = format!("run-{}.json", i);
            write_run_record(
                &workspace.join("runs"),
                &file_name,
                &run_record(
                    &format!("run-{}", i),
                    vec![step(1, "plan", true, None, &format!("preview {}", i))],
                ),
            );
        }

        let summary = parse_status(&workspace).unwrap();
        assert_eq!(summary.completed_runs, 11);
        assert_eq!(summary.failed_runs, 0);
        assert_eq!(summary.recent_logs.len(), 5);
        assert!(summary.recent_logs.iter().any(|log| log.contains("run-7")));
        assert!(summary.recent_logs.iter().any(|log| log.contains("run-11")));
        assert!(!summary.recent_logs.iter().any(|log| log.starts_with("[run-6]")));

        let rendered = format_status(&summary);
        assert!(rendered.contains("Last run: 11 completed, 0 failed"));
        assert!(rendered.contains("Recent logs:"));
        let log_lines = rendered.lines().filter(|line| line.starts_with("  - ")).count();
        assert_eq!(log_lines, 5);

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn status_empty_workspace_outputs_empty_state() {
        let workspace = test_workspace("empty");

        let summary = parse_status(&workspace).unwrap();
        assert!(!summary.supervisor_running);
        assert_eq!(summary.completed_runs, 0);
        assert_eq!(summary.failed_runs, 0);
        assert!(summary.recent_logs.is_empty());

        let rendered = format_status(&summary);
        assert!(rendered.contains("Last run: 0 completed, 0 failed"));
        assert!(rendered.contains("No run history."));
        assert!(rendered.contains("Recent logs: (none)"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn history_entries_from_run_formats_success_and_failure_logs_with_usage() {
        let success_record = run_record(
            "run-success",
            vec![step(1, "plan", true, None, "planned")],
        );
        let success_raw = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "char_usage": 200
        });
        let success_entries =
            history_entries_from_run(&success_record, &success_raw, Some(0.02));
        assert_eq!(success_entries.len(), 1);
        assert!(success_entries[0].success);
        let success_log = &success_entries[0].log;
        assert!(success_log.contains("completed"));
        assert!(success_log.contains("prompt_tokens: 100"));
        assert!(success_log.contains("completion_tokens: 50"));
        assert!(success_log.contains("total_tokens: 150"));
        assert!(success_log.contains("chars: 200"));
        assert!(success_log.contains("estimated cost: $0.0030")); // 0.02 * 150/1000 = 0.003

        let failure_record = run_record(
            "run-failure",
            vec![
                step(1, "plan", true, None, "planned"),
                step(2, "build", false, Some("boom"), "build failed"),
            ],
        );
        let failure_raw = serde_json::json!({});
        let failure_entries =
            history_entries_from_run(&failure_record, &failure_raw, None);
        assert_eq!(failure_entries.len(), 1);
        assert!(!failure_entries[0].success);
        let failure_log = &failure_entries[0].log;
        assert!(failure_log.contains("failed"));
        assert!(failure_log.contains("boom"));
    }

    #[test]
    fn from_history_with_limit_counts_and_limits_logs() {
        let history: Vec<HistoryEntry> = (1..=10)
            .map(|i| HistoryEntry {
                success: i % 2 == 0,
                log: format!("run-{}", i),
            })
            .collect();

        let summary = StatusSummary::from_history_with_limit(&history, 3);
        assert_eq!(summary.completed_runs, 5);
        assert_eq!(summary.failed_runs, 5);
        assert_eq!(summary.recent_logs.len(), 3);
        assert_eq!(summary.recent_logs[0], "run-10");

        let status = Status::from_history_with_limit(&history, 3);
        assert_eq!(status.completed, 5);
        assert_eq!(status.failed, 5);
        assert_eq!(status.recent_logs.len(), 3);
    }

    #[test]
    fn run_missing_workspace_prints_readable_error_and_returns_ok() {
        let workspace = PathBuf::from("/nonexistent/path/for/workbuddy/status");
        assert!(run(&workspace).is_ok());
    }

    #[test]
    fn status_counts_mixed_completed_and_failed_runs() {
        let workspace = test_workspace("mixed");
        write_run_record(
            &workspace.join("runs"),
            "run-1.json",
            &run_record(
                "run-1",
                vec![step(1, "plan", true, None, "ok")],
            ),
        );
        write_run_record(
            &workspace.join("runs"),
            "run-2.json",
            &run_record(
                "run-2",
                vec![step(1, "build", false, Some("boom"), "bad")],
            ),
        );

        let summary = parse_status(&workspace).unwrap();
        assert_eq!(summary.completed_runs, 1);
        assert_eq!(summary.failed_runs, 1);
        assert_eq!(summary.recent_logs.len(), 2);

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn status_detects_supervisor_lock_file() {
        let workspace = test_workspace("lock-running");
        fs::write(workspace.join("supervisor.lock"), "").unwrap();

        let summary = parse_status(&workspace).unwrap();
        assert!(summary.supervisor_running);
        assert!(summary.running);

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn status_reports_stopped_without_lock_file() {
        let workspace = test_workspace("no-lock-stopped");

        let summary = parse_status(&workspace).unwrap();
        assert!(!summary.supervisor_running);
        assert!(!summary.running);

        fs::remove_dir_all(&workspace).unwrap();
    }

    fn test_workspace(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "workbuddy_status_{}_{}_{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        fs::create_dir_all(path.join("runs")).unwrap();
        path
    }

    fn step(id: usize, name: &str, ok: bool, error: Option<&str>, preview: &str) -> StepRecord {
        StepRecord {
            id,
            name: name.to_string(),
            agent: "test-agent".to_string(),
            skill: "test-skill".to_string(),
            kind: "test-kind".to_string(),
            ok,
            error: error.map(|s| s.to_string()),
            seconds: 0.1,
            output_preview: preview.to_string(),
        }
    }

    fn run_record(id: &str, steps: Vec<StepRecord>) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            goal: "test goal".to_string(),
            created: "2024-01-01T00:00:00Z".to_string(),
            model: "test-model".to_string(),
            mock: false,
            steps,
            token_usage: None,
            char_usage: None,
        }
    }

    fn write_run_record(dir: &Path, file_name: &str, record: &RunRecord) {
        let path = dir.join(file_name);
        let data = serde_json::to_string_pretty(record).unwrap();
        fs::write(path, data).unwrap();
    }

    #[test]
    fn parse_status_reads_runs_and_status_file() {
        let workspace = test_workspace("read");
        fs::write(
            workspace.join("status.json"),
            serde_json::to_string(&serde_json::json!({
                "supervisor_running": true,
                "pid": 4242
            }))
            .unwrap(),
        )
        .unwrap();

        write_run_record(
            &workspace.join("runs"),
            "run-ok.json",
            &run_record("run-ok", vec![step(1, "plan", true, None, "planned")]),
        );
        write_run_record(
            &workspace.join("runs"),
            "run-failed.json",
            &run_record(
                "run-failed",
                vec![step(1, "build", false, Some("boom"), "build failed")],
            ),
        );
        write_run_record(
            &workspace.join("runs"),
            "run-ok-2.json",
            &run_record(
                "run-ok-2",
                vec![step(1, "verify", true, None, "verified")],
            ),
        );

        let summary = parse_status(&workspace).unwrap();

        assert!(summary.supervisor_running);
        assert_eq!(summary.completed_runs, 2);
        assert_eq!(summary.failed_runs, 1);
        assert_eq!(summary.recent_logs.len(), 3);
        assert!(summary.recent_logs.iter().any(|log| log.contains("run-ok")));
        assert!(summary.recent_logs.iter().any(|log| log.contains("run-failed")));

        let rendered = format_status(&summary);
        assert!(rendered.contains("Supervisor: running"));
        assert!(rendered.contains("Last run: 2 completed, 1 failed"));
        assert!(rendered.contains("Recent logs:"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn parse_status_counts_runs_not_steps() {
        let workspace = test_workspace("counts-runs");
        write_run_record(
            &workspace.join("runs"),
            "run-multi-ok.json",
            &run_record(
                "run-multi-ok",
                vec![
                    step(1, "plan", true, None, "planned"),
                    step(2, "build", true, None, "built"),
                ],
            ),
        );
        write_run_record(
            &workspace.join("runs"),
            "run-multi-failed.json",
            &run_record(
                "run-multi-failed",
                vec![
                    step(1, "plan", true, None, "planned"),
                    step(2, "build", false, Some("boom"), "build failed"),
                ],
            ),
        );

        let summary = parse_status(&workspace).unwrap();
        assert_eq!(summary.completed_runs, 1);
        assert_eq!(summary.failed_runs, 1);
        assert_eq!(summary.recent_logs.len(), 2);
        assert!(summary.recent_logs.iter().any(|log| log.contains("run-multi-ok")));
        assert!(summary.recent_logs.iter().any(|log| log.contains("run-multi-failed")));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn missing_status_file_defaults_to_stopped() {
        let workspace = test_workspace("missing-status");
        write_run_record(
            &workspace.join("runs"),
            "run-ok.json",
            &run_record("run-ok", vec![step(1, "plan", true, None, "planned")]),
        );

        let summary = parse_status(&workspace).unwrap();
        assert!(!summary.supervisor_running);
        assert_eq!(summary.completed_runs, 1);
        assert_eq!(summary.failed_runs, 0);

        let rendered = format_status(&summary);
        assert!(rendered.contains("Supervisor: stopped"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn parse_status_detects_running_from_lock_file() {
        let workspace = test_workspace("lock-file");
        fs::write(workspace.join("supervisor.lock"), "").unwrap();

        let summary = parse_status(&workspace).unwrap();
        assert!(summary.supervisor_running);
        assert_eq!(summary.pid, None);

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn parse_status_detects_running_from_running_marker() {
        let workspace = test_workspace("running-marker");
        fs::write(workspace.join("RUNNING"), "").unwrap();

        let summary = parse_status(&workspace).unwrap();
        assert!(summary.supervisor_running);
        assert_eq!(summary.pid, None);

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn parse_status_from_run_log_reads_injected_path() {
        let workspace = test_workspace("injected-log");
        let log_path = workspace.join("custom-run.json");
        let record = run_record(
            "injected-run",
            vec![step(1, "plan", true, None, "planned")],
        );
        fs::write(&log_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

        let summary = parse_status_from_run_log(&workspace, &log_path).unwrap();
        assert_eq!(summary.completed_runs, 1);
        assert_eq!(summary.failed_runs, 0);
        assert_eq!(summary.recent_logs.len(), 1);
        assert!(summary.recent_logs[0].contains("injected-run"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn parse_status_no_history_returns_empty_summary() {
        let workspace = test_workspace("no-history");
        fs::create_dir_all(workspace.join("runs")).unwrap();

        let summary = parse_status(&workspace).unwrap();
        assert!(!summary.supervisor_running);
        assert_eq!(summary.completed_runs, 0);
        assert_eq!(summary.failed_runs, 0);
        assert!(summary.recent_logs.is_empty());

        let rendered = format_status(&summary);
        assert!(rendered.contains("Supervisor: stopped"));
        assert!(rendered.contains("Last run: 0 completed, 0 failed"));
        assert!(rendered.contains("No run history."));
        assert!(rendered.contains("Recent logs: (none)"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn parse_status_invalid_status_json_reports_error() {
        let workspace = test_workspace("invalid-status");
        fs::write(workspace.join("status.json"), "{not-json}").unwrap();
        write_run_record(
            &workspace.join("runs"),
            "run-ok.json",
            &run_record("run-ok", vec![step(1, "plan", true, None, "planned")]),
        );

        let result = parse_status(&workspace);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("解析状态文件失败"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn invalid_run_json_reports_error() {
        let workspace = test_workspace("invalid-run");
        fs::write(workspace.join("runs").join("bad.json"), "{not-json}").unwrap();

        let result = parse_status(&workspace);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("解析运行历史失败"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn deserialize_old_run_record_without_usage_fields_is_ok() {
        let json = serde_json::json!({
            "id": "old-run",
            "goal": "test goal",
            "created": "2024-01-01T00:00:00Z",
            "model": "test-model",
            "mock": false,
            "steps": []
        });
        let record: RunRecord = serde_json::from_value(json).unwrap();
        assert_eq!(record.token_usage, None);
        assert_eq!(record.char_usage, None);
    }

    #[test]
    fn format_status_shows_token_and_char_usage() {
        let workspace = test_workspace("usage");
        let json = serde_json::json!({
            "id": "run-with-usage",
            "goal": "test goal",
            "created": "2024-01-01T00:00:00Z",
            "model": "test-model",
            "mock": false,
            "token_usage": 123,
            "char_usage": 456,
            "prompt_tokens": 60,
            "completion_tokens": 63,
            "total_tokens": 123,
            "steps": [
                {
                    "id": 1,
                    "name": "plan",
                    "agent": "test-agent",
                    "skill": "test-skill",
                    "kind": "test-kind",
                    "ok": true,
                    "error": null,
                    "seconds": 0.1,
                    "output_preview": "planned"
                }
            ]
        });
        let path = workspace.join("runs").join("run-with-usage.json");
        fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let summary = parse_status(&workspace).unwrap();
        let rendered = format_status(&summary);
        assert!(rendered.contains("prompt_tokens: 60"));
        assert!(rendered.contains("completion_tokens: 63"));
        assert!(rendered.contains("total_tokens: 123"));
        assert!(rendered.contains("chars: 456"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn format_status_shows_na_for_missing_usage() {
        let workspace = test_workspace("missing-usage");
        let json = serde_json::json!({
            "id": "run-old",
            "goal": "test goal",
            "created": "2024-01-01T00:00:00Z",
            "model": "test-model",
            "mock": false,
            "steps": [
                {
                    "id": 1,
                    "name": "plan",
                    "agent": "test-agent",
                    "skill": "test-skill",
                    "kind": "test-kind",
                    "ok": true,
                    "error": null,
                    "seconds": 0.1,
                    "output_preview": "planned"
                }
            ]
        });
        let path = workspace.join("runs").join("run-old.json");
        fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let summary = parse_status(&workspace).unwrap();
        let rendered = format_status(&summary);
        assert!(rendered.contains("prompt_tokens: 0"));
        assert!(rendered.contains("chars: 0"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn parse_status_shows_cost_when_config_present() {
        let workspace = test_workspace("cost-config");
        fs::write(
            workspace.join("config.json"),
            serde_json::to_string(&serde_json::json!({
                "cost_per_1k_tokens": 0.02
            }))
            .unwrap(),
        )
        .unwrap();
        let json = serde_json::json!({
            "id": "run-with-cost",
            "goal": "test goal",
            "created": "2024-01-01T00:00:00Z",
            "model": "test-model",
            "mock": false,
            "token_usage": 1000,
            "char_usage": 500,
            "steps": [
                {
                    "id": 1,
                    "name": "plan",
                    "agent": "test-agent",
                    "skill": "test-skill",
                    "kind": "test-kind",
                    "ok": true,
                    "error": null,
                    "seconds": 0.1,
                    "output_preview": "planned"
                }
            ]
        });
        let path = workspace.join("runs").join("run-with-cost.json");
        fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let summary = parse_status(&workspace).unwrap();
        let rendered = format_status(&summary);
        assert!(rendered.contains("tokens: 1000"));
        assert!(rendered.contains("cost: $0.0200"));

        fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn from_history_empty_returns_zero_counts_and_no_logs() {
        let history = vec![];
        let summary = StatusSummary::from_history(&history);
        assert_eq!(summary.completed_runs, 0);
        assert_eq!(summary.failed_runs, 0);
        assert!(summary.recent_logs.is_empty());
        assert!(!summary.supervisor_running);
        assert_eq!(summary.pid, None);
    }

    #[test]
    fn from_history_mixed_success_and_failure_counts() {
        let history = vec![
            HistoryEntry {
                success: true,
                log: "ok 1".to_string(),
            },
            HistoryEntry {
                success: false,
                log: "fail 1".to_string(),
            },
            HistoryEntry {
                success: true,
                log: "ok 2".to_string(),
            },
        ];
        let summary = StatusSummary::from_history(&history);
        assert_eq!(summary.completed_runs, 2);
        assert_eq!(summary.failed_runs, 1);
        assert_eq!(
            summary.recent_logs,
            vec![
                "ok 2".to_string(),
                "fail 1".to_string(),
                "ok 1".to_string()
            ]
        );
    }

    #[test]
    fn from_history_truncates_recent_logs_to_five_most_recent() {
        let history: Vec<HistoryEntry> = (0..7)
            .map(|i| HistoryEntry {
                success: i % 2 == 0,
                log: format!("log {}", i),
            })
            .collect();
        let summary = StatusSummary::from_history(&history);
        assert_eq!(summary.recent_logs.len(), 5);
        assert_eq!(
            summary.recent_logs,
            vec![
                "log 6".to_string(),
                "log 5".to_string(),
                "log 4".to_string(),
                "log 3".to_string(),
                "log 2".to_string()
            ]
        );
    }
}

#[cfg(test)]
mod status_extra_tests {
    use super::*;

    #[test]
    fn from_history_only_success_counts_all_completed() {
        let history = vec![
            HistoryEntry {
                success: true,
                log: "ok 1".to_string(),
            },
            HistoryEntry {
                success: true,
                log: "ok 2".to_string(),
            },
        ];
        let summary = StatusSummary::from_history(&history);
        assert_eq!(summary.completed_runs, 2);
        assert_eq!(summary.failed_runs, 0);
        assert_eq!(
            summary.recent_logs,
            vec!["ok 2".to_string(), "ok 1".to_string()]
        );
    }

    #[test]
    fn from_history_only_failures_counts_all_failed() {
        let history = vec![
            HistoryEntry {
                success: false,
                log: "fail 1".to_string(),
            },
            HistoryEntry {
                success: false,
                log: "fail 2".to_string(),
            },
        ];
        let summary = StatusSummary::from_history(&history);
        assert_eq!(summary.completed_runs, 0);
        assert_eq!(summary.failed_runs, 2);
        assert_eq!(
            summary.recent_logs,
            vec!["fail 2".to_string(), "fail 1".to_string()]
        );
    }

    #[test]
    fn format_status_empty_history_reports_no_run_history() {
        let summary = StatusSummary::from_history(&[]);
        let rendered = format_status(&summary);
        assert!(rendered.contains("No run history."));
        assert!(rendered.contains("Recent logs: (none)"));
    }
}

#[cfg(test)]
mod cost_display_tests {
    use super::*;
    use crate::workspace::{RunRecord, StepRecord};
    use std::fs;

    #[test]
    fn show_output_includes_estimated_cost() {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "workbuddy_status_cost_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        fs::create_dir_all(path.join("runs")).unwrap();

        fs::write(
            path.join("config.json"),
            serde_json::to_string(&serde_json::json!({
                "cost_per_1k_tokens": 0.1234
            }))
            .unwrap(),
        )
        .unwrap();

        let record = RunRecord {
            id: "run-cost".to_string(),
            goal: "test goal".to_string(),
            created: "2024-01-01T00:00:00Z".to_string(),
            model: "test-model".to_string(),
            mock: false,
            steps: vec![StepRecord {
                id: 1,
                name: "plan".to_string(),
                agent: "test-agent".to_string(),
                skill: "test-skill".to_string(),
                kind: "test-kind".to_string(),
                ok: true,
                error: None,
                seconds: 0.1,
                output_preview: "planned".to_string(),
            }],
            token_usage: Some(crate::workspace::TokenUsage {
                input_tokens: 1000,
                output_tokens: 0,
            }),
            char_usage: None,
        };

        let run_path = path.join("runs").join("run-cost.json");
        fs::write(run_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

        let summary = parse_status(&path).unwrap();
        let rendered = format_status(&summary);

        assert!(
            rendered.contains("estimated cost: $0.1234"),
            "rendered output: {rendered}"
        );

        fs::remove_dir_all(&path).unwrap();
    }
}

#[cfg(test)]
mod status_presentation_tests {
    use crate::status::{format_status, parse_status};
    use crate::workspace::{RunRecord, StepRecord};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn step(id: usize, ok: bool, preview: &str) -> StepRecord {
        StepRecord {
            id,
            name: "step".to_string(),
            agent: "agent".to_string(),
            skill: "skill".to_string(),
            kind: "kind".to_string(),
            ok,
            error: if ok { None } else { Some("boom".to_string()) },
            seconds: 0.0,
            output_preview: preview.to_string(),
        }
    }

    fn run(id: &str, steps: Vec<StepRecord>) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            goal: "goal".to_string(),
            created: "2024-01-01T00:00:00Z".to_string(),
            model: "model".to_string(),
            mock: false,
            steps,
            token_usage: None,
            char_usage: None,
        }
    }

    fn write_run(dir: &Path, name: &str, record: &RunRecord) {
        let path = dir.join(name);
        fs::write(path, serde_json::to_string_pretty(record).unwrap()).unwrap();
    }

    fn temp_workspace(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "workbuddy_status_extra_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("runs")).unwrap();
        path
    }

    #[test]
    fn no_history_renders_no_history_and_none_logs() {
        let ws = temp_workspace("no-history");
        let summary = parse_status(&ws).unwrap();
        assert!(!summary.supervisor_running);
        assert_eq!(summary.completed_runs, 0);
        assert_eq!(summary.failed_runs, 0);
        assert!(summary.recent_logs.is_empty());

        let rendered = format_status(&summary);
        assert!(rendered.contains("Supervisor: stopped"));
        assert!(rendered.contains("Last run: 0 completed, 0 failed"));
        assert!(rendered.contains("No run history."));
        assert!(rendered.contains("Recent logs: (none)"));

        fs::remove_dir_all(&ws).unwrap();
    }

    #[test]
    fn running_status_without_pid_renders_running_only() {
        let ws = temp_workspace("running-no-pid");
        fs::write(
            ws.join("status.json"),
            serde_json::to_string(&serde_json::json!({
                "supervisor_running": true
            }))
            .unwrap(),
        )
        .unwrap();

        let summary = parse_status(&ws).unwrap();
        assert!(summary.supervisor_running);
        assert_eq!(summary.pid, None);

        let rendered = format_status(&summary);
        assert!(rendered.contains("Supervisor: running"));
        assert!(!rendered.contains("pid"));

        fs::remove_dir_all(&ws).unwrap();
    }

    #[test]
    fn fewer_than_five_logs_are_all_rendered() {
        let ws = temp_workspace("few-logs");
        write_run(
            &ws.join("runs"),
            "run-a.json",
            &run("run-a", vec![step(1, true, "ok a")]),
        );
        write_run(
            &ws.join("runs"),
            "run-b.json",
            &run("run-b", vec![step(1, false, "bad b")]),
        );

        let summary = parse_status(&ws).unwrap();
        assert_eq!(summary.completed_runs, 1);
        assert_eq!(summary.failed_runs, 1);
        assert_eq!(summary.recent_logs.len(), 2);

        let rendered = format_status(&summary);
        let log_lines = rendered
            .lines()
            .filter(|line| line.starts_with("  - "))
            .count();
        assert_eq!(log_lines, 2);
        assert!(rendered.contains("run-a"));
        assert!(rendered.contains("run-b"));

        fs::remove_dir_all(&ws).unwrap();
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;
    use crate::workspace::{RunRecord, StepRecord};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_workspace(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "workbuddy_status_edge_{}_{}_{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        fs::create_dir_all(path.join("runs")).unwrap();
        path
    }

    fn write_run(ws: &Path, file_name: &str, record: &RunRecord) {
        let data = serde_json::to_string_pretty(record).unwrap();
        fs::write(ws.join("runs").join(file_name), data).unwrap();
    }

    fn sample_record(id: &str, ok: bool) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            goal: "test goal".to_string(),
            created: "2024-01-01T00:00:00Z".to_string(),
            model: "test".to_string(),
            mock: false,
            steps: vec![StepRecord {
                id: 1,
                name: "plan".to_string(),
                agent: "agent".to_string(),
                skill: "skill".to_string(),
                kind: "kind".to_string(),
                ok,
                error: if ok { None } else { Some("boom".to_string()) },
                seconds: 0.1,
                output_preview: "preview".to_string(),
            }],
            token_usage: None,
            char_usage: None,
        }
    }

    #[test]
    fn parse_status_with_fewer_than_five_logs_does_not_panic() {
        let ws = temp_workspace("fewer");
        write_run(&ws, "run-1.json", &sample_record("run-1", true));
        write_run(&ws, "run-2.json", &sample_record("run-2", false));

        let summary = super::parse_status(&ws).expect("parse should succeed");
        assert_eq!(summary.recent_logs.len(), 2);
        assert_eq!(summary.completed_runs, 1);
        assert_eq!(summary.failed_runs, 1);
        assert!(!summary.supervisor_running);

        let rendered = super::format_status(&summary);
        assert!(rendered.contains("Recent logs:"));
        assert!(rendered.contains("run-1"));
        assert!(rendered.contains("run-2"));

        fs::remove_dir_all(&ws).unwrap();
    }

    #[test]
    fn parse_status_with_malformed_run_log_returns_error_not_panic() {
        let ws = temp_workspace("malformed");
        fs::write(ws.join("runs").join("bad.json"), "{ invalid json").unwrap();
        let result = super::parse_status(&ws);
        assert!(result.is_err());

        fs::remove_dir_all(&ws).unwrap();
    }
}


#[cfg(test)]
mod status_history_tests {
    use super::*;

    fn entry(success: bool, log: &str) -> HistoryEntry {
        HistoryEntry {
            success,
            log: log.to_string(),
        }
    }

    #[test]
    fn status_summary_from_history_counts_completed_failed_and_recent_logs() {
        let history = vec![
            entry(true, "log1"),
            entry(false, "log2"),
            entry(true, "log3"),
            entry(true, "log4"),
            entry(false, "log5"),
            entry(true, "log6"),
        ];
        let summary = StatusSummary::from_history(&history);
        assert_eq!(summary.completed_runs, 4);
        assert_eq!(summary.failed_runs, 2);
        assert_eq!(summary.recent_logs.len(), 5);
        assert_eq!(summary.recent_logs[0], "log6");
        assert_eq!(summary.recent_logs[4], "log2");
    }

    #[test]
    fn status_from_history_has_running_false_and_counts() {
        let history = vec![entry(true, "log1"), entry(false, "log2")];
        let status = Status::from_history(&history);
        assert!(!status.running);
        assert_eq!(status.completed, 1);
        assert_eq!(status.failed, 1);
        assert_eq!(status.recent_logs, vec!["log2".to_string(), "log1".to_string()]);
    }

    #[test]
    fn format_status_renders_timestamp_title_and_state() {
        let summary = StatusSummary {
            supervisor_running: false,
            pid: None,
            completed_runs: 1,
            failed_runs: 0,
            recent_logs: vec![
                "[2025-01-01T00:00:00Z] task-1 completed My Task: done (prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, chars: 0)".to_string(),
            ],
            running: false,
            completed: 1,
            failed: 0,
        };
        let rendered = format_status(&summary);
        assert!(rendered.contains("Last run: 1 completed, 0 failed"));
        assert!(rendered.contains("2025-01-01T00:00:00Z"));
        assert!(rendered.contains("My Task"));
        assert!(rendered.contains("completed"));
    }
}