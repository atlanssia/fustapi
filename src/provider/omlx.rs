//! omlx adapter — custom local inference protocol.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderError, UnifiedRequest};

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
            endpoint: "http://localhost:5000/v1".to_string(), // default to 5000 from previous codebase
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
    pub fn new(config: OmlxConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: "omlx".to_string(),
                model: config.model.clone(),
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

    fn name(&self) -> &str {
        "omlx"
    }
}
