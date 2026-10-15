use std::net::SocketAddr;
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

#[derive(serde::Serialize)]
struct RunsResponse {
    runs: Vec<RunRecord>,
    limit: usize,
    offset: usize,
    total: usize,
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
) -> Result<Json<RunsResponse>, (StatusCode, Json<serde_json::Value>)> {
    let all_runs = Workspace::root(&state.workspace_dir);
    let total = all_runs.len();
    let (limit, offset) = parse_pagination(query.as_deref());
    let runs = all_runs.into_iter().skip(offset).take(limit).collect();
    Ok(Json(RunsResponse {
        runs,
        limit,
        offset,
        total,
    }))
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

pub async fn serve_with_dir(workspace_dir: &Path, bind_addr: SocketAddr) -> anyhow::Result<SocketAddr> {
    let state = AppState {
        workspace_dir: Arc::new(workspace_dir.to_path_buf()),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
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
        assert_eq!(json["limit"].as_u64(), Some(20));
        assert_eq!(json["offset"].as_u64(), Some(0));
        assert_eq!(json["total"].as_u64(), Some(1));
        let runs = json["runs"].as_array().unwrap();
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
        assert_eq!(json["limit"].as_u64(), Some(5));
        assert_eq!(json["offset"].as_u64(), Some(1));
        assert_eq!(json["total"].as_u64(), Some(1));
        assert_eq!(json["runs"].as_array().unwrap().len(), 0);

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
}