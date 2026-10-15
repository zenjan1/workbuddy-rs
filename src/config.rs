use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use clap::Args;

/// Webhook 通知配置。每个 URL 可独立扩展超时、重试等选项。
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookConfig {
    /// Webhook URL，必须是 http/https。
    pub url: String,
    // 预留扩展字段：例如 retry、timeout_secs、headers 等。
}

/// 按 token 单价计费的配置。单位：美元/token。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TokenPrices {
    /// 输入 token 单价。
    pub input_price: f64,
    /// 输出 token 单价。
    pub output_price: f64,
}

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
    pub price: f64,
    pub max_budget: f64,
    pub cost_per_1k_tokens: f64,
    pub mock: bool,
    pub webhook_enabled: bool,
    pub webhook_url: Option<String>,
    pub webhook_retry: u32,
    pub webhook_timeout_secs: u64,
}

pub static WEBHOOKS: std::sync::RwLock<Vec<WebhookConfig>> = std::sync::RwLock::new(Vec::new());

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    max_parallel: Option<usize>,
    workspace_dir: Option<PathBuf>,
    repo_dir: Option<PathBuf>,
    memory_path: Option<PathBuf>,
    request_timeout_secs: Option<u64>,
    price: Option<f64>,
    max_budget: Option<f64>,
    cost_per_1k_tokens: f64,
    token_prices: TokenPrices,
    webhook_enabled: Option<bool>,
    webhook_url: Option<String>,
    webhook_retry: Option<u32>,
    webhook_timeout_secs: Option<u64>,
    #[serde(default)]
    webhooks: Vec<WebhookConfig>,
}

/// 可通过 `config set` 修改的配置键。键名与 FileConfig/Config 字段保持一致。
const CONFIG_KEYS: &[&str] = &[
    "api_key",
    "base_url",
    "model",
    "max_parallel",
    "workspace_dir",
    "repo_dir",
    "request_timeout_secs",
    "price",
    "max_budget",
    "cost_per_1k_tokens",
    "webhook_url",
    "webhook_retry",
    "webhook_timeout_secs",
];

/// 修改后需要重启进程才会生效的配置键。
const RESTART_REQUIRED_KEYS: &[&str] = &[
    "base_url",
    "model",
    "max_parallel",
    "workspace_dir",
    "repo_dir",
    "request_timeout_secs",
    "webhook_url",
    "webhook_retry",
    "webhook_timeout_secs",
    "cost_per_1k_tokens",
];

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
        let cfg = Config {
            api_key,
            base_url,
            model,
            max_parallel: file.max_parallel.unwrap_or(4).max(1),
            workspace_dir,
            repo_dir,
            request_timeout_secs: file.request_timeout_secs.unwrap_or(300),
            price: file.price.unwrap_or(0.0),
            max_budget: file.max_budget.unwrap_or(0.0),
            cost_per_1k_tokens: file.cost_per_1k_tokens,
            mock: false,
            webhook_enabled: file.webhook_enabled.unwrap_or(false),
            webhook_url: file.webhook_url.filter(|u| !u.trim().is_empty()),
            webhook_retry: file.webhook_retry.unwrap_or(3),
            webhook_timeout_secs: file.webhook_timeout_secs.unwrap_or(5),
        };
        *WEBHOOKS.write().unwrap() = file.webhooks;
        Ok(cfg)
    }

    pub fn summary(&self) -> String {
        format!(
            "model={} | base_url={} | parallel={} | workspace={} | repo_dir={} | auth={} | mock={}",
            self.model,
            self.base_url,
            self.max_parallel,
            self.workspace_dir.display(),
            self.repo_dir.display(),
            match &self.api_key {
                Some(_) => "present",
                None => "absent",
            },
            self.mock
        )
    }

    pub fn validate(&self) -> std::result::Result<(), Vec<String>> {
        let errors = validate_config(self);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// 按每千 token 单价估算成本。
    pub fn estimate_cost(&self, total_tokens: u64) -> f64 {
        self.cost_per_1k_tokens * (total_tokens as f64 / 1000.0)
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

    pub fn set_value(&mut self, key: &str, value: &str) -> Result<()> {
        Self::validate_config_key(key)?;
        let path = Self::config_file_path();
        // 先持久化，成功后再更新内存配置，避免部分更新。
        Self::update_config_file(&path, key, value)?;
        self.apply_value(key, value)?;
        Ok(())
    }

    fn apply_value(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "max_budget" | "price" | "cost_per_1k_tokens" => {
                let n: f64 = value
                    .parse()
                    .with_context(|| format!("{key} 需要是数字"))?;
                if !n.is_finite() {
                    anyhow::bail!("{key} 必须是有限数字");
                }
                match key {
                    "max_budget" if n <= 0.0 => anyhow::bail!("max_budget 必须为非负数"),
                    "price" | "cost_per_1k_tokens" if n < 0.0 => {
                        anyhow::bail!("{key} 不能为负数")
                    }
                    _ => {}
                }
                match key {
                    "max_budget" => self.max_budget = n,
                    "price" => self.price = n,
                    "cost_per_1k_tokens" => self.cost_per_1k_tokens = n,
                    _ => unreachable!(),
                }
            }
            "max_parallel" | "request_timeout_secs" | "webhook_retry" | "webhook_timeout_secs" => {
                let n: i64 = value
                    .parse()
                    .with_context(|| format!("{key} 需要是整数"))?;
                if matches!(
                    key,
                    "max_parallel" | "request_timeout_secs" | "webhook_timeout_secs"
                ) && n <= 0 {
                    anyhow::bail!("{key} 必须大于 0");
                }
                if key == "webhook_retry" && n < 0 {
                    anyhow::bail!("{key} 不能为负数");
                }
                match key {
                    "max_parallel" => self.max_parallel = n as usize,
                    "request_timeout_secs" => self.request_timeout_secs = n as u64,
                    "webhook_retry" => self.webhook_retry = n as u32,
                    "webhook_timeout_secs" => self.webhook_timeout_secs = n as u64,
                    _ => unreachable!(),
                }
            }
            "base_url" | "webhook_url" => {
                validate_url_value(key, value)?;
                if key == "base_url" {
                    self.base_url = value.to_string();
                } else {
                    self.webhook_url = Some(value.to_string());
                }
            }
            "api_key" => self.api_key = Some(value.to_string()),
            "model" => {
                if value.trim().is_empty() {
                    anyhow::bail!("model 不能为空");
                }
                self.model = value.to_string();
            }
            "workspace_dir" => {
                if value.trim().is_empty() {
                    anyhow::bail!("workspace_dir 不能为空");
                }
                self.workspace_dir = PathBuf::from(value);
            }
            "repo_dir" => {
                if value.trim().is_empty() {
                    anyhow::bail!("repo_dir 不能为空");
                }
                self.repo_dir = PathBuf::from(value);
            }
            _ => anyhow::bail!(
                "未知配置键: {key}; 可用键: {}",
                CONFIG_KEYS.join(", ")
            ),
        }
        Ok(())
    }

    pub fn set_config_value(key: &str, value: &str) -> Result<()> {
        Self::validate_config_key(key)?;
        let path = Self::config_file_path();
        Self::update_config_file(&path, key, value)
    }

    /// 判断配置键是否允许通过 `config set` 写入。
    pub fn is_known_config_key(key: &str) -> bool {
        validate_key_syntax(key).is_ok() && normalize_config_key(key).is_ok()
    }

    /// 校验配置键是否允许通过 `config set` 写入。
    pub fn validate_config_key(key: &str) -> Result<()> {
        validate_key_syntax(key)?;
        normalize_config_key(key).map(|_| ())
    }

    /// 返回需要重启生效的全部配置键。
    pub fn restart_required_keys() -> &'static [&'static str] {
        RESTART_REQUIRED_KEYS
    }

    fn update_config_file(path: &Path, key: &str, value: &str) -> Result<()> {
        Self::validate_config_key(key)?;
        set_config_value_file(path, key, value)
    }

    fn config_file_path() -> PathBuf {
        for path in Self::config_candidates() {
            if path.exists() {
                return path;
            }
        }
        Self::config_candidates()
            .into_iter()
            .next()
            .unwrap_or_else(|| PathBuf::from("workbuddy.toml"))
    }

    fn config_candidates() -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Ok(explicit) = env::var("WORKBUDDY_CONFIG") {
            v.push(PathBuf::from(explicit));
        }
        v.push(PathBuf::from("config.toml"));
        v.push(PathBuf::from("workbuddy.toml"));
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            v.push(PathBuf::from(xdg).join("workbuddy/config.toml"));
        }
        if let Ok(home) = env::var("HOME") {
            v.push(PathBuf::from(home).join(".config/workbuddy/config.toml"));
        }
        v
    }
}

/// 使用 toml_edit 更新配置文件，保留注释和未知键。
pub fn set_config_value_file(path: &Path, key: &str, value: &str) -> Result<()> {
    validate_key_syntax(key)?;
    let normalized_key = normalize_config_key(key)?;

    let mut doc = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        text.parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("解析配置文件失败: {}", path.display()))?
    } else {
        toml_edit::DocumentMut::new()
    };

    let parsed = parse_config_set_value(normalized_key, value)?;
    let mut table = doc.as_table_mut();
    if let Some(existing) = table.get_mut(normalized_key) {
        let decor = existing.as_value_mut().map(|v| v.decor().clone());
        *existing = parsed;
        if let Some(decor) = decor {
            if let Some(v) = existing.as_value_mut() {
                *v.decor_mut() = decor;
            }
        }
    } else {
        table.insert(normalized_key, parsed);
    }
    drop(table);

    let contents = doc.to_string();
    atomic_write(path, &contents)
        .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
    Ok(())
}

fn parse_config_set_value(key: &str, raw: &str) -> Result<toml_edit::Item> {
    match key {
        "max_budget" | "price" | "cost_per_1k_tokens" => {
            let n: f64 = raw
                .parse()
                .with_context(|| format!("{key} 需要是数字"))?;
            if !n.is_finite() {
                anyhow::bail!("{key} 必须是有限数字");
            }
            match key {
                "max_budget" if n <= 0.0 => anyhow::bail!("max_budget 必须为非负数"),
                "price" | "cost_per_1k_tokens" if n < 0.0 => {
                    anyhow::bail!("{key} 不能为负数")
                }
                _ => {}
            }
            Ok(toml_edit::value(n))
        }
        "max_parallel" | "request_timeout_secs" | "webhook_retry" | "webhook_timeout_secs" => {
            let n: i64 = raw
                .parse()
                .with_context(|| format!("{key} 需要是整数"))?;
            if matches!(key, "max_parallel" | "request_timeout_secs" | "webhook_timeout_secs") && n <= 0 {
                anyhow::bail!("{key} 必须大于 0");
            }
            if key == "webhook_retry" && n < 0 {
                anyhow::bail!("{key} 不能为负数");
            }
            Ok(toml_edit::value(n))
        }
        "base_url" | "webhook_url" => {
            validate_url_value(key, raw)?;
            Ok(toml_edit::value(raw.to_string()))
        }
        "api_key" | "model" | "workspace_dir" | "repo_dir" => {
            if (key == "model" || key == "workspace_dir" || key == "repo_dir") && raw.trim().is_empty() {
                anyhow::bail!("{key} 不能为空");
            }
            Ok(toml_edit::value(raw.to_string()))
        }
        _ => anyhow::bail!(
            "未知配置键: {key}; 可用键: {}",
            CONFIG_KEYS.join(", ")
        ),
    }
}

fn validate_url_value(key: &str, raw: &str) -> Result<()> {
    let s = raw.trim();
    if s.is_empty() {
        anyhow::bail!("{key} 不能为空");
    }
    let url = reqwest::Url::parse(s)
        .with_context(|| format!("{key} 不是有效 URL: {raw}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("{key} 必须使用 http 或 https 协议");
    }
    if url.host_str().is_none() {
        anyhow::bail!("{key} 缺少主机名");
    }
    Ok(())
}

/// 公开的配置更新入口，支持点号路径别名。
pub fn update_config_value(path: &Path, key: &str, value: &str) -> Result<()> {
    set_config_value_file(path, key, value)
}

/// 校验配置键的基本语法：非空且仅包含字母、数字、下划线、连字符、点号。
fn validate_key_syntax(key: &str) -> Result<()> {
    if key.trim().is_empty() {
        anyhow::bail!("配置键不能为空");
    }
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        anyhow::bail!("配置键包含非法字符: {key}");
    }
    Ok(())
}

/// 将点号路径别名映射到实际的平面配置键。
fn normalize_config_key(key: &str) -> Result<&str> {
    match key {
        "llm.api_base" => Ok("base_url"),
        "llm.base_url" => Ok("base_url"),
        "llm.model" => Ok("model"),
        "llm.timeout" => Ok("request_timeout_secs"),
        "llm.api_key" => Ok("api_key"),
        "llm.max_parallel" => Ok("max_parallel"),
        "webhook.url" => Ok("webhook_url"),
        "webhook.retry" => Ok("webhook_retry"),
        "webhook.timeout_secs" => Ok("webhook_timeout_secs"),
        _ if CONFIG_KEYS.contains(&key) => Ok(key),
        _ => anyhow::bail!(
            "未知配置键: {key}; 可用键: {} 或别名: llm.api_base, llm.model, llm.timeout",
            CONFIG_KEYS.join(", ")
        ),
    }
}

/// 原子写入：写入同目录临时文件后 rename 替换，失败时清理临时文件。
fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("创建配置目录失败: {}", dir.display()))?;

    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp_path = dir.join(format!(
        ".{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        unique
    ));

    let write_result = (|| -> Result<()> {
        let mut tmp = std::fs::File::create(&tmp_path)
            .with_context(|| format!("创建临时文件失败: {}", tmp_path.display()))?;
        tmp.write_all(contents.as_bytes())
            .with_context(|| format!("写入临时文件失败: {}", tmp_path.display()))?;
        tmp.sync_all()
            .with_context(|| format!("同步临时文件失败: {}", tmp_path.display()))?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    std::fs::rename(&tmp_path, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("替换配置文件失败: {}", path.display())
    })?;
    Ok(())
}

/// 拆分并校验点号嵌套键。
fn split_nested_key(key: &str) -> Result<Vec<&str>> {
    if key.trim().is_empty() {
        anyhow::bail!("配置键不能为空");
    }
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        anyhow::bail!("配置键包含非法字符: {key}");
    }
    let parts: Vec<_> = key.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) {
        anyhow::bail!("配置键不能包含空段: {key}");
    }
    Ok(parts)
}

/// 读取配置文件并解析为 toml::Value。空文件返回空表。
pub fn read_config_toml(path: &Path) -> Result<toml::Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    toml::from_str(&text).with_context(|| format!("解析配置文件失败: {}", path.display()))
}

/// 将 CLI 字符串值按启发式规则解析为 toml::Value。
fn parse_nested_value(raw: &str) -> Result<toml::Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        anyhow::bail!("配置值不能为空");
    }
    if raw == "true" || raw == "false" {
        return Ok(toml::Value::Boolean(raw.parse().unwrap()));
    }
    if let Ok(i) = raw.parse::<i64>() {
        return Ok(toml::Value::Integer(i));
    }
    if let Ok(f) = raw.parse::<f64>() {
        if f.is_finite() {
            return Ok(toml::Value::Float(f));
        }
    }
    Ok(toml::Value::String(raw.to_string()))
}

/// 递归写入嵌套键，不存在中间表时自动创建。
fn set_nested_value_recursive(value: &mut toml::Value, parts: &[&str], new_value: toml::Value) -> Result<()> {
    if parts.is_empty() {
        anyhow::bail!("配置键不能为空");
    }
    if parts.len() == 1 {
        let table = value
            .as_table_mut()
            .with_context(|| format!("配置路径段 {} 不是表", parts[0]))?;
        table.insert(parts[0].to_string(), new_value);
    } else {
        let table = value
            .as_table_mut()
            .context("配置根节点不是表")?;
        let child = table
            .entry(parts[0].to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        set_nested_value_recursive(child, &parts[1..], new_value)?;
    }
    Ok(())
}

/// 按点号路径读取配置值。
pub fn get_nested_value<'a>(value: &'a toml::Value, key: &str) -> Result<&'a toml::Value> {
    let parts = split_nested_key(key)?;
    let mut current = value;
    for part in parts {
        let table = current
            .as_table()
            .with_context(|| format!("配置路径段 {} 不是表", part))?;
        current = table
            .get(part)
            .with_context(|| format!("配置键不存在: {key}"))?;
    }
    Ok(current)
}

/// 将 toml::Value 格式化为可读字符串。
pub fn format_config_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => toml::to_string_pretty(other).unwrap_or_else(|_| format!("{other:?}")),
    }
}

/// 从配置文件按点号路径读取值的字符串表示。
pub fn get_config_value(path: &Path, key: &str) -> Result<String> {
    let root = read_config_toml(path)?;
    let value = get_nested_value(&root, key)?;
    Ok(format_config_value(value))
}

/// 将完整 toml::Value 原子写回配置文件。
pub fn write_config_toml_atomic(path: &Path, value: &toml::Value) -> Result<()> {
    let contents = toml::to_string_pretty(value).context("序列化配置失败")?;
    atomic_write(path, &contents)
        .with_context(|| format!("写入配置文件失败: {}", path.display()))
}

/// 读取、修改、原子写回配置文件；支持 llm.model 形式的点号嵌套键。
pub fn set_nested_config_value_file(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut root = read_config_toml(path)?;
    let parsed = parse_nested_value(value)?;
    let parts = split_nested_key(key)?;
    set_nested_value_recursive(&mut root, &parts, parsed)?;
    write_config_toml_atomic(path, &root)
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_initial(path: &Path) {
        fs::write(
            path,
            r#"
api_key = "old-key"
base_url = "https://old.example.com"
model = "old-model"
max_parallel = 4

[llm]
model = "old-llm-model"

[budget]
max_budget = 10.0
"#,
        )
        .unwrap();
    }

    #[test]
    fn set_nested_value_preserves_other_keys() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_initial(&path);

        set_nested_config_value_file(&path, "llm.model", "qwen-plus").unwrap();

        let root = read_config_toml(&path).unwrap();
        assert_eq!(get_nested_value(&root, "llm.model").unwrap().as_str().unwrap(), "qwen-plus");
        assert_eq!(get_nested_value(&root, "api_key").unwrap().as_str().unwrap(), "old-key");
        assert_eq!(get_nested_value(&root, "base_url").unwrap().as_str().unwrap(), "https://old.example.com");
        assert_eq!(get_nested_value(&root, "budget.max_budget").unwrap().as_float().unwrap(), 10.0);

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("qwen-plus"));
        assert!(saved.contains("https://old.example.com"));
    }

    #[test]
    fn set_nested_value_creates_missing_tables() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "api_key = \"k\"\n").unwrap();

        set_nested_config_value_file(&path, "llm.timeout", "120").unwrap();

        let root = read_config_toml(&path).unwrap();
        assert_eq!(get_nested_value(&root, "llm.timeout").unwrap().as_integer().unwrap(), 120);
        assert_eq!(get_nested_value(&root, "api_key").unwrap().as_str().unwrap(), "k");
    }

    #[test]
    fn get_nested_value_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_initial(&path);

        set_nested_config_value_file(&path, "budget.max_budget", "42.5").unwrap();
        assert_eq!(get_config_value(&path, "budget.max_budget").unwrap(), "42.5");
    }
}

pub fn validate_config(config: &Config) -> Vec<String> {
    let mut errors = Vec::new();

    if config.model.trim().is_empty() {
        errors.push("model 不能为空".to_string());
    }
    if config.base_url.trim().is_empty() {
        errors.push("base_url 不能为空".to_string());
    } else {
        match reqwest::Url::parse(&config.base_url) {
            Ok(url) => {
                if !matches!(url.scheme(), "http" | "https") {
                    errors.push("base_url 必须使用 http 或 https 协议".to_string());
                }
                if url.host_str().is_none() {
                    errors.push("base_url 缺少主机名".to_string());
                }
            }
            Err(e) => {
                errors.push(format!("base_url 不是有效 URL: {e}"));
            }
        }
    }
    if config.workspace_dir.as_os_str().is_empty() {
        errors.push("workspace_dir 不能为空".to_string());
    }
    if config.repo_dir.as_os_str().is_empty() {
        errors.push("repo_dir 不能为空".to_string());
    }
    if !config.mock && config.api_key.as_deref().unwrap_or("").trim().is_empty() {
        errors.push("api_key 不能为空(mock 模式除外)".to_string());
    }
    if config.max_parallel == 0 {
        errors.push("max_parallel 必须大于 0".to_string());
    }
    if config.request_timeout_secs == 0 {
        errors.push("request_timeout_secs 必须大于 0".to_string());
    }
    if config.price < 0.0 {
        errors.push("price 不能为负数".to_string());
    }
    if config.cost_per_1k_tokens < 0.0 {
        errors.push("cost_per_1k_tokens 不能为负数".to_string());
    }
    if config.max_budget <= 0.0 {
        errors.push("max_budget 必须为正数".to_string());
    }

    errors
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Toml,
    Json,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigSetError {
    UnknownKey(String),
    InvalidValue { key: String, message: String },
    Parse(String),
    Serialize(String),
}

impl std::fmt::Display for ConfigSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey(key) => write!(f, "未知配置键: {key}"),
            Self::InvalidValue { key, message } => write!(f, "{key} 的值无效: {message}"),
            Self::Parse(message) => write!(f, "解析配置失败: {message}"),
            Self::Serialize(message) => write!(f, "序列化配置失败: {message}"),
        }
    }
}

impl std::error::Error for ConfigSetError {}



/// 纯函数版本的 config set：编辑字符串内容并返回新内容，不执行文件 IO。
///
/// 仅允许修改白名单中的标量字段：
/// - `llm.model`（字符串）
/// - `budget.max_budget`（数字）
/// - `cost.input_per_1k`（数字）
pub fn set_config_value_str(
    input: &str,
    format: ConfigFormat,
    key: &str,
    value: &str,
) -> std::result::Result<String, ConfigSetError> {
    let normalized_key = normalize_whitelist_key(key)?;
    match format {
        ConfigFormat::Toml => {
            let mut doc = input
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e| ConfigSetError::Parse(format!("TOML: {e}")))?;
            let item = parse_whitelist_toml(normalized_key, value)?;
            let mut table = doc.as_table_mut();
            if let Some(existing) = table.get_mut(normalized_key) {
                let decor = existing.as_value_mut().map(|v| v.decor().clone());
                *existing = item;
                if let Some(decor) = decor {
                    if let Some(v) = existing.as_value_mut() {
                        *v.decor_mut() = decor;
                    }
                }
            } else {
                table.insert(normalized_key, item);
            }
            drop(table);
            Ok(doc.to_string())
        }
        ConfigFormat::Json => {
            let mut root: serde_json::Value = serde_json::from_str(input)
                .map_err(|e| ConfigSetError::Parse(format!("JSON: {e}")))?;
            let obj = root
                .as_object_mut()
                .ok_or_else(|| ConfigSetError::Parse("JSON 配置根节点必须是对象".to_string()))?;
            let json_value = parse_whitelist_json(normalized_key, value)?;
            obj.insert(normalized_key.to_string(), json_value);
            serde_json::to_string_pretty(&root)
                .map_err(|e| ConfigSetError::Serialize(format!("{e}")))
        }
    }
}

fn normalize_whitelist_key(key: &str) -> std::result::Result<&str, ConfigSetError> {
    match key {
        "llm.model" => Ok("model"),
        "budget.max_budget" => Ok("max_budget"),
        "cost.input_per_1k" => Ok("cost_per_1k_tokens"),
        _ => Err(ConfigSetError::UnknownKey(key.to_string())),
    }
}

fn parse_whitelist_toml(key: &str, value: &str) -> std::result::Result<toml_edit::Item, ConfigSetError> {
    parse_config_set_value(key, value).map_err(|e| ConfigSetError::InvalidValue {
        key: key.to_string(),
        message: e.to_string(),
    })
}

fn parse_whitelist_json(key: &str, value: &str) -> std::result::Result<serde_json::Value, ConfigSetError> {
    parse_config_set_value(key, value).map_err(|e| ConfigSetError::InvalidValue {
        key: key.to_string(),
        message: e.to_string(),
    })?;

    match key {
        "model" => Ok(serde_json::Value::String(value.to_string())),
        "max_budget" | "cost_per_1k_tokens" => {
            let n: f64 = value
                .parse()
                .map_err(|e: std::num::ParseFloatError| ConfigSetError::InvalidValue {
                    key: key.to_string(),
                    message: e.to_string(),
                })?;
            serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| ConfigSetError::InvalidValue {
                    key: key.to_string(),
                    message: "数值无法序列化为 JSON number".to_string(),
                })
        }
        _ => unreachable!("白名单 key 已预先校验"),
    }
}

#[cfg(test)]
mod set_config_pure_tests {
    use super::*;
    use super::set_config_value_str as set_config_value;

    #[test]
    fn set_config_value_toml_updates_llm_model() {
        let input = r#"
# 注释
model = "qwen-plus"
unknown = "keep"
"#;
        let out = set_config_value(input, ConfigFormat::Toml, "llm.model", "qwen-max").unwrap();
        assert!(out.contains("qwen-max"));
        assert!(!out.contains("qwen-plus"));
        assert!(out.contains("# 注释"));
        assert!(out.contains("unknown"));
    }

    #[test]
    fn set_config_value_json_updates_llm_model_and_keeps_unknown() {
        let input = r#"{"model":"qwen-plus","keep":true}"#;
        let out = set_config_value(input, ConfigFormat::Json, "llm.model", "qwen-max").unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["model"].as_str(), Some("qwen-max"));
        assert_eq!(value["keep"].as_bool(), Some(true));
    }

    #[test]
    fn set_config_value_rejects_unknown_key() {
        let err = set_config_value("", ConfigFormat::Toml, "llm.api_key", "abc").unwrap_err();
        assert!(matches!(err, ConfigSetError::UnknownKey(_)));
    }

    #[test]
    fn set_config_value_rejects_invalid_numeric_value_toml() {
        let err = set_config_value(
            "max_budget = 10.0",
            ConfigFormat::Toml,
            "budget.max_budget",
            "abc",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigSetError::InvalidValue { .. }));
    }

    #[test]
    fn set_config_value_rejects_invalid_numeric_value_json() {
        let err = set_config_value("{}", ConfigFormat::Json, "cost.input_per_1k", "abc").unwrap_err();
        assert!(matches!(err, ConfigSetError::InvalidValue { .. }));
    }

    #[test]
    fn set_config_value_toml_preserves_comments_and_unknown_fields() {
        let input = r#"
# top comment
model = "old"
unknown = "keep"
"#;
        let out = set_config_value(input, ConfigFormat::Toml, "budget.max_budget", "42.5").unwrap();
        assert!(out.contains("max_budget = 42.5"));
        assert!(out.contains("# top comment"));
        assert!(out.contains("keep"));
    }

    #[test]
    fn set_config_value_json_preserves_unknown_fields() {
        let input = r#"{
  "model": "old",
  "unknown": {"nested": [1, 2]},
  "max_budget": 10.0
}"#;
        let out = set_config_value(input, ConfigFormat::Json, "cost.input_per_1k", "0.42").unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["cost_per_1k_tokens"].as_f64(), Some(0.42));
        assert_eq!(value["unknown"]["nested"][1].as_i64(), Some(2));
        assert_eq!(value["max_budget"].as_f64(), Some(10.0));
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

    fn valid_config() -> Config {
        Config {
            api_key: Some("test-key".into()),
            base_url: "https://example.com/v1".into(),
            model: "qwen-plus".into(),
            max_parallel: 4,
            workspace_dir: PathBuf::from("/tmp/workbuddy"),
            repo_dir: PathBuf::from("/tmp/repo"),
            request_timeout_secs: 120,
            price: 0.0,
            max_budget: 100.0,
            cost_per_1k_tokens: 0.0,
            mock: false,
            webhook_enabled: false,
            webhook_url: None,
            webhook_retry: 3,
            webhook_timeout_secs: 5,
        }
    }

    #[test]
    fn parse_file_config_reads_cost_per_1k_tokens() {
        let c: FileConfig = toml::from_str("cost_per_1k_tokens = 0.42")
            .unwrap();
        assert_eq!(c.cost_per_1k_tokens, 0.42);
    }

    #[test]
    fn file_config_default_cost_is_zero() {
        assert_eq!(FileConfig::default().cost_per_1k_tokens, 0.0);
    }

    #[test]
    fn estimate_cost_for_zero_tokens_is_zero() {
        let mut config = valid_config();
        config.cost_per_1k_tokens = 0.42;
        assert_eq!(config.estimate_cost(0), 0.0);
    }

    #[test]
    fn estimate_cost_for_one_token_uses_one_thousandth_price() {
        let mut config = valid_config();
        config.cost_per_1k_tokens = 0.42;
        assert!((config.estimate_cost(1) - 0.00042).abs() < 1e-12);
    }

    #[test]
    fn estimate_cost_for_1000_tokens_equals_price_per_1k() {
        let mut config = valid_config();
        config.cost_per_1k_tokens = 0.42;
        assert!((config.estimate_cost(1000) - 0.42).abs() < 1e-12);
    }

    #[test]
    fn estimate_cost_for_fractional_thousand_tokens_is_linear() {
        let mut config = valid_config();
        config.cost_per_1k_tokens = 0.42;
        assert!((config.estimate_cost(2500) - 1.05).abs() < 1e-12);
    }

    #[test]
    fn validate_config_accepts_valid() {
        let config = valid_config();
        assert!(validate_config(&config).is_empty(), "合法配置不应有错误: {:?}", validate_config(&config));
    }

    #[test]
    fn validate_config_rejects_invalid_url() {
        let mut config = valid_config();
        config.base_url = "not-a-url".into();
        let errors = validate_config(&config);
        assert!(errors.iter().any(|e| e.contains("base_url")), "应报 base_url 错误");
    }

    #[test]
    fn validate_config_rejects_invalid_scheme() {
        let mut config = valid_config();
        config.base_url = "ftp://example.com".into();
        let errors = validate_config(&config);
        assert!(errors.iter().any(|e| e.contains("http 或 https")), "应报 scheme 错误");
    }

    #[test]
    fn validate_config_rejects_negative_price() {
        let mut config = valid_config();
        config.price = -1.0;
        let errors = validate_config(&config);
        assert!(errors.iter().any(|e| e.contains("price")), "应报 price 错误");
    }

    #[test]
    fn validate_config_rejects_zero_or_negative_max_budget() {
        let mut config = valid_config();
        config.max_budget = 0.0;
        let errors = validate_config(&config);
        assert!(errors.iter().any(|e| e.contains("max_budget")), "应为 0 max_budget 报错");
        config.max_budget = -10.0;
        let errors = validate_config(&config);
        assert!(errors.iter().any(|e| e.contains("max_budget")), "应为负 max_budget 报错");
    }

    #[test]
    fn validate_config_rejects_missing_required_fields() {
        let mut config = valid_config();
        config.model = String::new();
        config.api_key = None;
        config.workspace_dir = PathBuf::new();
        let errors = validate_config(&config);
        assert!(errors.iter().any(|e| e.contains("model")));
        assert!(errors.iter().any(|e| e.contains("api_key")));
        assert!(errors.iter().any(|e| e.contains("workspace_dir")));
    }

    fn temp_config_path(test_name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("workbuddy-test-{}-{}.toml", test_name, unique));
        path
    }

    #[test]
    fn update_config_file_writes_new_key() {
        let path = temp_config_path("new_key");
        let initial = "model = \"qwen-plus\"\n";
        std::fs::write(&path, initial).unwrap();

        Config::update_config_file(&path, "max_budget", "123.45").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc["max_budget"].as_float(), Some(123.45));
        assert_eq!(doc["model"].as_str(), Some("qwen-plus"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_updates_existing_key() {
        let path = temp_config_path("update_key");
        let initial = "max_budget = 100.0\nmodel = \"qwen-plus\"\n";
        std::fs::write(&path, initial).unwrap();

        Config::update_config_file(&path, "max_budget", "200.5").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc["max_budget"].as_float(), Some(200.5));
        assert_eq!(doc["model"].as_str(), Some("qwen-plus"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_round_trip_read_consistent() {
        let path = temp_config_path("round_trip");
        std::fs::write(&path, "base_url = \"https://example.com/v1\"\n").unwrap();

        Config::update_config_file(&path, "max_budget", "42.0").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let loaded: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(loaded.max_budget, Some(42.0));
        assert_eq!(loaded.base_url.as_deref(), Some("https://example.com/v1"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn config_apply_value_updates_memory() {
        let mut config = valid_config();

        config.apply_value("model", "qwen-max").unwrap();
        assert_eq!(config.model, "qwen-max");

        config.apply_value("max_budget", "123.45").unwrap();
        assert!((config.max_budget - 123.45).abs() < 1e-12);

        config.apply_value("max_parallel", "8").unwrap();
        assert_eq!(config.max_parallel, 8);

        config.apply_value("webhook_retry", "6").unwrap();
        assert_eq!(config.webhook_retry, 6);

        config.apply_value("webhook_url", "https://example.com/hook").unwrap();
        assert_eq!(config.webhook_url.as_deref(), Some("https://example.com/hook"));
    }

    #[test]
    fn config_set_config_value_rejects_unknown_key() {
        let path = temp_config_path("unknown_key");
        let err = Config::update_config_file(&path, "unknown_key", "value").unwrap_err();
        assert!(err.to_string().contains("未知配置键"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn config_set_config_value_rejects_invalid_type() {
        let path = temp_config_path("invalid_type");
        let err = Config::update_config_file(&path, "max_budget", "not-a-number").unwrap_err();
        assert!(err.to_string().contains("需要是数字"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_preserves_comments_and_unknown_keys() {
        let path = temp_config_path("preserve_format");
        let initial = r#"
# 顶层注释
model = "qwen-plus"  # 行内注释
max_parallel = 2

[unknown_section]
foo = "bar"
"#;
        std::fs::write(&path, initial).unwrap();

        Config::update_config_file(&path, "max_budget", "123.45").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 顶层注释"));
        assert!(text.contains("# 行内注释"));
        assert!(text.contains("[unknown_section]"));
        assert!(text.contains("foo = \"bar\""));
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc["max_budget"].as_float(), Some(123.45));
        assert_eq!(doc["model"].as_str(), Some("qwen-plus"));
        assert_eq!(doc["unknown_section"]["foo"].as_str(), Some("bar"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_rejects_invalid_value_and_preserves_file() {
        let path = temp_config_path("invalid_value_preserve");
        std::fs::write(&path, "model = \"qwen-plus\"\n").unwrap();

        let err = Config::update_config_file(&path, "max_budget", "not-a-number").unwrap_err();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "model = \"qwen-plus\"\n");
        assert!(err.to_string().contains("max_budget"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_rejects_unknown_key() {
        let path = temp_config_path("unknown_key");
        std::fs::write(&path, "model = \"qwen-plus\"\n").unwrap();

        let err = Config::update_config_file(&path, "not_a_real_key", "value").unwrap_err();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "model = \"qwen-plus\"\n");
        assert!(err.to_string().contains("未知配置键"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_round_trip_all_scalar_keys() {
        let path = temp_config_path("all_scalar_roundtrip");
        std::fs::write(&path, "").unwrap();

        Config::update_config_file(&path, "max_budget", "100.5").unwrap();
        Config::update_config_file(&path, "price", "1.25").unwrap();
        Config::update_config_file(&path, "cost_per_1k_tokens", "0.42").unwrap();
        Config::update_config_file(&path, "max_parallel", "8").unwrap();
        Config::update_config_file(&path, "request_timeout_secs", "60").unwrap();
        Config::update_config_file(&path, "webhook_retry", "5").unwrap();
        Config::update_config_file(&path, "webhook_timeout_secs", "10").unwrap();
        Config::update_config_file(&path, "model", "qwen-max").unwrap();
        Config::update_config_file(&path, "base_url", "https://example.com/v1").unwrap();
        Config::update_config_file(&path, "api_key", "secret").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc["max_budget"].as_float(), Some(100.5));
        assert_eq!(doc["price"].as_float(), Some(1.25));
        assert_eq!(doc["cost_per_1k_tokens"].as_float(), Some(0.42));
        assert_eq!(doc["max_parallel"].as_integer(), Some(8));
        assert_eq!(doc["request_timeout_secs"].as_integer(), Some(60));
        assert_eq!(doc["webhook_retry"].as_integer(), Some(5));
        assert_eq!(doc["webhook_timeout_secs"].as_integer(), Some(10));
        assert_eq!(doc["model"].as_str(), Some("qwen-max"));
        assert_eq!(doc["base_url"].as_str(), Some("https://example.com/v1"));
        assert_eq!(doc["api_key"].as_str(), Some("secret"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_does_not_clobber_other_keys() {
        let path = temp_config_path("no_clobber");
        let initial = r#"
            api_key = "existing-key"
            model = "qwen-max"
            max_parallel = 2
            some_unknown = "keepme"
        "#;
        std::fs::write(&path, initial).unwrap();

        Config::update_config_file(&path, "max_budget", "999.0").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc["max_budget"].as_float(), Some(999.0));
        assert_eq!(doc["api_key"].as_str(), Some("existing-key"));
        assert_eq!(doc["model"].as_str(), Some("qwen-max"));
        assert_eq!(doc["max_parallel"].as_integer(), Some(2));
        assert_eq!(doc["some_unknown"].as_str(), Some("keepme"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn legal_config_keys_contains_expected_keys() {
        assert!(Config::is_known_config_key("model"));
        assert!(Config::is_known_config_key("cost_per_1k_tokens"));
        assert!(Config::is_known_config_key("request_timeout_secs"));
        assert!(!Config::is_known_config_key("unknown_key"));
    }

    #[test]
    fn update_config_file_writes_known_keys() {
        let path = temp_config_path("known_keys");
        let initial = "model = \"old-model\"\n";
        std::fs::write(&path, initial).unwrap();

        Config::update_config_file(&path, "model", "qwen-plus").unwrap();
        Config::update_config_file(&path, "cost_per_1k_tokens", "0.7").unwrap();
        Config::update_config_file(&path, "request_timeout_secs", "45").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc["model"].as_str(), Some("qwen-plus"));
        assert_eq!(doc["cost_per_1k_tokens"].as_float(), Some(0.7));
        assert_eq!(doc["request_timeout_secs"].as_integer(), Some(45));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_rejects_invalid_type_and_preserves_file() {
        let path = temp_config_path("invalid_type");
        let initial = "model = \"qwen-plus\"\n";
        std::fs::write(&path, initial).unwrap();

        let err = Config::update_config_file(&path, "request_timeout_secs", "not-a-number").unwrap_err();
        assert!(err.to_string().contains("需要是整数"), "错误信息应提示类型错误: {err}");

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, initial, "类型错误不应修改配置文件");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn update_config_file_rejects_unknown_key_and_preserves_file() {
        let path = temp_config_path("unknown_key");
        let initial = "model = \"qwen-plus\"\n";
        std::fs::write(&path, initial).unwrap();

        let err = Config::update_config_file(&path, "unknown_key", "value").unwrap_err();
        assert!(err.to_string().contains("未知配置键"));

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, initial);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn set_config_value_preserves_comments_and_unknown_keys() {
        let path = temp_config_path("set_comments");
        let initial = "# top comment\nmodel = \"old\" # inline comment\nunknown_key = \"keepme\"\n";
        std::fs::write(&path, initial).unwrap();

        set_config_value(&path, "max_budget", "12.5").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# top comment"), "顶部注释应保留: {text}");
        assert!(text.contains("# inline comment"), "行尾注释应保留: {text}");
        assert!(text.contains("unknown_key"), "未知键应保留: {text}");

        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["max_budget"].as_float(), Some(12.5));
        assert_eq!(parsed["model"].as_str(), Some("old"));
        assert_eq!(parsed["unknown_key"].as_str(), Some("keepme"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn set_config_value_accepts_non_negative_numbers() {
        let path = temp_config_path("numbers_ok");
        std::fs::write(&path, "").unwrap();

        set_config_value(&path, "max_budget", "100.5").unwrap();
        set_config_value(&path, "cost_per_1k_tokens", "0.001").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["max_budget"].as_float(), Some(100.5));
        assert_eq!(parsed["cost_per_1k_tokens"].as_float(), Some(0.001));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn set_config_value_rejects_invalid_numbers() {
        let path = temp_config_path("numbers_bad");
        std::fs::write(&path, "model = \"keep\"\n").unwrap();

        let err = set_config_value(&path, "max_budget", "-1.0").unwrap_err();
        assert!(err.to_string().contains("非负"), "负 max_budget 应报错: {err}");
        let err = set_config_value(&path, "cost_per_1k_tokens", "abc").unwrap_err();
        assert!(err.to_string().contains("cost_per_1k_tokens"), "非数字 cost 应报错: {err}");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("model = \"keep\""), "失败写入不应破坏已有配置");
        assert!(!text.contains("max_budget"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn set_config_value_rejects_empty_model() {
        let path = temp_config_path("empty_model");
        std::fs::write(&path, "").unwrap();

        let err = set_config_value(&path, "model", "   ").unwrap_err();
        assert!(err.to_string().contains("model 不能为空"), "空 model 应报错: {err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn set_config_value_updates_model_string() {
        let path = temp_config_path("model_ok");
        std::fs::write(&path, "model = \"old\"\n").unwrap();

        set_config_value(&path, "model", "new-model").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["model"].as_str(), Some("new-model"));
        std::fs::remove_file(&path).ok();
    }
}

#[derive(Debug, Clone, Args)]
pub struct CmdShow {
    /// Run ID
    pub id: String,
    /// 输出完整 JSON
    #[arg(long)]
    pub json: bool,
}

impl Default for CmdShow {
    fn default() -> Self {
        Self {
            id: String::new(),
            json: false,
        }
    }
}

/// `config set` 文件更新入口：写入配置并返回新内容。
pub fn set_config_value(
    path: &Path,
    key: &str,
    value: &str,
) -> std::result::Result<String, ConfigSetError> {
    set_config_value_file(path, key, value)
        .map_err(|e| ConfigSetError::Parse(e.to_string()))?;
    std::fs::read_to_string(path)
        .map_err(|e| ConfigSetError::Parse(format!("读取配置文件失败: {}", e)))
}