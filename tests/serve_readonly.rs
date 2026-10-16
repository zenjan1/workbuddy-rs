use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use workbuddy_rs::serve::serve_readonly;
use workbuddy_rs::workspace::Workspace;

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

#[test]
fn serve_readonly_health_runs_and_method_not_allowed() {
    let temp = TestWorkspace::new();

    let workspace = Workspace::create(temp.path().to_path_buf())
        .expect("failed to create workspace");

    let addr = serve_readonly(&workspace).expect("failed to start read-only server");

    let (status, body) = http_request(addr, "GET", "/health");
    assert_eq!(status, 200, "/health returned status {status}, body: {body}");
    assert!(
        body.contains("\"status\": \"ok\"") || body.contains("\"status\":\"ok\""),
        "unexpected /health body: {body}"
    );

    let (status, body) = http_request(addr, "GET", "/runs");
    assert_eq!(status, 200, "/runs returned status {status}, body: {body}");
    let trimmed = body.trim();
    assert!(
        trimmed.starts_with('[') && trimmed.ends_with(']'),
        "/runs did not return a JSON array: {body}"
    );

    let (status, _body) = http_request(addr, "POST", "/runs");
    assert_eq!(status, 405, "POST /runs should return 405");
}
