//! Shell tool implementation for Xi executor.
//! Provides controlled command execution capability with safety restrictions.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use xi_core::{Tool, ToolError, ToolResult, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use async_trait::async_trait;

/// Maximum allowed timeout in seconds.
const MAX_TIMEOUT_SEC: u64 = 300;
/// Default timeout in seconds.
const DEFAULT_TIMEOUT_SEC: u64 = 30;
/// Output truncation limit (10KB).
const OUTPUT_LIMIT: usize = 10 * 1024;

/// Commands that are prohibited due to interactive nature.
const PROHIBITED_COMMANDS: &[&str] = &[
    "vim", "vi", "nano", "emacs", "ed",
    "top", "htop", "btop", "atop",
    "less", "more", "most",
    "man", "info",
    "ssh", "telnet", "ftp", "sftp",
    "su", "sudo", "doas",
    "passwd", "chsh",
    "python", "python3", "python2", "ipython", "python -i",
    "node", "node -i", "node --inspect",
    "ruby", "irb", "pry",
    "lua", "luajit",
    "perl", "perl -d",
    "bash", "sh", "zsh", "fish", "tcsh", "csh", "ksh",
    "screen", "tmux", "byobu",
    "watch", "tail -f",
];

/// Environment variable whitelist.
const ENV_WHITELIST: &[&str] = &[
    "PATH", "HOME", "USER", "SHELL", "LANG", "TERM",
    "LC_ALL", "LC_CTYPE", "LANGUAGE",
    "TZ", "TMPDIR", "TEMP", "TMP",
    "PWD", "OLDPWD",
];

/// Input parameters for ShellTool.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellToolArgs {
    /// Command to execute (e.g., "echo", "ls", "cat").
    pub cmd: String,
    /// Arguments to pass to the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Timeout in seconds (default: 30, max: 300).
    #[serde(default)]
    pub timeout_sec: Option<u64>,
    /// Working directory (must be within workspace root).
    #[serde(default)]
    pub workdir: Option<String>,
}

/// Output result from ShellTool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellToolResult {
    /// Process exit code.
    pub exit_code: i32,
    /// Captured stdout (truncated to 10KB).
    pub stdout: String,
    /// Captured stderr (truncated to 10KB).
    pub stderr: String,
    /// Whether output was truncated.
    pub truncated: bool,
}

/// ShellTool provides controlled command execution.
#[derive(Debug, Clone)]
pub struct ShellTool;

impl ShellTool {
    /// Create a new ShellTool instance.
    pub fn new() -> Self {
        Self
    }

    /// Validate that the command is not prohibited.
    fn validate_command(&self, cmd: &str) -> Result<(), ToolError> {
        let cmd_lower = cmd.to_lowercase();
        let cmd_name = Path::new(&cmd_lower)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&cmd_lower);

        for prohibited in PROHIBITED_COMMANDS {
            if cmd_name == *prohibited || cmd_lower.starts_with(&format!("{} ", prohibited)) {
                return Err(ToolError::InvalidArguments(format!(
                    "Command '{}' is prohibited (interactive or unsafe)",
                    cmd
                )));
            }
        }
        Ok(())
    }

    /// Validate and resolve the working directory.
    fn validate_workdir(&self, workdir: Option<&str>) -> Result<PathBuf, ToolError> {
        let workspace_root = workspace::root()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to get workspace root: {}", e)))?;

        let target_dir = if let Some(wd) = workdir {
            let path = Path::new(wd);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace_root.join(path)
            }
        } else {
            workspace_root.clone()
        };

        // Canonicalize to resolve symlinks and verify existence
        let canonical_target = target_dir.canonicalize().map_err(|e| {
            ToolError::InvalidArguments(format!("Invalid workdir '{}': {}", target_dir.display(), e))
        })?;
        let canonical_root = workspace_root.canonicalize().map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to canonicalize workspace root: {}", e))
        })?;

        // Ensure target is within workspace root
        if !canonical_target.starts_with(&canonical_root) {
            return Err(ToolError::InvalidArguments(format!(
                "Workdir '{}' is outside workspace root '{}'",
                canonical_target.display(),
                canonical_root.display()
            )));
        }

        Ok(canonical_target)
    }

    /// Build environment with whitelisted variables only.
    fn build_env(&self) -> Vec<(String, String)> {
        std::env::vars()
            .filter(|(k, _)| ENV_WHITELIST.iter().any(|allowed| k.eq_ignore_ascii_case(allowed)))
            .collect()
    }

    /// Truncate output to limit and return truncated flag.
    fn truncate_output(output: &mut String) -> bool {
        if output.len() > OUTPUT_LIMIT {
            output.truncate(OUTPUT_LIMIT);
            output.push_str("\n... [truncated]");
            true
        } else {
            false
        }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command with safety restrictions. Non-interactive commands only."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "cmd": {
                        "type": "string",
                        "description": "Command to execute (e.g., 'echo', 'ls', 'cat')"
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Arguments to pass to the command",
                        "default": []
                    },
                    "timeoutSec": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 30, max: 300)",
                        "minimum": 1,
                        "maximum": 300,
                        "default": 30
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Working directory (must be within workspace root)"
                    }
                },
                "required": ["cmd"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: Value) -> Result<ToolResult, ToolError> {
        // Parse arguments
        let args: ShellToolArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("Invalid arguments: {}", e)))?;

        // Validate command
        self.validate_command(&args.cmd)?;

        // Validate and resolve workdir
        let workdir = self.validate_workdir(args.workdir.as_deref())?;

        // Determine timeout
        let timeout_sec = args.timeout_sec.unwrap_or(DEFAULT_TIMEOUT_SEC).min(MAX_TIMEOUT_SEC);
        let timeout_duration = Duration::from_secs(timeout_sec);

        // Build command
        let mut cmd = Command::new(&args.cmd);
        cmd.args(&args.args)
            .current_dir(&workdir)
            .envs(self.build_env())
            .stdin(std::process::Stdio::null()) // No stdin
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Execute with timeout
        let output = match timeout(timeout_duration, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(ToolError::ExecutionFailed(format!("Failed to spawn command: {}", e)));
            }
            Err(_) => {
                return Err(ToolError::Timeout(timeout_sec * 1000));
            }
        };

        // Process output
        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let truncated_stdout = Self::truncate_output(&mut stdout);
        let truncated_stderr = Self::truncate_output(&mut stderr);
        let truncated = truncated_stdout || truncated_stderr;

        let exit_code = output.status.code().unwrap_or(-1);

        let result = ShellToolResult {
            exit_code,
            stdout,
            stderr,
            truncated,
        };

        if exit_code != 0 {
            let error_msg = format!("Command exited with code {}: {}", exit_code, result.stderr);
            return Err(ToolError::ExecutionFailed(error_msg));
        }

        Ok(ToolResult::Structured(serde_json::to_value(result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize result: {}", e)))?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xi_core::Tool;
    use serde_json::json;

    #[tokio::test]
    async fn test_echo_success() {
        let tool = ShellTool::new();
        let args = json!({
            "cmd": "echo",
            "args": ["hello"]
        });
        let result = tool.execute(args).await.unwrap();
        match result {
            ToolResult::Structured(v) => {
                let r: ShellToolResult = serde_json::from_value(v).unwrap();
                assert_eq!(r.exit_code, 0);
                assert!(r.stdout.contains("hello"));
                assert!(!r.truncated);
            }
            _ => panic!("Expected structured result"),
        }
    }

    #[tokio::test]
    async fn test_exit_failure() {
        let tool = ShellTool::new();
        let args = json!({
            "cmd": "sh",
            "args": ["-c", "exit 1"]
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("exited with code 1"));
            }
            e => panic!("Expected ExecutionFailed, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_timeout() {
        let tool = ShellTool::new();
        let args = json!({
            "cmd": "sleep",
            "args": ["10"],
            "timeoutSec": 1
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::Timeout(ms) => {
                assert_eq!(ms, 1000);
            }
            e => panic!("Expected Timeout, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_prohibited_command() {
        let tool = ShellTool::new();
        let args = json!({
            "cmd": "vim",
            "args": []
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidArguments(msg) => {
                assert!(msg.contains("prohibited"));
            }
            e => panic!("Expected InvalidArguments, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_workdir_outside_workspace() {
        let tool = ShellTool::new();
        let args = json!({
            "cmd": "pwd",
            "workdir": "/tmp"
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidArguments(msg) => {
                assert!(msg.contains("outside workspace"));
            }
            e => panic!("Expected InvalidArguments, got {:?}", e),
        }
    }
}
