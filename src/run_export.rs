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

/// Per-task token usage summary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskTokenSummary {
    pub task_id: String,
    pub task_name: String,
    pub max_tokens: Option<u32>,
    pub tokens_used: u32,
    pub token_budget_exceeded: bool,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_token_summaries: Vec<TaskTokenSummary>,
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

    // Calculate per-task token summaries (placeholder - tasks would come from TaskStore)
    let task_token_summaries = calculate_task_token_summaries(run_id, workspace_dir, &steps)?;

    // Build export data
    let export_data = ExportData {
        version: 1,
        exported_at: now_timestamp_millis(),
        run,
        steps,
        artifacts,
        summary,
        task_token_summaries,
    };

    Ok(export_data)
}

/// Calculate per-task token summaries from steps and task metadata.
/// This is a placeholder implementation; in a full implementation, this would
/// query a TaskStore for task definitions (including max_tokens) and aggregate
/// step tokens per task.
fn calculate_task_token_summaries(
    run_id: &str,
    workspace_dir: Option<&Path>,
    steps: &[StepRecord],
) -> Result<Vec<TaskTokenSummary>, RunExportError> {
    // TODO: In a complete implementation, fetch TaskRecords from a TaskStore
    // using run_id and workspace_dir, then aggregate step tokens per task.
    // For now, return empty vector to maintain compatibility.
    //
    // Example of what the full implementation would do:
    // let task_store = TaskStore::open_in_workspace_or_config(workspace_dir)?;
    // let tasks = task_store.get_tasks_for_run(run_id)?;
    // for task in tasks {
    //     let task_steps: Vec<&StepRecord> = steps.iter()
    //         .filter(|s| s.metadata.get("task_id") == Some(&serde_json::Value::String(task.task_id.clone())))
    //         .collect();
    //     let tokens_used: u32 = task_steps.iter().map(|s| s.total_tokens as u32).sum();
    //     let token_budget_exceeded = task.max_tokens.map_or(false, |max| tokens_used > max);
    //     summaries.push(TaskTokenSummary {
    //         task_id: task.task_id,
    //         task_name: task.name,
    //         max_tokens: task.max_tokens,
    //         tokens_used,
    //         token_budget_exceeded,
    //     });
    // }
    Ok(Vec::new())
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

/// Export format for run export command
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Json,
    Timeline,
}

/// Timeline output format (for timeline-specific options)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineFormat {
    Json,
    Ascii,
    Mermaid,
}

/// Timeline event representing a single step in the run
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub step_id: String,
    pub step_index: usize,
    pub name: String,
    pub step_type: StepType,
    pub status: StepStatus,
    pub started_at: u128,
    pub ended_at: Option<u128>,
    pub duration_ms: Option<u128>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub input_summary: String,
    pub output_summary: String,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
}

/// Complete timeline export data
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineExport {
    pub run_id: String,
    pub run_started_at: u128,
    pub run_ended_at: Option<u128>,
    pub run_duration_ms: Option<u128>,
    pub total_steps: usize,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub events: Vec<TimelineEvent>,
}

impl From<&StepRecord> for TimelineEvent {
    fn from(step: &StepRecord) -> Self {
        let duration_ms = step.ended_at.map(|ended| ended.saturating_sub(step.started_at));
        Self {
            step_id: step.step_id.clone(),
            step_index: step.step_index,
            name: step.name.clone(),
            step_type: step.step_type.clone(),
            status: step.status.clone(),
            started_at: step.started_at,
            ended_at: step.ended_at,
            duration_ms,
            input_tokens: step.input_tokens,
            output_tokens: step.output_tokens,
            total_tokens: step.total_tokens,
            cost_usd: step.cost_usd,
            input_summary: step.input_summary.clone(),
            output_summary: step.output_summary.clone(),
            error: step.error.clone(),
            metadata: step.metadata.clone(),
        }
    }
}

/// Export timeline for a run
pub fn export_timeline(
    run_id: &str,
    format: TimelineFormat,
    workspace_dir: Option<&Path>,
) -> Result<String, RunExportError> {
    // Open stores
    let run_store = RunStore::open_in_workspace_or_config(workspace_dir)?;
    let step_store = StepStore::open_in_workspace_or_config(workspace_dir)?;

    // Get run record
    let run = run_store
        .get_run(run_id)?
        .ok_or_else(|| RunExportError::NotFound(run_id.to_string()))?;

    // Get steps for this run
    let mut steps = step_store.get_steps_for_run(run_id)?;
    
    // Sort by timestamp (started_at)
    steps.sort_by_key(|s| s.started_at);

    // Convert to timeline events
    let events: Vec<TimelineEvent> = steps.iter().map(TimelineEvent::from).collect();

    // Calculate run duration
    let run_duration_ms = run.ended_at.map(|ended| ended.saturating_sub(run.started_at));

    // Build timeline export
    let timeline = TimelineExport {
        run_id: run.run_id.clone(),
        run_started_at: run.started_at,
        run_ended_at: run.ended_at,
        run_duration_ms,
        total_steps: events.len(),
        total_tokens: events.iter().map(|e| e.total_tokens).sum(),
        total_cost_usd: events.iter().map(|e| e.cost_usd).sum(),
        events,
    };

    // Serialize based on format
    match format {
        TimelineFormat::Json => serialize_timeline_json(&timeline),
        TimelineFormat::Ascii => serialize_timeline_ascii(&timeline),
    }
}

/// Serialize timeline as JSON
fn serialize_timeline_json(timeline: &TimelineExport) -> Result<String, RunExportError> {
    serde_json::to_string_pretty(timeline)
        .map_err(|err| RunExportError::Parse(format!("failed to serialize timeline as JSON: {err}")))
}

/// Serialize timeline as Mermaid sequence diagram
fn serialize_timeline_mermaid(timeline: &TimelineExport) -> Result<String, RunExportError> {
    let mut output = String::new();
    output.push_str("sequenceDiagram\n");
    output.push_str(&format!("    participant Run as Run ({})\n", timeline.run_id));
    
    for event in &timeline.events {
        let participant = match event.step_type {
            StepType::Llm => "LLM",
            StepType::Tool => "Tool",
            StepType::Shell => "Shell",
            StepType::Custom => "Custom",
        };
        output.push_str(&format!("    participant {} as {} ({})\n", event.step_id, participant, event.name));
    }
    
    output.push_str("\n");
    
    for event in &timeline.events {
        let participant = match event.step_type {
            StepType::Llm => "LLM",
            StepType::Tool => "Tool",
            StepType::Shell => "Shell",
            StepType::Custom => "Custom",
        };
        
        output.push_str(&format!("    activate {}\n", event.step_id));
        output.push_str(&format!("    Run->>{}: {} (tokens: {}, cost: ${:.6})\n", 
            event.step_id, event.name, event.total_tokens, event.cost_usd));
        
        if let Some(error) = &event.error {
            output.push_str(&format!("    note right of {}: ERROR: {}\n", event.step_id, error));
        } else {
            output.push_str(&format!("    note right of {}: Input: {}\n", event.step_id, event.input_summary));
            output.push_str(&format!("    note right of {}: Output: {}\n", event.step_id, event.output_summary));
        }
        
        if let Some(duration) = event.duration_ms {
            output.push_str(&format!("    note left of {}: Duration: {}ms\n", event.step_id, duration));
        }
        
        output.push_str(&format!("    deactivate {}\n", event.step_id));
        output.push_str(&format!("    {}-->>Run: Completed\n", event.step_id));
        output.push_str("\n");
    }
    
    Ok(output)
}

/// Export run with specified format
pub fn export_run_with_format(
    run_id: &str,
    format: ExportFormat,
    timeline_format: Option<TimelineFormat>,
    workspace_dir: Option<&Path>,
    output: Option<&Path>,
) -> Result<(), RunExportError> {
    match format {
        ExportFormat::Json => {
            let export_data = export_run(run_id, workspace_dir, true)?;
            write_export(&export_data, output)
        }
        ExportFormat::Timeline => {
            let tf = timeline_format.unwrap_or(TimelineFormat::Mermaid);
            let timeline_output = export_timeline(run_id, tf, workspace_dir)?;
            match output {
                Some(path) => {
                    let mut file = File::create(path).map_err(RunExportError::Io)?;
                    file.write_all(timeline_output.as_bytes()).map_err(RunExportError::Io)?;
                    file.write_all(b"\n").map_err(RunExportError::Io)?;
                    file.sync_all().map_err(RunExportError::Io)?;
                }
                None => {
                    let mut stdout = std::io::stdout();
                    stdout.write_all(timeline_output.as_bytes()).map_err(RunExportError::Io)?;
                    stdout.write_all(b"\n").map_err(RunExportError::Io)?;
                    stdout.flush().map_err(RunExportError::Io)?;
                }
            }
            Ok(())
        }
    }
}

/// Export timeline for a run
pub fn export_timeline(
    run_id: &str,
    format: TimelineFormat,
    workspace_dir: Option<&Path>,
) -> Result<String, RunExportError> {
    // Open stores
    let run_store = RunStore::open_in_workspace_or_config(workspace_dir)?;
    let step_store = StepStore::open_in_workspace_or_config(workspace_dir)?;

    // Get run record
    let run = run_store
        .get_run(run_id)?
        .ok_or_else(|| RunExportError::NotFound(run_id.to_string()))?;

    // Get steps for this run
    let mut steps = step_store.get_steps_for_run(run_id)?;
    
    // Sort by timestamp (started_at)
    steps.sort_by_key(|s| s.started_at);

    // Convert to timeline events
    let events: Vec<TimelineEvent> = steps.iter().map(TimelineEvent::from).collect();

    // Calculate run duration
    let run_duration_ms = run.ended_at.map(|ended| ended.saturating_sub(run.started_at));

    // Build timeline export
    let timeline = TimelineExport {
        run_id: run.run_id.clone(),
        run_started_at: run.started_at,
        run_ended_at: run.ended_at,
        run_duration_ms,
        total_steps: events.len(),
        total_tokens: events.iter().map(|e| e.total_tokens).sum(),
        total_cost_usd: events.iter().map(|e| e.cost_usd).sum(),
        events,
    };

    // Serialize based on format
    match format {
        TimelineFormat::Json => serialize_timeline_json(&timeline),
        TimelineFormat::Ascii => serialize_timeline_ascii(&timeline),
        TimelineFormat::Mermaid => serialize_timeline_mermaid(&timeline),
    }
}!("failed to serialize timeline json: {err}")))
}

/// Serialize timeline as ASCII
fn serialize_timeline_ascii(timeline: &TimelineExport) -> Result<String, RunExportError> {
    let mut out = String::new();
    out.push_str(&format!("Run: {} ({} steps)\n", timeline.run_id, timeline.total_steps));
    out.push_str(&format!("Started: {}ms\n", timeline.run_started_at));
    if let Some(ended) = timeline.run_ended_at {
        out.push_str(&format!("Ended: {}ms\n", ended));
    }
    if let Some(dur) = timeline.run_duration_ms {
        out.push_str(&format!("Duration: {}ms\n", dur));
    }
    out.push_str(&format!("Total tokens: {}\n", timeline.total_tokens));
    out.push_str(&format!("Total cost: ${:.6}\n", timeline.total_cost_usd));
    out.push_str("\nEvents:\n");
    for event in &timeline.events {
        let dur = event.duration_ms.map(|d| format!("{}ms", d)).unwrap_or_else(|| "ongoing".to_string());
        out.push_str(&format!(
            "  [{}] {} ({:?}) - {} - tokens: {} - cost: ${:.6}\n",
            event.step_index, event.name, event.step_type, event.status, event.total_tokens, event.cost_usd
        ));
        out.push_str(&format!("    Duration: {}\n", dur));
        if !event.input_summary.is_empty() {
            out.push_str(&format!("    Input: {}\n", event.input_summary));
        }
        if !event.output_summary.is_empty() {
            out.push_str(&format!("    Output: {}\n", event.output_summary));
        }
        if let Some(err) = &event.error {
            out.push_str(&format!("    Error: {}\n", err));
        }
    }
    Ok(out)
}

/// Trait for exporting run data in various formats
pub trait Exporter {
    fn export(&self, record: &RunRecord) -> Result<String, RunExportError>;
}

/// Exporter that renders a run as a Mermaid sequenceDiagram
pub struct TimelineExporter {
    workspace_dir: Option<PathBuf>,
}

impl TimelineExporter {
    pub fn new(workspace_dir: Option<impl Into<PathBuf>>) -> Self {
        Self {
            workspace_dir: workspace_dir.map(Into::into),
        }
    }

    fn get_steps(&self, run_id: &str) -> Result<Vec<StepRecord>, RunExportError> {
        let step_store = StepStore::open_in_workspace_or_config(self.workspace_dir.as_deref())?;
        let mut steps = step_store.get_steps_for_run(run_id)?;
        steps.sort_by_key(|s| s.started_at);
        Ok(steps)
    }
}

impl Exporter for TimelineExporter {
    fn export(&self, record: &RunRecord) -> Result<String, RunExportError> {
        let steps = self.get_steps(&record.run_id)?;
        let mut mermaid = String::new();
        mermaid.push_str("```mermaid\n");
        mermaid.push_str("sequenceDiagram\n");
        mermaid.push_str(&format!("    autonumber\n"));

        // Collect unique participants
        let mut participants = std::collections::BTreeSet::new();
        participants.insert("User");
        participants.insert("Agent");
        for step in &steps {
            match step.step_type {
                StepType::Llm => participants.insert("LLM"),
                StepType::Tool => participants.insert(&step.name),
                StepType::Shell => participants.insert("Shell"),
                StepType::Custom => participants.insert(&step.name),
            };
        }

        // Declare participants
        for p in &participants {
            mermaid.push_str(&format!("    participant {}\n", p));
        }
        mermaid.push_str("\n");

        // Generate sequence for each step
        for step in &steps {
            let participant = match step.step_type {
                StepType::Llm => "LLM",
                StepType::Tool => &step.name,
                StepType::Shell => "Shell",
                StepType::Custom => &step.name,
            };

            mermaid.push_str(&format!("    activate {}\n", participant));

            let mut note_lines = vec![
                format!("Type: {:?}", step.step_type),
                format!("Status: {:?}", step.status),
                format!("Tokens: {} (in: {}, out: {})", step.total_tokens, step.input_tokens, step.output_tokens),
                format!("Cost: ${:.6}", step.cost_usd),
            ];
            if !step.input_summary.is_empty() {
                note_lines.push(format!("Input: {}", step.input_summary));
            }
            if !step.output_summary.is_empty() {
                note_lines.push(format!("Output: {}", step.output_summary));
            }
            if let Some(err) = &step.error {
                note_lines.push(format!("Error: {}", err));
            }
            let note_text = note_lines.join("\n");
            mermaid.push_str(&format!("    note right of {}: {}\n", participant, note_text.replace('\n', "<br>")));

            if let Some(ended) = step.ended_at {
                let dur = ended.saturating_sub(step.started_at);
                mermaid.push_str(&format!("    deactivate {}: Duration {}ms\n", participant, dur));
            } else {
                mermaid.push_str(&format!("    deactivate {}: Ongoing\n", participant));
            }
            mermaid.push_str("\n");
        }

        mermaid.push_str("```\n");
        Ok(mermaid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    #[test]
    fn test_timeline_exporter_mermaid_syntax() {
        let run = RunRecord {
            run_id: "test_run_1".to_string(),
            timestamp: now_ms(),
            command: "test".to_string(),
            args: vec![],
            working_dir: "/tmp".to_string(),
            status: RunStatus::Completed,
            started_at: now_ms(),
            ended_at: Some(now_ms() + 1000),
            total_tokens: 100,
            total_cost_usd: 0.001,
            metadata: serde_json::Value::Null,
        };

        let steps = vec![
            StepRecord {
                step_id: "step_1".to_string(),
                run_id: "test_run_1".to_string(),
                step_index: 0,
                name: "llm_call".to_string(),
                step_type: StepType::Llm,
                status: StepStatus::Completed,
                started_at: now_ms(),
                ended_at: Some(now_ms() + 100),
                input_tokens: 50,
                output_tokens: 30,
                total_tokens: 80,
                cost_usd: 0.0008,
                input_summary: "Hello".to_string(),
                output_summary: "Hi there".to_string(),
                error: None,
                metadata: serde_json::Value::Null,
            },
            StepRecord {
                step_id: "step_2".to_string(),
                run_id: "test_run_1".to_string(),
                step_index: 1,
                name: "read_file".to_string(),
                step_type: StepType::Tool,
                status: StepStatus::Completed,
                started_at: now_ms() + 100,
                ended_at: Some(now_ms() + 200),
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
                cost_usd: 0.0002,
                input_summary: "path: /tmp/test.txt".to_string(),
                output_summary: "file content".to_string(),
                error: None,
                metadata: serde_json::Value::Null,
            },
            StepRecord {
                step_id: "step_3".to_string(),
                run_id: "test_run_1".to_string(),
                step_index: 2,
                name: "shell_cmd".to_string(),
                step_type: StepType::Shell,
                status: StepStatus::Failed,
                started_at: now_ms() + 200,
                ended_at: Some(now_ms() + 300),
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cost_usd: 0.0,
                input_summary: "ls -la".to_string(),
                output_summary: "".to_string(),
                error: Some("permission denied".to_string()),
                metadata: serde_json::Value::Null,
            },
        ];

        // Create a temporary directory for test
        let temp_dir = tempfile::tempdir().unwrap();
        let step_store = StepStore::open(temp_dir.path()).unwrap();
        for step in &steps {
            step_store.add_step(step).unwrap();
        }

        let exporter = TimelineExporter::new(Some(temp_dir.path()));
        let result = exporter.export(&run).unwrap();

        // Verify mermaid syntax keywords
        assert!(result.starts_with("```mermaid"));
        assert!(result.ends_with("```\n"));
        assert!(result.contains("sequenceDiagram"));
        assert!(result.contains("participant User"));
        assert!(result.contains("participant Agent"));
        assert!(result.contains("participant LLM"));
        assert!(result.contains("participant read_file"));
        assert!(result.contains("participant Shell"));
        assert!(result.contains("activate"));
        assert!(result.contains("deactivate"));
        assert!(result.contains("note right of"));
        assert!(result.contains("Type: Llm"));
        assert!(result.contains("Type: Tool"));
        assert!(result.contains("Type: Shell"));
        assert!(result.contains("Status: Completed"));
        assert!(result.contains("Status: Failed"));
        assert!(result.contains("Error: permission denied"));
        assert!(result.contains("autonumber"));
    }
}
!("failed to serialize timeline as JSON: {err}")))
}

/// Serialize timeline as ASCII timeline
fn serialize_timeline_ascii(timeline: &TimelineExport) -> Result<String, RunExportError> {
    let mut output = String::new();
    
    // Header
    output.push_str(&format!("Timeline for run: {}\n", timeline.run_id));
    output.push_str(&format!("Started: {} (timestamp: {})\n", format_timestamp(timeline.run_started_at), timeline.run_started_at));
    if let Some(ended) = timeline.run_ended_at {
        output.push_str(&format!("Ended: {} (timestamp: {})\n", format_timestamp(ended), ended));
    }
    if let Some(duration) = timeline.run_duration_ms {
        output.push_str(&format!("Duration: {} ms\n", duration));
    }
    output.push_str(&format!("Total Steps: {}\n", timeline.total_steps));
    output.push_str(&format!("Total Tokens: {}\n", timeline.total_tokens));
    output.push_str(&format!("Total Cost: ${:.6}\n", timeline.total_cost_usd));
    output.push_str("\n");
    
    // Timeline events
    output.push_str("═══════════════════════════════════════════════════════════════════════════════\n");
    output.push_str("TIMELINE\n");
    output.push_str("═══════════════════════════════════════════════════════════════════════════════\n\n");

    for (idx, event) in timeline.events.iter().enumerate() {
        let step_num = idx + 1;
        let type_label = match event.step_type {
            StepType::Llm => "LLM",
            StepType::Tool => "TOOL",
            StepType::Shell => "SHELL",
            StepType::Custom => "CUSTOM",
        };
        
        let status_symbol = match event.status {
            StepStatus::Pending => "⏳",
            StepStatus::Running => "▶",
            StepStatus::Completed => "✓",
            StepStatus::Failed => "✗",
            StepStatus::Skipped => "⊘",
        };

        output.push_str(&format!("┌─ Step {}: {} [{}] {}\n", step_num, event.name, type_label, status_symbol));
        output.push_str(&format!("│  ID: {}\n", event.step_id));
        output.push_str(&format!("│  Started: {} (timestamp: {})\n", format_timestamp(event.started_at), event.started_at));
        
        if let Some(ended) = event.ended_at {
            output.push_str(&format!("│  Ended: {} (timestamp: {})\n", format_timestamp(ended), ended));
        }
        
        if let Some(duration) = event.duration_ms {
            output.push_str(&format!("│  Duration: {} ms\n", duration));
        }
        
        output.push_str(&format!("│  Tokens: {} in / {} out / {} total\n", event.input_tokens, event.output_tokens, event.total_tokens));
        output.push_str(&format!("│  Cost: ${:.6}\n", event.cost_usd));
        
        if !event.input_summary.is_empty() {
            let input_preview = truncate(&event.input_summary, 80);
            output.push_str(&format!("│  Input: {}\n", input_preview));
        }
        
        if !event.output_summary.is_empty() {
            let output_preview = truncate(&event.output_summary, 80);
            output.push_str(&format!("│  Output: {}\n", output_preview));
        }
        
        if let Some(error) = &event.error {
            let error_preview = truncate(error, 80);
            output.push_str(&format!("│  Error: {}\n", error_preview));
        }
        
        if idx < timeline.events.len() - 1 {
            output.push_str("├──────────────────────────────────────────────────────────────────────────────\n");
        } else {
            output.push_str("└──────────────────────────────────────────────────────────────────────────────\n");
        }
        output.push_str("\n");
    }

    // Summary footer
    output.push_str("═══════════════════════════════════════════════════════════════════════════════\n");
    output.push_str("SUMMARY\n");
    output.push_str("═══════════════════════════════════════════════════════════════════════════════\n");
    output.push_str(&format!("Total Steps: {}\n", timeline.total_steps));
    output.push_str(&format!("Completed: {}\n", timeline.events.iter().filter(|e| e.status == StepStatus::Completed).count()));
    output.push_str(&format!("Failed: {}\n", timeline.events.iter().filter(|e| e.status == StepStatus::Failed).count()));
    output.push_str(&format!("Total Tokens: {}\n", timeline.total_tokens));
    output.push_str(&format!("Total Cost: ${:.6}\n", timeline.total_cost_usd));
    if let Some(duration) = timeline.run_duration_ms {
        output.push_str(&format!("Total Duration: {} ms\n", duration));
    }

    Ok(output)
}

fn format_timestamp(timestamp: u128) -> String {
    // Convert milliseconds since epoch to human-readable format
    let secs = (timestamp / 1000) as u64;
    let millis = (timestamp % 1000) as u32;
    
    // Use chrono if available, otherwise simple format
    #[cfg(feature = "chrono")] {
        use chrono::{DateTime, Utc, TimeZone};
        if let Some(dt) = Utc.timestamp_millis_opt(secs as i64).single() {
            return format!("{}.{:03}", dt.format("%Y-%m-%d %H:%M:%S"), millis);
        }
    }
    
    // Fallback: just show the timestamp
    format!("{}.{:03}", secs, millis)
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
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

        // Test export
        let export = export_run(run.run_id.as_str(), Some(&workspace), true).unwrap();
        assert_eq!(export.run.run_id, run.run_id);
        assert_eq!(export.steps.len(), 2);
        assert_eq!(export.artifacts.len(), 1);
        assert_eq!(export.summary.total_steps, 2);
        assert_eq!(export.summary.completed_steps, 2);
        assert_eq!(export.summary.total_tokens, 150);
    }

    #[test]
    fn export_timeline_json() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).unwrap();

        let (run, steps, _artifacts) = create_test_run(&workspace);

        let run_store = RunStore::open(&workspace).unwrap();
        run_store.add_run(&run).unwrap();

        let step_store = StepStore::open(&workspace).unwrap();
        for step in &steps {
            step_store.add_step(step).unwrap();
        }

        let json = export_timeline(&run.run_id, TimelineFormat::Json, Some(&workspace)).unwrap();
        
        // Parse and verify
        let timeline: TimelineExport = serde_json::from_str(&json).unwrap();
        assert_eq!(timeline.run_id, run.run_id);
        assert_eq!(timeline.events.len(), 2);
        assert_eq!(timeline.total_steps, 2);
        assert_eq!(timeline.total_tokens, 150);
        
        // Verify events are sorted by timestamp
        assert_eq!(timeline.events[0].step_index, 0);
        assert_eq!(timeline.events[1].step_index, 1);
        assert!(timeline.events[0].started_at <= timeline.events[1].started_at);
        
        // Verify event fields
        assert_eq!(timeline.events[0].name, "analyze");
        assert_eq!(timeline.events[0].step_type, StepType::Llm);
        assert_eq!(timeline.events[0].status, StepStatus::Completed);
        assert_eq!(timeline.events[0].input_tokens, 50);
        assert_eq!(timeline.events[0].output_tokens, 30);
        assert_eq!(timeline.events[0].total_tokens, 80);
        assert!((timeline.events[0].cost_usd - 0.00016).abs() < f64::EPSILON);
        assert_eq!(timeline.events[0].duration_ms, Some(2000));
    }

    #[test]
    fn export_timeline_ascii() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).unwrap();

        let (run, steps, _artifacts) = create_test_run(&workspace);

        let run_store = RunStore::open(&workspace).unwrap();
        run_store.add_run(&run).unwrap();

        let step_store = StepStore::open(&workspace).unwrap();
        for step in &steps {
            step_store.add_step(step).unwrap();
        }

        let ascii = export_timeline(&run.run_id, TimelineFormat::Ascii, Some(&workspace)).unwrap();
        
        // Verify ASCII output contains expected elements
        assert!(ascii.contains(&run.run_id));
        assert!(ascii.contains("analyze"));
        assert!(ascii.contains("fix"));
        assert!(ascii.contains("LLM"));
        assert!(ascii.contains("TOOL"));
        assert!(ascii.contains("✓")); // Completed status symbol
        assert!(ascii.contains("Tokens:"));
        assert!(ascii.contains("Cost:"));
        assert!(ascii.contains("Duration:"));
        assert!(ascii.contains("SUMMARY"));
        assert!(ascii.contains("Total Steps: 2"));
    }

    #[test]
    fn export_timeline_not_found() {
        let workspace = temp_workspace();
        fs::create_dir_all(&workspace).unwrap();

        let result = export_timeline("nonexistent", TimelineFormat::Json, Some(&workspace));
        assert!(result.is_err());
        match result.unwrap_err() {
            RunExportError::NotFound(id) => assert_eq!(id, "nonexistent"),
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn timeline_event_from_step_record() {
        let step = StepRecord {
            step_id: "test-step".to_string(),
            run_id: "test-run".to_string(),
            step_index: 0,
            name: "test".to_string(),
            step_type: StepType::Llm,
            status: StepStatus::Completed,
            started_at: 1000,
            ended_at: Some(1500),
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
            cost_usd: 0.001,
            input_summary: "input".to_string(),
            output_summary: "output".to_string(),
            error: None,
            metadata: serde_json::json!({}),
        };

        let event = TimelineEvent::from(&step);
        assert_eq!(event.step_id, "test-step");
        assert_eq!(event.duration_ms, Some(500));
        assert_eq!(event.input_tokens, 10);
        assert_eq!(event.output_tokens, 20);
        assert_eq!(event.total_tokens, 30);
    }

    #[test]
    fn timeline_event_from_step_record_no_end() {
        let step = StepRecord {
            step_id: "test-step".to_string(),
            run_id: "test-run".to_string(),
            step_index: 0,
            name: "test".to_string(),
            step_type: StepType::Tool,
            status: StepStatus::Running,
            started_at: 1000,
            ended_at: None,
            input_tokens: 5,
            output_tokens: 0,
            total_tokens: 5,
            cost_usd: 0.0,
            input_summary: "input".to_string(),
            output_summary: "".to_string(),
            error: None,
            metadata: serde_json::json!({}),
        };

        let event = TimelineEvent::from(&step);
        assert_eq!(event.duration_ms, None);
        assert_eq!(event.ended_at, None);
        assert_eq!(event.status, StepStatus::Running);
    }

    #[test]
    fn serialize_timeline_json_format() {
        let timeline = TimelineExport {
            run_id: "test-run".to_string(),
            run_started_at: 1000,
            run_ended_at: Some(2000),
            run_duration_ms: Some(1000),
            total_steps: 1,
            total_tokens: 100,
            total_cost_usd: 0.001,
            events: vec![TimelineEvent {
                step_id: "step-1".to_string(),
                step_index: 0,
                name: "test".to_string(),
                step_type: StepType::Llm,
                status: StepStatus::Completed,
                started_at: 1000,
                ended_at: Some(2000),
                duration_ms: Some(1000),
                input_tokens: 50,
                output_tokens: 50,
                total_tokens: 100,
                cost_usd: 0.001,
                input_summary: "input".to_string(),
                output_summary: "output".to_string(),
                error: None,
                metadata: serde_json::json!({}),
            }],
        };

        let json = serialize_timeline_json(&timeline).unwrap();
        let parsed: TimelineExport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.run_id, "test-run");
        assert_eq!(parsed.events.len(), 1);
    }

    #[test]
    fn serialize_timeline_ascii_format() {
        let timeline = TimelineExport {
            run_id: "test-run".to_string(),
            run_started_at: 1000,
            run_ended_at: Some(2000),
            run_duration_ms: Some(1000),
            total_steps: 1,
            total_tokens: 100,
            total_cost_usd: 0.001,
            events: vec![TimelineEvent {
                step_id: "step-1".to_string(),
                step_index: 0,
                name: "test step".to_string(),
                step_type: StepType::Tool,
                status: StepStatus::Completed,
                started_at: 1000,
                ended_at: Some(2000),
                duration_ms: Some(1000),
                input_tokens: 50,
                output_tokens: 50,
                total_tokens: 100,
                cost_usd: 0.001,
                input_summary: "input summary".to_string(),
                output_summary: "output summary".to_string(),
                error: None,
                metadata: serde_json::json!({}),
            }],
        };

        let ascii = serialize_timeline_ascii(&timeline).unwrap();
        assert!(ascii.contains("test-run"));
        assert!(ascii.contains("test step"));
        assert!(ascii.contains("TOOL"));
        assert!(ascii.contains("✓"));
        assert!(ascii.contains("1000"));
        assert!(ascii.contains("Tokens:"));
        assert!(ascii.contains("Cost:"));
        assert!(ascii.contains("SUMMARY"));
    }

    #[test]
    fn truncate_test() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a very long string", 10), "this is...");
        assert_eq!(truncate("exact", 5), "exact");
        assert_eq!(truncate("exactly", 5), "ex...");
    }
}
};
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
use anyhow::{Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;
use workbuddy_core::store::RunStore;
use workspace::{RunRecord, StepRecord};

/// Export a run to a JSON file
#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Run ID to export
    pub run_id: Uuid,
    /// Output file path
    #[arg(short, long, default_value = "run.json")]
    pub output: PathBuf,
}

/// Exported run data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedRun {
    /// Schema version for compatibility checking
    pub schema_version: u32,
    /// The run record
    pub run_record: RunRecord,
    /// All step records for this run
    pub steps: Vec<StepRecord>,
    /// Total cost information
    pub costs: RunCosts,
    /// Log entries
    pub logs: Vec<LogEntry>,
}

/// Cost information for the run
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunCosts {
    /// Total input tokens
    pub input_tokens: u64,
    /// Total output tokens
    pub output_tokens: u64,
    /// Total cost in USD
    pub total_cost_usd: f64,
    /// Cost breakdown by model
    pub by_model: std::collections::HashMap<String, ModelCost>,
}

/// Cost per model
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelCost {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// Log entry structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub level: String,
    pub message: String,
    pub step_id: Option<Uuid>,
}

/// Current schema version for exported runs
pub const SCHEMA_VERSION: u32 = 1;

pub async fn run_export(args: ExportArgs, store: &RunStore) -> Result<()> {
    // Fetch the run record
    let run_record = store
        .get_run(&args.run_id)
        .await
        .with_context(|| format!("Run not found: {}", args.run_id))?;

    // Fetch all steps for this run
    let steps = store
        .get_steps(&args.run_id)
        .await
        .with_context(|| format!("Failed to get steps for run: {}", args.run_id))?;

    // Calculate costs from steps
    let costs = calculate_costs(&steps);

    // Read logs from the run directory
    let logs = read_logs(&run_record).await?;

    // Build exported data
    let exported = ExportedRun {
        schema_version: SCHEMA_VERSION,
        run_record,
        steps,
        costs,
        logs,
    };

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&exported)
        .context("Failed to serialize exported run to JSON")?;

    // Write to output file
    fs::write(&args.output, json)
        .with_context(|| format!("Failed to write export file: {}", args.output.display()))?;

    println!("Run exported to: {}", args.output.display());
    Ok(())
}

fn calculate_costs(steps: &[StepRecord]) -> RunCosts {
    let mut costs = RunCosts::default();
    
    for step in steps {
        if let Some(usage) = &step.token_usage {
            costs.input_tokens += usage.input_tokens as u64;
            costs.output_tokens += usage.output_tokens as u64;
            costs.total_cost_usd += usage.cost_usd.unwrap_or(0.0);
            
            let model_cost = costs.by_model.entry(step.model.clone()).or_default();
            model_cost.input_tokens += usage.input_tokens as u64;
            model_cost.output_tokens += usage.output_tokens as u64;
            model_cost.cost_usd += usage.cost_usd.unwrap_or(0.0);
        }
    }
    
    costs
}

async fn read_logs(run_record: &RunRecord) -> Result<Vec<LogEntry>> {
    let log_path = run_record.workdir.join("run.log");
    
    if !log_path.exists() {
        return Ok(Vec::new());
    }
    
    let content = fs::read_to_string(&log_path)
        .with_context(|| format!("Failed to read log file: {}", log_path.display()))?;
    
    let mut logs = Vec::new();
    for line in content.lines() {
        if let Ok(entry) = parse_log_line(line) {
            logs.push(entry);
        }
    }
    
    Ok(logs)
}

fn parse_log_line(line: &str) -> Result<LogEntry> {
    // Expected format: TIMESTAMP LEVEL [STEP_ID] MESSAGE
    // Example: 2024-01-15T10:30:00.123Z INFO [abc-123] Starting step
    let parts: Vec<&str> = line.splitn(4, ' ').collect();
    if parts.len() < 3 {
        anyhow::bail!("Invalid log line format");
    }
    
    let timestamp = chrono::DateTime::parse_from_rfc3339(parts[0])?
        .with_timezone(&chrono::Utc);
    let level = parts[1].to_string();
    
    let (step_id, message) = if parts[2].starts_with('[') && parts[2].ends_with(']') {
        let step_id_str = &parts[2][1..parts[2].len()-1];
        let step_id = Uuid::parse_str(step_id_str).ok();
        let message = parts.get(3).unwrap_or(&"").to_string();
        (step_id, message)
    } else {
        (None, parts[2..].join(" "))
    };
    
    Ok(LogEntry {
        timestamp,
        level,
        message,
        step_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use workbuddy_core::store::RunStore;
    use workspace::{RunRecord, RunStatus, StepRecord, StepStatus, TokenUsage};
    use std::sync::Arc;
    use chrono::Utc;
    use uuid::Uuid;

    async fn create_test_store() -> (RunStore, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let store = RunStore::new(temp_dir.path().to_path_buf()).await.unwrap();
        (store, temp_dir)
    }

    #[tokio::test]
    async fn test_export_roundtrip() {
        let (store, _temp_dir) = create_test_store().await;
        
        // Create a test run
        let run_id = Uuid::new_v4();
        let run_record = RunRecord {
            run_id,
            task_id: Uuid::new_v4(),
            status: RunStatus::Completed,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            workdir: store.runs_dir.join(run_id.to_string()),
            metadata: Default::default(),
        };
        
        store.create_run(&run_record).await.unwrap();
        
        // Add a step
        let step_id = Uuid::new_v4();
        let step = StepRecord {
            step_id,
            run_id,
            name: "test_step".to_string(),
            status: StepStatus::Completed,
            model: "gpt-4".to_string(),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            input: serde_json::json!({"prompt": "test"}),
            output: Some(serde_json::json!({"result": "ok"})),
            error: None,
            token_usage: Some(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                cost_usd: Some(0.01),
            }),
            metadata: Default::default(),
        };
        store.save_step(&step).await.unwrap();
        
        // Create a log file
        let log_content = format!(
            "{} INFO [{}] Starting step\n{} INFO [{}] Step completed\n",
            Utc::now().to_rfc3339(),
            step_id,
            Utc::now().to_rfc3339(),
            step_id
        );
        fs::write(run_record.workdir.join("run.log"), log_content).unwrap();
        
        // Export
        let output_path = _temp_dir.path().join("exported.json");
        let args = ExportArgs {
            run_id,
            output: output_path.clone(),
        };
        
        run_export(args, &store).await.unwrap();
        
        // Verify exported file exists and has correct content
        assert!(output_path.exists());
        let content = fs::read_to_string(&output_path).unwrap();
        let exported: ExportedRun = serde_json::from_str(&content).unwrap();
        
        assert_eq!(exported.schema_version, SCHEMA_VERSION);
        assert_eq!(exported.run_record.run_id, run_id);
        assert_eq!(exported.steps.len(), 1);
        assert_eq!(exported.steps[0].step_id, step_id);
        assert_eq!(exported.costs.input_tokens, 100);
        assert_eq!(exported.costs.output_tokens, 50);
        assert!((exported.costs.total_cost_usd - 0.01).abs() < f64::EPSILON);
        assert_eq!(exported.logs.len(), 2);
    }

    #[tokio::test]
    async fn test_export_nonexistent_run() {
        let (store, _temp_dir) = create_test_store().await;
        
        let args = ExportArgs {
            run_id: Uuid::new_v4(),
            output: _temp_dir.path().join("exported.json"),
        };
        
        let result = run_export(args, &store).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Run not found"));
    }
}