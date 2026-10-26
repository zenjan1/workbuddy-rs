use crate::config::Config;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::fs;

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

/// 扩展的步骤记录，包含时间线所需的完整信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineStepRecord {
    pub step_name: String,
    pub agent: String,
    pub tool: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub duration_ms: u64,
    pub tokens_used: TokenUsage,
    pub status: StepStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub steps: Vec<TimelineStepRecord>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ReplayOptions {
    pub run_id: String,
    pub new_model: String,
    pub compare: bool,
    pub baseline_model: Option<String>,
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ShowOptions {
    pub run_id: String,
    pub timeline: bool,
    pub output: Option<PathBuf>,
    pub ascii: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ComparisonReport {
    // placeholder for future implementation
}

pub async fn run_replay(_cfg: Config, _options: ReplayOptions) -> Result<()> {
    todo!("replay functionality not yet implemented")
}

/// 生成 Mermaid.js 时间线图表
pub fn generate_timeline_mermaid(record: &RunRecord) -> String {
    let mut lines = vec!["```mermaid".to_string(), "timeline".to_string()];
    
    for step in &record.steps {
        let start = step.start_time.format("%H:%M:%S%.3f").to_string();
        let end = step.end_time.format("%H:%M:%S%.3f").to_string();
        let duration_sec = step.duration_ms as f64 / 1000.0;
        let status_icon = match step.status {
            StepStatus::Success => "✅",
            StepStatus::Failed => "❌",
            StepStatus::Skipped => "⏭️",
        };
        
        let title = format!("{} {} / {}", status_icon, step.agent, step.tool);
        let detail = format!(
            "{} - {} | {:.2}s | tokens: {}/{}/{}",
            start, end, duration_sec,
            step.tokens_used.input_tokens,
            step.tokens_used.output_tokens,
            step.tokens_used.total_tokens
        );
        
        lines.push(format!("    {} : {}", title, detail));
        
        if let Some(err) = &step.error_message {
            lines.push(format!("    {} : ⚠️ Error: {}", title, err));
        }
    }
    
    lines.push("```".to_string());
    lines.join("\n")
}

/// 生成 ASCII 降级时间线
pub fn generate_timeline_ascii(record: &RunRecord) -> String {
    let mut output = String::new();
    output.push_str(&format!("Run ID: {}\n", record.run_id));
    output.push_str(&format!("Period: {} ~ {}\n", 
        record.start_time.format("%Y-%m-%d %H:%M:%S%.3f"),
        record.end_time.format("%Y-%m-%d %H:%M:%S%.3f")));
    output.push_str(&"─".repeat(80));
    output.push_str("\n");
    
    for (i, step) in record.steps.iter().enumerate() {
        let start = step.start_time.format("%H:%M:%S%.3f").to_string();
        let end = step.end_time.format("%H:%M:%S%.3f").to_string();
        let duration_sec = step.duration_ms as f64 / 1000.0;
        let status_str = match step.status {
            StepStatus::Success => "SUCCESS",
            StepStatus::Failed => "FAILED ",
            StepStatus::Skipped => "SKIPPED",
        };
        
        output.push_str(&format!(
            "{:2}. [{}] {} / {}\n",
            i + 1, status_str, step.agent, step.tool
        ));
        output.push_str(&format!(
            "     ⏱  {} → {}  ({:.2}s)  📊 tokens: in={} out={} total={}\n",
            start, end, duration_sec,
            step.tokens_used.input_tokens,
            step.tokens_used.output_tokens,
            step.tokens_used.total_tokens
        ));
        
        if let Some(err) = &step.error_message {
            output.push_str(&format!("     ⚠️  Error: {}\n", err));
        }
        output.push_str("\n");
    }
    
    output
}

/// 统一入口：根据 ascii 标志生成时间线
pub fn generate_timeline(record: &RunRecord, ascii: bool) -> String {
    if ascii {
        generate_timeline_ascii(record)
    } else {
        generate_timeline_mermaid(record)
    }
}

/// 处理 `wb show` 命令
pub async fn run_show(_cfg: Config, options: ShowOptions) -> Result<()> {
    // TODO: 从存储加载 RunRecord
    // 这里先构造一个示例用于演示
    let record = create_sample_record(&options.run_id);
    
    let timeline_output = generate_timeline(&record, options.ascii);
    
    match options.output {
        Some(path) => {
            fs::write(&path, &timeline_output)
                .map_err(|e| anyhow::anyhow!("写入时间线文件失败 {}: {}", path.display(), e))?;
            println!("时间线已导出到: {}", path.display());
        }
        None => {
            println!("{}", timeline_output);
        }
    }
    
    Ok(())
}

/// 创建示例 RunRecord（实际应从存储加载）
fn create_sample_record(run_id: &str) -> RunRecord {
    use chrono::TimeZone;
    let base = Utc.with_ymd_and_hms(2024, 1, 15, 10, 0, 0).unwrap();
    
    RunRecord {
        run_id: run_id.to_string(),
        start_time: base,
        end_time: base + chrono::Duration::seconds(45),
        steps: vec![
            TimelineStepRecord {
                step_name: "planning".to_string(),
                agent: "planner".to_string(),
                tool: "think".to_string(),
                start_time: base,
                end_time: base + chrono::Duration::seconds(5),
                duration_ms: 5000,
                tokens_used: TokenUsage { input_tokens: 1200, output_tokens: 800, total_tokens: 2000 },
                status: StepStatus::Success,
                error_message: None,
            },
            TimelineStepRecord {
                step_name: "code_generation".to_string(),
                agent: "coder".to_string(),
                tool: "write_file".to_string(),
                start_time: base + chrono::Duration::seconds(5),
                end_time: base + chrono::Duration::seconds(25),
                duration_ms: 20000,
                tokens_used: TokenUsage { input_tokens: 3500, output_tokens: 4200, total_tokens: 7700 },
                status: StepStatus::Success,
                error_message: None,
            },
            TimelineStepRecord {
                step_name: "test_execution".to_string(),
                agent: "tester".to_string(),
                tool: "run_tests".to_string(),
                start_time: base + chrono::Duration::seconds(25),
                end_time: base + chrono::Duration::seconds(40),
                duration_ms: 15000,
                tokens_used: TokenUsage { input_tokens: 800, output_tokens: 1200, total_tokens: 2000 },
                status: StepStatus::Failed,
                error_message: Some("Test case 'test_edge_case' failed".to_string()),
            },
            TimelineStepRecord {
                step_name: "fix_and_retry".to_string(),
                agent: "coder".to_string(),
                tool: "edit_file".to_string(),
                start_time: base + chrono::Duration::seconds(40),
                end_time: base + chrono::Duration::seconds(45),
                duration_ms: 5000,
                tokens_used: TokenUsage { input_tokens: 2100, output_tokens: 1500, total_tokens: 3600 },
                status: StepStatus::Success,
                error_message: None,
            },
        ],
    }
}
