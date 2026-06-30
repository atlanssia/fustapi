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

    /// Look up a provider instance by name.
    ///
    /// Used by Responses passthrough to read the provider's capabilities
    /// (e.g., `supports_responses`) before dispatching. The default impl
    /// returns `None` for mock/test routers; production routers override it.
    fn get_provider(&self, _name: &str) -> Option<&dyn crate::provider::Provider> {
        None
    }

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

/// Strip the `[1m]` suffix that Claude Code appends to model names when
/// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` is configured for 1M context.
pub(crate) fn normalize_model_name(model: &str) -> &str {
    model.strip_suffix("[1m]").unwrap_or(model)
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
            .get(normalize_model_name(model))
            .and_then(|entry| {
                entry
                    .provider_ids
                    .first()
                    .and_then(|name| self.providers.get(name))
            })
            .map(|v| &**v)
    }

    /// Look up a provider instance by its configured name.
    ///
    /// Enables handlers (e.g., Responses passthrough) to read provider
    /// capabilities directly without re-resolving a model→provider route.
    pub fn get_provider(&self, name: &str) -> Option<&dyn Provider> {
        self.providers.get(name).map(|p| &**p)
    }
}

#[async_trait]
impl Router for RealRouter {
    fn resolve(&self, model: &str) -> Result<String, RouterError> {
        let model = normalize_model_name(model);
        if let Some(entry) = self.routes.get(model)
            && let Some(first) = entry.provider_ids.first()
        {
            return Ok(first.clone());
        }
        Err(RouterError::ModelNotFound(model.to_string()))
    }
    fn resolve_upstream_model(&self, model: &str) -> Option<String> {
        let model = normalize_model_name(model);
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
    fn get_provider(&self, name: &str) -> Option<&dyn crate::provider::Provider> {
        // Delegate to the inherent method so trait callers (`&dyn Router`)
        // resolve to the production lookup rather than the default `None`.
        RealRouter::get_provider(self, name)
    }
    async fn chat_stream(
        &self,
        mut request: UnifiedRequest,
        mut allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, RouterError> {
        let model_name = normalize_model_name(&request.model).to_string();
        request.model.clone_from(&model_name);

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
                        // Forward the stream error unchanged. Re-wrapping it as
                        // StreamError::Provider collapses the variant
                        // (Connection/Parse/Provider) and double-prefixes the
                        // Display ("provider error: provider error: …"), which
                        // is what surfaced as the noisy stream chunk error log.
                        Err(e) => Err(e),
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
                crate::streaming::StreamMode::NonStreaming(json) => {
                    return Ok(crate::streaming::StreamMode::NonStreaming(json));
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
    fn get_provider(&self, name: &str) -> Option<&dyn crate::provider::Provider> {
        (**self).get_provider(name)
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
                    supports_responses: false,
                    supports_anthropic: false,
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
                    supports_responses: false,
                    supports_anthropic: false,
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
                    supports_responses: false,
                    supports_anthropic: false,
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
                    supports_responses: false,
                    supports_anthropic: false,
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
                    supports_responses: false,
                    supports_anthropic: false,
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

    // ── chat_stream characterization tests ───────────────────────────────

    /// A mock provider that captures the request and returns a configurable stream.
    /// This allows verifying that the router's capability orchestration modifies
    /// the request correctly before it reaches the provider.
    use std::sync::{Arc, Mutex};

    struct CapturingMockProvider {
        caps: ProviderCapabilities,
        name: &'static str,
        captured: Arc<Mutex<Option<UnifiedRequest>>>,
        stream_chunks: Vec<crate::streaming::LLMChunk>,
    }

    impl CapturingMockProvider {
        fn new_emulated(
            captured: Arc<Mutex<Option<UnifiedRequest>>>,
            stream_chunks: Vec<crate::streaming::LLMChunk>,
        ) -> Self {
            Self {
                caps: ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Emulated,
                    image_input: false,
                    streaming: true,
                    supports_responses: false,
                    supports_anthropic: false,
                },
                name: "emulated-mock",
                captured,
                stream_chunks,
            }
        }

        fn new_native(
            captured: Arc<Mutex<Option<UnifiedRequest>>>,
            stream_chunks: Vec<crate::streaming::LLMChunk>,
        ) -> Self {
            Self {
                caps: ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                    supports_responses: false,
                    supports_anthropic: false,
                },
                name: "native-mock",
                captured,
                stream_chunks,
            }
        }
    }

    #[async_trait]
    impl Provider for CapturingMockProvider {
        async fn chat_stream(
            &self,
            request: UnifiedRequest,
            _allow_passthrough: bool,
        ) -> Result<crate::streaming::StreamMode, ProviderError> {
            *self.captured.lock().unwrap() = Some(request);
            let chunks: Vec<_> = self.stream_chunks.iter().map(|c| Ok(c.clone())).collect();
            Ok(crate::streaming::StreamMode::Normalized(Box::pin(
                tokio_stream::iter(chunks),
            )))
        }
        fn capabilities(&self) -> ProviderCapabilities {
            self.caps
        }
        fn name(&self) -> &str {
            self.name
        }
    }

    /// A mock provider that emits a single `StreamError` in its Normalized
    /// stream. Used to verify the router forwards stream errors unchanged
    /// rather than re-wrapping them (which collapses the variant and
    /// double-prefixes the Display).
    struct ErrorStreamMock {
        caps: ProviderCapabilities,
    }

    impl ErrorStreamMock {
        fn new() -> Self {
            Self {
                caps: ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                    supports_responses: false,
                    supports_anthropic: false,
                },
            }
        }
    }

    #[async_trait]
    impl Provider for ErrorStreamMock {
        async fn chat_stream(
            &self,
            _request: UnifiedRequest,
            _allow_passthrough: bool,
        ) -> Result<crate::streaming::StreamMode, ProviderError> {
            let item: Result<crate::streaming::LLMChunk, crate::streaming::StreamError> = Err(
                crate::streaming::StreamError::Connection("upstream dropped".to_string()),
            );
            Ok(crate::streaming::StreamMode::Normalized(Box::pin(
                tokio_stream::iter(std::iter::once(item)),
            )))
        }
        fn capabilities(&self) -> ProviderCapabilities {
            self.caps
        }
        fn name(&self) -> &str {
            "error-mock"
        }
    }

    fn make_tools() -> Vec<crate::capability::ToolDefinition> {
        vec![crate::capability::ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]
    }

    fn make_request(
        model: &str,
        tools: Option<Vec<crate::capability::ToolDefinition>>,
    ) -> UnifiedRequest {
        UnifiedRequest {
            model: model.to_string(),
            messages: vec![crate::provider::Message {
                role: crate::provider::Role::System,
                content: "You are helpful.".to_string(),
                images: None,
                tool_calls: None,
                tool_call_id: None,
                extras: None,
            }],
            tools,
            stream: true,
            temperature: None,
            max_tokens: None,
            extra_params: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn chat_stream_emulated_injects_tool_schemas_into_system_prompt() {
        let captured: Arc<Mutex<Option<UnifiedRequest>>> = Arc::new(Mutex::new(None));
        let provider = CapturingMockProvider::new_emulated(captured.clone(), vec![]);
        let router = router_with("emulated-mock", Box::new(provider), &["test-model"]);

        let req = make_request("test-model", Some(make_tools()));
        let _ = router.chat_stream(req, true).await;

        let cap = captured.lock().unwrap().take().unwrap();
        let system_msg = cap
            .messages
            .iter()
            .find(|m| m.role == crate::provider::Role::System)
            .unwrap();
        assert!(
            system_msg.content.contains("get_weather"),
            "tool schemas should be injected into system prompt"
        );
        assert!(
            system_msg.content.contains("You are helpful."),
            "original system prompt should be preserved"
        );
    }

    #[tokio::test]
    async fn chat_stream_emulated_strips_tools_from_request() {
        let captured: Arc<Mutex<Option<UnifiedRequest>>> = Arc::new(Mutex::new(None));
        let provider = CapturingMockProvider::new_emulated(captured.clone(), vec![]);
        let router = router_with("emulated-mock", Box::new(provider), &["test-model"]);

        let req = make_request("test-model", Some(make_tools()));
        let _ = router.chat_stream(req, true).await;

        let cap = captured.lock().unwrap().take().unwrap();
        assert!(
            cap.tools.is_none(),
            "tools should be stripped from request when emulated"
        );
    }

    #[tokio::test]
    async fn chat_stream_native_passes_tools_through_unchanged() {
        let captured: Arc<Mutex<Option<UnifiedRequest>>> = Arc::new(Mutex::new(None));
        let provider = CapturingMockProvider::new_native(captured.clone(), vec![]);
        let router = router_with("native-mock", Box::new(provider), &["test-model"]);

        let tools = make_tools();
        let req = make_request("test-model", Some(tools.clone()));
        let _ = router.chat_stream(req, true).await;

        let cap = captured.lock().unwrap().take().unwrap();
        // Native tool calling: tools should still be present
        assert!(
            cap.tools.is_some(),
            "tools should NOT be stripped for native tool calling"
        );
        assert_eq!(cap.tools.unwrap().len(), 1);

        // System prompt should be unchanged
        let system_msg = cap
            .messages
            .iter()
            .find(|m| m.role == crate::provider::Role::System)
            .unwrap();
        assert_eq!(
            system_msg.content, "You are helpful.",
            "system prompt should be unchanged for native"
        );
    }

    #[tokio::test]
    async fn chat_stream_emulated_wraps_stream_with_tool_emulation() {
        let captured: Arc<Mutex<Option<UnifiedRequest>>> = Arc::new(Mutex::new(None));
        // Provider returns a JSON tool call in the stream
        let chunks = vec![crate::streaming::LLMChunk {
            content: Some("{\"name\":\"get_weather\",\"arguments\":{}}".to_string()),
            done: true,
            ..Default::default()
        }];
        let provider = CapturingMockProvider::new_emulated(captured.clone(), chunks);
        let router = router_with("emulated-mock", Box::new(provider), &["test-model"]);

        let req = make_request("test-model", Some(make_tools()));
        let result = router.chat_stream(req, true).await.unwrap();

        match result {
            crate::streaming::StreamMode::Normalized(mut stream) => {
                use tokio_stream::StreamExt;
                let chunk = stream.next().await.unwrap().unwrap();
                // ToolEmulationStream should parse the JSON into a tool_call
                assert!(
                    chunk.tool_call.is_some(),
                    "stream should have tool_call parsed by ToolEmulationStream"
                );
                assert_eq!(chunk.tool_call.unwrap().name, "get_weather");
            }
            crate::streaming::StreamMode::Passthrough(_) => {
                panic!("expected Normalized stream, got Passthrough");
            }
            crate::streaming::StreamMode::NonStreaming(_) => {
                panic!("expected Normalized stream, got NonStreaming");
            }
        }
    }

    #[tokio::test]
    async fn chat_stream_emulated_creates_system_prompt_if_missing() {
        let captured: Arc<Mutex<Option<UnifiedRequest>>> = Arc::new(Mutex::new(None));
        let provider = CapturingMockProvider::new_emulated(captured.clone(), vec![]);
        let router = router_with("emulated-mock", Box::new(provider), &["test-model"]);

        // Request with NO system message
        let mut req = make_request("test-model", Some(make_tools()));
        req.messages = vec![crate::provider::Message {
            role: crate::provider::Role::User,
            content: "Hello".to_string(),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            extras: None,
        }];
        let _ = router.chat_stream(req, true).await;

        let cap = captured.lock().unwrap().take().unwrap();
        // Should have created a system message at position 0 with tool schemas
        assert_eq!(
            cap.messages[0].role,
            crate::provider::Role::System,
            "system message should be inserted"
        );
        assert!(
            cap.messages[0].content.contains("get_weather"),
            "injected system prompt should contain tool schemas"
        );
    }

    #[tokio::test]
    async fn chat_stream_upstream_model_override_applied() {
        let captured: Arc<Mutex<Option<UnifiedRequest>>> = Arc::new(Mutex::new(None));
        let provider = CapturingMockProvider::new_native(captured.clone(), vec![]);
        let mut providers = HashMap::new();
        providers.insert("p1".to_string(), Box::new(provider) as Box<dyn Provider>);
        let mut upstream = HashMap::new();
        upstream.insert("p1".to_string(), "real-upstream-model".to_string());
        let mut routes = HashMap::new();
        routes.insert(
            "my-model".to_string(),
            RouteEntry {
                provider_ids: vec!["p1".to_string()],
                upstream_models: upstream,
            },
        );
        let router = RealRouter { providers, routes };

        let req = make_request("my-model", None);
        let _ = router.chat_stream(req, true).await;

        let cap = captured.lock().unwrap().take().unwrap();
        assert_eq!(
            cap.model, "real-upstream-model",
            "model should be overridden to upstream model"
        );
    }

    #[tokio::test]
    async fn chat_stream_unknown_model_returns_error() {
        let provider = CapturingMockProvider::new_native(Arc::new(Mutex::new(None)), vec![]);
        let router = router_with("native-mock", Box::new(provider), &["known-model"]);

        let req = make_request("unknown-model", None);
        let result = router.chat_stream(req, true).await;
        assert!(matches!(result, Err(RouterError::ModelNotFound(_))));
    }

    #[tokio::test]
    async fn chat_stream_forwards_stream_error_without_rewrapping() {
        // When a provider's Normalized stream yields a StreamError mid-stream
        // (e.g. reqwest "error decoding response body" when upstream drops the
        // chunked connection), the router must forward the error unchanged.
        // Re-wrapping it as StreamError::Provider collapses the variant and
        // double-prefixes the Display ("provider error: provider error: …"),
        // which is what surfaced as the noisy stream chunk error log.
        let provider = ErrorStreamMock::new();
        let router = router_with("error-mock", Box::new(provider), &["error-model"]);

        let req = make_request("error-model", None);
        let result = router.chat_stream(req, true).await.unwrap();

        match result {
            crate::streaming::StreamMode::Normalized(mut stream) => {
                use tokio_stream::StreamExt;
                let item = stream
                    .next()
                    .await
                    .expect("stream should yield the injected error");
                match item {
                    Err(crate::streaming::StreamError::Connection(msg)) => {
                        assert_eq!(msg, "upstream dropped");
                    }
                    other => panic!("expected StreamError::Connection, got {other:?}"),
                }
            }
            crate::streaming::StreamMode::Passthrough(_) => {
                panic!("expected Normalized stream, got Passthrough");
            }
            crate::streaming::StreamMode::NonStreaming(_) => {
                panic!("expected Normalized stream, got NonStreaming");
            }
        }
    }

    // ── [1m] suffix normalization tests ─────────────────────────────────

    #[test]
    fn normalize_model_name_strips_suffix() {
        assert_eq!(normalize_model_name("opus[1m]"), "opus");
        assert_eq!(normalize_model_name("sonnet"), "sonnet");
        assert_eq!(normalize_model_name(""), "");
        assert_eq!(normalize_model_name("[1m]"), "");
        // Only last occurrence stripped
        assert_eq!(normalize_model_name("op[1m]us[1m]"), "op[1m]us");
        // Mid-string [1m] is not a suffix — not stripped
        assert_eq!(
            normalize_model_name("model[1m]-variant"),
            "model[1m]-variant"
        );
    }

    #[test]
    fn resolve_model_with_suffix() {
        let router = router_with(
            "p1",
            Box::new(MockProvider::new(
                "p1",
                ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                    supports_responses: false,
                    supports_anthropic: false,
                },
            )),
            &["gpt-4"],
        );
        assert_eq!(router.resolve("gpt-4[1m]").unwrap(), "p1");
        // Without suffix still works
        assert_eq!(router.resolve("gpt-4").unwrap(), "p1");
    }

    #[test]
    fn resolve_unknown_model_with_suffix_returns_error() {
        let router = router_with(
            "p1",
            Box::new(MockProvider::new(
                "p1",
                ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                    supports_responses: false,
                    supports_anthropic: false,
                },
            )),
            &["gpt-4"],
        );
        let err = router.resolve("unknown[1m]").unwrap_err();
        assert!(matches!(err, RouterError::ModelNotFound(_)));
        // Error message uses normalized name (without [1m])
        assert_eq!(err.to_string(), "model not found: unknown");
    }

    #[test]
    fn resolve_upstream_model_with_suffix() {
        let mut providers = HashMap::new();
        providers.insert(
            "p1".to_string(),
            Box::new(MockProvider::new(
                "p1",
                ProviderCapabilities {
                    tool_calling: crate::types::ToolCallingSupport::Native,
                    image_input: false,
                    streaming: true,
                    supports_responses: false,
                    supports_anthropic: false,
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
            router.resolve_upstream_model("my-model[1m]"),
            Some("upstream-model".to_string())
        );
    }

    #[tokio::test]
    async fn chat_stream_normalizes_model_suffix() {
        let captured: Arc<Mutex<Option<UnifiedRequest>>> = Arc::new(Mutex::new(None));
        let provider = CapturingMockProvider::new_native(captured.clone(), vec![]);
        let router = router_with("native-mock", Box::new(provider), &["test-model"]);

        let req = make_request("test-model[1m]", None);
        let result = router.chat_stream(req, true).await;
        assert!(result.is_ok(), "should resolve with normalized model name");

        let cap = captured.lock().unwrap().take().unwrap();
        assert_eq!(
            cap.model, "test-model",
            "request.model should be normalized (no [1m] suffix)"
        );
    }

    #[tokio::test]
    async fn chat_stream_upstream_model_override_with_suffix() {
        let captured: Arc<Mutex<Option<UnifiedRequest>>> = Arc::new(Mutex::new(None));
        let provider = CapturingMockProvider::new_native(captured.clone(), vec![]);
        let mut providers = HashMap::new();
        providers.insert("p1".to_string(), Box::new(provider) as Box<dyn Provider>);
        let mut upstream = HashMap::new();
        upstream.insert("p1".to_string(), "real-upstream-model".to_string());
        let mut routes = HashMap::new();
        routes.insert(
            "my-model".to_string(),
            RouteEntry {
                provider_ids: vec!["p1".to_string()],
                upstream_models: upstream,
            },
        );
        let router = RealRouter { providers, routes };

        let req = make_request("my-model[1m]", None);
        let _ = router.chat_stream(req, true).await;

        let cap = captured.lock().unwrap().take().unwrap();
        assert_eq!(
            cap.model, "real-upstream-model",
            "upstream model override should still apply with [1m] suffix"
        );
    }
}
