//! LM Studio adapter — OpenAI-compatible local provider.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderError, UnifiedRequest};

/// LM Studio provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct LmStudioConfig {
    /// The base URL of the LM Studio server.
    pub endpoint: String,
    pub api_key: String,
    pub model: Option<String>,
}

impl Default for LmStudioConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:1234/v1".to_string(),
            api_key: String::new(),
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
    #[must_use]
    pub fn new(config: LmStudioConfig) -> Self {
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

    /// Create a new LM Studio provider with default config.
    #[must_use]
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

    fn name(&self) -> &'static str {
        "lmstudio"
    }

    async fn balance(
        &self,
    ) -> Result<Option<crate::provider::ProviderBalance>, ProviderError> {
        self.openai_backend.balance().await
    }
}
