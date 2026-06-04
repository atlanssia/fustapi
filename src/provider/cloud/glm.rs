//! GLM (`ZhipuAI` / `BigModel`) cloud provider adapter.
//!
//! Wraps the OpenAI-compatible chat API and adds balance/quota checking
//! via the dedicated monitoring endpoint.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderBalance, ProviderError, UnifiedRequest};

/// GLM provider configuration.
#[derive(Debug, Clone)]
pub struct GlmConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: Option<String>,
}

/// GLM provider — OpenAI-compatible chat with balance monitoring.
#[derive(Debug)]
pub struct GlmProvider {
    config: GlmConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
    http_client: reqwest::Client,
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
                provider_name: None,
                tool_calling: crate::provider::ToolCallingSupport::Native,
                image_input: false,
                streaming: true,
            },
        );
        Self {
            config,
            openai_backend,
            http_client: crate::provider::build_http_client(),
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

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        // GLM uses {endpoint}/models, not the OpenAI-style {base}/v1/models.
        let base = self.config.endpoint.trim_end_matches('/');
        let url = format!("{base}/models");
        let mut req = self.openai_backend.client().get(&url);
        if !self.config.api_key.is_empty() {
            req = req.header("Authorization", &self.config.api_key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Connection(format!(
                "GLM models endpoint returned {}",
                resp.status()
            )));
        }

        resp.json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| {
                v.get("data")?.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id")?.as_str().map(String::from))
                        .collect()
                })
            })
            .ok_or_else(|| ProviderError::Internal("Failed to parse GLM models response".into()))
    }

    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        use tracing::debug;
        let url = self.balance_url();

        debug!(url = %url, has_key = !self.config.api_key.is_empty(), "glm balance query");

        let resp = self
            .http_client
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

        let body = resp.text().await.unwrap_or_default();

        let has_key = !self.config.api_key.is_empty();
        crate::provider::health::parse_glm_balance(
            &body,
            &self.config.endpoint,
            has_key,
            self.config.model.as_deref(),
        )
        .map_err(ProviderError::Internal)
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            tool_calling: crate::provider::ToolCallingSupport::Native,
            image_input: false,
            streaming: true,
        }
    }

    fn name(&self) -> &'static str {
        "GLM"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn glm_balance_builds_correct_structure() {
        let body = r#"{"data":{"level":"plus","limits":[{"type":"TOKENS_LIMIT","percentage":72.0,"remaining":28.0,"usage":100.0,"currentValue":72.0,"unit":3,"number":5,"nextResetTime":1778499600000,"usageDetails":[{"modelCode":"glm-4","usage":1240},{"modelCode":"coder-1","usage":580}]},{"type":"TIME_LIMIT","percentage":45.0}]}}"#;
        let result = crate::provider::health::parse_glm_balance(
            body,
            "https://open.bigmodel.cn/api/coding/paas/v4",
            true,
            Some("glm-4-plus"),
        )
        .expect("parse should succeed")
        .expect("should return Some");

        assert_eq!(result.provider_name, "GLM");
        assert_eq!(result.status, crate::provider::BalanceStatus::Online);
        assert_eq!(result.plan.as_deref(), Some("plus"));
        assert_eq!(result.plan_type, Some(crate::provider::PlanType::Coding));
        assert_eq!(result.metrics.len(), 2);
        assert_eq!(result.metrics[0].label, "Tokens");
        assert_eq!(result.metrics[0].percentage, Some(72.0));
        assert_eq!(result.metrics[0].status, crate::provider::MetricStatus::Ok);
        assert_eq!(result.metrics[0].reset_at_ms, Some(1778499600000));
        assert_eq!(result.metrics[1].label, "MCP");
        assert_eq!(result.breakdown.len(), 2);
        assert_eq!(result.resets.len(), 1);
        assert!(result.config_summary.has_key);
    }

    #[test]
    fn glm_balance_alerts_on_high_usage() {
        let body = r#"{"data":{"level":"plus","limits":[{"type":"TOKENS_LIMIT","percentage":85.0,"remaining":15.0,"usage":100.0,"currentValue":85.0}]}}"#;
        let result = crate::provider::health::parse_glm_balance(
            body,
            "https://open.bigmodel.cn/api/coding/paas/v4",
            true,
            None,
        )
        .expect("parse should succeed")
        .expect("should return Some");
        assert!(result.alerts.iter().any(|a| a.message.contains("85")));
    }
}
