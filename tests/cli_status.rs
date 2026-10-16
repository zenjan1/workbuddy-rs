use std::process::Command;

#[test]
fn status_command_succeeds_offline_and_prints_summary() {
    let bin = env!("CARGO_BIN_EXE_workbuddy");
    let output = Command::new(bin)
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
}
