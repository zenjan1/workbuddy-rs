use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoadmapItem {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roadmap {
    pub pending: Vec<RoadmapItem>,
    pub completed: Vec<RoadmapItem>,
    pub log: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RoadmapError {
    NotFound(String),
    Io(String),
}

impl fmt::Display for RoadmapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "未找到未完成任务: {}", id),
            Self::Io(msg) => write!(f, "IO 错误: {}", msg),
        }
    }
}

impl std::error::Error for RoadmapError {}

pub fn parse_roadmap(content: &str) -> Result<Roadmap, RoadmapError> {
    let mut pending = Vec::new();
    let mut completed = Vec::new();
    let mut log = Vec::new();

    let mut in_log = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if is_log_header(trimmed) {
            in_log = true;
            continue;
        }
        if in_log {
            log.push(line.to_string());
            continue;
        }

        if let Some(rest) = line.strip_prefix("- [ ] ") {
            pending.push(RoadmapItem { task_id: rest.trim_end().to_string() });
        } else if let Some(rest) = line.strip_prefix("- [x] ") {
            completed.push(RoadmapItem { task_id: rest.trim_end().to_string() });
        }
    }

    Ok(Roadmap { pending, completed, log })
}

fn is_log_header(line: &str) -> bool {
    (line.starts_with('#') && line.contains("日志")) || line.contains("【日志】")
}

pub fn update_roadmap(content: &str, task_id: &str, note: &str) -> Result<String, RoadmapError> {
    let target = task_id.trim();
    if target.is_empty() {
        return Err(RoadmapError::NotFound(task_id.to_string()));
    }

    let mut found = false;
    let mut lines: Vec<String> = Vec::new();
    let mut in_log = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if is_log_header(trimmed) {
            in_log = true;
        }

        if !found && !in_log {
            if let Some(rest) = line.strip_prefix("- [ ] ") {
                if rest.trim_end() == target {
                    lines.push(format!("- [x] {}", rest));
                    found = true;
                    continue;
                }
            }
        }

        lines.push(line.to_string());
    }

    if !found {
        return Err(RoadmapError::NotFound(task_id.to_string()));
    }

    let date = utc_today();
    let log_line = if note.trim().is_empty() {
        format!("{} 完成:{}", date, target)
    } else {
        format!("{} 完成:{} -- {}", date, target, note)
    };

    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }

    if !has_log_header(content) {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("## 日志\n");
    } else if !out.ends_with('\n') {
        out.push('\n');
    }

    out.push_str(&log_line);
    out.push('\n');

    Ok(out)
}

fn has_log_header(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim();
        is_log_header(trimmed)
    })
}

pub fn update_roadmap_file(path: &Path, task_id: &str, note: &str) -> Result<(), RoadmapError> {
    let content = std::fs::read_to_string(path).map_err(|e| RoadmapError::Io(e.to_string()))?;
    let updated = update_roadmap(&content, task_id, note)?;
    std::fs::write(path, updated).map_err(|e| RoadmapError::Io(e.to_string()))
}

fn utc_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs / 86400;
    let (year, month, day) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

pub fn advance_milestone(content: &str, milestone_id: &str) -> Result<String, RoadmapError> {
    let target = milestone_id.trim();
    if target.is_empty() {
        return Err(RoadmapError::NotFound(milestone_id.to_string()));
    }

    let mut found = false;
    let mut milestone_text = String::new();
    let mut lines: Vec<String> = Vec::new();
    let mut in_log = false;
    let mut log_header_index = None;

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if is_log_header(trimmed) {
            in_log = true;
            log_header_index = Some(idx);
        }

        if !found && !in_log {
            if let Some(rest) = line.strip_prefix("- [ ] ") {
                let item_text = rest.trim_end();
                if item_text == target || item_text.starts_with(&format!("{}: ", target)) {
                    lines.push(format!("- [x] {}", rest));
                    found = true;
                    milestone_text = item_text.to_string();
                    continue;
                }
            }
        }

        lines.push(line.to_string());
    }

    if !found {
        return Err(RoadmapError::NotFound(milestone_id.to_string()));
    }

    let date = utc_today();
    let log_line = format!("{} 完成:{}", date, milestone_text);

    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }

    if !has_log_header(content) {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("## 日志\n");
    } else if !out.ends_with('\n') {
        out.push('\n');
    }

    out.push_str(&log_line);
    out.push('\n');

    Ok(out)
}

pub fn advance_milestone_file(path: &Path, milestone_id: &str) -> Result<String, RoadmapError> {
    let content = std::fs::read_to_string(path).map_err(|e| RoadmapError::Io(e.to_string()))?;
    let updated = advance_milestone(&content, milestone_id)?;
    std::fs::write(path, &updated).map_err(|e| RoadmapError::Io(e.to_string()))?;
    Ok(format!("chore(vision): advance {}", milestone_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sample() -> String {
        [
            "# VISION",
            "",
            "## 路线图",
            "- [ ] 任务一",
            "- [x] 任务二",
            "- [ ] 任务三",
            "",
            "## 日志",
            "2024-01-01 完成:旧任务",
            "",
        ]
        .join("\n")
    }

    fn vision_sample() -> String {
        [
            "# workbuddy VISION",
            "",
            "## 路线图(按里程碑,顺序即大方向)",
            "",
            "### M0 地基:循环可靠性与可观测性",
            "- [ ] 每 run 记录 token/字符用量并在 show 显示",
            "- [ ] `status` 子命令:supervisor 是否在跑 + 已完成/失败数 + 最近日志(无人值守命脉)",
            "- [x] `health` 子命令:探测 LLM 端点连通性",
            "",
            "### M1 核心使用功能",
            "- [ ] `stats` 子命令:运行历史统计",
            "",
        ]
        .join("\n")
    }

    #[test]
    fn parse_roadmap_extracts_pending_completed_and_log() {
        let roadmap = parse_roadmap(&sample()).unwrap();
        assert_eq!(roadmap.pending.len(), 2);
        assert_eq!(roadmap.pending[0].task_id, "任务一");
        assert_eq!(roadmap.completed.len(), 1);
        assert_eq!(roadmap.completed[0].task_id, "任务二");
        assert!(roadmap.log.iter().any(|line| line.contains("旧任务")));
    }

    #[test]
    fn update_roadmap_marks_task_completed() {
        let updated = update_roadmap(&sample(), "任务一", "完成说明").unwrap();
        assert!(updated.contains("- [x] 任务一"));
        assert!(!updated.contains("- [ ] 任务一"));
    }

    #[test]
    fn update_roadmap_appends_log_entry() {
        let updated = update_roadmap(&sample(), "任务三", "说明").unwrap();
        let roadmap = parse_roadmap(&updated).unwrap();
        assert!(roadmap.log.iter().any(|line| line.contains("任务三") && line.contains("说明")));
    }

    #[test]
    fn update_roadmap_returns_error_when_task_not_found() {
        let result = update_roadmap(&sample(), "不存在的任务", "说明");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), RoadmapError::NotFound("不存在的任务".to_string()));
    }

    #[test]
    fn updated_roadmap_no_longer_has_task_as_pending() {
        let updated = update_roadmap(&sample(), "任务一", "说明").unwrap();
        let roadmap = parse_roadmap(&updated).unwrap();
        assert!(roadmap.pending.iter().all(|item| item.task_id != "任务一"));
        assert!(roadmap.completed.iter().any(|item| item.task_id == "任务一"));
    }

    #[test]
    fn update_roadmap_file_writes_to_temp_file() {
        let mut path = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("workbuddy-roadmap-test-{}.md", unique));
        fs::write(&path, sample()).unwrap();

        let result = update_roadmap_file(&path, "任务一", "说明");
        assert!(result.is_ok());

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("- [x] 任务一"));
        assert!(written.contains("任务一") && written.contains("说明"));
        let _ = fs::remove_file(&path);
    }
}
