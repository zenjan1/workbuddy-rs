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
    pub id: u64,
    pub text: String,
    pub created_at: Option<String>,
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
    /// Creates a memory store using `path` as the persistent JSON file.
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

    /// Adds a new memory entry with the given text. Returns the created entry.
    pub fn add(&self, text: &str) -> Result<MemoryEntry, MemoryError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(MemoryError::Invalid("text must not be empty".to_owned()));
        }
        let _guard = self.acquire_lock()?;
        let mut entries = self.read_entries_locked()?;
        let next_id = entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
        let entry = MemoryEntry {
            id: next_id,
            text,
            created_at: Some(now_iso8601()),
        };
        entries.push(entry.clone());
        self.write_entries_atomic(&entries)?;
        Ok(entry)
    }

    /// Gets a memory entry by ID.
    pub fn get(&self, id: u64) -> Result<MemoryEntry, MemoryError> {
        let _guard = self.acquire_lock()?;
        let entries = self.read_entries_locked()?;
        entries
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| MemoryError::NotFound(id.to_string()))
    }

    /// Lists all memory entries sorted by ID.
    pub fn list(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        let _guard = self.acquire_lock()?;
        let mut entries = self.read_entries_locked()?;
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
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

    fn read_entries_locked(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&self.path).map_err(MemoryError::Io)?;
        match serde_json::from_str::<Vec<MemoryEntry>>(&content) {
            Ok(entries) => Ok(entries),
            Err(_err) => {
                // Do not panic on corrupt JSON; rename the bad file and start fresh.
                let backup = self.corrupt_backup_path();
                let _ = fs::rename(&self.path, &backup);
                Ok(Vec::new())
            }
        }
    }

    fn write_entries_atomic(&self, entries: &[MemoryEntry]) -> Result<(), MemoryError> {
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
    Add { text: String },
    List,
    Get { id: u64 },
}

pub fn run_memory_command(
    store: &MemoryStore,
    command: MemoryCommand,
) -> Result<String, MemoryError> {
    match command {
        MemoryCommand::Add { text } => {
            let entry = store.add(&text)?;
            Ok(format!("added memory #{}: {}", entry.id, entry.text))
        }
        MemoryCommand::List => {
            let entries = store.list()?;
            if entries.is_empty() {
                return Ok(String::new());
            }
            let mut out = String::new();
            for entry in entries {
                out.push_str(&format!("#{}\t{}\n", entry.id, entry.text));
            }
            Ok(out)
        }
        MemoryCommand::Get { id } => {
            let entry = store.get(id)?;
            let created = entry.created_at.as_deref().unwrap_or("-");
            Ok(format!(
                "#{}\t{}\tcreated_at: {}",
                entry.id, entry.text, created
            ))
        }
    }
}

impl MemoryCommand {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        if args.is_empty() {
            return Err("用法: memory add <text> | list | get <id>".to_string());
        }
        match args[0].as_str() {
            "add" => {
                if args.len() < 2 {
                    return Err("用法: memory add <text>".to_string());
                }
                let text = args[1..].join(" ");
                Ok(MemoryCommand::Add { text })
            }
            "list" => {
                if args.len() != 1 {
                    return Err("用法: memory list".to_string());
                }
                Ok(MemoryCommand::List)
            }
            "get" => {
                if args.len() != 2 {
                    return Err("用法: memory get <id>".to_string());
                }
                let id = args[1]
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| "memory id 必须是正整数".to_string())?;
                Ok(MemoryCommand::Get { id })
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
        assert!(matches!(store.get(1), Err(MemoryError::NotFound(_))));

        cleanup(&path);
    }

    #[test]
    fn add_persists_entry_and_get_returns_it() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        let entry = store.add("hello world").unwrap();
        assert_eq!(entry.id, 1);
        assert_eq!(entry.text, "hello world");
        assert!(entry.created_at.is_some());
        drop(store);

        let store = MemoryStore::new(&path).unwrap();
        let fetched = store.get(1).unwrap();
        assert_eq!(fetched, entry);

        cleanup(&path);
    }

    #[test]
    fn list_returns_all_entries_sorted_by_id() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();

        let entry1 = store.add("first").unwrap();
        let entry2 = store.add("second").unwrap();

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], entry1);
        assert_eq!(entries[1], entry2);

        cleanup(&path);
    }

    #[test]
    fn add_rejects_empty_text() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        let err = store.add("   ").unwrap_err();
        assert!(matches!(err, MemoryError::Invalid(_)));

        cleanup(&path);
    }

    #[test]
    fn get_returns_not_found_for_missing_id() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        let err = store.get(999).unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));

        cleanup(&path);
    }

    #[test]
    fn command_parse_add_missing_text_is_error() {
        let args = vec!["add".to_string()];
        assert!(MemoryCommand::parse(&args).is_err());
    }

    #[test]
    fn command_parse_get_invalid_id_is_error() {
        let args = vec!["get".to_string(), "abc".to_string()];
        assert!(MemoryCommand::parse(&args).is_err());
    }

    #[test]
    fn corrupt_json_is_backed_up_and_treated_as_empty() {
        let path = temp_memory_path();
        let store = MemoryStore::new(&path).unwrap();
        fs::write(&path, "{ definitely not json").unwrap();

        assert!(store.list().unwrap().is_empty());
        assert!(matches!(store.get(1), Err(MemoryError::NotFound(_))));

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