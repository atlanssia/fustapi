//! OpenAI-compatible SSE stream parser.
//!
//! Parses Server-Sent Events byte streams from OpenAI-compatible APIs into
//! normalized `LLMChunk` streams. Handles content deltas, reasoning content,
//! tool call accumulation, usage extraction, and upstream error detection.

use crate::streaming::{LLMChunk, LLMStream};

/// Parse an OpenAI-compatible SSE byte stream into a normalized `LLMStream`.
///
/// The stream is expected to contain `data: {...}\n\n` SSE frames, with
/// `data: [DONE]\n\n` signaling end-of-stream.
///
/// Handles:
/// - Content delta extraction
/// - Reasoning content extraction (DeepSeek-style)
/// - Tool call accumulation across multiple chunks
/// - Usage extraction (inline and usage-only chunks)
/// - Upstream error detection (context_window_exceeded, error finish_reason)
/// - Stream end with pending tool call flush
pub fn parse_openai_sse_stream(
    stream: impl futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Unpin + 'static,
) -> LLMStream {
    use futures::stream;
    use tokio_stream::StreamExt;

    let buffer = bytes::BytesMut::new();
    let tool_id: Option<String> = None;
    let tool_name: Option<String> = None;
    let tool_args = String::new();

    let s = stream::unfold(
        (stream, buffer, tool_id, tool_name, tool_args),
        |(mut stream, mut buffer, mut tool_id, mut tool_name, mut tool_args)| async move {
            fn extract_usage(v: &serde_json::Value) -> Option<crate::metrics::TokenUsage> {
                v.get("usage").and_then(|u| {
                    let pt = u
                        .get("prompt_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as u32;
                    let ct = u
                        .get("completion_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as u32;
                    if pt > 0 || ct > 0 {
                        Some(crate::metrics::TokenUsage {
                            prompt_tokens: pt,
                            completion_tokens: ct,
                        })
                    } else {
                        None
                    }
                })
            }
            loop {
                if let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                    let line_bytes = buffer.split_to(pos + 1);
                    let line = std::str::from_utf8(&line_bytes[..pos]).unwrap_or("").trim();

                    if line.starts_with("data:") && line.len() > 5 {
                        let data = line[5..].trim();
                        if data == "[DONE]" || data == " [DONE]" || data == "[DONE] " {
                            if let Some(name) = tool_name.take() {
                                let args = serde_json::from_str(&tool_args)
                                    .unwrap_or(serde_json::json!({}));
                                let tc = crate::capability::ToolCall {
                                    id: tool_id.take(),
                                    name,
                                    arguments: args,
                                };
                                return Some((
                                    Ok(LLMChunk {
                                        reasoning_content: None,
                                        usage: None,
                                        content: None,
                                        tool_call: Some(tc),
                                        done: true,
                                    }),
                                    (stream, buffer, tool_id, tool_name, tool_args),
                                ));
                            }
                            return Some((
                                Ok(LLMChunk {
                                    reasoning_content: None,
                                    usage: None,
                                    content: None,
                                    tool_call: None,
                                    done: true,
                                }),
                                (stream, buffer, tool_id, tool_name, tool_args),
                            ));
                        }
                        if !data.is_empty()
                            && let Ok(v) = serde_json::from_str::<serde_json::Value>(data)
                        {
                            // Detect upstream errors reported via finish_reason (e.g., GLM's
                            // "model_context_window_exceeded") — return as a stream error so
                            // the client sees a meaningful message instead of an empty response.
                            if let Some(fr) = v
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("finish_reason"))
                                .and_then(|v| v.as_str())
                            {
                                if fr.contains("context_window")
                                    || fr.contains("context_length")
                                    || fr == "error"
                                {
                                    return Some((
                                        Err(crate::streaming::StreamError::Provider(format!(
                                            "upstream error: {fr}"
                                        ))),
                                        (stream, buffer, tool_id, tool_name, tool_args),
                                    ));
                                }
                            }

                            // Handle usage-only chunk (sent when stream_options.include_usage is set).
                            // This arrives as a chunk with an empty choices array and a populated usage field.
                            if let Some(usage) = extract_usage(&v) {
                                // Only emit a separate chunk if there's no content/toolcall delta.
                                let has_choices = v
                                    .get("choices")
                                    .and_then(|c| c.as_array())
                                    .is_some_and(|a| !a.is_empty());
                                if !has_choices {
                                    return Some((
                                        Ok(LLMChunk {
                                            reasoning_content: None,
                                            usage: Some(usage),
                                            content: None,
                                            tool_call: None,
                                            done: false,
                                        }),
                                        (stream, buffer, tool_id, tool_name, tool_args),
                                    ));
                                }
                            }

                            let Some(choices) = v.get("choices") else {
                                continue;
                            };
                            let Some(choice) = choices.get(0) else {
                                continue;
                            };
                            let Some(delta) = choice.get("delta") else {
                                continue;
                            };
                            let chunk_usage = extract_usage(&v);
                            // Distinguish reasoning_content from content —
                            // DeepSeek requires reasoning_content to be echoed back.
                            let reasoning_str = delta
                                .get("reasoning_content")
                                .and_then(|c| c.as_str())
                                .filter(|s| !s.is_empty());
                            if let Some(reasoning) = reasoning_str {
                                return Some((
                                    Ok(LLMChunk {
                                        usage: None,
                                        content: None,
                                        reasoning_content: Some(reasoning.to_string()),
                                        tool_call: None,
                                        done: false,
                                    }),
                                    (stream, buffer, tool_id, tool_name, tool_args),
                                ));
                            }
                            let content_str = delta
                                .get("content")
                                .and_then(|c| c.as_str())
                                .filter(|s| !s.is_empty());
                            if let Some(content) = content_str {
                                return Some((
                                    Ok(LLMChunk {
                                        usage: chunk_usage,
                                        content: Some(content.to_string()),
                                        reasoning_content: None,
                                        tool_call: None,
                                        done: false,
                                    }),
                                    (stream, buffer, tool_id, tool_name, tool_args),
                                ));
                            }
                            if let Some(tool_calls) = delta.get("tool_calls")
                                && let Some(tc) = tool_calls.get(0)
                                && let Some(func) = tc.get("function")
                            {
                                let new_id =
                                    tc.get("id").and_then(|i| i.as_str()).map(String::from);
                                let new_name = func.get("name").and_then(|n| n.as_str());
                                let new_args =
                                    func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");

                                if let Some(name) = new_name {
                                    let mut flush_tc = None;
                                    if let Some(old_name) = tool_name.take() {
                                        let parsed_args = serde_json::from_str(&tool_args)
                                            .unwrap_or(serde_json::json!({}));
                                        flush_tc = Some(crate::capability::ToolCall {
                                            id: tool_id.take(),
                                            name: old_name,
                                            arguments: parsed_args,
                                        });
                                    }
                                    tool_id = new_id;
                                    tool_name = Some(name.to_string());
                                    tool_args = new_args.to_string();

                                    if flush_tc.is_some() {
                                        return Some((
                                            Ok(LLMChunk {
                                                reasoning_content: None,
                                                usage: None,
                                                content: None,
                                                tool_call: flush_tc,
                                                done: false,
                                            }),
                                            (stream, buffer, tool_id, tool_name, tool_args),
                                        ));
                                    }
                                } else {
                                    tool_args.push_str(new_args);
                                }
                            }
                        }
                    }
                    continue;
                }

                match stream.next().await {
                    Some(Ok(bytes)) => {
                        buffer.extend_from_slice(&bytes);
                    }
                    Some(Err(e)) => {
                        return Some((
                            Err(crate::streaming::StreamError::Provider(e.to_string())),
                            (stream, buffer, tool_id, tool_name, tool_args),
                        ));
                    }
                    None => {
                        if let Some(name) = tool_name.take() {
                            let args =
                                serde_json::from_str(&tool_args).unwrap_or(serde_json::json!({}));
                            let tc = crate::capability::ToolCall {
                                id: tool_id.take(),
                                name,
                                arguments: args,
                            };
                            return Some((
                                Ok(LLMChunk {
                                    reasoning_content: None,
                                    usage: None,
                                    content: None,
                                    tool_call: Some(tc),
                                    done: true,
                                }),
                                (stream, buffer, tool_id, tool_name, tool_args),
                            ));
                        }
                        return None;
                    }
                }
            }
        },
    );
    Box::pin(s) as LLMStream
}
