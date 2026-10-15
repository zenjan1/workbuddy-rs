use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
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
    pub id: String,
    pub content: String,
    pub created_at: String,
}

pub struct MemoryStore {
    path: PathBuf,
    lock: Mutex<()>,
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
    /// Creates a key-value memory store using `path` as the persistent JSON file.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, MemoryError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(MemoryError::Io)?;
            }
        }
        Ok(Self {
            path,
            lock: Mutex::new(()),
        })
    }

    /// Opens the default memory store under `<workspace_dir>/.workbuddy/memory.json`.
    pub fn open(workspace_dir: impl AsRef<Path>) -> Result<Self, MemoryError> {
        Self::new(memory_file_path(workspace_dir.as_ref()))
    }

    /// Adds or overwrites a key-value pair in the persistent store.
    pub fn add(&self, key: &str, value: &str) -> Result<(), MemoryError> {
        validate_key(key)?;
        let _guard = self.acquire_lock()?;
        let mut entries = self.read_entries_locked()?;
        entries.insert(key.to_string(), value.to_string());
        self.write_entries_atomic(&entries)?;
        Ok(())
    }

    /// Gets the value for `key`, if present.
    pub fn get(&self, key: &str) -> Result<Option<String>, MemoryError> {
        let _guard = self.acquire_lock()?;
        let entries = self.read_entries_locked()?;
        Ok(entries.get(key).cloned())
    }

    /// Lists all key-value pairs sorted by key.
    pub fn list(&self) -> Result<Vec<(String, String)>, MemoryError> {
        let _guard = self.acquire_lock()?;
        let entries = self.read_entries_locked()?;
        let mut pairs: Vec<_> = entries.into_iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(pairs)
    }

    fn acquire_lock(&self) -> Result<LockGuard, MemoryError> {
        let lock_path = PathBuf::from(format!("{}.lock", self.path.display()));
        let mut attempts = 0;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&lock_path) {
                Ok(file) => return Ok(LockGuard { path: lock_path, file: Some(file) }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempts >= 20 {
                        return Err(MemoryError::Locked);
                    }
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(MemoryError::Io(err)),
            }
        }
    }

    fn read_entries_locked(&self) -> Result<HashMap<String, String>, MemoryError> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }

        let content = fs::read_to_string(&self.path).map_err(MemoryError::Io)?;
        match serde_json::from_str::<HashMap<String, String>>(&content) {
            Ok(entries) => Ok(entries),
            Err(_err) => {
                // Do not panic on corrupt JSON; rename the bad file and start fresh.
                let backup = self.corrupt_backup_path();
                let _ = fs::rename(&self.path, &backup);
                Ok(HashMap::new())
            }
        }
    }

    fn write_entries_atomic(&self, entries: &HashMap<String, String>) -> Result<(), MemoryError> {
        let tmp_path = unique_tmp_path(&self.path);
        let serialized = serde_json::to_string_pretty(entries)
            .map_err(|err| MemoryError::Parse(err.to_string()))?;

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
            Ok(format!("added {key}"))
        }
        MemoryCommand::List => {
            let pairs = store.list()?;
            let mut out = String::new();
            for (key, value) in pairs {
                out.push_str(&key);
                out.push('\t');
                out.push_str(&value);
                out.push('\n');
            }
            Ok(out)
        }
        MemoryCommand::Get { key } => match store.get(&key)? {
            Some(value) => Ok(format!("{key}\t{value}")),
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
                Ok(MemoryCommand::Get { key: args[1].clone() })
            }
            other => Err(format!("未知记忆子命令: {other}")),
        }
    }
}

pub fn memory_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".workbuddy").join("memory.json")
}

fn validate_key(key: &str) -> Result<(), MemoryError> {
    if key.trim().is_empty() {
        return Err(MemoryError::Invalid("key must not be empty".to_owned()));
    }
    Ok(())
}

fn validate_content(content: &str) -> Result<(), MemoryError> {
    if content.is_empty() {
        return Err(MemoryError::Invalid(
            "content must not be empty".to_owned(),
        ));
    }
    Ok(())
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
        assert!(store.list().unwrap().is_empty());
        assert!(store.get("missing").unwrap().is_none());

        cleanup(&path);
    }

    #[test]
    fn add_persists_key_value_and_get_returns_it() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        store.add("theme", "dark").unwrap();
        drop(store);

        let store = MemoryStore::new(&path).unwrap();
        assert_eq!(store.get("theme").unwrap(), Some("dark".to_string()));

        cleanup(&path);
    }

    #[test]
    fn add_overwrites_existing_key() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("key", "one").unwrap();
        store.add("key", "two").unwrap();
        assert_eq!(store.get("key").unwrap(), Some("two".to_string()));

        cleanup(&path);
    }

    #[test]
    fn list_returns_sorted_pairs() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        store.add("b", "2").unwrap();
        store.add("a", "1").unwrap();
        store.add("c", "3").unwrap();

        let pairs = store.list().unwrap();
        assert_eq!(
            pairs,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
                ("c".to_string(), "3".to_string()),
            ]
        );

        cleanup(&path);
    }

    #[test]
    fn corrupt_json_is_backed_up_and_treated_as_empty() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        fs::write(&path, "{ definitely not json").unwrap();

        assert!(store.list().unwrap().is_empty());
        assert!(store.get("anything").unwrap().is_none());

        let parent = path.parent().unwrap();
        let backups: Vec<_> = fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("memory.json.corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1);

        cleanup(&path);
    }
}