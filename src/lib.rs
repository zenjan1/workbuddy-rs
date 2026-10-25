
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