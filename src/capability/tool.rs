//! Tool calling types and abstractions.
//!
//! Defines `ToolCall`, `ToolDefinition`, and `ToolMode` (native vs. emulated)
//! for provider-agnostic tool calling support.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A completed tool call from the LLM.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCall {
    /// The name of the tool to call.
    pub name: String,
    /// JSON-encoded arguments for the tool.
    pub arguments: Value,
}

/// A tool definition provided to the LLM for discovery.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolDefinition {
    /// The name of the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub parameters: Value,
}

/// Tool calling mode for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    /// Provider natively supports tool calling (passes tool definitions directly).
    Native,
    /// Gateway emulates tool calling (parses LLM output into tool calls).
    Emulated,
}
