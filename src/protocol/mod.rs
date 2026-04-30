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
    let unified_req =
        openai::parse_chat_request(&body).map_err(|e| ProtocolError::Parse(e.to_string()))?;
    let model = unified_req.model.clone();
    let provider_req = unified_req.clone();

    if is_streaming(&body) {
        forward_streaming(router, provider_req, &model).await
    } else {
        collect_non_streaming(router, provider_req, &model).await
    }
}

/// Forward provider stream as SSE response.  
async fn forward_streaming(
    router: &dyn Router,
    request: crate::provider::UnifiedRequest,
    model: &str,
) -> Result<Response, ProtocolError> {
    use crate::streaming::LLMStream;

    let model = model.to_string(); // Clone to make it 'static
    let stream: LLMStream = router
        .chat_stream(request)
        .await
        .map_err(|e| ProtocolError::Internal(e.to_string()))?;

    let body_stream = stream.map(move |chunk_result| match chunk_result {
        Ok(chunk) => Ok(create_sse_chunk(&chunk, &model)),
        Err(e) => Err(format!("Stream error: {}", e)),
    });

    let response = Response::builder()
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
        });

    Ok(response)
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
) -> Result<Response, ProtocolError> {
    use tokio_stream::StreamExt;

    let mut stream = router
        .chat_stream(request)
        .await
        .map_err(|e| ProtocolError::Internal(e.to_string()))?;

    let mut chunks = Vec::new();
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => chunks.push(chunk),
            Err(e) => return Err(ProtocolError::Internal(e.to_string())),
        }
    }

    let content = chunks
        .iter()
        .filter_map(|c| c.content.clone())
        .collect::<Vec<_>>()
        .join("");

    let response = openai::serialize_response(
        "chatcmpl-gateway-1",
        model,
        if content.is_empty() {
            None
        } else {
            Some(&content)
        },
        None,
        "stop",
        10,
        5,
        15,
    )
    .map_err(|e| ProtocolError::Internal(e.to_string()))?;

    Ok((
        StatusCode::OK,
        Json(serde_json::from_str::<serde_json::Value>(&response).unwrap()),
    )
        .into_response())
}

/// Handle an Anthropic-format request. Forwards to the appropriate provider.  
async fn anthropic_handler(_body: String, _router: &dyn Router) -> Result<Response, ProtocolError> {
    let response = anthropic::serialize_response(
        "msg-gateway-1",
        "claude-3",
        Some("Hello from FustAPI! (mock)"),
        None,
        Some("end_turn"),
    )
    .map_err(|e| ProtocolError::Internal(e.to_string()))?;

    Ok((
        StatusCode::OK,
        Json(serde_json::from_str::<serde_json::Value>(&response).unwrap()),
    )
        .into_response())
}

/// Error type for protocol dispatch failures.  
#[derive(Debug)]
pub enum ProtocolError {
    Parse(String),
    Internal(String),
}

impl IntoResponse for ProtocolError {
    fn into_response(self) -> Response {
        let (status, error_msg) = match self {
            ProtocolError::Parse(e) => (StatusCode::BAD_REQUEST, e),
            ProtocolError::Internal(e) => (StatusCode::INTERNAL_SERVER_ERROR, e),
        };

        (
            status,
            Json(serde_json::json!({ "error": { "message": error_msg } })),
        )
            .into_response()
    }
}
