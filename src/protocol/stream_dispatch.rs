//! Protocol-aware stream dispatch.
//!
//! Encapsulates the logic for converting an `LLMStream` (or `ByteStream` in
//! passthrough mode) into an SSE byte stream with protocol-correct serialization
//! and inline metrics tracking. The public interface is
//! [`forward_as_sse_response`], which replaces the inline closures that were
//! previously in `mod.rs`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use super::Protocol;
use crate::metrics::StreamTracker;
use crate::streaming::StreamMode;
use futures::StreamExt;

// ── Public entry point ───────────────────────────────────────────────

/// Convert a [`StreamMode`] into an SSE [`Response`], applying
/// protocol-specific serialization and metrics tracking.
///
/// * Normalized streams are serialized per-protocol (OpenAI or Anthropic SSE).
/// * Passthrough byte streams are forwarded unchanged (usage extraction only).
/// * The Anthropic stream is automatically wrapped with `message_start` /
///   `message_stop` events.
pub(crate) fn forward_as_sse_response(
    stream_mode: StreamMode,
    protocol: Protocol,
    model: &str,
    tracker: StreamTracker,
) -> Response {
    match stream_mode {
        StreamMode::Normalized(stream) => normalized_sse_response(stream, protocol, model, tracker),
        StreamMode::Passthrough(byte_stream) => {
            passthrough_sse_response(byte_stream, protocol, model, tracker)
        }
        StreamMode::NonStreaming(_) => {
            // Non-streaming responses are handled by collect_non_streaming().
            // This arm should not be reached in practice — forward_as_sse_response
            // is only called for streaming requests.
            tracing::warn!("NonStreaming reached in forward_as_sse_response — should not happen");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({
                    "error": {"message": "internal routing error"}
                })),
            )
                .into_response()
        }
    }
}

// ── Normalized path ──────────────────────────────────────────────────

fn normalized_sse_response(
    stream: crate::streaming::LLMStream,
    protocol: Protocol,
    model: &str,
    mut tracker: StreamTracker,
) -> Response {
    let model_name = model.to_string();
    let mut sent_role = false;
    let mut anthropic_state = super::serializer::AnthropicStreamState::new();
    let model_name_for_wrapper = model_name.clone();

    let body_stream = futures::StreamExt::map(stream, move |chunk_result| match chunk_result {
        Ok(chunk) => {
            tracker.set_ttft(tracker.start.elapsed().as_millis() as u64);
            if let Some(usage) = &chunk.usage {
                tracker.set_tokens(usage.clone());
            }
            let text = match protocol {
                Protocol::OpenAI => {
                    let include_role = !sent_role;
                    sent_role = true;
                    super::serializer::serialize_openai_chunk(&chunk, &model_name, include_role)
                }
                Protocol::Anthropic => anthropic_state.serialize_chunk(&chunk, &model_name),
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
    })
    .boxed();

    if protocol == Protocol::Anthropic {
        wrap_anthropic(body_stream, &model_name_for_wrapper)
    } else {
        sse_response(Box::pin(body_stream))
    }
}

// ── Passthrough path ─────────────────────────────────────────────────

fn passthrough_sse_response(
    byte_stream: crate::streaming::ByteStream,
    protocol: Protocol,
    model: &str,
    mut tracker: StreamTracker,
) -> Response {
    let mut buf = bytes::BytesMut::with_capacity(8192);
    let model_for_wrap = model.to_string();

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
                    if let Some(usage) = super::serializer::extract_usage_from_sse_bytes(&buf) {
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

    let pinned = Box::pin(body_stream);

    if protocol == Protocol::Anthropic {
        wrap_anthropic(pinned, &model_for_wrap)
    } else {
        sse_response(pinned)
    }
}

// ── Anthropic wrapper ────────────────────────────────────────────────

/// Wrap a body stream with `message_start` and `message_stop` SSE events,
/// as required by the Anthropic streaming protocol.
fn wrap_anthropic(
    body_stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<axum::body::Bytes, std::convert::Infallible>> + Send>,
    >,
    model: &str,
) -> Response {
    let start_bytes =
        axum::body::Bytes::from(super::anthropic::serialize_message_start("msg_gw", model));
    let stop_bytes = axum::body::Bytes::from(super::anthropic::serialize_message_stop());

    let combined = futures::StreamExt::chain(
        futures::StreamExt::chain(
            futures::stream::once(async move { Ok::<_, std::convert::Infallible>(start_bytes) }),
            body_stream,
        ),
        futures::stream::once(async move { Ok::<_, std::convert::Infallible>(stop_bytes) }),
    );

    sse_response(Box::pin(combined))
}

// ── SSE response builder ─────────────────────────────────────────────

/// Build an SSE response with correct headers from a byte stream.
pub(crate) fn sse_response(
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
