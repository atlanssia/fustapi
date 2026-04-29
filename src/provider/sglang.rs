//! SGLang adapter — high-performance local streaming provider.

use async_trait::async_trait;
use futures::StreamExt;
use serde::Serialize;

use crate::provider::{Provider, ProviderError, UnifiedRequest};
use crate::streaming::{LLMStream, StreamError};

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
    client: reqwest::Client,
}

impl SglProvider {
    /// Create a new SGLang provider with the given config.
    pub fn new(config: SglConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Create a new SGLang provider with default config.
    pub fn default_provider() -> Self {
        Self::new(SglConfig::default())
    }

    /// Convert a UnifiedRequest to an SGLang (OpenAI-compatible) request.
    fn unified_to_sglang(&self, request: &UnifiedRequest) -> SglRequest {
        SglRequest {
            model: request.model.clone(),
            messages: request
                .messages
                .iter()
                .map(|m| self.message_to_sglang(m))
                .collect(),
            stream: Some(request.stream),
            temperature: request.temperature,
            max_tokens: request.max_tokens,
        }
    }

    fn message_to_sglang(&self, msg: &crate::provider::Message) -> SglMessage {
        SglMessage {
            role: match msg.role {
                crate::provider::Role::System => "system",
                crate::provider::Role::User => "user",
                crate::provider::Role::Assistant => "assistant",
                crate::provider::Role::Tool => "tool",
            },
            content: msg.content.clone(),
        }
    }
}

#[async_trait]
impl Provider for SglProvider {
    async fn chat_stream(&self, request: UnifiedRequest) -> Result<LLMStream, ProviderError> {
        let sglang_req = self.unified_to_sglang(&request);
        let url = format!("{}/v1/chat/completions", self.config.endpoint);

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&sglang_req)
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("SGLang error: {}", body)));
        }

        // Parse SSE stream from response body.
        // In production, we'd properly parse SSE format line by line.
        // For now, return a placeholder stream.
        let stream = resp.bytes_stream().map(|result| match result {
            Ok(_bytes) => Ok(crate::streaming::LLMChunk {
                content: Some("SGLang response".to_string()),
                tool_call: None,
                done: false,
            }),
            Err(e) => Err(StreamError::Provider(e.to_string())),
        });

        let s: LLMStream = Box::new(stream);
        Ok(s)
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

// ── SGLang Request Types (for serialization) ─────────────────────────

#[derive(Serialize)]
struct SglRequest {
    model: String,
    messages: Vec<SglMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct SglMessage {
    role: &'static str,
    content: String,
}
