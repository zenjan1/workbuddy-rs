//! LLM 延迟基准模块。
//!
//! 该模块顺序发送固定短请求，采集成功延迟与失败次数，并提供无外部依赖的
//! ASCII 表格输出。调用方负责把 [`BENCH_REQUEST`] 适配到现有 `Llm` trait。

use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use clap::Args;

/// 固定基准请求：要求模型只回复 `pong`，尽量减少生成长度对延迟的干扰。
pub const BENCH_REQUEST: &str =
    "This is a latency benchmark. Reply with exactly one word: pong. Do not add anything else.";

/// 基准请求的预期短回复。
pub const BENCH_EXPECTED_RESPONSE: &str = "pong";

/// `bench` 子命令参数。
#[derive(Debug, Clone, Args)]
pub struct BenchParams {
    /// 请求次数，必须至少为 1。
    #[arg(
        long,
        default_value_t = 10,
        value_parser = parse_request_count,
        help = "Number of requests to send (minimum: 1)"
    )]
    pub requests: usize,

    /// 可选模型名称；由 main 在构造 LLM 客户端时使用。
    #[arg(long, value_name = "MODEL", help = "Model name to benchmark")]
    pub model: Option<String>,
}

impl BenchParams {
    /// 以编程方式构造参数，并校验请求数量。
    pub fn new(requests: usize, model: Option<String>) -> Result<Self> {
        let params = Self { requests, model };
        params.validate()?;
        Ok(params)
    }

    /// 校验参数。
    pub fn validate(&self) -> Result<()> {
        if self.requests == 0 {
            bail!("bench requests must be at least 1");
        }
        Ok(())
    }
}

impl Default for BenchParams {
    fn default() -> Self {
        Self {
            requests: 10,
            model: None,
        }
    }
}

/// clap 使用的正整数解析器。
fn parse_request_count(input: &str) -> std::result::Result<usize, String> {
    let count = input
        .parse::<usize>()
        .map_err(|error| format!("invalid request count `{input}`: {error}"))?;
    if count == 0 {
        return Err("request count must be at least 1".to_string());
    }
    Ok(count)
}

/// 轻量时钟抽象，便于在测试中注入确定性时间。
pub trait Clock {
    /// 返回单调时间线上的微秒值。生产实现基于 [`Instant`]。
    fn now_micros(&self) -> u128;
}

/// 生产环境时钟，内部使用真实 [`Instant`]。
#[derive(Debug, Clone)]
pub struct SystemClock {
    started_at: Instant,
}

impl SystemClock {
    /// 创建并记录起始时刻。
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_micros(&self) -> u128 {
        self.started_at.elapsed().as_micros()
    }
}

/// 测试用假时钟，可由请求脚本显式推进。
#[derive(Debug)]
pub struct FakeClock {
    micros: std::sync::atomic::AtomicU64,
}

impl FakeClock {
    /// 创建从 0 微秒开始的假时钟。
    pub const fn new() -> Self {
        Self {
            micros: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// 推进指定时长。
    pub fn advance(&self, duration: Duration) {
        let micros = duration.as_micros().min(u64::MAX as u128) as u64;
        self.micros
            .fetch_add(micros, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for FakeClock {
    fn now_micros(&self) -> u128 {
        self.micros
            .load(std::sync::atomic::Ordering::SeqCst) as u128
    }
}

/// 一次基准运行的统计结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BenchReport {
    /// 总请求数。
    pub requests: usize,
    /// 成功请求数。
    pub successful: usize,
    /// 失败请求数。
    pub failed: usize,
    /// 成功率，范围为 `0.0..=1.0`。
    pub success_rate: f64,
    /// 最小成功延迟；无成功请求时为 `None`。
    pub min: Option<Duration>,
    /// 平均成功延迟；无成功请求时为 `None`。
    pub avg: Option<Duration>,
    /// 成功延迟的 p95；无成功请求时为 `None`。
    pub p95: Option<Duration>,
    /// 总耗时，包含失败请求耗时。
    pub total_elapsed: Duration,
}

impl BenchReport {
    /// 根据成功延迟样本和总耗时构造统计结果。
    fn from_samples(
        requests: usize,
        mut success_latencies_us: Vec<u128>,
        total_elapsed_us: u128,
    ) -> Self {
        let successful = success_latencies_us.len();
        let failed = requests.saturating_sub(successful);
        let success_rate = if requests == 0 {
            0.0
        } else {
            successful as f64 / requests as f64
        };

        success_latencies_us.sort_unstable();

        let to_duration = |micros: u128| -> Duration {
            Duration::from_micros(micros.min(u64::MAX as u128) as u64)
        };

        let min = success_latencies_us.first().copied().map(to_duration);

        let avg = if successful > 0 {
            let total_success: u128 = success_latencies_us.iter().copied().sum();
            Some(to_duration(total_success / successful as u128))
        } else {
            None
        };

        // nearest-rank: rank = ceil(0.95 * N)，数组下标为 rank - 1。
        let p95 = if successful > 0 {
            let n = successful as u128;
            let rank = ((95 * n + 99) / 100) as usize;
            let index = rank.saturating_sub(1);
            Some(to_duration(success_latencies_us[index]))
        } else {
            None
        };

        Self {
            requests,
            successful,
            failed,
            success_rate,
            min,
            avg,
            p95,
            total_elapsed: to_duration(total_elapsed_us),
        }
    }
}

/// 顺序执行基准请求。
///
/// `invoke` 由调用方提供，用于适配现有 `Llm` trait，并应发送
/// [`BENCH_REQUEST`]。它接收从 0 开始的请求序号，便于测试脚本控制结果。
/// 单次请求失败只会增加失败计数，不会中断后续请求。
pub async fn run_bench<F, Fut, C>(
    params: &BenchParams,
    clock: &C,
    mut invoke: F,
) -> Result<BenchReport>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<String>>,
    C: Clock + ?Sized,
{
    params.validate()?;

    let mut success_latencies_us = Vec::with_capacity(params.requests);
    let started_at = clock.now_micros();

    for index in 0..params.requests {
        let request_started_at = clock.now_micros();

        // 无论成功还是失败都继续循环；错误耗时仍发生在总时间窗口内。
        let outcome = invoke(index).await;
        let elapsed_us = clock
            .now_micros()
            .saturating_sub(request_started_at);

        if outcome.is_ok() {
            success_latencies_us.push(elapsed_us);
        }
    }

    let total_elapsed_us = clock.now_micros().saturating_sub(started_at);

    Ok(BenchReport::from_samples(
        params.requests,
        success_latencies_us,
        total_elapsed_us,
    ))
}

/// 供 main 调用：使用真实时钟执行基准测试，并打印 ASCII 表格。
pub async fn execute<F, Fut>(params: BenchParams, invoke: F) -> Result<BenchReport>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    params.validate()?;

    let clock = SystemClock::new();
    let report = run_bench(&params, &clock, invoke).await?;

    print_report(&report, params.model.as_deref());
    Ok(report)
}

/// 打印基准报告表格。
pub fn print_report(report: &BenchReport, model: Option<&str>) {
    println!("{}", render_report(report, model));
}

/// 将基准报告渲染为 ASCII 表格字符串。
pub fn render_report(report: &BenchReport, model: Option<&str>) -> String {
    let rows = vec![
        ("model".to_string(), table_escape(model.unwrap_or("default"))),
        ("requests".to_string(), report.requests.to_string()),
        ("successful".to_string(), report.successful.to_string()),
        ("failed".to_string(), report.failed.to_string()),
        (
            "success_rate".to_string(),
            format!("{:.2}%", report.success_rate * 100.0),
        ),
        ("min".to_string(), format_duration(report.min)),
        ("avg".to_string(), format_duration(report.avg)),
        ("p95".to_string(), format_duration(report.p95)),
        (
            "total_elapsed".to_string(),
            format_duration(Some(report.total_elapsed)),
        ),
    ];

    render_table(&rows)
}

/// 无外部依赖的 ASCII 两列表格渲染。
fn render_table(rows: &[(String, String)]) -> String {
    let key_width = std::cmp::max(
        "Metric".chars().count(),
        rows.iter().map(|(key, _)| key.chars().count()).max().unwrap_or(0),
    );
    let value_width = std::cmp::max(
        "Value".chars().count(),
        rows.iter().map(|(_, value)| value.chars().count()).max().unwrap_or(0),
    );

    let separator = format!(
        "+{}+{}+",
        "-".repeat(key_width + 2),
        "-".repeat(value_width + 2)
    );

    let mut output = String::new();
    use std::fmt::Write as _;

    writeln!(output, "{separator}").ok();
    writeln!(
        output,
        "| {} | {} |",
        pad_cell("Metric", key_width),
        pad_cell("Value", value_width)
    )
    .ok();
    writeln!(output, "{separator}").ok();

    for (key, value) in rows {
        writeln!(
            output,
            "| {} | {} |",
            pad_cell(key, key_width),
            pad_cell(value, value_width)
        )
        .ok();
    }

    writeln!(output, "{separator}").ok();
    output
}

/// 按字符宽度在单元格右侧补空格。
fn pad_cell(value: &str, width: usize) -> String {
    let mut cell = value.to_string();
    let missing = width.saturating_sub(value.chars().count());
    cell.extend(std::iter::repeat(' ').take(missing));
    cell
}

/// 避免模型名中的换行破坏表格。
fn table_escape(value: &str) -> String {
    value.replace('\n', " ").replace('\r', " ")
}

/// 格式化可选耗时；`None` 渲染为 `N/A`。
fn format_duration(value: Option<Duration>) -> String {
    value.map(format_duration_value).unwrap_or_else(|| "N/A".to_string())
}

/// 格式化耗时，单位随量级自动切换。
fn format_duration_value(value: Duration) -> String {
    let micros = value.as_micros();
    if micros < 1_000 {
        format!("{micros} us")
    } else if micros < 1_000_000 {
        format!("{:.3} ms", micros as f64 / 1_000.0)
    } else {
        format!("{:.3} s", micros as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// 确定性 MockLlm：按脚本返回成功或错误，并通过假时钟注入耗时。
    struct MockLlm {
        outcomes: Mutex<VecDeque<std::result::Result<u64, &'static str>>>,
        clock: FakeClock,
    }

    impl MockLlm {
        fn new(outcomes: Vec<std::result::Result<u64, &'static str>>) -> Self {
            Self {
                outcomes: Mutex::new(VecDeque::from(outcomes)),
                clock: FakeClock::new(),
            }
        }

        async fn send(&self, _index: usize) -> Result<String> {
            let outcome = self
                .outcomes
                .lock()
                .expect("mock outcomes lock poisoned")
                .pop_front()
                .unwrap_or(Err("unexpected bench request"));

            match outcome {
                Ok(latency_ms) => {
                    self.clock.advance(Duration::from_millis(latency_ms));
                    Ok(BENCH_EXPECTED_RESPONSE.to_string())
                }
                Err(message) => {
                    // 失败请求也消耗时间，但该时间不会进入成功延迟分布。
                    self.clock.advance(Duration::from_millis(2));
                    bail!("{message}")
                }
            }
        }
    }

    #[tokio::test]
    async fn calculates_success_latency_statistics() {
        assert!(BENCH_REQUEST.contains("pong"));

        let params = BenchParams::new(10, Some("test-model".to_string())).unwrap();
        let outcomes = (1u64..=10)
            .map(Ok)
            .collect::<Vec<std::result::Result<u64, &'static str>>>();
        let mock = MockLlm::new(outcomes);

        let report = run_bench(&params, &mock.clock, |index| mock.send(index))
            .await
            .unwrap();

        assert_eq!(report.requests, 10);
        assert_eq!(report.successful, 10);
        assert_eq!(report.failed, 0);
        assert_eq!(report.success_rate, 1.0);
        assert_eq!(report.min, Some(Duration::from_millis(1)));
        assert_eq!(report.avg, Some(Duration::from_micros(5_500)));
        // N=10 时 nearest-rank p95 取排序后的第 10 个样本。
        assert_eq!(report.p95, Some(Duration::from_millis(10)));
        assert_eq!(report.total_elapsed, Duration::from_millis(55));

        let table = render_report(&report, Some("test-model"));
        assert!(table.contains("test-model"));
        assert!(table.contains("100.00%"));
        assert!(table.contains("1.000 ms"));
        assert!(table.contains("5.500 ms"));
        assert!(table.contains("10.000 ms"));
    }

    #[tokio::test]
    async fn keeps_failed_request_latency_out_of_success_distribution() {
        let params = BenchParams::new(5, None).unwrap();
        let outcomes = vec![
            Ok(10),
            Ok(20),
            Err("synthetic failure"),
            Ok(30),
            Err("synthetic failure"),
        ];
        let mock = MockLlm::new(outcomes);

        let report = run_bench(&params, &mock.clock, |index| mock.send(index))
            .await
            .unwrap();

        assert_eq!(report.requests, 5);
        assert_eq!(report.successful, 3);
        assert_eq!(report.failed, 2);
        assert!((report.success_rate - 0.6).abs() < f64::EPSILON);
        assert_eq!(report.min, Some(Duration::from_millis(10)));
        assert_eq!(report.avg, Some(Duration::from_millis(20)));
        assert_eq!(report.p95, Some(Duration::from_millis(30)));
        // 成功耗时 60ms，两个失败各 2ms，均计入总耗时。
        assert_eq!(report.total_elapsed, Duration::from_millis(64));
    }

    #[tokio::test]
    async fn renders_na_for_latencies_when_all_requests_fail() {
        let params = BenchParams::new(3, None).unwrap();
        let outcomes = vec![Err("one"), Err("two"), Err("three")];
        let mock = MockLlm::new(outcomes);

        let report = run_bench(&params, &mock.clock, |index| mock.send(index))
            .await
            .unwrap();

        assert_eq!(report.requests, 3);
        assert_eq!(report.successful, 0);
        assert_eq!(report.failed, 3);
        assert_eq!(report.success_rate, 0.0);
        assert_eq!(report.min, None);
        assert_eq!(report.avg, None);
        assert_eq!(report.p95, None);
        assert_eq!(report.total_elapsed, Duration::from_millis(6));

        let table = render_report(&report, None);
        assert!(table.contains("N/A"));
        assert!(table.contains("0.00%"));
        assert!(table.contains("default"));
    }

    #[test]
    fn rejects_zero_requests() {
        assert!(BenchParams::new(0, None).is_err());
        assert_eq!(BenchParams::default().requests, 10);
    }

    #[test]
    fn parses_positive_request_count() {
        assert!(parse_request_count("0").is_err());
        assert!(parse_request_count("-1").is_err());
        assert!(parse_request_count("abc").is_err());
        assert_eq!(parse_request_count("1").unwrap(), 1);
        assert_eq!(parse_request_count("25").unwrap(), 25);
    }
}
