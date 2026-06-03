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
    /// Upstream provider returned a client error (4xx) — passthrough to client.
    Upstream { status: u16, message: String },
    /// An internal router error occurred.
    Internal(String),
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouterError::ModelNotFound(model) => write!(f, "model not found: {model}"),
            RouterError::ProviderError(msg) => write!(f, "provider error: {msg}"),
            RouterError::Upstream { status, message } => {
                write!(f, "upstream error {status}: {message}")
            }
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
            ProviderError::Upstream { status, message } => {
                RouterError::Upstream { status, message }
            }
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

    /// Resolve the upstream model name for a given client model.
    fn resolve_upstream_model(&self, model: &str) -> Option<String>;

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
    routes: HashMap<String, RouteEntry>,
}

/// Internal route entry: provider list + per-provider upstream model overrides.
#[derive(Debug)]
struct RouteEntry {
    provider_ids: Vec<String>,
    upstream_models: HashMap<String, String>,
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
    #[must_use]
    pub fn from_config(config: &config::AppConfig) -> Self {
        let mut providers = HashMap::new();
        let mut routes = HashMap::new();

        // Create provider instances from config
        for (name, cfg) in &config.providers {
            let provider = config::create_provider(name, cfg);
            providers.insert(name.clone(), provider);
        }

        // Copy routes from config
        for (model, route_cfg) in &config.router {
            routes.insert(
                model.clone(),
                RouteEntry {
                    provider_ids: route_cfg.provider_ids.clone(),
                    upstream_models: route_cfg.upstream_models.clone(),
                },
            );
        }

        Self { providers, routes }
    }

    /// Get the provider instance for a model name.
    fn get_provider_for_model(&self, model: &str) -> Option<&dyn Provider> {
        self.routes
            .get(model)
            .and_then(|entry| {
                entry
                    .provider_ids
                    .first()
                    .and_then(|name| self.providers.get(name))
            })
            .map(|v| &**v)
    }
}

#[async_trait]
impl Router for RealRouter {
    fn resolve(&self, model: &str) -> Result<String, RouterError> {
        if let Some(entry) = self.routes.get(model)
            && let Some(first) = entry.provider_ids.first()
        {
            return Ok(first.clone());
        }
        Err(RouterError::ModelNotFound(model.to_string()))
    }
    fn resolve_upstream_model(&self, model: &str) -> Option<String> {
        if let Some(entry) = self.routes.get(model)
            && let Some(provider_id) = entry.provider_ids.first()
        {
            entry.upstream_models.get(provider_id).cloned()
        } else {
            None
        }
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

        // Inject per-provider upstream model override from route config
        if let Some(entry) = self.routes.get(&model_name)
            && let Some(provider_id) = entry.provider_ids.first()
            && let Some(upstream) = entry.upstream_models.get(provider_id)
        {
            request.model = upstream.clone();
        }

        if let Some(provider) = self.get_provider_for_model(&model_name) {
            let caps = provider.capabilities();
            let transforms = crate::capability::transform::build_transforms(
                caps.tool_calling,
                request.tools.clone(),
            );

            if crate::capability::transform::should_disable_passthrough(&transforms) {
                allow_passthrough = false;
            }

            // Apply transforms to request messages
            for t in &transforms {
                // Find system prompt and transform it
                let system_idx = request.messages.iter().position(|m| m.role == crate::provider::Role::System);
                if let Some(idx) = system_idx {
                    request.messages[idx].content = t.transform_prompt(&request.messages[idx].content);
                } else {
                    let prompt = t.transform_prompt("You are a helpful AI assistant.");
                    if prompt != "You are a helpful AI assistant." {
                        request.messages.insert(
                            0,
                            crate::provider::Message {
                                role: crate::provider::Role::System,
                                content: prompt,
                                images: None,
                                tool_calls: None,
                                tool_call_id: None,
                                extras: None,
                            },
                        );
                    }
                }
            }

            // Remove tools from request if transforms consumed them
            if !transforms.is_empty() {
                request.tools = None;
            }

            let stream_mode = provider.chat_stream(request, allow_passthrough).await?;

            match stream_mode {
                crate::streaming::StreamMode::Normalized(stream) => {
                    use tokio_stream::StreamExt;
                    let s = stream.map(|chunk_result| match chunk_result {
                        Ok(chunk) => Ok(chunk),
                        Err(e) => Err(crate::streaming::StreamError::Provider(e.to_string())),
                    });

                    let s = crate::capability::transform::apply_stream_transforms(
                        Box::pin(s),
                        &transforms,
                    );

                    return Ok(crate::streaming::StreamMode::Normalized(s));
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
    fn resolve_upstream_model(&self, model: &str) -> Option<String> {
        (**self).resolve_upstream_model(model)
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
