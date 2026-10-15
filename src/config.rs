use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use clap::Args;

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
    pub webhook_url: Option<String>,
    pub webhook_retry: u32,
    pub webhook_timeout_secs: u64,
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
    price: Option<f64>,
    max_budget: Option<f64>,
    cost_per_1k_tokens: f64,
    webhook_url: Option<String>,
    webhook_retry: Option<u32>,
    webhook_timeout_secs: Option<u64>,
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
    "webhook_url",
    "webhook_retry",
    "webhook_timeout_secs",
    "temperature",
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
    "temperature",
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

        Ok(Config {
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
            webhook_url: file.webhook_url.filter(|u| !u.trim().is_empty()),
            webhook_retry: file.webhook_retry.unwrap_or(3),
            webhook_timeout_secs: file.webhook_timeout_secs.unwrap_or(5),
        })
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

    fn load_file() -> Result<FileConfig> {
        for path in Self::config_candidates() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                return toml::from_str(&text)
                    .with_context(|| format!("解析配置文件失败: {}", path.display()));
            }
        }
        Ok(FileConfig::default())
    }

    pub fn set_config_value(key: &str, value: &str) -> Result<()> {
        Self::validate_config_key(key)?;
        let path = Self::config_file_path();
        Self::update_config_file(&path, key, value)
    }

    /// 判断配置键是否允许通过 `config set` 写入。
    pub fn is_known_config_key(key: &str) -> bool {
        CONFIG_KEYS.contains(&key)
    }

    /// 校验配置键是否允许通过 `config set` 写入。
    pub fn validate_config_key(key: &str) -> Result<()> {
        if Self::is_known_config_key(key) {
            Ok(())
        } else {
            anyhow::bail!("未知配置键: {key}; 可用键: {}", CONFIG_KEYS.join(", "))
        }
    }

    /// 返回需要重启生效的全部配置键。
    pub fn restart_required_keys() -> &'static [&'static str] {
        RESTART_REQUIRED_KEYS
    }

    fn update_config_file(path: &Path, key: &str, value: &str) -> Result<()> {
        Self::validate_config_key(key)?;
        let mut doc: toml::Value = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
            toml::from_str(&text)
                .with_context(|| format!("解析配置文件失败: {}", path.display()))?
        } else {
            toml::Value::Table(Default::default())
        };

        let table = doc.as_table_mut().context("配置文件根节点必须是表")?;
        let parsed = Self::parse_value_for_key(key, value)?;
        table.insert(key.to_string(), parsed);

        let text = toml::to_string_pretty(&doc)?;
        std::fs::write(path, text)
            .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }

    fn parse_value_for_key(key: &str, raw: &str) -> Result<toml::Value> {
        let value = match key {
            "max_budget" | "price" | "temperature" => {
                let n: f64 = raw.parse().with_context(|| format!("{key} 需要是数字"))?;
                toml::Value::Float(n)
            }
            "max_parallel" | "request_timeout_secs" | "webhook_retry" | "webhook_timeout_secs" => {
                let n: i64 = raw.parse().with_context(|| format!("{key} 需要是整数"))?;
                toml::Value::Integer(n)
            }
            "api_key" | "base_url" | "model" | "workspace_dir" | "repo_dir" | "webhook_url" => {
                toml::Value::String(raw.to_string())
            }
            _ => anyhow::bail!("未知配置键: {key}"),
        };
        Ok(value)
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

/// 使用 toml_edit 更新配置文件，保留注释和未知键。
pub fn set_config_value(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut doc = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        text.parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("解析配置文件失败: {}", path.display()))?
    } else {
        toml_edit::DocumentMut::new()
    };

    let parsed = parse_config_set_value(key, value)?;
    doc.as_table_mut().insert(key, parsed);

    std::fs::write(path, doc.to_string())
        .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
    Ok(())
}

fn parse_config_set_value(key: &str, raw: &str) -> Result<toml_edit::Item> {
    match key {
        "max_budget" | "cost_per_1k_tokens" => {
            let n: f64 = raw
                .parse()
                .with_context(|| format!("{key} 需要是非负数字"))?;
            if !n.is_finite() || n < 0.0 {
                anyhow::bail!("{key} 必须是非负数字");
            }
            Ok(toml_edit::value(n))
        }
        "model" => {
            if raw.trim().is_empty() {
                anyhow::bail!("model 不能为空");
            }
            Ok(toml_edit::value(raw.to_string()))
        }
        _ => anyhow::bail!(
            "未知配置键: {key}; 可用键: max_budget, cost_per_1k_tokens, model"
        ),
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

        let reloaded: FileConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reloaded.max_budget, Some(42.0));
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
        assert!(Config::is_known_config_key("temperature"));
        assert!(Config::is_known_config_key("request_timeout_secs"));
        assert!(!Config::is_known_config_key("unknown_key"));
    }

    #[test]
    fn update_config_file_writes_known_keys() {
        let path = temp_config_path("known_keys");
        let initial = "model = \"old-model\"\n";
        std::fs::write(&path, initial).unwrap();

        Config::update_config_file(&path, "model", "qwen-plus").unwrap();
        Config::update_config_file(&path, "temperature", "0.7").unwrap();
        Config::update_config_file(&path, "request_timeout_secs", "45").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc["model"].as_str(), Some("qwen-plus"));
        assert_eq!(doc["temperature"].as_float(), Some(0.7));
        assert_eq!(doc["request_timeout_secs"].as_integer(), Some(45));
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