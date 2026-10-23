//! 交互式运行调试器
//!
//! 基于检查点数据逐步重放，支持断点设置、单步执行、变量检查、
//! 跳过/重试步骤、修改上下文后继续。

use anyhow::Result;
use std::path::Path;
use workspace::{RunRecord, StepRecord, Workspace};

/// 调试会话状态
#[derive(Debug, Clone)]
pub struct DebugSession {
    pub run_id: String,
    pub run_record: RunRecord,
    pub current_step: usize,
    pub breakpoints: std::collections::HashSet<usize>,
    pub modified_context: Option<serde_json::Value>,
}

impl DebugSession {
    pub fn new(run_id: String, run_record: RunRecord, start_step: usize) -> Self {
        Self {
            run_id,
            run_record,
            current_step: start_step,
            breakpoints: std::collections::HashSet::new(),
            modified_context: None,
        }
    }

    /// 当前步骤记录
    pub fn current_step_record(&self) -> Option<&StepRecord> {
        self.run_record.steps.get(self.current_step)
    }

    /// 是否在断点处
    pub fn at_breakpoint(&self) -> bool {
        self.breakpoints.contains(&self.current_step)
    }

    /// 添加/移除断点
    pub fn toggle_breakpoint(&mut self, step: usize) -> bool {
        if self.breakpoints.contains(&step) {
            self.breakpoints.remove(&step);
            false
        } else {
            self.breakpoints.insert(step);
            true
        }
    }

    /// 单步跨过
    pub fn step_over(&mut self) -> Result<()> {
        if self.current_step + 1 < self.run_record.steps.len() {
            self.current_step += 1;
        }
        Ok(())
    }

    /// 单步进入（预留：进入子调用）
    pub fn step_into(&mut self) -> Result<()> {
        self.step_over()
    }

    /// 单步跳出（预留：跳出当前调用栈）
    pub fn step_out(&mut self) -> Result<()> {
        self.step_over()
    }

    /// 跳过当前步骤
    pub fn skip_step(&mut self) -> Result<()> {
        self.step_over()
    }

    /// 重试当前步骤（需结合修改上下文）
    pub fn retry_step(&mut self) -> Result<()> {
        // 实际重试逻辑由 TUI 层配合 executor 实现
        Ok(())
    }

    /// 修改上下文（input/output 等）
    pub fn set_modified_context(&mut self, ctx: serde_json::Value) {
        self.modified_context = Some(ctx);
    }
}

/// 启动调试会话主入口
pub async fn run_debug_session(run_id: String, start_checkpoint: Option<usize>) -> Result<()> {
    let repo = std::env::current_dir()?;
    let ws = Workspace::open(&repo)?;
    let run_record = ws.load_run(&run_id)?;
    
    let start_step = start_checkpoint.unwrap_or(0).min(run_record.steps.len().saturating_sub(1));
    let mut session = DebugSession::new(run_id, run_record, start_step);
    
    // TODO: 启动 TUI 交互循环 (复用 tui 模块基础设施)
    // tui::run_debug_tui(session).await?;
    
    eprintln!("[debug] 会话已初始化: run_id={}, 起始步骤={}", session.run_id, session.current_step);
    eprintln!("[debug] 共 {} 步骤, 断点: {:?}", session.run_record.steps.len(), session.breakpoints);
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use workspace::{RunRecord, StepRecord, StepStatus};
    use std::collections::HashMap;
    use serde_json::json;

    fn make_test_record() -> RunRecord {
        RunRecord {
            run_id: "test-run".into(),
            task: "test task".into(),
            steps: vec![
                StepRecord {
                    index: 0,
                    agent: "planner".into(),
                    input: json!({}),
                    output: json!({"plan": "step1"}),
                    status: StepStatus::Completed,
                    started_at: chrono::Utc::now(),
                    finished_at: Some(chrono::Utc::now()),
                    duration_ms: 100,
                    tool_calls: vec![],
                    checkpoint_id: None,
                },
                StepRecord {
                    index: 1,
                    agent: "executor".into(),
                    input: json!({}),
                    output: json!({"result": "ok"}),
                    status: StepStatus::Completed,
                    started_at: chrono::Utc::now(),
                    finished_at: Some(chrono::Utc::now()),
                    duration_ms: 200,
                    tool_calls: vec![],
                    checkpoint_id: Some("cp1".into()),
                },
            ],
            checkpoints: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn test_breakpoint_toggle() {
        let record = make_test_record();
        let mut session = DebugSession::new("test".into(), record, 0);
        
        assert!(!session.at_breakpoint());
        assert!(session.toggle_breakpoint(0));
        assert!(session.at_breakpoint());
        assert!(!session.toggle_breakpoint(0));
        assert!(!session.at_breakpoint());
    }

    #[test]
    fn test_step_over() {
        let record = make_test_record();
        let mut session = DebugSession::new("test".into(), record, 0);
        
        assert_eq!(session.current_step, 0);
        session.step_over().unwrap();
        assert_eq!(session.current_step, 1);
        session.step_over().unwrap();
        assert_eq!(session.current_step, 1); // 不越界
    }

    #[test]
    fn test_modified_context() {
        let record = make_test_record();
        let mut session = DebugSession::new("test".into(), record, 0);
        
        assert!(session.modified_context.is_none());
        session.set_modified_context(json!({"input": {"modified": true}}));
        assert!(session.modified_context.is_some());
        assert_eq!(session.modified_context.unwrap()["input"]["modified"], true);
    }
}
