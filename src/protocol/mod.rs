//! Protocol dispatch layer.
//!
//! Routes incoming requests to the appropriate protocol parser (`OpenAI` or Anthropic).

pub mod anthropic;
pub mod openai;
pub mod serializer;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::router::Router;

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
async fn forward_streaming(
    router: &dyn Router,
    request: crate::provider::UnifiedRequest,
    model: &str,
    protocol: Protocol,
    mut tracker: crate::metrics::StreamTracker,
) -> Result<Response, ProtocolError> {
    let model_name = model.to_string();
    let allow_passthrough = protocol == Protocol::OpenAI;

    // Sync tracker model with the upstream model for accurate metrics.
    if let Some(upstream) = router.resolve_upstream_model(model) {
        tracker.set_model(upstream);
    }

    let stream_mode = router
        .chat_stream(request, allow_passthrough)
        .await
        .map_err(|e| ProtocolError::Internal {
            message: e.to_string(),
            protocol,
        })?;

    match stream_mode {
        crate::streaming::StreamMode::Normalized(stream) => {
            use futures::StreamExt;
            let mut sent_role = false;
            let mut anthropic_state = serializer::AnthropicStreamState::new();
            let model_name_start = model_name.clone();

            let body_stream = futures::StreamExt::map(stream, move |chunk_result| {
                match chunk_result {
                    Ok(chunk) => {
                        tracker.set_ttft(tracker.start.elapsed().as_millis() as u64);
                        if let Some(usage) = &chunk.usage {
                            tracker.set_tokens(usage.clone());
                        }
                        let text = match protocol {
                            Protocol::OpenAI => {
                                let include_role = !sent_role;
                                sent_role = true;
                                serializer::serialize_openai_chunk(&chunk, &model_name, include_role)
                            }
                            Protocol::Anthropic => {
                                anthropic_state.serialize_chunk(&chunk, &model_name)
                            }
                        };
                        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(text))
                    }
                    Err(e) => {
                        tracker.set_success(false);
                        let err_json = serde_json::json!({
                            "error": {
                                "message": e.to_string(),
                                "type": "internal_error"
                            }
                        });
                        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(format!(
                            "data: {err_json}\n\n"
                        )))
                    }
                }
            })
            .boxed();

            if protocol == Protocol::Anthropic {
                let start_bytes = axum::body::Bytes::from(anthropic::serialize_message_start(
                    "msg_gw",
                    &model_name_start,
                ));
                let stop_bytes = axum::body::Bytes::from(anthropic::serialize_message_stop());

                let combined = futures::StreamExt::chain(
                    futures::StreamExt::chain(
                        futures::stream::once(async move {
                            Ok::<_, std::convert::Infallible>(start_bytes)
                        }),
                        body_stream,
                    ),
                    futures::stream::once(
                        async move { Ok::<_, std::convert::Infallible>(stop_bytes) },
                    ),
                );

                return Ok(sse_response(Box::pin(combined)));
            }

            Ok(sse_response(Box::pin(body_stream)))
        }
        crate::streaming::StreamMode::Passthrough(byte_stream) => {
            let mut buf = bytes::BytesMut::with_capacity(8192);

            let body_stream =
                futures::StreamExt::map(byte_stream, move |chunk_result| match chunk_result {
                    Ok(bytes) => {
                        tracker.set_ttft(tracker.start.elapsed().as_millis() as u64);

                        if tracker.tokens.is_none() {
                            buf.extend_from_slice(&bytes);
                            if buf.len() > 8192 {
                                let excess = buf.len() - 8192;
                                let _ = buf.split_to(excess);
                            }
                            if let Some(usage) = serializer::extract_usage_from_sse_bytes(&buf) {
                                tracker.set_tokens(usage);
                            }
                        }

                        Ok::<_, std::convert::Infallible>(bytes)
                    }
                    Err(e) => {
                        tracker.set_success(false);
                        let err_json = serde_json::json!({
                            "error": {
                                "message": e.to_string(),
                                "type": "internal_error"
                            }
                        });
                        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(format!(
                            "data: {err_json}\n\n"
                        )))
                    }
                });

            Ok(sse_response(Box::pin(body_stream)))
        }
    }
}

fn sse_response(body_stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<axum::body::Bytes, std::convert::Infallible>> + Send>>) -> Response {
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
            return Err(ProtocolError::Internal {
                message: e.to_string(),
                protocol,
            });
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
        collect_non_streaming(
            router,
            unified_req,
            &model_name,
            Protocol::Anthropic,
            guard,
        )
        .await
    }
}

/// Error type for protocol dispatch failures.  
#[derive(Debug)]
pub enum ProtocolError {
    Parse { message: String, protocol: Protocol },
    Internal { message: String, protocol: Protocol },
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
        };

        let body = match protocol {
            Protocol::OpenAI => serde_json::json!({
                "error": {
                    "type": if status == StatusCode::BAD_REQUEST { "invalid_request_error" } else { "server_error" },
                    "message": error_msg
                }
            }),
            Protocol::Anthropic => serde_json::json!({
                "type": "error",
                "error": {
                    "type": if status == StatusCode::BAD_REQUEST { "invalid_request_error" } else { "api_error" },
                    "message": error_msg
                }
            }),
        };

        (status, Json(body)).into_response()
    }
}
