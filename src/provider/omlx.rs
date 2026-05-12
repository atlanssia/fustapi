//! omlx adapter — custom local inference protocol.

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{
    BalanceStatus, ConfigSummary, Metric, MetricKind, MetricStatus, Provider, ProviderBalance,
    ProviderError, UnifiedRequest,
};

/// omlx provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OmlxConfig {
    /// The base URL of the omlx server.
    pub endpoint: String,
    pub model: Option<String>,
}

impl Default for OmlxConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8000/v1".to_string(),
            model: None,
        }
    }
}

/// omlx provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OmlxProvider {
    config: OmlxConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
}

impl OmlxProvider {
    /// Create a new omlx provider with the given config.
    #[must_use]
    pub fn new(config: OmlxConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: "omlx".to_string(),
                model: config.model.clone(),
                stream_options: false,
                provider_name: None,
                tool_calling: crate::provider::ToolCallingSupport::Emulated,
                image_input: true,
                streaming: true,
            },
        );
        Self {
            config,
            openai_backend,
        }
    }

    /// Create a new omlx provider with default config.
    #[must_use]
    pub fn default_provider() -> Self {
        Self::new(OmlxConfig::default())
    }
}

#[async_trait]
impl Provider for OmlxProvider {
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
        // We reuse the robust OpenAI streaming parser which handles tool aggregation and SSE flawlessly.
        // If omlx natively requires a different JSON schema on `/chat`, we fallback to its OpenAI-compatible endpoint.
        self.openai_backend
            .chat_stream(request, allow_passthrough)
            .await
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            // Assuming native for now, could be Emulated based on design logic
            tool_calling: crate::provider::ToolCallingSupport::Emulated,
            image_input: true,
            streaming: true,
        }
    }

    fn name(&self) -> &'static str {
        "omlx"
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        self.openai_backend.list_models().await
    }

    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        let client = reqwest::Client::new();
        // Health endpoint sits at root, not under /v1
        let url = self
            .config
            .endpoint
            .trim_end_matches("/v1")
            .trim_end_matches('/')
            .to_string()
            + "/health";

        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Request(format!(
                "health query failed {status}: {err_text}"
            )));
        }

        let body: OmlxHealthResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::Internal(e.to_string()))?;

        let is_healthy = matches!(body.status.as_str(), "healthy" | "ok" | "running" | "up" | "ready");
        let pool = &body.engine_pool;

        let mem_pct = if pool.max_model_memory > 0 {
            (pool.current_model_memory as f64 / pool.max_model_memory as f64 * 10000.0).round() / 100.0
        } else {
            0.0
        };

        let metrics = vec![
            Metric {
                label: "Models".to_string(),
                kind: MetricKind::Absolute,
                value: pool.model_count as f64,
                total: None,
                unit: Some("available".to_string()),
                percentage: None,
                status: MetricStatus::Ok,
                reset_at_ms: None,
            },
            Metric {
                label: "Loaded".to_string(),
                kind: MetricKind::Absolute,
                value: pool.loaded_count as f64,
                total: Some(pool.model_count as f64),
                unit: Some("models".to_string()),
                percentage: None,
                status: if pool.loaded_count == 0 && pool.model_count > 0 {
                    MetricStatus::Warn
                } else {
                    MetricStatus::Ok
                },
                reset_at_ms: None,
            },
            Metric {
                label: "VRAM".to_string(),
                kind: MetricKind::Percentage,
                value: mem_pct,
                total: Some(100.0),
                unit: Some("%".to_string()),
                percentage: Some(mem_pct),
                status: MetricStatus::from_percentage(mem_pct),
                reset_at_ms: None,
            },
        ];

        let mut alerts = Vec::new();
        if mem_pct >= 95.0 {
            alerts.push(crate::provider::Alert {
                level: crate::provider::AlertLevel::Critical,
                message: format!("VRAM usage {:.0}% — cannot load more models", mem_pct),
            });
        } else if mem_pct >= 80.0 {
            alerts.push(crate::provider::Alert {
                level: crate::provider::AlertLevel::Warn,
                message: format!("VRAM usage {:.0}% — approaching limit", mem_pct),
            });
        }

        if !is_healthy {
            alerts.push(crate::provider::Alert {
                level: crate::provider::AlertLevel::Critical,
                message: "Engine reports unhealthy status".to_string(),
            });
        }

        Ok(Some(ProviderBalance {
            provider_name: "omlx".to_string(),
            status: if is_healthy {
                BalanceStatus::Online
            } else {
                BalanceStatus::Error
            },
            plan: body.default_model.clone(),
            plan_type: None,
            alerts,
            metrics,
            breakdown: vec![],
            resets: vec![],
            config_summary: ConfigSummary {
                provider_type: "local".to_string(),
                endpoint: self.config.endpoint.clone(),
                has_key: false,
                model: self.config.model.clone().or(body.default_model),
            },
        }))
    }
}

// ── omlx health API response ──────────────────────────────────────────

#[derive(Deserialize)]
struct OmlxHealthResponse {
    status: String,
    default_model: Option<String>,
    engine_pool: EnginePool,
}

#[derive(Deserialize)]
struct EnginePool {
    model_count: u32,
    loaded_count: u32,
    max_model_memory: u64,
    current_model_memory: u64,
}
