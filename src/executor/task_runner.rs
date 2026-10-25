use std::future::Future;
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use crate::plan::{StepRecord, TaskSpec};

/// Custom error type for timeout scenarios
#[derive(Debug, thiserror::Error)]
pub enum TimeoutError {
    /// The operation timed out after the specified duration
    #[error("Operation timed out after {0} seconds")]
    TimedOut(u64),
    /// The operation exceeded the absolute deadline
    #[error("Operation exceeded deadline at {0}")]
    DeadlineExceeded(DateTime<Utc>),
    /// The future was cancelled due to timeout
    #[error("Future cancelled due to timeout")]
    Cancelled,
    /// Failed to send signal to child process
    #[error("Failed to send signal to child process: {0}")]
    SignalError(String),
    /// The underlying future returned an error
    #[error("Future error: {0}")]
    FutureError(String),
}

/// Error type for budget exceeded scenarios
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    /// The task exceeded its token budget
    #[error("Task token budget exceeded: used {used} tokens, limit {limit} tokens")]
    TokenBudgetExceeded { used: u32, limit: u32 },
    /// The task exceeded its cost budget
    #[error("Task cost budget exceeded: used ${used:.6}, limit ${limit:.6}")]
    CostBudgetExceeded { used: f64, limit: f64 },
    /// Generic budget exceeded (when both or unspecified)
    #[error("Task budget exceeded: {message}")]
    BudgetExceeded { message: String },
}

impl BudgetError {
    /// Check if this error represents a budget exceeded condition
    pub fn is_budget_exceeded(&self) -> bool {
        true
    }
}

impl TimeoutError {
    /// Check if this error represents a timeout (as opposed to other errors)
    pub fn is_timeout(&self) -> bool {
        matches!(self, TimeoutError::TimedOut(_) | TimeoutError::DeadlineExceeded(_) | TimeoutError::Cancelled)
    }
}

/// Configuration for timeout behavior
#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    /// Maximum duration in seconds before timeout
    pub timeout_sec: Option<u64>,
    /// Absolute deadline by which the operation must complete
    pub deadline: Option<DateTime<Utc>>,
    /// Process ID of the child process to signal on timeout (if any)
    pub child_pid: Option<u32>,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            timeout_sec: None,
            deadline: None,
            child_pid: None,
        }
    }
}

impl TimeoutConfig {
    /// Create a new TimeoutConfig with only timeout_sec
    pub fn with_timeout_sec(timeout_sec: u64) -> Self {
        Self {
            timeout_sec: Some(timeout_sec),
            deadline: None,
            child_pid: None,
        }
    }

    /// Create a new TimeoutConfig with only deadline
    pub fn with_deadline(deadline: DateTime<Utc>) -> Self {
        Self {
            timeout_sec: None,
            deadline: Some(deadline),
            child_pid: None,
        }
    }

    /// Create a new TimeoutConfig with both timeout_sec and deadline
    pub fn with_both(timeout_sec: u64, deadline: DateTime<Utc>) -> Self {
        Self {
            timeout_sec: Some(timeout_sec),
            deadline: Some(deadline),
            child_pid: None,
        }
    }

    /// Set the child process PID to signal on timeout
    pub fn with_child_pid(mut self, pid: u32) -> Self {
        self.child_pid = Some(pid);
        self
    }

    /// Calculate the remaining duration until the earliest timeout/deadline
    fn remaining_duration(&self) -> Option<Duration> {
        let now = Utc::now();
        let mut durations = Vec::new();

        if let Some(secs) = self.timeout_sec {
            durations.push(Duration::from_secs(secs));
        }

        if let Some(deadline) = self.deadline {
            if deadline > now {
                let secs = (deadline - now).num_seconds().max(0) as u64;
                durations.push(Duration::from_secs(secs));
            } else {
                // Deadline already passed
                return Some(Duration::from_secs(0));
            }
        }

        durations.into_iter().min()
    }

    /// Check if the deadline has already passed
    fn is_deadline_passed(&self) -> bool {
        if let Some(deadline) = self.deadline {
            Utc::now() >= deadline
        } else {
            false
        }
    }
}

/// Guard that enforces timeout and deadline constraints on async operations
pub struct TimeoutGuard {
    config: TimeoutConfig,
    /// Whether the guard has been triggered (timeout occurred)
    triggered: Arc<std::sync::atomic::AtomicBool>,
}

impl TimeoutGuard {
    /// Create a new TimeoutGuard with the given configuration
    pub fn new(config: TimeoutConfig) -> Self {
        Self {
            config,
            triggered: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Create a TimeoutGuard from timeout_sec and deadline directly
    pub fn from_parts(timeout_sec: Option<u64>, deadline: Option<DateTime<Utc>>) -> Self {
        Self::new(TimeoutConfig {
            timeout_sec,
            deadline,
            child_pid: None,
        })
    }

    /// Set the child process PID to signal on timeout
    pub fn with_child_pid(mut self, pid: u32) -> Self {
        self.config.child_pid = Some(pid);
        self
    }

    /// Run a future with timeout enforcement
    /// 
    /// If the future completes before the timeout/deadline, returns its output.
    /// If the timeout or deadline is exceeded, sends SIGTERM to the child process (if configured),
    /// cancels the future, and returns a TimeoutError.
    pub async fn run<F, T>(&self, fut: F) -> Result<T, TimeoutError>
    where
        F: Future<Output = Result<T, TimeoutError>> + Send,
        T: Send,
    {
        // Check if deadline already passed
        if self.config.is_deadline_passed() {
            return Err(TimeoutError::DeadlineExceeded(
                self.config.deadline.unwrap_or_else(|| Utc::now())
            ));
        }

        let remaining = self.config.remaining_duration();
        let triggered = self.triggered.clone();
        let child_pid = self.config.child_pid;
        let deadline = self.config.deadline;
        let timeout_sec = self.config.timeout_sec;

        // Wrap the future with timeout handling
        let result = match remaining {
            Some(duration) if duration > Duration::from_secs(0) => {
                // Use tokio::time::timeout for the duration-based timeout
                match timeout(duration, fut).await {
                    Ok(inner_result) => inner_result,
                    Err(_) => {
                        // Timeout elapsed
                        triggered.store(true, std::sync::atomic::Ordering::SeqCst);
                        Self::handle_timeout(child_pid, timeout_sec, deadline).await;
                        Err(TimeoutError::TimedOut(timeout_sec.unwrap_or(0)))
                    }
                }
            }
            Some(_) => {
                // Duration is zero or negative - deadline already passed
                triggered.store(true, std::sync::atomic::Ordering::SeqCst);
                Self::handle_timeout(child_pid, timeout_sec, deadline).await;
                Err(TimeoutError::DeadlineExceeded(deadline.unwrap_or_else(|| Utc::now())))
            }
            None => {
                // No timeout configured, run the future without timeout
                fut.await
            }
        };

        result
    }

    /// Run a future that doesn't return a Result, with timeout enforcement
    pub async fn run_simple<F, T>(&self, fut: F) -> Result<T, TimeoutError>
    where
        F: Future<Output = T> + Send,
        T: Send,
    {
        self.run(async move {
            Ok(fut.await)
        })
        .await
    }

    /// Handle timeout by sending SIGTERM to child process
    async fn handle_timeout(child_pid: Option<u32>, timeout_sec: Option<u64>, deadline: Option<DateTime<Utc>>) {
        if let Some(pid) = child_pid {
            info!("Timeout triggered, sending SIGTERM to child process {}", pid);
            match Self::send_sigterm(pid).await {
                Ok(_) => debug!("Successfully sent SIGTERM to process {}", pid),
                Err(e) => {
                    error!("Failed to send SIGTERM to process {}: {}", pid, e);
                    // Don't return error here, just log it
                }
            }
        }

        if let Some(secs) = timeout_sec {
            warn!("Operation timed out after {} seconds", secs);
        }
        if let Some(dl) = deadline {
            warn!("Operation exceeded deadline: {}", dl);
        }
    }

    /// Send SIGTERM to a process using nix crate
    async fn send_sigterm(pid: u32) -> Result<(), TimeoutError> {
        let pid = Pid::from_raw(pid as i32);
        tokio::task::spawn_blocking(move || {
            kill(pid, Signal::SIGTERM)
                .map_err(|e| TimeoutError::SignalError(e.to_string()))
        })
        .await
        .map_err(|e| TimeoutError::SignalError(format!("Task join error: {}", e)))?
    }

    /// Check if the guard has been triggered (timeout occurred)
    pub fn is_triggered(&self) -> bool {
        self.triggered.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the timeout configuration
    pub fn config(&self) -> &TimeoutConfig {
        &self.config
    }
}

/// Builder for TimeoutGuard to allow fluent configuration
pub struct TimeoutGuardBuilder {
    config: TimeoutConfig,
}

impl TimeoutGuardBuilder {
    pub fn new() -> Self {
        Self {
            config: TimeoutConfig::default(),
        }
    }

    pub fn timeout_sec(mut self, secs: u64) -> Self {
        self.config.timeout_sec = Some(secs);
        self
    }

    pub fn deadline(mut self, deadline: DateTime<Utc>) -> Self {
        self.config.deadline = Some(deadline);
        self
    }

    pub fn child_pid(mut self, pid: u32) -> Self {
        self.config.child_pid = Some(pid);
        self
    }

    pub fn build(self) -> TimeoutGuard {
        TimeoutGuard::new(self.config)
    }
}

impl Default for TimeoutGuardBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Budget tracker for task-level LLM call budget enforcement
#[derive(Debug, Clone, Default)]
pub struct BudgetTracker {
    /// Cumulative tokens used so far
    pub total_tokens: u32,
    /// Cumulative cost in USD so far
    pub total_cost_usd: f64,
    /// Token budget limit (None = unlimited)
    pub max_tokens: Option<u32>,
    /// Cost budget limit in USD (None = unlimited)
    pub max_cost_usd: Option<f64>,
}

impl BudgetTracker {
    /// Create a new BudgetTracker from TaskSpec budget limits
    pub fn from_task_spec(spec: &TaskSpec) -> Self {
        Self {
            total_tokens: 0,
            total_cost_usd: 0.0,
            max_tokens: spec.max_tokens,
            max_cost_usd: spec.max_cost_usd,
        }
    }

    /// Check if adding the given tokens/cost would exceed budget
    /// Returns BudgetError if budget would be exceeded
    pub fn check_budget(&self, additional_tokens: u32, additional_cost_usd: f64) -> Result<(), BudgetError> {
        if let Some(limit) = self.max_tokens {
            let projected = self.total_tokens.saturating_add(additional_tokens);
            if projected > limit {
                return Err(BudgetError::TokenBudgetExceeded {
                    used: projected,
                    limit,
                });
            }
        }

        if let Some(limit) = self.max_cost_usd {
            let projected = self.total_cost_usd + additional_cost_usd;
            if projected > limit {
                return Err(BudgetError::CostBudgetExceeded {
                    used: projected,
                    limit,
                });
            }
        }

        Ok(())
    }

    /// Record actual consumption from an LLM call
    pub fn record_consumption(&mut self, tokens: u32, cost_usd: f64) {
        self.total_tokens = self.total_tokens.saturating_add(tokens);
        self.total_cost_usd += cost_usd;
    }

    /// Get current cumulative tokens
    pub fn total_tokens(&self) -> u32 {
        self.total_tokens
    }

    /// Get current cumulative cost
    pub fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }

    /// Check if any budget is configured
    pub fn has_budget(&self) -> bool {
        self.max_tokens.is_some() || self.max_cost_usd.is_some()
    }
}

use crate::plan::{TaskStatus, StepRecord};
use std::path::PathBuf;

/// Artifact produced by a task step
#[derive(Debug, Clone, Default)]
pub struct TaskArtifact {
    /// Standard output from the task
    pub stdout: String,
    /// Standard error from the task
    pub stderr: String,
    /// Exit code of the task
    pub exit_code: Option<i32>,
    /// Path to any output files generated
    pub output_files: Vec<PathBuf>,
    /// Token usage statistics
    pub tokens_used: u32,
    /// Cost in USD
    pub cost_usd: f64,
}

/// Task runner that executes task steps with support for skipping successful tasks
pub struct TaskRunner {
    /// Whether to skip tasks that have already succeeded (for --retry-failed)
    pub skip_successful: bool,
    /// Base directory for artifact storage
    pub artifact_dir: PathBuf,
    /// Timeout configuration
    pub timeout_config: TimeoutConfig,
    /// Budget tracker
    pub budget_tracker: BudgetTracker,
}

impl TaskRunner {
    /// Create a new TaskRunner
    pub fn new(skip_successful: bool, artifact_dir: PathBuf, timeout_config: TimeoutConfig, budget_tracker: BudgetTracker) -> Self {
        Self {
            skip_successful,
            artifact_dir,
            timeout_config,
            budget_tracker,
        }
    }

    /// Run a single task step, skipping if already successful and skip_successful is true
    /// 
    /// Returns the TaskArtifact (either from cache or fresh execution)
    pub async fn run_step(
        &mut self,
        step: &StepRecord,
        spec: &TaskSpec,
        execute_fn: impl FnOnce() -> Pin<Box<dyn Future<Output = Result<TaskArtifact, TimeoutError>> + Send>>,
    ) -> Result<TaskArtifact, TimeoutError> {
        // If skip_successful is enabled and the step already succeeded, return cached artifact
        if self.skip_successful && step.status == TaskStatus::Success {
            info!("Skipping successful step: {} (id: {})", step.name, step.id);
            return Ok(self.load_cached_artifact(step).await);
        }

        // Otherwise, execute the step
        info!("Executing step: {} (id: {})", step.name, step.id);
        let guard = TimeoutGuard::new(self.timeout_config.clone());
        let artifact = guard.run(execute_fn()).await?;

        // Record budget consumption if any
        if artifact.tokens_used > 0 || artifact.cost_usd > 0.0 {
            self.budget_tracker.record_consumption(artifact.tokens_used, artifact.cost_usd);
        }

        Ok(artifact)
    }

    /// Load cached artifact for a successful step
    async fn load_cached_artifact(&self, step: &StepRecord) -> TaskArtifact {
        // In a real implementation, this would load from the artifact directory
        // For now, reconstruct from StepRecord fields
        TaskArtifact {
            stdout: step.stdout.clone().unwrap_or_default(),
            stderr: step.stderr.clone().unwrap_or_default(),
            exit_code: step.exit_code,
            output_files: step.output_files.clone().unwrap_or_default(),
            tokens_used: step.tokens_used.unwrap_or(0),
            cost_usd: step.cost_usd.unwrap_or(0.0),
        }
    }

    /// Run multiple steps in sequence
    pub async fn run_steps(
        &mut self,
        steps: &mut [StepRecord],
        spec: &TaskSpec,
        mut execute_fn: impl FnMut(&StepRecord, &TaskSpec) -> Pin<Box<dyn Future<Output = Result<TaskArtifact, TimeoutError>> + Send>>,
    ) -> Result<Vec<TaskArtifact>, TimeoutError> {
        let mut artifacts = Vec::with_capacity(steps.len());

        for step in steps.iter_mut() {
            let artifact = self.run_step(step, spec, || execute_fn(step, spec)).await?;
            // Update step record with results
            step.status = TaskStatus::Success;
            step.stdout = Some(artifact.stdout.clone());
            step.stderr = Some(artifact.stderr.clone());
            step.exit_code = artifact.exit_code;
            step.output_files = Some(artifact.output_files.clone());
            step.tokens_used = Some(artifact.tokens_used);
            step.cost_usd = Some(artifact.cost_usd);
            artifacts.push(artifact);
        }

        Ok(artifacts)
    }

    /// Get the current budget tracker
    pub fn budget_tracker(&self) -> &BudgetTracker {
        &self.budget_tracker
    }

    /// Get mutable reference to budget tracker
    pub fn budget_tracker_mut(&mut self) -> &mut BudgetTracker {
        &mut self.budget_tracker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_timeout_guard_basic_timeout() {
        let guard = TimeoutGuard::from_parts(Some(1), None); // 1 second timeout
        
        let result = guard.run_simple(async {
            sleep(Duration::from_secs(5)).await;
            "completed"
        }).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_timeout());
        assert!(matches!(err, TimeoutError::TimedOut(1)));
        assert!(guard.is_triggered());
    }

    #[tokio::test]
    async fn test_budget_tracker_token_limit() {
        let mut tracker = BudgetTracker {
            total_tokens: 0,
            total_cost_usd: 0.0,
            max_tokens: Some(100),
            max_cost_usd: None,
        };

        // Should pass - under limit
        assert!(tracker.check_budget(50, 0.0).is_ok());
        tracker.record_consumption(50, 0.0);
        assert_eq!(tracker.total_tokens(), 50);

        // Should pass - exactly at limit
        assert!(tracker.check_budget(50, 0.0).is_ok());
        tracker.record_consumption(50, 0.0);
        assert_eq!(tracker.total_tokens(), 100);

        // Should fail - would exceed limit
        let err = tracker.check_budget(1, 0.0).unwrap_err();
        assert!(matches!(err, BudgetError::TokenBudgetExceeded { used: 101, limit: 100 }));
    }

    #[tokio::test]
    async fn test_budget_tracker_cost_limit() {
        let mut tracker = BudgetTracker {
            total_tokens: 0,
            total_cost_usd: 0.0,
            max_tokens: None,
            max_cost_usd: Some(0.01),
        };

        // Should pass - under limit
        assert!(tracker.check_budget(0, 0.005).is_ok());
        tracker.record_consumption(0, 0.005);
        assert!((tracker.total_cost_usd() - 0.005).abs() < f64::EPSILON);

        // Should fail - would exceed limit
        let err = tracker.check_budget(0, 0.006).unwrap_err();
        assert!(matches!(err, BudgetError::CostBudgetExceeded { used, limit: 0.01 } if (used - 0.011).abs() < f64::EPSILON));
    }

    #[tokio::test]
    async fn test_budget_tracker_both_limits() {
        let mut tracker = BudgetTracker {
            total_tokens: 0,
            total_cost_usd: 0.0,
            max_tokens: Some(100),
            max_cost_usd: Some(0.01),
        };

        // Should pass - both under limits
        assert!(tracker.check_budget(50, 0.005).is_ok());
        tracker.record_consumption(50, 0.005);

        // Should fail - token limit exceeded
        let err = tracker.check_budget(60, 0.0).unwrap_err();
        assert!(matches!(err, BudgetError::TokenBudgetExceeded { .. }));

        // Reset and test cost limit
        tracker = BudgetTracker {
            total_tokens: 0,
            total_cost_usd: 0.0,
            max_tokens: Some(100),
            max_cost_usd: Some(0.01),
        };
        tracker.record_consumption(50, 0.005);
        let err = tracker.check_budget(0, 0.006).unwrap_err();
        assert!(matches!(err, BudgetError::CostBudgetExceeded { .. }));
    }

    #[tokio::test]
    async fn test_budget_tracker_no_limits() {
        let mut tracker = BudgetTracker::default();
        assert!(!tracker.has_budget());
        assert!(tracker.check_budget(1000000, 1000.0).is_ok());
        tracker.record_consumption(1000000, 1000.0);
        assert_eq!(tracker.total_tokens(), 1000000);
        assert!((tracker.total_cost_usd() - 1000.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_budget_tracker_from_task_spec() {
        let spec = TaskSpec {
            id: "test".to_string(),
            name: "Test Task".to_string(),
            description: "Test".to_string(),
            steps: vec![],
            max_tokens: Some(500),
            max_cost_usd: Some(0.05),
            ..Default::default()
        };

        let tracker = BudgetTracker::from_task_spec(&spec);
        assert_eq!(tracker.max_tokens, Some(500));
        assert_eq!(tracker.max_cost_usd, Some(0.05));
        assert_eq!(tracker.total_tokens(), 0);
        assert_eq!(tracker.total_cost_usd(), 0.0);
    }

    #[tokio::test]
    async fn test_budget_error_is_budget_exceeded() {
        assert!(BudgetError::TokenBudgetExceeded { used: 100, limit: 50 }.is_budget_exceeded());
        assert!(BudgetError::CostBudgetExceeded { used: 0.1, limit: 0.05 }.is_budget_exceeded());
        assert!(BudgetError::BudgetExceeded { message: "test".to_string() }.is_budget_exceeded());
    }

    #[tokio::test]
    async fn test_timeout_guard_completes_before_timeout() {
        let guard = TimeoutGuard::from_parts(Some(5), None); // 5 second timeout
        
        let result = guard.run_simple(async {
            sleep(Duration::from_millis(100)).await;
            42
        }).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert!(!guard.is_triggered());
    }

    #[tokio::test]
    async fn test_timeout_guard_deadline_exceeded() {
        let past_deadline = Utc::now() - chrono::Duration::seconds(1);
        let guard = TimeoutGuard::from_parts(None, Some(past_deadline));
        
        let result = guard.run_simple(async {
            sleep(Duration::from_millis(100)).await;
            "completed"
        }).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_timeout());
        assert!(matches!(err, TimeoutError::DeadlineExceeded(_)));
        assert!(guard.is_triggered());
    }

    #[tokio::test]
    async fn test_timeout_guard_future_deadline() {
        let future_deadline = Utc::now() + chrono::Duration::seconds(2);
        let guard = TimeoutGuard::from_parts(None, Some(future_deadline));
        
        let result = guard.run_simple(async {
            sleep(Duration::from_millis(100)).await;
            "completed"
        }).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "completed");
        assert!(!guard.is_triggered());
    }

    #[tokio::test]
    async fn test_timeout_guard_both_timeout_and_deadline() {
        // Deadline is sooner than timeout_sec
    let deadline = Utc::now() + chrono::Duration::seconds(1);
    let guard = TimeoutGuard::from_parts(Some(5), Some(deadline));
        
        let result = guard.run_simple(async {
            sleep(Duration::from_secs(3)).await;
            "completed"
        }).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_timeout());
        // Should be deadline exceeded since it's sooner
        assert!(matches!(err, TimeoutError::DeadlineExceeded(_)));
        assert!(guard.is_triggered());
    }

    #[tokio::test]
    async fn test_timeout_guard_no_timeout_configured() {
        let guard = TimeoutGuard::from_parts(None, None);
        
        let result = guard.run_simple(async {
            sleep(Duration::from_millis(100)).await;
            "completed"
        }).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "completed");
        assert!(!guard.is_triggered());
    }

    #[tokio::test]
    async fn test_timeout_guard_with_result_future() {
        let guard = TimeoutGuard::from_parts(Some(1), None);
        
        let result = guard.run(async {
            sleep(Duration::from_secs(5)).await;
            Ok::<i32, TimeoutError>(42)
        }).await;

        assert!(result.is_err());
        assert!(guard.is_triggered());
    }

    #[tokio::test]
    async fn test_timeout_guard_builder() {
        let guard = TimeoutGuardBuilder::new()
            .timeout_sec(2)
            .deadline(Utc::now() + chrono::Duration::seconds(10))
            .build();

        let result = guard.run_simple(async {
            sleep(Duration::from_millis(50)).await;
            "ok"
        }).await;

        assert!(result.is_ok());
        assert!(!guard.is_triggered());
    }

    #[tokio::test]
    async fn test_timeout_guard_child_pid_signaling() {
        // Test that we can create a guard with child_pid
        // We won't actually send signals in this test since we don't have a real child process
        let guard = TimeoutGuard::from_parts(Some(1), None).with_child_pid(12345);
        
        assert_eq!(guard.config().child_pid, Some(12345));
        
        let result = guard.run_simple(async {
            sleep(Duration::from_secs(5)).await;
            "completed"
        }).await;

        assert!(result.is_err());
        assert!(guard.is_triggered());
    }

    #[tokio::test]
    async fn test_timeout_error_is_timeout() {
        assert!(TimeoutError::TimedOut(5).is_timeout());
        assert!(TimeoutError::DeadlineExceeded(Utc::now()).is_timeout());
        assert!(TimeoutError::Cancelled.is_timeout());
        assert!(!TimeoutError::SignalError("test".to_string()).is_timeout());
        assert!(!TimeoutError::FutureError("test".to_string()).is_timeout());
    }

    #[tokio::test]
    async fn test_timeout_config_remaining_duration() {
        let config = TimeoutConfig::with_timeout_sec(10);
        let remaining = config.remaining_duration();
        assert!(remaining.is_some());
        assert!(remaining.unwrap() <= Duration::from_secs(10));

        let config = TimeoutConfig::with_deadline(Utc::now() + chrono::Duration::seconds(5));
        let remaining = config.remaining_duration();
        assert!(remaining.is_some());
        assert!(remaining.unwrap() <= Duration::from_secs(5));

        let config = TimeoutConfig::with_both(10, Utc::now() + chrono::Duration::seconds(3));
        let remaining = config.remaining_duration();
        assert!(remaining.is_some());
        // Should be the minimum of the two (3 seconds)
        assert!(remaining.unwrap() <= Duration::from_secs(3));
    }
}
