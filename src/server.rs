//! HTTP server setup and routing.
//!
//! Initializes the axum HTTP server, configures routes, and handles
//! graceful shutdown.

use std::net::SocketAddr;

use axum::{
    extract::DefaultBodyLimit,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use serde::Serialize;
use tokio::net::TcpListener;
use tracing::info;

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

/// Error response for unknown routes.
#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

/// Run the HTTP server.
///
/// Binds to the configured address, starts the axum router, and handles
/// graceful shutdown on SIGINT/SIGTERM.
pub async fn run(config: ServerConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = Router::new()
        .route("/health", get(health_handler))
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
    (StatusCode::OK, axum::Json(HealthResponse { status: "ok" }))
}

/// Fallback handler for unknown routes — returns 404 with JSON error.
async fn fallback_handler() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, axum::Json(ErrorResponse { error: "not found" }))
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
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
            sigterm.recv().await;
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, only listen for Ctrl+C (already handled above).
            // This branch is a no-op; the ctrl_c branch will fire first.
            tokio::signal::ctrl_c().await.ok();
        }
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received");
}
