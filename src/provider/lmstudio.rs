//! LM Studio adapter — OpenAI-compatible local provider.

use async_trait::async_trait;
use futures::StreamExt;
use serde::Serialize;

use crate::provider::{Provider, ProviderError, UnifiedRequest};
use crate::streaming::{LLMStream, StreamError};

/// LM Studio provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct LmStudioConfig {
    /// The base URL of the LM Studio server.
    pub endpoint: String,
}

impl Default for LmStudioConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:1234".to_string(),
        }
    }
}

/// LM Studio provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct LmStudioProvider {
    config: LmStudioConfig,
    client: reqwest::Client,
}

impl LmStudioProvider {
    /// Create a new LM Studio provider with the given config.
    pub fn new(config: LmStudioConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Create a new LM Studio provider with default config.
    pub fn default_provider() -> Self {
        Self::new(LmStudioConfig::default())
    }

    /// Convert a UnifiedRequest to an LM Studio (OpenAI-compatible) request.
    fn unified_to_openai(&self, request: &UnifiedRequest) -> LmStudioRequest {
        LmStudioRequest {
            model: request.model.clone(),
            messages: request
                .messages
                .iter()
                .map(|m| self.message_to_openai(m))
                .collect(),
            stream: Some(request.stream),
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            tools: request
                .tools
                .as_ref()
                .map(|tools| tools.iter().map(|t| self.tool_def_to_openai(t)).collect()),
        }
    }

    fn message_to_openai(&self, msg: &crate::provider::Message) -> LmStudioMessage {
        LmStudioMessage {
            role: match msg.role {
                crate::provider::Role::System => "system",
                crate::provider::Role::User => "user",
                crate::provider::Role::Assistant => "assistant",
                crate::provider::Role::Tool => "tool",
            },
            content: Some(msg.content.clone()),
        }
    }

    fn tool_def_to_openai(&self, tool: &crate::capability::ToolDefinition) -> LmStudioTool {
        LmStudioTool {
            r#type: "function".to_string(),
            function: LmStudioFunctionTool {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        }
    }
}

#[async_trait]
impl Provider for LmStudioProvider {
    async fn chat_stream(&self, request: UnifiedRequest) -> Result<LLMStream, ProviderError> {
        let openai_req = self.unified_to_openai(&request);
        let url = format!("{}/v1/chat/completions", self.config.endpoint);

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&openai_req)
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("LM Studio error: {}", body)));
        }

        // Parse SSE stream from response body.
        // In production, we'd properly parse SSE format line by line.
        // For now, return a placeholder stream that can be replaced with real parsing.
        let stream = resp.bytes_stream().map(|result| match result {
            Ok(_bytes) => Ok(crate::streaming::LLMChunk {
                content: Some("LM Studio response".to_string()),
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
        "lmstudio"
    }
}

// ── LM Studio Request Types (for serialization) ──────────────────────

#[derive(Serialize)]
struct LmStudioRequest {
    model: String,
    messages: Vec<LmStudioMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<LmStudioTool>>,
}

#[derive(Serialize)]
struct LmStudioMessage {
    role: &'static str,
    content: Option<String>,
}

#[derive(Serialize)]
struct LmStudioTool {
    #[serde(rename = "type")]
    r#type: String,
    function: LmStudioFunctionTool,
}

#[derive(Serialize)]
struct LmStudioFunctionTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
}
