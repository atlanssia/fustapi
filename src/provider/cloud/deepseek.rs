//! `DeepSeek` cloud provider adapter.
//!
//! Fallback adapter for the `DeepSeek` API.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderBalance, ProviderError, UnifiedRequest};

/// `DeepSeek` provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DeepSeekConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: Option<String>,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.deepseek.com".to_string(),
            api_key: String::new(),
            model: None,
        }
    }
}

/// `DeepSeek` provider implementation.
#[allow(dead_code)]
#[derive(Debug)]
pub struct DeepSeekProvider {
    config: DeepSeekConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
    http_client: reqwest::Client,
}

impl DeepSeekProvider {
    #[must_use]
    pub fn new(config: DeepSeekConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: config.api_key.clone(),
                model: config.model.clone(),
                stream_options: true,
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
}

#[async_trait]
impl Provider for DeepSeekProvider {
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
        self.openai_backend.list_models().await
    }

    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        use tracing::debug;
        let url = format!(
            "{}/user/balance",
            self.config.endpoint.trim_end_matches('/')
        );

        debug!(url = %url, has_key = !self.config.api_key.is_empty(), "deepseek balance query");

        let mut builder = self
            .http_client
            .get(&url)
            .header("Accept", "application/json");

        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        debug!(status = %resp.status(), "deepseek balance response");

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            debug!(status = %status, err = %err_text, "deepseek balance failed");
            return Err(ProviderError::Request(format!(
                "balance query failed {status}: {err_text}"
            )));
        }

        let body = resp.text().await.unwrap_or_default();
        debug!(body = %body, "deepseek balance raw response");

        let has_key = !self.config.api_key.is_empty();
        let balance =
            crate::provider::health::parse_deepseek_balance(&body, &self.config.endpoint, has_key)
                .map_err(ProviderError::Internal)?;

        Ok(Some(balance))
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            tool_calling: crate::provider::ToolCallingSupport::Native,
            image_input: false,
            streaming: true,
        }
    }

    fn name(&self) -> &'static str {
        "DeepSeek"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn deepseek_balance_builds_credit_structure() {
        let body =
            r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"1.60"}]}"#;
        let result =
            crate::provider::health::parse_deepseek_balance(body, "api.deepseek.com", true)
                .expect("parse should succeed");

        assert_eq!(result.provider_name, "DeepSeek");
        assert_eq!(result.status, crate::provider::BalanceStatus::Online);
        assert_eq!(result.plan_type, Some(crate::provider::PlanType::Credit));
        assert_eq!(result.metrics.len(), 1);
        assert_eq!(result.metrics[0].label, "Balance");
        assert!((result.metrics[0].value - 1.60).abs() < 0.01);
        assert_eq!(result.metrics[0].unit.as_deref(), Some("CNY"));
        assert!(result.alerts.is_empty());
    }

    #[test]
    fn deepseek_balance_alerts_on_zero() {
        let body =
            r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"0.00"}]}"#;
        let result =
            crate::provider::health::parse_deepseek_balance(body, "api.deepseek.com", true)
                .expect("parse should succeed");
        assert_eq!(
            result.metrics[0].status,
            crate::provider::MetricStatus::Critical
        );
        assert!(
            result
                .alerts
                .iter()
                .any(|a| a.level == crate::provider::AlertLevel::Critical)
        );
    }
}
