//! Streaming engine types and utilities.
//!
//! Defines `LLMChunk`, `LLMStream`, and `StreamError` — the core types for the
//! streaming pipeline: Provider → Normalize → Forward (SSE).

use serde::{Deserialize, Serialize};
use tokio_stream::Stream;

use crate::capability::ToolCall;

/// A single chunk from an LLM streaming response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LLMChunk {
    /// The text content delta (may be empty for tool calls).
    #[serde(default)]
    pub content: Option<String>,
    /// Tool call if this chunk contains one.
    #[serde(default)]
    pub tool_call: Option<ToolCall>,
    /// Whether this is the final chunk.
    #[serde(default)]
    pub done: bool,
}

/// Type alias for the standard LLM stream.
/// Every provider adapter returns this type.
pub type LLMStream = Box<dyn Stream<Item = Result<LLMChunk, StreamError>> + Send + Unpin>;

/// Errors that can occur during streaming.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// Provider returned an error.
    #[error("provider error: {0}")]
    Provider(String),
    /// Connection error (network, timeout, etc.).
    #[error("connection error: {0}")]
    Connection(String),
    /// Failed to parse provider response.
    #[error("parse error: {0}")]
    Parse(String),
}
