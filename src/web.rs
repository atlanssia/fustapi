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

use crate::router::{RealRouter, RouterStore};
use axum::{Json, extract::Extension, extract::Path, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    #[serde(skip_serializing_if = "Option::is_none")]
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

fn default_type() -> String {
    "openai".to_string()
}

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
pub struct ProvidersResponse {
    pub providers: Vec<ProviderInfo>,
}

#[derive(Serialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelInfo>,
}

#[derive(Serialize)]
pub struct MessageResponse {
    pub message: String,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn load_config() -> crate::config::AppConfig {
    crate::config::load_merged(&crate::config::db_path())
        .unwrap_or_else(|_| crate::config::default_config())
}

fn save_and_rebuild(
    config: &crate::config::AppConfig,
    router_store: &RouterStore,
) -> Result<(), (StatusCode, String)> {
    crate::config::save_to_db(config, &crate::config::db_path()).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to save config: {e}"),
        )
    })?;
    router_store.store(Arc::new(RealRouter::from_config(config)));
    Ok(())
}

fn provider_config_from_form(
    form: ProviderForm,
    existing: Option<&crate::config::ProviderConfig>,
) -> crate::config::ProviderConfig {
    let api_key = match form.api_key {
        Some(key) if key.is_empty() => None,
        Some(key) => Some(key),
        None => existing.and_then(|cfg| cfg.api_key.clone()),
    };

    crate::config::ProviderConfig {
        endpoint: form.endpoint,
        api_key,
        r#type: form.provider_type,
    }
}

fn validate_route_form(form: &RouteForm) -> Result<(), &'static str> {
    if form.providers.is_empty() {
        return Err("At least one provider is required");
    }
    Ok(())
}

fn validate_provider_form(form: &ProviderForm) -> Result<(), &'static str> {
    if form.name.trim().is_empty() {
        return Err("Provider name is required");
    }
    if reqwest::Url::parse(&form.endpoint)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .is_none()
    {
        return Err("Provider endpoint must be a valid http or https URL");
    }
    if !matches!(
        form.provider_type.as_str(),
        "omlx" | "lmstudio" | "sglang" | "openai" | "deepseek"
    ) {
        return Err("Provider type is not supported");
    }
    Ok(())
}

fn provider_info_from_config(
    name: String,
    cfg: Option<&crate::config::ProviderConfig>,
) -> ProviderInfo {
    ProviderInfo {
        name,
        endpoint: cfg.map(|c| c.endpoint.clone()).unwrap_or_default(),
        api_key: None,
        has_key: cfg.map(|c| c.api_key.is_some()).unwrap_or(false),
        provider_type: cfg
            .map(|c| c.r#type.clone())
            .unwrap_or_else(|| "openai".to_string()),
    }
}

// ── GET Handlers ────────────────────────────────────────────────────

/// GET /api/providers — returns provider list with real config data.
pub async fn providers_api_handler() -> impl IntoResponse {
    let config = load_config();
    let providers = config
        .providers
        .keys()
        .cloned()
        .into_iter()
        .map(|name| {
            let cfg = config.providers.get(&name);
            provider_info_from_config(name, cfg)
        })
        .collect();
    (StatusCode::OK, Json(ProvidersResponse { providers })).into_response()
}

/// GET /api/models — returns model routing with real provider data.
pub async fn models_api_handler() -> impl IntoResponse {
    let config = load_config();
    let models = config
        .router
        .keys()
        .cloned()
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
    Extension(router_store): Extension<RouterStore>,
    Json(form): Json<ProviderForm>,
) -> impl IntoResponse {
    let mut config = load_config();
    let name = form.name.clone();

    if let Err(message) = validate_provider_form(&form) {
        return (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: message.to_string(),
            }),
        )
            .into_response();
    }

    if config.providers.contains_key(&name) {
        return (
            StatusCode::CONFLICT,
            Json(MessageResponse {
                message: format!("Provider '{}' already exists", name),
            }),
        )
            .into_response();
    }

    config
        .providers
        .insert(name.clone(), provider_config_from_form(form, None));

    if let Err(e) = save_and_rebuild(&config, &router_store) {
        return e.into_response();
    }

    (
        StatusCode::CREATED,
        Json(MessageResponse {
            message: format!("Provider '{}' created", name),
        }),
    )
        .into_response()
}

/// POST /api/routes — create/update a model route. Persists to DB.
pub async fn create_route(
    Extension(router_store): Extension<RouterStore>,
    Json(form): Json<RouteForm>,
) -> impl IntoResponse {
    let mut config = load_config();

    if let Err(message) = validate_route_form(&form) {
        return (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: message.to_string(),
            }),
        )
            .into_response();
    }

    let all_provider_names = config
        .providers
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    for p in &form.providers {
        if !all_provider_names.contains(p) {
            return (
                StatusCode::BAD_REQUEST,
                Json(MessageResponse {
                    message: format!("Provider '{}' not found", p),
                }),
            )
                .into_response();
        }
    }

    let existed = config.router.contains_key(&form.model);
    config.router.insert(form.model.clone(), form.providers);

    if let Err(e) = save_and_rebuild(&config, &router_store) {
        return e.into_response();
    }

    let msg = if existed {
        "Route updated"
    } else {
        "Route created"
    };
    (
        StatusCode::OK,
        Json(MessageResponse {
            message: format!("{}: {}", msg, form.model),
        }),
    )
        .into_response()
}

// ── PUT Handlers ────────────────────────────────────────────────────

/// PUT /api/providers/:id — update an existing provider. Persists to DB.
pub async fn update_provider(
    Extension(router_store): Extension<RouterStore>,
    Path(id): Path<String>,
    Json(form): Json<ProviderForm>,
) -> impl IntoResponse {
    let mut config = load_config();

    if let Err(message) = validate_provider_form(&form) {
        return (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: message.to_string(),
            }),
        )
            .into_response();
    }

    let Some(existing_provider) = config.providers.get(&id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(MessageResponse {
                message: format!("Provider '{}' not found", id),
            }),
        )
            .into_response();
    };

    if form.name != id && config.providers.contains_key(&form.name) {
        return (
            StatusCode::CONFLICT,
            Json(MessageResponse {
                message: format!("Provider '{}' already exists", form.name),
            }),
        )
            .into_response();
    }

    if form.name != id {
        config.providers.remove(&id);
        for providers in config.router.values_mut() {
            for provider in providers {
                if provider == &id {
                    *provider = form.name.clone();
                }
            }
        }
    }
    let name = form.name.clone();
    let provider_config = provider_config_from_form(form, Some(&existing_provider));

    config.providers.insert(name.clone(), provider_config);

    if let Err(e) = save_and_rebuild(&config, &router_store) {
        return e.into_response();
    }

    (
        StatusCode::OK,
        Json(MessageResponse {
            message: format!("Provider '{}' updated", name),
        }),
    )
        .into_response()
}

// ── DELETE Handlers ─────────────────────────────────────────────────

/// DELETE /api/providers/:id — delete a provider and clean up routes referencing it. Persists to DB.
pub async fn delete_provider(
    Extension(router_store): Extension<RouterStore>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut config = load_config();

    if !config.providers.contains_key(&id) {
        return (
            StatusCode::NOT_FOUND,
            Json(MessageResponse {
                message: format!("Provider '{}' not found", id),
            }),
        )
            .into_response();
    }

    config.providers.remove(&id);

    // Clean up routes that reference this provider; remove models with no providers left.
    config.router.retain(|_, providers| {
        providers.retain(|p| p != &id);
        !providers.is_empty()
    });

    if let Err(e) = save_and_rebuild(&config, &router_store) {
        return e.into_response();
    }

    (
        StatusCode::OK,
        Json(MessageResponse {
            message: format!("Provider '{}' deleted", id),
        }),
    )
        .into_response()
}

/// DELETE /api/routes/:model — delete a model route. Persists to DB.
pub async fn delete_route(
    Extension(router_store): Extension<RouterStore>,
    Path(model): Path<String>,
) -> impl IntoResponse {
    let mut config = load_config();

    if config.router.remove(&model).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(MessageResponse {
                message: format!("Route '{}' not found", model),
            }),
        )
            .into_response();
    }

    if let Err(e) = save_and_rebuild(&config, &router_store) {
        return e.into_response();
    }

    (
        StatusCode::OK,
        Json(MessageResponse {
            message: format!("Route '{}' deleted", model),
        }),
    )
        .into_response()
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
    fn ui_exposes_accessible_control_plane_structure() {
        let text = std::str::from_utf8(super::ui_html()).expect("HTML should be valid UTF-8");
        for required in [
            "<main",
            "role=\"tablist\"",
            "role=\"tab\"",
            "role=\"tabpanel\"",
            "aria-selected=\"true\"",
            "aria-live=\"polite\"",
            "role=\"dialog\"",
            "aria-modal=\"true\"",
        ] {
            assert!(text.contains(required), "UI should contain {required}");
        }
    }

    #[test]
    fn ui_implements_control_plane_workflows() {
        let text = std::str::from_utf8(super::ui_html()).expect("HTML should be valid UTF-8");
        for required in [
            "fetchProviders",
            "renderProviders",
            "openProviderModal",
            "deleteProvider",
            "fetchModels",
            "openRouteModal",
            "deleteRoute",
            "fetchHealth",
            "encodeURIComponent",
        ] {
            assert!(text.contains(required), "UI should implement {required}");
        }
    }

    #[test]
    fn ui_does_not_contain_known_invalid_css_tokens() {
        let text = std::str::from_utf8(super::ui_html()).expect("HTML should be valid UTF-8");
        for invalid in [
            "backgroundrgba",
            "colorvar",
            "backgroundvar",
            "width400px",
            "margin-bottom1rem",
            "font-size1.125rem",
            "text-transformuppercase",
            "font-familyinherit",
        ] {
            assert!(
                !text.contains(invalid),
                "UI should not contain invalid token {invalid}"
            );
        }
    }

    #[test]
    fn ui_does_not_use_inline_event_handlers() {
        let text = std::str::from_utf8(super::ui_html()).expect("HTML should be valid UTF-8");
        assert!(!text.contains("onclick="));
    }

    #[test]
    fn providers_response_serializes() {
        let resp = ProvidersResponse {
            providers: vec![ProviderInfo {
                name: "test".into(),
                endpoint: "http://localhost".into(),
                has_key: true,
                api_key: Some("sk-test".into()),
                provider_type: "omlx".into(),
            }],
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"has_key\":true"));
        assert!(json.contains("\"type\":\"omlx\""));
    }

    #[test]
    fn provider_info_from_config_redacts_api_key() {
        let cfg = crate::config::ProviderConfig {
            endpoint: "http://localhost".into(),
            api_key: Some("sk-secret".into()),
            r#type: "openai".into(),
        };

        let info = provider_info_from_config("main".into(), Some(&cfg));
        let json = serde_json::to_string(&info).expect("should serialize");

        assert!(info.has_key);
        assert_eq!(info.api_key, None);
        assert!(!json.contains("sk-secret"));
        assert!(!json.contains("api_key"));
    }

    #[test]
    fn provider_config_from_update_preserves_key_when_api_key_is_omitted() {
        let existing = crate::config::ProviderConfig {
            endpoint: "http://old".into(),
            api_key: Some("sk-existing".into()),
            r#type: "openai".into(),
        };
        let form = ProviderForm {
            name: "main".into(),
            endpoint: "http://new".into(),
            api_key: None,
            provider_type: "openai".into(),
        };

        let config = provider_config_from_form(form, Some(&existing));

        assert_eq!(config.endpoint, "http://new");
        assert_eq!(config.api_key.as_deref(), Some("sk-existing"));
    }

    #[test]
    fn provider_config_from_update_clears_key_when_api_key_is_empty() {
        let existing = crate::config::ProviderConfig {
            endpoint: "http://old".into(),
            api_key: Some("sk-existing".into()),
            r#type: "openai".into(),
        };
        let form = ProviderForm {
            name: "main".into(),
            endpoint: "http://new".into(),
            api_key: Some(String::new()),
            provider_type: "openai".into(),
        };

        let config = provider_config_from_form(form, Some(&existing));

        assert_eq!(config.api_key, None);
    }

    #[test]
    fn route_form_rejects_empty_provider_list() {
        let form = RouteForm {
            model: "qwen".into(),
            providers: Vec::new(),
        };

        let err = validate_route_form(&form).expect_err("empty provider list should be rejected");

        assert_eq!(err, "At least one provider is required");
    }

    #[test]
    fn provider_form_rejects_invalid_fields() {
        let empty_name = ProviderForm {
            name: " ".into(),
            endpoint: "http://localhost:8000".into(),
            api_key: None,
            provider_type: "openai".into(),
        };
        assert_eq!(
            validate_provider_form(&empty_name).expect_err("empty names should be rejected"),
            "Provider name is required"
        );

        let bad_endpoint = ProviderForm {
            name: "main".into(),
            endpoint: "file:///tmp/provider".into(),
            api_key: None,
            provider_type: "openai".into(),
        };
        assert_eq!(
            validate_provider_form(&bad_endpoint).expect_err("bad URLs should be rejected"),
            "Provider endpoint must be a valid http or https URL"
        );

        let bad_type = ProviderForm {
            name: "main".into(),
            endpoint: "http://localhost:8000".into(),
            api_key: None,
            provider_type: "unknown".into(),
        };
        assert_eq!(
            validate_provider_form(&bad_type).expect_err("unknown types should be rejected"),
            "Provider type is not supported"
        );
    }

    #[test]
    fn models_response_serializes() {
        let resp = ModelsResponse {
            models: vec![ModelInfo {
                id: "gpt-4".into(),
                providers: vec!["openai".into()],
            }],
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"id\":\"gpt-4\""));
        assert!(json.contains("\"providers\":[\"openai\"]"));
    }
}
