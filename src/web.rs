//! Embedded Web UI for the FustAPI control plane.
//!
//! Serves a single-page application embedded at compile time via
//! `include_bytes!`. No external dependencies or build tools required.

use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json,
};

use serde::Serialize;

/// HTML content embedded at compile time.
pub fn ui_html() -> &'static [u8] {
    include_bytes!("../ui/index.html")
}

/// Serve the embedded Web UI.
///
/// Returns the HTML page with appropriate content-type headers.
pub async fn ui_handler() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/html; charset=utf-8",
        )],
        ui_html(),
    )
}

/// Provider info for the Web UI API.
#[derive(Serialize)]
struct ProviderInfo {
    name: String,
    endpoint: String,
    has_key: bool,
}

/// Model routing info for the Web UI API.
#[derive(Serialize)]
struct ModelInfo {
    id: String,
    providers: Vec<String>,
}

/// Providers API response.
#[derive(Serialize)]
struct ProvidersResponse {
    providers: Vec<ProviderInfo>,
}

/// Models API response.
#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<ModelInfo>,
}

/// GET /api/providers — returns provider list for the Web UI.
///
/// Returns static data for now. Will be wired to config when stateful
/// provider registry is added.
pub async fn providers_api_handler() -> impl IntoResponse {
    // TODO: Wire to actual config providers when stateful registry is available.
    let providers = vec![
        ProviderInfo { name: "mock".to_string(), endpoint: "http://127.0.0.1:52415".to_string(), has_key: false },
    ];

    (StatusCode::OK, Json(ProvidersResponse { providers })).into_response()
}

/// GET /api/models — returns model routing for the Web UI.
///
/// Returns static data for now. Will be wired to config router when available.
pub async fn models_api_handler() -> impl IntoResponse {
    // TODO: Wire to actual config router when stateful registry is available.
    let models = vec![
        ModelInfo { id: "fustapi-mock".to_string(), providers: vec!["mock".to_string()] },
        ModelInfo { id: "gpt-4".to_string(), providers: vec!["openai".to_string()] },
        ModelInfo { id: "claude-3".to_string(), providers: vec!["anthropic".to_string()] },
    ];

    (StatusCode::OK, Json(ModelsResponse { models })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_html_is_non_empty() {
        let html = super::ui_html();
        assert!(!html.is_empty(), "Embedded HTML should not be empty");
        let text = std::str::from_utf8(html).expect("HTML should be valid UTF-8");
        assert!(text.contains("<!DOCTYPE html>"), "HTML should have DOCTYPE");
        assert!(text.contains("FustAPI"), "HTML should contain FustAPI branding");
    }

    #[test]
    fn providers_response_serializes() {
        let resp = ProvidersResponse { providers: vec![ProviderInfo { name: "test".into(), endpoint: "http://localhost".into(), has_key: true }] };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"has_key\":true"));
    }

    #[test]
    fn models_response_serializes() {
        let resp = ModelsResponse { models: vec![ModelInfo { id: "gpt-4".into(), providers: vec!["openai".into()] }] };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"id\":\"gpt-4\""));
        assert!(json.contains("\"providers\":[\"openai\"]"));
    }
}
