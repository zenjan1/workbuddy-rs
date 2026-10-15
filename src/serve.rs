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

use crate::memory;
use crate::workspace::RunRecord;

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

async fn list_runs(
    State(workspace_dir): State<Arc<PathBuf>>,
) -> Result<Json<Vec<RunRecord>>, ApiError> {
    let runs = memory::list_runs(&workspace_dir).map_err(ApiError::internal)?;
    Ok(Json(runs))
}

async fn get_run(
    State(workspace_dir): State<Arc<PathBuf>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<RunRecord>, ApiError> {
    match memory::get_run(&workspace_dir, &id) {
        Ok(run) => Ok(Json(run)),
        Err(_) => Err(ApiError::not_found(format!("run {id} not found"))),
    }
}

pub fn router(workspace_dir: &Path) -> Router {
    Router::new()
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
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
        std::fs::create_dir_all(workspace_dir.join("memory")).unwrap();
        std::fs::create_dir_all(workspace_dir.join("runs")).unwrap();

        let app = router(&workspace_dir);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/runs"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = resp.json().await.unwrap();
        assert!(body.is_array());

        let resp = client
            .get(format!("http://{addr}/runs/does-not-exist"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value = resp.json().await.unwrap();
        assert!(body.get("error").is_some());
    }
}
