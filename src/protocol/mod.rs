//! Protocol dispatch layer.
//!
//! Routes incoming requests to the appropriate protocol parser (`OpenAI` or Anthropic).

pub mod anthropic;
pub mod openai;
pub mod responses;
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
    Responses,
}

/// Detect protocol from request path and headers.
#[must_use]
pub fn detect_protocol(path: &str, headers: &axum::http::HeaderMap) -> Protocol {
    if path.starts_with("/v1/responses") {
        Protocol::Responses
    } else if path.starts_with("/v1/messages") || headers.get("anthropic-version").is_some() {
        Protocol::Anthropic
    } else {
        Protocol::OpenAI // default for /v1/ and all other paths
    }
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
        Protocol::Responses => responses_handler_impl(body, router, guard).await,
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

    if unified_req.stream {
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

/// Collect all chunks and return a single non-streaming response.
///
/// When the provider returns [`StreamMode::NonStreaming`] (for `stream: false` requests),
/// the upstream JSON is passed through directly with minimal parsing (extracting token
/// usage for metrics). For the Anthropic protocol, the OpenAI-format JSON from the
/// provider is converted to Anthropic format.
async fn collect_non_streaming(
    router: &dyn Router,
    request: crate::provider::UnifiedRequest,
    model: &str,
    protocol: Protocol,
    mut guard: crate::metrics::guard::RequestGuard,
) -> Result<Response, ProtocolError> {
    // Defensive guard: Responses non-streaming is handled by
    // dispatch_responses_conversion; dispatch_request routes Responses
    // through responses_handler_impl, never reaching collect_non_streaming.
    if protocol == Protocol::Responses {
        guard.finish_err();
        return Err(ProtocolError::Internal {
            message: "Responses API not yet implemented".to_string(),
            protocol: Protocol::Responses,
        });
    }

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

    match stream_mode {
        crate::streaming::StreamMode::NonStreaming(json) => {
            // Extract usage for metrics from the upstream JSON response.
            let usage = json.get("usage").map(|u| crate::metrics::TokenUsage {
                prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                completion_tokens: u
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32,
            });
            let first_token_ms = Some(guard.elapsed_ms());
            let usage_for_metrics = usage.clone();
            guard.finish(true, usage_for_metrics, first_token_ms);

            match protocol {
                Protocol::OpenAI => {
                    // Pass-through: return the upstream JSON as-is.
                    Ok((StatusCode::OK, Json(json)).into_response())
                }
                Protocol::Anthropic => {
                    // Convert OpenAI-format upstream JSON to Anthropic format.
                    let choices = json["choices"].as_array().and_then(|arr| arr.first());
                    let message = choices.and_then(|c| c.get("message"));

                    let content = message
                        .and_then(|m| m.get("content").and_then(|v| v.as_str()))
                        .unwrap_or("");
                    let reasoning =
                        message.and_then(|m| m.get("reasoning_content").and_then(|v| v.as_str()));
                    let tool_calls_json =
                        message.and_then(|m| m.get("tool_calls").and_then(|v| v.as_array()));
                    let upstream_finish_reason = choices
                        .and_then(|c| c.get("finish_reason").and_then(|v| v.as_str()))
                        .unwrap_or("stop");

                    let anthropic_stop_reason = match upstream_finish_reason {
                        "tool_calls" => "tool_use",
                        _ => "end_turn",
                    };

                    let tool_calls = tool_calls_json.map(|arr| {
                        let original_count = arr.len();
                        let converted: Vec<_> = arr
                            .iter()
                            .filter_map(|tc| {
                                let name = tc
                                    .get("function")
                                    .and_then(|f| f.get("name").and_then(|v| v.as_str()))?;
                                let id = tc.get("id").and_then(|v| v.as_str()).map(String::from);
                                let args = tc
                                    .get("function")
                                    .and_then(|f| f.get("arguments").and_then(|v| v.as_str()))
                                    .and_then(|s| serde_json::from_str(s).ok())
                                    .unwrap_or_default();
                                Some(crate::capability::ToolCall {
                                    id,
                                    name: name.to_string(),
                                    arguments: args,
                                })
                            })
                            .collect::<Vec<_>>();
                        if converted.len() < original_count {
                            tracing::warn!(
                                dropped = original_count - converted.len(),
                                "dropped malformed tool calls in Anthropic NonStreaming conversion"
                            );
                        }
                        converted
                    });
                    let tool_calls_opt = tool_calls.filter(|v| !v.is_empty());

                    let usage_ref = usage.as_ref();
                    let prompt_tokens = usage_ref.map(|u| u.prompt_tokens as usize).unwrap_or(0);
                    let completion_tokens =
                        usage_ref.map(|u| u.completion_tokens as usize).unwrap_or(0);

                    let response_body = anthropic::serialize_response(
                        "msg-gw",
                        model,
                        if content.is_empty() {
                            None
                        } else {
                            Some(content)
                        },
                        tool_calls_opt,
                        Some(anthropic_stop_reason),
                        reasoning,
                        prompt_tokens,
                        completion_tokens,
                    )
                    .map_err(|e| ProtocolError::Internal {
                        message: e.to_string(),
                        protocol,
                    })?;

                    let body: serde_json::Value = serde_json::from_str(&response_body)
                        .expect("serialize_response produced invalid JSON — this is a bug");
                    Ok((StatusCode::OK, Json(body)).into_response())
                }
                // Unreachable: the Responses guard at the top of this function
                // returns early. Real implementation lands in a later task.
                Protocol::Responses => {
                    unreachable!("Responses guarded at collect_non_streaming entry")
                }
            }
        }
        crate::streaming::StreamMode::Normalized(mut stream) => {
            use tokio_stream::StreamExt;
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
                    // Unreachable: guarded at collect_non_streaming entry.
                    Protocol::Responses => {
                        unreachable!("Responses guarded at collect_non_streaming entry")
                    }
                }
            } else {
                match protocol {
                    Protocol::OpenAI => "tool_calls",
                    Protocol::Anthropic => "tool_use",
                    // Unreachable: guarded at collect_non_streaming entry.
                    Protocol::Responses => {
                        unreachable!("Responses guarded at collect_non_streaming entry")
                    }
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
                // Unreachable: guarded at collect_non_streaming entry.
                Protocol::Responses => {
                    unreachable!("Responses guarded at collect_non_streaming entry")
                }
            };

            guard.finish(true, final_usage, first_token_ms);

            let body: serde_json::Value =
                serde_json::from_str(&response_body).unwrap_or_else(|e| {
                    serde_json::json!({"error": {"message": format!(
                        "serialization error: {}",
                        e
                    )}})
                });
            Ok((StatusCode::OK, Json(body)).into_response())
        }
        crate::streaming::StreamMode::Passthrough(_) => {
            guard.finish_err();
            Err(ProtocolError::Internal {
                message: "Passthrough not supported for non-streaming".to_string(),
                protocol,
            })
        }
    }
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

    if unified_req.stream {
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

/// Handle a Responses-format request.
///
/// * If the selected provider advertises `supports_responses`, forward the raw
///   body via [`Provider::responses_passthrough`] (zero-parse, end-to-end
///   passthrough — the upstream already speaks the Responses wire format).
/// * Otherwise, fall through to a conversion placeholder (filled in by a later
///   task). Until that lands, the placeholder surfaces a clear `Internal` error.
async fn responses_handler_impl(
    body: String,
    router: &dyn Router,
    mut guard: crate::metrics::guard::RequestGuard,
) -> Result<Response, ProtocolError> {
    let protocol = Protocol::Responses;
    let model = extract_model_field(&body);

    // Sync guard model with the upstream model for accurate metrics.
    if let Some(upstream) = router.resolve_upstream_model(&model) {
        guard.set_model(upstream);
    }

    let provider_name = match router.resolve(&model) {
        Ok(n) => n,
        Err(e) => {
            guard.finish_err();
            return Err(map_router_error(e, protocol));
        }
    };
    let provider = match router.get_provider(&provider_name) {
        Some(p) => p,
        None => {
            guard.finish_err();
            return Err(ProtocolError::Internal {
                message: "provider not found".into(),
                protocol,
            });
        }
    };

    let caps = provider.capabilities();
    let stream = extract_stream_field(&body);

    if caps.supports_responses {
        let mode = match provider.responses_passthrough(body, stream).await {
            Ok(m) => m,
            Err(e) => {
                guard.finish_err();
                return Err(map_router_error(
                    crate::router::RouterError::from(e),
                    protocol,
                ));
            }
        };
        dispatch_responses_stream_mode(mode, guard, protocol)
    } else {
        // Conversion mode: Responses client → Chat Completions upstream.
        // Parse body once; thread through boundary checks + request parser
        // to avoid re-parsing the same JSON (issue #1, code review).
        let parsed: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                guard.finish_err();
                return Err(ProtocolError::Parse { message: e.to_string(), protocol });
            }
        };
        // Boundary validation (stateless principle): the gateway does not retain
        // conversation state, so stateful Responses features must be rejected
        // with a clear 400 rather than silently dropped.
        if has_previous_response_id(&parsed) || has_store_true(&parsed) {
            guard.finish_err();
            return Err(ProtocolError::Parse {
                message: "previous_response_id / store not supported in conversion mode; \
                          send full input array"
                    .into(),
                protocol,
            });
        }
        if has_builtin_tools_value(&parsed) {
            guard.finish_err();
            return Err(ProtocolError::Parse {
                message: "built-in tools (web_search/file_search/computer_use) not supported \
                          by upstream"
                    .into(),
                protocol,
            });
        }
        let unified = match responses::parse_responses_request(&body) {
            Ok(r) => r,
            Err(e) => {
                guard.finish_err();
                return Err(ProtocolError::Parse {
                    message: e.to_string(),
                    protocol,
                });
            }
        };
        let unified = if let Some(up) = router.resolve_upstream_model(&model) {
            let mut u = unified;
            u.model = up;
            u
        } else {
            unified
        };
        // Force Normalized (allow_passthrough=false): the upstream speaks Chat
        // Completions, so we must convert every chunk to the Responses wire format.
        let stream_mode = match router.chat_stream(unified, false).await {
            Ok(m) => m,
            Err(e) => {
                guard.finish_err();
                return Err(map_router_error(e, protocol));
            }
        };
        dispatch_responses_conversion(stream_mode, &model, guard, protocol)
    }
}

/// Check whether the pre-parsed request body carries a non-empty `previous_response_id`.
fn has_previous_response_id(parsed: &serde_json::Value) -> bool {
    parsed
        .get("previous_response_id")
        .and_then(|f| f.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// Check whether the pre-parsed request body sets `store: true`.
fn has_store_true(parsed: &serde_json::Value) -> bool {
    parsed
        .get("store")
        .and_then(|f| f.as_bool())
        .unwrap_or(false)
}

/// Check whether the pre-parsed request body's `tools` array contains any non-function
/// (built-in) tool entry (`web_search_preview`, `file_search`, `computer_use`, ...).
fn has_builtin_tools_value(parsed: &serde_json::Value) -> bool {
    let Some(tools) = parsed.get("tools").and_then(|t| t.as_array()) else {
        return false;
    };
    tools.iter().any(|t| {
        t.get("type")
            .and_then(|ty| ty.as_str())
            .map(|s| s != "function")
            .unwrap_or(true)
    })
}

/// Dispatch a conversion-mode `StreamMode` to a final HTTP response.
///
/// * `Normalized` → SSE response using [`serializer::ResponsesStreamState`] (the
///   conversion path always normalizes — see `chat_stream(.., false)`).
/// * `NonStreaming` → JSON response via [`responses::serialize_responses_response`].
/// * `Passthrough` → programming error; conversion forces Normalized.
fn dispatch_responses_conversion(
    stream_mode: crate::streaming::StreamMode,
    model: &str,
    guard: crate::metrics::guard::RequestGuard,
    protocol: Protocol,
) -> Result<Response, ProtocolError> {
    use crate::streaming::StreamMode;
    match stream_mode {
        StreamMode::NonStreaming(json) => {
            let usage = json.get("usage").map(|u| crate::metrics::TokenUsage {
                prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                completion_tokens: u
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32,
            });
            let body = match responses::serialize_responses_response(&json, model) {
                Ok(v) => v,
                Err(e) => {
                    guard.finish_err();
                    return Err(ProtocolError::Internal {
                        message: e,
                        protocol,
                    });
                }
            };
            let first_token_ms = Some(guard.elapsed_ms());
            guard.finish(true, usage, first_token_ms);
            Ok((StatusCode::OK, Json(body)).into_response())
        }
        StreamMode::Normalized(stream) => {
            let tracker = guard.into_tracker();
            Ok(responses_conversion_sse(stream, model, tracker))
        }
        StreamMode::Passthrough(_) => {
            guard.finish_err();
            Err(ProtocolError::Internal {
                message: "conversion mode must not produce Passthrough".into(),
                protocol,
            })
        }
    }
}

/// Build an SSE response from a Normalized chunk stream, serializing each chunk
/// via [`serializer::ResponsesStreamState`] (Responses `response.*` events).
fn responses_conversion_sse(
    stream: crate::streaming::LLMStream,
    model: &str,
    mut tracker: crate::metrics::StreamTracker,
) -> Response {
    let model_name = model.to_string();
    let mut state = serializer::ResponsesStreamState::new();

    let body_stream = futures::StreamExt::map(stream, move |chunk_result| match chunk_result {
        Ok(chunk) => {
            tracker.set_ttft(tracker.start.elapsed().as_millis() as u64);
            if let Some(usage) = &chunk.usage {
                tracker.set_tokens(usage.clone());
            }
            let text = state.serialize_chunk(&chunk, &model_name);
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
    });

    stream_dispatch::sse_response(Box::pin(body_stream))
}

/// Extract the `model` field from a JSON body (best-effort, defaults to "unknown").
fn extract_model_field(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Extract the `stream` flag from a JSON body (defaults to `false`).
fn extract_stream_field(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("stream")?.as_bool())
        .unwrap_or(false)
}

/// Map a Responses `usage` object (`input_tokens` / `output_tokens`) into the
/// gateway's [`TokenUsage`].
fn extract_responses_usage(usage: &serde_json::Value) -> crate::metrics::TokenUsage {
    crate::metrics::TokenUsage {
        prompt_tokens: usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        completion_tokens: usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
    }
}

/// Dispatch a Responses passthrough `StreamMode` to a final HTTP response.
///
/// * `NonStreaming` → return the upstream JSON as-is (with usage→metrics).
/// * `Passthrough`  → forward as an SSE response through `stream_dispatch`.
/// * `Normalized`   → programming error; passthrough must never normalize.
fn dispatch_responses_stream_mode(
    stream_mode: crate::streaming::StreamMode,
    guard: crate::metrics::guard::RequestGuard,
    protocol: Protocol,
) -> Result<Response, ProtocolError> {
    use crate::streaming::StreamMode;
    match stream_mode {
        StreamMode::NonStreaming(json) => {
            let usage = json.get("usage").map(extract_responses_usage);
            let elapsed_ms = guard.elapsed_ms();
            guard.finish(true, usage, Some(elapsed_ms));
            Ok((StatusCode::OK, Json(json)).into_response())
        }
        StreamMode::Passthrough(bytes) => {
            let tracker = guard.into_tracker();
            Ok(stream_dispatch::forward_as_sse_response(
                StreamMode::Passthrough(bytes),
                protocol,
                "",
                tracker,
            ))
        }
        StreamMode::Normalized(_) => Err(ProtocolError::Internal {
            message: "responses_passthrough returned Normalized".into(),
            protocol,
        }),
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
            // Responses follows the OpenAI error envelope shape.
            Protocol::Responses => serde_json::json!({
                "error": {
                    "type": if status.is_client_error() { "invalid_request_error" } else { "server_error" },
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
    use crate::metrics::TokenUsage;
    use crate::provider::UnifiedRequest;
    use crate::streaming::{LLMChunk, StreamError};
    use async_trait::async_trait;
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};

    // ── Mock Router ──────────────────────────────────────────────────

    struct MockRouter {
        stream_mode: Arc<Mutex<Option<crate::streaming::StreamMode>>>,
    }

    impl MockRouter {
        fn with_non_streaming(json: serde_json::Value) -> Self {
            Self {
                stream_mode: Arc::new(Mutex::new(Some(
                    crate::streaming::StreamMode::NonStreaming(json),
                ))),
            }
        }

        fn with_router_error() -> Self {
            Self {
                stream_mode: Arc::new(Mutex::new(None)),
            }
        }

        fn with_normalized_chunks(chunks: Vec<Result<LLMChunk, StreamError>>) -> Self {
            let stream = tokio_stream::iter(chunks);
            let mode = crate::streaming::StreamMode::Normalized(Box::pin(stream));
            Self {
                stream_mode: Arc::new(Mutex::new(Some(mode))),
            }
        }
    }

    #[async_trait]
    impl crate::router::Router for MockRouter {
        fn resolve(&self, _model: &str) -> Result<String, crate::router::RouterError> {
            Ok("mock".to_string())
        }
        fn resolve_upstream_model(&self, _model: &str) -> Option<String> {
            None
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
        ) -> Result<crate::streaming::StreamMode, crate::router::RouterError> {
            self.stream_mode
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| crate::router::RouterError::Internal("no stream configured".into()))
        }
    }

    fn make_guard() -> crate::metrics::guard::RequestGuard {
        let (emitter, _reader) = crate::metrics::init();
        crate::metrics::guard::RequestGuard::start(emitter, "mock", "test-model")
    }

    fn make_request() -> UnifiedRequest {
        UnifiedRequest {
            model: "test-model".to_string(),
            messages: vec![],
            tools: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            extra_params: serde_json::Map::new(),
        }
    }

    /// Build a typical upstream OpenAI JSON response.
    fn openai_chat_json(content: &str, reasoning: Option<&str>) -> serde_json::Value {
        let mut msg = serde_json::json!({
            "role": "assistant",
            "content": content
        });
        if let Some(r) = reasoning {
            msg["reasoning_content"] = serde_json::json!(r);
        }
        serde_json::json!({
            "id": "chatcmpl-upstream-123",
            "object": "chat.completion",
            "created": 1712345678,
            "model": "upstream-model",
            "choices": [{
                "index": 0,
                "message": msg,
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 25,
                "total_tokens": 40
            }
        })
    }

    // ── NonStreaming: OpenAI pass-through ───────────────────────────

    #[tokio::test]
    async fn non_streaming_openai_passes_through_upstream_json() {
        let upstream_json = openai_chat_json("Hello world", None);
        let router = MockRouter::with_non_streaming(upstream_json.clone());
        let guard = make_guard();

        let result = collect_non_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            guard,
        )
        .await
        .unwrap();

        assert_eq!(result.status(), StatusCode::OK);
        // The response content-type should be application/json (not text/event-stream)
        assert_eq!(
            result.headers().get("content-type").unwrap(),
            "application/json"
        );
        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        // The upstream JSON should be preserved as-is for OpenAI protocol
        assert_eq!(v["id"], "chatcmpl-upstream-123");
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["model"], "upstream-model");
        assert_eq!(v["choices"][0]["message"]["content"], "Hello world");
        assert_eq!(v["usage"]["prompt_tokens"], 15);
        assert_eq!(v["usage"]["completion_tokens"], 25);
        assert_eq!(v["usage"]["total_tokens"], 40);
    }

    #[tokio::test]
    async fn non_streaming_openai_preserves_reasoning_content() {
        let upstream_json = openai_chat_json("answer", Some("thinking..."));
        let router = MockRouter::with_non_streaming(upstream_json);
        let guard = make_guard();

        let result = collect_non_streaming(
            &router,
            make_request(),
            "test-model",
            Protocol::OpenAI,
            guard,
        )
        .await
        .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(
            v["choices"][0]["message"]["reasoning_content"],
            "thinking..."
        );
        assert_eq!(v["choices"][0]["message"]["content"], "answer");
    }

    #[tokio::test]
    async fn non_streaming_openai_preserves_tool_calls() {
        let upstream_json = serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1712345678,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"Tokyo\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        });
        let router = MockRouter::with_non_streaming(upstream_json);
        let guard = make_guard();

        let result = collect_non_streaming(
            &router,
            make_request(),
            "test-model",
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

    // ── NonStreaming: Anthropic conversion ──────────────────────────

    #[tokio::test]
    async fn non_streaming_anthropic_converts_upstream_json() {
        let upstream_json = openai_chat_json("Hello from Anthropic", None);
        let router = MockRouter::with_non_streaming(upstream_json);
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

        assert_eq!(result.status(), StatusCode::OK);
        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        // Anthropic format
        assert_eq!(v["type"], "message");
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["model"], "claude-3");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "Hello from Anthropic");
        assert_eq!(v["stop_reason"], "end_turn");
    }

    #[tokio::test]
    async fn non_streaming_anthropic_converts_reasoning_to_thinking() {
        let upstream_json = openai_chat_json("answer", Some("reasoning..."));
        let router = MockRouter::with_non_streaming(upstream_json);
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

        // First block: thinking
        assert_eq!(v["content"][0]["type"], "thinking");
        assert_eq!(v["content"][0]["thinking"], "reasoning...");
        // Second block: text
        assert_eq!(v["content"][1]["type"], "text");
        assert_eq!(v["content"][1]["text"], "answer");
    }

    // ── NonStreaming: Error handling ────────────────────────────────

    #[tokio::test]
    async fn non_streaming_router_error_returns_error() {
        let router = MockRouter::with_router_error();
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

    // ── NonStreaming: Passthrough still rejected for non-streaming ──

    #[tokio::test]
    async fn non_streaming_passthrough_rejected() {
        let stream = tokio_stream::iter(vec![Ok::<_, StreamError>(bytes::Bytes::from("data"))]);
        let mode = crate::streaming::StreamMode::Passthrough(Box::pin(stream));
        let router = MockRouter {
            stream_mode: Arc::new(Mutex::new(Some(mode))),
        };
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

    // ── forward_streaming: protocol-aware passthrough ────────────────
    //
    // Passthrough requires upstream byte format == entry protocol format.
    // All fustapi providers are OpenAI-compatible (CC-up), so:
    //   - OpenAI entry  → passthrough OK (format match)
    //   - Anthropic entry → must convert (would otherwise be mixed-format garbage:
    //                          stream_dispatch wrap_anthropic on OpenAI bytes)

    struct PassthroughCaptureRouter {
        allow_passthrough: Arc<Mutex<Option<bool>>>,
    }

    #[async_trait]
    impl crate::router::Router for PassthroughCaptureRouter {
        fn resolve(&self, _model: &str) -> Result<String, crate::router::RouterError> {
            Ok("mock".to_string())
        }
        fn resolve_upstream_model(&self, _model: &str) -> Option<String> {
            None
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
            allow_passthrough: bool,
        ) -> Result<crate::streaming::StreamMode, crate::router::RouterError> {
            *self.allow_passthrough.lock().unwrap() = Some(allow_passthrough);
            Ok(crate::streaming::StreamMode::Normalized(Box::pin(
                tokio_stream::iter(vec![] as Vec<Result<LLMChunk, StreamError>>),
            )))
        }
    }

    #[tokio::test]
    async fn forward_streaming_anthropic_forces_conversion_not_passthrough() {
        let router = PassthroughCaptureRouter {
            allow_passthrough: Arc::new(Mutex::new(None)),
        };
        let req = UnifiedRequest {
            model: "m".into(),
            messages: vec![],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            extra_params: serde_json::Map::new(),
        };
        let tracker = make_guard().into_tracker();
        let _ = forward_streaming(&router, req, "m", Protocol::Anthropic, tracker).await;
        assert_eq!(
            *router.allow_passthrough.lock().unwrap(),
            Some(false),
            "Anthropic entry must force conversion (allow_passthrough=false), not passthrough"
        );
    }

    #[tokio::test]
    async fn forward_streaming_openai_allows_passthrough() {
        let router = PassthroughCaptureRouter {
            allow_passthrough: Arc::new(Mutex::new(None)),
        };
        let req = UnifiedRequest {
            model: "m".into(),
            messages: vec![],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            extra_params: serde_json::Map::new(),
        };
        let tracker = make_guard().into_tracker();
        let _ = forward_streaming(&router, req, "m", Protocol::OpenAI, tracker).await;
        assert_eq!(*router.allow_passthrough.lock().unwrap(), Some(true));
    }

    // ── detect_protocol: /v1/responses routing ──────────────────────

    #[test]
    fn detect_protocol_responses_path() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(
            detect_protocol("/v1/responses", &headers),
            Protocol::Responses
        );
    }

    #[test]
    fn detect_protocol_responses_does_not_clobber_others() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(
            detect_protocol("/v1/chat/completions", &headers),
            Protocol::OpenAI
        );
        assert_eq!(
            detect_protocol("/v1/messages", &headers),
            Protocol::Anthropic
        );
    }

    // ── Backward compatibility: Normalized stream still works ───────

    #[tokio::test]
    async fn non_streaming_normalized_path_still_works() {
        let chunks = vec![
            Ok(LLMChunk {
                content: Some("Hello ".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: None,
            }),
            Ok(LLMChunk {
                content: Some("world".to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
                usage: Some(TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                }),
            }),
        ];
        let router = MockRouter::with_normalized_chunks(chunks);
        let guard = make_guard();
        let result =
            collect_non_streaming(&router, make_request(), "gpt-4", Protocol::OpenAI, guard)
                .await
                .unwrap();

        let body = result.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(v["choices"][0]["message"]["content"], "Hello world");
        assert_eq!(v["usage"]["prompt_tokens"], 10);
        assert_eq!(v["usage"]["completion_tokens"], 5);
    }
}
