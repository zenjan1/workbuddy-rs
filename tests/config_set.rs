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
    let err = result
        .expect_err("setting an unknown key should return an error")
        .to_string();
    assert!(
        err.to_lowercase().contains("unknown") || err.to_lowercase().contains("invalid"),
        "error message should mention unknown/invalid key, got: {}",
        err
    );
    assert!(
        err.contains("llm.unknown_key"),
        "error message should mention the rejected key, got: {}",
        err
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
    let err = result
        .expect_err("setting max_budget to a non-integer should return an error")
        .to_string();
    assert!(
        err.contains("max_budget"),
        "error message should mention the rejected key, got: {}",
        err
    );
    assert!(
        err.to_lowercase().contains("invalid")
            || err.to_lowercase().contains("type")
            || err.to_lowercase().contains("number")
            || err.to_lowercase().contains("integer"),
        "error message should describe the type problem, got: {}",
        err
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

#[test]
fn config_set_dot_path_parsing_updates_nested_value_only() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "llm.model", "gpt-4o").expect("set llm.model");

    let raw = read_config(&ws);
    let llm_section = raw
        .split("[llm]")
        .nth(1)
        .expect("config should contain [llm] section");
    assert!(
        llm_section.contains("model = \"gpt-4o\""),
        "llm.model was not updated in [llm]: {}",
        raw
    );
    assert!(
        raw.contains("model = \"default-model\""),
        "top-level model should be unaffected by llm.model update: {}",
        raw
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "llm.model").expect("get llm.model"),
        "gpt-4o"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "model").expect("get model"),
        "default-model"
    );
}

#[test]
fn config_set_type_conversion_handles_string_and_integer_values() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set string");
    set_config_value(&ws.config_path(), "max_budget", "12000").expect("set integer");

    let raw = read_config(&ws);
    assert!(
        raw.contains("model = \"gpt-4o\""),
        "string not written: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 12000"),
        "integer not written: {}",
        raw
    );

    assert_eq!(
        get_config_value(&ws.config_path(), "model").expect("get model"),
        "gpt-4o"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "max_budget").expect("get max_budget"),
        "12000"
    );
}

#[test]
fn config_set_preserves_existing_config_keys_on_successful_update() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set model");

    let raw = read_config(&ws);
    assert!(
        raw.contains("model = \"gpt-4o\""),
        "model not updated: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 5000"),
        "max_budget changed: {}",
        raw
    );
    assert!(
        raw.contains("timeout = 30"),
        "timeout changed: {}",
        raw
    );
    assert!(
        raw.contains("stream = true"),
        "stream changed: {}",
        raw
    );
    assert!(
        raw.contains("base_url = \"https://example.com/v1\""),
        "base_url changed: {}",
        raw
    );
    assert!(
        raw.contains("model = \"default-llm\""),
        "llm.model changed: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 7000"),
        "llm.max_budget changed: {}",
        raw
    );
    assert!(
        raw.contains("# WorkBuddy configuration"),
        "comment not preserved: {}",
        raw
    );
}

#[test]
fn config_set_rolls_back_on_failed_update_and_keeps_original_config() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let original = read_config(&ws);

    let invalid_key = set_config_value(&ws.config_path(), "llm.unknown_key", "value");
    assert!(
        invalid_key.is_err(),
        "writing an unknown key should fail before updating the file"
    );

    let invalid_type = set_config_value(&ws.config_path(), "max_budget", "not-a-number");
    assert!(
        invalid_type.is_err(),
        "writing an invalid type should fail before updating the file"
    );

    let raw = read_config(&ws);
    assert_eq!(
        raw, original,
        "config file changed after failed writes: {}",
        raw
    );
    assert!(
        raw.contains("model = \"default-model\""),
        "original model missing after failed writes: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 5000"),
        "original max_budget missing after failed writes: {}",
        raw
    );
    assert!(
        raw.contains("base_url = \"https://example.com/v1\""),
        "original base_url missing after failed writes: {}",
        raw
    );
}


fn workbuddy_bin() -> &'static str {
    if let Some(bin) = option_env!("CARGO_BIN_EXE_workbuddy") {
        bin
    } else if let Some(bin) = option_env!("CARGO_BIN_EXE_workbuddy-rs") {
        bin
    } else if let Some(bin) = option_env!("CARGO_BIN_EXE_workbuddy_rs") {
        bin
    } else {
        panic!("neither CARGO_BIN_EXE_workbuddy, CARGO_BIN_EXE_workbuddy-rs, nor CARGO_BIN_EXE_workbuddy_rs is set")
    }
}

fn run_config_set(ws: &TestWorkspace, key: &str, value: &str) -> std::process::Output {
    std::process::Command::new(workbuddy_bin())
        .arg("config")
        .arg("set")
        .arg(key)
        .arg(value)
        .current_dir(&ws.path)
        .env("WORKBUDDY_CONFIG", ws.config_path())
        .env("WORKBUDDY_CONFIG_PATH", ws.config_path())
        .env("WORKBUDDY_CONFIG_FILE", ws.config_path())
        .output()
        .expect("failed to execute workbuddy config set")
}

#[test]
fn cli_config_set_valid_nested_key_persists_and_reads_back() {
    let ws = TestWorkspace::new();
    fs::write(
        ws.config_path(),
        r#"model = "default-model"
max_budget = 5000
stream = true

[llm]
model = "default-llm"
temperature = 0.7
"#,
    )
    .expect("failed to write initial config");

    let output = run_config_set(&ws, "llm.temperature", "0.9");
    assert!(
        output.status.success(),
        "config set llm.temperature failed; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = read_config(&ws);
    assert!(
        raw.contains("temperature = 0.9"),
        "temperature was not persisted: {}",
        raw
    );

    let read_back = get_config_value(&ws.config_path(), "llm.temperature").expect("get llm.temperature");
    assert_eq!(read_back, "0.9", "config value read back did not match");
}

#[test]
fn cli_config_set_unknown_key_exits_nonzero_and_preserves_config() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let original = read_config(&ws);

    let output = run_config_set(&ws, "llm.unknown_key", "value");
    assert!(
        !output.status.success(),
        "config set llm.unknown_key should fail; status: {:?}, stdout: {}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("unknown") || stderr.to_lowercase().contains("invalid"),
        "expected unknown-key error on stderr, got: {}",
        stderr
    );

    let raw = read_config(&ws);
    assert_eq!(
        raw, original,
        "config file changed after rejected unknown-key set: {}",
        raw
    );
}

#[test]
fn cli_config_set_preserves_existing_unknown_fields() {
    let ws = TestWorkspace::new();
    fs::write(
        ws.config_path(),
        r#"# WorkBuddy configuration
model = "default-model"
future_setting = "keep-me"

[llm]
model = "default-llm"
temperature = 0.7
future_llm_setting = true
"#,
    )
    .expect("failed to write initial config");

    let output = run_config_set(&ws, "llm.temperature", "0.9");
    assert!(
        output.status.success(),
        "config set llm.temperature failed with unknown fields present; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = read_config(&ws);
    assert!(
        raw.contains("future_setting = \"keep-me\""),
        "top-level unknown field was not preserved: {}",
        raw
    );
    assert!(
        raw.contains("future_llm_setting = true"),
        "nested unknown field was not preserved: {}",
        raw
    );
    assert!(
        raw.contains("temperature = 0.9"),
        "temperature was not updated: {}",
        raw
    );
    assert!(
        raw.contains("# WorkBuddy configuration"),
        "comment was not preserved: {}",
        raw
    );
}

#[test]
fn config_set_writes_new_whitelisted_key_to_toml_and_updates_runtime() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    set_config_value(&ws.config_path(), "llm.temperature", "0.9").expect("set llm.temperature");

    let raw = read_config(&ws);
    assert!(
        raw.contains("temperature = 0.9"),
        "new whitelisted key was not written to TOML: {}",
        raw
    );

    let read_back =
        get_config_value(&ws.config_path(), "llm.temperature").expect("get llm.temperature");
    assert_eq!(
        read_back, "0.9",
        "runtime config did not reflect newly written key"
    );
}

#[test]
fn cli_config_set_llm_model_reloads_and_reads_back() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let output = run_config_set(&ws, "llm.model", "gpt-4o");
    assert!(
        output.status.success(),
        "config set llm.model failed; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = read_config(&ws);
    let llm_section = raw
        .split("[llm]")
        .nth(1)
        .expect("config should contain [llm] section");
    assert!(
        llm_section.contains("model = \"gpt-4o\""),
        "llm.model was not persisted in [llm]: {}",
        raw
    );
    assert!(
        raw.contains("model = \"default-model\""),
        "top-level model should be preserved: {}",
        raw
    );

    let read_back = get_config_value(&ws.config_path(), "llm.model").expect("get llm.model");
    assert_eq!(read_back, "gpt-4o", "config value read back did not match");
}

#[test]
fn config_set_rejects_unknown_top_level_key() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let original = read_config(&ws);

    let result = set_config_value(&ws.config_path(), "unknown_top_level", "value");
    let err = result
        .expect_err("setting an unknown top-level key should return an error")
        .to_string();
    assert!(
        err.to_lowercase().contains("unknown") || err.to_lowercase().contains("invalid"),
        "error message should mention unknown/invalid key, got: {}",
        err
    );
    assert!(
        err.contains("unknown_top_level"),
        "error message should mention the rejected key, got: {}",
        err
    );

    assert_eq!(
        read_config(&ws),
        original,
        "config file changed after rejected top-level key update"
    );
}

#[test]
fn config_set_automatic_type_conversion_persists_across_reads() {
    let ws = TestWorkspace::new();
    fs::write(
        ws.config_path(),
        "model = \"default-model\"\nmax_budget = 5000\nstream = true\n",
    )
    .expect("failed to write initial config");

    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set model");
    set_config_value(&ws.config_path(), "max_budget", "12000").expect("set max_budget");
    set_config_value(&ws.config_path(), "stream", "false").expect("set stream");

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
        raw.contains("stream = false"),
        "stream was not updated: {}",
        raw
    );

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
}

#[test]
fn config_set_creates_bak_backup_and_writes_valid_toml_after_typed_updates() {
    let ws = TestWorkspace::new();
    ws.write_initial_config();

    let original = read_config(&ws);

    set_config_value(&ws.config_path(), "model", "gpt-4o").expect("set model");

    let backup_path = ws.path.join("config.toml.bak");
    let backup = fs::read_to_string(&backup_path).unwrap_or_else(|err| {
        panic!("backup file {} was not created: {}", backup_path.display(), err)
    });
    assert_eq!(
        backup, original,
        "backup content should match original config before update"
    );

    set_config_value(&ws.config_path(), "max_budget", "12000").expect("set max_budget");
    set_config_value(&ws.config_path(), "stream", "false").expect("set stream");

    let raw = read_config(&ws);
    assert!(
        raw.contains("model = \"gpt-4o\""),
        "string value was not updated: {}",
        raw
    );
    assert!(
        raw.contains("max_budget = 12000"),
        "integer value was not updated: {}",
        raw
    );
    assert!(
        raw.contains("stream = false"),
        "boolean value was not updated: {}",
        raw
    );

    assert_eq!(
        get_config_value(&ws.config_path(), "model").expect("reparse updated model"),
        "gpt-4o",
        "updated config could not be parsed back for model"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "max_budget").expect("reparse updated max_budget"),
        "12000",
        "updated config could not be parsed back for max_budget"
    );
    assert_eq!(
        get_config_value(&ws.config_path(), "stream").expect("reparse updated stream"),
        "false",
        "updated config could not be parsed back for stream"
    );
}