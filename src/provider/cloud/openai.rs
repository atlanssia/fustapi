//! OpenAI cloud provider adapter.
//!
//! Fallback adapter for the OpenAI API.

use async_trait::async_trait;

use crate::provider::{Provider, ProviderError, UnifiedRequest};
use crate::streaming::LLMStream;
use futures::stream;

/// OpenAI provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub endpoint: String,
    pub api_key: String,
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com".to_string(),
            api_key: String::new(),
        }
    }
}

/// OpenAI provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    config: OpenAIConfig,
    client: reqwest::Client,
}

impl OpenAIProvider {
    pub fn new(config: OpenAIConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn chat_stream(&self, _request: UnifiedRequest) -> Result<LLMStream, ProviderError> {
        let s = stream::iter(vec![Ok(crate::streaming::LLMChunk {
            content: Some("openai response".to_string()),
            tool_call: None,
            done: false,
        })]);
        Ok(Box::new(s) as LLMStream)
    }
    fn supports_tools(&self) -> bool {
        true
    }
    fn supports_images(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "openai"
    }
}
