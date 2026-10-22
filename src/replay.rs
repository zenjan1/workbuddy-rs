use crate::config::Config;
use anyhow::Result;
use std::path::PathBuf;

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
