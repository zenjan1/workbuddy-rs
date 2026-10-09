use chrono::Local;
use std::fs;

/// 生成失败报告文件
/// 参数:
/// - validation_output: 最后一次验证输出
/// - fix_attempts: 修复尝试记录
/// - title: 任务标题
pub fn write_failure_report(validation_output: &str, fix_attempts: &str, title: &str) -> Result<(), Box<dyn std::error::Error>> {
    // 生成ISO8601时间戳
    let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();

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