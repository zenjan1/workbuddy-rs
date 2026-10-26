use std::path::PathBuf;
use std::sync::OnceLock;
use rusqlite::{Connection, Result as SqliteResult, params, OptionalExtension};
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use uuid::Uuid;
use dirs;
use crate::error::{XiError, Result};

/// Pattern type classification
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatternType {
    Failure,
    ToolSequence,
    PromptTemplate,
}

impl PatternType {
    fn as_str(&self) -> &'static str {
        match self {
            PatternType::Failure => "failure",
            PatternType::ToolSequence => "tool_sequence",
            PatternType::PromptTemplate => "prompt_template",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "failure" => Some(PatternType::Failure),
            "tool_sequence" => Some(PatternType::ToolSequence),
            "prompt_template" => Some(PatternType::PromptTemplate),
            _ => None,
        }
    }
}

/// A representative example of a pattern occurrence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternExample {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub context: serde_json::Value,
}

/// Type-specific metadata for patterns
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum PatternMetadata {
    Failure {
        error_type: String,
        error_message: String,
        stack_trace: Option<String>,
    },
    ToolSequence {
        tools: Vec<String>,
        duration_ms: u64,
        success_rate: f32,
    },
    PromptTemplate {
        template: String,
        variables: Vec<String>,
        model: String,
    },
}

/// Core pattern entity stored in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    pub pattern_type: PatternType,
    pub description: String,
    pub frequency: u32,
    pub examples: Vec<PatternExample>,
    pub metadata: PatternMetadata,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Pattern {
    /// Create a new pattern with generated UUID and timestamps
    pub fn new(
        pattern_type: PatternType,
        description: String,
        examples: Vec<PatternExample>,
        metadata: PatternMetadata,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            pattern_type,
            description,
            frequency: 1,
            examples,
            metadata,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Database-backed pattern storage
pub struct PatternStore {
    conn: Connection,
}

impl PatternStore {
    /// Get the default database path (~/.xi/patterns.db)
    fn default_db_path() -> PathBuf {
        let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push(".xi");
        std::fs::create_dir_all(&path).ok();
        path.push("patterns.db");
        path
    }

    /// Initialize the database connection and run migrations
    pub fn init_db() -> Result<Self> {
        let path = Self::default_db_path();
        let conn = Connection::open(&path)
            .map_err(|e| XiError::Storage(format!("Failed to open pattern database: {}", e)))?;

        let mut store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    /// Initialize with a custom database path (useful for testing)
    pub fn init_db_at(path: &PathBuf) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| XiError::Storage(format!("Failed to open pattern database: {}", e)))?;

        let mut store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    /// Run database migrations to create schema
    fn run_migrations(&mut self) -> Result<()> {
        // Create patterns table
        self.conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS patterns (
                id TEXT PRIMARY KEY,
                pattern_type TEXT NOT NULL,
                description TEXT NOT NULL,
                frequency INTEGER NOT NULL DEFAULT 1,
                examples TEXT NOT NULL DEFAULT '[]',
                metadata TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"#,
            [],
        ).map_err(|e| XiError::Storage(format!("Failed to create patterns table: {}", e)))?;

        // Create index on pattern_type for faster filtering
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_patterns_type ON patterns(pattern_type)",
            [],
        ).map_err(|e| XiError::Storage(format!("Failed to create index: {}", e)))?;

        // Create index on frequency for sorting
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_patterns_frequency ON patterns(frequency DESC)",
            [],
        ).map_err(|e| XiError::Storage(format!("Failed to create frequency index: {}", e)))?;

        Ok(())
    }

    /// Save a new pattern or update existing one
    pub fn save_pattern(&mut self, pattern: &Pattern) -> Result<()> {
        let examples_json = serde_json::to_string(&pattern.examples)
            .map_err(|e| XiError::Storage(format!("Failed to serialize examples: {}", e)))?;
        let metadata_json = serde_json::to_string(&pattern.metadata)
            .map_err(|e| XiError::Storage(format!("Failed to serialize metadata: {}", e)))?;

        self.conn.execute(
            r#"
            INSERT INTO patterns (id, pattern_type, description, frequency, examples, metadata, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                frequency = excluded.frequency,
                examples = excluded.examples,
                metadata = excluded.metadata,
                updated_at = excluded.updated_at
            "#,
            params![
                pattern.id,
                pattern.pattern_type.as_str(),
                pattern.description,
                pattern.frequency,
                examples_json,
                metadata_json,
                pattern.created_at.to_rfc3339(),
                pattern.updated_at.to_rfc3339(),
            ],
        ).map_err(|e| XiError::Storage(format!("Failed to save pattern: {}", e)))?;

        Ok(())
    }

    /// Retrieve all patterns, optionally filtered by type
    pub fn get_patterns(&self, pattern_type: Option<PatternType>) -> Result<Vec<Pattern>> {
        let (sql, params): (String, Vec<&dyn rusqlite::ToSql>) = match pattern_type {
            Some(pt) => (
                "SELECT id, pattern_type, description, frequency, examples, metadata, created_at, updated_at FROM patterns WHERE pattern_type = ?1 ORDER BY frequency DESC, created_at DESC".to_string(),
                vec![&pt.as_str()],
            ),
            None => (
                "SELECT id, pattern_type, description, frequency, examples, metadata, created_at, updated_at FROM patterns ORDER BY frequency DESC, created_at DESC".to_string(),
                vec![],
            ),
        };

        let mut stmt = self.conn.prepare(&sql)
            .map_err(|e| XiError::Storage(format!("Failed to prepare query: {}", e)))?;

        let pattern_iter = stmt.query_map(params.as_slice(), |row| {
            Self::row_to_pattern(row)
        }).map_err(|e| XiError::Storage(format!("Failed to query patterns: {}", e)))?;

        let mut patterns = Vec::new();
        for pattern in pattern_iter {
            patterns.push(pattern.map_err(|e| XiError::Storage(format!("Failed to parse pattern: {}", e)))?);
        }

        Ok(patterns)
    }

    /// Retrieve a single pattern by ID
    pub fn get_pattern_by_id(&self, id: &str) -> Result<Option<Pattern>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pattern_type, description, frequency, examples, metadata, created_at, updated_at FROM patterns WHERE id = ?1"
        ).map_err(|e| XiError::Storage(format!("Failed to prepare query: {}", e)))?;

        let pattern = stmt.query_row(params![id], |row| {
            Self::row_to_pattern(row)
        }).optional()
        .map_err(|e| XiError::Storage(format!("Failed to query pattern: {}", e)))?;

        Ok(pattern)
    }

    /// Increment the frequency counter for a pattern
    pub fn increment_frequency(&mut self, id: &str) -> Result<u32> {
        let new_freq: u32 = self.conn.query_row(
            "UPDATE patterns SET frequency = frequency + 1, updated_at = ?1 WHERE id = ?2 RETURNING frequency",
            params![Utc::now().to_rfc3339(), id],
            |row| row.get(0),
        ).optional()
        .map_err(|e| XiError::Storage(format!("Failed to increment frequency: {}", e)))?
        .ok_or_else(|| XiError::NotFound(format!("Pattern not found: {}", id)))?;

        Ok(new_freq)
    }

    /// Delete a pattern by ID
    pub fn delete_pattern(&mut self, id: &str) -> Result<bool> {
        let rows_affected = self.conn.execute(
            "DELETE FROM patterns WHERE id = ?1",
            params![id],
        ).map_err(|e| XiError::Storage(format!("Failed to delete pattern: {}", e)))?;

        Ok(rows_affected > 0)
    }

    /// Get pattern count (total or by type)
    pub fn count_patterns(&self, pattern_type: Option<PatternType>) -> Result<u64> {
        let (sql, params): (String, Vec<&dyn rusqlite::ToSql>) = match pattern_type {
            Some(pt) => (
                "SELECT COUNT(*) FROM patterns WHERE pattern_type = ?1".to_string(),
                vec![&pt.as_str()],
            ),
            None => (
                "SELECT COUNT(*) FROM patterns".to_string(),
                vec![],
            ),
        };

        let count: u64 = self.conn.query_row(&sql, params.as_slice(), |row| row.get(0))
            .map_err(|e| XiError::Storage(format!("Failed to count patterns: {}", e)))?;

        Ok(count)
    }

    /// Convert a database row to a Pattern struct
    fn row_to_pattern(row: &rusqlite::Row) -> SqliteResult<Pattern> {
        let pattern_type_str: String = row.get(1)?;
        let pattern_type = PatternType::from_str(&pattern_type_str)
            .ok_or_else(|| rusqlite::Error::InvalidColumnType(1, "pattern_type".into(), rusqlite::types::Type::Text))?;

        let examples_json: String = row.get(4)?;
        let examples: Vec<PatternExample> = serde_json::from_str(&examples_json)
            .map_err(|_| rusqlite::Error::InvalidColumnType(4, "examples".into(), rusqlite::types::Type::Text))?;

        let metadata_json: String = row.get(5)?;
        let metadata: PatternMetadata = serde_json::from_str(&metadata_json)
            .map_err(|_| rusqlite::Error::InvalidColumnType(5, "metadata".into(), rusqlite::types::Type::Text))?;

        let created_at_str: String = row.get(6)?;
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|_| rusqlite::Error::InvalidColumnType(6, "created_at".into(), rusqlite::types::Type::Text))?;

        let updated_at_str: String = row.get(7)?;
        let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|_| rusqlite::Error::InvalidColumnType(7, "updated_at".into(), rusqlite::types::Type::Text))?;

        Ok(Pattern {
            id: row.get(0)?,
            pattern_type,
            description: row.get(2)?,
            frequency: row.get(3)?,
            examples,
            metadata,
            created_at,
            updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_store() -> PatternStore {
        let dir = tempdir().unwrap();
        let mut path = dir.path().to_path_buf();
        path.push("test_patterns.db");
        PatternStore::init_db_at(&path).unwrap()
    }

    fn create_test_pattern(pattern_type: PatternType) -> Pattern {
        let metadata = match pattern_type {
            PatternType::Failure => PatternMetadata::Failure {
                error_type: "TestError".to_string(),
                error_message: "Something went wrong".to_string(),
                stack_trace: Some("at test.rs:10".to_string()),
            },
            PatternType::ToolSequence => PatternMetadata::ToolSequence {
                tools: vec!["read_file".to_string(), "write_file".to_string()],
                duration_ms: 1500,
                success_rate: 0.95,
            },
            PatternType::PromptTemplate => PatternMetadata::PromptTemplate {
                template: "Analyze {input}".to_string(),
                variables: vec!["input".to_string()],
                model: "gpt-4".to_string(),
            },
        };

        Pattern::new(
            pattern_type,
            "Test pattern description".to_string(),
            vec![PatternExample {
                run_id: "run-123".to_string(),
                timestamp: Utc::now(),
                context: serde_json::json!({"key": "value"}),
            }],
            metadata,
        )
    }

    #[test]
    fn test_save_and_get_pattern() {
        let mut store = create_test_store();
        let pattern = create_test_pattern(PatternType::Failure);
        let id = pattern.id.clone();

        store.save_pattern(&pattern).unwrap();
        let retrieved = store.get_pattern_by_id(&id).unwrap().unwrap();

        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.pattern_type, PatternType::Failure);
        assert_eq!(retrieved.description, "Test pattern description");
        assert_eq!(retrieved.frequency, 1);
        assert_eq!(retrieved.examples.len(), 1);
    }

    #[test]
    fn test_increment_frequency() {
        let mut store = create_test_store();
        let pattern = create_test_pattern(PatternType::ToolSequence);
        let id = pattern.id.clone();

        store.save_pattern(&pattern).unwrap();
        let freq1 = store.increment_frequency(&id).unwrap();
        assert_eq!(freq1, 2);

        let freq2 = store.increment_frequency(&id).unwrap();
        assert_eq!(freq2, 3);

        let retrieved = store.get_pattern_by_id(&id).unwrap().unwrap();
        assert_eq!(retrieved.frequency, 3);
    }

    #[test]
    fn test_get_patterns_filtered() {
        let mut store = create_test_store();

        store.save_pattern(&create_test_pattern(PatternType::Failure)).unwrap();
        store.save_pattern(&create_test_pattern(PatternType::Failure)).unwrap();
        store.save_pattern(&create_test_pattern(PatternType::ToolSequence)).unwrap();

        let failures = store.get_patterns(Some(PatternType::Failure)).unwrap();
        assert_eq!(failures.len(), 2);

        let sequences = store.get_patterns(Some(PatternType::ToolSequence)).unwrap();
        assert_eq!(sequences.len(), 1);

        let all = store.get_patterns(None).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_delete_pattern() {
        let mut store = create_test_store();
        let pattern = create_test_pattern(PatternType::PromptTemplate);
        let id = pattern.id.clone();

        store.save_pattern(&pattern).unwrap();
        assert!(store.get_pattern_by_id(&id).unwrap().is_some());

        let deleted = store.delete_pattern(&id).unwrap();
        assert!(deleted);
        assert!(store.get_pattern_by_id(&id).unwrap().is_none());

        let deleted_again = store.delete_pattern(&id).unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn test_count_patterns() {
        let mut store = create_test_store();

        assert_eq!(store.count_patterns(None).unwrap(), 0);

        store.save_pattern(&create_test_pattern(PatternType::Failure)).unwrap();
        store.save_pattern(&create_test_pattern(PatternType::Failure)).unwrap();
        store.save_pattern(&create_test_pattern(PatternType::ToolSequence)).unwrap();

        assert_eq!(store.count_patterns(None).unwrap(), 3);
        assert_eq!(store.count_patterns(Some(PatternType::Failure)).unwrap(), 2);
        assert_eq!(store.count_patterns(Some(PatternType::ToolSequence)).unwrap(), 1);
        assert_eq!(store.count_patterns(Some(PatternType::PromptTemplate)).unwrap(), 0);
    }
}
