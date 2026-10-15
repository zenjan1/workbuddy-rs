use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::plan::Plan;
use crate::utils::{parse_task_id, slugify, unique_slug};

/// 任务工作区:每次 run 在 workspace_dir 下生成独立目录,
/// 持久化计划、各步骤产出与执行结果,供 list/show 查询。
pub struct Workspace {
    pub dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
pub struct StepRecord {
    pub id: usize,
    pub name: String,
    pub agent: String,
    pub skill: String,
    pub kind: String,
    pub ok: bool,
    pub error: Option<String>,
    pub seconds: f64,
    pub output_preview: String,
}

#[derive(Serialize, Deserialize)]
pub struct RunRecord {
    #[serde(default)]
    pub token_usage: Option<u64>,
    #[serde(default)]
    pub char_usage: Option<u64>,
    pub id: String,
    pub goal: String,
    pub created: String,
    pub model: String,
    pub mock: bool,
    pub steps: Vec<StepRecord>,
}

impl Workspace {
    /// 创建新的任务工作区目录(含 outputs 子目录)。
    /// 任务 id 基于标题 slug 生成,并通过 unique_slug 保证同根目录下唯一。
    pub fn create_new(root: &Path, title: &str) -> Result<Self> {
        let base = slugify(title);
        let existing: Vec<String> = std::fs::read_dir(root)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        let id = unique_slug(&base, &existing);
        let dir = root.join(&id);
        std::fs::create_dir_all(dir.join("outputs"))
            .with_context(|| format!("创建工作区失败: {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// 打开已存在的任务工作区。
    /// 支持 slug 与旧数字 id:通过 utils::parse_task_id 解析实际目录名。
    pub fn open(root: &Path, id: &str) -> Result<Self> {
        let actual_id = parse_task_id(id);
        let dir = root.join(actual_id);
        if !dir.is_dir() {
            anyhow::bail!("找不到任务 {}: {}", id, dir.display());
        }
        Ok(Self { dir })
    }

    /// 将 task_id 字符串解析为任务在 Workspace::root 列表中的索引。
    /// 数字输入保持原有数字索引语义(0-based);失败后回退到标题精确/前缀匹配。
    pub fn resolve_task_id(root: &Path, input: &str) -> Result<usize, String> {
        let runs = Workspace::root(root);
        if let Ok(idx) = input.parse::<usize>() {
            if idx >= runs.len() {
                return Err(format!(
                    "任务索引 {} 超出范围(共 {} 个任务)",
                    idx,
                    runs.len()
                ));
            }
            return Ok(idx);
        }

        let exact: Vec<(usize, String)> = runs
            .iter()
            .enumerate()
            .filter(|(_, r)| r.goal == input)
            .map(|(i, r)| (i, r.goal.clone()))
            .collect();
        match exact.len() {
            1 => return Ok(exact[0].0),
            n if n > 1 => {
                let titles: Vec<String> = exact.iter().map(|(_, t)| t.clone()).collect();
                return Err(format!(
                    "任务标题 '{}' 匹配到多个任务: {}",
                    input,
                    titles.join(", ")
                ));
            }
            _ => {}
        }

        let prefix: Vec<(usize, String)> = runs
            .iter()
            .enumerate()
            .filter(|(_, r)| r.goal.starts_with(input))
            .map(|(i, r)| (i, r.goal.clone()))
            .collect();
        match prefix.len() {
            1 => Ok(prefix[0].0),
            0 => Err(format!("未找到任务: {}", input)),
            _ => {
                let titles: Vec<String> = prefix.iter().map(|(_, t)| t.clone()).collect();
                Err(format!(
                    "任务标题前缀 '{}' 匹配到多个任务: {}",
                    input,
                    titles.join(", ")
                ))
            }
        }
    }

    pub fn root(root: &Path) -> Vec<RunRecord> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let mut runs = Vec::new();
        for e in entries.flatten() {
            if let Ok(rec) = Self::open(root, &e.file_name().to_string_lossy())
                .and_then(|ws| ws.load_record())
            {
                runs.push(rec);
            }
        }
        runs.sort_by(|a, b| b.id.cmp(&a.id));
        runs
    }

    pub fn load_record(&self) -> Result<RunRecord> {
        let meta = serde_json::from_str(&std::fs::read_to_string(self.meta_path())?)?;
        Ok(meta)
    }

    pub fn meta_path(&self) -> PathBuf {
        self.dir.join("meta.json")
    }

    /// 规划阶段:落盘 goal 与 plan。
    pub fn save_plan(&self, run: &RunRecord, plan: &Plan) -> Result<()> {
        std::fs::write(
            self.dir.join("plan.json"),
            serde_json::to_string_pretty(plan)?,
        )
        .context("写入 plan.json 失败")?;
        std::fs::write(
            self.meta_path(),
            serde_json::to_string_pretty(run)?,
        )
        .context("写入 meta.json 失败")?;
        Ok(())
    }

    /// 执行阶段:保存某步骤的完整产出。
    pub fn save_step_output(&self, id: usize, text: &str) -> Result<()> {
        std::fs::write(self.dir.join("outputs").join(format!("{id}.md")), text)
            .with_context(|| format!("写入步骤 {} 产出失败", id))
    }

    /// 执行阶段:更新 meta 中的步骤结果与最终报告。
    pub fn finish_run(&self, run: &mut RunRecord, report: &str) -> Result<()> {
        std::fs::write(self.dir.join("report.md"), report)
            .context("写入 report.md 失败")?;
        std::fs::write(
            self.meta_path(),
            serde_json::to_string_pretty(run)?,
        )
        .context("更新 meta.json 失败")?;
        Ok(())
    }

    /// 读取最新执行日志/产出摘要,供 TUI 每帧轮询。
    /// 返回首行当前步骤摘要,随后为步骤输出文件尾部(最多 max_lines 行)。
    pub fn latest_logs(&self, max_lines: usize) -> Vec<String> {
        let mut lines = Vec::new();
        if let Ok(rec) = self.load_record() {
            if let Some(step) = rec.steps.last() {
                lines.push(format!(
                    "[step {}] {}: {}",
                    step.id, step.name, step.output_preview
                ));
                let out_path = self.dir.join("outputs").join(format!("{}.md", step.id));
                if let Ok(text) = std::fs::read_to_string(out_path) {
                    let tail: Vec<_> = text.lines().rev().take(max_lines).collect();
                    lines.extend(tail.into_iter().rev().map(|s| s.to_string()));
                }
            }
        }
        lines
    }

    /// 读取 workspace 状态文件并格式化为可读摘要;无状态文件时返回 "not running"。
    pub fn status_summary(root: &Path) -> String {
        let status_path = root.join("status.json");
        let Ok(text) = std::fs::read_to_string(status_path) else {
            return "not running".to_string();
        };
        let Ok(status) = serde_json::from_str::<serde_json::Value>(&text) else {
            return "not running".to_string();
        };

        let running = status
            .get("running")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let completed = status
            .get("completed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let failed = status
            .get("failed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let mut lines = vec![
            format!(
                "supervisor: {}",
                if running { "running" } else { "not running" }
            ),
            format!("completed: {}", completed),
            format!("failed: {}", failed),
        ];

        let logs = status
            .get("last_logs")
            .or_else(|| status.get("recent_logs"))
            .and_then(|v| v.as_array());
        if let Some(logs) = logs {
            if !logs.is_empty() {
                lines.push("recent logs:".to_string());
                for log in logs {
                    if let Some(s) = log.as_str() {
                        lines.push(format!("  - {}", s));
                    }
                }
            }
        }

        lines.join("\n")
    }
}

/// 纯函数:根据已完成条目推进路线图。返回更新后的文本;无任何改动时返回 None。
fn advance_roadmap_text(input: &str, today: &str) -> Option<String> {
    #[derive(Debug, Clone)]
    struct Item {
        raw: String,
        key: String,
    }

    fn normalize(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn parse_item(line: &str, checked: bool) -> Option<Item> {
        let prefix = if checked { "- [x]" } else { "- [ ]" };
        let trimmed = line.trim_start();
        let rest = trimmed.strip_prefix(prefix)?;
        if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
            return None;
        }
        let raw = rest.trim().to_string();
        if raw.is_empty() {
            return None;
        }
        Some(Item {
            key: normalize(&raw),
            raw,
        })
    }

    fn log_exists(text: &str, today: &str, item: &str) -> bool {
        let needle = format!("完成:{}", item);
        text.lines().any(|line| {
            let line = line.trim();
            line.starts_with(today) && line.contains(&needle)
        })
    }

    let mut completed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for line in input.split('\n') {
        if let Some(item) = parse_item(line, true) {
            completed.insert(item.key, item.raw);
        }
    }
    if completed.is_empty() {
        return None;
    }

    let mut updated_any = false;
    let mut logs_to_append: Vec<String> = Vec::new();
    let mut output_lines: Vec<String> = Vec::new();

    for line in input.split('\n') {
        let mut new_line = line.to_string();
        if let Some(item) = parse_item(line, false) {
            if let Some(matched_raw) = completed.get(&item.key) {
                new_line = line.replacen("[ ]", "[x]", 1);
                updated_any = true;
                if !log_exists(input, today, matched_raw) {
                    let log_line = format!("{} 完成:{}", today, matched_raw);
                    if !logs_to_append.contains(&log_line) {
                        logs_to_append.push(log_line);
                    }
                }
            }
        }
        output_lines.push(new_line);
    }

    if !updated_any {
        return None;
    }

    let ends_with_newline = input.ends_with('\n');
    let mut output = output_lines.join("\n");
    if ends_with_newline {
        output.push('\n');
    }

    for log_line in logs_to_append {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&log_line);
        output.push('\n');
    }

    Some(output)
}

/// 读取 VISION.md,推进路线图。返回是否修改了文件。
pub fn advance_roadmap(path: &Path) -> Result<bool> {
    let original = std::fs::read_to_string(path)
        .with_context(|| format!("读取 VISION.md 失败: {}", path.display()))?;
    let today = today_string();
    let Some(updated) = advance_roadmap_text(&original, &today) else {
        return Ok(false);
    };
    if updated != original {
        std::fs::write(path, &updated)
            .with_context(|| format!("写入 VISION.md 失败: {}", path.display()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// 当前日期字符串。标准库无本地时区,这里使用 UTC 日期;
/// 如需严格本地日期,可替换为 chrono::Local::now().format("%Y-%m-%d")。
fn today_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// 将 UNIX 天数转换为公历日期(Howard Hinnant 的 civil_from_days 算法)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

static RUN_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 生成 run id:unix 秒十六进制-进程内序号哈希(同秒内保证不重复)。
fn new_run_id() -> String {
    use std::sync::atomic::Ordering;
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut h = 0u64;
    for _ in 0..32 {
        h = h
            .wrapping_mul(6364136223846793005)
            .wrapping_add(t.wrapping_add(1442695040888963407).wrapping_add(seq as u64));
    }
    // 前缀 13 位(世纪内足够)+ 后缀固定 8 位十六进制,总长恒定 22
    format!("{:013x}-{:08x}", t, h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn run_id_is_unique_and_formatted() {
        let a = new_run_id();
        let b = new_run_id();
        assert_eq!(a.len(), b.len());
        assert!(a.contains('-'));
        // 同一时刻可能前缀相同,后缀应不同(或整体不同)
        assert_ne!(a, b);
    }

    #[test]
    fn workspace_lifecycle() {
        let tmp = std::env::temp_dir().join(format!("wb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = Config {
            cost_per_1k_tokens: 0.0,
            price: 0.0,
            max_budget: 0.0,
            api_key: None,
            base_url: "x".into(),
            model: "x".into(),
            max_parallel: 1,
            workspace_dir: tmp.clone(),
            repo_dir: tmp.clone(),
            request_timeout_secs: 1,
            mock: true,
            webhook_url: None,
            webhook_retry: 0,
            webhook_timeout_secs: 1,
        };
        let ws = Workspace::create_new(&cfg.workspace_dir, "测试 任务").unwrap();
        let task_id = ws.dir.file_name().unwrap().to_string_lossy().to_string();
        let mut run = RunRecord {
            token_usage: None,
            char_usage: None,
            id: task_id,
            goal: "测试".into(),
            created: "now".into(),
            model: "mock".into(),
            mock: true,
            steps: vec![],
        };
        ws.save_step_output(1, "产出内容").unwrap();
        let plan = crate::plan::Plan::from_llm(
            "测试",
            r#"{"goal":"测试","steps":[{"id":1,"name":"a","agent":"writer","skill":"report-writing","prompt":"p","kind":"llm","needs":[]}]}"#,
        )
        .unwrap();
        ws.save_plan(&run, &plan).unwrap();
        run.steps.push(StepRecord {
            id: 1,
            name: "a".into(),
            agent: "writer".into(),
            skill: "report-writing".into(),
            kind: "llm".into(),
            ok: true,
            error: None,
            seconds: 0.1,
            output_preview: "产出内容".into(),
        });
        ws.finish_run(&mut run, "# 报告\n完成").unwrap();

        // 重新打开并校验
        let ws2 = Workspace::open(&cfg.workspace_dir, &run.id).unwrap();
        let rec = ws2.load_record().unwrap();
        assert_eq!(rec.steps.len(), 1);
        let report = std::fs::read_to_string(ws2.dir.join("report.md")).unwrap();
        assert!(report.contains("报告"));
        let listed = Workspace::root(&cfg.workspace_dir);
        assert_eq!(listed.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn status_summary_handles_no_status_file() {
        let tmp = std::env::temp_dir().join(format!("wb-status-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(Workspace::status_summary(&tmp), "not running");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn status_summary_formats_completed_and_failed() {
        let tmp = std::env::temp_dir().join(format!("wb-status-records-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("status.json"),
            r#"{"running":false,"completed":2,"failed":1,"last_logs":["step 1 ok","step 2 failed"]}"#,
        )
        .unwrap();
        let out = Workspace::status_summary(&tmp);
        assert!(out.contains("supervisor: not running"));
        assert!(out.contains("completed: 2"));
        assert!(out.contains("failed: 1"));
        assert!(out.contains("step 1 ok"));
        assert!(out.contains("step 2 failed"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn status_summary_formats_running() {
        let tmp = std::env::temp_dir().join(format!("wb-status-running-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("status.json"),
            r#"{"running":true,"completed":0,"failed":0,"last_logs":["initializing"]}"#,
        )
        .unwrap();
        let out = Workspace::status_summary(&tmp);
        assert!(out.contains("supervisor: running"));
        assert!(out.contains("completed: 0"));
        assert!(out.contains("failed: 0"));
        assert!(out.contains("initializing"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn advance_roadmap_updates_matching_item_and_appends_log() {
        let input = "\
# VISION

## 路线图
- [x] 完成任务 A
- [ ] 完成任务 A
- [ ] 其他任务

【日志】
2025-01-01 完成:旧任务
";
        let updated = advance_roadmap_text(input, "2025-04-08").unwrap();
        assert!(updated.contains("- [x] 完成任务 A"));
        assert!(updated.contains("- [ ] 其他任务"));
        assert!(updated.contains("2025-04-08 完成:完成任务 A"));
        assert!(updated.contains("2025-01-01 完成:旧任务"));
        assert_eq!(updated.matches("- [x] 完成任务 A").count(), 2);
        assert_eq!(updated.matches("- [ ] 完成任务 A").count(), 0);
    }

    #[test]
    fn advance_roadmap_skips_duplicate_log_line() {
        let input = "\
- [x] 完成任务 A
- [ ] 完成任务 A

【日志】
2025-04-08 完成:完成任务 A
";
        let updated = advance_roadmap_text(input, "2025-04-08").unwrap();
        assert!(updated.contains("- [x] 完成任务 A"));
        assert_eq!(updated.matches("2025-04-08 完成:完成任务 A").count(), 1);
    }

    #[test]
    fn advance_roadmap_preserves_unmatched_content() {
        let input = "\
# 标题

一些介绍文字。
- [x] 完成任务 A
- [ ] 完成任务 A
- [ ] 未匹配文本

【日志】
2025-01-01 完成:历史
";
        let updated = advance_roadmap_text(input, "2025-04-08").unwrap();
        assert!(updated.contains("# 标题"));
        assert!(updated.contains("一些介绍文字。"));
        assert!(updated.contains("- [ ] 未匹配文本"));
        assert!(updated.contains("2025-01-01 完成:历史"));
        assert!(!updated.contains("完成:未匹配文本"));
    }

    #[test]
    fn advance_roadmap_returns_none_without_changes() {
        let input = "# VISION\n\n- [ ] 任务 A\n- [ ] 任务 B\n";
        assert!(advance_roadmap_text(input, "2025-04-08").is_none());
    }

    fn create_task_dir(root: &Path, id: &str, goal: &str) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let run = RunRecord {
            token_usage: None,
            char_usage: None,
            id: id.to_string(),
            goal: goal.to_string(),
            created: "now".into(),
            model: "mock".into(),
            mock: true,
            steps: vec![],
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_string(&run).unwrap()).unwrap();
    }

    #[test]
    fn resolve_task_id_parses_numeric_index() {
        let tmp = std::env::temp_dir().join(format!("wb-resolve-num-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        create_task_dir(&tmp, "task-a", "alpha");
        create_task_dir(&tmp, "task-b", "beta");
        create_task_dir(&tmp, "task-c", "gamma");

        let idx = Workspace::resolve_task_id(&tmp, "1").unwrap();
        assert_eq!(idx, 1);
        let err = Workspace::resolve_task_id(&tmp, "99").unwrap_err();
        assert!(err.contains("超出范围"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_task_id_matches_exact_title() {
        let tmp = std::env::temp_dir().join(format!("wb-resolve-exact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        create_task_dir(&tmp, "task-a", "my-feature");
        create_task_dir(&tmp, "task-b", "other");
        create_task_dir(&tmp, "task-c", "another");

        let idx = Workspace::resolve_task_id(&tmp, "my-feature").unwrap();
        let runs = Workspace::root(&tmp);
        assert_eq!(runs[idx].goal, "my-feature");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_task_id_matches_prefix_title() {
        let tmp = std::env::temp_dir().join(format!("wb-resolve-prefix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        create_task_dir(&tmp, "task-a", "my-feature");
        create_task_dir(&tmp, "task-b", "other");
        create_task_dir(&tmp, "task-c", "another");

        let idx = Workspace::resolve_task_id(&tmp, "my-").unwrap();
        let runs = Workspace::root(&tmp);
        assert_eq!(runs[idx].goal, "my-feature");
        assert!(runs[idx].goal.starts_with("my-"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_task_id_detects_prefix_conflicts() {
        let tmp = std::env::temp_dir().join(format!("wb-resolve-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        create_task_dir(&tmp, "task-a", "my-feature");
        create_task_dir(&tmp, "task-b", "my-other");

        let err = Workspace::resolve_task_id(&tmp, "my-").unwrap_err();
        assert!(err.contains("my-feature"), "unexpected error: {err}");
        assert!(err.contains("my-other"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
