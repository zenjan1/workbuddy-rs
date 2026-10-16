use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// 按字符数安全截断字符串(保证返回合法 &str,不切断多字节字符)。
pub fn clip(s: &str, max_chars: usize) -> &str {
    s.char_indices()
        .nth(max_chars)
        .map(|(i, _)| &s[..i])
        .unwrap_or(s)
}

/// 将字符串转义为 JSON 字符串字面量（含双引号）。
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEntry {
    pub id: u64,
    pub content: String,
    pub created_at: u64,
}

fn memory_now_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 计算文件当前内容的指纹，用于轻量级读-改-写冲突检测。
/// 文件不存在时返回 None。
fn compute_memory_fingerprint(path: &Path) -> Result<Option<u64>> {
    match fs::read(path) {
        Ok(bytes) => {
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            Ok(Some(hasher.finish()))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("读取 memory.json 失败"),
    }
}

/// 公开的加载 memory.json 工具函数，文件不存在时返回空列表。
pub fn load_memory_json(path: &Path) -> Result<Vec<MemoryEntry>> {
    load_memories(path)
}

/// 公开的保存 memory.json 工具函数，会创建父目录；并发控制请配合 add_memory 使用。
pub fn save_memory_json(path: &Path, memories: &[MemoryEntry]) -> Result<()> {
    save_memories(path, memories)
}

fn load_memories(path: &Path) -> Result<Vec<MemoryEntry>> {
    match fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Vec::new()),
        Ok(s) => serde_json::from_str(&s).context("解析 memory.json 失败"),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).context("读取 memory.json 失败"),
    }
}

fn save_memories(path: &Path, memories: &[MemoryEntry]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)
        .with_context(|| format!("创建目录 {} 失败", dir.display()))?;
    let json = serde_json::to_string_pretty(memories)
        .context("序列化记忆失败")?;
    fs::write(path, json)
        .with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(())
}

pub fn add_memory(path: &Path, content: &str) -> Result<MemoryEntry> {
    // 读-改-写冲突检测：先记录当前文件指纹，保存前再次比对。
    let original_fingerprint = compute_memory_fingerprint(path)?;
    let mut memories = load_memories(path)?;
    let id = memories.iter().map(|m| m.id).max().unwrap_or(0) + 1;
    let entry = MemoryEntry {
        id,
        content: content.to_string(),
        created_at: memory_now_timestamp(),
    };
    memories.push(entry.clone());

    let current_fingerprint = compute_memory_fingerprint(path)?;
    if original_fingerprint != current_fingerprint {
        bail!("memory.json 已被并发修改，请重试");
    }

    save_memories(path, &memories)?;
    Ok(entry)
}

pub fn list_memories(path: &Path) -> Result<Vec<MemoryEntry>> {
    let mut memories = load_memories(path)?;
    memories.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(memories)
}

pub fn get_memory(path: &Path, id: u64) -> Result<MemoryEntry> {
    let memories = load_memories(path)?;
    memories
        .into_iter()
        .find(|m| m.id == id)
        .with_context(|| format!("未找到 id 为 {} 的记忆", id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_short_string() {
        assert_eq!(clip("abc", 10), "abc");
    }

    #[test]
    fn clip_long_string_on_char_boundary() {
        assert_eq!(clip("中文测试abc", 3), "中文测");
    }

    #[test]
    fn test_clip_empty() {
        assert_eq!(clip("", 5), "");
    }

    #[test]
    fn test_clip_exact() {
        // 英文字符数恰好等于max
        assert_eq!(clip("abc", 3), "abc");
        // 中文字符数恰好等于max
        assert_eq!(clip("中文测", 3), "中文测");
        // emoji字符数恰好等于max
        assert_eq!(clip("🚀", 1), "🚀");
    }

    #[test]
    fn test_clip_overflow() {
        // 中文+英文混合字符串超过max
        assert_eq!(clip("中文测试abc", 3), "中文测");
        // 多字节字符在中间位置
        assert_eq!(clip("abc中文测试", 4), "abc中");
        // emoji在中间被截断保护
        assert_eq!(clip("abc🚀xyz", 3), "abc");
        // 纯英文字符串超过max
        assert_eq!(clip("abcdef", 3), "abc");
    }

    fn write_test_memories(path: &std::path::Path, memories: &[MemoryEntry]) {
        let json = serde_json::to_string_pretty(memories).unwrap();
        std::fs::write(path, json).unwrap();
    }

    #[test]
    fn memory_add_assigns_incrementing_id_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");

        let first = add_memory(&path, "第一条记忆").unwrap();
        assert_eq!(first.id, 1);
        assert!(!first.content.is_empty());

        let second = add_memory(&path, "第二条记忆").unwrap();
        assert_eq!(second.id, 2);

        let loaded = super::load_memories(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, 1);
        assert_eq!(loaded[1].id, 2);
    }

    #[test]
    fn memory_list_sorts_by_created_at_descending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let memories = vec![
            MemoryEntry { id: 1, content: "old".into(), created_at: 10 },
            MemoryEntry { id: 2, content: "new".into(), created_at: 30 },
            MemoryEntry { id: 3, content: "mid".into(), created_at: 20 },
        ];
        write_test_memories(&path, &memories);

        let listed = list_memories(&path).unwrap();
        let ids: Vec<u64> = listed.iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    #[test]
    fn memory_get_returns_matching_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let entry = add_memory(&path, "需要查找的记忆").unwrap();

        let fetched = get_memory(&path, entry.id).unwrap();
        assert_eq!(fetched, entry);
    }

    #[test]
    fn memory_get_missing_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        add_memory(&path, "已存在的记忆").unwrap();

        assert!(get_memory(&path, 999).is_err());
    }

    #[test]
    fn memory_list_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");

        assert!(list_memories(&path).unwrap().is_empty());
    }

    #[test]
    fn memory_corrupt_json_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        std::fs::write(&path, "not valid json").unwrap();

        assert!(list_memories(&path).is_err());
    }
}
