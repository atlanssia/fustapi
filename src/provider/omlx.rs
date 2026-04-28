//! omlx adapter — custom local inference protocol.

use async_trait::async_trait;

use crate::provider::{Provider, UnifiedRequest, ProviderError};
use crate::streaming::LLMStream;

/// omlx provider configuration.
#[derive(Debug, Clone)]
pub struct OmlxConfig {
    /// The base URL of the omlx server.
    pub endpoint: String,
}

impl Default for OmlxConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:5000".to_string(),
        }
    }
}

/// omlx provider implementation.
#[derive(Debug, Clone)]
pub struct OmlxProvider {
    config: OmlxConfig,
    client: reqwest::Client,
}

impl OmlxProvider {
    /// Create a new omlx provider with the given config.
    pub fn new(config: OmlxConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Create a new omlx provider with default config.
    pub fn default_provider() -> Self {
        Self::new(OmlxConfig::default())
    }
}

#[async_trait]
impl Provider for OmlxProvider {
    async fn chat_stream(&self, _request: UnifiedRequest) -> Result<LLMStream, ProviderError> {
        let url = format!("{}/chat", self.config.endpoint);

        // In production, we'd send the request and parse the omlx-specific response.
        // For now, return a placeholder stream.
        Ok(Box::pin(tokio_stream::iter(vec![
            Ok(crate::streaming::LLMChunk {
                content: Some("omlx response".to_string()),
                tool_call: None,
                done: false,
            }),
            Ok(crate::streaming::LLMChunk {
                content: None,
                tool_call: None,
                done: true,
            }),
        ])))
    }

    fn supports_tools(&self) -> bool { true }

    fn supports_images(&self) -> bool { true }

    fn name(&self) -> &str { "omlx" }
}
