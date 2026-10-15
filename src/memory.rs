use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum MemoryError {
    Io(std::io::Error),
    NotFound(String),
    Locked,
    Invalid(String),
    Parse(String),
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::Io(err) => write!(f, "memory io error: {err}"),
            MemoryError::NotFound(key) => write!(f, "memory entry not found: {key}"),
            MemoryError::Locked => write!(f, "memory is locked by another writer"),
            MemoryError::Invalid(msg) => write!(f, "invalid memory entry: {msg}"),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: u64,
    pub text: String,
    pub created_at: Option<String>,
}

pub struct MemoryStore {
    path: PathBuf,
    data: RwLock<HashMap<String, String>>,
}

struct LockGuard {
    path: PathBuf,
    file: Option<File>,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

impl MemoryStore {
    /// Creates a memory store using `path` as the persistent JSON file.
    /// If the file already exists, its contents are loaded into memory.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, MemoryError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(MemoryError::Io)?;
            }
        }
        let store = Self {
            path,
            data: RwLock::new(HashMap::new()),
        };
        store.load()?;
        Ok(store)
    }

    /// Returns the default path `~/.workbuddy/memory.json`.
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".workbuddy").join("memory.json")
    }

    /// Opens the default memory store at `~/.workbuddy/memory.json`.
    pub fn open_default() -> Result<Self, MemoryError> {
        Self::new(Self::default_path())
    }

    /// Opens a memory store rooted at `workspace_dir` (kept for compatibility).
    pub fn open(workspace_dir: impl AsRef<Path>) -> Result<Self, MemoryError> {
        Self::new(memory_file_path(workspace_dir.as_ref()))
    }

    /// Adds or updates a key-value pair.
    pub fn add(&self, key: &str, value: &str) -> Result<(), MemoryError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(MemoryError::Invalid("key must not be empty".to_owned()));
        }
        {
            let mut data = self.data.write().unwrap();
            data.insert(key.to_string(), value.to_string());
        }
        self.save()
    }

    /// Sets a key-value pair. This is an alias for `add`.
    pub fn set(&self, key: &str, value: &str) -> Result<(), MemoryError> {
        self.add(key, value)
    }

    /// Gets a value by key.
    pub fn get(&self, key: &str) -> Option<String> {
        let data = self.data.read().unwrap();
        data.get(key).cloned()
    }

    /// Lists all key-value pairs sorted by key.
    pub fn list(&self) -> Vec<(String, String)> {
        let data = self.data.read().unwrap();
        let mut entries: Vec<_> = data.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// Removes a key and returns the previous value, if any.
    pub fn remove(&self, key: &str) -> Option<String> {
        let prev = {
            let mut data = self.data.write().unwrap();
            data.remove(key)
        };
        if prev.is_some() {
            let _ = self.save();
        }
        prev
    }

    /// Loads entries from the JSON file into memory.
    /// If the file does not exist, the in-memory map is cleared.
    /// If the file exists but is corrupt, it is backed up and treated as empty.
    pub fn load(&self) -> Result<(), MemoryError> {
        let _guard = self.acquire_lock()?;
        if !self.path.exists() {
            fs::write(&self.path, "{}").map_err(MemoryError::Io)?;
            let mut data = self.data.write().unwrap();
            data.clear();
            return Ok(());
        }
        let content = fs::read_to_string(&self.path).map_err(MemoryError::Io)?;
        match serde_json::from_str::<HashMap<String, String>>(&content) {
            Ok(entries) => {
                let mut data = self.data.write().unwrap();
                *data = entries;
                Ok(())
            }
            Err(_err) => {
                // Do not panic on corrupt JSON; rename the bad file and start fresh.
                let backup = self.corrupt_backup_path();
                let _ = fs::rename(&self.path, &backup);
                fs::write(&self.path, "{}").map_err(MemoryError::Io)?;
                let mut data = self.data.write().unwrap();
                data.clear();
                Ok(())
            }
        }
    }

    /// Saves the in-memory map atomically to the JSON file.
    pub fn save(&self) -> Result<(), MemoryError> {
        let _guard = self.acquire_lock()?;
        let data = self.data.read().unwrap();
        let serialized = serde_json::to_string_pretty(&*data)
            .map_err(|err| MemoryError::Parse(err.to_string()))?;

        let tmp_path = unique_tmp_path(&self.path);
        let write_result = (|| -> Result<(), MemoryError> {
            let mut tmp = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(MemoryError::Io)?;
            tmp.write_all(serialized.as_bytes()).map_err(MemoryError::Io)?;
            tmp.sync_all().map_err(MemoryError::Io)?;
            Ok(())
        })();

        if let Err(err) = write_result {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }

        if let Err(err) = fs::rename(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(MemoryError::Io(err));
        }

        Ok(())
    }

    fn acquire_lock(&self) -> Result<LockGuard, MemoryError> {
        let lock_path = PathBuf::from(format!("{}.lock", self.path.display()));
        let mut attempts = 0;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&lock_path) {
                Ok(file) => return Ok(LockGuard { path: lock_path, file: Some(file) }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempts >= 200 {
                        return Err(MemoryError::Locked);
                    }
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(MemoryError::Io(err)),
            }
        }
    }

    fn corrupt_backup_path(&self) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let file_name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let backup_name = format!("{}.corrupt-{}", file_name, timestamp);
        self.path.with_file_name(backup_name)
    }
}

pub enum MemoryCommand {
    Add { key: String, value: String },
    List,
    Get { key: String },
}

pub fn run_memory_command(
    store: &MemoryStore,
    command: MemoryCommand,
) -> Result<String, MemoryError> {
    match command {
        MemoryCommand::Add { key, value } => {
            store.add(&key, &value)?;
            Ok(format!("memory saved: {key} = {value}"))
        }
        MemoryCommand::List => {
            let entries = store.list();
            if entries.is_empty() {
                return Ok(String::new());
            }
            let mut out = String::new();
            for (key, value) in entries {
                out.push_str(&format!("{}\t{}\n", key, value));
            }
            Ok(out)
        }
        MemoryCommand::Get { key } => match store.get(&key) {
            Some(value) => Ok(value),
            None => Err(MemoryError::NotFound(key)),
        },
    }
}

impl MemoryCommand {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        if args.is_empty() {
            return Err("用法: memory add <key> <value> | list | get <key>".to_string());
        }
        match args[0].as_str() {
            "add" => {
                if args.len() < 3 {
                    return Err("用法: memory add <key> <value>".to_string());
                }
                let key = args[1].clone();
                let value = args[2..].join(" ");
                Ok(MemoryCommand::Add { key, value })
            }
            "list" => {
                if args.len() != 1 {
                    return Err("用法: memory list".to_string());
                }
                Ok(MemoryCommand::List)
            }
            "get" => {
                if args.len() != 2 {
                    return Err("用法: memory get <key>".to_string());
                }
                let key = args[1].clone();
                Ok(MemoryCommand::Get { key })
            }
            other => Err(format!("未知记忆子命令: {other}")),
        }
    }
}

pub fn memory_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".workbuddy").join("memory.json")
}

fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn unique_tmp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp_name = format!(".{}.tmp.{}.{}", file_name, std::process::id(), counter);
    path.with_file_name(tmp_name)
}

pub fn list_runs(workspace_dir: &std::path::Path) -> anyhow::Result<Vec<crate::workspace::RunRecord>> {
    if !workspace_dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(workspace_dir)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if let Ok(run) = get_run(workspace_dir, &id) {
            runs.push(run);
        }
    }
    runs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(runs)
}

pub fn get_run(workspace_dir: &std::path::Path, id: &str) -> anyhow::Result<crate::workspace::RunRecord> {
    let run_dir = workspace_dir.join(id);
    if !run_dir.exists() {
        return Err(anyhow::anyhow!("run {id} 不存在"));
    }
    let ws = crate::workspace::Workspace::open(workspace_dir, id)?;
    ws.load_record()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_memory_path() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "workbuddy-memory-test-{}-{}",
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
    fn new_initializes_parent_directory_and_empty_store() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        assert!(path.parent().unwrap().exists());
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(serde_json::from_str::<HashMap<String, String>>(&content).unwrap().is_empty());
        assert!(store.get("missing").is_none());
        assert!(store.list().is_empty());

        cleanup(&path);
    }

    #[test]
    fn add_persists_entry_and_get_returns_it() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        store.add("hello", "world").unwrap();
        assert_eq!(store.get("hello"), Some("world".to_string()));
        drop(store);

        let store = MemoryStore::new(&path).unwrap();
        assert_eq!(store.get("hello"), Some("world".to_string()));

        cleanup(&path);
    }

    #[test]
    fn list_returns_all_entries_sorted_by_key() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("banana", "yellow").unwrap();
        store.add("apple", "red").unwrap();

        let entries = store.list();
        assert_eq!(entries, vec![
            ("apple".to_string(), "red".to_string()),
            ("banana".to_string(), "yellow".to_string()),
        ]);

        cleanup(&path);
    }

    #[test]
    fn remove_deletes_key_and_returns_old_value() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("key", "value").unwrap();
        let old = store.remove("key");
        assert_eq!(old, Some("value".to_string()));
        assert!(store.get("key").is_none());

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

    #[test]
    fn concurrent_adds_are_persisted_and_file_is_valid_json() {
        let path = temp_memory_path();
        let store = Arc::new(MemoryStore::new(&path).unwrap());

        let mut handles = Vec::new();
        for i in 0..10 {
            let store = Arc::clone(&store);
            let key = format!("key{}", i);
            let value = format!("value{}", i);
            handles.push(std::thread::spawn(move || {
                store.add(&key, &value).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(store.get("key5"), Some("value5".to_string()));

        drop(store);
        let store = MemoryStore::new(&path).unwrap();
        let entries = store.list();
        assert_eq!(entries.len(), 10);
        assert_eq!(store.get("key0"), Some("value0".to_string()));

        cleanup(&path);
    }

    #[test]
    fn command_parse_add_missing_value_is_error() {
        let args = vec!["add".to_string(), "key".to_string()];
        assert!(MemoryCommand::parse(&args).is_err());
    }

    #[test]
    fn command_parse_get_invalid_args_is_error() {
        let args = vec!["get".to_string()];
        assert!(MemoryCommand::parse(&args).is_err());
    }

    #[test]
    fn set_adds_new_key_and_overwrites_existing_key() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.set("new", "first").unwrap();
        assert_eq!(store.get("new"), Some("first".to_string()));

        store.set("new", "second").unwrap();
        assert_eq!(store.get("new"), Some("second".to_string()));
        assert_eq!(store.list(), vec![("new".to_string(), "second".to_string())]);

        store.set("another", "value").unwrap();
        let entries = store.list();
        assert_eq!(entries.len(), 2);
        assert_eq!(store.get("another"), Some("value".to_string()));

        cleanup(&path);
    }

    #[test]
    fn get_missing_key_returns_none_after_reload() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        assert!(store.get("missing").is_none());

        store.add("present", "value").unwrap();
        drop(store);

        let store = MemoryStore::new(&path).unwrap();
        assert!(store.get("missing").is_none());
        assert_eq!(store.get("present"), Some("value".to_string()));

        cleanup(&path);
    }

    #[test]
    fn remove_persists_and_missing_remove_returns_none() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("to-remove", "value").unwrap();
        assert_eq!(store.remove("to-remove"), Some("value".to_string()));
        assert_eq!(store.remove("to-remove"), None);

        drop(store);

        let store = MemoryStore::new(&path).unwrap();
        assert!(store.get("to-remove").is_none());
        assert!(store.list().is_empty());

        cleanup(&path);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_store_starts_empty() {
        let path = temp_memory_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not valid json").unwrap();

        let store = MemoryStore::new(&path).unwrap();
        assert!(store.list().is_empty());
        assert!(store.get("any").is_none());
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(serde_json::from_str::<HashMap<String, String>>(&content).unwrap().is_empty());

        let parent = path.parent().unwrap();
        let mut backup_count = 0;
        for entry in fs::read_dir(parent).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("memory.json.corrupt-") {
                backup_count += 1;
            }
        }
        assert_eq!(backup_count, 1);

        cleanup(&path);
    }
}

/// 累计可选 token/字符用量。多次调用结果传入各自 Option<u64>，缺失视为 None。
/// 如果两个值都缺失，返回 None；否则返回 Some(总和)。
pub fn accumulate_optional(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

/// 统计字符串中的字符数（按 Rust char 计数，不是字节数）。
pub fn char_usage(content: &str) -> Option<u64> {
    if content.is_empty() {
        None
    } else {
        Some(content.chars().count() as u64)
    }
}

/// 将可选用量渲染为展示字符串。缺失时显示占位符 "-"。
pub fn display_usage(token_usage: Option<u64>, char_usage: Option<u64>) -> String {
    let token = token_usage.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    let chars = char_usage.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    format!("tokens: {token}\nchars: {chars}")
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn accumulate_optional_adds_two_present_values() {
        assert_eq!(accumulate_optional(Some(10), Some(5)), Some(15));
    }

    #[test]
    fn accumulate_optional_treats_missing_as_zero_but_still_some() {
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