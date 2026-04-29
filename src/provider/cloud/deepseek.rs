//! DeepSeek cloud provider adapter.
//!
//! Fallback adapter for the DeepSeek API.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::{Provider, ProviderError, UnifiedRequest};
use crate::streaming::LLMStream;
use futures::stream;

/// DeepSeek provider configuration.
#[derive(Debug, Clone)]
pub struct DeepSeekConfig {
    pub endpoint: String,
    pub api_key: String,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.deepseek.com".to_string(),
            api_key: String::new(),
        }
    }
}

/// DeepSeek provider implementation.
#[derive(Debug, Clone)]
pub struct DeepSeekProvider {
    config: DeepSeekConfig,
    client: reqwest::Client,
}

impl DeepSeekProvider {
    pub fn new(config: DeepSeekConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
    async fn chat_stream(&self, _request: UnifiedRequest) -> Result<LLMStream, ProviderError> {
        let s = stream::iter(vec![Ok(crate::streaming::LLMChunk {
            content: Some("deepseek response".to_string()),
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
        "deepseek"
    }
}
