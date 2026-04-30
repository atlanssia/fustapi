//! SGLang adapter — high-performance local streaming provider.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderError, UnifiedRequest};
use crate::streaming::LLMStream;

/// SGLang provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SglConfig {
    /// The base URL of the SGLang server.
    pub endpoint: String,
}

impl Default for SglConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:3000".to_string(),
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
            }
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
    async fn chat_stream(&self, request: UnifiedRequest) -> Result<LLMStream, ProviderError> {
        self.openai_backend.chat_stream(request).await
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_images(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sglang"
    }
}
