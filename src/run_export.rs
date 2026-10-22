use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use crate::memory::{MemoryError, MemoryStore, memory_file_path, config_memory_file_path};

/// Run record stored in the run history
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub timestamp: u128,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: String,
    pub status: RunStatus,
    pub started_at: u128,
    pub ended_at: Option<u128>,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Step record within a run
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub step_id: String,
    pub run_id: String,
    pub step_index: usize,
    pub name: String,
    pub step_type: StepType,
    pub status: StepStatus,
    pub started_at: u128,
    pub ended_at: Option<u128>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub input_summary: String,
    pub output_summary: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepType {
    Llm,
    Tool,
    Shell,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

/// Artifact produced during a run
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub artifact_id: String,
    pub run_id: String,
    pub step_id: Option<String>,
    pub name: String,
    pub artifact_type: ArtifactType,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mime_type: Option<String>,
    pub created_at: u128,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactType {
    File,
    Directory,
    Log,
    Output,
    Custom,
}

/// Complete export data structure
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportData {
    pub version: u32,
    pub exported_at: u128,
    pub run: RunRecord,
    pub steps: Vec<StepRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub summary: ExportSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportSummary {
    pub total_steps: usize,
    pub completed_steps: usize,
    pub failed_steps: usize,
    pub total_tokens: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub total_artifacts: usize,
    pub total_artifact_size_bytes: u64,
    pub duration_ms: Option<u128>,
}

/// Storage for run records (JSONL format)
#[derive(Debug)]
pub struct RunStore {
    path: PathBuf,
}

impl RunStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, RunExportError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(RunExportError::Io)?;
            }
        }
        Ok(Self { path })
    }

    /// Opens the workspace-scoped run history file.
    pub fn open(workspace_dir: impl AsRef<Path>) -> Result<Self, RunExportError> {
        Self::new(run_file_path(workspace_dir.as_ref()))
    }

    /// Opens run store in workspace or config directory.
    pub fn open_in_workspace_or_config(workspace_dir: Option<&Path>) -> Result<Self, RunExportError> {
        let path = match workspace_dir {
            Some(dir) => run_file_path(dir),
            None => default_run_file_path(),
        };
        Self::new(path)
    }

    pub fn add_run(&self, run: &RunRecord) -> Result<(), RunExportError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(RunExportError::Io)?;
        let line = serde_json::to_string(run)
            .map_err(|err| RunExportError::Parse(format!("failed to serialize run record: {err}")))?;
        writeln!(file, "{}", line).map_err(RunExportError::Io)?;
        file.sync_all().map_err(RunExportError::Io)?;
        Ok(())
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<RunRecord>, RunExportError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let file = fs::File::open(&self.path).map_err(RunExportError::Io)?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.map_err(RunExportError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(run) = serde_json::from_str::<RunRecord>(trimmed) {
                if run.run_id == run_id {
                    return Ok(Some(run));
                }
            }
        }
        Ok(None)
    }

    pub fn list_runs(&self) -> Result<Vec<RunRecord>, RunExportError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path).map_err(RunExportError::Io)?;
        let reader = BufReader::new(file);
        let mut runs = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(RunExportError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(run) = serde_json::from_str::<RunRecord>(trimmed) {
                runs.push(run);
            }
        }
        runs.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(runs)
    }
}

/// Storage for step records (JSONL format)
#[derive(Debug)]
pub struct StepStore {
    path: PathBuf,
}

impl StepStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, RunExportError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(RunExportError::Io)?;
            }
        }
        Ok(Self { path })
    }

    pub fn open(workspace_dir: impl AsRef<Path>) -> Result<Self, RunExportError> {
        Self::new(step_file_path(workspace_dir.as_ref()))
    }

    pub fn open_in_workspace_or_config(workspace_dir: Option<&Path>) -> Result<Self, RunExportError> {
        let path = match workspace_dir {
            Some(dir) => step_file_path(dir),
            None => default_step_file_path(),
        };
        Self::new(path)
    }

    pub fn add_step(&self, step: &StepRecord) -> Result<(), RunExportError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(RunExportError::Io)?;
        let line = serde_json::to_string(step)
            .map_err(|err| RunExportError::Parse(format!("failed to serialize step record: {err}")))?;
        writeln!(file, "{}", line).map_err(RunExportError::Io)?;
        file.sync_all().map_err(RunExportError::Io)?;
        Ok(())
    }

    pub fn get_steps_for_run(&self, run_id: &str) -> Result<Vec<StepRecord>, RunExportError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path).map_err(RunExportError::Io)?;
        let reader = BufReader::new(file);
        let mut steps = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(RunExportError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(step) = serde_json::from_str::<StepRecord>(trimmed) {
                if step.run_id == run_id {
                    steps.push(step);
                }
            }
        }
        steps.sort_by_key(|s| s.step_index);
        Ok(steps)
    }
}

/// Storage for artifact records (JSONL format)
#[derive(Debug)]
pub struct ArtifactStore {
    path: PathBuf,
}

impl ArtifactStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, RunExportError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(RunExportError::Io)?;
            }
        }
        Ok(Self { path })
    }

    pub fn open(workspace_dir: impl AsRef<Path>) -> Result<Self, RunExportError> {
        Self::new(artifact_file_path(workspace_dir.as_ref()))
    }

    pub fn open_in_workspace_or_config(workspace_dir: Option<&Path>) -> Result<Self, RunExportError> {
        let path = match workspace_dir {
            Some(dir) => artifact_file_path(dir),
            None => default_artifact_file_path(),
        };
        Self::new(path)
    }

    pub fn add_artifact(&self, artifact: &ArtifactRecord) -> Result<(), RunExportError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(RunExportError::Io)?;
        let line = serde_json::to_string(artifact)
            .map_err(|err| RunExportError::Parse(format!("failed to serialize artifact record: {err}")))?;
        writeln!(file, "{}", line).map_err(RunExportError::Io)?;
        file.sync_all().map_err(RunExportError::Io)?;
        Ok(())
    }

    pub fn get_artifacts_for_run(&self, run_id: &str) -> Result<Vec<ArtifactRecord>, RunExportError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path).map_err(RunExportError::Io)?;
        let reader = BufReader::new(file);
        let mut artifacts = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(RunExportError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(artifact) = serde_json::from_str::<ArtifactRecord>(trimmed) {
                if artifact.run_id == run_id {
                    artifacts.push(artifact);
                }
            }
        }
        Ok(artifacts)
    }
}

#[derive(Debug)]
pub enum RunExportError {
    Io(std::io::Error),
    NotFound(String),
    Invalid(String),
    Parse(String),
}

impl fmt::Display for RunExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunExportError::Io(err) => write!(f, "run export io error: {err}"),
            RunExportError::NotFound(id) => write!(f, "run not found: {id}"),
            RunExportError::Invalid(msg) => write!(f, "invalid run export: {msg}"),
            RunExportError::Parse(msg) => write!(f, "failed to parse run export file: {msg}"),
        }
    }
}

impl Error for RunExportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RunExportError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RunExportError {
    fn from(err: std::io::Error) -> Self {
        RunExportError::Io(err)
    }
}

impl From<MemoryError> for RunExportError {
    fn from(err: MemoryError) -> Self {
        RunExportError::Io(std::io::Error::other(err.to_string()))
    }
}

fn run_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".workbuddy").join("runs.jsonl")
}

fn step_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".workbuddy").join("steps.jsonl")
}

fn artifact_file_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".workbuddy").join("artifacts.jsonl")
}

fn default_run_file_path() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".workbuddy").join("runs.jsonl"))
        .unwrap_or_else(|| PathBuf::from(".workbuddy").join("runs.jsonl"))
}

fn default_step_file_path() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".workbuddy").join("steps.jsonl"))
        .unwrap_or_else(|| PathBuf::from(".workbuddy").join("steps.jsonl"))
}

fn default_artifact_file_path() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".workbuddy").join("artifacts.jsonl"))
        .unwrap_or_else(|| PathBuf::from(".workbuddy").join("artifacts.jsonl"))
}

/// Export a complete run record including steps and artifacts
pub fn export_run(
    run_id: &str,
    workspace_dir: Option<&Path>,
    include_artifacts: bool,
) -> Result<ExportData, RunExportError> {
    // Open stores
    let run_store = RunStore::open_in_workspace_or_config(workspace_dir)?;
    let step_store = StepStore::open_in_workspace_or_config(workspace_dir)?;
    let artifact_store = ArtifactStore::open_in_workspace_or_config(workspace_dir)?;

    // Get run record
    let run = run_store
        .get_run(run_id)?
        .ok_or_else(|| RunExportError::NotFound(run_id.to_string()))?;

    // Get steps for this run
    let steps = step_store.get_steps_for_run(run_id)?;

    // Get artifacts for this run
    let artifacts = if include_artifacts {
        artifact_store.get_artifacts_for_run(run_id)?
    } else {
        Vec::new()
    };

    // Calculate summary
    let summary = calculate_summary(&run, &steps, &artifacts);

    // Build export data
    let export_data = ExportData {
        version: 1,
        exported_at: now_timestamp_millis(),
        run,
        steps,
        artifacts,
        summary,
    };

    Ok(export_data)
}

/// Write export data to a file or stdout
pub fn write_export(
    export_data: &ExportData,
    output: Option<&Path>,
) -> Result<(), RunExportError> {
    let json = serde_json::to_string_pretty(export_data)
        .map_err(|err| RunExportError::Parse(format!("failed to serialize export data: {err}")))?;

    match output {
        Some(path) => {
            let mut file = File::create(path).map_err(RunExportError::Io)?;
            file.write_all(json.as_bytes()).map_err(RunExportError::Io)?;
            file.write_all(b"\n").map_err(RunExportError::Io)?;
            file.sync_all().map_err(RunExportError::Io)?;
        }
        None => {
            let mut stdout = std::io::stdout();
            stdout.write_all(json.as_bytes()).map_err(RunExportError::Io)?;
            stdout.write_all(b"\n").map_err(RunExportError::Io)?;
            stdout.flush().map_err(RunExportError::Io)?;
        }
    }
    Ok(())
}

fn calculate_summary(
    run: &RunRecord,
    steps: &[StepRecord],
    artifacts: &[ArtifactRecord],
) -> ExportSummary {
    let total_steps = steps.len();
    let completed_steps = steps.iter().filter(|s| s.status == StepStatus::Completed).count();
    let failed_steps = steps.iter().filter(|s| s.status == StepStatus::Failed).count();

    let total_input_tokens: u64 = steps.iter().map(|s| s.input_tokens).sum();
    let total_output_tokens: u64 = steps.iter().map(|s| s.output_tokens).sum();
    let total_tokens: u64 = steps.iter().map(|s| s.total_tokens).sum();
    let total_cost_usd: f64 = steps.iter().map(|s| s.cost_usd).sum();

    let total_artifacts = artifacts.len();
    let total_artifact_size_bytes: u64 = artifacts.iter().map(|a| a.size_bytes).sum();

    let duration_ms = run.ended_at.map(|ended| ended.saturating_sub(run.started_at));

    ExportSummary {
        total_steps,
        completed_steps,
        failed_steps,
        total_tokens,
        total_input_tokens,
        total_output_tokens,
        total_cost_usd,
        total_artifacts,
        total_artifact_size_bytes,
        duration_ms,
    }
}

fn now_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    static NEXT_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> PathBuf {
        let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "workbuddy-run-export-test-{}-{}",
            std::process::id(),
            id
        ))
    }

    fn create_test_run(workspace: &Path) -> (RunRecord, Vec<StepRecord>, Vec<ArtifactRecord>) {
        let run = RunRecord {
            run_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            timestamp: 1700000000000,
            command: "workbuddy".to_string(),
            args: vec!["run".to_string(), "test".to_string()],
            working_dir: workspace.to_string_lossy().to_string(),
            status: RunStatus::Completed,
            started_at: 1700000000000,
            ended_at: Some(1700000005000),
            total_tokens: 150,
            total_cost_usd: 0.0003,
            metadata: serde_json::json!({}),
        };

        let steps = vec![
            StepRecord {
                step_id: "step-1".to_string(),
                run_id: run.run_id.clone(),
                step_index: 0,
                name: "analyze".to_string(),
                step_type: StepType::Llm,
                status: StepStatus::Completed,
                started_at: 1700000000000,
                ended_at: Some(1700000002000),
                input_tokens: 50,
                output_tokens: 30,
                total_tokens: 80,
                cost_usd: 0.00016,
                input_summary: "Analyze the codebase".to_string(),
                output_summary: "Found 3 issues".to_string(),
                error: None,
                metadata: serde_json::json!({}),
            },
            StepRecord {
                step_id: "step-2".to_string(),
                run_id: run.run_id.clone(),
                step_index: 1,
                name: "fix".to_string(),
                step_type: StepType::Tool,
                status: StepStatus::Completed,
                started_at: 1700000002000,
                ended_at: Some(1700000005000),
                input_tokens: 40,
                output_tokens: 30,
                total_tokens: 70,
                cost_usd: 0.00014,
                input_summary: "Fix issues".to_string(),
                output_summary: "Fixed 3 issues".to_string(),
                error: None,
                metadata: serde_json::json!({}),
            },
        ];

        let artifacts = vec![
            ArtifactRecord {
                artifact_id: "artifact-1".to_string(),
                run_id: run.run_id.clone(),
                step_id: Some("step-1".to_string()),
                name: "analysis.json".to_string(),
                artifact_type: ArtifactType::File,
                path: workspace.join("analysis.json"),
                size_bytes: 1024,
                mime_type: Some("application/json".to_string()),
                created_at: 1700000002000,
                metadata: serde_json::json!({}),
            },
        ];

        (run, steps, artifacts)
    }

    #[test]
    fn export_run_roundtrip() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).unwrap();

        let (run, steps, artifacts) = create_test_run(&workspace);

        // Write test data to stores
        let run_store = RunStore::open(&workspace).unwrap();
        run_store.add_run(&run).unwrap();

        let step_store = StepStore::open(&workspace).unwrap();
        for step in &steps {
            step_store.add_step(step).unwrap();
        }

        let artifact_store = ArtifactStore::open(&workspace).unwrap();
        for artifact in &artifacts {
            artifact_store.add_artifact(artifact).unwrap();
        }

        // Export
        let export_data = export_run(&run.run_id, Some(&workspace), true).unwrap();

        // Verify
        assert_eq!(export_data.version, 1);
        assert_eq!(export_data.run.run_id, run.run_id);
        assert_eq!(export_data.steps.len(), 2);
        assert_eq!(export_data.artifacts.len(), 1);
        assert_eq!(export_data.summary.total_steps, 2);
        assert_eq!(export_data.summary.completed_steps, 2);
        assert_eq!(export_data.summary.failed_steps, 0);
        assert_eq!(export_data.summary.total_tokens, 150);
        assert_eq!(export_data.summary.total_input_tokens, 90);
        assert_eq!(export_data.summary.total_output_tokens, 60);
        assert!((export_data.summary.total_cost_usd - 0.0003).abs() < f64::EPSILON);
        assert_eq!(export_data.summary.total_artifacts, 1);
        assert_eq!(export_data.summary.total_artifact_size_bytes, 1024);
        assert_eq!(export_data.summary.duration_ms, Some(5000));

        // Test write to file
        let output_path = workspace.join("export.json");
        write_export(&export_data, Some(&output_path)).unwrap();
        assert!(output_path.exists());

        // Verify file content
        let content = fs::read_to_string(&output_path).unwrap();
        let parsed: ExportData = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed, export_data);
    }

    #[test]
    fn export_run_without_artifacts() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).unwrap();

        let (run, steps, _artifacts) = create_test_run(&workspace);

        let run_store = RunStore::open(&workspace).unwrap();
        run_store.add_run(&run).unwrap();

        let step_store = StepStore::open(&workspace).unwrap();
        for step in &steps {
            step_store.add_step(step).unwrap();
        }

        // Export without artifacts
        let export_data = export_run(&run.run_id, Some(&workspace), false).unwrap();
        assert_eq!(export_data.artifacts.len(), 0);
        assert_eq!(export_data.summary.total_artifacts, 0);
    }

    #[test]
    fn export_run_not_found() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).unwrap();

        let err = export_run("nonexistent", Some(&workspace), true).unwrap_err();
        assert!(matches!(err, RunExportError::NotFound(_)));
    }

    #[test]
    fn export_data_serialization() {
        let (run, steps, artifacts) = create_test_run(&temp_workspace());
        let summary = calculate_summary(&run, &steps, &artifacts);

        let export_data = ExportData {
            version: 1,
            exported_at: 1700000010000,
            run: run.clone(),
            steps: steps.clone(),
            artifacts: artifacts.clone(),
            summary,
        };

        let json = serde_json::to_string_pretty(&export_data).unwrap();
        let parsed: ExportData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, export_data);
    }

    #[test]
    fn write_export_to_stdout() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).unwrap();

        let (run, steps, artifacts) = create_test_run(&workspace);
        let summary = calculate_summary(&run, &steps, &artifacts);

        let export_data = ExportData {
            version: 1,
            exported_at: now_timestamp_millis(),
            run,
            steps,
            artifacts,
            summary,
        };

        // This should not panic
        write_export(&export_data, None).unwrap();
    }
}
