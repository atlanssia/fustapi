//! Protocol dispatch layer.
//!
//! Routes incoming requests to the appropriate protocol parser (`OpenAI` or Anthropic).

pub mod anthropic;
pub mod openai;
pub mod serializer;
mod stream_dispatch;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::router::{Router, RouterError};

/// Map a `RouterError` to the appropriate `ProtocolError`, preserving upstream HTTP status.
fn map_router_error(e: RouterError, protocol: Protocol) -> ProtocolError {
    match e {
        RouterError::Upstream { status, message } => ProtocolError::Upstream {
            status,
            message,
            protocol,
        },
        RouterError::ModelNotFound(msg) => ProtocolError::Parse {
            message: msg,
            protocol,
        },
        other => ProtocolError::Internal {
            message: other.to_string(),
            protocol,
        },
    }
}

/// Protocol identifier for dispatch decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAI,
    Anthropic,
}

/// Detect protocol from request path and headers.
#[must_use]
pub fn detect_protocol(path: &str, headers: &axum::http::HeaderMap) -> Protocol {
    if path.starts_with("/v1/messages") || headers.get("anthropic-version").is_some() {
        Protocol::Anthropic
    } else {
        Protocol::OpenAI // default for /v1/ and all other paths
    }
}

/// Check if request wants streaming.
fn is_streaming(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .is_ok_and(|v| v.get("stream").is_some_and(|s| s.as_bool() == Some(true)))
}

/// Dispatch a request to the appropriate protocol handler.
pub async fn dispatch_request(
    protocol: Protocol,
    body: String,
    router: &dyn Router,
    guard: crate::metrics::guard::RequestGuard,
) -> Result<Response, ProtocolError> {
    match protocol {
        Protocol::OpenAI => openai_handler(body, router, guard).await,
        Protocol::Anthropic => anthropic_handler(body, router, guard).await,
    }
}

/// Handle an OpenAI-format request. Forwards to the appropriate provider.
async fn openai_handler(
    body: String,
    router: &dyn Router,
    guard: crate::metrics::guard::RequestGuard,
) -> Result<Response, ProtocolError> {
    let unified_req = match openai::parse_chat_request(&body) {
        Ok(req) => req,
        Err(e) => {
            return Err(ProtocolError::Parse {
                message: e.to_string(),
                protocol: Protocol::OpenAI,
            });
        }
    };
    let model_name = unified_req.model.clone();
    let provider_req = unified_req.clone();

    if is_streaming(&body) {
        let tracker = guard.into_tracker();
        forward_streaming(router, provider_req, &model_name, Protocol::OpenAI, tracker).await
    } else {
        collect_non_streaming(router, provider_req, &model_name, Protocol::OpenAI, guard).await
    }
}

/// Forward provider stream as SSE response.
///
/// Delegates serialization + metrics to [`stream_dispatch::forward_as_sse_response`].
async fn forward_streaming(
    router: &dyn Router,
    request: crate::provider::UnifiedRequest,
    model: &str,
    protocol: Protocol,
    mut tracker: crate::metrics::StreamTracker,
) -> Result<Response, ProtocolError> {
    let allow_passthrough = protocol == Protocol::OpenAI;

    // Sync tracker model with the upstream model for accurate metrics.
    if let Some(upstream) = router.resolve_upstream_model(model) {
        tracker.set_model(upstream);
    }

    let stream_mode = router
        .chat_stream(request, allow_passthrough)
        .await
        .map_err(|e| map_router_error(e, protocol))?;

    Ok(stream_dispatch::forward_as_sse_response(
        stream_mode,
        protocol,
        model,
        tracker,
    ))
}

/// Build an SSE response with correct headers.
///
/// Re-exported from [`stream_dispatch`] for backward compatibility with tests.
fn sse_response(
    body_stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<axum::body::Bytes, std::convert::Infallible>> + Send>,
    >,
) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(axum::body::Body::from_stream(body_stream))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap()
        })
}

/// Collect all chunks and return a single non-streaming response.
async fn collect_non_streaming(
    router: &dyn Router,
    request: crate::provider::UnifiedRequest,
    model: &str,
    protocol: Protocol,
    mut guard: crate::metrics::guard::RequestGuard,
) -> Result<Response, ProtocolError> {
    use tokio_stream::StreamExt;

    // Sync guard model with the upstream model for accurate metrics.
    if let Some(upstream) = router.resolve_upstream_model(model) {
        guard.set_model(upstream);
    }

    let stream_mode = match router.chat_stream(request, false).await {
        Ok(mode) => mode,
        Err(e) => {
            guard.finish_err();
            return Err(map_router_error(e, protocol));
        }
    };

    let mut stream = match stream_mode {
        crate::streaming::StreamMode::Normalized(stream) => stream,
        crate::streaming::StreamMode::Passthrough(_) => {
            guard.finish_err();
            return Err(ProtocolError::Internal {
                message: "Passthrough not supported for non-streaming".to_string(),
                protocol,
            });
        }
    };

    let mut chunks = Vec::new();
    let mut first_token_ms = None;
    let mut final_usage = None;
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                if first_token_ms.is_none() {
                    first_token_ms = Some(guard.elapsed_ms());
                }
                if let Some(ref usage) = chunk.usage {
                    final_usage = Some(usage.clone());
                }
                chunks.push(chunk);
            }
            Err(e) => {
                guard.finish_err();
                return Err(ProtocolError::Internal {
                    message: e.to_string(),
                    protocol,
                });
            }
        }
    }

    let content = chunks
        .iter()
        .filter_map(|c| c.content.clone())
        .collect::<String>();

    let reasoning_content = chunks
        .iter()
        .filter_map(|c| c.reasoning_content.clone())
        .collect::<String>();

    let tool_calls: Vec<_> = chunks.iter().filter_map(|c| c.tool_call.clone()).collect();

    let tool_calls_opt = if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls.clone())
    };

    let finish_reason = if tool_calls.is_empty() {
        match protocol {
            Protocol::OpenAI => "stop",
            Protocol::Anthropic => "end_turn",
        }
    } else {
        match protocol {
            Protocol::OpenAI => "tool_calls",
            Protocol::Anthropic => "tool_use",
        }
    };

    let (prompt_tokens, completion_tokens) = final_usage.as_ref().map_or((0, 0), |u| {
        (u.prompt_tokens as usize, u.completion_tokens as usize)
    });

    let response_body = match protocol {
        Protocol::OpenAI => match openai::serialize_response(
            "chatcmpl-gw",
            model,
            if content.is_empty() {
                None
            } else {
                Some(&content)
            },
            tool_calls_opt,
            finish_reason,
            prompt_tokens,
            completion_tokens,
            prompt_tokens + completion_tokens,
            if reasoning_content.is_empty() {
                None
            } else {
                Some(&reasoning_content)
            },
        ) {
            Ok(body) => body,
            Err(e) => {
                guard.finish_err();
                return Err(ProtocolError::Internal {
                    message: e.to_string(),
                    protocol,
                });
            }
        },
        Protocol::Anthropic => match anthropic::serialize_response(
            "msg-gw",
            model,
            if content.is_empty() {
                None
            } else {
                Some(&content)
            },
            tool_calls_opt,
            Some(finish_reason),
            if reasoning_content.is_empty() {
                None
            } else {
                Some(&reasoning_content)
            },
            prompt_tokens,
            completion_tokens,
        ) {
            Ok(body) => body,
            Err(e) => {
                guard.finish_err();
                return Err(ProtocolError::Internal {
                    message: e.to_string(),
                    protocol,
                });
            }
        },
    };

    guard.finish(true, final_usage, first_token_ms);

    let body: serde_json::Value = serde_json::from_str(&response_body).unwrap_or_else(
        |e| serde_json::json!({"error": {"message": format!("serialization error: {}", e)}}),
    );
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// Handle an Anthropic-format request. Forwards to the appropriate provider.
async fn anthropic_handler(
    body: String,
    router: &dyn Router,
    guard: crate::metrics::guard::RequestGuard,
) -> Result<Response, ProtocolError> {
    let unified_req = match anthropic::parse_messages_request(&body) {
        Ok(req) => req,
        Err(e) => {
            return Err(ProtocolError::Parse {
                message: e.to_string(),
                protocol: Protocol::Anthropic,
            });
        }
    };

    let model_name = unified_req.model.clone();

    if is_streaming(&body) {
        let tracker = guard.into_tracker();
        forward_streaming(
            router,
            unified_req,
            &model_name,
            Protocol::Anthropic,
            tracker,
        )
        .await
    } else {
        collect_non_streaming(router, unified_req, &model_name, Protocol::Anthropic, guard).await
    }
}

/// Error type for protocol dispatch failures.
#[derive(Debug)]
pub enum ProtocolError {
    Parse {
        message: String,
        protocol: Protocol,
    },
    Internal {
        message: String,
        protocol: Protocol,
    },
    /// Upstream provider returned a client error — forward with original status.
    Upstream {
        status: u16,
        message: String,
        protocol: Protocol,
    },
}

impl IntoResponse for ProtocolError {
    fn into_response(self) -> Response {
        let (status, error_msg, protocol) = match self {
            ProtocolError::Parse { message, protocol } => {
                (StatusCode::BAD_REQUEST, message, protocol)
            }
            ProtocolError::Internal { message, protocol } => {
                (StatusCode::INTERNAL_SERVER_ERROR, message, protocol)
            }
            ProtocolError::Upstream {
                status,
                message,
                protocol,
            } => {
                let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
                (code, message, protocol)
            }
        };

        let body = match protocol {
            Protocol::OpenAI => serde_json::json!({
                "error": {
                    "type": if status.is_client_error() { "invalid_request_error" } else { "server_error" },
                    "message": error_msg
                }
            }),
            Protocol::Anthropic => serde_json::json!({
                "type": "error",
                "error": {
                    "type": if status.is_client_error() { "invalid_request_error" } else { "api_error" },
                    "message": error_msg
                }
            }),
        };

        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{StreamTracker, TokenUsage};
    use crate::provider::{Message, Role, UnifiedRequest};
    use crate::streaming::{LLMChunk, StreamError, StreamMode};
    use crate::capability::ToolCall;
    use async_trait::async_trait;
    use axum::http::HeaderValue;
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};

    // ── Mock Router ──────────────────────────────────────────────────

    struct MockRouter {
        stream_mode: Arc<Mutex<Option<StreamMode>>>,
        upstream_model: Option<String>,
    }

    impl MockRouter {
        fn with_normalized_chunks(chunks: Vec<Result<LLMChunk, StreamError>>) -> Self {
            let stream = tokio_stream::iter(chunks);
            let mode = StreamMode::Normalized(Box::pin(stream));
            Self {
                stream_mode: Arc::new(Mutex::new(Some(mode))),
                upstream_model: None,
            }
        }

        fn with_passthrough_bytes(batches: Vec<Result<bytes::Bytes, StreamError>>) -> Self {
            let stream = tokio_stream::iter(batches);
            let mode = StreamMode::Passthrough(Box::pin(stream));
            Self {
                stream_mode: Arc::new(Mutex::new(Some(mode))),
                upstream_model: None,
            }
        }

        fn with_upstream_model(model: &str) -> Self {
            Self {
                stream_mode: Arc::new(Mutex::new(None)),
                upstream_model: Some(model.to_string()),
            }
        }
    }

    #[async_trait]
    impl crate::router::Router for MockRouter {
        fn resolve(&self, _model: &str) -> Result<String, crate::router::RouterError> {
            Ok("mock".to_string())
        }
        fn resolve_upstream_model(&self, _model: &str) -> Option<String> {
            self.upstream_model.clone()
        }
        fn list_models(&self) -> Vec<String> {
            vec!["test-model".to_string()]
        }
        fn list_providers(&self) -> Vec<String> {
            vec!["mock".to_string()]
        }
        async fn chat_stream(
            &self,
            _request: UnifiedRequest,
            _allow_passthrough: bool,
        ) -> Result<StreamMode, crate::router::RouterError> {
            self.stream_mode
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| crate::router::RouterError::Internal("no stream configured".into()))
        }
    }

    fn make_tracker() -> StreamTracker {
        let (emitter, _reader) = crate::metrics::init();
        StreamTracker::new(emitter, "mock".to_string(), "test-model".to_string(), std::time::Instant::now())
    }

    fn make_guard() -> crate::metrics::guard::RequestGuard {
        let (emitter, _reader) = crate::metrics::init();
        crate::metrics::guard::RequestGuard::start(emitter, "mock", "test-model")
    }

    fn make_request() -> UnifiedRequest {
        UnifiedRequest {
            model: "test-model".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: "hello".to_string(),
                images: None,
                tool_calls: None,
                tool_call_id: None,
                extras: None,
            }],
            stream: true,
            temperature: None,
            max_tokens: None,
            tools: None,
            extra_params: serde_json::Map::new(),
        }
    }

    // ── detect_protocol tests ────────────────────────────────────────

    #[test]
    fn detect_protocol_openai_default() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(detect_protocol("/v1/chat/completions", &headers), Protocol::OpenAI);
        assert_eq!(detect_protocol("/v1/models", &headers), Protocol::OpenAI);
        assert_eq!(detect_protocol("/anything", &headers), Protocol::OpenAI);
    }

    #[test]
    fn detect_protocol_anthropic_by_path() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(detect_protocol("/v1/messages", &headers), Protocol::Anthropic);
    }

    #[test]
    fn detect_protocol_anthropic_by_header() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        assert_eq!(detect_protocol("/v1/chat/completions", &headers), Protocol::Anthropic);
    }

    // ── is_streaming tests ───────────────────────────────────────────

    #[test]
    fn is_streaming_true() {
        assert!(is_streaming(r#"{"stream":true}"#));
    }

    #[test]
    fn is_streaming_false() {
        assert!(!is_streaming(r#"{"stream":false}"#));
    }

    #[test]
    fn is_streaming_missing() {
        assert!(!is_streaming(r#"{"model":"gpt-4"}"#));
    }

    #[test]
    fn is_streaming_invalid_json() {
        assert!(!is_streaming("not json"));
    }

    // ── sse_response tests ───────────────────────────────────────────

    #[tokio::test]
    async fn sse_response_has_correct_headers() {
        let stream = futures::stream::once(async {
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from("data: test\n\n"))
        });
        let resp = sse_response(Box::pin(stream));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
        assert_eq!(
            resp.headers().get("cache-control").unwrap(),
            "no-cache"
        );
        assert_eq!(
            resp.headers().get("connection").unwrap(),
            "keep-alive"
        );
    }

    // ── forward_streaming: OpenAI normalized ─────────────────────────

    #[tokio::test]
    async fn forward_streaming_openai_produces_sse_format() {
        let chunks = vec![
            Ok(LLMChunk {
                content: Some("Hello".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
            Ok(LLMChunk {
                content: Some(" world".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
            Ok(LLMChunk {
                content: None,
                reasoning_content: None,
                tool_call: None,
                done: true,
                usage: Some(TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                }),
            }),
        ];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await
        .unwrap();

        assert_eq!(result.status(), StatusCode::OK);
        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);

        // First chunk should include role
        assert!(text.contains(r#""role":"assistant""#));
        // Content deltas
        assert!(text.contains(r#""content":"Hello""#));
        assert!(text.contains(r#""content":" world""#));
        // Done marker
        assert!(text.contains("[DONE]"));
        // Stop reason
        assert!(text.contains(r#""finish_reason":"stop""#));
    }

    #[tokio::test]
    async fn forward_streaming_openai_only_first_chunk_has_role() {
        let chunks = vec![
            Ok(LLMChunk {
                content: Some("a".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
            Ok(LLMChunk {
                content: Some("b".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
        ];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);

        // Count role occurrences -- should be exactly 1
        assert_eq!(text.matches(r#""role":"assistant""#).count(), 1);
    }

    #[tokio::test]
    async fn forward_streaming_openai_includes_model_in_chunks() {
        let chunks = vec![Ok(LLMChunk {
            content: Some("hi".to_string()),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: None,
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "my-model",
            Protocol::OpenAI,
            tracker,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(r#""model":"my-model""#));
    }

    // ── forward_streaming: OpenAI error chunks ───────────────────────

    #[tokio::test]
    async fn forward_streaming_openai_error_produces_error_sse() {
        let chunks = vec![
            Ok(LLMChunk {
                content: Some("partial".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
            Err(StreamError::Provider("boom".to_string())),
        ];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("error"));
        assert!(text.contains("boom"));
    }

    // ── forward_streaming: OpenAI passthrough ────────────────────────

    #[tokio::test]
    async fn forward_streaming_passthrough_forwards_bytes_unchanged() {
        let batches = vec![
            Ok(bytes::Bytes::from("data: hello\n\n")),
            Ok(bytes::Bytes::from("data: world\n\n")),
        ];
        let router = MockRouter::with_passthrough_bytes(batches);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(text, "data: hello\n\ndata: world\n\n");
    }

    #[tokio::test]
    async fn forward_streaming_passthrough_extracts_usage() {
        let usage_json = r#"data: {"id":"x","usage":{"prompt_tokens":10,"completion_tokens":20}}"#;
        let batches = vec![
            Ok(bytes::Bytes::from(format!("{usage_json}\n\n"))),
        ];
        let router = MockRouter::with_passthrough_bytes(batches);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await
        .unwrap();

        // Verify it returns successfully (usage extraction happens internally)
        assert_eq!(result.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn forward_streaming_passthrough_error_produces_error_sse() {
        let batches = vec![Err(StreamError::Connection("timeout".to_string()))];
        let router = MockRouter::with_passthrough_bytes(batches);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("error"));
        assert!(text.contains("timeout"));
    }

    // ── forward_streaming: Anthropic normalized ──────────────────────

    #[tokio::test]
    async fn forward_streaming_anthropic_wraps_with_message_start_stop() {
        let chunks = vec![Ok(LLMChunk {
            content: Some("hi".to_string()),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: None,
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "claude-3",
            Protocol::Anthropic,
            tracker,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);

        // Anthropic wraps with message_start and message_stop
        assert!(text.contains("event: message_start"));
        assert!(text.contains("event: message_stop"));
        // Content should have content_block_start/delta
        assert!(text.contains("event: content_block_start"));
        assert!(text.contains("event: content_block_delta"));
    }

    #[tokio::test]
    async fn forward_streaming_anthropic_message_start_includes_model() {
        let chunks = vec![Ok(LLMChunk {
            content: Some("test".to_string()),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: None,
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "claude-3-opus",
            Protocol::Anthropic,
            tracker,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(r#""model":"claude-3-opus""#));
    }

    #[tokio::test]
    async fn forward_streaming_anthropic_text_delta_events() {
        let chunks = vec![
            Ok(LLMChunk {
                content: Some("Hello".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
            Ok(LLMChunk {
                content: Some(" world".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
        ];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "claude-3",
            Protocol::Anthropic,
            tracker,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);

        // Should have text_delta events with correct content
        assert!(text.contains(r#""text":"Hello""#));
        assert!(text.contains(r#""text":" world""#));
        // Subsequent text chunks should NOT start a new content block
        assert_eq!(text.matches("event: content_block_start").count(), 1);
    }

    // ── forward_streaming: Metrics tracking ──────────────────────────

    #[tokio::test]
    async fn forward_streaming_tracks_ttft_on_first_chunk() {
        let chunks = vec![Ok(LLMChunk {
            content: Some("hi".to_string()),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: None,
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn forward_streaming_tracks_token_usage() {
        let chunks = vec![Ok(LLMChunk {
            content: None,
            reasoning_content: None,
            tool_call: None,
            done: true,
            usage: Some(TokenUsage {
                prompt_tokens: 50,
                completion_tokens: 25,
            }),
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await;
        assert!(result.is_ok());
    }

    // ── forward_streaming: upstream model resolution ─────────────────

    #[tokio::test]
    async fn forward_streaming_resolves_upstream_model() {
        let chunks = vec![Ok(LLMChunk {
            content: Some("hi".to_string()),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: None,
        })];
        let mut router = MockRouter::with_normalized_chunks(chunks);
        router.upstream_model = Some("upstream-model-v2".to_string());
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            tracker,
        )
        .await;
        assert!(result.is_ok());
    }

    // ── forward_streaming: Router errors ─────────────────────────────

    #[tokio::test]
    async fn forward_streaming_model_not_found_returns_error() {
        let router = MockRouter {
            stream_mode: Arc::new(Mutex::new(None)),
            upstream_model: None,
        };
        let tracker = make_tracker();
        let result = forward_streaming(
            &router,
            make_request(),
            "nonexistent",
            Protocol::OpenAI,
            tracker,
        )
        .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::Internal { .. } => {}
            other => panic!("expected Internal error, got {:?}", other),
        }
    }

    // ── collect_non_streaming: OpenAI ────────────────────────────────

    #[tokio::test]
    async fn collect_non_streaming_openai_text_response() {
        let chunks = vec![
            Ok(LLMChunk {
                content: Some("Hello".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
            Ok(LLMChunk {
                content: Some(" world".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
        ];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "gpt-4",
            Protocol::OpenAI,
            guard,
        )
        .await
        .unwrap();

        assert_eq!(result.status(), StatusCode::OK);
        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["model"], "gpt-4");
        assert_eq!(v["choices"][0]["message"]["content"], "Hello world");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert!(v["usage"]["total_tokens"].is_number());
    }

    #[tokio::test]
    async fn collect_non_streaming_openai_reasoning_content() {
        let chunks = vec![
            Ok(LLMChunk {
                content: None,
                reasoning_content: Some("thinking...".to_string()),
                tool_call: None,
                done: false,
                usage: None,
            }),
            Ok(LLMChunk {
                content: Some("answer".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
        ];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "deepseek-r1",
            Protocol::OpenAI,
            guard,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["choices"][0]["message"]["reasoning_content"], "thinking...");
        assert_eq!(v["choices"][0]["message"]["content"], "answer");
    }

    #[tokio::test]
    async fn collect_non_streaming_openai_tool_calls() {
        let chunks = vec![Ok(LLMChunk {
            content: None,
            reasoning_content: None,
            tool_call: Some(ToolCall {
                id: Some("call_1".to_string()),
                name: "get_weather".to_string(),
                arguments: serde_json::json!({"city": "nyc"}),
            }),
            done: false,
            usage: None,
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "gpt-4",
            Protocol::OpenAI,
            guard,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            v["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
    }

    #[tokio::test]
    async fn collect_non_streaming_openai_with_usage() {
        let chunks = vec![Ok(LLMChunk {
            content: Some("hi".to_string()),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: Some(TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
            }),
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "gpt-4",
            Protocol::OpenAI,
            guard,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["usage"]["prompt_tokens"], 100);
        assert_eq!(v["usage"]["completion_tokens"], 50);
        assert_eq!(v["usage"]["total_tokens"], 150);
    }

    // ── collect_non_streaming: Anthropic ─────────────────────────────

    #[tokio::test]
    async fn collect_non_streaming_anthropic_text_response() {
        let chunks = vec![Ok(LLMChunk {
            content: Some("Hello".to_string()),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: None,
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "claude-3",
            Protocol::Anthropic,
            guard,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(v["type"], "message");
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["model"], "claude-3");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "Hello");
        assert_eq!(v["stop_reason"], "end_turn");
    }

    #[tokio::test]
    async fn collect_non_streaming_anthropic_tool_use() {
        let chunks = vec![Ok(LLMChunk {
            content: None,
            reasoning_content: None,
            tool_call: Some(ToolCall {
                id: Some("toolu_1".to_string()),
                name: "search".to_string(),
                arguments: serde_json::json!({"q": "rust"}),
            }),
            done: false,
            usage: None,
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "claude-3",
            Protocol::Anthropic,
            guard,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["stop_reason"], "tool_use");
        assert_eq!(v["content"][0]["type"], "tool_use");
        assert_eq!(v["content"][0]["name"], "search");
    }

    #[tokio::test]
    async fn collect_non_streaming_anthropic_reasoning_maps_to_thinking() {
        let chunks = vec![
            Ok(LLMChunk {
                content: None,
                reasoning_content: Some("reasoning...".to_string()),
                tool_call: None,
                done: false,
                usage: None,
            }),
            Ok(LLMChunk {
                content: Some("answer".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
        ];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "deepseek-v3",
            Protocol::Anthropic,
            guard,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        // First content block should be thinking
        assert_eq!(v["content"][0]["type"], "thinking");
        assert_eq!(v["content"][0]["thinking"], "reasoning...");
        // Second content block should be text
        assert_eq!(v["content"][1]["type"], "text");
        assert_eq!(v["content"][1]["text"], "answer");
    }

    // ── collect_non_streaming: error handling ────────────────────────

    #[tokio::test]
    async fn collect_non_streaming_stream_error_returns_internal() {
        let chunks = vec![Err(StreamError::Provider("fail".to_string()))];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            guard,
        )
        .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::Internal { message, .. } => {
                assert!(message.contains("fail"));
            }
            other => panic!("expected Internal error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn collect_non_streaming_passthrough_returns_error() {
        let batches = vec![Ok(bytes::Bytes::from("data"))];
        let router = MockRouter::with_passthrough_bytes(batches);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            guard,
        )
        .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::Internal { message, .. } => {
                assert!(message.contains("Passthrough not supported"));
            }
            other => panic!("expected Internal error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn collect_non_streaming_router_error() {
        let router = MockRouter {
            stream_mode: Arc::new(Mutex::new(None)),
            upstream_model: None,
        };
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "nonexistent",
            Protocol::OpenAI,
            guard,
        )
        .await;
        assert!(result.is_err());
    }

    // ── collect_non_streaming: metrics tracking ──────────────────────

    #[tokio::test]
    async fn collect_non_streaming_tracks_first_token_time() {
        let chunks = vec![Ok(LLMChunk {
            content: Some("hi".to_string()),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: Some(TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 3,
            }),
        })];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result = collect_non_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            guard,
        )
        .await;
        assert!(result.is_ok());
    }

    // ── ProtocolError serialization ──────────────────────────────────

    #[test]
    fn protocol_error_parse_openai_format() {
        let err = ProtocolError::Parse {
            message: "bad json".to_string(),
            protocol: Protocol::OpenAI,
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn protocol_error_internal_status() {
        let err = ProtocolError::Internal {
            message: "oops".to_string(),
            protocol: Protocol::OpenAI,
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn protocol_error_upstream_forwards_status() {
        let err = ProtocolError::Upstream {
            status: 429,
            message: "rate limited".to_string(),
            protocol: Protocol::OpenAI,
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = resp.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["message"], "rate limited");
    }

    #[tokio::test]
    async fn protocol_error_anthropic_format() {
        let err = ProtocolError::Parse {
            message: "missing field".to_string(),
            protocol: Protocol::Anthropic,
        };
        let resp = err.into_response();
        let body = resp.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "error");
        assert!(v["error"]["message"].is_string());
    }

    // ── map_router_error tests ───────────────────────────────────────

    #[test]
    fn map_router_error_upstream() {
        let err = crate::router::RouterError::Upstream {
            status: 400,
            message: "bad".to_string(),
        };
        let result = map_router_error(err, Protocol::OpenAI);
        match result {
            ProtocolError::Upstream { status, .. } => assert_eq!(status, 400),
            other => panic!("expected Upstream, got {:?}", other),
        }
    }

    #[test]
    fn map_router_error_model_not_found() {
        let err = crate::router::RouterError::ModelNotFound("x".to_string());
        let result = map_router_error(err, Protocol::OpenAI);
        match result {
            ProtocolError::Parse { message, .. } => assert!(message.contains("x")),
            other => panic!("expected Parse, got {:?}", other),
        }
    }

    #[test]
    fn map_router_error_other() {
        let err = crate::router::RouterError::Internal("fail".to_string());
        let result = map_router_error(err, Protocol::Anthropic);
        match result {
            ProtocolError::Internal { message, protocol } => {
                assert!(message.contains("fail"));
                assert_eq!(protocol, Protocol::Anthropic);
            }
            other => panic!("expected Internal, got {:?}", other),
        }
    }

    // ── dispatch_request tests ───────────────────────────────────────

    #[tokio::test]
    async fn dispatch_request_openai_bad_json_returns_parse_error() {
        let router = MockRouter {
            stream_mode: Arc::new(Mutex::new(None)),
            upstream_model: None,
        };
        let guard = make_guard();
        let result = dispatch_request(
            Protocol::OpenAI,
            "not json".to_string(),
            &router,
            guard,
        )
        .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::Parse { protocol, .. } => assert_eq!(protocol, Protocol::OpenAI),
            other => panic!("expected Parse, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dispatch_request_anthropic_bad_json_returns_parse_error() {
        let router = MockRouter {
            stream_mode: Arc::new(Mutex::new(None)),
            upstream_model: None,
        };
        let guard = make_guard();
        let result = dispatch_request(
            Protocol::Anthropic,
            "not json".to_string(),
            &router,
            guard,
        )
        .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::Parse { protocol, .. } => assert_eq!(protocol, Protocol::Anthropic),
            other => panic!("expected Parse, got {:?}", other),
        }
    }
}
