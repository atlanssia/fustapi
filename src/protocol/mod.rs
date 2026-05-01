//! Protocol dispatch layer.
//!
//! Routes incoming requests to the appropriate protocol parser (OpenAI or Anthropic).

pub mod anthropic;
pub mod openai;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tokio_stream::StreamExt;

use crate::router::Router;
use crate::streaming::LLMChunk;

/// Protocol identifier for dispatch decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAI,
    Anthropic,
}

/// Detect protocol from request path and headers.
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
        .map(|v| v.get("stream").is_some_and(|s| s.as_bool() == Some(true)))
        .unwrap_or(false)
}

/// Dispatch a request to the appropriate protocol handler.
pub async fn dispatch_request(
    protocol: Protocol,
    body: String,
    router: &dyn Router,
) -> Result<Response, ProtocolError> {
    match protocol {
        Protocol::OpenAI => openai_handler(body, router).await,
        Protocol::Anthropic => anthropic_handler(body, router).await,
    }
}

/// Handle an OpenAI-format request. Forwards to the appropriate provider.
async fn openai_handler(body: String, router: &dyn Router) -> Result<Response, ProtocolError> {
    let unified_req = openai::parse_chat_request(&body).map_err(|e| ProtocolError::Parse {
        message: e.to_string(),
        protocol: Protocol::OpenAI,
    })?;
    let model = unified_req.model.clone();
    let provider_req = unified_req.clone();

    if is_streaming(&body) {
        forward_streaming(router, provider_req, &model, Protocol::OpenAI).await
    } else {
        collect_non_streaming(router, provider_req, &model, Protocol::OpenAI).await
    }
}

/// Forward provider stream as SSE response.
async fn forward_streaming(
    router: &dyn Router,
    request: crate::provider::UnifiedRequest,
    model: &str,
    protocol: Protocol,
) -> Result<Response, ProtocolError> {
    // If client requested Anthropic protocol, we MUST normalize if the provider is OpenAI-compatible (which all are currently).
    // Passthrough only works if the provider's native SSE matches the client's requested protocol.
    let model_name = model.to_string();
    let allow_passthrough = protocol == Protocol::OpenAI;

    let stream_mode = router.chat_stream(request, allow_passthrough).await.map_err(|e| {
        ProtocolError::Internal {
            message: e.to_string(),
            protocol,
        }
    })?;

    match stream_mode {
        crate::streaming::StreamMode::Normalized(stream) => {
            use futures::StreamExt;
            let mut block_index = 0;
            let model_name_start = model_name.to_string();

            let mut body_stream = futures::StreamExt::map(stream, move |chunk_result| match chunk_result {
                Ok(chunk) => {
                    let text = match protocol {
                        Protocol::OpenAI => create_sse_chunk(&chunk, &model_name),
                        Protocol::Anthropic => {
                            let s = anthropic::serialize_stream_event(
                                &chunk,
                                "msg_gw",
                                &model_name,
                                &block_index,
                            );
                            if chunk.content.is_some() || chunk.tool_call.is_some() {
                                block_index += 1;
                            }
                            s
                        }
                    };
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(text))
                }
                Err(e) => {
                    let err_json = serde_json::json!({
                        "error": {
                            "message": e.to_string(),
                            "type": "internal_error"
                        }
                    });
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(format!("data: {}\n\n", err_json)))
                }
            }).boxed();

            if protocol == Protocol::Anthropic {
                let start_bytes = axum::body::Bytes::from(anthropic::serialize_message_start("msg_gw", &model_name_start));
                let stop_bytes = axum::body::Bytes::from(anthropic::serialize_message_stop());
                
                let combined = futures::StreamExt::chain(
                    futures::StreamExt::chain(
                        futures::stream::once(async move { Ok::<_, std::convert::Infallible>(start_bytes) }),
                        body_stream
                    ),
                    futures::stream::once(async move { Ok::<_, std::convert::Infallible>(stop_bytes) })
                );
                
                let response = Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(axum::body::Body::from_stream(combined))
                    .unwrap();
                return Ok(response);
            }

            let response = Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                .header("cache-control", "no-cache")
                .body(axum::body::Body::from_stream(body_stream))
                .unwrap();

            Ok(response)
        }
        crate::streaming::StreamMode::Passthrough(byte_stream) => {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                .header("cache-control", "no-cache")
                .header("connection", "keep-alive")
                .body(axum::body::Body::from_stream(byte_stream))
                .unwrap_or_else(|_| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(axum::body::Body::empty())
                        .unwrap()
                });

            Ok(response)
        }
    }
}

/// Create an SSE-formatted chunk from an LLMChunk.  
fn create_sse_chunk(chunk: &LLMChunk, model: &str) -> String {
    let ts = current_timestamp();
    let mut lines = Vec::new();

    if let Some(ref content) = chunk.content
        && !content.is_empty()
    {
        let escaped = escape_json(content);
        let data = serde_json::json!({
            "id": "chatcmpl-gw",
            "object": "chat.completion.chunk",
            "created": ts,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"content": escaped},
                "finish_reason": null
            }]
        });
        lines.push(format!("data: {}", data));
    }

    if let Some(ref tc) = chunk.tool_call {
        let args_str = if tc.arguments.is_string() {
            tc.arguments.as_str().unwrap().to_string()
        } else {
            serde_json::to_string(&tc.arguments).unwrap_or_default()
        };
        let data = serde_json::json!({
            "id": "chatcmpl-gw",
            "object": "chat.completion.chunk",
            "created": ts,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_emulated",
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": args_str
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        lines.push(format!("data: {}", data));
    }

    if chunk.done {
        let data = serde_json::json!({
            "id": "chatcmpl-gw",
            "object": "chat.completion.chunk",
            "created": ts,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        lines.push(format!("data: {}", data));
        lines.push("data: [DONE]".to_string());
    }

    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n\n") + "\n\n"
    }
}

/// Get current UTC timestamp in seconds.  
fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Escape a string for JSON embedding in SSE data.  
fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Collect all chunks and return a single non-streaming response.  
async fn collect_non_streaming(
    router: &dyn Router,
    request: crate::provider::UnifiedRequest,
    model: &str,
    protocol: Protocol,
) -> Result<Response, ProtocolError> {
    use tokio_stream::StreamExt;

    let stream_mode =
        router
            .chat_stream(request, false)
            .await
            .map_err(|e| ProtocolError::Internal {
                message: e.to_string(),
                protocol,
            })?;

    let mut stream = match stream_mode {
        crate::streaming::StreamMode::Normalized(stream) => stream,
        crate::streaming::StreamMode::Passthrough(_) => {
            return Err(ProtocolError::Internal {
                message: "Passthrough not supported for non-streaming".to_string(),
                protocol,
            });
        }
    };

    let mut chunks = Vec::new();
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => chunks.push(chunk),
            Err(e) => {
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
        .collect::<Vec<_>>()
        .join("");

    let tool_calls: Vec<_> = chunks.iter().filter_map(|c| c.tool_call.clone()).collect();

    let tool_calls_opt = if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls.clone())
    };

    let finish_reason = if !tool_calls.is_empty() {
        match protocol {
            Protocol::OpenAI => "tool_calls",
            Protocol::Anthropic => "tool_use",
        }
    } else {
        match protocol {
            Protocol::OpenAI => "stop",
            Protocol::Anthropic => "end_turn",
        }
    };

    let response_body = match protocol {
        Protocol::OpenAI => openai::serialize_response(
            "chatcmpl-gw",
            model,
            if content.is_empty() {
                None
            } else {
                Some(&content)
            },
            tool_calls_opt,
            finish_reason,
            10,
            5,
            15,
        )
        .map_err(|e| ProtocolError::Internal {
            message: e.to_string(),
            protocol,
        })?,
        Protocol::Anthropic => anthropic::serialize_response(
            "msg-gw",
            model,
            if content.is_empty() {
                None
            } else {
                Some(&content)
            },
            tool_calls_opt,
            Some(finish_reason),
        )
        .map_err(|e| ProtocolError::Internal {
            message: e.to_string(),
            protocol,
        })?,
    };

    Ok((
        StatusCode::OK,
        Json(serde_json::from_str::<serde_json::Value>(&response_body).unwrap()),
    )
        .into_response())
}

/// Handle an Anthropic-format request. Forwards to the appropriate provider.
async fn anthropic_handler(body: String, router: &dyn Router) -> Result<Response, ProtocolError> {
    let unified_req =
        anthropic::parse_messages_request(&body).map_err(|e| ProtocolError::Parse {
            message: e.to_string(),
            protocol: Protocol::Anthropic,
        })?;
    let model = unified_req.model.clone();
    let provider_req = unified_req.clone();

    if is_streaming(&body) {
        forward_streaming(router, provider_req, &model, Protocol::Anthropic).await
    } else {
        collect_non_streaming(router, provider_req, &model, Protocol::Anthropic).await
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
