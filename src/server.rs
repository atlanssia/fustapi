//! HTTP server setup and routing.
//!
//! Initializes the axum HTTP server, configures routes, and handles
//! graceful shutdown. Single port serves Web UI + LLM API + Control Plane.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use serde::Serialize;
use tokio::net::TcpListener;
use tracing::info;

use crate::metrics::{self, MetricsEmitter};
use crate::protocol;
use crate::router::{RealRouter, Router as _, RouterStore};
use crate::web;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The address to bind to.
    pub addr: SocketAddr,
    /// The router instance for provider dispatch.
    pub router: Arc<RealRouter>,
    /// Path to the SQLite database.
    pub db_path: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let host = crate::config::DEFAULT_HOST;
        let port = crate::config::DEFAULT_PORT;
        let addr: std::net::SocketAddr = format!("{host}:{port}")
            .parse()
            .expect("invalid default host:port");

        Self {
            addr,
            router: Arc::new(RealRouter::from_config(&crate::config::default_config())),
            db_path: crate::config::BootstrapConfig::default().db_path(),
        }
    }
}

/// Health check response.
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

/// Model info for /v1/models endpoint.
#[derive(Serialize)]
struct ModelInfo {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: &'static str,
}

/// Model list response.
#[derive(Serialize)]
struct ModelListResponse {
    object: &'static str,
    data: Vec<ModelInfo>,
}

/// Run the HTTP server.
///
/// Binds to the configured address, starts the axum router, and handles
/// graceful shutdown on SIGINT/SIGTERM.
pub async fn run(config: ServerConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router_store: RouterStore = Arc::new(arc_swap::ArcSwap::new(config.router.clone()));
    let db_path: Arc<PathBuf> = Arc::new(config.db_path.clone());

    // Initialize metrics system (spawns background aggregator)
    let (metrics_emitter, metrics_reader) = metrics::init();

    let app = Router::new()
        // Web UI routes (served before API routes)
        .route("/ui", axum::routing::get(web::ui_handler))
        .route("/ui/", axum::routing::get(web::ui_handler))
        // Control plane API — providers
        .route(
            "/api/providers",
            get(web::providers_api_handler).post(web::create_provider),
        )
        .route(
            "/api/providers/{id}",
            put(web::update_provider).delete(web::delete_provider),
        )
        // Control plane API — model routes
        .route("/api/models", get(web::models_api_handler))
        .route("/api/routes", post(web::create_route))
        .route("/api/routes/{model}", delete(web::delete_route))
        // Dashboard API — metrics
        .route("/metrics/summary", get(web::metrics_summary_handler))
        .route("/metrics/timeseries", get(web::metrics_timeseries_handler))
        // API routes
        .route("/health", get(health_handler))
        .route(
            "/v1/chat/completions",
            post({
                let router = router_store.clone();
                let emitter = metrics_emitter.clone();
                move |headers, body| chat_completions_handler(headers, body, router, emitter)
            }),
        )
        .route(
            "/v1/v1/chat/completions",
            post({
                let router = router_store.clone();
                let emitter = metrics_emitter.clone();
                move |headers, body| chat_completions_handler(headers, body, router, emitter)
            }),
        )
        .route(
            "/v1/messages",
            post({
                let router = router_store.clone();
                let emitter = metrics_emitter.clone();
                move |headers, body| messages_handler(headers, body, router, emitter)
            }),
        )
        .route(
            "/v1/v1/messages",
            post({
                let router = router_store.clone();
                let emitter = metrics_emitter.clone();
                move |headers, body| messages_handler(headers, body, router, emitter)
            }),
        )
        .route(
            "/v1/models",
            get({
                let router = router_store.clone();
                move |headers| models_handler(headers, router)
            }),
        )
        .route(
            "/v1/v1/models",
            get({
                let router = router_store.clone();
                move |headers| models_handler(headers, router)
            }),
        )
        .route("/", get(web::ui_handler))
        .fallback(fallback_handler)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)) // 10MB body limit
        .layer(axum::extract::Extension(db_path))
        .layer(axum::extract::Extension(router_store))
        .layer(axum::extract::Extension(metrics_reader));

    let addr = config.addr;
    let listener = TcpListener::bind(addr).await?;

    info!("Listening on {}", listener.local_addr()?);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("Server shut down gracefully");
    Ok(())
}

/// GET /health — returns {"status": "ok"}.
async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(HealthResponse { status: "ok" }))
}

/// POST /v1/chat/completions — OpenAI-compatible chat completions endpoint.
async fn chat_completions_handler(
    headers: axum::http::HeaderMap,
    body: String,
    router: RouterStore,
    emitter: MetricsEmitter,
) -> impl IntoResponse {
    let proto = protocol::detect_protocol("/v1/chat/completions", &headers);
    let current_router = router.load_full();

    // Resolve provider name for metrics (best-effort)
    let (provider_name, model_name) = resolve_provider_and_model(&body, current_router.as_ref());
    let start = emitter.request_start(&provider_name, &model_name);

    match protocol::dispatch_request(
        proto,
        body,
        current_router.as_ref(),
        emitter.clone(),
        provider_name.clone(),
        model_name.clone(),
        start,
    )
    .await
    {
        Ok(response) => response, // StreamTracker/collector handles emitting request_end
        Err(e) => {
            emitter.request_end(&provider_name, &model_name, start, false, None, None);
            e.into_response()
        }
    }
}

/// POST /v1/messages — Anthropic-compatible messages endpoint.
async fn messages_handler(
    headers: axum::http::HeaderMap,
    body: String,
    router: RouterStore,
    emitter: MetricsEmitter,
) -> impl IntoResponse {
    let proto = protocol::detect_protocol("/v1/messages", &headers);
    let current_router = router.load_full();

    let (provider_name, model_name) = resolve_provider_and_model(&body, current_router.as_ref());
    let start = emitter.request_start(&provider_name, &model_name);

    match protocol::dispatch_request(
        proto,
        body,
        current_router.as_ref(),
        emitter.clone(),
        provider_name.clone(),
        model_name.clone(),
        start,
    )
    .await
    {
        Ok(response) => response,
        Err(e) => {
            emitter.request_end(&provider_name, &model_name, start, false, None, None);
            e.into_response()
        }
    }
}

/// Extract model name from body and resolve to provider name (best-effort).
fn resolve_provider_and_model(body: &str, router: &dyn crate::router::Router) -> (String, String) {
    let model = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string());
    let provider = router.resolve(&model).ok().unwrap_or_else(|| "unknown".to_string());
    (provider, model)
}

/// GET /v1/models — returns a list of available models.
async fn models_handler(headers: axum::http::HeaderMap, router: RouterStore) -> impl IntoResponse {
    let current_router = router.load_full();
    let model_ids = current_router.list_models();

    let is_anthropic = headers.contains_key("anthropic-version");

    if is_anthropic {
        let models: Vec<serde_json::Value> = model_ids
            .into_iter()
            .map(|id| {
                serde_json::json!({
                    "type": "model",
                    "id": id,
                    "display_name": id,
                    "created_at": "2024-01-01T00:00:00Z"
                })
            })
            .collect();

        let first_id = models
            .first()
            .and_then(|m| m["id"].as_str())
            .unwrap_or("")
            .to_string();
        let last_id = models
            .last()
            .and_then(|m| m["id"].as_str())
            .unwrap_or("")
            .to_string();

        (
            StatusCode::OK,
            Json(serde_json::json!({
                "data": models,
                "has_more": false,
                "first_id": if first_id.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(first_id) },
                "last_id": if last_id.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(last_id) }
            })),
        )
            .into_response()
    } else {
        let models = model_ids
            .into_iter()
            .map(|id| ModelInfo {
                id,
                object: "model",
                created: 1_000_000_000,
                owned_by: "fustapi",
            })
            .collect();

        (
            StatusCode::OK,
            Json(ModelListResponse {
                object: "list",
                data: models,
            }),
        )
            .into_response()
    }
}

/// Fallback handler for unknown routes — returns 404 with JSON error.
async fn fallback_handler() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": { "message": "not found" } })),
    )
}

/// Wait for SIGINT or SIGTERM signal to trigger graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
            sigterm.recv().await;
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.ok();
        }
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received");
}
