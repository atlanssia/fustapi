//! Model-to-provider router with priority ordering and fallback.

use async_trait::async_trait;

use crate::provider::UnifiedRequest;
use crate::streaming::LLMStream;

/// Error type for router operations.
#[derive(Debug)]
pub enum RouterError {
    /// The requested model is not available.
    ModelNotFound(String),
    /// The selected provider returned an error.
    ProviderError(String),
    /// An internal router error occurred.
    Internal(String),
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouterError::ModelNotFound(model) => write!(f, "model not found: {model}"),
            RouterError::ProviderError(msg) => write!(f, "provider error: {msg}"),
            RouterError::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for RouterError {}

/// Trait for resolving model names to provider backends.
#[async_trait]
pub trait Router: Send + Sync {
    /// Resolve a model name to a provider name.
    fn resolve(&self, model: &str) -> Result<String, RouterError>;

    /// Get list of available models.
    fn list_models(&self) -> Vec<String>;

    /// Stream a chat completion through the selected provider.
    async fn chat_stream(&self, request: UnifiedRequest) -> Result<LLMStream, RouterError>;
}

/// Mock router that returns canned responses for testing.
#[derive(Debug, Clone)]
pub struct MockRouter {
    models: Vec<String>,
}

impl MockRouter {
    /// Create a new mock router with default models.
    pub fn new() -> Self {
        Self {
            models: vec![
                "fustapi-mock".to_string(),
                "gpt-4".to_string(),
                "claude-3".to_string(),
            ],
        }
    }
}

impl Default for MockRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Router for MockRouter {
    fn resolve(&self, model: &str) -> Result<String, RouterError> {
        if self.models.iter().any(|m| m == model) {
            Ok(model.to_string())
        } else {
            Err(RouterError::ModelNotFound(model.to_string()))
        }
    }

    fn list_models(&self) -> Vec<String> {
        self.models.clone()
    }

    async fn chat_stream(&self, _request: UnifiedRequest) -> Result<LLMStream, RouterError> {
        use futures::stream;
        let chunks = vec![
            Ok(crate::streaming::LLMChunk {
                content: Some("Hello from FustAPI mock router!".to_string()),
                tool_call: None,
                done: false,
            }),
            Ok(crate::streaming::LLMChunk {
                content: None,
                tool_call: None,
                done: true,
            }),
        ];
        // Box the stream as a trait object for dynamic dispatch.
        let s: LLMStream = Box::new(stream::iter(chunks));
        Ok(s)
    }
}
