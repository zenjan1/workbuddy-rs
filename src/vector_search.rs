//! Vector Search Module for Semantic Similarity Search
//! 
//! This module provides functionality to generate embeddings for RunRecords,
//! store them in a sqlite-vec index, and perform semantic similarity searches.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

// Re-export types from workspace
use crate::workspace::{RunRecord, StepRecord, StepStatus};

/// Configuration for the vector search engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSearchConfig {
    /// Path to the SQLite database with vec extension
    pub db_path: String,
    /// Embedding model endpoint (for text-embeddings-inference)
    pub embedding_endpoint: String,
    /// Embedding dimension (depends on model)
    pub embedding_dim: usize,
    /// Maximum number of results to return
    pub default_top_k: usize,
}

impl Default for VectorSearchConfig {
    fn default() -> Self {
        Self {
            db_path: "./data/vector_index.db".to_string(),
            embedding_endpoint: "http://localhost:8080".to_string(), // TEI default
            embedding_dim: 384, // all-MiniLM-L6-v2 dimension
            default_top_k: 5,
        }
    }
}

/// Search result with similarity score and highlighted differences
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub run_id: String,
    pub similarity: f32,
    pub command: String,
    pub args: Vec<String>,
    pub started_at: String,
    pub status: String,
    pub highlighted_differences: Vec<String>,
}

/// Vector search engine using sqlite-vec
pub struct VectorSearchEngine {
    config: VectorSearchConfig,
    db: Arc<Mutex<rusqlite::Connection>>,
    http_client: reqwest::Client,
}

impl VectorSearchEngine {
    /// Create a new vector search engine instance
    pub async fn new(config: VectorSearchConfig) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = Path::new(&config.db_path).parent() {
            std::fs::create_dir_all(parent).context("Failed to create database directory")?;
        }

        // Open SQLite connection with vec extension
        let db = rusqlite::Connection::open(&config.db_path)
            .context("Failed to open SQLite database")?;

        // Load sqlite-vec extension
        #[cfg(feature = "sqlite-vec")]
        {
            // The vec0 extension should be loaded automatically if compiled in
            // For dynamic loading, we'd use: db.load_extension("vec0", None)?;
        }

        // Initialize schema
        Self::init_schema(&db).context("Failed to initialize vector schema")?;

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            config,
            db: Arc::new(Mutex::new(db)),
            http_client,
        })
    }

    /// Initialize the database schema for vector storage
    fn init_schema(db: &rusqlite::Connection) -> Result<()> {
        // Create runs table if not exists (metadata)
        db.execute(
            r#"
            CREATE TABLE IF NOT EXISTS runs (
                run_id TEXT PRIMARY KEY,
                command TEXT NOT NULL,
                args TEXT NOT NULL, -- JSON array
                started_at TEXT NOT NULL,
                status TEXT NOT NULL,
                embedding_text TEXT NOT NULL
            )"#,
            [],
        ).context("Failed to create runs table")?;

        // Create virtual table for vector index using sqlite-vec
        // vec0 requires: CREATE VIRTUAL TABLE vec_table USING vec0(embedding float[384]);
        db.execute(
            &format!(
                r#"CREATE VIRTUAL TABLE IF NOT EXISTS run_embeddings USING vec0(
                    run_id TEXT PRIMARY KEY,
                    embedding float[{}]
                )"#,
                384 // Default dimension, will be configurable
            ),
            [],
        ).context("Failed to create vector index table")?;

        Ok(())
    }

    /// Build embedding text from RunRecord and its steps
    pub fn build_embedding_text(record: &RunRecord, steps: &[StepRecord]) -> String {
        let mut parts = Vec::new();

        // 1. Target/Command (highest weight)
        parts.push(format!("目标: {}", record.command));
        if !record.args.is_empty() {
            parts.push(format!("参数: {}", record.args.join(" ")));
        }

        // 2. Key steps summary (completed steps, max 5)
        let key_steps: Vec<String> = steps
            .iter()
            .filter(|s| s.status == StepStatus::Completed)
            .take(5)
            .map(|s| {
                let summary = s.summary.as_deref().unwrap_or("无摘要");
                format!("步骤[{}]: {} -> {}", s.step_id, s.action, summary)
            })
            .collect();

        if !key_steps.is_empty() {
            parts.push(format!("关键步骤: {}", key_steps.join("; ")));
        }

        // 3. Outcome
        parts.push(format!("结果: {}", record.status));

        parts.join(" | ")
    }

    /// Generate embedding for a given text using the embedding endpoint
    async fn generate_embedding(&self, text: &str) -> Result<Vec<f32>> {
        let request_body = serde_json::json!({
            "inputs": text,
            "truncate": true
        });

        let response = self.http_client
            .post(&format!("{}/embed", self.config.embedding_endpoint))
            .json(&request_body)
            .send()
            .await
            .context("Failed to send embedding request")?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Embedding request failed: {}", error_text);
        }

        let embeddings: Vec<Vec<f32>> = response.json().await
            .context("Failed to parse embedding response")?;

        // TEI returns [[embedding]] for single input
        let embedding = embeddings.into_iter().next()
            .context("Empty embedding response")?;

        // Validate dimension
        if embedding.len() != self.config.embedding_dim {
            anyhow::bail!(
                "Embedding dimension mismatch: expected {}, got {}",
                self.config.embedding_dim,
                embedding.len()
            );
        }

        Ok(embedding)
    }

    /// Insert or update embedding for a run
    pub async fn upsert_run_embedding(&self, record: &RunRecord, steps: &[StepRecord]) -> Result<()> {
        let embedding_text = Self::build_embedding_text(record, steps);
        let embedding = self.generate_embedding(&embedding_text).await?;

        let db = self.db.lock().await;
        
        // Insert/update run metadata
        let args_json = serde_json::to_string(&record.args)?;
        db.execute(
            r#"INSERT OR REPLACE INTO runs (run_id, command, args, started_at, status, embedding_text)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)"#,
            rusqlite::params![
                record.run_id,
                record.command,
                args_json,
                record.started_at.to_rfc3339(),
                record.status,
                embedding_text
            ],
        ).context("Failed to upsert run metadata")?;

        // Insert/update vector embedding
        // sqlite-vec uses: INSERT INTO vec_table (run_id, embedding) VALUES (?, ?)
        // where embedding is a blob of float32 values
        let embedding_blob: Vec<u8> = embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        db.execute(
            "INSERT OR REPLACE INTO run_embeddings (run_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![record.run_id, embedding_blob],
        ).context("Failed to upsert vector embedding")?;

        Ok(())
    }

    /// Search for similar runs by natural language query
    pub async fn search(&self, query: &str, top_k: Option<usize>) -> Result<Vec<SearchResult>> {
        let k = top_k.unwrap_or(self.config.default_top_k);

        // Generate embedding for query
        let query_embedding = self.generate_embedding(query).await?;
        let query_blob: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let db = self.db.lock().await;

        // Perform vector similarity search using sqlite-vec
        // The vec0 extension provides a distance function
        // We use: SELECT run_id, distance FROM run_embeddings WHERE embedding MATCH ? ORDER BY distance LIMIT ?
        let mut stmt = db.prepare(
            r#"
            SELECT 
                re.run_id,
                vec_distance_cosine(re.embedding, ?1) as distance,
                r.command,
                r.args,
                r.started_at,
                r.status,
                r.embedding_text
            FROM run_embeddings re
            JOIN runs r ON re.run_id = r.run_id
            WHERE re.embedding MATCH ?1
            ORDER BY distance ASC
            LIMIT ?2
            "#
        ).context("Failed to prepare search query")?;

        let rows = stmt.query_map(rusqlite::params![query_blob, k as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,  // run_id
                row.get::<_, f32>(1)?,     // distance (cosine)
                row.get::<_, String>(2)?,  // command
                row.get::<_, String>(3)?,  // args JSON
                row.get::<_, String>(4)?,  // started_at
                row.get::<_, String>(5)?,  // status
                row.get::<_, String>(6)?,  // embedding_text
            ))
        }).context("Failed to execute search query")?;

        let mut results = Vec::new();
        for row in rows {
            let (run_id, distance, command, args_json, started_at, status, embedding_text) = row?;
            let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
            
            // Convert cosine distance to similarity (1 - distance for cosine)
            let similarity = 1.0 - distance;

            // Generate highlighted differences
            let highlighted_differences = self.highlight_differences(query, &embedding_text);

            results.push(SearchResult {
                run_id,
                similarity,
                command,
                args,
                started_at,
                status,
                highlighted_differences,
            });
        }

        Ok(results)
    }

    /// Highlight key differences between query and stored embedding text
    fn highlight_differences(&self, query: &str, embedding_text: &str) -> Vec<String> {
        let mut differences = Vec::new();

        // Simple keyword-based difference highlighting
        let query_lower = query.to_lowercase();
        let text_lower = embedding_text.to_lowercase();

        // Extract key terms from query (split by spaces, filter short words)
        let query_terms: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .collect();

        // Check which terms are present/missing
        for term in query_terms {
            if text_lower.contains(term) {
                differences.push(format!("✓ 包含关键词: '{}'", term));
            } else {
                differences.push(format!("✗ 缺少关键词: '{}'", term));
            }
        }

        // Also highlight structural differences (command, steps, outcome)
        if embedding_text.contains("目标:") && !query_lower.contains("目标") {
            differences.push("📌 记录包含明确目标描述".to_string());
        }
        if embedding_text.contains("关键步骤:") && !query_lower.contains("步骤") {
            differences.push("📌 记录包含详细步骤信息".to_string());
        }

        differences
    }

    /// Get all run IDs that have embeddings (for batch operations)
    pub async fn get_all_run_ids(&self) -> Result<Vec<String>> {
        let db = self.db.lock().await;
        let mut stmt = db.prepare("SELECT run_id FROM runs")?;
        let run_ids: Result<Vec<String>, _> = stmt.query_map([], |row| row.get(0))?.collect();
        Ok(run_ids?)
    }

    /// Delete embedding for a run
    pub async fn delete_run_embedding(&self, run_id: &str) -> Result<()> {
        let db = self.db.lock().await;
        db.execute("DELETE FROM runs WHERE run_id = ?1", rusqlite::params![run_id])?;
        db.execute("DELETE FROM run_embeddings WHERE run_id = ?1", rusqlite::params![run_id])?;
        Ok(())
    }

    /// Rebuild index for all runs (useful after model change)
    pub async fn rebuild_index(&self, runs_with_steps: Vec<(&RunRecord, Vec<StepRecord>)>) -> Result<usize> {
        let mut count = 0;
        for (record, steps) in runs_with_steps {
            self.upsert_run_embedding(record, &steps).await?;
            count += 1;
        }
        Ok(count)
    }
}

/// Convenience function to create engine with default config
pub async fn create_default_engine() -> Result<VectorSearchEngine> {
    VectorSearchEngine::new(VectorSearchConfig::default()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{RunRecord, StepRecord, StepStatus};
    use chrono::Utc;
    use std::collections::HashMap;

    fn create_test_record(id: &str, command: &str, args: Vec<&str>) -> RunRecord {
        RunRecord {
            run_id: id.to_string(),
            command: command.to_string(),
            args: args.into_iter().map(String::from).collect(),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            status: "completed".to_string(),
            env_snapshot: HashMap::new(),
            artifacts: vec![],
        }
    }

    fn create_test_steps(run_id: &str, actions: Vec<(&str, &str)>) -> Vec<StepRecord> {
        actions
            .into_iter()
            .enumerate()
            .map(|(i, (action, summary))| StepRecord {
                step_id: format!("{}-step-{}", run_id, i + 1),
                run_id: run_id.to_string(),
                action: action.to_string(),
                status: StepStatus::Completed,
                started_at: Utc::now(),
                completed_at: Some(Utc::now()),
                input_ref: None,
                output_ref: None,
                summary: Some(summary.to_string()),
                error: None,
            })
            .collect()
    }

    #[tokio::test]
    async fn test_build_embedding_text() {
        let record = create_test_record(
            "test-1",
            "refactor",
            vec!["auth", "module"]
        );
        let steps = create_test_steps("test-1", vec![
            ("analyze", "分析 auth 模块结构"),
            ("extract", "提取接口定义"),
            ("implement", "实现新的认证逻辑"),
            ("test", "运行单元测试"),
        ]);

        let text = VectorSearchEngine::build_embedding_text(&record, &steps);
        assert!(text.contains("目标: refactor"));
        assert!(text.contains("参数: auth module"));
        assert!(text.contains("关键步骤:"));
        assert!(text.contains("分析 auth 模块结构"));
        assert!(text.contains("结果: completed"));
    }

    #[tokio::test]
    async fn test_highlight_differences() {
        let config = VectorSearchConfig::default();
        let engine = VectorSearchEngine {
            config,
            db: Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap())),
            http_client: reqwest::Client::new(),
        };

        let query = "重构 auth 模块";
        let embedding_text = "目标: refactor | 参数: auth module | 关键步骤: 步骤[1]: analyze -> 分析 auth 模块结构; 步骤[2]: implement -> 实现新认证逻辑 | 结果: completed";

        let diffs = engine.highlight_differences(query, embedding_text);
        assert!(diffs.iter().any(|d| d.contains("auth")));
        assert!(diffs.iter().any(|d| d.contains("模块") || d.contains("module")));
    }
}
