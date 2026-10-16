use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use workbuddy_rs::config::{get_config_value, set_config_value};

struct TestWorkspace {
    path: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let unique = format!(
            "workbuddy_config_set_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("failed to create temporary workspace directory");
        Self { path }
    }

    fn config_path(&self) -> PathBuf {
        self.path.join("config.toml")
    }

    fn write_initial_config(&self) {
        fs::write(
            self.config_path(),
            r#"# WorkBuddy configuration
model = "default-model"
max_budget = 5000
timeout = 30
stream = true

[llm]
model = "default-llm"
max_budget = 7000
base_url = "https://example.com/v1"
"#,
        )
        .expect("failed to write initial config");
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn read_config(ws: &TestWorkspace) -> String {
    fs::read_to_string(ws.config_path()).expect("failed to read config")
}

#[test]
fn config_set_updates_existing_keys_and_preserves_other_keys() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set model");
    set_config_value(&ws.config_path(), "max_budget", "12000").expect("set max_budget");

    let raw = read_config(&ws);
    assert!(
        raw.contains("model = \"gpt-4o\""),
        "model was not updated: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 12000"),
        "max_budget was not updated: {}",
        raw
    );
    assert!(
        raw.contains("base_url = \"https://example.com/v1\""),
        "unmodified llm.base_url changed: {}",
        raw
    );
    assert!(
        raw.contains("# WorkBuddy configuration"),
        "comment was not preserved: {}",
        raw
    );
}

#[test]
fn config_set_rejects_unknown_keys() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let original = read_config(&ws);

    let result = set_config_value(&ws.config_path(), "llm.unknown_key", "value");
    assert!(
        result.is_err(),
        "setting an unknown key should return an error"
    );

    let raw = read_config(&ws);
    assert_eq!(
        raw, original,
        "config file changed after rejected key update: {}",
        raw
    );
    assert!(
        raw.contains("model = \"default-model\"") && raw.contains("max_budget = 5000"),
        "config was modified after rejected key update: {}",
        raw
    );
    assert!(
        raw.contains("base_url = \"https://example.com/v1\""),
        "unmodified llm.base_url changed after rejected key update: {}",
        raw
    );
}

#[test]
fn config_set_rejects_invalid_types() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let original = read_config(&ws);

    let result = set_config_value(&ws.config_path(), "max_budget", "not-a-number");
    assert!(
        result.is_err(),
        "setting max_budget to a non-integer should return an error"
    );

    let raw = read_config(&ws);
    assert_eq!(
        raw, original,
        "config file changed after rejected type update: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 5000"),
        "config was modified after rejected type update: {}",
        raw
    );
    assert!(
        raw.contains("model = \"default-model\""),
        "unmodified model changed after rejected type update: {}",
        raw
    );
}

#[test]
fn config_set_value_can_be_read_back_with_get() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set model");

    let read_back = get_config_value(&ws.config_path(), "model").expect("get model");
    assert_eq!(
        read_back, "gpt-4o",
        "config --get returned unexpected value after config set"
    );
}

#[test]
fn config_set_accepts_boolean_values() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "stream", "false").expect("set stream to false");

    let raw = read_config(&ws);
    assert!(
        raw.contains("stream = false"),
        "boolean value was not updated: {}",
        raw
    );

    let read_back = get_config_value(&ws.config_path(), "stream").expect("get stream");
    assert_eq!(
        read_back, "false",
        "config --get returned unexpected boolean value after config set"
    );
}

#[test]
fn config_set_persists_updates_to_expected_path() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let config_path = ws.config_path();
    set_config_value(&config_path, "model", "gpt-4o").expect("set model");

    let persisted = fs::read_to_string(&config_path).expect("failed to read persisted config");
    assert!(
        persisted.contains("gpt-4o"),
        "set value was not persisted to expected path: {}",
        persisted
    );
}

#[test]
fn config_set_leaves_default_config_unchanged_without_updates() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let before = read_config(&ws);

    // Read-only config access should not modify the default configuration file.
    let default_model = get_config_value(&ws.config_path(), "model").expect("get default model");
    assert_eq!(default_model, "default-model");

    let after = read_config(&ws);
    assert_eq!(
        before, after,
        "default config changed after read-only access"
    );
}

#[test]
fn config_set_common_keys_are_reloaded_from_disk() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set model");
    set_config_value(&ws.config_path(), "max_budget", "12000").expect("set max_budget");
    set_config_value(&ws.config_path(), "timeout", "45").expect("set timeout");

    assert_eq!(
        get_config_value(&ws.config_path(), "model").expect("get model"),
        "gpt-4o",
        "config --get returned unexpected model value after config set"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "max_budget").expect("get max_budget"),
        "12000",
        "config --get returned unexpected max_budget value after config set"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "timeout").expect("get timeout"),
        "45",
        "config --get returned unexpected timeout value after config set"
    );
}

#[test]
fn config_set_updates_nested_llm_keys_and_preserves_siblings() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "llm.model", "gpt-4o").expect("set llm.model");
    set_config_value(&ws.config_path(), "llm.max_budget", "12000").expect("set llm.max_budget");

    let raw = read_config(&ws);
    assert!(
        raw.contains("model = \"gpt-4o\""),
        "llm.model was not updated: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 12000"),
        "llm.max_budget was not updated: {}",
        raw
    );
    assert!(
        raw.contains("base_url = \"https://example.com/v1\""),
        "unmodified llm.base_url changed: {}",
        raw
    );
    assert!(
        raw.contains("model = \"default-model\""),
        "top-level model should not have changed: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 5000"),
        "top-level max_budget should not have changed: {}",
        raw
    );

    assert_eq!(
        get_config_value(&ws.config_path(), "llm.model").expect("get llm.model"),
        "gpt-4o",
        "config --get returned unexpected llm.model value after config set"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "llm.max_budget").expect("get llm.max_budget"),
        "12000",
        "config --get returned unexpected llm.max_budget value after config set"
    );
}

#[test]
fn config_set_rejects_invalid_nested_type_values_without_writes() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let original = read_config(&ws);

    let invalid_budget = set_config_value(&ws.config_path(), "llm.max_budget", "not-a-number");
    assert!(
        invalid_budget.is_err(),
        "setting llm.max_budget to a non-integer should fail"
    );

    let invalid_model = set_config_value(&ws.config_path(), "llm.model", "123");
    assert!(
        invalid_model.is_err(),
        "setting llm.model to a non-string value should fail"
    );

    let raw = read_config(&ws);
    assert_eq!(
        raw, original,
        "config file changed after rejected nested type updates: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 7000"),
        "llm.max_budget changed after rejected update: {}",
        raw
    );
    assert!(
        raw.contains("model = \"default-llm\""),
        "llm.model changed after rejected update: {}",
        raw
    );
}

#[test]
fn config_set_rejects_readonly_keys() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let original = read_config(&ws);

    let result = set_config_value(&ws.config_path(), "llm.base_url", "https://changed.example.com");
    assert!(
        result.is_err(),
        "setting a read-only key should return an error"
    );

    let raw = read_config(&ws);
    assert_eq!(
        raw, original,
        "config file changed after rejected read-only key update: {}",
        raw
    );
}

#[test]
fn config_set_updates_string_integer_and_boolean_and_can_be_reloaded() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set model");
    set_config_value(&ws.config_path(), "max_budget", "12000").expect("set max_budget");
    set_config_value(&ws.config_path(), "stream", "false").expect("set stream");

    let raw = read_config(&ws);
    assert!(raw.contains("gpt-4o"), "model not updated: {}", raw);
    assert!(raw.contains("max_budget = 12000"), "max_budget not updated: {}", raw);
    assert!(raw.contains("stream = false"), "stream not updated: {}", raw);

    assert_eq!(
        get_config_value(&ws.config_path(), "model").expect("get model"),
        "gpt-4o"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "max_budget").expect("get max_budget"),
        "12000"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "stream").expect("get stream"),
        "false"
    );

    assert!(raw.contains("timeout = 30"), "timeout changed: {}", raw);
    assert!(raw.contains("https://example.com/v1"), "base_url changed: {}", raw);
}

#[test]
fn config_set_read_write_consistency_for_supported_types() {
    let ws = TestWorkspace::new();

    fs::write(
        ws.config_path(),
        r#"model = "default-model"
max_budget = 5000
timeout = 30
stream = true

[llm]
model = "default-llm"
max_budget = 7000
base_url = "https://example.com/v1"
temperature = 0.7
"#,
    )
    .expect("failed to write custom initial config");

    set_config_value(&ws.config_path(), "stream", "false").expect("set stream");
    set_config_value(&ws.config_path(), "max_budget", "12000").expect("set max_budget");
    set_config_value(&ws.config_path(), "llm.temperature", "0.9").expect("set llm.temperature");
    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set model");

    assert_eq!(
        get_config_value(&ws.config_path(), "stream").expect("get stream"),
        "false",
        "boolean value readback mismatch"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "max_budget").expect("get max_budget"),
        "12000",
        "integer value readback mismatch"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "llm.temperature").expect("get llm.temperature"),
        "0.9",
        "float value readback mismatch"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "model").expect("get model"),
        "gpt-4o",
        "string value readback mismatch"
    );

    let raw = read_config(&ws);
    assert!(raw.contains("stream = false"), "boolean not persisted: {}", raw);
    assert!(raw.contains("max_budget = 12000"), "integer not persisted: {}", raw);
    assert!(raw.contains("temperature = 0.9"), "float not persisted: {}", raw);
    assert!(raw.contains("model = \"gpt-4o\""), "string not persisted: {}", raw);
}