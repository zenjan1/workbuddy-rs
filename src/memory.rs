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

    /// Adds a memory entry and returns its ID. Duplicate content returns the
    /// existing ID without modifying the file.
    pub fn add(&self, content: &str) -> Result<String, MemoryError> {
        validate_content(content)?;
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let mut entries = self.read_entries_locked()?;

        if let Some((existing_id, _)) = entries.iter().find(|(_, value)| *value == content) {
            return Ok(existing_id.clone());
        }

        let mut id = generate_id(content);
        let mut counter = 0u64;
        while entries.contains_key(&id) {
            counter += 1;
            id = format!("{}-{}", generate_id(content), counter);
        }

        entries.insert(id.clone(), content.to_owned());
        self.write_entries_atomic(&entries)?;
        Ok(id)
    }

    /// Resolves an ID or keyword. Exact ID match has priority over fuzzy
    /// content match.
    pub fn get(&self, id_or_keyword: &str) -> Result<MemoryEntry, MemoryError> {
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let entries = self.read_entries_locked()?;

        if let Some(content) = entries.get(id_or_keyword) {
            return Ok(MemoryEntry {
                id: id_or_keyword.to_owned(),
                content: content.clone(),
            });
        }

        for (id, content) in entries {
            if content.contains(id_or_keyword) {
                return Ok(MemoryEntry { id, content });
            }
        }

        Err(MemoryError::NotFound(id_or_keyword.to_owned()))
    }

    /// Lists all entries sorted by ID.
    pub fn list(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let entries = self.read_entries_locked()?;
        let mut result = Vec::with_capacity(entries.len());
        for (id, content) in entries {
            result.push(MemoryEntry { id, content });
        }
        Ok(result)
    }

    fn read_entries_locked(&self) -> Result<BTreeMap<String, String>, MemoryError> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }

        let content = fs::read_to_string(&self.path).map_err(MemoryError::Io)?;
        let parsed: serde_json::Value = serde_json::from_str(&content)
            .map_err(|_| MemoryError::Parse("memory file contains invalid JSON".to_string()))?;
        let object = parsed
            .as_object()
            .ok_or_else(|| MemoryError::Parse("memory file must contain a JSON object".to_string()))?;

        let mut entries = BTreeMap::new();
        for (key, value) in object {
            let value = value
                .as_str()
                .ok_or_else(|| MemoryError::Parse("memory file contains a non-string value".to_owned()))?;
            validate_content(value).map_err(|_| {
                MemoryError::Parse("memory file contains an invalid entry".to_owned())
            })?;
            entries.insert(key.clone(), value.to_owned());
        }

        Ok(entries)
    }

    fn write_entries_atomic(&self, entries: &BTreeMap<String, String>) -> Result<(), MemoryError> {
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
    Get { query: String },
}

pub fn run_memory_command(
    store: &MemoryStore,
    command: MemoryCommand,
) -> Result<String, MemoryError> {
    match command {
        MemoryCommand::Add { content } => {
            let id = store.add(&content)?;
            Ok(format!("added {id}"))
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
        MemoryCommand::Get { query } => {
            let entry = store.get(&query)?;
            Ok(format!("{}\t{}", entry.id, entry.content))
        }
    }
}

impl MemoryCommand {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        if args.is_empty() {
            return Err("用法: memory add <内容> | list | get <id或关键词>".to_string());
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
                    return Err("用法: memory get <id或关键词>".to_string());
                }
                Ok(MemoryCommand::Get { query: args[1].clone() })
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

fn generate_id(content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

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

        let id = store.add("first note").unwrap();
        let entry = store.get(&id).unwrap();
        assert_eq!(entry.content, "first note");
        assert_eq!(entry.id, id);

        cleanup(&dir);
    }

    #[test]
    fn add_duplicate_returns_same_id_without_new_entry() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let id1 = store.add("repeat me").unwrap();
        let id2 = store.add("repeat me").unwrap();
        assert_eq!(id1, id2);

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "repeat me");

        cleanup(&dir);
    }

    #[test]
    fn list_returns_all_entries_sorted_by_id() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let id_banana = store.add("banana").unwrap();
        let id_apple = store.add("apple").unwrap();
        let id_cherry = store.add("cherry").unwrap();

        let entries = store.list().unwrap();
        let mut ids: Vec<_> = entries.iter().map(|e| e.id.as_str()).collect();
        let mut expected = vec![id_banana.as_str(), id_apple.as_str(), id_cherry.as_str()];
        expected.sort_unstable();
        ids.sort_unstable();
        assert_eq!(ids, expected);
        assert_eq!(entries.len(), 3);

        cleanup(&dir);
    }

    #[test]
    fn get_by_keyword_fuzzy_matches_content() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let _id = store.add("remember to buy milk").unwrap();
        let entry = store.get("milk").unwrap();
        assert_eq!(entry.content, "remember to buy milk");

        cleanup(&dir);
    }

    #[test]
    fn get_by_id_takes_precedence_over_keyword() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let id1 = store.add("project alpha notes").unwrap();
        let _id2 = store.add("this mentions alpha").unwrap();

        let entry = store.get(id1.as_str()).unwrap();
        assert_eq!(entry.id, id1);
        assert_eq!(entry.content, "project alpha notes");

        cleanup(&dir);
    }

    #[test]
    fn concurrent_adds_preserve_all_entries_and_file_is_valid() {
        let dir = temp_workspace();
        let store = Arc::new(MemoryStore::open(&dir).unwrap());

        let mut handles = vec![];

        for i in 0..20 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                store.add(&format!("memory-{i}")).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 20);

        let file_path = dir.join(".workbuddy").join("memory.json");
        let content = fs::read_to_string(&file_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.is_object());
        assert_eq!(parsed.as_object().unwrap().len(), 20);

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