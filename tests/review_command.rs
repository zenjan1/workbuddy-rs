use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!("workbuddy_review_{}_{}_{}", tag, std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &Path { &self.0 }
}
impl Drop for TempDir {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test").env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test").env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output().unwrap();
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

fn init_repo(dir: &Path, file: &str, content: &str) {
    git(dir, &["init", "-q"]);
    fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "initial"]);
}

fn review(dir: &Path, extra: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_workbuddy"));
    cmd.arg("review").current_dir(dir);
    for (k, v) in extra { cmd.env(k, v); }
    cmd.output().unwrap()
}

fn parse(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap()
}

#[test]
fn local_static_checks_detect_issues_and_blocking_exit_code() {
    let dir = TempDir::new("local_checks");
    init_repo(dir.path(), "sample.rs", r#"fn main() {}
"#);
    let new_content = r#"fn main() {
    dbg!("debug residual");
    let long_line = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    <<<<<<< HEAD
}
"#;
    fs::write(dir.path().join("sample.rs"), new_content).unwrap();
    let output = review(dir.path(), &[]);
    let json = parse(&output);
    let arr = json.as_array().unwrap();
    assert!(!arr.is_empty());
    assert!(arr.iter().all(|p| p.get("severity").is_some() && p.get("message").is_some()));
    let msgs: Vec<String> = arr.iter().map(|p| p["message"].as_str().unwrap_or("").to_string()).collect();
    assert!(msgs.iter().any(|m| m.contains("debug") || m.contains("dbg")));
    assert!(msgs.iter().any(|m| m.contains("long") || m.contains("length")));
    assert!(msgs.iter().any(|m| m.contains("conflict") || m.contains("<<<<<<<")));
    assert!(arr.iter().any(|p| p["severity"].as_str() == Some("blocking")));
    assert!(!output.status.success());
}

#[test]
fn reviewer_model_triggers_llm_and_merges_results() {
    let dir = TempDir::new("reviewer_model");
    init_repo(dir.path(), "sample.rs", r#"fn main() {}
"#);
    fs::write(dir.path().join("sample.rs"), r#"fn main() {
    // normal change
}
"#).unwrap();
    fs::write(dir.path().join(".workbuddy.toml"), r#"reviewer = "mock"
"#).unwrap();
    let output = review(dir.path(), &[("WORKBUDDY_LLM_MOCK", "1")]);
    assert!(output.status.success());
    let json = parse(&output);
    let arr = json.as_array().unwrap();
    assert!(!arr.is_empty());
    assert!(arr.iter().all(|p| p.get("severity").is_some() && p.get("message").is_some()));
    let msgs: Vec<String> = arr.iter().map(|p| p["message"].as_str().unwrap_or("").to_string()).collect();
    assert!(!msgs.iter().any(|m| m.contains("debug") || m.contains("dbg")));
    assert!(!msgs.iter().any(|m| m.contains("long") || m.contains("length")));
    assert!(!msgs.iter().any(|m| m.contains("conflict") || m.contains("<<<<<<<")));
}
