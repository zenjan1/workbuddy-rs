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

[llm]
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
