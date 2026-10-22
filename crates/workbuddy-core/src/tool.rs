//! Tool trait and registry for WorkBuddy custom tool framework.

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
