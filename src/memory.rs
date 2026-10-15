use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

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
            MemoryError::NotFound(key) => write!(f, "memory key not found: {key}"),
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
    dir: PathBuf,
}

impl MemoryStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self { dir: dir.as_ref().to_path_buf() }
    }

    pub fn add(&self, key: &str, value: &str) -> Result<(), MemoryError> {
        validate_key_value(key, value)?;
        fs::create_dir_all(&self.dir).map_err(MemoryError::Io)?;

        let _lock = LockGuard::acquire(&self.lock_path())?;
        let mut entries = self.read_entries()?;
        entries.retain(|(existing_key, _)| existing_key.as_str() != key);
        entries.push((key.to_owned(), value.to_owned()));
        entries.sort_by(|(left_key, left_value), (right_key, right_value)| {
            left_key.cmp(right_key).then_with(|| left_value.cmp(right_value))
        });
        self.write_entries_atomic(&entries)
    }

    pub fn get(&self, key: &str) -> Result<String, MemoryError> {
        self.read_entries()?
            .into_iter()
            .find(|(existing_key, _)| existing_key == key)
            .map(|(_, value)| value)
            .ok_or_else(|| MemoryError::NotFound(key.to_owned()))
    }

    pub fn list(&self) -> Result<Vec<(String, String)>, MemoryError> {
        let mut entries = self.read_entries()?;
        entries.sort_by(|(left_key, left_value), (right_key, right_value)| {
            left_key.cmp(right_key).then_with(|| left_value.cmp(right_value))
        });
        Ok(entries)
    }

    fn path(&self) -> PathBuf {
        self.dir.join("memory.tsv")
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("memory.lock")
    }

    fn read_entries(&self) -> Result<Vec<(String, String)>, MemoryError> {
        let path = self.path();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&path).map_err(MemoryError::Io)?;
        let mut entries = Vec::new();

        for (line_index, line) in content.lines().enumerate() {
            if line.is_empty() {
                continue;
            }

            let (key, value) = line
                .split_once(tab())
                .ok_or_else(|| {
                    MemoryError::Parse(format!("line {}: missing tab delimiter", line_index + 1))
                })?;

            validate_key_value(key, value).map_err(|_| {
                MemoryError::Parse(format!(
                    "line {}: key and value must be non-empty and contain no newline or tab",
                    line_index + 1
                ))
            })?;

            entries.push((key.to_owned(), value.to_owned()));
        }

        Ok(entries)
    }

    fn write_entries_atomic(&self, entries: &[(String, String)]) -> Result<(), MemoryError> {
        let tmp_path = self.dir.join("memory.tsv.tmp");
        let write_result = (|| -> Result<(), MemoryError> {
            let mut tmp = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(MemoryError::Io)?;

            for (key, value) in entries {
                tmp.write_all(key.as_bytes()).map_err(MemoryError::Io)?;
                tmp.write_all(&[TAB_BYTE]).map_err(MemoryError::Io)?;
                tmp.write_all(value.as_bytes()).map_err(MemoryError::Io)?;
                tmp.write_all(&[NEWLINE_BYTE]).map_err(MemoryError::Io)?;
            }

            tmp.sync_all().map_err(MemoryError::Io)?;
            Ok(())
        })();

        if let Err(err) = write_result {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }

        if let Err(err) = fs::rename(&tmp_path, self.path()) {
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

    Ok(())
}

fn contains_tab_or_newline(value: &str) -> bool {
    value
        .chars()
        .any(|c| c == tab() || c == newline())
}

struct LockGuard {
    path: PathBuf,
    file: Option<File>,
}

impl LockGuard {
    fn acquire(lock_path: &Path) -> Result<Self, MemoryError> {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(file) => Ok(Self {
                path: lock_path.to_path_buf(),
                file: Some(file),
            }),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => Err(MemoryError::Locked),
            Err(err) => Err(MemoryError::Io(err)),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "workbuddy-memory-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn add_then_get_returns_value() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let store = MemoryStore::new(&dir);

        store.add("foo", "bar").unwrap();
        assert_eq!(store.get("foo").unwrap(), "bar");

        cleanup(&dir);
    }

    #[test]
    fn list_returns_all_keys_in_deterministic_order() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let store = MemoryStore::new(&dir);

        store.add("b", "2").unwrap();
        store.add("a", "1").unwrap();

        assert_eq!(
            store.list().unwrap(),
            vec![("a".to_owned(), "1".to_owned()), ("b".to_owned(), "2".to_owned())]
        );

        cleanup(&dir);
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let store = MemoryStore::new(&dir);

        let err = store.get("missing").unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));

        cleanup(&dir);
    }

    #[test]
    fn lock_file_exists_returns_locked() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let store = MemoryStore::new(&dir);

        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(store.lock_path())
            .unwrap();
        drop(lock);

        let err = store.add("foo", "bar").unwrap_err();
        assert!(matches!(err, MemoryError::Locked));

        cleanup(&dir);
    }

    #[test]
    fn write_leaves_no_temp_files() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let store = MemoryStore::new(&dir);

        store.add("foo", "bar").unwrap();

        let files: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();

        assert!(
            !files.iter().any(|name| name.contains("memory.tsv.tmp")),
            "temporary file should not remain: {files:?}"
        );
        assert!(
            !files.iter().any(|name| name == "memory.lock"),
            "lock file should be removed after write: {files:?}"
        );

        cleanup(&dir);
    }
}

impl MemoryStore {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, String> {
        Ok(Self::new(dir))
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