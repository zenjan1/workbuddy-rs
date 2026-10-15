use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn format_utc_compact(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, min, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, mo, d, h, min, s)
}

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

/// 生成失败报告文件
/// 参数:
/// - validation_output: 最后一次验证输出
/// - fix_attempts: 修复尝试记录
/// - title: 任务标题
pub fn write_failure_report(validation_output: &str, fix_attempts: &str, title: &str) -> Result<(), Box<dyn std::error::Error>> {
    // 生成ISO8601时间戳(UTC,紧凑格式)
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let timestamp = format_utc_compact(now);

    // 安全处理标题: 截取前12字并替换非法字符
    let mut sanitized_title = title.chars().take(12).collect::<String>();
    sanitized_title = sanitized_title
        .replace("/", "_")
        .replace("\\", "_")
        .replace(":", "_")
        .replace("*", "_")
        .replace("?", "_")
        .replace("\"", "_")
        .replace("<", "_")
        .replace(">", "_")
        .replace("|", "_")
        .trim()
        .to_string();

    // 创建目录结构
    let dir_path = ".workbuddy/failures/";
    fs::create_dir_all(dir_path)?;

    // 构建文件路径
    let file_path = format!("{}{}-{}.md", dir_path, timestamp, sanitized_title);

    // 格式化markdown内容
    let content = format!(
        "# 失败报告: {}\n\n## 最后一次验证输出\n```text\n{}\n```
\n## 修复尝试记录\n{}\n",
        sanitized_title, validation_output, fix_attempts
    );

    // 写入文件
    fs::write(&file_path, content)?;

    Ok(())
}

pub fn slugify(title: &str) -> String {
    let mut result = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                result.push(lc);
            }
        } else {
            result.push('-');
        }
    }

    let mut collapsed = String::new();
    let mut prev_dash = false;
    for c in result.chars() {
        if c == '-' {
            if !prev_dash {
                collapsed.push(c);
                prev_dash = true;
            }
        } else {
            collapsed.push(c);
            prev_dash = false;
        }
    }

    let slug = collapsed.trim_matches('-').to_string();
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

pub fn unique_slug(base: &str, existing_slugs: &[String]) -> String {
    if !existing_slugs.iter().any(|s| s.as_str() == base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{}-{}", base, n);
        if !existing_slugs.iter().any(|s| s.as_str() == candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

pub fn parse_task_id(input: &str) -> String {
    if !input.is_empty() && input.chars().all(|c| c.is_ascii_digit()) {
        input.to_string()
    } else {
        slugify(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_keeps_chinese_chars() {
        assert_eq!(slugify(" 你好世界 "), "你好世界");
    }

    #[test]
    fn slugify_collapses_whitespace_and_punctuation() {
        assert_eq!(slugify("Hello,   World!"), "hello-world");
    }

    #[test]
    fn slugify_falls_back_to_task_for_empty_result() {
        assert_eq!(slugify("!!!"), "task");
    }

    #[test]
    fn unique_slug_appends_suffix_on_duplicate() {
        let existing = vec!["foo".to_string(), "foo-2".to_string()];
        assert_eq!(unique_slug("foo", &existing), "foo-3");
    }

    #[test]
    fn parse_task_id_accepts_numeric_id() {
        assert_eq!(parse_task_id("123"), "123");
    }

    #[test]
    fn parse_task_id_slugifies_non_numeric_input() {
        assert_eq!(parse_task_id("My Task"), "my-task");
    }
}