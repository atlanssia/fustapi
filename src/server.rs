//! HTTP server setup and routing.
//!
//! Initializes the axum HTTP server, configures routes, and handles
//! graceful shutdown.

use std::net::SocketAddr;

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Serialize;
use tokio::net::TcpListener;
use tracing::info;

use crate::protocol;
use crate::web;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The address to bind to.
    pub addr: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
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
    let router = Router::new()
        // Web UI routes (served before API routes)
        .route("/ui", axum::routing::get(web::ui_handler))
        .route("/ui/", axum::routing::get(web::ui_handler))
        .route(
            "/api/providers",
            axum::routing::get(web::providers_api_handler),
        )
        .route("/api/models", axum::routing::get(web::models_api_handler))
        // API routes
        .route("/health", get(health_handler))
        .route("/v1/chat/completions", post(chat_completions_handler))
        .route("/v1/messages", post(messages_handler))
        .route("/v1/models", get(models_handler))
        .fallback(fallback_handler)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)); // 10MB body limit

    let addr = config.addr;
    let listener = TcpListener::bind(addr).await?;

    info!("Listening on {}", listener.local_addr()?);

    axum::serve(listener, router)
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
) -> impl IntoResponse {
    let protocol = protocol::detect_protocol("/v1/chat/completions", &headers);
    match protocol::dispatch_request(protocol, body).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

/// POST /v1/messages — Anthropic-compatible messages endpoint.
async fn messages_handler(headers: axum::http::HeaderMap, body: String) -> impl IntoResponse {
    let protocol = protocol::detect_protocol("/v1/messages", &headers);
    match protocol::dispatch_request(protocol, body).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

/// GET /v1/models — returns a list of available models.
async fn models_handler() -> impl IntoResponse {
    let models = vec![
        ModelInfo {
            id: "fustapi-mock".to_string(),
            object: "model",
            created: 1_000_000_000,
            owned_by: "fustapi",
        },
        ModelInfo {
            id: "gpt-4".to_string(),
            object: "model",
            created: 1_000_000_000,
            owned_by: "openai",
        },
        ModelInfo {
            id: "claude-3".to_string(),
            object: "model",
            created: 1_000_000_000,
            owned_by: "anthropic",
        },
    ];

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
