use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

/// 审计事件动作类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// 配额检查
    QuotaCheck,
    /// 并发获取
    ConcurrencyAcquire,
    /// 并发释放
    ConcurrencyRelease,
    /// 速率限制检查
    RateLimitCheck,
    /// 预算检查
    BudgetCheck,
}

impl std::fmt::Display for AuditAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditAction::QuotaCheck => write!(f, "quota_check"),
            AuditAction::ConcurrencyAcquire => write!(f, "concurrency_acquire"),
            AuditAction::ConcurrencyRelease => write!(f, "concurrency_release"),
            AuditAction::RateLimitCheck => write!(f, "rate_limit_check"),
            AuditAction::BudgetCheck => write!(f, "budget_check"),
        }
    }
}

/// 审计事件结果
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditResult {
    /// 允许
    Allow,
    /// 拒绝
    Deny,
    /// 排队中
    Queued,
    /// 超时
    Timeout,
}

impl std::fmt::Display for AuditResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditResult::Allow => write!(f, "allow"),
            AuditResult::Deny => write!(f, "deny"),
            AuditResult::Queued => write!(f, "queued"),
            AuditResult::Timeout => write!(f, "timeout"),
        }
    }
}

/// 审计事件详情
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditDetail {
    /// 当前用量
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_usage: Option<f64>,
    /// 限制值
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// 等待时长（毫秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// 并发槽位
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_slots: Option<u32>,
    /// 令牌桶可用令牌
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_tokens: Option<f64>,
    /// 错误信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 扩展字段
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for AuditDetail {
    fn default() -> Self {
        Self {
            current_usage: None,
            limit: None,
            wait_ms: None,
            parallel_slots: None,
            available_tokens: None,
            error: None,
            extra: serde_json::Map::new(),
        }
    }
}

/// 审计事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// 时间戳（Unix 毫秒）
    pub timestamp: u64,
    /// 项目名称
    pub project: String,
    /// 动作类型
    pub action: AuditAction,
    /// 结果
    pub result: AuditResult,
    /// 详情
    pub detail: AuditDetail,
}

impl AuditEvent {
    /// 创建新的审计事件
    pub fn new(project: impl Into<String>, action: AuditAction, result: AuditResult) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            timestamp,
            project: project.into(),
            action,
            result,
            detail: AuditDetail::default(),
        }
    }

    /// 设置当前用量
    pub fn with_current_usage(mut self, usage: f64) -> Self {
        self.detail.current_usage = Some(usage);
        self
    }

    /// 设置限制值
    pub fn with_limit(mut self, limit: f64) -> Self {
        self.detail.limit = Some(limit);
        self
    }

    /// 设置等待时长
    pub fn with_wait_ms(mut self, wait_ms: u64) -> Self {
        self.detail.wait_ms = Some(wait_ms);
        self
    }

    /// 设置并发槽位
    pub fn with_parallel_slots(mut self, slots: u32) -> Self {
        self.detail.parallel_slots = Some(slots);
        self
    }

    /// 设置可用令牌
    pub fn with_available_tokens(mut self, tokens: f64) -> Self {
        self.detail.available_tokens = Some(tokens);
        self
    }

    /// 设置错误信息
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.detail.error = Some(error.into());
        self
    }

    /// 添加扩展字段
    pub fn with_extra(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.detail.extra.insert(key.into(), value);
        self
    }
}

/// 审计日志写入器
struct AuditWriter {
    /// 项目根目录
    project_dir: PathBuf,
    /// 缓冲写入器
    writer: Option<BufWriter<std::fs::File>>,
}

impl AuditWriter {
    fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            writer: None,
        }
    }

    /// 确保文件已打开
    fn ensure_open(&mut self) -> Result<()> {
        if self.writer.is_none() {
            let audit_path = self.project_dir.join("audit.log");
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&audit_path)
                .with_context(|| format!("打开审计日志失败: {}", audit_path.display()))?;
            self.writer = Some(BufWriter::new(file));
        }
        Ok(())
    }

    /// 写入事件
    fn write_event(&mut self, event: &AuditEvent) -> Result<()> {
        self.ensure_open()?;
        let line = serde_json::to_string(event).context("序列化审计事件失败")?;
        if let Some(writer) = &mut self.writer {
            writer.write_all(line.as_bytes()).context("写入审计日志失败")?;
            writer.write_all(b"\n").context("写入换行符失败")?;
            writer.flush().context("刷新审计日志失败")?;
        }
        Ok(())
    }

    /// 关闭写入器
    fn close(&mut self) -> Result<()> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush().context("关闭时刷新审计日志失败")?;
        }
        Ok(())
    }
}

/// 审计日志发送端
#[derive(Clone)]
pub struct AuditLogger {
    sender: mpsc::UnboundedSender<AuditEvent>,
}

impl AuditLogger {
    /// 发送审计事件（非阻塞）
    pub fn log(&self, event: AuditEvent) {
        if let Err(e) = self.sender.send(event) {
            // 通道已关闭，记录错误但不阻塞
            error!("审计日志通道已关闭: {}", e);
        }
    }

    /// 同步记录配额检查事件（便捷方法）
    pub fn log_quota_check(
        &self,
        project: &str,
        allowed: bool,
        current_usage: f64,
        limit: f64,
        wait_ms: Option<u64>,
    ) {
        let event = AuditEvent::new(project, AuditAction::QuotaCheck, if allowed { AuditResult::Allow } else { AuditResult::Deny })
            .with_current_usage(current_usage)
            .with_limit(limit);
        if let Some(ms) = wait_ms {
            event.with_wait_ms(ms);
        }
        self.log(event);
    }

    /// 同步记录并发获取事件
    pub fn log_concurrency_acquire(&self, project: &str, allowed: bool, slots: u32, wait_ms: Option<u64>) {
        let event = AuditEvent::new(project, AuditAction::ConcurrencyAcquire, if allowed { AuditResult::Allow } else { AuditResult::Deny })
            .with_parallel_slots(slots);
        if let Some(ms) = wait_ms {
            event.with_wait_ms(ms);
        }
        self.log(event);
    }

    /// 同步记录并发释放事件
    pub fn log_concurrency_release(&self, project: &str, slots: u32) {
        let event = AuditEvent::new(project, AuditAction::ConcurrencyRelease, AuditResult::Allow)
            .with_parallel_slots(slots);
        self.log(event);
    }

    /// 同步记录速率限制事件
    pub fn log_rate_limit_check(&self, project: &str, allowed: bool, tokens: f64, wait_ms: Option<u64>) {
        let event = AuditEvent::new(project, AuditAction::RateLimitCheck, if allowed { AuditResult::Allow } else { AuditResult::Deny })
            .with_available_tokens(tokens);
        if let Some(ms) = wait_ms {
            event.with_wait_ms(ms);
        }
        self.log(event);
    }

    /// 同步记录预算检查事件
    pub fn log_budget_check(&self, project: &str, allowed: bool, current: f64, limit: f64) {
        let event = AuditEvent::new(project, AuditAction::BudgetCheck, if allowed { AuditResult::Allow } else { AuditResult::Deny })
            .with_current_usage(current)
            .with_limit(limit);
        self.log(event);
    }
}

/// 启动审计日志后台任务
///
/// 返回 AuditLogger 用于发送事件，以及一个 JoinHandle 用于等待任务结束
pub fn start_audit_logger(project_dir: PathBuf) -> (AuditLogger, tokio::task::JoinHandle<Result<()>>) {
    let (sender, mut receiver) = mpsc::unbounded_channel::<AuditEvent>();
    let logger = AuditLogger { sender };

    let handle = tokio::spawn(async move {
        let mut writer = AuditWriter::new(project_dir);
        
        while let Some(event) = receiver.recv().await {
            if let Err(e) = writer.write_event(&event) {
                error!("写入审计日志失败: {}", e);
            } else {
                debug!("审计事件已记录: project={}, action={}, result={}", event.project, event.action, event.result);
            }
        }
        
        // 通道关闭，刷新并关闭
        if let Err(e) = writer.close() {
            error!("关闭审计日志失败: {}", e);
        }
        
        Ok(())
    });

    (logger, handle)
}

/// 同步版本的审计日志记录（用于不需要异步的场景）
/// 直接追加写入 audit.log，不经过通道
pub fn log_quota_event_sync(project_dir: &Path, event: AuditEvent) -> Result<()> {
    let audit_path = project_dir.join("audit.log");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .with_context(|| format!("打开审计日志失败: {}", audit_path.display()))?;
    
    let line = serde_json::to_string(&event).context("序列化审计事件失败")?;
    file.write_all(line.as_bytes()).context("写入审计日志失败")?;
    file.write_all(b"\n").context("写入换行符失败")?;
    file.flush().context("刷新审计日志失败")?;
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_audit_logger_basic() -> Result<()> {
        let dir = tempdir()?;
        let (logger, handle) = start_audit_logger(dir.path().to_path_buf());
        
        // 发送几个事件
        logger.log(AuditEvent::new("test-project", AuditAction::QuotaCheck, AuditResult::Allow)
            .with_current_usage(10.0)
            .with_limit(100.0));
        logger.log(AuditEvent::new("test-project", AuditAction::QuotaCheck, AuditResult::Deny)
            .with_current_usage(100.0)
            .with_limit(100.0)
            .with_error("budget exceeded"));
        
        // 等待写入完成
        sleep(Duration::from_millis(100)).await;
        
        // 关闭 logger（drop sender）
        drop(logger);
        
        // 等待任务结束
        handle.await??;
        
        // 验证日志文件
        let audit_path = dir.path().join("audit.log");
        assert!(audit_path.exists());
        let content = std::fs::read_to_string(&audit_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        
        let event1: AuditEvent = serde_json::from_str(lines[0])?;
        assert_eq!(event1.project, "test-project");
        assert_eq!(event1.action, AuditAction::QuotaCheck);
        assert_eq!(event1.result, AuditResult::Allow);
        assert_eq!(event1.detail.current_usage, Some(10.0));
        assert_eq!(event1.detail.limit, Some(100.0));
        
        let event2: AuditEvent = serde_json::from_str(lines[1])?;
        assert_eq!(event2.result, AuditResult::Deny);
        assert_eq!(event2.detail.error, Some("budget exceeded".to_string()));
        
        Ok(())
    }

    #[tokio::test]
    async fn test_audit_logger_convenience_methods() -> Result<()> {
        let dir = tempdir()?;
        let (logger, handle) = start_audit_logger(dir.path().to_path_buf());
        
        logger.log_quota_check("proj1", true, 5.0, 10.0, Some(0));
        logger.log_concurrency_acquire("proj1", true, 3, Some(10));
        logger.log_concurrency_release("proj1", 3);
        logger.log_rate_limit_check("proj1", true, 5.0, Some(0));
        logger.log_budget_check("proj1", false, 100.0, 50.0);
        
        sleep(Duration::from_millis(100)).await;
        drop(logger);
        handle.await??;
        
        let audit_path = dir.path().join("audit.log");
        let content = std::fs::read_to_string(&audit_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);
        
        // 验证每种动作类型都被记录
        let actions: Vec<AuditAction> = lines.iter()
            .map(|l| serde_json::from_str::<AuditEvent>(l).unwrap().action)
            .collect();
        assert_eq!(actions, vec![
            AuditAction::QuotaCheck,
            AuditAction::ConcurrencyAcquire,
            AuditAction::ConcurrencyRelease,
            AuditAction::RateLimitCheck,
            AuditAction::BudgetCheck,
        ]);
        
        Ok(())
    }

    #[test]
    fn test_sync_log_quota_event() -> Result<()> {
        let dir = tempdir()?;
        let event = AuditEvent::new("sync-test", AuditAction::QuotaCheck, AuditResult::Allow)
            .with_current_usage(1.0)
            .with_limit(10.0);
        
        log_quota_event_sync(dir.path(), event.clone())?;
        log_quota_event_sync(dir.path(), event)?;
        
        let audit_path = dir.path().join("audit.log");
        let content = std::fs::read_to_string(&audit_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        
        Ok(())
    }

    #[test]
    fn test_audit_event_builder() {
        let event = AuditEvent::new("test", AuditAction::QuotaCheck, AuditResult::Allow)
            .with_current_usage(5.0)
            .with_limit(10.0)
            .with_wait_ms(100)
            .with_parallel_slots(3)
            .with_available_tokens(5.5)
            .with_error("test error")
            .with_extra("custom_field", serde_json::json!("custom_value"));
        
        assert_eq!(event.project, "test");
        assert_eq!(event.action, AuditAction::QuotaCheck);
        assert_eq!(event.result, AuditResult::Allow);
        assert_eq!(event.detail.current_usage, Some(5.0));
        assert_eq!(event.detail.limit, Some(10.0));
        assert_eq!(event.detail.wait_ms, Some(100));
        assert_eq!(event.detail.parallel_slots, Some(3));
        assert_eq!(event.detail.available_tokens, Some(5.5));
        assert_eq!(event.detail.error, Some("test error".to_string()));
        assert_eq!(event.detail.extra.get("custom_field"), Some(&serde_json::json!("custom_value")));
    }
}
