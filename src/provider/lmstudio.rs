//! LM Studio adapter — OpenAI-compatible local provider.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderError, UnifiedRequest};

/// LM Studio provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct LmStudioConfig {
    /// The base URL of the LM Studio server.
    pub endpoint: String,
    pub model: Option<String>,
}

impl Default for LmStudioConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:1234/v1".to_string(),
            model: None,
        }
    }
}

/// LM Studio provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct LmStudioProvider {
    config: LmStudioConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
}

impl LmStudioProvider {
    /// Create a new LM Studio provider with the given config.
    pub fn new(config: LmStudioConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: "lm-studio".to_string(),
                model: config.model.clone(),
            },
        );
        Self {
            config,
            openai_backend,
        }
    }

    /// Create a new LM Studio provider with default config.
    pub fn default_provider() -> Self {
        Self::new(LmStudioConfig::default())
    }
}

#[async_trait]
impl Provider for LmStudioProvider {
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
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
        "lmstudio"
    }
}
