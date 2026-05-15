//! Embedded Web UI for the `FustAPI` control plane.
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

use crate::metrics::MetricsReader;
use crate::router::{RealRouter, RouterStore};
use axum::{Json, extract::Extension, extract::Path, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// HTML content embedded at compile time.
#[must_use]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    #[serde(rename = "type")]
    pub provider_type: String,
}

#[derive(Deserialize)]
pub struct ProviderForm {
    pub name: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default = "default_type", rename = "type")]
    pub provider_type: String,
}

fn default_type() -> String {
    crate::config::default_type()
}

#[derive(Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub providers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
}

#[derive(Deserialize)]
pub struct RouteForm {
    pub model: String,
    pub providers: Vec<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
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

#[derive(Serialize)]
pub struct UnifiedBalanceEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: String,
    pub endpoint: String,
    pub has_key: bool,
    pub balance: Option<crate::provider::ProviderBalance>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct BalanceResponse {
    pub balances: Vec<UnifiedBalanceEntry>,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn load_config(db_path: &std::path::Path) -> crate::config::AppConfig {
    crate::config::load_from_db(db_path).unwrap_or_else(|_| crate::config::default_config())
}

fn save_and_rebuild(
    config: &crate::config::AppConfig,
    router_store: &RouterStore,
    db_path: &std::path::Path,
) -> Result<(), (StatusCode, String)> {
    crate::config::save_to_db(config, db_path).map_err(|e| {
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

    let model = match form.upstream_model {
        Some(m) if m.is_empty() => None,
        Some(m) => Some(m),
        None => existing.and_then(|cfg| cfg.model.clone()),
    };

    crate::config::ProviderConfig {
        endpoint: if form.endpoint.trim().is_empty() {
            form.provider_type
                .parse::<crate::types::ProviderType>()
                .ok()
                .and_then(|pt| pt.default_endpoint().map(String::from))
                .unwrap_or_default()
        } else {
            form.endpoint
        },
        api_key,
        model,
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
    let has_default = form
        .provider_type
        .parse::<crate::types::ProviderType>()
        .ok()
        .and_then(|pt| pt.default_endpoint())
        .is_some();
    if (!form.endpoint.trim().is_empty() || !has_default)
        && reqwest::Url::parse(&form.endpoint)
            .ok()
            .is_none_or(|url| !matches!(url.scheme(), "http" | "https"))
    {
        return Err("Provider endpoint must be a valid http or https URL");
    }
    if form.provider_type.parse::<crate::types::ProviderType>().is_err() {
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
        has_key: cfg.is_some_and(|c| c.api_key.is_some()),
        upstream_model: cfg.and_then(|c| c.model.clone()),
        provider_type: cfg.map_or_else(|| "openai".to_string(), |c| c.r#type.clone()),
    }
}
// ── GET Handlers ────────────────────────────────────────────────────

/// GET /api/providers — returns provider list with real config data.
pub async fn providers_api_handler(
    Extension(db_path): Extension<Arc<PathBuf>>,
) -> impl IntoResponse {
    let config = load_config(&db_path);
    let providers = config
        .providers
        .keys()
        .cloned()
        .map(|name| {
            let cfg = config.providers.get(&name);
            provider_info_from_config(name, cfg)
        })
        .collect();
    (StatusCode::OK, Json(ProvidersResponse { providers })).into_response()
}

/// GET /api/models — returns model routing with real provider data.
pub async fn models_api_handler(Extension(db_path): Extension<Arc<PathBuf>>) -> impl IntoResponse {
    let config = load_config(&db_path);
    let models = config
        .router
        .iter()
        .map(|(id, route_cfg)| ModelInfo {
            id: id.clone(),
            providers: route_cfg.provider_ids.clone(),
            upstream_model: route_cfg.upstream_model.clone(),
        })
        .collect();
    (StatusCode::OK, Json(ModelsResponse { models })).into_response()
}

// ── POST Handlers ───────────────────────────────────────────────────

/// POST /api/providers — create a new provider. Persists to DB.
pub async fn create_provider(
    Extension(router_store): Extension<RouterStore>,
    Extension(db_path): Extension<Arc<PathBuf>>,
    Json(form): Json<ProviderForm>,
) -> impl IntoResponse {
    let mut config = load_config(&db_path);
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
                message: format!("Provider '{name}' already exists"),
            }),
        )
            .into_response();
    }

    config
        .providers
        .insert(name.clone(), provider_config_from_form(form, None));

    if let Err(e) = save_and_rebuild(&config, &router_store, &db_path) {
        return e.into_response();
    }

    (
        StatusCode::CREATED,
        Json(MessageResponse {
            message: format!("Provider '{name}' created"),
        }),
    )
        .into_response()
}

/// POST /api/routes — create/update a model route. Persists to DB.
pub async fn create_route(
    Extension(router_store): Extension<RouterStore>,
    Extension(db_path): Extension<Arc<PathBuf>>,
    Json(form): Json<RouteForm>,
) -> impl IntoResponse {
    let mut config = load_config(&db_path);

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
                    message: format!("Provider '{p}' not found"),
                }),
            )
                .into_response();
        }
    }

    let existed = config.router.contains_key(&form.model);
    config.router.insert(
        form.model.clone(),
        crate::config::RouteConfig {
            provider_ids: form.providers,
            upstream_model: form.upstream_model.filter(|m| !m.trim().is_empty()),
        },
    );

    if let Err(e) = save_and_rebuild(&config, &router_store, &db_path) {
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
    Extension(db_path): Extension<Arc<PathBuf>>,
    Path(id): Path<String>,
    Json(form): Json<ProviderForm>,
) -> impl IntoResponse {
    let mut config = load_config(&db_path);

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
                message: format!("Provider '{id}' not found"),
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
        for route_cfg in config.router.values_mut() {
            for provider in &mut route_cfg.provider_ids {
                if provider == &id {
                    provider.clone_from(&form.name);
                }
            }
        }
    }
    let name = form.name.clone();
    let provider_config = provider_config_from_form(form, Some(&existing_provider));

    config.providers.insert(name.clone(), provider_config);

    if let Err(e) = save_and_rebuild(&config, &router_store, &db_path) {
        return e.into_response();
    }

    (
        StatusCode::OK,
        Json(MessageResponse {
            message: format!("Provider '{name}' updated"),
        }),
    )
        .into_response()
}

// ── DELETE Handlers ─────────────────────────────────────────────────

/// DELETE /api/providers/:id — delete a provider and clean up routes referencing it. Persists to DB.
pub async fn delete_provider(
    Extension(router_store): Extension<RouterStore>,
    Extension(db_path): Extension<Arc<PathBuf>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut config = load_config(&db_path);

    if !config.providers.contains_key(&id) {
        return (
            StatusCode::NOT_FOUND,
            Json(MessageResponse {
                message: format!("Provider '{id}' not found"),
            }),
        )
            .into_response();
    }

    config.providers.remove(&id);

    // Clean up routes that reference this provider; remove models with no providers left.
    config.router.retain(|_, route_cfg| {
        route_cfg.provider_ids.retain(|p| p != &id);
        !route_cfg.provider_ids.is_empty()
    });

    if let Err(e) = save_and_rebuild(&config, &router_store, &db_path) {
        return e.into_response();
    }

    (
        StatusCode::OK,
        Json(MessageResponse {
            message: format!("Provider '{id}' deleted"),
        }),
    )
        .into_response()
}

/// DELETE /api/routes/:model — delete a model route. Persists to DB.
pub async fn delete_route(
    Extension(router_store): Extension<RouterStore>,
    Extension(db_path): Extension<Arc<PathBuf>>,
    Path(model): Path<String>,
) -> impl IntoResponse {
    let mut config = load_config(&db_path);

    if config.router.remove(&model).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(MessageResponse {
                message: format!("Route '{model}' not found"),
            }),
        )
            .into_response();
    }

    if let Err(e) = save_and_rebuild(&config, &router_store, &db_path) {
        return e.into_response();
    }

    (
        StatusCode::OK,
        Json(MessageResponse {
            message: format!("Route '{model}' deleted"),
        }),
    )
        .into_response()
}

/// GET /api/providers/:id/models — list models available from a provider.
pub async fn provider_models_api_handler(
    Extension(db_path): Extension<Arc<PathBuf>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let config = load_config(&db_path);
    let Some(cfg) = config.providers.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Provider not found"})),
        )
            .into_response();
    };
    let provider = crate::config::create_provider(&id, cfg);
    match tokio::time::timeout(std::time::Duration::from_secs(10), provider.list_models()).await {
        Ok(Ok(models)) => (
            StatusCode::OK,
            Json(serde_json::json!({"models": models})),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e.to_string(), "models": []})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(serde_json::json!({"error": "timeout", "models": []})),
        )
            .into_response(),
    }
}

// ── Balance Handler ──────────────────────────────────────────────────

/// GET /api/balance — query account balance for all providers that support it.
pub async fn balance_api_handler(Extension(db_path): Extension<Arc<PathBuf>>) -> impl IntoResponse {
    let config = load_config(&db_path);

    let tasks: Vec<_> = config
        .providers
        .iter()
        .map(|(name, cfg)| {
            let name = name.clone();
            let ptype = cfg.r#type.clone();
            let endpoint = cfg.endpoint.clone();
            let has_key = cfg.api_key.is_some();
            let provider = crate::config::create_provider(&name, cfg);
            tokio::spawn(async move {
                let entry = |balance, error| UnifiedBalanceEntry {
                    name,
                    provider_type: ptype,
                    endpoint,
                    has_key,
                    balance,
                    error,
                };
                match tokio::time::timeout(std::time::Duration::from_secs(10), provider.balance())
                    .await
                {
                    Ok(Ok(Some(balance))) => entry(Some(balance), None),
                    Ok(Ok(None)) => entry(None, None),
                    Ok(Err(e)) => entry(None, Some(e.to_string())),
                    Err(_) => entry(None, Some("timeout".into())),
                }
            })
        })
        .collect();

    let mut balances = Vec::with_capacity(tasks.len());
    for handle in tasks {
        balances.push(handle.await.unwrap_or_else(|e| UnifiedBalanceEntry {
            name: "unknown".into(),
            provider_type: "unknown".into(),
            endpoint: String::new(),
            has_key: false,
            balance: None,
            error: Some(e.to_string()),
        }));
    }
    balances.sort_by(|a, b| a.name.cmp(&b.name));

    (StatusCode::OK, Json(BalanceResponse { balances })).into_response()
}

// ── Metrics Dashboard Handlers ──────────────────────────────────────

/// GET /metrics/summary — returns current metrics snapshot from memory.
pub async fn metrics_summary_handler(
    Extension(reader): Extension<MetricsReader>,
) -> impl IntoResponse {
    let snapshot = reader.snapshot();
    (StatusCode::OK, Json(snapshot.as_ref().clone())).into_response()
}

/// GET /metrics/timeseries — returns timeseries data from memory.
pub async fn metrics_timeseries_handler(
    Extension(reader): Extension<MetricsReader>,
) -> impl IntoResponse {
    let snapshot = reader.snapshot();
    (StatusCode::OK, Json(&snapshot.timeseries)).into_response()
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
                upstream_model: Some("gpt-4".into()),
                provider_type: "omlx".into(),
            }],
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"has_key\":true"));
        assert!(json.contains("\"upstream_model\":\"gpt-4\""));
        assert!(json.contains("\"type\":\"omlx\""));
    }

    #[test]
    fn provider_info_from_config_redacts_api_key() {
        let cfg = crate::config::ProviderConfig {
            endpoint: "http://localhost".into(),
            api_key: Some("sk-secret".into()),
            model: Some("gpt-4".into()),
            r#type: "openai".into(),
        };

        let info = provider_info_from_config("main".into(), Some(&cfg));
        let json = serde_json::to_string(&info).expect("should serialize");

        assert!(info.has_key);
        assert_eq!(info.api_key, None);
        assert_eq!(info.upstream_model, Some("gpt-4".into()));
        assert!(!json.contains("sk-secret"));
        assert!(!json.contains("api_key"));
    }

    #[test]
    fn provider_config_from_update_preserves_key_when_api_key_is_omitted() {
        let existing = crate::config::ProviderConfig {
            endpoint: "http://old".into(),
            api_key: Some("sk-existing".into()),
            model: Some("old-model".into()),
            r#type: "openai".into(),
        };
        let form = ProviderForm {
            name: "main".into(),
            endpoint: "http://new".into(),
            api_key: None,
            upstream_model: None,
            provider_type: "openai".into(),
        };

        let config = provider_config_from_form(form, Some(&existing));

        assert_eq!(config.endpoint, "http://new");
        assert_eq!(config.api_key.as_deref(), Some("sk-existing"));
        assert_eq!(config.model.as_deref(), Some("old-model"));
    }

    #[test]
    fn provider_config_from_update_clears_key_when_api_key_is_empty() {
        let existing = crate::config::ProviderConfig {
            endpoint: "http://old".into(),
            api_key: Some("sk-existing".into()),
            model: None,
            r#type: "openai".into(),
        };
        let form = ProviderForm {
            name: "main".into(),
            endpoint: "http://new".into(),
            api_key: Some(String::new()),
            upstream_model: None,
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
            upstream_model: None,
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
            upstream_model: None,
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
            upstream_model: None,
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
            upstream_model: None,
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
                upstream_model: None,
            }],
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"id\":\"gpt-4\""));
        assert!(json.contains("\"providers\":[\"openai\"]"));
    }

    #[test]
    fn unified_balance_entry_serializes() {
        use crate::provider::{
            BalanceStatus, ConfigSummary, Metric, MetricKind, MetricStatus, ProviderBalance,
        };
        let entry = UnifiedBalanceEntry {
            name: "glm".into(),
            provider_type: "cloud".into(),
            endpoint: "open.bigmodel.cn".into(),
            has_key: true,
            balance: Some(ProviderBalance {
                provider_name: "glm".into(),
                status: BalanceStatus::Online,
                plan: Some("plus".into()),
                plan_type: None,
                alerts: vec![],
                metrics: vec![Metric {
                    label: "Tokens".into(),
                    kind: MetricKind::Percentage,
                    value: 72.0,
                    total: Some(100.0),
                    unit: Some("%".into()),
                    percentage: Some(72.0),
                    status: MetricStatus::Ok,
                    reset_at_ms: None,
                }],
                breakdown: vec![],
                resets: vec![],
                config_summary: ConfigSummary {
                    provider_type: "cloud".into(),
                    endpoint: "open.bigmodel.cn".into(),
                    has_key: true,
                    model: None,
                },
            }),
            error: None,
        };

        let resp = BalanceResponse {
            balances: vec![entry],
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"provider_name\":\"glm\""));
        assert!(json.contains("\"metrics\""));
    }
}
