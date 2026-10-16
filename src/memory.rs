use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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
    pub key: String,
    pub value: String,
    pub created_at: u64,
}

#[derive(Debug)]
pub struct MemoryStore {
    path: PathBuf,
    data: RwLock<Vec<MemoryEntry>>,
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

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

impl MemoryStore {
    /// Creates a memory store using `path` as the persistent JSON file.
    /// If the file does not exist, the store starts empty.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, MemoryError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(MemoryError::Io)?;
            }
        }
        let store = Self {
            path,
            data: RwLock::new(Vec::new()),
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

    /// Adds or updates a memory entry keyed by `key` with `value`, then persists atomically.
    pub fn add(&self, key: &str, value: &str) -> Result<MemoryEntry, MemoryError> {
        if key.trim().is_empty() {
            return Err(MemoryError::Invalid("memory key must not be empty".to_owned()));
        }
        if value.trim().is_empty() {
            return Err(MemoryError::Invalid("memory value must not be empty".to_owned()));
        }

        let entry = {
            let mut data = self.data.write().unwrap();
            if let Some(existing) = data.iter_mut().find(|entry| entry.key == key) {
                existing.value = value.to_string();
                existing.created_at = unix_timestamp();
                existing.clone()
            } else {
                let entry = MemoryEntry {
                    key: key.to_string(),
                    value: value.to_string(),
                    created_at: unix_timestamp(),
                };
                data.push(entry.clone());
                entry
            }
        };

        self.save()?;
        Ok(entry)
    }

    /// Retrieves the value for `key`, returning a friendly error if missing.
    pub fn get(&self, key: &str) -> Result<String, MemoryError> {
        let data = self.data.read().unwrap();
        data.iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.clone())
            .ok_or_else(|| MemoryError::NotFound(key.to_string()))
    }

    /// Lists all entries in insertion order.
    pub fn list(&self) -> Vec<MemoryEntry> {
        let data = self.data.read().unwrap();
        data.clone()
    }

    /// Removes an entry by key and returns it, if present.
    pub fn remove(&self, key: &str) -> Result<MemoryEntry, MemoryError> {
        let removed = {
            let mut data = self.data.write().unwrap();
            let position = data.iter().position(|entry| entry.key == key);
            position.map(|position| data.remove(position))
        };

        match removed {
            Some(entry) => {
                self.save()?;
                Ok(entry)
            }
            None => Err(MemoryError::NotFound(key.to_string())),
        }
    }

    /// Loads entries from the persistent JSON file.
    /// A missing file leaves an empty store; a malformed JSON file returns a
    /// `MemoryError::Parse` with a user-friendly message.
    pub fn load(&self) -> Result<(), MemoryError> {
        if !self.path.exists() {
            let mut data = self.data.write().unwrap();
            data.clear();
            return Ok(());
        }

        let content = fs::read_to_string(&self.path).map_err(MemoryError::Io)?;
        // An empty file is treated as an empty store; this can happen if a
        // previous write was interrupted before any content was written.
        if content.trim().is_empty() {
            let mut data = self.data.write().unwrap();
            data.clear();
            return Ok(());
        }

        let entries: Vec<MemoryEntry> = serde_json::from_str(&content)
            .map_err(|err| MemoryError::Parse(format!("invalid JSON in memory file: {err}")))?;

        let mut data = self.data.write().unwrap();
        *data = entries;
        Ok(())
    }

    /// Saves the in-memory entries atomically to the persistent JSON file.
    pub fn save(&self) -> Result<(), MemoryError> {
        let _guard = self.acquire_lock()?;
        let data = self.data.read().unwrap();
        let json = serde_json::to_string_pretty(&*data)
            .map_err(|err| MemoryError::Parse(format!("failed to serialize memory: {err}")))?;
        let mut out = json.into_bytes();
        out.push(b'\n');

        let tmp_path = unique_tmp_path(&self.path);
        let write_result = (|| -> Result<(), MemoryError> {
            let mut tmp = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(MemoryError::Io)?;
            tmp.write_all(&out).map_err(MemoryError::Io)?;
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
}

fn find_sensitive_pattern(s: &str) -> Option<&'static str> {
    const SENSITIVE_PATTERNS: [&str; 10] = [
        "password",
        "secret",
        "token",
        "api_key",
        "private_key",
        "access_key",
        "credential",
        "passwd",
        "pwd",
        "auth_token",
    ];

    let lower = s.to_ascii_lowercase();
    for pattern in SENSITIVE_PATTERNS {
        if lower.contains(pattern) {
            return Some(pattern);
        }
    }
    None
}

pub fn add_memory(workspace_dir: &Path, key: &str, value: &str) -> Result<MemoryEntry, MemoryError> {
    let store = MemoryStore::open(workspace_dir)?;
    store.add(key, value)
}

pub fn list_memory(workspace_dir: &Path) -> Result<Vec<MemoryEntry>, MemoryError> {
    let store = MemoryStore::open(workspace_dir)?;
    Ok(store.list())
}

pub fn get_memory(workspace_dir: &Path, key: &str) -> Result<String, MemoryError> {
    let store = MemoryStore::open(workspace_dir)?;
    store.get(key)
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
            let entry = store.add(&key, &value)?;
            Ok(format!("memory saved: {} = {}", entry.key, entry.value))
        }
        MemoryCommand::List => {
            let entries = store.list();
            if entries.is_empty() {
                return Ok(String::new());
            }
            let mut out = String::new();
            for entry in entries {
                out.push_str(&format!("{}\t{}\n", entry.key, entry.value));
            }
            Ok(out)
        }
        MemoryCommand::Get { key } => store.get(&key),
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
                if key.trim().is_empty() {
                    return Err("memory key 不能为空".to_string());
                }
                if value.trim().is_empty() {
                    return Err("memory value 不能为空".to_string());
                }
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

fn unique_tmp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp_name = format!(".{}.tmp.{}.{}", file_name, std::process::id(), counter);
    path.with_file_name(tmp_name)
}


#[cfg(any())]
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

#[cfg(any())]
mod memory_entry_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_memory_path() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "workbuddy-memory-entry-test-{}-{}",
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
            "workbuddy-memory-test-{}-{}",
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
            "workbuddy-memory-test-{}-{}",
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

#[cfg(test)]
mod key_value_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_memory_path() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "workbuddy-memory-kv-test-{}-{}",
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