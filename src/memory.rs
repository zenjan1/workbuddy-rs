use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

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

impl MemoryStore {
    /// Opens a memory store rooted at `workspace_dir`.
    /// The store file is `<workspace_dir>/.workbuddy/memory.json`.
    pub fn open(workspace_dir: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let workspace_dir = workspace_dir.as_ref();
        let canonical_workspace = if workspace_dir.is_dir() {
            fs::canonicalize(workspace_dir).map_err(MemoryError::Io)?
        } else {
            fs::create_dir_all(workspace_dir).map_err(MemoryError::Io)?;
            fs::canonicalize(workspace_dir).map_err(MemoryError::Io)?
        };

        let memory_dir = canonical_workspace.join(".workbuddy");
        fs::create_dir_all(&memory_dir).map_err(MemoryError::Io)?;

        let memory_file = memory_dir.join("memory.json");
        if !memory_file.starts_with(&canonical_workspace) {
            return Err(MemoryError::Invalid(
                "memory file path escapes workspace".to_string(),
            ));
        }

        Ok(Self {
            path: memory_file,
            lock: Mutex::new(()),
        })
    }

    /// Adds a memory entry and returns the stored entry.
    pub fn add(&self, content: &str) -> Result<MemoryEntry, MemoryError> {
        validate_content(content)?;
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let mut entries = self.read_entries_locked();

        let next_id = entries
            .keys()
            .filter_map(|k| k.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let id = next_id.to_string();

        let entry = MemoryEntry {
            id: id.clone(),
            content: content.to_owned(),
            created_at: now_iso8601(),
        };
        entries.insert(id, entry.clone());
        self.write_entries_atomic(&entries)?;
        Ok(entry)
    }

    /// Gets an entry by exact ID, if present.
    pub fn get(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let entries = self.read_entries_locked();
        Ok(entries.get(id).cloned())
    }

    /// Lists all entries sorted by ID.
    pub fn list(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let entries = self.read_entries_locked();
        Ok(entries.into_values().collect())
    }

    fn read_entries_locked(&self) -> BTreeMap<String, MemoryEntry> {
        if !self.path.exists() {
            return BTreeMap::new();
        }

        let content = match fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(_) => return BTreeMap::new(),
        };

        match serde_json::from_str::<BTreeMap<String, MemoryEntry>>(&content) {
            Ok(entries) => entries,
            Err(_) => BTreeMap::new(),
        }
    }

    fn write_entries_atomic(&self, entries: &BTreeMap<String, MemoryEntry>) -> Result<(), MemoryError> {
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
}

pub enum MemoryCommand {
    Add { content: String },
    List,
    Get { id: String },
}

pub fn run_memory_command(
    store: &MemoryStore,
    command: MemoryCommand,
) -> Result<String, MemoryError> {
    match command {
        MemoryCommand::Add { content } => {
            let entry = store.add(&content)?;
            Ok(format!("added {}", entry.id))
        }
        MemoryCommand::List => {
            let entries = store.list()?;
            let mut out = String::new();
            for entry in entries {
                out.push_str(&entry.id);
                out.push('\t');
                out.push_str(&entry.content);
                out.push('\n');
            }
            Ok(out)
        }
        MemoryCommand::Get { id } => match store.get(&id)? {
            Some(entry) => Ok(format!("{}\t{}", entry.id, entry.content)),
            None => Err(MemoryError::NotFound(id)),
        },
    }
}

impl MemoryCommand {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        if args.is_empty() {
            return Err("用法: memory add <内容> | list | get <id>".to_string());
        }
        match args[0].as_str() {
            "add" => {
                if args.len() < 2 {
                    return Err("用法: memory add <内容>".to_string());
                }
                let content = args[1..].join(" ");
                Ok(MemoryCommand::Add { content })
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
                Ok(MemoryCommand::Get { id: args[1].clone() })
            }
            other => Err(format!("未知记忆子命令: {other}")),
        }
    }
}

pub fn memory_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".workbuddy").join("memory.json")
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

    fn temp_workspace() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "workbuddy-memory-test-{}-{}",
            std::process::id(),
            NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(workspace: &Path) {
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn add_then_get_by_id_returns_entry() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let added = store.add("first note").unwrap();
        assert_eq!(added.id, "1");
        assert!(!added.created_at.is_empty());

        let fetched = store.get(&added.id).unwrap().expect("entry should exist");
        assert_eq!(fetched.id, added.id);
        assert_eq!(fetched.content, "first note");
        assert_eq!(fetched.created_at, added.created_at);

        cleanup(&dir);
    }

    #[test]
    fn list_returns_all_entries_sorted_by_id() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let _banana = store.add("banana").unwrap();
        let _apple = store.add("apple").unwrap();
        let _cherry = store.add("cherry").unwrap();

        let entries = store.list().unwrap();
        let ids: Vec<_> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "2", "3"]);
        assert_eq!(entries[0].content, "banana");
        assert_eq!(entries[1].content, "apple");
        assert_eq!(entries[2].content, "cherry");

        cleanup(&dir);
    }

    #[test]
    fn get_unknown_id_returns_none() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();
        store.add("alpha").unwrap();

        assert!(store.get("999").unwrap().is_none());

        cleanup(&dir);
    }

    #[test]
    fn damaged_json_file_falls_back_to_empty_list() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();
        let path = store.path.clone();
        fs::write(&path, "{ definitely not json").unwrap();

        assert_eq!(store.list().unwrap(), Vec::<MemoryEntry>::new());
        assert!(store.get("1").unwrap().is_none());

        cleanup(&dir);
    }

    #[test]
    fn missing_file_list_is_empty() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        assert_eq!(store.list().unwrap(), Vec::<MemoryEntry>::new());

        cleanup(&dir);
    }
}