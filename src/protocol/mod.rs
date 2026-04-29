//! Protocol dispatch layer.
//!
//! Routes incoming requests to the appropriate protocol parser (OpenAI or Anthropic).

pub mod anthropic;
pub mod openai;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

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

/// Dispatch a request to the appropriate protocol handler.
pub async fn dispatch_request(protocol: Protocol, body: String) -> Result<Response, ProtocolError> {
    match protocol {
        Protocol::OpenAI => openai_handler(body).await,
        Protocol::Anthropic => anthropic_handler(body).await,
    }
}

/// Handle an OpenAI-format request. Returns a canned response for now.
async fn openai_handler(body: String) -> Result<Response, ProtocolError> {
    // Try to parse as OpenAI request to validate format
    if let Err(e) = openai::parse_chat_request(&body) {
        return Err(ProtocolError::Parse(e.to_string()));
    }

    // Return a canned OpenAI response (mock provider)
    let response = openai::serialize_response(
        "chatcmpl-mock-1",
        "mock",
        Some("Hello from FustAPI! (mock)"),
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

/// Handle an Anthropic-format request. Returns a canned response for now.
async fn anthropic_handler(body: String) -> Result<Response, ProtocolError> {
    // Try to parse as Anthropic request to validate format
    if let Err(e) = anthropic::parse_messages_request(&body) {
        return Err(ProtocolError::Parse(e.to_string()));
    }

    // Return a canned Anthropic response (mock provider)
    let response = anthropic::serialize_response(
        "msg-mock-1",
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
