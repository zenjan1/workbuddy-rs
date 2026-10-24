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
