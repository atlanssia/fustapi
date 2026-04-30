//! HTTP server setup and routing.
//!
//! Initializes the axum HTTP server, configures routes, and handles
//! graceful shutdown.

use std::net::SocketAddr;
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            router: Arc::new(RealRouter::from_config(&crate::config::default_config())),
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
    let router2 = router_store.clone();
    let router3 = router_store.clone();
    let router_for_v1_models = router_store.clone();
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
        // API routes
        .route("/health", get(health_handler))
        .route(
            "/v1/chat/completions",
            post(move |headers, body| chat_completions_handler(headers, body, router2.clone())),
        )
        .route(
            "/v1/messages",
            post(move |headers, body| messages_handler(headers, body, router3.clone())),
        )
        .route(
            "/v1/models",
            get(move |headers| models_handler(headers, router_for_v1_models.clone())),
        )
        .fallback(fallback_handler)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)) // 10MB body limit
        .layer(axum::extract::Extension(router_store));

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
) -> impl IntoResponse {
    let protocol = protocol::detect_protocol("/v1/chat/completions", &headers);
    let current_router = router.load_full();
    match protocol::dispatch_request(protocol, body, current_router.as_ref()).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

/// POST /v1/messages — Anthropic-compatible messages endpoint.
async fn messages_handler(
    headers: axum::http::HeaderMap,
    body: String,
    router: RouterStore,
) -> impl IntoResponse {
    let protocol = protocol::detect_protocol("/v1/messages", &headers);
    let current_router = router.load_full();
    match protocol::dispatch_request(protocol, body, current_router.as_ref()).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

/// GET /v1/models — returns a list of available models.
async fn models_handler(_headers: axum::http::HeaderMap, router: RouterStore) -> impl IntoResponse {
    let current_router = router.load_full();
    let model_ids = current_router.list_models();
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
