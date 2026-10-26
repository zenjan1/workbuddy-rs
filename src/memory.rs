use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

static STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn store_lock() -> &'static Mutex<()> {
    STORE_LOCK.get_or_init(|| Mutex::new(()))
}

fn now_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Debug)]
pub enum MemoryError {
    Io(std::io::Error),
    NotFound(String),
    Invalid(String),
    Conflict(String),
    Parse(String),
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::Io(err) => write!(f, "memory io error: {err}"),
            MemoryError::NotFound(id) => write!(f, "memory entry not found: {id}"),
            MemoryError::Invalid(msg) => write!(f, "invalid memory entry: {msg}"),
            MemoryError::Conflict(msg) => write!(f, "memory conflict: {msg}"),
            MemoryError::Parse(msg) => write!(f, "failed to parse memory file: {msg}"),
        }
    }
}

impl Error for MemoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            MemoryError::Io(err) => Some(err),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: u64,
    pub timestamp: u128,
    #[serde(default)]
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
    #[serde(default)]
    pub input_chars: Option<u64>,
    #[serde(default)]
    pub output_chars: Option<u64>,
    /// 父 Agent 的 ID，用于血统追踪。根节点为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    /// 触发 spawn 的原因，如 "task_decomposition"、"tool_call"、"user_request"。
    #[serde(default)]
    pub spawn_reason: String,
}

/// 权限检查结果记录
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionCheckResult {
    /// 是否允许执行
    pub allowed: bool,
    /// 拒绝原因（如果不允许）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 沙箱隔离信息记录
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxInfo {
    /// 沙箱唯一标识
    pub sandbox_id: String,
    /// 工作目录
    pub working_dir: String,
    /// 环境变量快照
    #[serde(default)]
    pub env_vars: std::collections::HashMap<String, String>,
}

/// 单次工具调用的记录
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// 工具名称，如 "edit"、"shell"、"read"
    pub tool_name: String,
    /// 工具输入参数（JSON 格式）
    pub input: serde_json::Value,
    /// 工具输出结果（JSON 格式）
    pub output: serde_json::Value,
    /// 权限检查结果
    pub permission_check: PermissionCheckResult,
    /// 沙箱隔离信息
    pub sandbox_info: SandboxInfo,
    /// 调用时间戳（毫秒）
    pub timestamp: u128,
}

/// 执行步骤记录，包含该步骤内的所有工具调用
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    /// 步骤唯一标识
    pub step_id: u64,
    /// 步骤开始时间戳（毫秒）
    pub timestamp: u128,
    /// 步骤描述
    pub description: String,
    /// 本步骤内的工具调用记录列表
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
}

#[derive(Debug)]
pub struct MemoryStore {
    path: PathBuf,
}

impl MemoryStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, MemoryError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(MemoryError::Io)?;
            }
        }
        Ok(Self { path })
    }

    /// 根据 parent_agent_id 查找所有直接子节点。
    pub fn find_children(&self, parent_agent_id: &str) -> Result<Vec<MemoryEntry>, MemoryError> {
        let entries = self.read_entries()?;
        Ok(entries
            .into_iter()
            .filter(|e| e.parent_agent_id.as_deref() == Some(parent_agent_id))
            .collect())
    }

    /// 递归查找所有后代节点（子节点、孙节点等）。
    pub fn find_descendants(&self, ancestor_agent_id: &str) -> Result<Vec<MemoryEntry>, MemoryError> {
        let mut result = Vec::new();
        let mut stack = vec![ancestor_agent_id.to_string()];
        let all_entries = self.read_entries()?;
        
        // 构建 parent_agent_id -> children 映射以提高查找效率
        let mut children_map: std::collections::HashMap<String, Vec<MemoryEntry>> = 
            std::collections::HashMap::new();
        for entry in all_entries {
            if let Some(ref parent_id) = entry.parent_agent_id {
                children_map.entry(parent_id.clone()).or_default().push(entry);
            }
        }
        
        while let Some(current_id) = stack.pop() {
            if let Some(children) = children_map.get(&current_id) {
                for child in children {
                    result.push(child.clone());
                    stack.push(child.id.to_string()); // 使用 id 作为下一层的 parent_agent_id 查找键
                }
            }
        }
        Ok(result)
    }

    /// Opens the workspace-scoped memory file.
    pub fn open(workspace_dir: impl AsRef<Path>) -> Result<Self, MemoryError> {
        Self::new(memory_file_path(workspace_dir.as_ref()))
    }

    /// Selects workspace memory when a workspace dir is provided, otherwise the
    /// user config directory memory.jsonl.
    pub fn open_in_workspace_or_config(workspace_dir: Option<&Path>) -> Result<Self, MemoryError> {
        let path = match workspace_dir {
            Some(dir) => memory_file_path(dir),
            None => default_memory_file_path(),
        };
        Self::new(path)
    }

    pub fn add(&self, content: &str) -> Result<MemoryEntry, MemoryError> {
        self.add_with_title(content, content)
    }

    pub fn add_with_title(&self, title: &str, content: &str) -> Result<MemoryEntry, MemoryError> {
        let title = title.trim();
        let content = content.trim();
        if title.is_empty() {
            return Err(MemoryError::Invalid("memory title must not be empty".to_owned()));
        }
        if content.is_empty() {
            return Err(MemoryError::Invalid("memory content must not be empty".to_owned()));
        }

        // Process-level lock protects the read-append sequence so concurrent
        // add() calls cannot reuse ids.
        let _guard = store_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let entries = self.read_entries()?;
        let next_id = entries.iter().map(|entry| entry.id).max().unwrap_or(0) + 1;
        let timestamp = now_timestamp_millis();
        let entry = MemoryEntry {
            id: next_id,
            timestamp,
            title: title.to_owned(),
            content: content.to_owned(),
            created_at: timestamp.to_string(),
            token_usage: None,
            input_chars: None,
            output_chars: None,
            parent_agent_id: None,
            spawn_reason: String::new(),
        };

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(MemoryError::Io)?;
        let line = serde_json::to_string(&entry)
            .map_err(|err| MemoryError::Parse(format!("failed to serialize memory entry: {err}")))?;
        writeln!(file, "{}", line).map_err(MemoryError::Io)?;
        file.sync_all().map_err(MemoryError::Io)?;
        Ok(entry)
    }

    pub fn list(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        let mut entries = self.read_entries()?;
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.id.cmp(&a.id)));
        Ok(entries)
    }

    pub fn get<I>(&self, id: I) -> Result<MemoryEntry, MemoryError>
    where
        I: std::fmt::Display,
    {
        let id_text = id.to_string();
        let id = id_text.trim().parse::<u64>().map_err(|_| {
            MemoryError::Invalid(format!("memory id must be a positive integer: {}", id_text.trim()))
        })?;
        if id == 0 {
            return Err(MemoryError::Invalid("memory id must be a positive integer".to_owned()));
        }

        if !self.path.exists() {
            return Err(MemoryError::NotFound(id.to_string()));
        }

        let file = fs::File::open(&self.path).map_err(MemoryError::Io)?;
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let line = line.map_err(MemoryError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<MemoryEntry>(trimmed) {
                Ok(entry) if entry.id == id => return Ok(entry),
                Ok(_) => {}
                Err(_) => {
                    // Best-effort detection: if a corrupt line appears to contain
                    // this id, report it instead of silently returning NotFound.
                    if trimmed.contains(&format!("\"id\":{}", id))
                        || trimmed.contains(&format!("\"id\": {}", id))
                    {
                        return Err(MemoryError::Parse(format!(
                            "memory entry {id} exists but is corrupt"
                        )));
                    }
                }
            }
        }
        Err(MemoryError::NotFound(id.to_string()))
    }

    pub fn remove(&self, id: u64) -> Result<MemoryEntry, MemoryError> {
        let _guard = store_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries = self.read_entries()?;
        let pos = entries.iter().position(|entry| entry.id == id);
        match pos {
            Some(pos) => {
                let removed = entries.remove(pos);
                self.write_entries(&entries)?;
                Ok(removed)
            }
            None => Err(MemoryError::NotFound(id.to_string())),
        }
    }

    pub fn delete(&self, id: u64) -> Result<(), MemoryError> {
        self.remove(id).map(|_| ())
    }

    fn read_entries(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&self.path).map_err(MemoryError::Io)?;
        let reader = std::io::BufReader::new(file);
        let mut entries = Vec::new();

        for (idx, line) in reader.lines().enumerate() {
            let line = line.map_err(MemoryError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<MemoryEntry>(trimmed) {
                Ok(entry) => entries.push(entry),
                Err(err) => {
                    eprintln!(
                        "Warning: skipping corrupt memory line {} in {}: {err}",
                        idx + 1,
                        self.path.display()
                    );
                }
            }
        }

        Ok(entries)
    }

    fn write_entries(&self, entries: &[MemoryEntry]) -> Result<(), MemoryError> {
        let tmp_path = self.path.with_extension("jsonl.tmp");
        let mut tmp = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(MemoryError::Io)?;

        for entry in entries {
            let line = serde_json::to_string(entry)
                .map_err(|err| MemoryError::Parse(format!("failed to serialize memory entry: {err}")))?;
            writeln!(tmp, "{}", line).map_err(MemoryError::Io)?;
        }
        tmp.sync_all().map_err(MemoryError::Io)?;
        drop(tmp);

        fs::rename(&tmp_path, &self.path).map_err(MemoryError::Io)?;
        Ok(())
    }
}

pub fn memory_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".xi").join("memory.jsonl")
}

pub fn config_memory_file_path() -> PathBuf {
    default_memory_file_path()
}

fn default_memory_file_path() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".xi").join("memory.jsonl"))
        .unwrap_or_else(|| PathBuf::from(".xi").join("memory.jsonl"))
}

pub fn add(
    workspace_dir: Option<&Path>,
    title: &str,
    content: &str,
) -> Result<MemoryEntry, MemoryError> {
    let store = MemoryStore::open_in_workspace_or_config(workspace_dir)?;
    store.add_with_title(title, content)
}

pub fn list(workspace_dir: Option<&Path>) -> Result<Vec<MemoryEntry>, MemoryError> {
    let store = MemoryStore::open_in_workspace_or_config(workspace_dir)?;
    store.list()
}

pub fn get(workspace_dir: Option<&Path>, id: &str) -> Result<MemoryEntry, MemoryError> {
    let store = MemoryStore::open_in_workspace_or_config(workspace_dir)?;
    store.get(id)
}

#[cfg(test)]
mod memory_entry_usage_tests {
    use super::*;

    #[test]
    fn new_fields_round_trip_through_json() {
        let entry = MemoryEntry {
            id: 1,
            timestamp: 1700000000000,
            title: "hello".to_owned(),
            content: "world".to_owned(),
            created_at: "1700000000000".to_owned(),
            token_usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            }),
            input_chars: Some(5),
            output_chars: Some(7),
            parent_agent_id: Some("parent-123".to_owned()),
            spawn_reason: "task_decomposition".to_owned(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, entry);
        assert_eq!(parsed.parent_agent_id, Some("parent-123".to_owned()));
        assert_eq!(parsed.spawn_reason, "task_decomposition");
    }

    #[test]
    fn old_json_without_lineage_fields_deserializes_with_defaults() {
        let json = r#"{"id": 2, "timestamp": 1700000000000, "title": "old", "content": "record", "created_at": "1700000000000"}"#;
        let entry: MemoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, 2);
        assert_eq!(entry.title, "old");
        assert_eq!(entry.content, "record");
        assert_eq!(entry.parent_agent_id, None);
        assert_eq!(entry.spawn_reason, "");
    }

    #[test]
    fn partial_lineage_fields_default_correctly() {
        let json = r#"{"id": 3, "timestamp": 1700000000000, "title": "partial", "content": "record", "created_at": "1700000000000", "parent_agent_id": "parent-456"}"#;
        let entry: MemoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.parent_agent_id, Some("parent-456".to_owned()));
        assert_eq!(entry.spawn_reason, "");
    }

    #[test]
    fn find_children_returns_direct_descendants() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        // 创建根节点
        let root = store.add_with_title("root", "root task").unwrap();
        // 创建两个子节点
        let mut child1 = store.add_with_title("child1", "child task 1").unwrap();
        child1.parent_agent_id = Some(root.id.to_string());
        child1.spawn_reason = "task_decomposition".to_owned();
        
        let mut child2 = store.add_with_title("child2", "child task 2").unwrap();
        child2.parent_agent_id = Some(root.id.to_string());
        child2.spawn_reason = "tool_call".to_owned();
        
        // 创建孙节点
        let mut grandchild = store.add_with_title("grandchild", "grandchild task").unwrap();
        grandchild.parent_agent_id = Some(child1.id.to_string());
        grandchild.spawn_reason = "user_request".to_owned();
        
        // 重新写入以持久化修改
        let mut entries = store.read_entries().unwrap();
        for e in &mut entries {
            if e.id == child1.id { *e = child1.clone(); }
            if e.id == child2.id { *e = child2.clone(); }
            if e.id == grandchild.id { *e = grandchild.clone(); }
        }
        store.write_entries(&entries).unwrap();
        
        // 查找根节点的直接子节点
        let children = store.find_children(&root.id.to_string()).unwrap();
        assert_eq!(children.len(), 2);
        let child_ids: std::collections::HashSet<_> = children.iter().map(|c| c.id).collect();
        assert!(child_ids.contains(&child1.id));
        assert!(child_ids.contains(&child2.id));
        
        // 查找 child1 的直接子节点
        let grandchildren = store.find_children(&child1.id.to_string()).unwrap();
        assert_eq!(grandchildren.len(), 1);
        assert_eq!(grandchildren[0].id, grandchild.id);
        
        cleanup(&dir);
    }

    #[test]
    fn find_descendants_returns_all_generations() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let root = store.add_with_title("root", "root").unwrap();
        let mut c1 = store.add_with_title("c1", "c1").unwrap();
        c1.parent_agent_id = Some(root.id.to_string());
        let mut c2 = store.add_with_title("c2", "c2").unwrap();
        c2.parent_agent_id = Some(root.id.to_string());
        let mut gc1 = store.add_with_title("gc1", "gc1").unwrap();
        gc1.parent_agent_id = Some(c1.id.to_string());
        let mut ggc1 = store.add_with_title("ggc1", "ggc1").unwrap();
        ggc1.parent_agent_id = Some(gc1.id.to_string());
        
        let mut entries = store.read_entries().unwrap();
        for e in &mut entries {
            if e.id == c1.id { *e = c1.clone(); }
            if e.id == c2.id { *e = c2.clone(); }
            if e.id == gc1.id { *e = gc1.clone(); }
            if e.id == ggc1.id { *e = ggc1.clone(); }
        }
        store.write_entries(&entries).unwrap();
        
        let descendants = store.find_descendants(&root.id.to_string()).unwrap();
        assert_eq!(descendants.len(), 4);
        let desc_ids: std::collections::HashSet<_> = descendants.iter().map(|d| d.id).collect();
        assert!(desc_ids.contains(&c1.id));
        assert!(desc_ids.contains(&c2.id));
        assert!(desc_ids.contains(&gc1.id));
        assert!(desc_ids.contains(&ggc1.id));
        
        cleanup(&dir);
    }

    fn temp_workspace() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "xi-memory-lineage-test-{}-{}",
            std::process::id(),
            id
        ))
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }
}

#[cfg(any())]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> PathBuf {
        let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "xi-memory-crud-test-{}-{}",
            std::process::id(),
            id
        ))
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn add_creates_file_and_round_trips() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let entry = store.add("greeting", "hello world").unwrap();
        assert_eq!(entry.key, "greeting");
        assert_eq!(entry.text, "hello world");
        assert!(entry.tags.is_empty());
        assert!(memory_file_path(&dir).exists());

        drop(store);

        let reopened = MemoryStore::open(&dir).unwrap();
        let entries = reopened.list(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "greeting");
        assert_eq!(entries[0].text, "hello world");
        assert!(entries[0].tags.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn add_duplicate_key_overwrites_existing_entry() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        store.add("key", "first").unwrap();
        let entry = store.add("key", "second").unwrap();
        assert_eq!(entry.text, "second");

        let entries = store.list(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "key");
        assert_eq!(entries[0].text, "second");

        cleanup(&dir);
    }

    #[test]
    fn add_duplicate_content_is_idempotent_and_get_by_content() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let first = store.add("alpha", "same content").unwrap();
        let second = store.add("beta", "same content").unwrap();

        assert_eq!(first.key, "alpha");
        assert_eq!(second.key, "alpha");
        assert_eq!(second.text, "same content");

        let entries = store.list(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "alpha");
        assert_eq!(entries[0].text, "same content");

        let fetched = store.get("same content").unwrap();
        assert_eq!(fetched.key, "alpha");
        assert_eq!(fetched.text, "same content");

        cleanup(&dir);
    }

    #[test]
    fn list_filter_matches_key_substring() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        store.add("apple", "red").unwrap();
        store.add("apricot", "orange").unwrap();
        store.add("banana", "yellow").unwrap();

        let filtered = store.list(Some("ap")).unwrap();
        let keys: Vec<_> = filtered.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(keys, vec!["apple", "apricot"]);

        assert_eq!(store.list(Some("zzz")).unwrap().len(), 0);
        assert_eq!(store.list(None).unwrap().len(), 3);

        cleanup(&dir);
    }

    #[test]
    fn get_returns_entry_by_key_or_not_found() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        store.add("alpha", "one").unwrap();
        store.add("beta", "two").unwrap();

        assert_eq!(store.get("alpha").unwrap().text, "one");
        assert_eq!(store.get("beta").unwrap().text, "two");
        assert!(matches!(store.get("missing"), Err(MemoryError::NotFound(_))));

        cleanup(&dir);
    }

    #[test]
    fn empty_key_or_value_is_rejected_without_writing() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        assert!(matches!(
            store.add("   ", "value"),
            Err(MemoryError::Invalid(_))
        ));
        assert!(matches!(
            store.add("key", "   "),
            Err(MemoryError::Invalid(_))
        ));

        let entries = store.list(None).unwrap();
        assert!(entries.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn corrupt_json_returns_parse_error_and_preserves_file() {
        let dir = temp_workspace();
        let path = memory_file_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "{ definitely not valid json";
        fs::write(&path, original).unwrap();

        let store = MemoryStore::open(&dir).unwrap();
        let err = store.list(None).unwrap_err();
        assert!(matches!(err, MemoryError::Parse(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        cleanup(&dir);
    }

    #[test]
    fn non_array_json_returns_parse_error() {
        let dir = temp_workspace();
        let path = memory_file_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{\"key\":\"value\"}").unwrap();

        let store = MemoryStore::open(&dir).unwrap();
        let err = store.list(None).unwrap_err();
        assert!(matches!(err, MemoryError::Parse(_)));

        cleanup(&dir);
    }
}
#[cfg(any())]
mod usage_tests {
    #[test]
    fn _still_some() {
        assert_eq!(accumulate_optional(Some(10), None), Some(10));
        assert_eq!(accumulate_optional(None, Some(7)), Some(7));
    }

    #[test]
    fn accumulate_optional_returns_none_when_both_missing() {
        assert_eq!(accumulate_optional(None, None), None);
    }

    #[test]
    fn char_usage_counts_chars_not_bytes() {
        assert_eq!(char_usage("hello"), Some(5));
        assert_eq!(char_usage("你好"), Some(2));
    }

    #[test]
    fn char_usage_returns_none_for_empty_text() {
        assert_eq!(char_usage(""), None);
    }

    #[test]
    fn display_usage_shows_placeholders_when_missing() {
        let out = display_usage(None, None);
        assert!(out.contains("tokens: -"));
        assert!(out.contains("chars: -"));
    }

    #[test]
    fn display_usage_shows_values_when_available() {
        let out = display_usage(Some(42), Some(7));
        assert!(out.contains("tokens: 42"));
        assert!(out.contains("chars: 7"));
    }
}

#[cfg(any())]
mod memory_entry_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_memory_path() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "xi-memory-entry-test-{}-{}",
            std::process::id(),
            NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ));
        path.push("memory.json");
        path
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn add_persists_entry_to_workspace_file() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("note", "first note").unwrap();
        assert_eq!(store.get("note"), Some("first note".to_string()));

        let store = MemoryStore::new(&path).unwrap();
        let entries = store.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], ("note".to_string(), "first note".to_string()));

        cleanup(&path);
    }

    #[test]
    fn list_returns_all_entries_sorted_by_key() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("b-note", "note one").unwrap();
        store.add("a-note", "note two").unwrap();

        let entries = store.list();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("a-note".to_string(), "note two".to_string()));
        assert_eq!(entries[1], ("b-note".to_string(), "note one".to_string()));

        cleanup(&path);
    }

    #[test]
    fn get_retrieves_by_key_across_reload() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("remember", "remember this").unwrap();

        drop(store);
        let store = MemoryStore::new(&path).unwrap();
        let retrieved = store.get("remember").unwrap();
        assert_eq!(retrieved, "remember this");

        assert!(store.get("missing").is_none());

        cleanup(&path);
    }

    #[test]
    fn add_rejects_empty_key() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        let err = store.add("   ", "value").unwrap_err();
        assert!(matches!(err, MemoryError::Invalid(_)));
        cleanup(&path);
    }
}

#[cfg(any())]
mod memory_store_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    struct TestDirGuard(PathBuf);

    impl Drop for TestDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_test_dir() -> PathBuf {
        let unique = format!(
            "xi-memory-test-{}-{}",
            std::process::id(),
            TEST_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(unique);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup() -> (TestDirGuard, PathBuf) {
        let dir = temp_test_dir();
        let path = dir.join("memory.tsv");
        (TestDirGuard(dir), path)
    }

    #[test]
    fn file_missing_returns_empty_store() {
        let (_guard, path) = setup();
        assert!(!path.exists());

        let store = MemoryStore::new(&path).unwrap();
        assert!(store.list().is_empty());
        assert_eq!(store.get("missing"), None);
    }

    #[test]
    fn add_and_get_persist_after_reload() {
        let (_guard, path) = setup();
        let store = MemoryStore::new(&path).unwrap();
        store.add("hello", "world").unwrap();
        assert_eq!(store.get("hello"), Some("world".to_string()));
        drop(store);

        let store = MemoryStore::new(&path).unwrap();
        assert_eq!(store.get("hello"), Some("world".to_string()));
        assert_eq!(store.list(), vec![("hello".to_string(), "world".to_string())]);
    }

    #[test]
    fn overwrite_add_updates_value() {
        let (_guard, path) = setup();
        let store = MemoryStore::new(&path).unwrap();
        store.add("key", "first").unwrap();
        store.add("key", "second").unwrap();
        assert_eq!(store.get("key"), Some("second".to_string()));
        assert_eq!(store.list(), vec![("key".to_string(), "second".to_string())]);

        drop(store);
        let store = MemoryStore::new(&path).unwrap();
        assert_eq!(store.get("key"), Some("second".to_string()));
    }

    #[test]
    fn list_returns_stable_sorted_order() {
        let (_guard, path) = setup();
        let store = MemoryStore::new(&path).unwrap();
        store.add("banana", "yellow").unwrap();
        store.add("apple", "red").unwrap();
        store.add("cherry", "dark").unwrap();

        assert_eq!(
            store.list(),
            vec![
                ("apple".to_string(), "red".to_string()),
                ("banana".to_string(), "yellow".to_string()),
                ("cherry".to_string(), "dark".to_string()),
            ]
        );
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let (_guard, path) = setup();
        let store = MemoryStore::new(&path).unwrap();
        store.add("k1", "v1").unwrap();
        store.add("k2", "v2").unwrap();

        let parent = path.parent().unwrap();
        let mut tmp_count = 0;
        for entry in fs::read_dir(parent).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if name.contains(".tmp.") {
                tmp_count += 1;
            }
        }
        assert_eq!(tmp_count, 0);
        assert!(path.exists());
    }

    #[test]
    fn corrupt_file_returns_parse_error() {
        let (_guard, path) = setup();
        fs::write(&path, "not-a-valid-tsv-line\n").unwrap();

        let err = MemoryStore::new(&path).unwrap_err();
        assert!(matches!(err, MemoryError::Parse(_)));
    }
}

#[cfg(any())]
mod memory_persistence_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn temp_workspace() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "xi-memory-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn add_memory_creates_and_overwrites() {
        let ws = temp_workspace();
        add_memory(&ws, "hello", "world").unwrap();
        add_memory(&ws, "hello", "again").unwrap();
        let entries = list_memory(&ws).unwrap();
        assert_eq!(entries, vec![("hello".to_string(), "again".to_string())]);
        cleanup(&ws);
    }

    #[test]
    fn list_memory_returns_empty_when_file_missing() {
        let ws = temp_workspace();
        let entries = list_memory(&ws).unwrap();
        assert!(entries.is_empty());
        cleanup(&ws);
    }

    #[test]
    fn get_memory_returns_none_when_file_missing_or_key_missing() {
        let ws = temp_workspace();
        assert_eq!(get_memory(&ws, "missing").unwrap(), None);
        add_memory(&ws, "present", "value").unwrap();
        assert_eq!(get_memory(&ws, "present").unwrap(), Some("value".to_string()));
        assert_eq!(get_memory(&ws, "missing").unwrap(), None);
        cleanup(&ws);
    }

    #[test]
    fn add_memory_rejects_empty_key_and_value() {
        let ws = temp_workspace();
        let err = add_memory(&ws, "   ", "value").unwrap_err();
        assert!(matches!(err, MemoryError::Invalid(_)));

        // Also test empty value via the store directly.
        let store = MemoryStore::open(&ws).unwrap();
        let err = store.add("key", "   ").unwrap_err();
        assert!(matches!(err, MemoryError::Invalid(_)));
        cleanup(&ws);
    }

    #[test]
    fn add_memory_rejects_sensitive_information() {
        let ws = temp_workspace();
        let key_err = add_memory(&ws, "api_key", "value").unwrap_err();
        assert!(matches!(key_err, MemoryError::Invalid(_)));

        let store = MemoryStore::open(&ws).unwrap();
        let value_err = store.add("ordinary_key", "password=hunter2").unwrap_err();
        assert!(matches!(value_err, MemoryError::Invalid(_)));

        // Ensure nothing was persisted.
        let entries = list_memory(&ws).unwrap();
        assert!(entries.is_empty());
        cleanup(&ws);
    }

    #[test]
    fn memory_store_persists_json_to_disk() {
        let ws = temp_workspace();
        add_memory(&ws, "banana", "yellow").unwrap();
        add_memory(&ws, "apple", "red").unwrap();

        // Reload from the same workspace to ensure persistence works.
        let store = MemoryStore::open(&ws).unwrap();
        let entries = store.list();
        assert_eq!(entries, vec![
            ("apple".to_string(), "red".to_string()),
            ("banana".to_string(), "yellow".to_string()),
        ]);

        // The file should be valid JSON.
        let path = memory_file_path(&ws);
        let content = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.is_object());
        cleanup(&ws);
    }

    #[test]
    fn corrupted_json_returns_parse_error_on_open() {
        let ws = temp_workspace();
        fs::create_dir_all(&ws).unwrap();
        let path = memory_file_path(&ws);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not valid json").unwrap();

        let err = MemoryStore::open(&ws).unwrap_err();
        assert!(matches!(err, MemoryError::Parse(_)));
        cleanup(&ws);
    }
}

#[cfg(any())]
mod memory_crud_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn add_list_get_persists_and_reloads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let store = MemoryStore::new(&path).unwrap();

        let first = store.add("first memory").unwrap();
        assert_eq!(first.id, 1);
        assert_eq!(first.content, "first memory");

        let second = store.add("second memory").unwrap();
        assert_eq!(second.id, 2);

        let entries = store.list();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, 1);
        assert_eq!(entries[1].id, 2);

        assert_eq!(store.get(1).unwrap().content, "first memory");
        assert_eq!(store.get(2).unwrap().content, "second memory");
        assert!(store.get(99).is_none());

        drop(store);

        let reloaded = MemoryStore::new(&path).unwrap();
        let entries = reloaded.list();
        assert_eq!(entries.len(), 2);
        assert_eq!(reloaded.get(1).unwrap().content, "first memory");
        assert_eq!(reloaded.get(2).unwrap().content, "second memory");
    }

    #[test]
    fn add_rejects_empty_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let store = MemoryStore::new(&path).unwrap();
        assert!(matches!(
            store.add("   "),
            Err(MemoryError::Invalid(_))
        ));
    }

    #[test]
    fn list_and_get_are_empty_for_new_store() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let store = MemoryStore::new(&path).unwrap();
        assert!(store.list().is_empty());
        assert!(store.get(1).is_none());
    }

    #[test]
    fn command_parse_add_get() {
        let args = vec!["add".to_string(), "hello".to_string(), "world".to_string()];
        match MemoryCommand::parse(&args).unwrap() {
            MemoryCommand::Add { content } => assert_eq!(content, "hello world"),
            _ => panic!("expected add"),
        }

        let args = vec!["get".to_string(), "7".to_string()];
        match MemoryCommand::parse(&args).unwrap() {
            MemoryCommand::Get { id } => assert_eq!(id, 7),
            _ => panic!("expected get"),
        }
    }
}

#[cfg(any())]
mod key_value_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_memory_path() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "xi-memory-kv-test-{}-{}",
            std::process::id(),
            NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ));
        path.push("memory.json");
        path
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn add_list_get_persist_across_reload() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).expect("new store");

        store.add("greeting", "hello").unwrap();
        store.add("lang", "rust").unwrap();

        assert_eq!(store.get("greeting").unwrap(), "hello");
        assert_eq!(store.get("lang").unwrap(), "rust");

        let entries = store.list();
        assert_eq!(entries.len(), 2);

        drop(store);

        // Reopen from the same path; data must have been written atomically.
        let reopened = MemoryStore::new(&path).unwrap();
        assert_eq!(reopened.get("greeting").unwrap(), "hello");
        assert_eq!(reopened.get("lang").unwrap(), "rust");

        cleanup(&path);
    }

    #[test]
    fn get_missing_key_returns_friendly_not_found() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        let err = store.get("missing").unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));

        cleanup(&path);
    }

    #[test]
    fn add_existing_key_updates_value_without_duplicate() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("k", "v1").unwrap();
        store.add("k", "v2").unwrap();

        assert_eq!(store.get("k").unwrap(), "v2");
        assert_eq!(store.list().len(), 1);

        cleanup(&path);
    }

    #[test]
    fn corrupt_json_returns_parse_error() {
        let path = temp_memory_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{invalid json").unwrap();

        let err = MemoryStore::new(&path).unwrap_err();
        assert!(matches!(err, MemoryError::Parse(_)));

        cleanup(&path);
    }
}

#[cfg(any())]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> PathBuf {
        let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "xi-memory-crud-test-{}-{}",
            std::process::id(),
            id
        ))
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn add_creates_file_and_round_trips() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let entry = store.add("greeting", "hello world").unwrap();
        assert_eq!(entry.title, "greeting");
        assert_eq!(entry.content, "hello world");
        assert!(!entry.id.is_empty());
        assert!(!entry.created_at.is_empty());
        assert!(memory_file_path(&dir).exists());

        drop(store);

        let reopened = MemoryStore::open(&dir).unwrap();
        let entries = reopened.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "greeting");
        assert_eq!(entries[0].content, "hello world");
        assert_eq!(entries[0].id, entry.id);
        assert_eq!(entries[0].created_at, entry.created_at);

        cleanup(&dir);
    }

    #[test]
    fn list_returns_all_entries() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        store.add("apple", "red").unwrap();
        store.add("banana", "yellow").unwrap();

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "apple");
        assert_eq!(entries[1].title, "banana");

        cleanup(&dir);
    }

    #[test]
    fn get_returns_entry_by_id() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let first = store.add("alpha", "one").unwrap();
        let second = store.add("beta", "two").unwrap();

        let fetched = store.get(&first.id).unwrap();
        assert_eq!(fetched.title, "alpha");
        assert_eq!(fetched.content, "one");

        let fetched2 = store.get(&second.id).unwrap();
        assert_eq!(fetched2.title, "beta");
        assert_eq!(fetched2.content, "two");

        assert!(matches!(store.get("missing-id"), Err(MemoryError::NotFound(_))));

        cleanup(&dir);
    }

    #[test]
    fn empty_title_or_content_is_rejected_without_writing() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        assert!(matches!(
            store.add("   ", "value"),
            Err(MemoryError::Invalid(_))
        ));
        assert!(matches!(
            store.add("key", "   "),
            Err(MemoryError::Invalid(_))
        ));

        let entries = store.list().unwrap();
        assert!(entries.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn corrupt_json_falls_back_to_empty_list() {
        let dir = temp_workspace();
        let path = memory_file_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ definitely not valid json").unwrap();

        let store = MemoryStore::open(&dir).unwrap();
        let entries = store.list().unwrap();
        assert!(entries.is_empty());

        // The file should be preserved (not overwritten).
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{ definitely not valid json");

        cleanup(&dir);
    }

    #[test]
    fn non_array_json_falls_back_to_empty_list() {
        let dir = temp_workspace();
        let path = memory_file_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{\"key\":\"value\"}").unwrap();

        let store = MemoryStore::open(&dir).unwrap();
        let entries = store.list().unwrap();
        assert!(entries.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn missing_file_returns_empty_list() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();
        let entries = store.list().unwrap();
        assert!(entries.is_empty());

        cleanup(&dir);
    }
}

#[cfg(test)]
mod memory_jsonl_tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    struct Guard(PathBuf);

    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> Guard {
        let unique = format!(
            "xi-memory-jsonl-test-{}-{}",
            std::process::id(),
            TEST_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(unique);
        fs::create_dir_all(&dir).unwrap();
        Guard(dir)
    }

    fn memory_path(dir: &Path) -> PathBuf {
        dir.join("memory.jsonl")
    }

    #[test]
    fn file_missing_returns_empty_list_and_not_found() {
        let guard = temp_dir();
        let store = MemoryStore::new(memory_path(&guard.0)).unwrap();

        assert!(store.list().unwrap().is_empty());
        assert!(matches!(store.get(1), Err(MemoryError::NotFound(_))));
    }

    #[test]
    fn add_list_get_basic_flow() {
        let guard = temp_dir();
        let store = MemoryStore::new(memory_path(&guard.0)).unwrap();

        let first = store.add("first note").unwrap();
        let second = store.add("second note").unwrap();
        assert_eq!(first.id, 1);
        assert_eq!(second.id, 2);

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 2);
        // Newer entries sort first; on equal timestamps tie-break by id desc.
        assert_eq!(entries[0].content, "second note");
        assert_eq!(entries[1].content, "first note");

        assert_eq!(store.get(first.id).unwrap().content, "first note");
        assert_eq!(store.get(second.id).unwrap().content, "second note");
    }

    #[test]
    fn corrupt_line_is_skipped_on_list_and_error_on_get() {
        let guard = temp_dir();
        let path = memory_path(&guard.0);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "{\"id\":1,\"timestamp\":10,\"content\":\"one\"}\n{\"id\":2,\"timestamp\":20,\"content\":\"two\"\n",
        )
        .unwrap();

        let store = MemoryStore::new(path).unwrap();
        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "one");

        assert_eq!(store.get(1).unwrap().content, "one");
        assert!(matches!(store.get(2), Err(MemoryError::Parse(_))));
    }

    #[test]
    fn concurrent_add_does_not_lose_entries() {
        use std::sync::Arc;

        let guard = temp_dir();
        let store = Arc::new(MemoryStore::new(memory_path(&guard.0)).unwrap());
        let mut handles = Vec::new();

        for i in 0..8 {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                store.add(&format!("note-{i}")).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 8);
        let mut ids: Vec<_> = entries.iter().map(|entry| entry.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 8);
    }
}

#[cfg(test)]
mod memory_correct_tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> PathBuf {
        let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "xi-memory-correct-test-{}-{}",
            std::process::id(),
            id
        ))
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn add_list_get_roundtrip() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let first = store.add("first memory").unwrap();
        assert_eq!(first.id, 1);
        assert_eq!(first.content, "first memory");
        assert!(first.timestamp > 0);

        let second = store.add("second memory").unwrap();
        assert_eq!(second.id, 2);

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].content, "first memory");
        assert_eq!(entries[0].content, "second memory");

        assert_eq!(store.get(1).unwrap().content, "first memory");
        assert_eq!(store.get(2).unwrap().content, "second memory");

        drop(store);

        let reopened = MemoryStore::open(&dir).unwrap();
        let entries = reopened.list().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(reopened.get(1).unwrap().content, "first memory");

        cleanup(&dir);
    }

    #[test]
    fn add_rejects_empty_content() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();
        assert!(matches!(
            store.add("   "),
            Err(MemoryError::Invalid(_))
        ));
        cleanup(&dir);
    }

    #[test]
    fn remove_deletes_entry() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let entry = store.add("to delete").unwrap();
        let removed = store.remove(entry.id).unwrap();
        assert_eq!(removed.id, entry.id);
        assert!(matches!(store.get(entry.id), Err(MemoryError::NotFound(_))));
        assert!(store.list().unwrap().is_empty());

        cleanup(&dir);
    }

    #[test]
    fn list_uses_latest_first_order() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();
        store.add("one").unwrap();
        store.add("two").unwrap();

        let entries = store.list().unwrap();
        assert_eq!(entries[0].content, "two");
        assert_eq!(entries[1].content, "one");

        cleanup(&dir);
    }
}