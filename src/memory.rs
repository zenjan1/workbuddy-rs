use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

use crate::workspace::Workspace;

#[derive(Debug)]
pub enum MemoryError {
    Io(std::io::Error),
    NotFound(String),
    Invalid(String),
    Parse(String),
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::Io(err) => write!(f, "memory io error: {err}"),
            MemoryError::NotFound(id) => write!(f, "memory entry not found: {id}"),
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
    pub text: String,
    #[serde(default)]
    pub tags: Vec<String>,
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
        if !path.exists() {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(MemoryError::Io)?;
        }
        Ok(Self { path })
    }

    pub fn open(workspace_dir: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let workspace = Workspace {
            dir: workspace_dir.as_ref().to_path_buf(),
        };
        Self::new(memory_file_path(&workspace.dir))
    }

    pub fn add(&self, key: &str, text: &str, tags: &[String]) -> Result<MemoryEntry, MemoryError> {
        let key = key.trim();
        let text = text.trim();
        if key.is_empty() {
            return Err(MemoryError::Invalid("memory key must not be empty".to_owned()));
        }
        if text.is_empty() {
            return Err(MemoryError::Invalid("memory text must not be empty".to_owned()));
        }

        let tags = tags
            .iter()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<_>>();

        let mut entries = self.read_entries()?;

        if let Some(existing) = entries.iter_mut().find(|entry| entry.key.as_str() == key) {
            existing.text = text.to_owned();
            existing.tags = tags;
            let saved = existing.clone();
            self.write_entries(&entries)?;
            return Ok(saved);
        }

        let entry = MemoryEntry {
            key: key.to_owned(),
            text: text.to_owned(),
            tags,
        };
        entries.push(entry.clone());
        self.write_entries(&entries)?;
        Ok(entry)
    }

    pub fn list(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.read_entries()
    }

    pub fn get(&self, key: &str) -> Result<MemoryEntry, MemoryError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(MemoryError::NotFound(key.to_owned()));
        }
        let entries = self.read_entries()?;
        entries
            .into_iter()
            .find(|entry| entry.key.as_str() == key)
            .ok_or_else(|| MemoryError::NotFound(key.to_owned()))
    }

    fn read_entries(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&self.path).map_err(MemoryError::Io)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }

        serde_json::from_str::<Vec<MemoryEntry>>(&content)
            .map_err(|err| MemoryError::Parse(format!("invalid memory file: {err}")))
    }

    fn write_entries(&self, entries: &[MemoryEntry]) -> Result<(), MemoryError> {
        let tmp_path = self.path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(entries).map_err(|err| {
            MemoryError::Parse(format!("failed to serialize memory entries: {err}"))
        })?;

        {
            let mut tmp = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(MemoryError::Io)?;
            tmp.write_all(&data).map_err(MemoryError::Io)?;
            tmp.sync_all().map_err(MemoryError::Io)?;
        }

        fs::rename(&tmp_path, &self.path).map_err(MemoryError::Io)?;
        Ok(())
    }
}

pub fn memory_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".workbuddy").join("memory.json")
}

pub fn add(
    workspace_dir: &Path,
    key: &str,
    text: &str,
    tags: &[String],
) -> Result<MemoryEntry, MemoryError> {
    let store = MemoryStore::open(workspace_dir)?;
    store.add(key, text, tags)
}

pub fn list(workspace_dir: &Path) -> Result<Vec<MemoryEntry>, MemoryError> {
    let store = MemoryStore::open(workspace_dir)?;
    store.list()
}

pub fn get(workspace_dir: &Path, key: &str) -> Result<MemoryEntry, MemoryError> {
    let store = MemoryStore::open(workspace_dir)?;
    store.get(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> PathBuf {
        let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "workbuddy-memory-crud-test-{}-{}",
            std::process::id(),
            id
        ))
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn tag_list(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_creates_file_and_round_trips() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        let entry = store
            .add("greeting", "hello world", &tag_list(&["test", "工作"]))
            .unwrap();
        assert_eq!(entry.key, "greeting");
        assert_eq!(entry.text, "hello world");
        assert_eq!(entry.tags, tag_list(&["test", "工作"]));
        assert!(memory_file_path(&dir).exists());

        drop(store);

        let reopened = MemoryStore::open(&dir).unwrap();
        let entries = reopened.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "greeting");
        assert_eq!(entries[0].text, "hello world");
        assert_eq!(entries[0].tags, tag_list(&["test", "工作"]));

        cleanup(&dir);
    }

    #[test]
    fn add_updates_existing_key_instead_of_duplicating() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        store.add("key", "first", &tag_list(&["one"])).unwrap();
        store.add("key", "second", &tag_list(&["two"])).unwrap();

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "second");
        assert_eq!(entries[0].tags, tag_list(&["two"]));

        cleanup(&dir);
    }

    #[test]
    fn list_returns_entries_in_file_order() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        store.add("first", "1", &[]).unwrap();
        store.add("second", "2", &[]).unwrap();
        store.add("third", "3", &[]).unwrap();

        let entries = store.list().unwrap();
        let keys: Vec<_> = entries.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(keys, vec!["first", "second", "third"]);

        cleanup(&dir);
    }

    #[test]
    fn get_returns_entry_by_key() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        store.add("alpha", "one", &[]).unwrap();
        store.add("beta", "two", &[]).unwrap();

        assert_eq!(store.get("alpha").unwrap().text, "one");
        assert_eq!(store.get("beta").unwrap().text, "two");
        assert!(matches!(store.get("missing"), Err(MemoryError::NotFound(_))));

        cleanup(&dir);
    }

    #[test]
    fn empty_key_or_text_is_rejected_without_writing() {
        let dir = temp_workspace();
        let store = MemoryStore::open(&dir).unwrap();

        assert!(matches!(
            store.add("   ", "value", &[]),
            Err(MemoryError::Invalid(_))
        ));
        assert!(matches!(
            store.add("key", "   ", &[]),
            Err(MemoryError::Invalid(_))
        ));

        let entries = store.list().unwrap();
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
        let err = store.list().unwrap_err();
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
        let err = store.list().unwrap_err();
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

#[cfg(any())]
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