use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

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

fn json_response(status: u16, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let status_code = StatusCode(status);
    let content_type = Header::from_bytes(b"Content-Type", b"application/json").unwrap();
    let connection_close = Header::from_bytes(b"Connection", b"close").unwrap();
    Response::from_string(body)
        .with_status_code(status_code)
        .with_header(content_type)
        .with_header(connection_close)
}

fn respond_json(request: Request, status: u16, body: String) -> Result<()> {
    let response = json_response(status, body);
    request.respond(response)?;
    Ok(())
}

fn respond_error(request: Request, status: u16, message: &str) -> Result<()> {
    respond_json(request, status, json_error_body(message))
}

fn parse_limit(query: &str) -> Option<usize> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key == "limit" {
            value.parse::<usize>().ok()
        } else {
            None
        }
    })
}

fn handle_request(request: Request, workspace_dir: &Path) -> Result<()> {
    let method = request.method();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or(&url).trim_start_matches('/');

    if method != &Method::Get {
        return respond_error(request, 405, "method not allowed");
    }

    if path == "health" {
        let body = serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION")
        })
        .to_string();
        return respond_json(request, 200, body);
    }

    match parse_request_path(&url) {
        Some(RequestPath::Runs) => {
            let query = url.split('?').nth(1).unwrap_or("");
            let limit = parse_limit(query);
            let mut runs = Workspace::root(workspace_dir);
            if let Some(limit) = limit {
                runs.truncate(limit);
            }
            match runs_to_json(&runs) {
                Ok(body) => respond_json(request, 200, body),
                Err(e) => {
                    let msg = format!("failed to serialize runs: {e}");
                    respond_error(request, 500, &msg)
                }
            }
        }
        Some(RequestPath::Run(id)) => match Workspace::open(workspace_dir, &id) {
            Ok(ws) => match ws.load_record() {
                Ok(run) => match run_to_json(&run) {
                    Ok(body) => respond_json(request, 200, body),
                    Err(e) => {
                        let msg = format!("failed to serialize run: {e}");
                        respond_error(request, 500, &msg)
                    }
                },
                Err(e) => {
                    let msg = format!("failed to load run {id}: {e}");
                    respond_error(request, 500, &msg)
                }
            },
            Err(_) => {
                let msg = format!("run {id} not found");
                respond_error(request, 404, &msg)
            }
        },
        None => respond_error(request, 404, "not found"),
    }
}

pub async fn run(workspace_dir: &Path, bind_addr: SocketAddr) -> anyhow::Result<()> {
    let server = Server::http(bind_addr)
        .map_err(|e| anyhow::anyhow!("failed to bind {bind_addr}: {e}"))?;
    let local_addr = server.server_addr();
    println!("Serving on http://{local_addr}");
    let workspace_dir = workspace_dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        for request in server.incoming_requests() {
            let ws_dir = workspace_dir.clone();
            if let Err(e) = handle_request(request, &ws_dir) {
                eprintln!("request handling error: {e}");
            }
        }
        Ok(())
    })
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{RunRecord, Workspace};
    use serde_json::Value;
    use std::fs;
    use std::io::{Read, Write};
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
    fn test_parse_limit() {
        assert_eq!(parse_limit("limit=5"), Some(5));
        assert_eq!(parse_limit("foo=bar&limit=10"), Some(10));
        assert_eq!(parse_limit("limit=0"), Some(0));
        assert_eq!(parse_limit(""), None);
        assert_eq!(parse_limit("foo=bar"), None);
    }

    #[test]
    fn test_serve_http_routes() {
        let (workspace_dir, run_id) = create_test_workspace();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().expect("expected ip");
        let ws_clone = workspace_dir.clone();
        let server_thread = std::thread::spawn(move || {
            for _ in 0..3 {
                let request = server.recv().unwrap();
                handle_request(request, &ws_clone).unwrap();
            }
        });

        // GET /health
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("\"status\":\"ok\""));
        assert!(response.contains(env!("CARGO_PKG_VERSION")));

        // GET /runs
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET /runs HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.contains('['));

        // GET /runs/{id}
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