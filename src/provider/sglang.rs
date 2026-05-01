//! SGLang adapter — high-performance local streaming provider.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderError, UnifiedRequest};

/// SGLang provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SglConfig {
    /// The base URL of the SGLang server.
    pub endpoint: String,
    pub model: Option<String>,
}

impl Default for SglConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:3000/v1".to_string(),
            model: None,
        }
    }
}

/// SGLang provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SglProvider {
    config: SglConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
}

impl SglProvider {
    /// Create a new SGLang provider with the given config.
    pub fn new(config: SglConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: "sglang".to_string(),
                model: config.model.clone(),
            },
        );
        Self {
            config,
            openai_backend,
        }
    }

    /// Create a new SGLang provider with default config.
    pub fn default_provider() -> Self {
        Self::new(SglConfig::default())
    }
}

#[async_trait]
impl Provider for SglProvider {
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
        // SGLang provides a fast OpenAI-compatible endpoint.
        self.openai_backend
            .chat_stream(request, allow_passthrough)
            .await
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            tool_calling: crate::provider::ToolCallingSupport::Native,
            image_input: true,
            streaming: true,
        }
    }

    fn name(&self) -> &str {
        "sglang"
    }
}
