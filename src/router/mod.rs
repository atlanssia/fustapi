//! Model-to-provider router with priority ordering and fallback.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::config;
use crate::provider::{Provider, ProviderError, UnifiedRequest};

pub type RouterStore = std::sync::Arc<arc_swap::ArcSwap<RealRouter>>;

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

impl From<ProviderError> for RouterError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::ModelNotFound(model) => RouterError::ModelNotFound(model),
            ProviderError::Connection(msg) => RouterError::ProviderError(msg),
            ProviderError::Request(msg) => RouterError::ProviderError(msg),
            ProviderError::Internal(msg) => RouterError::Internal(msg),
            ProviderError::Stream(msg) => RouterError::ProviderError(msg),
            ProviderError::Capability(_) | ProviderError::Api(_) => {
                RouterError::Internal("provider error".to_string())
            }
        }
    }
}

/// Trait for resolving model names to provider backends.
#[async_trait]
pub trait Router: Send + Sync {
    /// Resolve a model name to a provider name.
    fn resolve(&self, model: &str) -> Result<String, RouterError>;

    /// Get list of available models.
    fn list_models(&self) -> Vec<String>;

    /// Get list of configured providers.
    fn list_providers(&self) -> Vec<String>;

    /// Stream a chat completion through the selected provider.
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, RouterError>;
}

/// Real router that uses configured providers.
pub struct RealRouter {
    providers: HashMap<String, Box<dyn Provider>>,
    routes: HashMap<String, Vec<String>>,
}

impl std::fmt::Debug for RealRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealRouter")
            .field("provider_names", &self.providers.keys().collect::<Vec<_>>())
            .field("routes", &self.routes)
            .finish()
    }
}

impl RealRouter {
    /// Create a new real router from config.
    pub fn from_config(config: &config::AppConfig) -> Self {
        let mut providers = HashMap::new();
        let mut routes = HashMap::new();

        // Create provider instances from config
        for (name, cfg) in &config.providers {
            let provider = config::create_provider(name, cfg);
            providers.insert(name.clone(), provider);
        }

        // Copy routes from config
        for (model, provider_names) in &config.router {
            routes.insert(model.clone(), provider_names.clone());
        }

        Self { providers, routes }
    }

    /// Get the provider instance for a model name.
    fn get_provider_for_model(&self, model: &str) -> Option<&dyn Provider> {
        self.routes
            .get(model)
            .and_then(|provider_names| {
                provider_names
                    .first()
                    .and_then(|name| self.providers.get(name))
            })
            .map(|v| &**v)
    }
}

#[async_trait]
impl Router for RealRouter {
    fn resolve(&self, model: &str) -> Result<String, RouterError> {
        if let Some(provider_names) = self.routes.get(model)
            && let Some(first) = provider_names.first()
        {
            return Ok(first.clone());
        }
        Err(RouterError::ModelNotFound(model.to_string()))
    }
    fn list_models(&self) -> Vec<String> {
        self.routes.keys().cloned().collect()
    }
    fn list_providers(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
    async fn chat_stream(
        &self,
        mut request: UnifiedRequest,
        mut allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, RouterError> {
        let model_name = request.model.clone();
        if let Some(provider) = self.get_provider_for_model(&model_name) {
            let caps = provider.capabilities();
            let needs_emulation = caps.tool_calling
                == crate::provider::ToolCallingSupport::Emulated
                && request.tools.as_ref().is_some_and(|t| !t.is_empty());

            if needs_emulation {
                let tools = request.tools.take().unwrap();

                let mut system_msg_idx = None;
                for (i, msg) in request.messages.iter().enumerate() {
                    if msg.role == crate::provider::Role::System {
                        system_msg_idx = Some(i);
                        break;
                    }
                }

                if let Some(idx) = system_msg_idx {
                    request.messages[idx].content = crate::capability::tool::inject_tool_schemas(
                        &request.messages[idx].content,
                        &tools,
                    );
                } else {
                    request.messages.insert(
                        0,
                        crate::provider::Message {
                            role: crate::provider::Role::System,
                            content: crate::capability::tool::inject_tool_schemas(
                                "You are a helpful AI assistant.",
                                &tools,
                            ),
                            images: None,
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    );
                }

                allow_passthrough = false;
            }

            let stream_mode = provider.chat_stream(request, allow_passthrough).await?;

            match stream_mode {
                crate::streaming::StreamMode::Normalized(stream) => {
                    use tokio_stream::StreamExt;
                    let s = stream.map(|chunk_result| match chunk_result {
                        Ok(chunk) => Ok(chunk),
                        Err(e) => Err(crate::streaming::StreamError::Provider(e.to_string())),
                    });

                    if needs_emulation {
                        let emulated =
                            crate::capability::tool::ToolEmulationStream::new(Box::pin(s));
                        return Ok(crate::streaming::StreamMode::Normalized(Box::pin(emulated)));
                    }

                    return Ok(crate::streaming::StreamMode::Normalized(Box::pin(s)));
                }
                crate::streaming::StreamMode::Passthrough(byte_stream) => {
                    return Ok(crate::streaming::StreamMode::Passthrough(byte_stream));
                }
            }
        }
        Err(RouterError::ModelNotFound(model_name))
    }
}

/// Blanket impl for Arc<RealRouter>
#[async_trait]
impl Router for std::sync::Arc<RealRouter> {
    fn resolve(&self, model: &str) -> Result<String, RouterError> {
        (**self).resolve(model)
    }
    fn list_models(&self) -> Vec<String> {
        (**self).list_models()
    }
    fn list_providers(&self) -> Vec<String> {
        (**self).list_providers()
    }
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, RouterError> {
        (**self).chat_stream(request, allow_passthrough).await
    }
}
