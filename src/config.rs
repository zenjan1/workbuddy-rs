use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// 运行时配置。优先级:命令行 > 环境变量 > 配置文件 > 默认值。
#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    pub max_parallel: usize,
    pub workspace_dir: PathBuf,
    /// dev 模式的操作仓库(默认当前目录)
    pub repo_dir: PathBuf,
    pub request_timeout_secs: u64,
    pub mock: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    max_parallel: Option<usize>,
    workspace_dir: Option<PathBuf>,
    repo_dir: Option<PathBuf>,
    request_timeout_secs: Option<u64>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let file = Self::load_file()?;
        let home = env::var_os("HOME").context("环境变量 HOME 未设置")?;
        let home = Path::new(&home);

        let api_key = env::var("DASHSCOPE_API_KEY")
            .ok()
            .or_else(|| env::var("OPENAI_API_KEY").ok())
            .or(file.api_key)
            .filter(|k| !k.is_empty());

        let base_url = env::var("DASHSCOPE_BASE_URL")
            .ok()
            .or(file.base_url)
            .unwrap_or_else(|| "https://dashscope.aliyuncs.com/compatible-mode/v1".into());

        let model = env::var("DASHSCOPE_MODEL")
            .ok()
            .or(file.model)
            .unwrap_or_else(|| "qwen-plus".into());

        let workspace_dir = file
            .workspace_dir
            .unwrap_or_else(|| home.join(".local/share/workbuddy"));
        let repo_dir = file
            .repo_dir
            .or_else(|| env::var("WORKBUDDY_REPO").ok().map(PathBuf::from))
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        Ok(Config {
            api_key,
            base_url,
            model,
            max_parallel: file.max_parallel.unwrap_or(4).max(1),
            workspace_dir,
            repo_dir,
            request_timeout_secs: file.request_timeout_secs.unwrap_or(120),
            mock: false,
        })
    }

    pub fn summary(&self) -> String {
        format!(
            "model={} | base_url={} | parallel={} | workspace={} | auth={} | mock={}",
            self.model,
            self.base_url,
            self.max_parallel,
            self.workspace_dir.display(),
            match &self.api_key {
                Some(_) => "present",
                None => "absent",
            },
            self.mock
        )
    }

    fn load_file() -> Result<FileConfig> {
        for path in Self::config_candidates() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                return toml::from_str(&text)
                    .with_context(|| format!("解析配置文件失败: {}", path.display()));
            }
        }
        Ok(FileConfig::default())
    }

    fn config_candidates() -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from("workbuddy.toml")];
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            v.push(PathBuf::from(xdg).join("workbuddy/config.toml"));
        }
        if let Ok(home) = env::var("HOME") {
            v.push(PathBuf::from(home).join(".config/workbuddy/config.toml"));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_config() {
        let c: FileConfig = toml::from_str(
            r#"
            model = "qwen-max"
            max_parallel = 2
            base_url = "https://example.com/v1"
            "#,
        )
        .unwrap();
        assert_eq!(c.model.as_deref(), Some("qwen-max"));
        assert_eq!(c.max_parallel, Some(2));
        assert_eq!(c.base_url.as_deref(), Some("https://example.com/v1"));
    }

    #[test]
    fn parse_empty_file_config() {
        let c: FileConfig = toml::from_str("").unwrap();
        assert!(c.model.is_none());
        assert!(c.max_parallel.is_none());
    }
}
