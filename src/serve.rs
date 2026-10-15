use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use crate::workspace::{RunRecord, Workspace};

#[derive(Debug, PartialEq)]
pub enum RequestPath {
    Runs,
    Run(String),
}

pub fn parse_request_path(path: &str) -> Option<RequestPath> {
    let path = path.split('?').next().unwrap_or(path);
    let path = path.trim_start_matches('/');
    if path == "runs" {
        return Some(RequestPath::Runs);
    }
    if let Some(rest) = path.strip_prefix("runs/") {
        if !rest.is_empty() && !rest.contains('/') {
            return Some(RequestPath::Run(rest.to_string()));
        }
    }
    None
}

pub fn runs_to_json(runs: &[RunRecord]) -> Result<String, serde_json::Error> {
    serde_json::to_string(runs)
}

pub fn run_to_json(run: &RunRecord) -> Result<String, serde_json::Error> {
    serde_json::to_string(run)
}

fn json_error_body(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

fn read_request_line(stream: &mut TcpStream) -> std::io::Result<(String, String)> {
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    Ok((method, path))
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    status_text: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn handle_connection(mut stream: TcpStream, workspace_dir: &Path) -> anyhow::Result<()> {
    let (method, path) = read_request_line(&mut stream)?;
    let route = parse_request_path(&path);

    let (status, status_text, body) = match (method.as_str(), route) {
        ("GET", Some(RequestPath::Runs)) => {
            let runs = Workspace::root(workspace_dir);
            match runs_to_json(&runs) {
                Ok(body) => (200, "OK", body),
                Err(e) => {
                    let msg = format!("failed to serialize runs: {e}");
                    (500, "Internal Server Error", json_error_body(&msg))
                }
            }
        }
        ("GET", Some(RequestPath::Run(id))) => {
            match Workspace::open(workspace_dir, &id) {
                Ok(ws) => match ws.load_record() {
                    Ok(run) => match run_to_json(&run) {
                        Ok(body) => (200, "OK", body),
                        Err(e) => {
                            let msg = format!("failed to serialize run: {e}");
                            (500, "Internal Server Error", json_error_body(&msg))
                        }
                    },
                    Err(e) => {
                        let msg = format!("failed to load run {id}: {e}");
                        (500, "Internal Server Error", json_error_body(&msg))
                    }
                },
                Err(_) => {
                    let msg = format!("run {id} not found");
                    (404, "Not Found", json_error_body(&msg))
                }
            }
        }
        ("GET", None) => (404, "Not Found", json_error_body("not found")),
        (_, None) => (404, "Not Found", json_error_body("not found")),
        (_, Some(_)) => (405, "Method Not Allowed", json_error_body("method not allowed")),
    };

    write_response(&mut stream, status, status_text, &body)?;
    Ok(())
}

fn serve_blocking(listener: TcpListener, workspace_dir: &Path) -> anyhow::Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let ws_dir = workspace_dir.to_path_buf();
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, &ws_dir) {
                        eprintln!("connection error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
    Ok(())
}

pub async fn run(workspace_dir: &Path, bind_addr: SocketAddr) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind_addr)?;
    let local_addr = listener.local_addr()?;
    println!("Serving on http://{local_addr}");
    let workspace_dir = workspace_dir.to_path_buf();
    tokio::task::spawn_blocking(move || serve_blocking(listener, &workspace_dir)).await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{RunRecord, Workspace};
    use serde_json::Value;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_run() -> RunRecord {
        RunRecord {
            id: "test-run".to_string(),
            goal: "echo test".to_string(),
            created: "2025-01-01T00:00:00Z".to_string(),
            model: "test-model".to_string(),
            mock: false,
            steps: Vec::new(),
            token_usage: None,
            char_usage: None,
        }
    }

    fn create_test_workspace() -> (PathBuf, String) {
        let workspace_dir = std::env::temp_dir().join(format!(
            "workbuddy-serve-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&workspace_dir).unwrap();

        let ws = Workspace::create_new(&workspace_dir, "echo test").unwrap();
        let mut run = sample_run();
        run.id = ws.dir.file_name().unwrap().to_string_lossy().to_string();
        let run_id = run.id.clone();
        ws.finish_run(&mut run, "# report").unwrap();
        (workspace_dir, run_id)
    }

    #[test]
    fn test_parse_request_path() {
        assert_eq!(parse_request_path("/runs"), Some(RequestPath::Runs));
        assert_eq!(
            parse_request_path("/runs/abc123"),
            Some(RequestPath::Run("abc123".to_string()))
        );
        assert_eq!(
            parse_request_path("/runs/abc?foo=bar"),
            Some(RequestPath::Run("abc".to_string()))
        );
        assert_eq!(parse_request_path("/runs/"), None);
        assert_eq!(parse_request_path("/health"), None);
        assert_eq!(parse_request_path("/"), None);
    }

    #[test]
    fn test_runs_to_json_and_run_to_json() {
        let runs = vec![sample_run()];
        let json_str = runs_to_json(&runs).unwrap();
        let value: Value = serde_json::from_str(&json_str).unwrap();
        assert!(value.is_array());

        let run = sample_run();
        let json_str = run_to_json(&run).unwrap();
        let value: Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(value.get("id").and_then(Value::as_str), Some(run.id.as_str()));
    }

    #[test]
    fn test_serve_http_routes() {
        let (workspace_dir, run_id) = create_test_workspace();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ws_clone = workspace_dir.clone();
        let server_thread = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let ws_dir = ws_clone.clone();
                handle_connection(stream, &ws_dir).unwrap();
            }
        });

        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET /runs HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.contains('['));

        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        let request = format!(
            "GET /runs/{run_id} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains(&run_id));

        server_thread.join().unwrap();
    }
}
