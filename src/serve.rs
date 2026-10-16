use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{Path as AxumPath, RawQuery, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde_json::json;

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

#[allow(dead_code)]
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

fn parse_offset(query: &str) -> Option<usize> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key == "offset" {
            value.parse::<usize>().ok()
        } else {
            None
        }
    })
}

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

fn parse_pagination(query: Option<&str>) -> (usize, usize) {
    let Some(query) = query else {
        return (DEFAULT_LIMIT, 0);
    };
    let limit = parse_limit(query)
        .unwrap_or(DEFAULT_LIMIT)
        .min(MAX_LIMIT);
    let offset = parse_offset(query).unwrap_or(0);
    (limit, offset)
}

fn run_id_of_value(value: &serde_json::Value) -> Option<&str> {
    value.get("id").and_then(|v| v.as_str())
}

fn load_history_values(workspace_dir: &Path) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    if let Ok(contents) = std::fs::read_to_string(workspace_dir.join("runs.json")) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) {
            if let Some(arr) = parsed.as_array() {
                values.extend(arr.iter().cloned());
            }
        }
    }
    if let Ok(contents) = std::fs::read_to_string(workspace_dir.join("runs.jsonl")) {
        for line in contents.lines() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                values.push(value);
            }
        }
    }
    values
}

fn combined_runs_json(workspace_dir: &Path) -> Vec<serde_json::Value> {
    let mut runs: Vec<serde_json::Value> = Workspace::root(workspace_dir)
        .iter()
        .filter_map(|run| serde_json::to_value(run).ok())
        .collect();
    for value in load_history_values(workspace_dir) {
        let id = run_id_of_value(&value).unwrap_or("");
        if id.is_empty() || !runs.iter().any(|existing| run_id_of_value(existing) == Some(id)) {
            runs.push(value);
        }
    }
    runs
}

#[derive(Clone)]
struct AppState {
    workspace_dir: Arc<PathBuf>,
}

async fn health() -> impl axum::response::IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn list_runs(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let all_runs = combined_runs_json(&state.workspace_dir);
    let (limit, offset) = parse_pagination(query.as_deref());
    let runs = all_runs.into_iter().skip(offset).take(limit).collect();
    Ok(Json(runs))
}

async fn get_run(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<RunRecord>, (StatusCode, Json<serde_json::Value>)> {
    let ws = Workspace::open(&state.workspace_dir, &id).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("run {id} not found") })),
        )
    })?;
    let run = ws.load_record().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to load run {id}: {e}") })),
        )
    })?;
    Ok(Json(run))
}

fn readonly_http_response(status_code: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn find_run_in_history_files(workspace_dir: &Path, id: &str) -> Option<String> {
    let json_path = workspace_dir.join("runs.json");
    if let Ok(contents) = std::fs::read_to_string(&json_path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
            if let Some(arr) = value.as_array() {
                for item in arr {
                    if item.get("id").and_then(|v| v.as_str()) == Some(id) {
                        return Some(item.to_string());
                    }
                }
            }
        }
    }

    let jsonl_path = workspace_dir.join("runs.jsonl");
    if let Ok(contents) = std::fs::read_to_string(&jsonl_path) {
        for line in contents.lines() {
            if let Ok(item) = serde_json::from_str::<serde_json::Value>(line) {
                if item.get("id").and_then(|v| v.as_str()) == Some(id) {
                    return Some(item.to_string());
                }
            }
        }
    }

    None
}

fn readonly_route(workspace_dir: &Path, path: &str) -> (u16, &'static str, String) {
    if path == "health" {
        return (200, "OK", json!({ "status": "ok" }).to_string());
    }

    if path == "runs" {
        let all_runs = combined_runs_json(workspace_dir);
        match serde_json::to_string(&all_runs) {
            Ok(body) => return (200, "OK", body),
            Err(e) => {
                return (
                    500,
                    "Internal Server Error",
                    json!({ "error": format!("failed to serialize runs: {e}") }).to_string(),
                )
            }
        }
    }

    if let Some(rest) = path.strip_prefix("runs/") {
        if rest.is_empty() || rest.contains('/') {
            return (404, "Not Found", json!({ "error": "not found" }).to_string());
        }
        match Workspace::open(workspace_dir, rest) {
            Ok(ws) => match ws.load_record() {
                Ok(run) => match run_to_json(&run) {
                    Ok(body) => return (200, "OK", body),
                    Err(e) => {
                        return (
                            500,
                            "Internal Server Error",
                            json!({ "error": format!("failed to serialize run {rest}: {e}") }).to_string(),
                        )
                    }
                },
                Err(e) => {
                    return (
                        500,
                        "Internal Server Error",
                        json!({ "error": format!("failed to load run {rest}: {e}") }).to_string(),
                    )
                }
            },
            Err(_) => {
                if let Some(body) = find_run_in_history_files(workspace_dir, rest) {
                    return (200, "OK", body);
                }
                return (
                    404,
                    "Not Found",
                    json!({ "error": format!("run {rest} not found") }).to_string(),
                );
            }
        }
    }

    (404, "Not Found", json!({ "error": "not found" }).to_string())
}

fn handle_readonly_connection(stream: &mut TcpStream, workspace_dir: &Path) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&tmp[..n]);
        if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    if buffer.is_empty() {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buffer);
    let mut lines = request.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method != "GET" {
        let body = json!({ "error": "method not allowed" }).to_string();
        let response = readonly_http_response(405, "Method Not Allowed", &body);
        stream.write_all(response.as_bytes())?;
        return Ok(());
    }

    let path = target.split('?').next().unwrap_or(target);
    let path = path.trim_start_matches('/');
    let (status_code, reason, body) = readonly_route(workspace_dir, path);
    let response = readonly_http_response(status_code, reason, &body);
    stream.write_all(response.as_bytes())?;
    Ok(())
}



pub fn serve_readonly<P: AsRef<Path>>(workspace_dir: P) -> Result<SocketAddr> {
    let workspace_dir = workspace_dir.as_ref();
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let local_addr = listener.local_addr()?;
    let dir = workspace_dir.to_path_buf();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    if let Err(e) = handle_readonly_connection(&mut stream, &dir) {
                        eprintln!("readonly connection error: {e}");
                    }
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                Err(e) => {
                    eprintln!("readonly accept error: {e}");
                }
            }
        }
    });

    Ok(local_addr)
}

pub async fn serve_with_dir(workspace_dir: &Path, bind_addr: SocketAddr) -> anyhow::Result<SocketAddr> {
    let state = AppState {
        workspace_dir: Arc::new(workspace_dir.to_path_buf()),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
        .with_state(state);

    let loopback = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bind_addr.port()));
    let listener = tokio::net::TcpListener::bind(loopback).await?;
    let local_addr = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Ok(local_addr)
}

pub async fn serve(addr: SocketAddr) -> anyhow::Result<SocketAddr> {
    let workspace_dir = std::env::var("WORKBUDDY_WORKSPACE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".workbuddy"));
    serve_with_dir(&workspace_dir, addr).await
}

pub async fn start(workspace_dir: &Path, bind_addr: SocketAddr) -> anyhow::Result<SocketAddr> {
    serve_with_dir(workspace_dir, bind_addr).await
}

pub async fn run(workspace_dir: &Path, bind_addr: SocketAddr) -> anyhow::Result<()> {
    let addr = serve_with_dir(workspace_dir, bind_addr).await?;
    println!("Serving on http://{addr}");
    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{RunRecord, Workspace};
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::Value;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt;

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
    fn test_parse_pagination_defaults_and_bounds() {
        assert_eq!(parse_pagination(None), (20, 0));
        assert_eq!(parse_pagination(Some("")), (20, 0));
        assert_eq!(parse_pagination(Some("limit=5&offset=2")), (5, 2));
        assert_eq!(parse_pagination(Some("limit=999&offset=10")), (100, 10));
        assert_eq!(parse_pagination(Some("limit=abc&offset=xyz")), (20, 0));
    }

    #[tokio::test]
    async fn test_axum_http_routes() {
        let (workspace_dir, run_id) = create_test_workspace();
        let state = AppState {
            workspace_dir: Arc::new(workspace_dir.clone()),
        };
        let app = Router::new()
            .route("/health", get(health))
            .route("/runs", get(list_runs))
            .route("/runs/{id}", get(get_run))
            .with_state(state);

        // health
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");

        // runs
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/runs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.is_array());
        let runs = json.as_array().unwrap();
        assert_eq!(runs.len(), 1);

        // runs with pagination query: offset=1 should skip the only run
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/runs?limit=5&offset=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 0);

        // runs/{id}
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&format!("/runs/{}", run_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], run_id);

        // nonexistent
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn serve_readonly_health_and_runs() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let (workspace_dir, run_id) = create_test_workspace();
        let addr = serve_readonly(&workspace_dir).unwrap();

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json"));
        let body = response.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["status"], "ok");

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /runs HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        let body = response.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["id"].as_str(), Some(run_id.as_str()));
    }
}

