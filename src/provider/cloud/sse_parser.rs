//! OpenAI-compatible SSE stream parser.
//!
//! Parses Server-Sent Events byte streams from OpenAI-compatible APIs into
//! normalized `LLMChunk` streams. Handles content deltas, reasoning content,
//! tool call accumulation, usage extraction, and upstream error detection.

use crate::streaming::{LLMChunk, LLMStream};

/// Pending tool call being accumulated across SSE chunks.
///
/// OpenAI streams tool call arguments incrementally — `name` arrives first,
/// then zero or more `arguments` fragments. This struct holds the partial
/// state until the tool call is complete.
struct PendingTool {
    id: Option<String>,
    name: String,
    args: String,
}

/// Mutable state for parsing an OpenAI SSE byte stream.
struct SseParseState {
    buffer: bytes::BytesMut,
    pending_tool: Option<PendingTool>,
}

impl SseParseState {
    fn new() -> Self {
        Self {
            buffer: bytes::BytesMut::new(),
            pending_tool: None,
        }
    }

    /// Feed newly arrived bytes into the internal buffer.
    fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Drain one complete line from the buffer, if available.
    fn take_line(&mut self) -> Option<String> {
        let pos = self.buffer.iter().position(|&b| b == b'\n')?;
        let line_bytes = self.buffer.split_to(pos + 1);
        let line = std::str::from_utf8(&line_bytes[..pos]).unwrap_or("");
        Some(line.trim().to_string())
    }

    /// Flush the pending tool call (if any) into an `LLMChunk`.
    fn flush_tool(&mut self, done: bool) -> Option<LLMChunk> {
        let tool = self.pending_tool.take()?;
        let args = serde_json::from_str(&tool.args).unwrap_or(serde_json::json!({}));
        Some(LLMChunk {
            reasoning_content: None,
            usage: None,
            content: None,
            tool_call: Some(crate::capability::ToolCall {
                id: tool.id,
                name: tool.name,
                arguments: args,
            }),
            done,
        })
    }

    /// Process one SSE `data:` line, returning zero or one `LLMChunk`.
    fn process_line(
        &mut self,
        line: &str,
    ) -> Option<Result<LLMChunk, crate::streaming::StreamError>> {
        let data = line[5..].trim();
        if data == "[DONE]" || data == " [DONE]" || data == "[DONE] " {
            if self.pending_tool.is_some() {
                return Some(Ok(self.flush_tool(true).unwrap()));
            }
            return Some(Ok(LLMChunk {
                reasoning_content: None,
                usage: None,
                content: None,
                tool_call: None,
                done: true,
            }));
        }
        if data.is_empty() {
            return None;
        }
        let v: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return None,
        };

        // Detect upstream errors reported via finish_reason.
        if let Some(fr) = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
            && (fr.contains("context_window") || fr.contains("context_length") || fr == "error")
        {
            return Some(Err(crate::streaming::StreamError::Provider(format!(
                "upstream error: {fr}"
            ))));
        }

        // Usage-only chunk (stream_options.include_usage).
        if let Some(usage) = extract_usage(&v) {
            let has_choices = v
                .get("choices")
                .and_then(|c| c.as_array())
                .is_some_and(|a| !a.is_empty());
            if !has_choices {
                return Some(Ok(LLMChunk {
                    reasoning_content: None,
                    usage: Some(usage),
                    content: None,
                    tool_call: None,
                    done: false,
                }));
            }
        }

        let choices = v.get("choices")?;
        let choice = choices.get(0)?;
        let delta = choice.get("delta")?;
        let chunk_usage = extract_usage(&v);

        // Reasoning content (DeepSeek).
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(Ok(LLMChunk {
                usage: None,
                content: None,
                reasoning_content: Some(reasoning.to_string()),
                tool_call: None,
                done: false,
            }));
        }

        // Text content.
        if let Some(content) = delta
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(Ok(LLMChunk {
                usage: chunk_usage,
                content: Some(content.to_string()),
                reasoning_content: None,
                tool_call: None,
                done: false,
            }));
        }

        // Tool call accumulation.
        if let Some(tool_calls) = delta.get("tool_calls")
            && let Some(tc) = tool_calls.get(0)
            && let Some(func) = tc.get("function")
        {
            let new_id = tc.get("id").and_then(|i| i.as_str()).map(String::from);
            let new_name = func.get("name").and_then(|n| n.as_str());
            let new_args = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");

            if let Some(name) = new_name {
                // New tool call starts → flush previous.
                let flush = self.flush_tool(false);
                self.pending_tool = Some(PendingTool {
                    id: new_id,
                    name: name.to_string(),
                    args: new_args.to_string(),
                });
                return flush.map(Ok);
            }
            // Arguments continuation → append.
            if let Some(ref mut pending) = self.pending_tool {
                pending.args.push_str(new_args);
            }
        }

        None
    }
}

/// Extract token usage from an SSE JSON value, if present.
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

    let state = SseParseState::new();

    let s = stream::unfold((stream, state), |(mut stream, mut state)| async move {
        loop {
            // Drain any complete lines from the buffer.
            while let Some(line) = state.take_line() {
                if line.starts_with("data:")
                    && line.len() > 5
                    && let Some(chunk) = state.process_line(&line)
                {
                    return Some((chunk, (stream, state)));
                }
            }

            // Buffer is empty — fetch more bytes from upstream.
            match stream.next().await {
                Some(Ok(bytes)) => {
                    state.feed(&bytes);
                }
                Some(Err(e)) => {
                    // Classify the reqwest error at the source so intermittent
                    // upstream drops — e.g. "error decoding response body"
                    // (hyper chunked connection closed mid-stream) — are
                    // distinguishable from timeouts / connect failures in the
                    // logs. The downstream `stream chunk error` log only shows
                    // the Display string, not the reqwest error category.
                    tracing::warn!(
                        error = %e,
                        is_timeout = e.is_timeout(),
                        is_connect = e.is_connect(),
                        is_decode = e.is_decode(),
                        is_body = e.is_body(),
                        is_request = e.is_request(),
                        "upstream stream error"
                    );
                    return Some((
                        Err(crate::streaming::StreamError::Provider(e.to_string())),
                        (stream, state),
                    ));
                }
                None => {
                    // Stream ended — flush pending tool call.
                    if let Some(tc) = state.flush_tool(true) {
                        return Some((Ok(tc), (stream, state)));
                    }
                    return None;
                }
            }
        }
    });
    Box::pin(s) as LLMStream
}

// SSE parser behaviour is exercised through the integration tests in
// tests/api_tests.rs (streaming chat completions, streaming messages,
// tool call, and error responses).
