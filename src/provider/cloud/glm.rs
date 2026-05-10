//! GLM (`ZhipuAI` / `BigModel`) cloud provider adapter.
//!
//! Wraps the OpenAI-compatible chat API and adds balance/quota checking
//! via the dedicated monitoring endpoint.

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{Provider, ProviderError, UnifiedRequest};

/// GLM provider configuration.
#[derive(Debug, Clone)]
pub struct GlmConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: Option<String>,
}

/// GLM provider — OpenAI-compatible chat with balance monitoring.
#[derive(Debug, Clone)]
pub struct GlmProvider {
    config: GlmConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
}

impl GlmProvider {
    #[must_use]
    pub fn new(config: GlmConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: config.api_key.clone(),
                model: config.model.clone(),
                stream_options: false,
            },
        );
        Self {
            config,
            openai_backend,
        }
    }

    fn balance_url(&self) -> String {
        // The quota API is at a fixed path on the same host.
        // e.g. https://open.bigmodel.cn/api/coding/paas/v4
        //   -> https://open.bigmodel.cn/api/monitor/usage/quota/limit
        if let Some(pos) = self.config.endpoint.find("/api/") {
            let host = &self.config.endpoint[..pos];
            format!("{host}/api/monitor/usage/quota/limit")
        } else {
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit".to_string()
        }
    }
}

// ── Quota API response types ──────────────────────────────────────────

#[derive(Deserialize)]
struct GlmQuotaResponse {
    #[allow(dead_code)]
    code: Option<i32>,
    #[allow(dead_code)]
    success: Option<bool>,
    data: Option<QuotaData>,
}

#[derive(Deserialize)]
struct QuotaData {
    level: Option<String>,
    limits: Option<Vec<QuotaLimit>>,
}

#[derive(Deserialize)]
struct QuotaLimit {
    #[serde(rename = "type")]
    limit_type: Option<String>,
    percentage: Option<f64>,
    #[allow(dead_code)]
    usage: Option<f64>,
    #[serde(rename = "currentValue")]
    #[allow(dead_code)]
    current_value: Option<f64>,
    remaining: Option<f64>,
}

#[async_trait]
impl Provider for GlmProvider {
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
        self.openai_backend
            .chat_stream(request, allow_passthrough)
            .await
    }

    async fn balance(&self) -> Result<Option<String>, ProviderError> {
        use tracing::debug;
        let client = reqwest::Client::new();
        let url = self.balance_url();

        debug!(url = %url, has_key = !self.config.api_key.is_empty(), "glm balance query");

        let resp = client
            .get(&url)
            .header("Authorization", &self.config.api_key)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        debug!(status = %resp.status(), "glm balance response");

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            debug!(status = %status, err = %err_text, "glm balance failed");
            return Err(ProviderError::Request(format!(
                "balance query failed {status}: {err_text}"
            )));
        }

        let body: GlmQuotaResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::Internal(e.to_string()))?;

        if let Some(data) = body.data {
            let level = data.level.as_deref().unwrap_or("unknown");
            let limits = data.limits.as_deref().unwrap_or(&[]);

            let mut parts = vec![format!("Plan: {}", level)];

            for limit in limits {
                match limit.limit_type.as_deref() {
                    Some("TOKENS_LIMIT") => {
                        let pct = limit.percentage.unwrap_or(0.0);
                        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                        let remain = (limit.remaining.unwrap_or(0.0).max(0.0)) as u64;
                        if remain > 0 {
                            parts.push(format!("Tokens: {pct:.0}% used ({remain} left)"));
                        } else {
                            parts.push(format!("Tokens: {pct:.0}% used"));
                        }
                    }
                    Some("TIME_LIMIT") => {
                        let pct = limit.percentage.unwrap_or(0.0);
                        parts.push(format!("Time: {pct:.0}% used"));
                    }
                    _ => {}
                }
            }

            return Ok(Some(parts.join(" · ")));
        }

        Ok(None)
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            tool_calling: crate::provider::ToolCallingSupport::Native,
            image_input: false,
            streaming: true,
        }
    }

    fn name(&self) -> &'static str {
        "glm"
    }
}
