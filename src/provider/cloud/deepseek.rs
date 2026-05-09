//! DeepSeek cloud provider adapter.
//!
//! Fallback adapter for the DeepSeek API.

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{Provider, ProviderError, UnifiedRequest};

/// DeepSeek provider configuration.
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
            endpoint: "https://api.deepseek.com/v1".to_string(),
            api_key: String::new(),
            model: None,
        }
    }
}

/// DeepSeek provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DeepSeekProvider {
    config: DeepSeekConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
}

impl DeepSeekProvider {
    pub fn new(config: DeepSeekConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: config.api_key.clone(),
                model: config.model.clone(),
                stream_options: true,
            },
        );
        Self {
            config,
            openai_backend,
        }
    }
}

#[derive(Deserialize)]
struct DeepSeekBalanceResponse {
    is_available: bool,
    balance_infos: Vec<BalanceInfo>,
}

#[derive(Deserialize)]
struct BalanceInfo {
    currency: String,
    total_balance: String,
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

    async fn balance(&self) -> Result<Option<String>, ProviderError> {
        use tracing::debug;
        let client = reqwest::Client::new();
        let url = format!("{}/user/balance", self.config.endpoint.trim_end_matches('/'));

        debug!(url = %url, has_key = !self.config.api_key.is_empty(), "deepseek balance query");

        let mut builder = client
            .get(&url)
            .header("Accept", "application/json");

        if !self.config.api_key.is_empty() {
            builder = builder
                .header("Authorization", format!("Bearer {}", self.config.api_key));
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
                "balance query failed {}: {}",
                status, err_text
            )));
        }

        let resp_text = resp.text().await.unwrap_or_default();
        debug!(body = %resp_text, "deepseek balance raw response");

        let body: DeepSeekBalanceResponse = serde_json::from_str(&resp_text)
            .map_err(|e| ProviderError::Internal(e.to_string()))?;

        debug!(is_available = body.is_available, infos = body.balance_infos.len(), "deepseek balance parsed");

        if body.is_available
            && let Some(info) = body.balance_infos.first()
        {
            let balance = info.total_balance.parse::<f64>().unwrap_or(0.0);
            let status = if balance <= 0.0 { "Insufficient" } else { "Available" };
            return Ok(Some(format!(
                "{} {} ({})",
                info.currency, info.total_balance, status
            )));
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

    fn name(&self) -> &str {
        "deepseek"
    }
}
