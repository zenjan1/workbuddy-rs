use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::workspace::RunRecord;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct SupervisorStatus {
    #[serde(alias = "running")]
    supervisor_running: bool,
    pid: Option<u32>,
    last_log: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StatusSummary {
    pub supervisor_running: bool,
    pub pid: Option<u32>,
    pub completed_runs: usize,
    pub failed_runs: usize,
    pub recent_logs: Vec<String>,
}

pub fn run(workspace_dir: &Path) -> Result<(), Box<dyn Error>> {
    let status_path = workspace_dir.join("status.json");
    if !status_path.exists() {
        println!(
            "No status file found; supervisor is not running or has no recent runs."
        );
        return Ok(());
    }

    let summary = parse_status(workspace_dir)?;
    println!("{}", format_status(&summary));
    Ok(())
}

pub fn parse_status(workspace_dir: &Path) -> anyhow::Result<StatusSummary> {
    let status_path = workspace_dir.join("status.json");
    let supervisor_running = read_supervisor_running(&status_path)?;
    let pid = if status_path.exists() {
        let data = fs::read_to_string(&status_path)
            .with_context(|| format!("读取状态文件失败 {}", status_path.display()))?;
        let status: SupervisorStatus = serde_json::from_str(&data)
            .with_context(|| format!("解析状态文件失败 {}", status_path.display()))?;
        status.pid
    } else {
        None
    };

    let cost_per_1k_tokens = read_cost_per_1k_tokens(workspace_dir);

    let mut run_files = collect_run_files(workspace_dir)?;
    run_files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
    });

    let mut completed_runs = 0;
    let mut failed_runs = 0;
    let mut recent_logs = Vec::new();

    for path in run_files.iter().rev() {
        let data = fs::read_to_string(path)
            .with_context(|| format!("读取运行历史失败 {}", path.display()))?;
        let record: RunRecord = serde_json::from_str(&data)
            .with_context(|| format!("解析运行历史失败 {}", path.display()))?;

        if record.steps.is_empty() {
            continue;
        }

        let all_ok = record.steps.iter().all(|step| step.ok);
        let any_failed = record.steps.iter().any(|step| !step.ok);

        if all_ok {
            completed_runs += 1;
        } else if any_failed {
            failed_runs += 1;
        }

        if recent_logs.len() < 3 {
            if let Some(last_step) = record.steps.last() {
                let content = last_step
                    .error
                    .clone()
                    .unwrap_or_else(|| last_step.output_preview.clone());
                let content = truncate_chars(&content, 80);
                let cost = record.token_usage.and_then(|tokens| {
                    cost_per_1k_tokens.map(|price| price * (tokens as f64 / 1000.0))
                });
                let usage = format_usage(record.token_usage, record.char_usage, cost);
                recent_logs.push(format!("[{}] {}: {} {}", record.id, last_step.name, content, usage));
            }
        }
    }

    Ok(StatusSummary {
        supervisor_running,
        pid,
        completed_runs,
        failed_runs,
        recent_logs,
    })
}

fn read_supervisor_running(status_path: &Path) -> anyhow::Result<bool> {
    if !status_path.exists() {
        return Ok(false);
    }
    let data = fs::read_to_string(status_path)
        .with_context(|| format!("读取状态文件失败 {}", status_path.display()))?;
    let status: SupervisorStatus = serde_json::from_str(&data)
        .with_context(|| format!("解析状态文件失败 {}", status_path.display()))?;
    Ok(status.supervisor_running)
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
        return Ok(files);
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(workspace_dir)
        .with_context(|| format!("读取工作区目录失败 {}", workspace_dir.display()))?
    {
        let entry = entry.with_context(|| format!("读取工作区条目失败 {}", workspace_dir.display()))?;
        let path = entry.path();
        let is_status = path.file_name().and_then(|name| name.to_str()) == Some("status.json");
        if path.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("json")
            && !is_status
        {
            files.push(path);
        }
    }
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
        "Supervisor: {state}\nCompleted runs: {}\nFailed runs: {}",
        summary.completed_runs, summary.failed_runs
    );
    if !summary.recent_logs.is_empty() {
        output.push_str("\nRecent logs:");
        for log in &summary.recent_logs {
            output.push_str(&format!("\n  - {log}"));
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

fn format_usage(token_usage: Option<u64>, char_usage: Option<u64>, cost: Option<f64>) -> String {
    let tokens = token_usage
        .map(|v| v.to_string())
        .unwrap_or_else(|| "N/A".to_string());
    let chars = char_usage
        .map(|v| v.to_string())
        .unwrap_or_else(|| "N/A".to_string());
    let mut result = format!("(tokens: {}, chars: {}", tokens, chars);
    if let Some(cost) = cost {
        result.push_str(&format!(", cost: ${:.4}", cost));
    }
    result.push(')');
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{RunRecord, StepRecord};
    use std::fs;
    use std::path::{Path, PathBuf};

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
        assert!(rendered.contains("Completed runs: 2"));
        assert!(rendered.contains("Failed runs: 1"));
        assert!(rendered.contains("Recent logs:"));

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
        assert!(rendered.contains("tokens: 123"));
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
        assert!(rendered.contains("tokens: N/A"));
        assert!(rendered.contains("chars: N/A"));

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
}