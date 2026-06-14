//! Model-to-provider router with priority ordering and fallback.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::capability::transform::RequestTransform;
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
            let transform = crate::capability::transform::build_transforms(
                caps.tool_calling,
                request.tools.clone(),
            );

            if transform.is_some() {
                allow_passthrough = false;
            }

            // Apply transform to request messages, if present
            if let Some(t) = &transform {
                // Find system prompt and transform it
                let system_idx = request
                    .messages
                    .iter()
                    .position(|m| m.role == crate::provider::Role::System);
                if let Some(idx) = system_idx {
                    request.messages[idx].content =
                        t.transform_prompt(&request.messages[idx].content);
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

                // Transform consumed the tools — remove them from the request
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

                    let s = if let Some(t) = &transform {
                        t.transform_stream(Box::pin(s))
                    } else {
                        Box::pin(s)
                    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderCapabilities, ProviderError};

    struct MockProvider {
        caps: ProviderCapabilities,
        name: &'static str,
    }

    impl MockProvider {
        fn new(name: &'static str, caps: ProviderCapabilities) -> Self {
            Self { caps, name }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_stream(
            &self,
            _request: UnifiedRequest,
            _allow_passthrough: bool,
        ) -> Result<crate::streaming::StreamMode, ProviderError> {
            Err(ProviderError::Internal("mock".to_string()))
        }
        fn capabilities(&self) -> ProviderCapabilities {
            self.caps
        }
        fn name(&self) -> &str {
            self.name
        }
    }

    fn router_with(
        provider_name: &str,
        provider: Box<dyn Provider>,
        models: &[&str],
    ) -> RealRouter {
        let mut providers = HashMap::new();
        providers.insert(provider_name.to_string(), provider);
        let mut routes = HashMap::new();
        for model in models {
            routes.insert(
                model.to_string(),
                RouteEntry {
                    provider_ids: vec![provider_name.to_string()],
                    upstream_models: HashMap::new(),
                },
            );
        }
        RealRouter { providers, routes }
    }

    #[test]
    fn resolve_known_model() {
        let router = router_with(
            "p1",
            Box::new(MockProvider::new(
                "p1",
                ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                },
            )),
            &["gpt-4"],
        );
        assert_eq!(router.resolve("gpt-4").unwrap(), "p1");
    }

    #[test]
    fn resolve_unknown_model_returns_error() {
        let router = router_with(
            "p1",
            Box::new(MockProvider::new(
                "p1",
                ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                },
            )),
            &["gpt-4"],
        );
        assert!(matches!(
            router.resolve("unknown"),
            Err(RouterError::ModelNotFound(_))
        ));
    }

    #[test]
    fn list_models_returns_configured_models() {
        let router = router_with(
            "p1",
            Box::new(MockProvider::new(
                "p1",
                ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                },
            )),
            &["gpt-4", "gpt-3.5-turbo"],
        );
        let mut models = router.list_models();
        models.sort();
        assert_eq!(
            models,
            vec!["gpt-3.5-turbo".to_string(), "gpt-4".to_string()]
        );
    }

    #[test]
    fn resolve_upstream_model_returns_none_when_no_override() {
        let router = router_with(
            "p1",
            Box::new(MockProvider::new(
                "p1",
                ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                },
            )),
            &["gpt-4"],
        );
        assert!(router.resolve_upstream_model("gpt-4").is_none());
    }

    #[test]
    fn resolve_upstream_model_returns_override() {
        let mut providers = HashMap::new();
        providers.insert(
            "p1".to_string(),
            Box::new(MockProvider::new(
                "p1",
                ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                },
            )) as Box<dyn Provider>,
        );
        let mut upstream = HashMap::new();
        upstream.insert("p1".to_string(), "upstream-model".to_string());
        let mut routes = HashMap::new();
        routes.insert(
            "my-model".to_string(),
            RouteEntry {
                provider_ids: vec!["p1".to_string()],
                upstream_models: upstream,
            },
        );
        let router = RealRouter { providers, routes };
        assert_eq!(
            router.resolve_upstream_model("my-model"),
            Some("upstream-model".to_string())
        );
    }
}
