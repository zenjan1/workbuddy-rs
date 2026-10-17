use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn make_temp_dir() -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "workbuddy-status-test-{}-{}",
        std::process::id(),
        unique
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_mock_state(dir: &Path) {
    fs::create_dir_all(dir).expect("create state dir");

    // Legacy file-based state, kept for compatibility with parsers that
    // read individual metadata files rather than the structured JSON file.
    fs::write(dir.join("supervisor.pid"), format!("{}\n", std::process::id())).unwrap();
    fs::write(dir.join("supervisor.lock"), "").unwrap();
    fs::write(dir.join("completed.txt"), "314159\n").unwrap();
    fs::write(dir.join("completed"), "314159\n").unwrap();
    fs::write(dir.join("failed.txt"), "271828\n").unwrap();
    fs::write(dir.join("failed"), "271828\n").unwrap();
    fs::write(
        dir.join("workbuddy.log"),
        "OLD_LOG_0\nLOG_UNIQUE_ALPHA\nLOG_UNIQUE_BETA\nLOG_UNIQUE_GAMMA\nLOG_UNIQUE_DELTA\nLOG_UNIQUE_EPSILON\n",
    )
    .unwrap();

    // Structured state file used by the status command. The recent log lines
    // intentionally omit OLD_LOG_0 so the summary only contains recent entries.
    let state_dir = dir.join(".workbuddy");
    fs::create_dir_all(&state_dir).expect("create .workbuddy state dir");
    fs::write(
        state_dir.join("status.json"),
        r#"{
  "supervisor_running": false,
  "completed": 314159,
  "failed": 271828,
  "recent_logs": [
    "LOG_UNIQUE_ALPHA",
    "LOG_UNIQUE_BETA",
    "LOG_UNIQUE_GAMMA",
    "LOG_UNIQUE_DELTA",
    "LOG_UNIQUE_EPSILON"
  ]
}
"#,
    )
    .unwrap();
}

#[test]
fn status_command_succeeds_offline_and_prints_summary() {
    let dir = make_temp_dir();
    write_mock_state(&dir);

    let bin = env!("CARGO_BIN_EXE_workbuddy");
    let output = Command::new(bin)
        .current_dir(&dir)
        .arg("status")
        .output()
        .expect("failed to run workbuddy status");

    assert!(output.status.success(), "status exited with {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout_lower = stdout.to_lowercase();
    let stderr_lower = stderr.to_lowercase();

    assert!(
        !stderr_lower.contains("error") && !stderr_lower.contains("panic"),
        "stderr should not contain errors: {}",
        stderr
    );

    assert!(
        stdout_lower.contains("supervisor") || stdout.contains("监督者"),
        "stdout should mention supervisor status: {}",
        stdout
    );
    assert!(
        stdout_lower.contains("completed") || stdout.contains("已完成") || stdout.contains("完成"),
        "stdout should mention completed tasks: {}",
        stdout
    );
    assert!(
        stdout_lower.contains("failed") || stdout.contains("失败"),
        "stdout should mention failed tasks: {}",
        stdout
    );
    assert!(
        stdout_lower.contains("recent") || stdout.contains("最近日志") || stdout.contains("日志摘要") || stdout_lower.contains("log"),
        "stdout should mention recent logs: {}",
        stdout
    );

    assert!(
        stdout.contains("314159"),
        "stdout should include completed count from mock state: {}",
        stdout
    );
    assert!(
        stdout.contains("271828"),
        "stdout should include failed count from mock state: {}",
        stdout
    );
    assert!(
        stdout.contains("LOG_UNIQUE_ALPHA"),
        "stdout should include a recent log entry: {}",
        stdout
    );
    assert!(
        stdout.contains("LOG_UNIQUE_EPSILON"),
        "stdout should include the latest log entry: {}",
        stdout
    );
    assert!(
        !stdout.contains("OLD_LOG_0"),
        "stdout should omit old log entries: {}",
        stdout
    );
}

#[test]
fn status_parses_mock_state_and_formats_summary() {
    let dir = make_temp_dir();
    write_mock_state(&dir);

    let report = workbuddy_rs::status::parse_status(&dir).expect("parse_status should succeed");
    let output = workbuddy_rs::status::format_status(&report);

    let output_lower = output.to_lowercase();
    assert!(
        output_lower.contains("supervisor") || output.contains("监督者"),
        "formatted output should mention supervisor status: {output}"
    );
    assert!(
        output.contains("314159"),
        "formatted output should include completed count: {output}"
    );
    assert!(
        output.contains("271828"),
        "formatted output should include failed count: {output}"
    );
    assert!(
        output.contains("LOG_UNIQUE_ALPHA"),
        "recent log should include the 2nd line: {output}"
    );
    assert!(
        output.contains("LOG_UNIQUE_DELTA"),
        "recent log should include the 5th line: {output}"
    );
    assert!(
        output.contains("LOG_UNIQUE_EPSILON"),
        "recent log should include the last line: {output}"
    );
    assert!(
        !output.contains("OLD_LOG_0"),
        "oldest log line should be omitted from summary: {output}"
    );
}

#[test]
fn status_missing_metadata_returns_friendly_default() {
    let dir = make_temp_dir();

    let summary = workbuddy_rs::status::parse_status(&dir)
        .expect("missing metadata should fall back to zero counts");
    assert_eq!(summary.completed_runs, 0);
    assert_eq!(summary.failed_runs, 0);
    assert!(summary.recent_logs.is_empty());
}

#[test]
fn status_corrupt_metadata_returns_friendly_error() {
    let dir = make_temp_dir();
    write_mock_state(&dir);

    fs::write(dir.join("completed.txt"), "not-a-number\n").unwrap();
    fs::write(dir.join("completed"), "not-a-number\n").unwrap();

    // Corrupt the structured JSON state too so parsers that prefer it
    // also observe invalid metadata.
    fs::write(
        dir.join(".workbuddy").join("status.json"),
        r#"{
  "supervisor_running": false,
  "completed": "not-a-number",
  "failed": 271828,
  "recent_logs": [
    "LOG_UNIQUE_ALPHA",
    "LOG_UNIQUE_BETA",
    "LOG_UNIQUE_GAMMA",
    "LOG_UNIQUE_DELTA",
    "LOG_UNIQUE_EPSILON"
  ]
}
"#,
    )
    .unwrap();

    let err = workbuddy_rs::status::parse_status(&dir).expect_err("corrupt metadata should fail");
    let msg = err.to_string().to_lowercase();

    assert!(
        msg.contains("invalid")
            || msg.contains("corrupt")
            || msg.contains("parse")
            || msg.contains("无法")
            || msg.contains("错误"),
        "error should be friendly when metadata is corrupt: {msg}"
    );
}

fn write_status_state(dir: &Path, completed: u64, failed: u64, recent_logs: &[&str]) {
    let state_dir = dir.join(".workbuddy");
    fs::create_dir_all(&state_dir).expect("create .workbuddy state dir");
    let logs = if recent_logs.is_empty() {
        String::new()
    } else {
        recent_logs
            .iter()
            .map(|line| format!("    \"{}\"", line))
            .collect::<Vec<_>>()
            .join(",\n")
            + "\n"
    };
    let json = format!(
        r#"{{
  "supervisor_running": false,
  "completed": {},
  "failed": {},
  "recent_logs": [
{}
  ]
}}"#,
        completed, failed, logs
    );
    fs::write(state_dir.join("status.json"), json).unwrap();
}

#[test]
fn status_command_with_no_history_reports_zero_counts_offline() {
    let dir = make_temp_dir();
    write_status_state(&dir, 0, 0, &[]);

    let bin = env!("CARGO_BIN_EXE_workbuddy");
    let output = Command::new(bin)
        .current_dir(&dir)
        .arg("status")
        .output()
        .expect("failed to run workbuddy status");

    assert!(output.status.success(), "status exited with {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr_lower = stderr.to_lowercase();

    assert!(
        !stderr_lower.contains("error") && !stderr_lower.contains("panic"),
        "stderr should not contain errors: {stderr}"
    );
    assert!(stdout.contains('0'), "stdout should report zero counts: {stdout}");
}

#[test]
fn status_with_mixed_success_and_failure_counts_accurately() {
    let dir = make_temp_dir();
    write_status_state(&dir, 424242, 434343, &["alpha-success", "beta-failure", "gamma-success"]);

    let bin = env!("CARGO_BIN_EXE_workbuddy");
    let output = Command::new(bin)
        .current_dir(&dir)
        .arg("status")
        .output()
        .expect("failed to run workbuddy status");

    assert!(output.status.success(), "status exited with {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr_lower = stderr.to_lowercase();

    assert!(
        !stderr_lower.contains("error") && !stderr_lower.contains("panic"),
        "stderr should not contain errors: {stderr}"
    );
    assert!(stdout.contains("424242"), "stdout should show completed count: {stdout}");
    assert!(stdout.contains("434343"), "stdout should show failed count: {stdout}");
}

#[test]
fn status_format_truncates_recent_logs_to_latest_five() {
    let dir = make_temp_dir();
    write_status_state(
        &dir,
        1,
        1,
        &[
            "OLD_LOG_A",
            "OLD_LOG_B",
            "OLD_LOG_C",
            "RECENT_LOG_1",
            "RECENT_LOG_2",
            "RECENT_LOG_3",
            "RECENT_LOG_4",
            "RECENT_LOG_5",
        ],
    );

    let report = workbuddy_rs::status::parse_status(&dir).expect("state with many logs should parse");
    let output = workbuddy_rs::status::format_status(&report);

    for keep in [
        "RECENT_LOG_1",
        "RECENT_LOG_2",
        "RECENT_LOG_3",
        "RECENT_LOG_4",
        "RECENT_LOG_5",
    ] {
        assert!(output.contains(keep), "recent log {keep} should be shown: {output}");
    }
    for old in ["OLD_LOG_A", "OLD_LOG_B", "OLD_LOG_C"] {
        assert!(!output.contains(old), "old log {old} should be truncated: {output}");
    }
}

#[test]
fn status_report_paths_are_fully_offline() {
    let dir = make_temp_dir();
    write_status_state(&dir, 1, 1, &["offline-log"]);

    // These functions only read local state and format text; they must not
    // construct an LLM client or make network calls.
    let report = workbuddy_rs::status::parse_status(&dir).expect("parse_status should work offline");
    let output = workbuddy_rs::status::format_status(&report);
    assert!(output.contains("offline-log"));
}