//! Embedded Web UI for the FustAPI control plane.
//!
//! Serves a single-page application embedded at compile time via
//! `include_bytes!`. No external dependencies or build tools required.
//!
//! API endpoints:
//! - `GET /api/providers` — list providers with real config data
//! - `POST /api/providers` — create a new provider
//! - `PUT /api/providers/:id` — update an existing provider
//! - `DELETE /api/providers/:id` — delete a provider (cascades to routes)
//! - `GET /api/models` — list model routing with real data
//! - `POST /api/routes` — create/update a model route
//! - `DELETE /api/routes/:model` — delete a model route

use axum::{
    Json, extract::Path, http::StatusCode, response::IntoResponse,
    extract::Extension,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use crate::router::Router as _;

/// HTML content embedded at compile time.
pub fn ui_html() -> &'static [u8] {
    include_bytes!("../ui/index.html")
}

/// Serve the embedded Web UI.
pub async fn ui_handler() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        ui_html(),
    )
}

// ── Request/Response Types ──────────────────────────────────────────

#[derive(Serialize)]
pub struct ProviderInfo {
    pub name: String,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub has_key: bool,
    #[serde(rename = "type")]
    pub provider_type: String,
}

#[derive(Deserialize)]
pub struct ProviderForm {
    pub name: String,
    pub endpoint: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_type")]
    pub provider_type: String,
}

fn default_type() -> String { "openai".to_string() }

#[derive(Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub providers: Vec<String>,
}

#[derive(Deserialize)]
pub struct RouteForm {
    pub model: String,
    pub providers: Vec<String>,
}

#[derive(Serialize)]
pub struct ProvidersResponse { pub providers: Vec<ProviderInfo> }

#[derive(Serialize)]
pub struct ModelsResponse { pub models: Vec<ModelInfo> }

#[derive(Serialize)]
pub struct MessageResponse { pub message: String }

// ── Helpers ─────────────────────────────────────────────────────────

fn load_config() -> crate::config::AppConfig {
    crate::config::load_merged(&crate::config::db_path())
        .unwrap_or_else(|_| crate::config::default_config())
}

fn save_and_rebuild(config: &crate::config::AppConfig) -> Result<(), (StatusCode, String)> {
    crate::config::save_to_db(config, &crate::config::db_path())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to save config: {e}")))?;
    Ok(())
}

// ── GET Handlers ────────────────────────────────────────────────────

/// GET /api/providers — returns provider list with real config data.
pub async fn providers_api_handler(
    Extension(router): Extension<Arc<crate::router::RealRouter>>,
) -> impl IntoResponse {
    let config = load_config();
    let providers = router.list_providers()
        .into_iter()
        .map(|name| {
            let cfg = config.providers.get(&name);
            ProviderInfo {
                name: name.clone(),
                endpoint: cfg.map(|c| c.endpoint.clone()).unwrap_or_default(),
                api_key: cfg.and_then(|c| c.api_key.clone()),
                has_key: cfg.map(|c| c.api_key.is_some()).unwrap_or(false),
                provider_type: cfg.map(|c| c.r#type.clone()).unwrap_or_else(|| "openai".to_string()),
            }
        })
        .collect();
    (StatusCode::OK, Json(ProvidersResponse { providers })).into_response()
}

/// GET /api/models — returns model routing with real provider data.
pub async fn models_api_handler(
    Extension(router): Extension<Arc<crate::router::RealRouter>>,
) -> impl IntoResponse {
    let config = load_config();
    let models = router.list_models()
        .into_iter()
        .map(|id| {
            let providers = config.router.get(&id).cloned().unwrap_or_default();
            ModelInfo { id, providers }
        })
        .collect();
    (StatusCode::OK, Json(ModelsResponse { models })).into_response()
}

// ── POST Handlers ───────────────────────────────────────────────────

/// POST /api/providers — create a new provider. Persists to DB.
pub async fn create_provider(
    Json(form): Json<ProviderForm>,
) -> impl IntoResponse {
    let mut config = load_config();

    if config.providers.contains_key(&form.name) {
        return (StatusCode::CONFLICT, Json(MessageResponse { message: format!("Provider '{}' already exists", form.name) })).into_response();
    }

    config.providers.insert(form.name.clone(), crate::config::ProviderConfig {
        endpoint: form.endpoint, api_key: form.api_key, r#type: form.provider_type,
    });

    if let Err(e) = save_and_rebuild(&config) { return e.into_response(); }

    (StatusCode::CREATED, Json(MessageResponse { message: format!("Provider '{}' created", form.name) })).into_response()
}

/// POST /api/routes — create/update a model route. Persists to DB.
pub async fn create_route(
    Json(form): Json<RouteForm>,
) -> impl IntoResponse {
    let mut config = load_config();

    let all_provider_names = config.providers.keys().cloned().collect::<std::collections::HashSet<_>>();
    for p in &form.providers {
        if !all_provider_names.contains(p) {
            return (StatusCode::BAD_REQUEST, Json(MessageResponse { message: format!("Provider '{}' not found", p) })).into_response();
        }
    }

    let existed = config.router.contains_key(&form.model);
    config.router.insert(form.model.clone(), form.providers);

    if let Err(e) = save_and_rebuild(&config) { return e.into_response(); }

    let msg = if existed { "Route updated" } else { "Route created" };
    (StatusCode::OK, Json(MessageResponse { message: format!("{}: {}", msg, form.model) })).into_response()
}

// ── PUT Handlers ────────────────────────────────────────────────────

/// PUT /api/providers/:id — update an existing provider. Persists to DB.
pub async fn update_provider(
    Path(id): Path<String>,
    Json(form): Json<ProviderForm>,
) -> impl IntoResponse {
    let mut config = load_config();

    if !config.providers.contains_key(&id) {
        return (StatusCode::NOT_FOUND, Json(MessageResponse { message: format!("Provider '{}' not found", id) })).into_response();
    }

    if form.name != id && config.providers.contains_key(&form.name) {
        return (StatusCode::CONFLICT, Json(MessageResponse { message: format!("Provider '{}' already exists", form.name) })).into_response();
    }

    if form.name != id { config.providers.remove(&id); }

    config.providers.insert(form.name.clone(), crate::config::ProviderConfig {
        endpoint: form.endpoint, api_key: form.api_key, r#type: form.provider_type,
    });

    if let Err(e) = save_and_rebuild(&config) { return e.into_response(); }

    (StatusCode::OK, Json(MessageResponse { message: format!("Provider '{}' updated", form.name) })).into_response()
}

// ── DELETE Handlers ─────────────────────────────────────────────────

/// DELETE /api/providers/:id — delete a provider and clean up routes referencing it. Persists to DB.
pub async fn delete_provider(
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut config = load_config();

    if !config.providers.contains_key(&id) {
        return (StatusCode::NOT_FOUND, Json(MessageResponse { message: format!("Provider '{}' not found", id) })).into_response();
    }

    config.providers.remove(&id);

    // Clean up routes that reference this provider; remove models with no providers left.
    config.router.retain(|_, providers| { providers.retain(|p| p != &id); !providers.is_empty() });

    if let Err(e) = save_and_rebuild(&config) { return e.into_response(); }

    (StatusCode::OK, Json(MessageResponse { message: format!("Provider '{}' deleted", id) })).into_response()
}

/// DELETE /api/routes/:model — delete a model route. Persists to DB.
pub async fn delete_route(
    Path(model): Path<String>,
) -> impl IntoResponse {
    let mut config = load_config();

    if config.router.remove(&model).is_none() {
        return (StatusCode::NOT_FOUND, Json(MessageResponse { message: format!("Route '{}' not found", model) })).into_response();
    }

    if let Err(e) = save_and_rebuild(&config) { return e.into_response(); }

    (StatusCode::OK, Json(MessageResponse { message: format!("Route '{}' deleted", model) })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_html_is_non_empty() {
        let html = super::ui_html();
        assert!(!html.is_empty());
        let text = std::str::from_utf8(html).expect("HTML should be valid UTF-8");
        assert!(text.contains("<!DOCTYPE html>"));
        assert!(text.contains("FustAPI"));
    }

    #[test]
    fn providers_response_serializes() {
        let resp = ProvidersResponse { providers: vec![ProviderInfo { name: "test".into(), endpoint: "http://localhost".into(), has_key: true, api_key: Some("sk-test".into()), provider_type: "omlx".into() }] };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"has_key\":true"));
        assert!(json.contains("\"type\":\"omlx\""));
    }

    #[test]
    fn models_response_serializes() {
        let resp = ModelsResponse { models: vec![ModelInfo { id: "gpt-4".into(), providers: vec!["openai".into()] }] };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"id\":\"gpt-4\""));
        assert!(json.contains("\"providers\":[\"openai\"]"));
    }
}
