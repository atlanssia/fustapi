//! Health probing for OpenAI-compatible providers.
//!
//! Implements the balance/health-check logic extracted from `OpenAIProvider::balance()`.
//! Uses three strategies: /health endpoint (local only), /v1/models listing, and
//! rich JSON parsing for omlx-style engine pool responses.

use crate::provider::{
    BalanceStatus, ConfigSummary, Metric, MetricKind, MetricStatus, ProviderBalance, ProviderError,
};

use super::openai::OpenAIConfig;

/// Check if an endpoint points to a local address.
pub fn is_local(endpoint: &str) -> bool {
    endpoint.contains("localhost") || endpoint.contains("127.0.0.1") || endpoint.contains("::1")
}

/// Probe the health and balance of an OpenAI-compatible provider.
///
/// Returns structured balance information using three strategies:
/// 1. (local only) Try /health endpoint — omlx returns rich JSON with engine_pool,
///    vLLM/SGLang return empty 200.
/// 2. Try /v1/models for model listing (all endpoints).
/// 3. If nothing is reachable, return Offline status.
pub async fn probe_balance(
    client: &reqwest::Client,
    config: &OpenAIConfig,
    provider_name: &str,
    fetch_models: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<String>, ProviderError>> + Send>,
    >,
) -> Result<Option<ProviderBalance>, ProviderError> {
    let local = is_local(&config.endpoint);

    let base = config
        .endpoint
        .trim_end_matches("/v1")
        .trim_end_matches('/');

    // Strategy 1 (local only): Try /health endpoint
    let health_ok = if local {
        match client.get(format!("{base}/health")).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(text) if text.trim().is_empty() => Some((true, None)),
                Ok(text) => {
                    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
                    parsed.ok().map(|v| (true, Some(v)))
                }
                Err(_) => Some((true, None)),
            },
            Ok(_) => Some((false, None)),
            Err(_) => None,
        }
    } else {
        None
    };

    // Strategy 2: Try /v1/models for model listing (all endpoints)
    let need_models = health_ok.is_none() || health_ok.as_ref().is_some_and(|(ok, _)| !ok);
    let models_data: Option<Vec<String>> = if need_models {
        fetch_models.await.ok()
    } else {
        None
    };

    // Nothing reachable at all → offline
    if health_ok.is_none() && models_data.is_none() {
        return Ok(Some(ProviderBalance {
            provider_name: provider_name.to_string(),
            status: BalanceStatus::Offline,
            plan: None,
            plan_type: None,
            alerts: vec![],
            metrics: vec![],
            breakdown: vec![],
            resets: vec![],
            config_summary: ConfigSummary {
                provider_type: "local".to_string(),
                endpoint: config.endpoint.clone(),
                has_key: !config.api_key.is_empty(),
                model: config.model.clone(),
            },
        }));
    }

    // Build result from available data
    let mut status = BalanceStatus::Online;
    let mut metrics = Vec::new();
    let mut detected_model: Option<String> = None;

    if let Some((_is_healthy, Some(json_body))) = &health_ok {
        // Rich JSON health response — try structured parsing first
        let has_engine_pool = json_body.get("engine_pool").is_some();
        if has_engine_pool {
            // Normalize the JSON so parse_omlx_balance can handle both
            // omlx (final_ceiling) and openai-compatible (max_model_memory) formats
            let mut normalized = json_body.clone();
            if let Some(pool) = normalized.get_mut("engine_pool")
                && pool.get("final_ceiling").is_none()
                && let Some(max_mem) = pool.get("max_model_memory").cloned()
            {
                pool.as_object_mut()
                    .map(|m| m.insert("final_ceiling".to_string(), max_mem));
            }
            if let Ok(balance) = crate::provider::health::parse_omlx_balance(
                &normalized.to_string(),
                &config.endpoint,
                config.model.as_deref(),
            ) {
                return Ok(Some(ProviderBalance {
                    provider_name: provider_name.to_string(),
                    ..balance
                }));
            }
        }

        // Fallback: lightweight status extraction for non-engine_pool responses
        let status_str = json_body
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let healthy = matches!(
            status_str,
            "healthy" | "ok" | "running" | "up" | "ready" | ""
        );
        if !healthy && !status_str.is_empty() {
            status = BalanceStatus::Error;
        }
        detected_model = json_body
            .get("default_model")
            .and_then(|v| v.as_str())
            .map(String::from);
    }

    // Fallback: use /v1/models data if no rich health info
    if metrics.is_empty()
        && let Some(model_ids) = &models_data
    {
        metrics.push(Metric {
            label: "Models".to_string(),
            kind: MetricKind::Absolute,
            value: model_ids.len() as f64,
            total: None,
            unit: Some("loaded".to_string()),
            percentage: None,
            status: if model_ids.is_empty() {
                MetricStatus::Warn
            } else {
                MetricStatus::Ok
            },
            reset_at_ms: None,
        });
        if detected_model.is_none() {
            detected_model = model_ids.first().cloned();
        }
    }

    Ok(Some(ProviderBalance {
        provider_name: "local".to_string(),
        status,
        plan: detected_model.clone(),
        plan_type: None,
        alerts: vec![],
        metrics,
        breakdown: vec![],
        resets: vec![],
        config_summary: ConfigSummary {
            provider_type: "local".to_string(),
            endpoint: config.endpoint.clone(),
            has_key: !config.api_key.is_empty(),
            model: config.model.clone().or(detected_model),
        },
    }))
}
