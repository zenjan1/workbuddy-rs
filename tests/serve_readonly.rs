use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use workbuddy_rs::serve::{serve, serve_readonly};

struct TestWorkspace {
    path: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let unique = format!(
            "workbuddy_serve_readonly_{}_{}",
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

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn http_request(addr: SocketAddr, method: &str, target: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("failed to connect to read-only server");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("failed to set read timeout");

    let request = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nAccept: application/json\r\n\r\n",
        method, target
    );
    stream
        .write_all(request.as_bytes())
        .expect("failed to write HTTP request");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("failed to read HTTP response");

    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("response did not contain a valid HTTP status line");

    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();

    (status, body)
}

fn wait_for_port_release(addr: SocketAddr, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                drop(listener);
                return;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
                if Instant::now() >= deadline {
                    panic!("port {addr} was not released within {timeout:?}: {err}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => panic!("unexpected error while binding {addr}: {err}"),
        }
    }
}

#[test]
fn serve_readonly_health_runs_and_method_not_allowed() {
    let temp = TestWorkspace::new();

    let addr = serve_readonly(temp.path()).expect("failed to start read-only server");

    let (status, body) = http_request(addr, "GET", "/health");
    assert_eq!(status, 200, "/health returned status {status}, body: {body}");
    let health: serde_json::Value =
        serde_json::from_str(&body).expect("/health body is not valid JSON");
    assert_eq!(
        health["status"].as_str(),
        Some("ok"),
        "unexpected /health body: {body}"
    );

    let workspace = temp.path().to_string_lossy().to_string();
    let path_present = health
        .as_object()
        .map(|obj| obj.values().any(|value| value.as_str() == Some(workspace.as_str())))
        .unwrap_or(false);
    assert!(
        path_present,
        "/health did not include workspace path: {body}"
    );

    let (status, body) = http_request(addr, "GET", "/runs");
    assert_eq!(status, 200, "/runs returned status {status}, body: {body}");
    let runs: serde_json::Value =
        serde_json::from_str(&body).expect("/runs body is not valid JSON");
    assert!(
        runs.is_array(),
        "/runs did not return a JSON array: {body}"
    );

    let (status, _body) = http_request(addr, "POST", "/runs");
    assert!(
        matches!(status, 405 | 404),
        "POST /runs should return 405 or 404, got {status}"
    );

    let (status, _body) = http_request(addr, "POST", "/health");
    assert!(
        matches!(status, 405 | 404),
        "POST /health should return 405 or 404, got {status}"
    );

    let (status, _body) = http_request(addr, "POST", "/runs/mock-run-id");
    assert!(
        matches!(status, 405 | 404),
        "POST /runs/mock-run-id should return 405 or 404, got {status}"
    );
}

fn seed_mock_run_history(temp: &TestWorkspace, run_ids: &[&str]) {
    let cwd = temp.path().to_string_lossy().to_string();
    let runs: Vec<serde_json::Value> = run_ids
        .iter()
        .map(|run_id| {
            serde_json::json!({
                "id": run_id,
                "command": "echo hello",
                "cwd": cwd,
                "started_at": "2024-01-01T00:00:00Z",
                "start_time": "2024-01-01T00:00:00Z",
                "started": "2024-01-01T00:00:00Z",
                "finished_at": "2024-01-01T00:00:01Z",
                "finish_time": "2024-01-01T00:00:01Z",
                "ended_at": "2024-01-01T00:00:01Z",
                "finished": "2024-01-01T00:00:01Z",
                "ended": "2024-01-01T00:00:01Z",
                "exit_code": 0,
                "exit_status": 0,
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:01Z",
                "output_files": []
            })
        })
        .collect();

    for root in [temp.path().join(".workbuddy"), temp.path().to_path_buf()] {
        fs::create_dir_all(&root).expect("failed to create run storage directory");

        fs::write(
            root.join("runs.json"),
            serde_json::to_vec_pretty(&runs).expect("failed to serialize mock runs"),
        )
        .expect("failed to write mock runs.json");

        let mut jsonl = String::new();
        for run in &runs {
            jsonl.push_str(&serde_json::to_string(run).expect("failed to serialize mock run"));
            jsonl.push('\n');
        }
        fs::write(root.join("runs.jsonl"), jsonl.as_bytes()).expect("failed to write mock runs.jsonl");
    }
}

#[test]
fn serve_readonly_run_lookup_uses_mock_history_and_returns_404_for_unknown_id() {
    let temp = TestWorkspace::new();

    let run_ids = ["mock-run-id", "run-2", "run-3", "run-4", "run-5"];
    seed_mock_run_history(&temp, &run_ids);

    let runs_json_path = temp.path().join(".workbuddy").join("runs.json");
    let runs_jsonl_path = temp.path().join(".workbuddy").join("runs.jsonl");
    let original_runs_json = fs::read(&runs_json_path).expect("failed to read runs.json");
    let original_runs_jsonl = fs::read(&runs_jsonl_path).expect("failed to read runs.jsonl");

    let addr = serve_readonly(temp.path()).expect("failed to start read-only server");

    let (status, body) = http_request(addr, "GET", "/runs/mock-run-id");
    assert_eq!(status, 200, "/runs/mock-run-id returned status {status}, body: {body}");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("response body is not valid JSON");
    assert_eq!(
        parsed["id"].as_str(),
        Some("mock-run-id"),
        "unexpected run JSON: {body}"
    );

    let expected_runs: serde_json::Value =
        serde_json::from_slice(&original_runs_json).expect("failed to parse original runs.json");
    let expected_run = expected_runs
        .as_array()
        .and_then(|runs| runs.iter().find(|run| run["id"].as_str() == Some("mock-run-id")))
        .expect("mock-run-id is missing from runs.json");
    assert_eq!(
        parsed["command"].as_str(),
        expected_run["command"].as_str(),
        "run command does not match stored run: {body}"
    );
    assert_eq!(
        parsed["cwd"].as_str(),
        expected_run["cwd"].as_str(),
        "run cwd does not match stored run: {body}"
    );

    let (status, body) = http_request(addr, "GET", "/runs/unknown-run-id");
    assert_eq!(
        status,
        404,
        "GET /runs/unknown-run-id should return 404, body: {body}"
    );

    let (status, body) = http_request(addr, "GET", "/runs");
    assert_eq!(status, 200, "/runs returned status {status}, body: {body}");
    let runs: serde_json::Value =
        serde_json::from_str(&body).expect("response body is not valid JSON");
    let arr = runs.as_array().expect("/runs should be a JSON array");
    assert_eq!(arr.len(), 5, "expected 5 runs, got: {body}");
    assert!(
        arr.iter().any(|run| run["id"].as_str() == Some("mock-run-id")),
        "/runs did not include mock run: {body}"
    );

    let (status, body) = http_request(addr, "GET", "/runs?limit=2");
    assert_eq!(status, 200, "GET /runs?limit=2 returned {status}, body: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("invalid JSON");
    assert_eq!(page.as_array().expect("expected array").len(), 2);

    let (status, body) = http_request(addr, "GET", "/runs?limit=2&offset=2");
    assert_eq!(status, 200, "GET /runs?limit=2&offset=2 returned {status}, body: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("invalid JSON");
    assert_eq!(page.as_array().expect("expected array").len(), 2);

    let (status, body) = http_request(addr, "GET", "/runs?offset=4");
    assert_eq!(status, 200, "GET /runs?offset=4 returned {status}, body: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("invalid JSON");
    assert_eq!(page.as_array().expect("expected array").len(), 1);

    assert_eq!(
        fs::read(&runs_json_path).expect("failed to read runs.json"),
        original_runs_json,
        "runs.json was modified by read-only server"
    );
    assert_eq!(
        fs::read(&runs_jsonl_path).expect("failed to read runs.jsonl"),
        original_runs_jsonl,
        "runs.jsonl was modified by read-only server"
    );
}

#[test]
fn serve_post_run() {
    fn post_json(addr: SocketAddr, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).expect("failed to connect to server");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("failed to set read timeout");

        let request = format!(
            "POST /run HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(request.as_bytes())
            .expect("failed to write HTTP request");

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("failed to read HTTP response");

        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .expect("response did not contain a valid HTTP status line");

        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();

        (status, body)
    }

    let temp = TestWorkspace::new();
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let addr = runtime
        .block_on(serve(
            temp.path(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
        ))
        .expect("failed to start server");

    let (status, body) = post_json(addr, r#"{"task":"hello"}"#);
    assert_eq!(status, 202, "POST /run with valid task returned {status}, body: {body}");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("POST /run response body is not valid JSON");
    let run_id = parsed["id"]
        .as_str()
        .expect("POST /run response did not include string id");
    assert!(!run_id.is_empty(), "POST /run returned an empty id");

    let (status, body) = http_request(addr, "GET", "/runs");
    assert_eq!(status, 200, "GET /runs returned {status}, body: {body}");
    let runs: serde_json::Value =
        serde_json::from_str(&body).expect("/runs body is not valid JSON");
    let arr = runs.as_array().expect("/runs should be a JSON array");
    assert!(
        arr.iter().any(|run| run["id"].as_str() == Some(run_id)),
        "/runs did not include POST /run id {run_id}: {body}"
    );

    let (status, _body) = post_json(addr, r#"{"task":""}"#);
    assert_eq!(
        status, 400,
        "POST /run with empty task should return 400, got {status}"
    );
}

#[test]
fn serve_releases_port_after_shutdown() {
    let temp = TestWorkspace::new();
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let addr = runtime
        .block_on(serve(
            temp.path(),
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        ))
        .expect("failed to start server");

    assert!(
        addr.ip().is_loopback(),
        "server should bind loopback, got {addr}"
    );

    let (status, body) = http_request(addr, "GET", "/health");
    assert_eq!(status, 200, "/health returned status {status}, body: {body}");
    let health: serde_json::Value =
        serde_json::from_str(&body).expect("/health body is not valid JSON");
    assert_eq!(health["status"].as_str(), Some("ok"), "/health body: {body}");

    let (status, body) = http_request(addr, "GET", "/runs");
    assert_eq!(status, 200, "/runs returned status {status}, body: {body}");
    let runs: serde_json::Value =
        serde_json::from_str(&body).expect("/runs body is not valid JSON");
    assert!(runs.is_array(), "/runs did not return an array: {body}");

    let (status, body) = http_request(addr, "GET", "/runs/mock-run-id-that-does-not-exist");
    assert_eq!(
        status, 404,
        "GET /runs/mock-run-id-that-does-not-exist should return 404, got {status}, body: {body}"
    );

    // Simulate service exit by shutting down the ephemeral runtime that owns the
    // server task. Once the runtime is dropped, the listening socket must be
    // released so another process (or test) can bind the same port.
    let _ = runtime.shutdown_timeout(Duration::from_secs(5));

    wait_for_port_release(addr, Duration::from_secs(5));
}