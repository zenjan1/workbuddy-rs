use std::collections::HashMap;
use std::env;
use toml::Table;
use toml::Value;

/// 解析所有以指定前缀开头的环境变量，转换为嵌套的 TOML Table
///
/// # 参数
/// - `prefix`: 环境变量前缀，例如 "WB_"
///
/// # 返回
/// 包含解析后配置的 Table
///
/// # 示例
/// ```
/// // 环境变量: WB_SERVER__PORT=8080, WB_SERVER__HOST=localhost
/// // 结果: { server: { port: 8080, host: "localhost" } }
/// ```
pub fn parse_env_vars(prefix: &str) -> Table {
    let mut result = Table::new();
    
    for (key, value) in env::vars() {
        if let Some(stripped) = key.strip_prefix(prefix) {
            if stripped.is_empty() {
                continue;
            }
            
            // 将双下划线转换为点号路径
            let path = stripped.to_lowercase().replace("__", ".");
            
            // 解析值类型
            let parsed_value = parse_value(&value);
            
            // 设置嵌套值
            set_nested_value(&mut result, &path, parsed_value);
        }
    }
    
    result
}

/// 解析字符串值为 TOML Value，支持基本类型自动推断
fn parse_value(s: &str) -> Value {
    let trimmed = s.trim();
    
    // 尝试解析为布尔值
    if let Ok(b) = trimmed.parse::<bool>() {
        return Value::Boolean(b);
    }
    
    // 尝试解析为整数
    if let Ok(i) = trimmed.parse::<i64>() {
        return Value::Integer(i);
    }
    
    // 尝试解析为浮点数
    if let Ok(f) = trimmed.parse::<f64>() {
        return Value::Float(toml::value::Float::from(f));
    }
    
    // 尝试解析为数组 (逗号分隔)
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let inner = &trimmed[1..trimmed.len() - 1];
        if !inner.trim().is_empty() {
            let items: Vec<Value> = inner
                .split(',')
                .map(|item| parse_value(item.trim()))
                .collect();
            return Value::Array(items);
        }
        return Value::Array(vec![]);
    }
    
    // 默认作为字符串
    Value::String(trimmed.to_string())
}

/// 在 Table 中设置嵌套值，路径用点号分隔
/// 例如: path = "server.port", value = 8080
fn set_nested_value(table: &mut Table, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = table;
    
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // 最后一层，设置值
            current.insert(part.to_string(), value);
        } else {
            // 中间层，确保是 Table
            current = current
                .entry(part.to_string())
                .or_insert_with(|| Value::Table(Table::new()))
                .as_table_mut()
                .expect("Expected table for nested path");
        }
    }
}

/// 深度合并两个 TOML Value
///
/// # 参数
/// - `base`: 基础配置，将被原地修改
/// - `overlay`: 覆盖层配置（环境特定配置）
///
/// # 合并规则
/// - Table: 递归合并，同名键递归处理，新键直接插入
/// - Array: 直接替换（不合并元素）
/// - 标量值: 直接覆盖
/// - 其他: overlay 覆盖 base
pub fn deep_merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Table(base_map), Value::Table(overlay_map)) => {
            for (k, v) in overlay_map {
                match base_map.entry(k.clone()) {
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        deep_merge(e.get_mut(), v);
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(v.clone());
                    }
                }
            }
        }
        (base_val, overlay_val) => {
            // 数组、标量值等直接替换
            *base_val = overlay_val.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    
    fn set_test_env(vars: &[(&str, &str)]) {
        for (k, v) in vars {
            env::set_var(k, v);
        }
    }
    
    fn clear_test_env(vars: &[&str]) {
        for k in vars {
            env::remove_var(k);
        }
    }
    
    #[test]
    fn test_parse_simple_values() {
        set_test_env(&[
            ("WB_SERVER__PORT", "8080"),
            ("WB_SERVER__HOST", "localhost"),
            ("WB_DEBUG", "true"),
            ("WB_RATE", "3.14"),
        ]);
        
        let result = parse_env_vars("WB_");
        
        assert_eq!(result.get("server").and_then(|v| v.get("port")), Some(&Value::Integer(8080)));
        assert_eq!(result.get("server").and_then(|v| v.get("host")), Some(&Value::String("localhost".to_string())));
        assert_eq!(result.get("debug"), Some(&Value::Boolean(true)));
        assert_eq!(result.get("rate"), Some(&Value::Float(toml::value::Float::from(3.14))));
        
        clear_test_env(&["WB_SERVER__PORT", "WB_SERVER__HOST", "WB_DEBUG", "WB_RATE"]);
    }
    
    #[test]
    fn test_parse_nested_structure() {
        set_test_env(&[
            ("WB_DATABASE__HOST", "db.example.com"),
            ("WB_DATABASE__PORT", "5432"),
            ("WB_DATABASE__USERNAME", "admin"),
            ("WB_DATABASE__PASSWORD", "secret"),
            ("WB_DATABASE__POOL__MAX", "10"),
            ("WB_DATABASE__POOL__MIN", "2"),
        ]);
        
        let result = parse_env_vars("WB_");
        
        let db = result.get("database").and_then(|v| v.as_table()).expect("database table");
        assert_eq!(db.get("host"), Some(&Value::String("db.example.com".to_string())));
        assert_eq!(db.get("port"), Some(&Value::Integer(5432)));
        assert_eq!(db.get("username"), Some(&Value::String("admin".to_string())));
        assert_eq!(db.get("password"), Some(&Value::String("secret".to_string())));
        
        let pool = db.get("pool").and_then(|v| v.as_table()).expect("pool table");
        assert_eq!(pool.get("max"), Some(&Value::Integer(10)));
        assert_eq!(pool.get("min"), Some(&Value::Integer(2)));
        
        clear_test_env(&[
            "WB_DATABASE__HOST", "WB_DATABASE__PORT", "WB_DATABASE__USERNAME",
            "WB_DATABASE__PASSWORD", "WB_DATABASE__POOL__MAX", "WB_DATABASE__POOL__MIN",
        ]);
    }
    
    #[test]
    fn test_parse_array_values() {
        set_test_env(&[
            ("WB_ALLOWED_HOSTS", "[localhost, 127.0.0.1, example.com]"),
            ("WB_PORTS", "[8080, 8081, 8082]"),
            ("WB_EMPTY_ARRAY", "[]"),
        ]);
        
        let result = parse_env_vars("WB_");
        
        let hosts = result.get("allowed_hosts").and_then(|v| v.as_array()).expect("hosts array");
        assert_eq!(hosts.len(), 3);
        assert_eq!(hosts[0], Value::String("localhost".to_string()));
        assert_eq!(hosts[1], Value::String("127.0.0.1".to_string()));
        assert_eq!(hosts[2], Value::String("example.com".to_string()));
        
        let ports = result.get("ports").and_then(|v| v.as_array()).expect("ports array");
        assert_eq!(ports.len(), 3);
        assert_eq!(ports[0], Value::Integer(8080));
        assert_eq!(ports[1], Value::Integer(8081));
        assert_eq!(ports[2], Value::Integer(8082));
        
        let empty = result.get("empty_array").and_then(|v| v.as_array()).expect("empty array");
        assert_eq!(empty.len(), 0);
        
        clear_test_env(&["WB_ALLOWED_HOSTS", "WB_PORTS", "WB_EMPTY_ARRAY"]);
    }
    
    #[test]
    fn test_case_insensitive_keys() {
        set_test_env(&[
            ("WB_SERVER__PORT", "8080"),
            ("WB_SERVER__HOST", "localhost"),
        ]);
        
        let result = parse_env_vars("WB_");
        
        // 键应该被转换为小写
        assert!(result.contains_key("server"));
        let server = result.get("server").and_then(|v| v.as_table()).expect("server table");
        assert!(server.contains_key("port"));
        assert!(server.contains_key("host"));
        
        clear_test_env(&["WB_SERVER__PORT", "WB_SERVER__HOST"]);
    }
    
    #[test]
    fn test_prefix_filtering() {
        set_test_env(&[
            ("WB_SERVER__PORT", "8080"),
            ("OTHER_VAR", "ignored"),
            ("WB_DEBUG", "true"),
        ]);
        
        let result = parse_env_vars("WB_");
        
        assert!(result.contains_key("server"));
        assert!(result.contains_key("debug"));
        assert!(!result.contains_key("other_var"));
        
        clear_test_env(&["WB_SERVER__PORT", "OTHER_VAR", "WB_DEBUG"]);
    }
    
    #[test]
    fn test_empty_prefix_value() {
        set_test_env(&[("WB_", "should_be_ignored")]);
        
        let result = parse_env_vars("WB_");
        assert!(result.is_empty());
        
        clear_test_env(&["WB_"]);
    }
    
    #[test]
    fn test_string_values_remain_strings() {
        set_test_env(&[
            ("WB_VERSION", "1.0.0"),
            ("WB_NAME", "my-app"),
            ("WB_DESCRIPTION", "A test application"),
        ]);
        
        let result = parse_env_vars("WB_");
        
        assert_eq!(result.get("version"), Some(&Value::String("1.0.0".to_string())));
        assert_eq!(result.get("name"), Some(&Value::String("my-app".to_string())));
        assert_eq!(result.get("description"), Some(&Value::String("A test application".to_string())));
        
        clear_test_env(&["WB_VERSION", "WB_NAME", "WB_DESCRIPTION"]);
    }
}
