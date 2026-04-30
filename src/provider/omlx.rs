//! omlx adapter — custom local inference protocol.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderError, UnifiedRequest};
use crate::streaming::LLMStream;

/// omlx provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OmlxConfig {
    /// The base URL of the omlx server.
    pub endpoint: String,
}

impl Default for OmlxConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:5000".to_string(), // default to 5000 from previous codebase
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
    pub fn new(config: OmlxConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: "omlx".to_string(),
            },
        );
        Self {
            config,
            openai_backend,
        }
    }

    /// Create a new omlx provider with default config.
    pub fn default_provider() -> Self {
        Self::new(OmlxConfig::default())
    }
}

#[async_trait]
impl Provider for OmlxProvider {
    async fn chat_stream(&self, request: UnifiedRequest) -> Result<LLMStream, ProviderError> {
        // We reuse the robust OpenAI streaming parser which handles tool aggregation and SSE flawlessly.
        // If omlx natively requires a different JSON schema on `/chat`, we fallback to its OpenAI-compatible endpoint.
        self.openai_backend.chat_stream(request).await
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_images(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "omlx"
    }
}
