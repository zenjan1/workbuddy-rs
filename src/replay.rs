use crate::config::Config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Success,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub step_name: String,
    pub status: StepStatus,
    pub input_chars: usize,
    pub output_chars: usize,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReplayOptions {
    pub run_id: String,
    pub new_model: String,
    pub compare: bool,
    pub baseline_model: Option<String>,
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ComparisonReport {
    // placeholder for future implementation
}

pub async fn run_replay(_cfg: Config, _options: ReplayOptions) -> Result<()> {
    todo!("replay functionality not yet implemented")
}
