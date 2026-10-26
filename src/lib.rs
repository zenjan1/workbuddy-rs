
pub mod agents;
pub mod bench;
pub mod config;
pub mod dev;
pub mod edits;
pub mod executor;
pub mod hooks;
pub mod health;
pub mod llm;
pub mod memory;
pub mod plan;
pub mod planner;
pub mod replay;
pub mod resolver;
pub mod roadmap;
pub mod review;
pub mod serve;
pub mod status;
pub mod task_id;
pub mod tui;
pub mod util;
pub mod utils;
pub mod webhook;
pub mod workspace;

use std::time::{SystemTime, UNIX_EPOCH};

/// Returns current time as a timestamp string (seconds since UNIX epoch).
/// Note: For full ISO 8601/RFC3339 support, add the `chrono` crate to Cargo.toml.
pub fn main_now_iso() -> String {
    let now = SystemTime::now();
    let secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    format!("{}", secs)
}

// Xi Memory Store - Persistent key-value storage with SQLite backend.
//
// This crate provides a thread-safe key-value storage abstraction with support for:
// - Namespace isolation
// - Atomic compare-and-set (CAS) operations
// - TTL-based expiration
// - Prefix scanning
// - Transactional batch writes
// - Schema migrations

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use thiserror::Error;

/// Errors that can occur during memory store operations.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// The requested key was not found.
    #[error("key not found: {0}")]
    KeyNotFound(String),
    
    /// The CAS operation failed because the value didn't match the expected value.
    #[error("compare-and-set failed: value mismatch for key {0}")]
    CasFailed(String),
    
    /// A namespace with the given name already exists.
    #[error("namespace already exists: {0}")]
    NamespaceExists(String),
    
    /// The requested namespace was not found.
    #[error("namespace not found: {0}")]
    NamespaceNotFound(String),
    
    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    
    /// A SQLite error occurred.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    
    /// A serialization/deserialization error occurred.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    
    /// A migration error occurred.
    #[error("migration error: {0}")]
    Migration(String),
    
    /// The operation timed out.
    #[error("operation timed out")]
    Timeout,
    
    /// Invalid argument provided.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    
    /// The store has been closed.
    #[error("store closed")]
    StoreClosed,
}

/// Result type for memory store operations.
pub type MemoryResult<T> = Result<T, MemoryError>;

/// A key-value pair with optional TTL.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KvEntry {
    /// The key.
    pub key: String,
    /// The value (JSON-serialized).
    pub value: serde_json::Value,
    /// Optional TTL in seconds. None means no expiration.
    pub ttl_seconds: Option<u64>,
    /// Creation timestamp (Unix epoch seconds).
    pub created_at: i64,
    /// Last update timestamp (Unix epoch seconds).
    pub updated_at: i64,
}

impl KvEntry {
    /// Create a new entry without TTL.
    pub fn new(key: String, value: serde_json::Value) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            key,
            value,
            ttl_seconds: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Create a new entry with TTL.
    pub fn with_ttl(key: String, value: serde_json::Value, ttl: Duration) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            key,
            value,
            ttl_seconds: Some(ttl.as_secs()),
            created_at: now,
            updated_at: now,
        }
    }

    /// Check if the entry has expired.
    pub fn is_expired(&self) -> bool {
        if let Some(ttl) = self.ttl_seconds {
            let now = chrono::Utc::now().timestamp();
            now > self.created_at + ttl as i64
        } else {
            false
        }
    }
}

/// Options for scan operations.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Maximum number of results to return.
    pub limit: Option<usize>,
    /// Offset for pagination.
    pub offset: Option<usize>,
    /// Whether to include expired entries (default: false).
    pub include_expired: bool,
    /// Sort order (default: ascending by key).
    pub reverse: bool,
}

/// Result of a scan operation.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// The entries found.
    pub entries: Vec<KvEntry>,
    /// Whether there are more results available.
    pub has_more: bool,
    /// Total count (if available).
    pub total_count: Option<usize>,
}

/// Batch write operation.
#[derive(Debug, Clone)]
pub enum BatchOperation {
    /// Set a key-value pair.
    Set { key: String, value: serde_json::Value, ttl: Option<Duration> },
    /// Delete a key.
    Delete { key: String },
    /// Compare and set.
    CompareAndSet { key: String, expected: Option<serde_json::Value>, new_value: serde_json::Value, ttl: Option<Duration> },
}

/// Trait defining the memory store interface.
///
/// This trait is designed to be implementable by both SQLite-backed
/// and in-memory implementations for testing.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync + 'static {
    /// Get a value by key.
    ///
    /// Returns None if the key doesn't exist or has expired.
    async fn get(&self, namespace: &str, key: &str) -> MemoryResult<Option<serde_json::Value>>;

    /// Set a key-value pair.
    ///
    /// If the key already exists, it will be overwritten.
    /// Returns the previous value if any.
    async fn set(
        &self,
        namespace: &str,
        key: &str,
        value: serde_json::Value,
        ttl: Option<Duration>,
    ) -> MemoryResult<Option<serde_json::Value>>;

    /// Delete a key.
    ///
    /// Returns true if the key existed and was deleted.
    async fn delete(&self, namespace: &str, key: &str) -> MemoryResult<bool>;

    /// Atomic compare-and-set operation.
    ///
    /// Only sets the new value if the current value matches `expected`.
    /// If `expected` is None, the operation succeeds only if the key doesn't exist.
    /// Returns the previous value on success.
    async fn compare_and_set(
        &self,
        namespace: &str,
        key: &str,
        expected: Option<serde_json::Value>,
        new_value: serde_json::Value,
        ttl: Option<Duration>,
    ) -> MemoryResult<Option<serde_json::Value>>;

    /// Scan keys with a given prefix.
    ///
    /// Returns entries matching the prefix, with pagination support.
    async fn scan_prefix(
        &self,
        namespace: &str,
        prefix: &str,
        options: ScanOptions,
    ) -> MemoryResult<ScanResult>;

    /// Execute multiple operations atomically in a single transaction.
    ///
    /// All operations succeed or none do.
    async fn batch_write(&self, namespace: &str, operations: Vec<BatchOperation>) -> MemoryResult<()>;

    /// Create a new namespace.
    ///
    /// Returns an error if the namespace already exists.
    async fn create_namespace(&self, name: &str) -> MemoryResult<()>;

    /// Delete a namespace and all its data.
    async fn delete_namespace(&self, name: &str) -> MemoryResult<()>;

    /// List all namespaces.
    async fn list_namespaces(&self) -> MemoryResult<Vec<String>>;

    /// Check if a namespace exists.
    async fn namespace_exists(&self, name: &str) -> MemoryResult<bool>;

    /// Clean up expired entries in a namespace.
    ///
    /// Returns the number of entries removed.
    async fn cleanup_expired(&self, namespace: &str) -> MemoryResult<usize>;

    /// Get statistics for a namespace.
    async fn stats(&self, namespace: &str) -> MemoryResult<NamespaceStats>;

    /// Close the store and release resources.
    async fn close(&self) -> MemoryResult<()>;
}

/// Statistics for a namespace.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NamespaceStats {
    /// Number of keys in the namespace.
    pub key_count: usize,
    /// Total size of values in bytes (approximate).
    pub total_size_bytes: u64,
    /// Number of expired but not yet cleaned up keys.
    pub expired_count: usize,
    /// Oldest entry timestamp.
    pub oldest_entry: Option<i64>,
    /// Newest entry timestamp.
    pub newest_entry: Option<i64>,
}

impl Default for NamespaceStats {
    fn default() -> Self {
        Self {
            key_count: 0,
            total_size_bytes: 0,
            expired_count: 0,
            oldest_entry: None,
            newest_entry: None,
        }
    }
}

/// A handle to a specific namespace within a store.
///
/// This provides a more ergonomic API for working within a single namespace.
#[derive(Debug, Clone)]
pub struct NamespaceHandle<S: MemoryStore> {
    store: S,
    name: String,
}

impl<S: MemoryStore> NamespaceHandle<S> {
    /// Create a new namespace handle.
    pub fn new(store: S, name: String) -> Self {
        Self { store, name }
    }

    /// Get the namespace name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get a value by key.
    pub async fn get(&self, key: &str) -> MemoryResult<Option<serde_json::Value>> {
        self.store.get(&self.name, key).await
    }

    /// Set a key-value pair.
    pub async fn set(
        &self,
        key: &str,
        value: serde_json::Value,
        ttl: Option<Duration>,
    ) -> MemoryResult<Option<serde_json::Value>> {
        self.store.set(&self.name, key, value, ttl).await
    }

    /// Delete a key.
    pub async fn delete(&self, key: &str) -> MemoryResult<bool> {
        self.store.delete(&self.name, key).await
    }

    /// Atomic compare-and-set operation.
    pub async fn compare_and_set(
        &self,
        key: &str,
        expected: Option<serde_json::Value>,
        new_value: serde_json::Value,
        ttl: Option<Duration>,
    ) -> MemoryResult<Option<serde_json::Value>> {
        self.store
            .compare_and_set(&self.name, key, expected, new_value, ttl)
            .await
    }

    /// Scan keys with a given prefix.
    pub async fn scan_prefix(&self, prefix: &str, options: ScanOptions) -> MemoryResult<ScanResult> {
        self.store.scan_prefix(&self.name, prefix, options).await
    }

    /// Execute multiple operations atomically.
    pub async fn batch_write(&self, operations: Vec<BatchOperation>) -> MemoryResult<()> {
        self.store.batch_write(&self.name, operations).await
    }

    /// Clean up expired entries.
    pub async fn cleanup_expired(&self) -> MemoryResult<usize> {
        self.store.cleanup_expired(&self.name).await
    }

    /// Get namespace statistics.
    pub async fn stats(&self) -> MemoryResult<NamespaceStats> {
        self.store.stats(&self.name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_kv_entry_creation() {
        let entry = KvEntry::new("test_key".to_string(), serde_json::json!({"foo": "bar"}));
        assert_eq!(entry.key, "test_key");
        assert_eq!(entry.value, serde_json::json!({"foo": "bar"}));
        assert!(entry.ttl_seconds.is_none());
        assert!(!entry.is_expired());
    }

    #[test]
    fn test_kv_entry_with_ttl() {
        let entry = KvEntry::with_ttl(
            "test_key".to_string(),
            serde_json::json!({"foo": "bar"}),
            Duration::from_secs(60),
        );
        assert_eq!(entry.ttl_seconds, Some(60));
        assert!(!entry.is_expired());
    }

    #[test]
    fn test_scan_options_default() {
        let options = ScanOptions::default();
        assert_eq!(options.limit, None);
        assert_eq!(options.offset, None);
        assert!(!options.include_expired);
        assert!(!options.reverse);
    }

    #[test]
    fn test_memory_error_display() {
        let err = MemoryError::KeyNotFound("test_key".to_string());
        assert_eq!(err.to_string(), "key not found: test_key");

        let err = MemoryError::CasFailed("test_key".to_string());
        assert_eq!(err.to_string(), "compare-and-set failed: value mismatch for key test_key");

        let err = MemoryError::NamespaceExists("test_ns".to_string());
        assert_eq!(err.to_string(), "namespace already exists: test_ns");

        let err = MemoryError::NamespaceNotFound("test_ns".to_string());
        assert_eq!(err.to_string(), "namespace not found: test_ns");
    }

    #[test]
    fn test_batch_operations() {
        let ops = vec![
            BatchOperation::Set {
                key: "key1".to_string(),
                value: serde_json::json!("value1"),
                ttl: None,
            },
            BatchOperation::Delete {
                key: "key2".to_string(),
            },
            BatchOperation::CompareAndSet {
                key: "key3".to_string(),
                expected: Some(serde_json::json!("old")),
                new_value: serde_json::json!("new"),
                ttl: Some(Duration::from_secs(60)),
            },
        ];
        assert_eq!(ops.len(), 3);
    }
}