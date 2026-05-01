//! Provider trait and adapter registry.
//!
//! Defines the `Provider` trait that all adapters implement, along with
//! unified types like `UnifiedRequest`, `Message`, and error types.

use crate::capability::{ImageInput, ToolCall, ToolDefinition};
use crate::streaming::StreamMode;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Level of tool calling support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallingSupport {
    Native,
    Emulated,
    Unsupported,
}

/// Provider capabilities representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub tool_calling: ToolCallingSupport,
    pub image_input: bool,
    pub streaming: bool,
}

/// Provider-agnostic chat message.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    /// Message role.
    pub role: Role,
    /// Message content (text).
    pub content: String,
    /// Optional image attachments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
    /// Optional tool calls (for assistant messages with tool responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Chat message role.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System message.
    System,
    /// User message.
    User,
    /// Assistant message.
    Assistant,
    /// Tool result message.
    Tool,
}

/// Unified request sent to any provider, regardless of protocol format.
#[derive(Debug, Clone)]
pub struct UnifiedRequest {
    /// Model identifier.
    pub model: String,
    /// Chat messages.
    pub messages: Vec<Message>,
    /// Optional tool definitions.
    pub tools: Option<Vec<ToolDefinition>>,
    /// Whether to stream the response.
    pub stream: bool,
    /// Sampling temperature (0.0–2.0).
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
}

/// Provider trait — every adapter implements this.
///
/// This is the core abstraction that allows the router and protocol layers to be
/// provider-agnostic. Each provider (omlx, lmstudio, sglang, deepseek, openai) gets its own adapter.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stream a chat completion request to this provider.
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<StreamMode, ProviderError>;

    /// Get the provider's capabilities.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Human-readable provider name (e.g., "omlx", "lmstudio").
    fn name(&self) -> &str;

    /// Check if the provider is reachable/healthy.
    async fn health_check(&self) -> Result<(), ProviderError> {
        // Default: assume healthy unless overridden.
        Ok(())
    }
}

/// Errors that can occur when interacting with a provider.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Failed to connect to the provider endpoint.
    #[error("connection failed: {0}")]
    Connection(String),

    /// Provider returned an error response.
    #[error("request failed: {0}")]
    Request(String),

    /// The requested model is not available on this provider.
    #[error("model not found: {0}")]
    ModelNotFound(String),

    /// The requested capability (tool calling, images) is not supported.
    #[error("capability not supported: {0}")]
    Capability(String),

    /// Internal provider error (unexpected behavior).
    #[error("internal error: {0}")]
    Internal(String),

    /// Provider API error.
    #[error("api error: {0}")]
    Api(String),

    /// Stream error.
    #[error("stream error: {0}")]
    Stream(String),
}

// ── Provider module exports ───────────────────────────────────────────

pub mod cloud;
pub mod lmstudio;
pub mod omlx;
pub mod sglang;
