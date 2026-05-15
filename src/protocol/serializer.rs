//! Protocol-specific SSE serialization for streaming responses.
//!
//! Converts internal `LLMChunk`s into OpenAI or Anthropic SSE wire format.
//! The `AnthropicStreamState` struct encapsulates the content-block state
//! machine required by the Anthropic streaming protocol.

use crate::streaming::LLMChunk;

/// State machine for Anthropic content block lifecycle during streaming.
///
/// Anthropic requires explicit `content_block_start` / `content_block_stop`
/// events around each content block, with a monotonically increasing index.
/// This struct tracks which blocks are open so they can be closed correctly
/// when transitioning between text, reasoning, and tool-call blocks.
pub struct AnthropicStreamState {
    block_index: usize,
    need_block_start: bool,
    text_block_open: bool,
    reasoning_block_open: bool,
    has_tool_calls: bool,
}

impl Default for AnthropicStreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicStreamState {
    pub fn new() -> Self {
        Self {
            block_index: 0,
            need_block_start: true,
            text_block_open: false,
            reasoning_block_open: false,
            has_tool_calls: false,
        }
    }

    /// Serialize an `LLMChunk` into Anthropic SSE events, managing block
    /// lifecycle. Returns the SSE text (one or more `event: / data:` lines).
    pub fn serialize_chunk(&mut self, chunk: &LLMChunk, model: &str) -> String {
        let mut prefix = String::new();

        // Close any open block before transitioning to a different block type.
        let needs_close = (self.reasoning_block_open
            && (chunk.content.is_some() || chunk.tool_call.is_some()))
            || (self.text_block_open && chunk.tool_call.is_some());
        if needs_close {
            let block_stop = serde_json::json!({
                "type": "content_block_stop",
                "index": self.block_index
            });
            prefix.push_str(&format!(
                "event: content_block_stop\ndata: {}\n\n",
                serde_json::to_string(&block_stop).unwrap_or_default()
            ));
            self.block_index += 1;
            self.reasoning_block_open = false;
            self.text_block_open = false;
            self.need_block_start = true;
        }

        let stop_reason = if self.has_tool_calls || chunk.tool_call.is_some() {
            "tool_use"
        } else {
            "end_turn"
        };

        let s = super::anthropic::serialize_stream_event(
            chunk,
            "msg_gw",
            model,
            &self.block_index,
            self.need_block_start,
            stop_reason,
        );

        if chunk.reasoning_content.is_some() {
            self.need_block_start = false;
            self.reasoning_block_open = true;
        }
        if chunk.content.is_some() {
            self.need_block_start = false;
            self.text_block_open = true;
        }
        if chunk.tool_call.is_some() {
            self.has_tool_calls = true;
            self.block_index += 1;
            self.need_block_start = true;
            self.text_block_open = false;
            self.reasoning_block_open = false;
        }
        if chunk.done {
            self.block_index += 1;
            self.need_block_start = true;
            self.text_block_open = false;
            self.reasoning_block_open = false;
        }

        format!("{prefix}{s}")
    }
}

/// Serialize an `LLMChunk` into an OpenAI SSE chunk string.
pub fn serialize_openai_chunk(chunk: &LLMChunk, model: &str, include_role: bool) -> String {
    let ts = current_timestamp();
    let mut lines = Vec::new();

    if include_role {
        let role_data = serde_json::json!({
            "id": "chatcmpl-gw",
            "object": "chat.completion.chunk",
            "created": ts,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        });
        lines.push(format!("data: {role_data}"));
    }

    if let Some(ref content) = chunk.content
        && !content.is_empty()
    {
        let data = serde_json::json!({
            "id": "chatcmpl-gw",
            "object": "chat.completion.chunk",
            "created": ts,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"content": content},
                "finish_reason": null
            }]
        });
        lines.push(format!("data: {data}"));
    }

    if let Some(ref reasoning) = chunk.reasoning_content
        && !reasoning.is_empty()
    {
        let data = serde_json::json!({
            "id": "chatcmpl-gw",
            "object": "chat.completion.chunk",
            "created": ts,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": reasoning},
                "finish_reason": null
            }]
        });
        lines.push(format!("data: {data}"));
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
                        "id": tc.id.clone().unwrap_or_else(|| "call_emulated".to_string()),
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
        lines.push(format!("data: {data}"));
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
        lines.push(format!("data: {data}"));
        lines.push("data: [DONE]".to_string());
    }

    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n\n") + "\n\n"
    }
}

/// Extract token usage from raw SSE bytes in passthrough mode.
///
/// Scans SSE data lines for usage fields from upstream providers that
/// support `stream_options.include_usage`. Maintains a small sliding
/// buffer to handle cross-chunk boundaries.
pub fn extract_usage_from_sse_bytes(buf: &[u8]) -> Option<crate::metrics::TokenUsage> {
    let text = String::from_utf8_lossy(buf);
    for line in text.lines() {
        let data = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"));
        let Some(data) = data else { continue };
        let data = data.trim();
        if data == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(data)
            && let Some(usage) = v.get("usage")
        {
            let pt = usage
                .get("prompt_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            let ct = usage
                .get("completion_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            if pt > 0 || ct > 0 {
                return Some(crate::metrics::TokenUsage {
                    prompt_tokens: pt,
                    completion_tokens: ct,
                });
            }
        }
    }
    None
}

fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::tool::ToolCall;

    fn chunk(content: Option<&str>) -> LLMChunk {
        LLMChunk {
            content: content.map(String::from),
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: None,
        }
    }

    fn reasoning_chunk(content: &str) -> LLMChunk {
        LLMChunk {
            content: None,
            reasoning_content: Some(content.to_string()),
            tool_call: None,
            done: false,
            usage: None,
        }
    }

    fn tool_call_chunk(name: &str, args: &str) -> LLMChunk {
        LLMChunk {
            content: None,
            reasoning_content: None,
            tool_call: Some(ToolCall {
                id: Some("call_test".to_string()),
                name: name.to_string(),
                arguments: serde_json::from_str(args).unwrap(),
            }),
            done: false,
            usage: None,
        }
    }

    fn done_chunk() -> LLMChunk {
        LLMChunk {
            content: None,
            reasoning_content: None,
            tool_call: None,
            done: true,
            usage: None,
        }
    }

    // ── AnthropicStreamState tests ──────────────────────────────────

    #[test]
    fn anthropic_text_block_emits_stream_event() {
        let mut state = AnthropicStreamState::new();
        let output = state.serialize_chunk(&chunk(Some("hello")), "model-a");
        assert!(output.contains("event: content_block_delta")
            || output.contains("event: content_block_start"));
    }

    #[test]
    fn anthropic_reasoning_then_text_closes_block() {
        let mut state = AnthropicStreamState::new();
        let r = state.serialize_chunk(&reasoning_chunk("thinking..."), "model-a");
        // Anthropic maps reasoning_content to "thinking" blocks.
        assert!(r.contains("thinking"));

        let t = state.serialize_chunk(&chunk(Some("answer")), "model-a");
        // Must close reasoning block before opening text block.
        assert!(t.contains("content_block_stop"));
        assert!(t.contains("content_block_delta"));
    }

    #[test]
    fn anthropic_text_then_tool_call_closes_block() {
        let mut state = AnthropicStreamState::new();
        state.serialize_chunk(&chunk(Some("text")), "model-a");

        let tc = state.serialize_chunk(
            &tool_call_chunk("get_weather", r#"{"city":"SF"}"#),
            "model-a",
        );
        assert!(tc.contains("content_block_stop"));
    }

    #[test]
    fn anthropic_done_sets_stop_reason_end_turn() {
        let mut state = AnthropicStreamState::new();
        let out = state.serialize_chunk(&done_chunk(), "model-a");
        assert!(out.contains("end_turn") || out.contains("message_stop"));
    }

    #[test]
    fn anthropic_tool_call_sets_stop_reason_tool_use() {
        let mut state = AnthropicStreamState::new();
        let _ = state.serialize_chunk(
            &tool_call_chunk("fn", r#"{}"#),
            "model-a",
        );
        let done = state.serialize_chunk(&done_chunk(), "model-a");
        assert!(done.contains("tool_use"));
    }

    #[test]
    fn anthropic_multiple_text_chunks_share_block() {
        let mut state = AnthropicStreamState::new();
        let first = state.serialize_chunk(&chunk(Some("a")), "m");
        assert!(first.contains("content_block_start"));

        let second = state.serialize_chunk(&chunk(Some("b")), "m");
        // Subsequent text chunks should NOT start a new block.
        assert!(!second.contains("content_block_start"));
    }

    // ── serialize_openai_chunk tests ────────────────────────────────

    #[test]
    fn openai_first_chunk_includes_role() {
        let c = chunk(Some("hi"));
        let out = serialize_openai_chunk(&c, "gpt-4", true);
        assert!(out.contains(r#""role":"assistant""#));
    }

    #[test]
    fn openai_later_chunk_omits_role() {
        let c = chunk(Some("hi"));
        let out = serialize_openai_chunk(&c, "gpt-4", false);
        assert!(!out.contains("role"));
    }

    #[test]
    fn openai_content_delta() {
        let c = chunk(Some("hello world"));
        let out = serialize_openai_chunk(&c, "gpt-4", false);
        assert!(out.contains(r#""content":"hello world""#));
        assert!(out.contains("chat.completion.chunk"));
    }

    #[test]
    fn openai_empty_content_skipped() {
        let c = chunk(Some(""));
        let out = serialize_openai_chunk(&c, "gpt-4", false);
        assert!(!out.contains("content"));
    }

    #[test]
    fn openai_reasoning_delta() {
        let c = reasoning_chunk("let me think");
        let out = serialize_openai_chunk(&c, "deepseek-r1", false);
        assert!(out.contains("reasoning_content"));
        assert!(out.contains("let me think"));
    }

    #[test]
    fn openai_tool_call_delta() {
        let c = tool_call_chunk("search", r#"{"q":"rust"}"#);
        let out = serialize_openai_chunk(&c, "gpt-4", false);
        assert!(out.contains("tool_calls"));
        assert!(out.contains(r#""name":"search""#));
        assert!(out.contains(r#""arguments":"{\"q\":\"rust\"}""#));
    }

    #[test]
    fn openai_done_emits_stop_and_done_marker() {
        let c = done_chunk();
        let out = serialize_openai_chunk(&c, "gpt-4", false);
        assert!(out.contains(r#""finish_reason":"stop""#));
        assert!(out.contains("[DONE]"));
    }

    #[test]
    fn openai_empty_chunk_returns_empty_string() {
        let c = LLMChunk {
            content: None,
            reasoning_content: None,
            tool_call: None,
            done: false,
            usage: None,
        };
        let out = serialize_openai_chunk(&c, "gpt-4", false);
        assert!(out.is_empty());
    }

    #[test]
    fn openai_tool_call_with_object_args() {
        let c = LLMChunk {
            content: None,
            reasoning_content: None,
            tool_call: Some(ToolCall {
                id: Some("call_obj".to_string()),
                name: "run".to_string(),
                arguments: serde_json::json!({"x": 1}),
            }),
            done: false,
            usage: None,
        };
        let out = serialize_openai_chunk(&c, "gpt-4", false);
        // Object arguments should be serialized to a JSON string.
        assert!(out.contains("arguments"));
        assert!(out.contains(r#""name":"run""#));
    }

    // ── extract_usage_from_sse_bytes tests ──────────────────────────

    #[test]
    fn extract_usage_valid() {
        let data = b"data: {\"id\":\"x\",\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20}}\n\n";
        let usage = extract_usage_from_sse_bytes(data).unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
    }

    #[test]
    fn extract_usage_skips_done_marker() {
        let data = b"data: [DONE]\n\n";
        assert!(extract_usage_from_sse_bytes(data).is_none());
    }

    #[test]
    fn extract_usage_skips_no_usage() {
        let data = b"data: {\"id\":\"x\",\"choices\":[]}\n\n";
        assert!(extract_usage_from_sse_bytes(data).is_none());
    }

    #[test]
    fn extract_usage_zero_tokens_skipped() {
        let data = b"data: {\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}\n\n";
        assert!(extract_usage_from_sse_bytes(data).is_none());
    }

    #[test]
    fn extract_usage_invalid_json_skipped() {
        let data = b"data: {not json}\n\n";
        assert!(extract_usage_from_sse_bytes(data).is_none());
    }

    #[test]
    fn extract_usage_from_multiple_lines() {
        let data = b"data: {\"id\":\"x\"}\ndata: {\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":8}}\n\n";
        let usage = extract_usage_from_sse_bytes(data).unwrap();
        assert_eq!(usage.prompt_tokens, 5);
        assert_eq!(usage.completion_tokens, 8);
    }
}
