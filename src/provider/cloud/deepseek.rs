//! DeepSeek cloud provider adapter.
//!
//! Fallback adapter for the DeepSeek API.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderError, UnifiedRequest};
use crate::streaming::LLMStream;

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
            },
        );
        Self {
            config,
            openai_backend,
        }
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
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
        "deepseek"
    }
}
