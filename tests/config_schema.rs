use serde_json::Value;
use std::fs;
use std::process::Command;

const BINARY_NAME: &str = env!("CARGO_BIN_EXE_xi");

fn run_schema_command(args: &[&str]) -> Value {
    let output = Command::new(BINARY_NAME)
        .args(["config", "schema"])
        .args(args)
        .output()
        .expect("Failed to execute command");

    if !output.status.success() {
        panic!(
            "Command failed with status: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).expect("Failed to parse JSON output")
}

fn read_or_create_snapshot(snapshot_path: &str, generated: &Value) -> Value {
    let path = std::path::Path::new(snapshot_path);
    
    if path.exists() {
        let content = fs::read_to_string(path).expect("Failed to read snapshot file");
        serde_json::from_str(&content).expect("Failed to parse snapshot JSON")
    } else {
        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("Failed to create snapshot directory");
        }
        
        let json = serde_json::to_string_pretty(generated).expect("Failed to serialize JSON");
        fs::write(path, json).expect("Failed to write snapshot file");
        
        panic!(
            "Snapshot file created at {}. Please review and commit it.",
            snapshot_path
        );
    }
}

#[test]
fn test_config_schema() {
    let generated = run_schema_command(&["--config"]);
    let snapshot = read_or_create_snapshot("tests/snapshots/config_schema.json", &generated);
    assert_eq!(generated, snapshot, "Config schema does not match snapshot");
}

#[test]
fn test_agents_schema() {
    let generated = run_schema_command(&["--agents"]);
    let snapshot = read_or_create_snapshot("tests/snapshots/agents_schema.json", &generated);
    assert_eq!(generated, snapshot, "Agents schema does not match snapshot");
}
