//! omlx adapter — custom local inference protocol.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderBalance, ProviderError, UnifiedRequest};

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
#[derive(Debug)]
pub struct OmlxProvider {
    config: OmlxConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
    http_client: reqwest::Client,
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
            http_client: crate::provider::build_http_client(),
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
        "oMLX"
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        self.openai_backend.list_models().await
    }

    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        let url = self
            .config
            .endpoint
            .trim_end_matches("/v1")
            .trim_end_matches('/')
            .to_string()
            + "/health";

        let resp = self
            .http_client
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

        let body = resp
            .text()
            .await
            .map_err(|e| ProviderError::Internal(e.to_string()))?;

        let balance = crate::provider::health::parse_omlx_balance(
            &body,
            &self.config.endpoint,
            self.config.model.as_deref(),
        )
        .map_err(ProviderError::Internal)?;

        Ok(Some(balance))
    }
}
