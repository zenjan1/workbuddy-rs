//! Tool trait and registry for Xi custom tool framework.

use std::collections::HashMap;
use std::sync::Arc;
use serde_json::Value;
use thiserror::Error;

/// Error type for tool execution and registration.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("tool already registered: {0}")]
    AlreadyRegistered(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
    #[error("timeout after {0} ms")]
    Timeout(u64),
    #[error("resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),
    #[error("schema error: {0}")]
    SchemaError(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result type for tool operations.
pub type ToolResult<T> = Result<T, ToolError>;

/// Trait representing an executable tool.
///
/// Tools have a name, a JSON Schema describing their parameters,
/// and an execute function that takes JSON arguments and returns JSON results.
pub trait Tool: Send + Sync {
    /// Returns the unique name of this tool.
    fn name(&self) -> &str;

    /// Returns the JSON Schema describing the tool's parameters.
    fn schema(&self) -> Value;

    /// Executes the tool with the given arguments.
    ///
    /// # Arguments
    /// * `args` - JSON value conforming to the tool's schema
    ///
    /// # Returns
    /// JSON value representing the tool's output
    fn execute(&self, args: Value) -> ToolResult<Value>;
}

/// Registry for managing a collection of tools.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Creates a new empty tool registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Registers a tool in the registry.
    ///
    /// # Arguments
    /// * `tool` - The tool to register (wrapped in Arc for shared ownership)
    ///
    /// # Returns
    /// Ok(()) if registration succeeds, Err if a tool with the same name already exists
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> ToolResult<()> {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            return Err(ToolError::AlreadyRegistered(name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    /// Retrieves a tool by name.
    ///
    /// # Arguments
    /// * `name` - The name of the tool to retrieve
    ///
    /// # Returns
    /// Some(Arc<dyn Tool>) if found, None otherwise
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Returns a list of all registered tool names.
    pub fn list(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Returns the number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Returns true if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Removes a tool from the registry by name.
    ///
    /// # Arguments
    /// * `name` - The name of the tool to remove
    ///
    /// # Returns
    /// true if a tool was removed, false if no tool with that name existed
    pub fn remove(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
    }
}

/// A built-in tool implemented as a Rust function.
///
/// This is the simplest tool type - a native Rust function that can be
/// called directly without any external process or WASM runtime.
pub struct BuiltinTool {
    name: String,
    schema: Value,
    executor: Arc<dyn Fn(Value) -> ToolResult<Value> + Send + Sync>,
}

impl BuiltinTool {
    /// Creates a new built-in tool.
    ///
    /// # Arguments
    /// * `name` - Unique tool name
    /// * `schema` - JSON Schema for the tool's parameters
    /// * `executor` - Function that executes the tool logic
    pub fn new<
        F: Fn(Value) -> ToolResult<Value> + Send + Sync + 'static,
    >(
        name: impl Into<String>,
        schema: Value,
        executor: F,
    ) -> Self {
        Self {
            name: name.into(),
            schema,
            executor: Arc::new(executor),
        }
    }
}

impl Tool for BuiltinTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn execute(&self, args: Value) -> ToolResult<Value> {
        (self.executor)(args)
    }
}

/// A tool that executes an external binary.
///
/// The binary is invoked as a subprocess with arguments passed via stdin/stdout
/// using a JSON-based protocol.
pub struct BinaryTool {
    name: String,
    schema: Value,
    binary_path: std::path::PathBuf,
    default_args: Vec<String>,
    timeout_ms: u64,
}

impl BinaryTool {
    /// Creates a new binary tool.
    ///
    /// # Arguments
    /// * `name` - Unique tool name
    /// * `schema` - JSON Schema for the tool's parameters
    /// * `binary_path` - Path to the executable binary
    /// * `default_args` - Default command-line arguments to pass to the binary
    /// * `timeout_ms` - Execution timeout in milliseconds
    pub fn new(
        name: impl Into<String>,
        schema: Value,
        binary_path: impl Into<std::path::PathBuf>,
        default_args: Vec<String>,
        timeout_ms: u64,
    ) -> Self {
        Self {
            name: name.into(),
            schema,
            binary_path: binary_path.into(),
            default_args,
            timeout_ms,
        }
    }
}

impl Tool for BinaryTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn execute(&self, args: Value) -> ToolResult<Value> {
        // TODO: Implement actual binary execution with sandbox
        // This is a placeholder that returns an error indicating not implemented
        Err(ToolError::ExecutionFailed(
            "BinaryTool execution not yet implemented; requires sandbox executor".to_string(),
        ))
    }
}

/// A tool implemented as a WebAssembly module.
///
/// Uses wasmtime to execute WASM modules in a sandboxed environment.
pub struct WasmTool {
    name: String,
    schema: Value,
    module_path: std::path::PathBuf,
    timeout_ms: u64,
    // wasmtime::Instance would be stored here in a real implementation
}

impl WasmTool {
    /// Creates a new WASM tool.
    ///
    /// # Arguments
    /// * `name` - Unique tool name
    /// * `schema` - JSON Schema for the tool's parameters
    /// * `module_path` - Path to the .wasm module file
    /// * `timeout_ms` - Execution timeout in milliseconds
    pub fn new(
        name: impl Into<String>,
        schema: Value,
        module_path: impl Into<std::path::PathBuf>,
        timeout_ms: u64,
    ) -> Self {
        Self {
            name: name.into(),
            schema,
            module_path: module_path.into(),
            timeout_ms,
        }
    }
}

impl Tool for WasmTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn execute(&self, args: Value) -> ToolResult<Value> {
        // TODO: Implement actual WASM execution with wasmtime
        // This is a placeholder that returns an error indicating not implemented
        Err(ToolError::ExecutionFailed(
            "WasmTool execution not yet implemented; requires wasmtime integration".to_string(),
        ))
    }
}

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command as TokioCommand;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Sandbox policy configuration for builtin tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// Working directory - all relative paths are resolved against this
    pub work_dir: PathBuf,
    /// Allowed directories for file operations (read/edit)
    pub allowed_dirs: Vec<PathBuf>,
    /// Allowed commands for shell execution
    pub allowed_commands: Vec<String>,
    /// Maximum file size for read operations (bytes)
    pub max_read_size: u64,
    /// Maximum file size for write operations (bytes)
    pub max_write_size: u64,
    /// Shell execution timeout in milliseconds
    pub shell_timeout_ms: u64,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            work_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            allowed_dirs: vec![],
            allowed_commands: vec![],
            max_read_size: 10 * 1024 * 1024,      // 10 MB
            max_write_size: 10 * 1024 * 1024,     // 10 MB
            shell_timeout_ms: 30_000,             // 30 seconds
        }
    }
}

impl SandboxPolicy {
    /// Creates a new sandbox policy with the given work directory.
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
            ..Default::default()
        }
    }

    /// Adds an allowed directory for file operations.
    pub fn allow_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.allowed_dirs.push(dir.into());
        self
    }

    /// Adds an allowed command for shell execution.
    pub fn allow_command(mut self, cmd: impl Into<String>) -> Self {
        self.allowed_commands.push(cmd.into());
        self
    }

    /// Sets the maximum read size.
    pub fn max_read_size(mut self, size: u64) -> Self {
        self.max_read_size = size;
        self
    }

    /// Sets the maximum write size.
    pub fn max_write_size(mut self, size: u64) -> Self {
        self.max_write_size = size;
        self
    }

    /// Sets the shell timeout.
    pub fn shell_timeout_ms(mut self, ms: u64) -> Self {
        self.shell_timeout_ms = ms;
        self
    }

    /// Resolves a path relative to work_dir and validates it against allowed_dirs.
    /// Returns the canonicalized absolute path if allowed.
    pub fn resolve_path(&self, path: &str) -> ToolResult<PathBuf> {
        let path = Path::new(path);
        
        // If absolute, use as-is; otherwise resolve relative to work_dir
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.work_dir.join(path)
        };
        
        // Canonicalize to resolve symlinks and .. components
        let canonical = absolute.canonicalize()
            .map_err(|e| ToolError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Path not found or inaccessible: {}", e)
            )))?;
        
        // Check if path is within allowed directories
        if !self.allowed_dirs.is_empty() {
            let allowed = self.allowed_dirs.iter().any(|allowed_dir| {
                // Try to canonicalize allowed_dir for comparison
                if let Ok(canon_allowed) = allowed_dir.canonicalize() {
                    canonical.starts_with(&canon_allowed)
                } else {
                    canonical.starts_with(allowed_dir)
                }
            });
            
            if !allowed {
                return Err(ToolError::ExecutionFailed(
                    format!("Path '{}' is not within allowed directories", canonical.display())
                ));
            }
        }
        
        // Also ensure it's within work_dir (defense in depth)
        let canon_work = self.work_dir.canonicalize().unwrap_or_else(|_| self.work_dir.clone());
        if !canonical.starts_with(&canon_work) {
            return Err(ToolError::ExecutionFailed(
                format!("Path '{}' escapes working directory", canonical.display())
            ));
        }
        
        Ok(canonical)
    }

    /// Validates a command against the allowed list.
    pub fn validate_command(&self, cmd: &str) -> ToolResult<()> {
        if self.allowed_commands.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "No commands are allowed in sandbox policy".to_string()
            ));
        }
        
        // Extract the command name (first word)
        let cmd_name = cmd.split_whitespace().next().unwrap_or("");
        let cmd_path = Path::new(cmd_name);
        let cmd_base = cmd_path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(cmd_name);
        
        let allowed = self.allowed_commands.iter().any(|allowed| {
            let allowed_path = Path::new(allowed);
            let allowed_base = allowed_path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(allowed);
            allowed_base == cmd_base || allowed == cmd_name
        });
        
        if !allowed {
            return Err(ToolError::ExecutionFailed(
                format!("Command '{}' is not in allowed list", cmd_name)
            ));
        }
        
        Ok(())
    }
}

/// Tool for reading file contents.
#[derive(Debug, Clone)]
pub struct ReadFileTool {
    policy: Arc<SandboxPolicy>,
}

impl ReadFileTool {
    /// Creates a new ReadFileTool with the given sandbox policy.
    pub fn new(policy: Arc<SandboxPolicy>) -> Self {
        Self { policy }
    }

    /// Returns the JSON schema for this tool.
    pub fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to work_dir or absolute)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Byte offset to start reading from",
                    "minimum": 0,
                    "default": 0
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of bytes to read",
                    "minimum": 1,
                    "maximum": 10485760,
                    "default": 10485760
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn schema(&self) -> Value {
        Self::schema()
    }

    fn execute(&self, args: Value) -> ToolResult<Value> {
        let path = args.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("Missing required 'path' parameter".to_string()))?;
        
        let offset = args.get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u64;
        
        let limit = args.get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.policy.max_read_size)
            .min(self.policy.max_read_size);
        
        // Resolve and validate path
        let resolved_path = self.policy.resolve_path(path)?;
        
        // Check file size
        let metadata = std::fs::metadata(&resolved_path)
            .map_err(|e| ToolError::Io(e))?;
        
        if !metadata.is_file() {
            return Err(ToolError::ExecutionFailed(
                format!("Path '{}' is not a file", resolved_path.display())
            ));
        }
        
        let file_size = metadata.len();
        if offset >= file_size {
            return Ok(json!({
                "content": "",
                "size": file_size,
                "offset": offset,
                "truncated": false
            }));
        }
        
        let read_len = std::cmp::min(limit, file_size - offset) as usize;
        
        // Read file
        let mut file = std::fs::File::open(&resolved_path)
            .map_err(|e| ToolError::Io(e))?;
        
        use std::io::{Seek, SeekFrom, Read};
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| ToolError::Io(e))?;
        
        let mut buffer = vec![0u8; read_len];
        let bytes_read = file.read(&mut buffer)
            .map_err(|e| ToolError::Io(e))?;
        
        buffer.truncate(bytes_read);
        
        // Try to decode as UTF-8, fallback to base64
        let content = String::from_utf8(buffer.clone()).unwrap_or_else(|_| {
            use base64::{Engine as _, engine::general_purpose};
            general_purpose::STANDARD.encode(&buffer)
        });
        
        Ok(json!({
            "content": content,
            "size": file_size,
            "offset": offset,
            "bytes_read": bytes_read,
            "truncated": bytes_read < limit as usize
        }))
    }
}

/// Tool for editing files (create, update, delete).
#[derive(Debug, Clone)]
pub struct EditFileTool {
    policy: Arc<SandboxPolicy>,
}

impl EditFileTool {
    /// Creates a new EditFileTool with the given sandbox policy.
    pub fn new(policy: Arc<SandboxPolicy>) -> Self {
        Self { policy }
    }

    /// Returns the JSON schema for this tool.
    pub fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative to work_dir or absolute)"
                },
                "operation": {
                    "type": "string",
                    "enum": ["create", "write", "append", "patch", "delete"],
                    "description": "Edit operation to perform"
                },
                "content": {
                    "type": "string",
                    "description": "Content for create/write/append operations"
                },
                "patch": {
                    "type": "array",
                    "description": "Unified diff patches for patch operation",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {"type": "string"},
                            "new_text": {"type": "string"}
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path", "operation"],
            "additionalProperties": false
        })
    }
}

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn schema(&self) -> Value {
        Self::schema()
    }

    fn execute(&self, args: Value) -> ToolResult<Value> {
        let path = args.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("Missing required 'path' parameter".to_string()))?;
        
        let operation = args.get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("Missing required 'operation' parameter".to_string()))?;
        
        // Resolve and validate path
        let resolved_path = self.policy.resolve_path(path)?;
        
        match operation {
            "create" | "write" => {
                let content = args.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArguments("Missing 'content' for write operation".to_string()))?;
                
                if content.len() as u64 > self.policy.max_write_size {
                    return Err(ToolError::ExecutionFailed(
                        format!("Content size {} exceeds maximum allowed {}", content.len(), self.policy.max_write_size)
                    ));
                }
                
                // For create, ensure parent directory exists
                if operation == "create" {
                    if let Some(parent) = resolved_path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| ToolError::Io(e))?;
                    }
                }
                
                std::fs::write(&resolved_path, content)
                    .map_err(|e| ToolError::Io(e))?;
                
                Ok(json!({
                    "path": resolved_path.to_string_lossy(),
                    "operation": operation,
                    "bytes_written": content.len()
                }))
            }
            "append" => {
                let content = args.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArguments("Missing 'content' for append operation".to_string()))?;
                
                if content.len() as u64 > self.policy.max_write_size {
                    return Err(ToolError::ExecutionFailed(
                        format!("Content size {} exceeds maximum allowed {}", content.len(), self.policy.max_write_size)
                    ));
                }
                
                use std::io::{Write, Seek, SeekFrom};
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&resolved_path)
                    .map_err(|e| ToolError::Io(e))?;
                
                file.write_all(content.as_bytes())
                    .map_err(|e| ToolError::Io(e))?;
                
                let new_size = file.seek(SeekFrom::End(0))
                    .map_err(|e| ToolError::Io(e))?;
                
                Ok(json!({
                    "path": resolved_path.to_string_lossy(),
                    "operation": "append",
                    "bytes_appended": content.len(),
                    "new_size": new_size
                }))
            }
            "patch" => {
                let patches = args.get("patch")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| ToolError::InvalidArguments("Missing 'patch' array for patch operation".to_string()))?;
                
                // Read current content
                let current_content = std::fs::read_to_string(&resolved_path)
                    .map_err(|e| ToolError::Io(e))?;
                
                let mut new_content = current_content;
                for patch in patches {
                    let old_text = patch.get("old_text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| ToolError::InvalidArguments("Missing 'old_text' in patch".to_string()))?;
                    let new_text = patch.get("new_text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| ToolError::InvalidArguments("Missing 'new_text' in patch".to_string()))?;
                    
                    if !new_content.contains(old_text) {
                        return Err(ToolError::ExecutionFailed(
                            format!("Patch failed: old_text not found in file: {}", old_text.chars().take(50).collect::<String>()) 
                        ));
                    }
                    
                    new_content = new_content.replacen(old_text, new_text, 1);
                }
                
                if new_content.len() as u64 > self.policy.max_write_size {
                    return Err(ToolError::ExecutionFailed(
                        format!("Resulting content size {} exceeds maximum allowed {}", new_content.len(), self.policy.max_write_size)
                    ));
                }
                
                std::fs::write(&resolved_path, &new_content)
                    .map_err(|e| ToolError::Io(e))?;
                
                Ok(json!({
                    "path": resolved_path.to_string_lossy(),
                    "operation": "patch",
                    "patches_applied": patches.len(),
                    "new_size": new_content.len()
                }))
            }
            "delete" => {
                if resolved_path.exists() {
                    std::fs::remove_file(&resolved_path)
                        .map_err(|e| ToolError::Io(e))?;
                    Ok(json!({
                        "path": resolved_path.to_string_lossy(),
                        "operation": "delete",
                        "deleted": true
                    }))
                } else {
                    Ok(json!({
                        "path": resolved_path.to_string_lossy(),
                        "operation": "delete",
                        "deleted": false,
                        "reason": "File did not exist"
                    }))
                }
            }
            _ => Err(ToolError::InvalidArguments(
                format!("Unknown operation: {}. Supported: create, write, append, patch, delete", operation)
            )),
        }
    }
}

/// Tool for executing shell commands.
#[derive(Debug, Clone)]
pub struct ShellTool {
    policy: Arc<SandboxPolicy>,
}

impl ShellTool {
    /// Creates a new ShellTool with the given sandbox policy.
    pub fn new(policy: Arc<SandboxPolicy>) -> Self {
        Self { policy }
    }

    /// Returns the JSON schema for this tool.
    pub fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Command arguments",
                    "default": []
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Execution timeout in milliseconds",
                    "minimum": 100,
                    "maximum": 300000,
                    "default": 30000
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory for command (relative to sandbox work_dir)",
                    "default": "."
                },
                "env": {
                    "type": "object",
                    "description": "Additional environment variables",
                    "additionalProperties": {"type": "string"},
                    "default": {}
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn schema(&self) -> Value {
        Self::schema()
    }

    fn execute(&self, args: Value) -> ToolResult<Value> {
        let command = args.get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("Missing required 'command' parameter".to_string()))?;
        
        // Validate command against allowed list
        self.policy.validate_command(command)?;
        
        let args_vec: Vec<String> = args.get("args")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        
        let timeout_ms = args.get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.policy.shell_timeout_ms)
            .min(self.policy.shell_timeout_ms);
        
        let working_dir = args.get("working_dir")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        
        let resolved_work_dir = self.policy.resolve_path(working_dir)?;
        
        let env_vars: std::collections::HashMap<String, String> = args.get("env")
            .and_then(|v| v.as_object())
            .map(|obj| obj.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
            .unwrap_or_default();
        
        // Execute command asynchronously with timeout
        let output = tokio::runtime::Handle::current().block_on(async {
            let mut cmd = TokioCommand::new(command);
            cmd.args(&args_vec)
                .current_dir(&resolved_work_dir)
                .envs(&env_vars)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            
            let mut child = cmd.spawn()
                .map_err(|e| ToolError::Io(e))?;
            
            let timeout_duration = Duration::from_millis(timeout_ms);
            
            match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
                Ok(Ok(output)) => Ok(output),
                Ok(Err(e)) => Err(ToolError::Io(e)),
                Err(_) => {
                    // Kill the process on timeout
                    let _ = child.kill().await;
                    Err(ToolError::Timeout(timeout_ms))
                }
            }
        })?;
        
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        
        Ok(json!({
            "command": command,
            "args": args_vec,
            "exit_code": output.status.code(),
            "stdout": stdout,
            "stderr": stderr,
            "success": output.status.success()
        }))
    }
}

/// Creates a default set of builtin tools with the given sandbox policy.
/// Returns a vector of (tool_name, Arc<dyn Tool>) pairs ready for registration.
pub fn create_builtin_tools(policy: Arc<SandboxPolicy>) -> Vec<(String, Arc<dyn Tool>)> {
    vec![
        ("read_file".to_string(), Arc::new(ReadFileTool::new(policy.clone())) as Arc<dyn Tool>),
        ("edit_file".to_string(), Arc::new(EditFileTool::new(policy.clone())) as Arc<dyn Tool>),
        ("shell".to_string(), Arc::new(ShellTool::new(policy)) as Arc<dyn Tool>),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn create_test_policy() -> Arc<SandboxPolicy> {
        let temp_dir = TempDir::new().unwrap();
        let work_dir = temp_dir.path().to_path_buf();
        
        // Create a test file
        std::fs::write(work_dir.join("test.txt"), "Hello, World!").unwrap();
        std::fs::create_dir_all(work_dir.join("subdir")).unwrap();
        std::fs::write(work_dir.join("subdir\nested.txt"), "Nested content").unwrap();
        
        Arc::new(SandboxPolicy::new(work_dir.clone())
            .allow_dir(work_dir)
            .allow_command("echo")
            .allow_command("cat")
            .allow_command("ls"))
    }

    #[test]
    fn test_sandbox_policy_resolve_path() {
        let policy = create_test_policy();
        
        // Test relative path resolution
        let resolved = policy.resolve_path("test.txt").unwrap();
        assert!(resolved.ends_with("test.txt"));
        assert!(resolved.is_absolute());
        
        // Test subdirectory
        let resolved = policy.resolve_path("subdir/nested.txt").unwrap();
        assert!(resolved.ends_with("subdir/nested.txt"));
        
        // Test absolute path within allowed dir
        let abs_path = policy.work_dir.join("test.txt");
        let resolved = policy.resolve_path(abs_path.to_str().unwrap()).unwrap();
        assert_eq!(resolved, abs_path.canonicalize().unwrap());
    }

    #[test]
    fn test_sandbox_policy_rejects_outside() {
        let policy = create_test_policy();
        
        // Try to escape with ..
        let result = policy.resolve_path("../etc/passwd");
        assert!(result.is_err());
        
        // Try absolute path outside allowed dirs
        let result = policy.resolve_path("/etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_sandbox_policy_validate_command() {
        let policy = create_test_policy();
        
        assert!(policy.validate_command("echo hello").is_ok());
        assert!(policy.validate_command("cat file.txt").is_ok());
        assert!(policy.validate_command("ls -la").is_ok());
        
        assert!(policy.validate_command("rm -rf /").is_err());
        assert!(policy.validate_command("unknown_command").is_err());
    }

    #[test]
    fn test_read_file_tool() {
        let policy = create_test_policy();
        let tool = ReadFileTool::new(policy.clone());
        
        // Test basic read
        let result = tool.execute(json!({"path": "test.txt"})).unwrap();
        assert_eq!(result["content"], "Hello, World!");
        assert_eq!(result["size"], 13);
        assert_eq!(result["offset"], 0);
        assert!(!result["truncated"].as_bool().unwrap());
        
        // Test read with offset and limit
        let result = tool.execute(json!({"path": "test.txt", "offset": 7, "limit": 5})).unwrap();
        assert_eq!(result["content"], "World");
        assert_eq!(result["offset"], 7);
        assert_eq!(result["bytes_read"], 5);
        
        // Test read beyond file size
        let result = tool.execute(json!({"path": "test.txt", "offset": 100})).unwrap();
        assert_eq!(result["content"], "");
        assert_eq!(result["bytes_read"], 0);
    }

    #[test]
    fn test_read_file_tool_rejects_outside() {
        let policy = create_test_policy();
        let tool = ReadFileTool::new(policy);
        
        let result = tool.execute(json!({"path": "../etc/passwd"}));
        assert!(result.is_err());
    }

    #[test]
    fn test_edit_file_tool_create() {
        let policy = create_test_policy();
        let tool = EditFileTool::new(policy.clone());
        
        let result = tool.execute(json!({
            "path": "new_file.txt",
            "operation": "create",
            "content": "New file content"
        })).unwrap();
        
        assert_eq!(result["operation"], "create");
        assert_eq!(result["bytes_written"], 16);
        
        // Verify file was created
        let content = std::fs::read_to_string(policy.work_dir.join("new_file.txt")).unwrap();
        assert_eq!(content, "New file content");
    }

    #[test]
    fn test_edit_file_tool_write() {
        let policy = create_test_policy();
        let tool = EditFileTool::new(policy.clone());
        
        let result = tool.execute(json!({
            "path": "test.txt",
            "operation": "write",
            "content": "Overwritten content"
        })).unwrap();
        
        assert_eq!(result["operation"], "write");
        assert_eq!(result["bytes_written"], 19);
        
        let content = std::fs::read_to_string(policy.work_dir.join("test.txt")).unwrap();
        assert_eq!(content, "Overwritten content");
    }

    #[test]
    fn test_edit_file_tool_append() {
        let policy = create_test_policy();
        let tool = EditFileTool::new(policy.clone());
        
        let result = tool.execute(json!({
            "path": "test.txt",
            "operation": "append",
            "content": " - appended"
        })).unwrap();
        
        assert_eq!(result["operation"], "append");
        assert_eq!(result["bytes_appended"], 11);
        
        let content = std::fs::read_to_string(policy.work_dir.join("test.txt")).unwrap();
        assert_eq!(content, "Hello, World! - appended");
    }

    #[test]
    fn test_edit_file_tool_patch() {
        let policy = create_test_policy();
        let tool = EditFileTool::new(policy.clone());
        
        let result = tool.execute(json!({
            "path": "test.txt",
            "operation": "patch",
            "patch": [
                {"old_text": "Hello", "new_text": "Hi"},
                {"old_text": "World", "new_text": "There"}
            ]
        })).unwrap();
        
        assert_eq!(result["operation"], "patch");
        assert_eq!(result["patches_applied"], 2);
        
        let content = std::fs::read_to_string(policy.work_dir.join("test.txt")).unwrap();
        assert_eq!(content, "Hi, There!");
    }

    #[test]
    fn test_edit_file_tool_delete() {
        let policy = create_test_policy();
        let tool = EditFileTool::new(policy.clone());
        
        let result = tool.execute(json!({
            "path": "test.txt",
            "operation": "delete"
        })).unwrap();
        
        assert_eq!(result["operation"], "delete");
        assert_eq!(result["deleted"], true);
        
        assert!(!policy.work_dir.join("test.txt").exists());
        
        // Delete non-existent file
        let result = tool.execute(json!({
            "path": "test.txt",
            "operation": "delete"
        })).unwrap();
        assert_eq!(result["deleted"], false);
    }

    #[test]
    fn test_shell_tool() {
        let policy = create_test_policy();
        let tool = ShellTool::new(policy.clone());
        
        let result = tool.execute(json!({
            "command": "echo",
            "args": ["hello", "world"]
        })).unwrap();
        
        assert_eq!(result["command"], "echo");
        assert_eq!(result["args"], json!(["hello", "world"]));
        assert_eq!(result["exit_code"], 0);
        assert!(result["success"].as_bool().unwrap());
        assert!(result["stdout"].as_str().unwrap().contains("hello world"));
    }

    #[test]
    fn test_shell_tool_rejects_unallowed() {
        let policy = create_test_policy();
        let tool = ShellTool::new(policy);
        
        let result = tool.execute(json!({
            "command": "rm",
            "args": ["-rf", "/"]
        }));
        assert!(result.is_err());
    }

    #[test]
    fn test_shell_tool_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let work_dir = temp_dir.path().to_path_buf();
        let policy = Arc::new(SandboxPolicy::new(work_dir)
            .allow_command("sleep")
            .shell_timeout_ms(100));
        let tool = ShellTool::new(policy);
        
        let result = tool.execute(json!({
            "command": "sleep",
            "args": ["10"]
        }));
        assert!(matches!(result, Err(ToolError::Timeout(_))));
    }

    #[test]
    fn test_create_builtin_tools() {
        let policy = create_test_policy();
        let tools = create_builtin_tools(policy);
        
        assert_eq!(tools.len(), 3);
        let names: Vec<_> = tools.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["read_file", "edit_file", "shell"]);
        
        for (name, tool) in tools {
            assert_eq!(tool.name(), name);
            assert!(tool.schema().is_object());
        }
    }

    #[test]
    fn test_builtin_tool_creation() {
        let tool = BuiltinTool::new(
            "test_tool",
            json!({"type": "object", "properties": {}}),
            |_args| Ok(json!({"result": "ok"})),
        );
        assert_eq!(tool.name(), "test_tool");
        assert!(tool.schema().is_object());
    }

    #[test]
    fn test_builtin_tool_execution() {
        let tool = BuiltinTool::new(
            "echo",
            json!({"type": "object", "properties": {"msg": {"type": "string"}}}),
            |args| Ok(json!({"echo": args["msg"]})),
        );
        let result = tool.execute(json!({"msg": "hello"})).unwrap();
        assert_eq!(result["echo"], "hello");
    }

    #[test]
    fn test_tool_registry_register_and_get() {
        let mut registry = ToolRegistry::new();
        let tool = Arc::new(BuiltinTool::new(
            "test",
            json!({}),
            |_| Ok(json!({})),
        ));
        registry.register(tool.clone()).unwrap();
        let retrieved = registry.get("test").unwrap();
        assert_eq!(retrieved.name(), "test");
    }

    #[test]
    fn test_tool_registry_duplicate_registration() {
        let mut registry = ToolRegistry::new();
        let tool = Arc::new(BuiltinTool::new(
            "test",
            json!({}),
            |_| Ok(json!({})),
        ));
        registry.register(tool.clone()).unwrap();
        let result = registry.register(tool);
        assert!(matches!(result, Err(ToolError::AlreadyRegistered(_))));
    }

    #[test]
    fn test_tool_registry_list() {
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(BuiltinTool::new("a", json!({}), |_| Ok(json!({})))))
            .unwrap();
        registry
            .register(Arc::new(BuiltinTool::new("b", json!({}), |_| Ok(json!({})))))
            .unwrap();
        let list = registry.list();
        assert_eq!(list.len(), 2);
        assert!(list.contains(&"a".to_string()));
        assert!(list.contains(&"b".to_string()));
    }

    #[test]
    fn test_tool_registry_remove() {
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(BuiltinTool::new("test", json!({}), |_| Ok(json!({})))))
            .unwrap();
        assert!(registry.remove("test"));
        assert!(!registry.remove("test"));
        assert!(registry.get("test").is_none());
    }
}
