use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;

use crate::workspace::{RunRecord, Workspace};

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn internal(err: impl std::fmt::Display) -> Self {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }

    fn not_found(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn list_runs(
    State(workspace_dir): State<Arc<PathBuf>>,
) -> Result<Json<Vec<RunRecord>>, ApiError> {
    let runs = Workspace::root(&workspace_dir);
    Ok(Json(runs))
}

async fn get_run(
    State(workspace_dir): State<Arc<PathBuf>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<RunRecord>, ApiError> {
    let ws = Workspace::open(&workspace_dir, &id)
        .map_err(|_| ApiError::not_found(format!("run {id} not found")))?;
    let run = ws.load_record().map_err(ApiError::internal)?;
    Ok(Json(run))
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })))
}

pub fn router(workspace_dir: &Path) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/runs", get(list_runs))
        .route("/runs/:id", get(get_run))
        .fallback(not_found)
        .with_state(Arc::new(workspace_dir.to_path_buf()))
}

pub async fn run(workspace_dir: &Path, bind_addr: SocketAddr) -> anyhow::Result<()> {
    let app = router(workspace_dir);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use crate::workspace::Workspace;
    use serde_json::Value;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn test_serve_routes() {
        let workspace_dir = std::env::temp_dir().join(format!(
            "workbuddy-serve-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let ws = Workspace::create_new(&workspace_dir, "echo test").unwrap();
        let mut run = crate::workspace::RunRecord {
            id: ws.dir.file_name().unwrap().to_string_lossy().to_string(),
            goal: "echo test".to_string(),
            created: "2025-01-01T00:00:00Z".to_string(),
            model: "test-model".to_string(),
            mock: false,
            steps: Vec::new(),
            token_usage: None,
            char_usage: None,
        };
        let run_id = run.id.clone();
        ws.finish_run(&mut run, "# report").unwrap();

        let app = router(&workspace_dir);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let health_url = format!("http://{addr}/health");
        // 等待服务真正开始接受连接，避免建立连接时尚未就绪。
        let mut ready = false;
        for _ in 0..100 {
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status() == StatusCode::OK => {
                    ready = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => {}
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(ready, "HTTP 服务未能在超时内就绪");

        let resp = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body.get("status").and_then(Value::as_str), Some("ok"));

        let resp = client
            .get(format!("http://{addr}/runs"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = resp.json().await.unwrap();
        assert!(body.is_array());

        // The run fixture above is served by GET /runs/{id}.
        let resp = client
            .get(format!("http://{addr}/runs/{run_id}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body.get("id").and_then(Value::as_str), Some(run_id.as_str()));

        let resp = client
            .get(format!("http://{addr}/runs/does-not-exist"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value = resp.json().await.unwrap();
        assert!(body.get("error").is_some());

        let resp = client
            .get(format!("http://{addr}/missing"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value = resp.json().await.unwrap();
        assert!(body.get("error").is_some());
    }
}
