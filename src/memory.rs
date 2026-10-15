use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
            MemoryError::NotFound(key) => write!(f, "memory key not found: {}", redact_if_sensitive(key)),
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

pub struct MemoryStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl MemoryStore {
    /// Creates a new memory store backed by the given JSON file path.
    ///
    /// Parent directories are created if missing. The file itself is not
    /// created until the first write; read operations on a missing file
    /// return an empty store.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn add(&self, key: &str, value: &str) -> Result<(), MemoryError> {
        validate_key_value(key, value)?;
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let mut entries = self.read_entries_locked()?;
        entries.insert(key.to_owned(), value.to_owned());
        self.write_entries_atomic(&entries)
    }

    pub fn get(&self, key: &str) -> Result<String, MemoryError> {
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let entries = self.read_entries_locked()?;
        entries
            .get(key)
            .cloned()
            .ok_or_else(|| MemoryError::NotFound(key.to_owned()))
    }

    pub fn list(&self) -> Result<Vec<(String, String)>, MemoryError> {
        let _guard = self.lock.lock().map_err(|_| MemoryError::Locked)?;
        let entries = self.read_entries_locked()?;
        let mut entries: Vec<_> = entries.into_iter().collect();
        entries.sort_by(|(left_key, _), (right_key, _)| left_key.cmp(right_key));
        Ok(entries)
    }

    fn read_entries_locked(&self) -> Result<BTreeMap<String, String>, MemoryError> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }

        let content = fs::read_to_string(&self.path).map_err(MemoryError::Io)?;
        let parsed: serde_json::Value =
            serde_json::from_str(&content).map_err(|_| MemoryError::Parse("memory file contains invalid JSON".to_string()))?;
        let object = parsed
            .as_object()
            .ok_or_else(|| MemoryError::Parse("memory file must contain a JSON object".to_string()))?;

        let mut entries = BTreeMap::new();
        for (key, value) in object {
            let value = value
                .as_str()
                .ok_or_else(|| MemoryError::Parse("memory file contains a non-string value".to_owned()))?;
            validate_key_value(key, value).map_err(|_| {
                MemoryError::Parse("memory file contains an invalid entry".to_owned())
            })?;
            entries.insert(key.clone(), value.to_owned());
        }

        Ok(entries)
    }

    fn write_entries_atomic(&self, entries: &BTreeMap<String, String>) -> Result<(), MemoryError> {
        let tmp_path = self.path.with_extension("json.tmp");
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
            let entries = store.list()?;
            let mut out = String::new();
            for (key, value) in entries {
                out.push_str(&key);
                out.push(tab());
                out.push_str(&value);
                out.push(newline());
            }
            Ok(out)
        }
        MemoryCommand::Get { key } => store.get(&key),
    }
}

const TAB_BYTE: u8 = 0x09;
const NEWLINE_BYTE: u8 = 0x0A;

fn tab() -> char {
    char::from_u32(TAB_BYTE as u32).unwrap()
}

fn newline() -> char {
    char::from_u32(NEWLINE_BYTE as u32).unwrap()
}

const SENSITIVE_WORDS: &[&str] = &[
    "password",
    "token",
    "secret",
    "api_key",
    "credential",
    "private_key",
    "passwd",
    "apikey",
    "auth_token",
    "access_key",
];

fn is_sensitive(value: &str) -> bool {
    let lower = value.to_lowercase();
    SENSITIVE_WORDS.iter().any(|&word| lower.contains(word))
}

fn redact_if_sensitive(value: &str) -> String {
    if is_sensitive(value) {
        "[redacted]".to_owned()
    } else {
        value.to_owned()
    }
}

pub fn memory_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("memory.json")
}

fn validate_key_value(key: &str, value: &str) -> Result<(), MemoryError> {
    if key.is_empty() || value.is_empty() {
        return Err(MemoryError::Invalid(
            "key and value must not be empty".to_owned(),
        ));
    }

    if contains_tab_or_newline(key) || contains_tab_or_newline(value) {
        return Err(MemoryError::Invalid(
            "key and value must not contain newline or tab".to_owned(),
        ));
    }

    if is_sensitive(key) || is_sensitive(value) {
        return Err(MemoryError::Invalid(
            "refusing to store sensitive information".to_owned(),
        ));
    }

    Ok(())
}

fn contains_tab_or_newline(value: &str) -> bool {
    value
        .chars()
        .any(|c| c == tab() || c == newline())
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_file() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "workbuddy-memory-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("memory.json")
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn add_then_get_returns_value() {
        let file = temp_file();
        let store = MemoryStore::new(&file);

        store.add("foo", "bar").unwrap();
        assert_eq!(store.get("foo").unwrap(), "bar");

        cleanup(&file);
    }

    #[test]
    fn add_overwrites_existing_key() {
        let file = temp_file();
        let store = MemoryStore::new(&file);

        store.add("foo", "bar").unwrap();
        store.add("foo", "baz").unwrap();
        assert_eq!(store.get("foo").unwrap(), "baz");

        let content = fs::read_to_string(&file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["foo"], "baz");

        cleanup(&file);
    }

    #[test]
    fn list_returns_all_keys_sorted() {
        let file = temp_file();
        let store = MemoryStore::new(&file);

        store.add("b", "2").unwrap();
        store.add("a", "1").unwrap();
        store.add("c", "3").unwrap();

        let entries = store.list().unwrap();
        assert_eq!(
            entries,
            vec![
                ("a".to_owned(), "1".to_owned()),
                ("b".to_owned(), "2".to_owned()),
                ("c".to_owned(), "3".to_owned()),
            ]
        );

        cleanup(&file);
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let file = temp_file();
        let store = MemoryStore::new(&file);

        let err = store.get("missing").unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));

        cleanup(&file);
    }

    #[cfg(unix)]
    #[test]
    fn permission_error_returns_io_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_file();
        let store_path = dir;
        let parent = store_path.parent().unwrap();
        fs::create_dir_all(parent).unwrap();

        // Make the directory read-only.
        let mut perms = fs::metadata(parent).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(parent, perms).unwrap();

        let store = MemoryStore::new(&store_path);
        let result = store.add("foo", "bar");

        // Restore permissions so cleanup can remove the directory.
        let mut perms = fs::metadata(parent).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(parent, perms).unwrap();

        assert!(matches!(result, Err(MemoryError::Io(_))));
        cleanup(&store_path);
    }

    #[test]
    fn concurrent_writes_preserve_all_entries() {
        let file = temp_file();
        let store = Arc::new(MemoryStore::new(&file));

        let mut handles = vec![];

        // Unique keys written by separate threads.
        for i in 0..10 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                store
                    .add(&format!("key-{i}"), &format!("value-{i}"))
                    .unwrap();
            }));
        }

        // Same key written by multiple threads concurrently.
        for i in 0..5 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                store
                    .add("shared", &format!("shared-value-{i}"))
                    .unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All unique keys must be present.
        for i in 0..10 {
            assert_eq!(
                store.get(&format!("key-{i}")).unwrap(),
                format!("value-{i}")
            );
        }

        // The shared key must exist and be one of the values written.
        let shared_value = store.get("shared").unwrap();
        assert!(shared_value.starts_with("shared-value-"));

        // The file must be valid JSON and contain exactly 11 keys.
        let content = fs::read_to_string(&file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let obj = parsed.as_object().unwrap();
        assert_eq!(obj.len(), 11);
        assert!(obj.contains_key("shared"));

        cleanup(&file);
    }

    #[test]
    fn missing_file_list_is_empty() {
        let file = temp_file();
        let store = MemoryStore::new(&file);

        assert_eq!(store.list().unwrap(), Vec::<(String, String)>::new());

        cleanup(&file);
    }

    #[test]
    fn rejects_sensitive_keys_and_values() {
        let file = temp_file();
        let store = MemoryStore::new(&file);

        for key in ["password", "api_key", "private_key"] {
            let err = store.add(key, "value").unwrap_err();
            assert!(matches!(err, MemoryError::Invalid(_)));
        }

        for value in ["secret", "token", "credential"] {
            let err = store.add("safe_key", value).unwrap_err();
            assert!(matches!(err, MemoryError::Invalid(_)));
        }

        let err = store.add("normal_key", "my password").unwrap_err();
        let msg = err.to_string();
        assert!(!msg.to_lowercase().contains("password"));
        assert!(store.list().unwrap().is_empty());

        cleanup(&file);
    }

    #[test]
    fn corrupted_json_returns_parse_error() {
        let file = temp_file();
        let store = MemoryStore::new(&file);

        fs::write(&file, "{ not json").unwrap();
        let result = store.list();

        assert!(matches!(result, Err(MemoryError::Parse(_))));

        cleanup(&file);
    }
}

impl MemoryStore {
    /// Opens a memory store at the given path.
    ///
    /// If `path` refers to a directory, `memory.json` inside that directory
    /// is used. Otherwise the path is treated as the full file path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let file_path = if path.is_dir() {
            path.join("memory.json")
        } else {
            path.to_path_buf()
        };
        Ok(Self::new(file_path))
    }
}

impl MemoryCommand {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        if args.is_empty() {
            return Err("用法: memory add <key> <value> | list | get <key>".to_string());
        }
        match args[0].as_str() {
            "add" => {
                if args.len() != 3 {
                    return Err("用法: memory add <key> <value>".to_string());
                }
                Ok(MemoryCommand::Add {
                    key: args[1].clone(),
                    value: args[2].clone(),
                })
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