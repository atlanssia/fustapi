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
///
/// It also emits a `signature_delta` event before closing reasoning blocks
/// so that clients (Claude Code) preserve `reasoning_content` across
/// multi-turn conversations with providers like DeepSeek.
pub struct AnthropicStreamState {
    block_index: usize,
    need_block_start: bool,
    text_block_open: bool,
    reasoning_block_open: bool,
    tool_block_open: bool,
    has_tool_calls: bool,
}

/// Placeholder signature emitted with Anthropic thinking blocks that originate
/// from non-Anthropic providers (e.g. DeepSeek `reasoning_content`).
/// Aliases [`super::anthropic::THINKING_SIGNATURE`] to avoid duplicating the value.
const PROXY_SIGNATURE: &str = super::anthropic::THINKING_SIGNATURE;

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
            tool_block_open: false,
            has_tool_calls: false,
        }
    }

    /// Build a `signature_delta` SSE event for the given block index.
    fn emit_signature_delta(index: usize) -> String {
        let sig_delta = serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": { "type": "signature_delta", "signature": PROXY_SIGNATURE }
        });
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&sig_delta).unwrap_or_default()
        )
    }

    /// Serialize an `LLMChunk` into Anthropic SSE events, managing block
    /// lifecycle. Returns the SSE text (one or more `event: / data:` lines).
    pub fn serialize_chunk(&mut self, chunk: &LLMChunk, _model: &str) -> String {
        let mut prefix = String::new();

        // Close any open block before transitioning to a different block type.
        // tool_block_open is excluded for done chunks — the done case in
        // emit_chunk_event handles the final content_block_stop itself.
        let needs_close = (self.reasoning_block_open
            && (chunk.content.is_some() || chunk.tool_call.is_some()))
            || (self.text_block_open
                && (chunk.tool_call.is_some() || chunk.reasoning_content.is_some()))
            || (self.tool_block_open && !chunk.done);
        if needs_close {
            // When closing a reasoning block, emit the signature_delta first.
            if self.reasoning_block_open {
                prefix.push_str(&Self::emit_signature_delta(self.block_index));
            }

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
            self.tool_block_open = false;
            self.need_block_start = true;
        }

        let stop_reason = if self.has_tool_calls || chunk.tool_call.is_some() {
            "tool_use"
        } else {
            "end_turn"
        };

        // Determine whether the reasoning block is being closed by this chunk.
        let close_reasoning =
            chunk.done && (self.reasoning_block_open || chunk.reasoning_content.is_some());

        let s = self.emit_chunk_event(chunk, self.need_block_start, stop_reason, close_reasoning);

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
            self.tool_block_open = true;
            self.need_block_start = false;
            self.text_block_open = false;
            self.reasoning_block_open = false;
        }
        if chunk.done {
            self.block_index += 1;
            self.need_block_start = true;
            self.text_block_open = false;
            self.reasoning_block_open = false;
            self.tool_block_open = false;
        }

        format!("{prefix}{s}")
    }

    /// Emit SSE events for one chunk (the former `serialize_stream_event`
    /// from `anthropic.rs`, now a private method — all content-block event
    /// generation is internal to this struct).
    fn emit_chunk_event(
        &self,
        chunk: &LLMChunk,
        need_block_start: bool,
        stop_reason: &str,
        close_reasoning: bool,
    ) -> String {
        let index = self.block_index;
        let mut s = String::new();
        let had_reasoning = chunk.reasoning_content.is_some();

        if let Some(reasoning) = &chunk.reasoning_content {
            if need_block_start {
                let block_start = serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "thinking", "thinking": "", "signature": "" }
                });
                s.push_str(&format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::to_string(&block_start).unwrap_or_default()
                ));
            }

            let delta = serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "thinking_delta", "thinking": reasoning }
            });
            s.push_str(&format!(
                "event: content_block_delta\ndata: {}\n\n",
                serde_json::to_string(&delta).unwrap_or_default()
            ));

            if close_reasoning {
                s.push_str(&Self::emit_signature_delta(index));
            }
        }

        if let Some(text) = &chunk.content {
            if need_block_start {
                let block_start = serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "text", "text": "" }
                });
                s.push_str(&format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::to_string(&block_start).unwrap_or_default()
                ));
            }

            let delta = serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            });
            s.push_str(&format!(
                "event: content_block_delta\ndata: {}\n\n",
                serde_json::to_string(&delta).unwrap_or_default()
            ));
        }

        if let Some(tc) = &chunk.tool_call {
            let tool_id = tc.id.clone().unwrap_or_else(|| format!("toolu_{index}"));
            if need_block_start {
                let block_start = serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": tool_id,
                        "name": tc.name,
                        "input": {}
                    }
                });
                s.push_str(&format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::to_string(&block_start).unwrap_or_default()
                ));
            }
            let json_str = tc.arguments.to_string();
            let delta = serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": json_str }
            });
            s.push_str(&format!(
                "event: content_block_delta\ndata: {}\n\n",
                serde_json::to_string(&delta).unwrap_or_default()
            ));
        }

        if chunk.done {
            // Bare [DONE] while reasoning block was still open → signature first.
            if close_reasoning && !had_reasoning {
                s.push_str(&Self::emit_signature_delta(index));
            }

            let block_stop = serde_json::json!({
                "type": "content_block_stop",
                "index": index
            });
            s.push_str(&format!(
                "event: content_block_stop\ndata: {}\n\n",
                serde_json::to_string(&block_stop).unwrap_or_default()
            ));

            let msg_delta = serde_json::json!({
                "type": "message_delta",
                "delta": {
                    "type": "stop_reason",
                    "stop_reason": stop_reason
                },
                "usage": {
                    "output_tokens": 0
                }
            });
            s.push_str(&format!(
                "event: message_delta\ndata: {}\n\n",
                serde_json::to_string(&msg_delta).unwrap_or_default()
            ));
        }

        s
    }
}

/// State machine for converting a normalized `LLMChunk` stream into
/// OpenAI Responses API (`response.*`) SSE events.
///
/// Mirrors [`AnthropicStreamState`]'s role for the Anthropic protocol. The
/// Responses protocol requires explicit output-item / content-part lifecycle
/// events (`output_item.added`, `content_part.added`, `output_text.delta`,
/// `output_item.done`, ...) around text, reasoning, and function-call items.
///
/// This struct tracks which items/parts are open so they are closed in the
/// correct order when transitioning between block types or when the stream
/// terminates.
pub struct ResponsesStreamState {
    started: bool,
    message_open: bool,
    text_part_open: bool,
    reasoning_open: bool,
    usage: Option<crate::metrics::TokenUsage>,
    response_id: String,
}

impl Default for ResponsesStreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponsesStreamState {
    pub fn new() -> Self {
        Self {
            started: false,
            message_open: false,
            text_part_open: false,
            reasoning_open: false,
            usage: None,
            response_id: format!("resp_{}", short_id()),
        }
    }

    /// Serialize an `LLMChunk` into Responses API SSE events, managing the
    /// output-item / content-part lifecycle. Returns the SSE text (one or
    /// more `event: / data:` lines).
    pub fn serialize_chunk(&mut self, chunk: &LLMChunk, model: &str) -> String {
        let mut out = String::new();

        // 1. First chunk: emit response.created once.
        if !self.started {
            let created = serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "in_progress",
                    "model": model,
                }
            });
            out.push_str(&sse_event("response.created", &created));
            self.started = true;
        }

        // 2. reasoning_content (non-empty).
        if let Some(ref rc) = chunk.reasoning_content
            && !rc.is_empty()
        {
            // Close any open text_part / message before reasoning.
            self.close_text_and_message(&mut out);
            if !self.reasoning_open {
                let added = serde_json::json!({
                    "type": "response.output_item.added",
                    "item": {
                        "type": "reasoning",
                        "id": "rs_1",
                        "summary": []
                    }
                });
                out.push_str(&sse_event("response.output_item.added", &added));
                self.reasoning_open = true;
            }
            let delta = serde_json::json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": "rs_1",
                "delta": rc,
            });
            out.push_str(&sse_event("response.reasoning_summary_text.delta", &delta));
        }

        // 3. content (non-empty).
        if let Some(ref content) = chunk.content
            && !content.is_empty()
        {
            // Close reasoning block if open.
            if self.reasoning_open {
                let done = serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "reasoning",
                        "id": "rs_1",
                        "summary": []
                    }
                });
                out.push_str(&sse_event("response.output_item.done", &done));
                self.reasoning_open = false;
            }
            if !self.message_open {
                let added = serde_json::json!({
                    "type": "response.output_item.added",
                    "item": {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "status": "in_progress",
                        "content": []
                    }
                });
                out.push_str(&sse_event("response.output_item.added", &added));
                self.message_open = true;
            }
            if !self.text_part_open {
                let part = serde_json::json!({
                    "type": "response.content_part.added",
                    "item_id": "msg_1",
                    "part": {
                        "type": "output_text",
                        "text": "",
                        "annotations": []
                    }
                });
                out.push_str(&sse_event("response.content_part.added", &part));
                self.text_part_open = true;
            }
            let delta = serde_json::json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "delta": content,
            });
            out.push_str(&sse_event("response.output_text.delta", &delta));
        }

        // 4. tool_call.
        if let Some(ref tc) = chunk.tool_call {
            // Close any open text_part / message before tool call.
            self.close_text_and_message(&mut out);
            // Also close reasoning if open.
            if self.reasoning_open {
                let done = serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {"type": "reasoning", "id": "rs_1", "summary": []}
                });
                out.push_str(&sse_event("response.output_item.done", &done));
                self.reasoning_open = false;
            }

            let tcid = tc.id.clone().unwrap_or_else(|| "call_gw".to_string());
            let args_str = if tc.arguments.is_string() {
                tc.arguments.as_str().unwrap_or("").to_string()
            } else {
                serde_json::to_string(&tc.arguments).unwrap_or_default()
            };

            let added = serde_json::json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": tcid,
                    "call_id": tcid,
                    "name": tc.name,
                    "arguments": ""
                }
            });
            out.push_str(&sse_event("response.output_item.added", &added));

            let arg_delta = serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "item_id": tcid,
                "delta": args_str,
            });
            out.push_str(&sse_event(
                "response.function_call_arguments.delta",
                &arg_delta,
            ));

            let arg_done = serde_json::json!({
                "type": "response.function_call_arguments.done",
                "item_id": tcid,
                "arguments": args_str,
            });
            out.push_str(&sse_event(
                "response.function_call_arguments.done",
                &arg_done,
            ));

            let item_done = serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "id": tcid,
                    "call_id": tcid,
                    "name": tc.name,
                    "arguments": args_str
                }
            });
            out.push_str(&sse_event("response.output_item.done", &item_done));
        }

        // 5. done.
        if chunk.done {
            // Store usage if provided.
            if chunk.usage.is_some() {
                self.usage = chunk.usage.clone();
            }

            // Close any open text_part + message.
            self.close_text_and_message(&mut out);

            // Build usage block (may be absent if provider omits it).
            let usage_json = self.usage.as_ref().map(|u| {
                let pt = u.prompt_tokens as u64;
                let ct = u.completion_tokens as u64;
                serde_json::json!({
                    "input_tokens": pt,
                    "output_tokens": ct,
                    "total_tokens": pt + ct,
                })
            });

            let mut response = serde_json::json!({
                "id": self.response_id,
                "object": "response",
                "status": "completed",
                "model": model,
            });
            if let Some(u) = usage_json {
                response["usage"] = u;
            }

            let completed = serde_json::json!({
                "type": "response.completed",
                "response": response,
            });
            out.push_str(&sse_event("response.completed", &completed));
        }

        out
    }

    /// Close an open `output_text` part and the `msg_1` message item, emitting
    /// the corresponding `done` events. Resets both flags. No-op if neither
    /// is open.
    fn close_text_and_message(&mut self, out: &mut String) {
        if self.text_part_open {
            let text_done = serde_json::json!({
                "type": "response.output_text.done",
                "item_id": "msg_1",
                "text": "",
            });
            out.push_str(&sse_event("response.output_text.done", &text_done));
            self.text_part_open = false;
        }
        if self.message_open {
            let item_done = serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "status": "completed",
                    "content": []
                }
            });
            out.push_str(&sse_event("response.output_item.done", &item_done));
            self.message_open = false;
        }
    }
}

/// Format a single SSE event: `event: <name>\ndata: <json>\n\n`.
///
/// This mirrors the inline formatting used by [`AnthropicStreamState`]
/// (`format!("event: X\ndata: {}\n\n", ...)`) so wire output is consistent
/// across protocols.
fn sse_event(name: &str, payload: &serde_json::Value) -> String {
    format!(
        "event: {name}\ndata: {}\n\n",
        serde_json::to_string(payload).unwrap_or_default()
    )
}

/// Generate a short, lowercase alphanumeric id for `resp_<id>`.
///
/// Uses a `OnceLock`-seeded PRNG; sufficient for a per-stream response id and
/// avoids pulling in `uuid`. The Responses protocol does not constrain the
/// shape beyond the `resp_` prefix used by OpenAI's reference server.
fn short_id() -> String {
    use std::sync::OnceLock;
    static SEED: OnceLock<u64> = OnceLock::new();
    let seed = *SEED.get_or_init(|| {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
    });

    // Simple xorshift to derive 24 hex chars from the seed + a thread-local
    // counter. Determinism within a process is acceptable: ids only need to be
    // unique within a stream, and the seed is process-time-based.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut x = seed.wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed));
    let mut buf = String::with_capacity(24);
    for _ in 0..24 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let nibble = (x & 0xF) as u8;
        let c = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        };
        buf.push(c as char);
    }
    buf
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
///
/// Field names are selected by `protocol`: the Responses API emits
/// `input_tokens`/`output_tokens`, while OpenAI/Anthropic emit
/// `prompt_tokens`/`completion_tokens`. The returned `TokenUsage`
/// always uses the canonical `prompt_tokens`/`completion_tokens` fields.
pub fn extract_usage_from_sse_bytes(
    buf: &[u8],
    protocol: super::Protocol,
) -> Option<crate::metrics::TokenUsage> {
    let (prompt_key, completion_key) = match protocol {
        super::Protocol::Responses => ("input_tokens", "output_tokens"),
        super::Protocol::OpenAI | super::Protocol::Anthropic => {
            ("prompt_tokens", "completion_tokens")
        }
    };
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
                .get(prompt_key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            let ct = usage
                .get(completion_key)
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
        assert!(
            output.contains("event: content_block_delta")
                || output.contains("event: content_block_start")
        );
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
    fn anthropic_reasoning_then_done_emits_signature_delta() {
        let mut state = AnthropicStreamState::new();
        let r = state.serialize_chunk(&reasoning_chunk("thinking..."), "model-a");
        assert!(r.contains("thinking_delta"));
        // Signature is NOT emitted yet — the reasoning block is still open.
        assert!(!r.contains("signature_delta"));

        let d = state.serialize_chunk(&done_chunk(), "model-a");
        // Bare DONE must emit signature_delta before content_block_stop.
        assert!(
            d.contains("signature_delta"),
            "expected signature_delta in done chunk:\n{d}"
        );
        assert!(d.contains("fustapi-transparent-proxy"));
        assert!(d.contains("content_block_stop"));
    }

    #[test]
    fn anthropic_reasoning_done_single_chunk_emits_signature_delta() {
        let mut state = AnthropicStreamState::new();
        let chunk = LLMChunk {
            content: None,
            reasoning_content: Some("quick thought".to_string()),
            tool_call: None,
            done: true,
            usage: None,
        };
        let out = state.serialize_chunk(&chunk, "model-a");
        // Single chunk: thinking_delta then signature_delta then content_block_stop.
        assert!(out.contains("thinking_delta"));
        assert!(
            out.contains("signature_delta"),
            "expected signature_delta in single reasoning+done chunk:\n{out}"
        );
        assert!(out.contains("fustapi-transparent-proxy"));
        assert!(out.contains("content_block_stop"));
        // Must be exactly ONE signature_delta.
        assert_eq!(
            out.matches("signature_delta").count(),
            1,
            "expected single signature_delta, got:\n{out}"
        );
    }

    #[test]
    fn anthropic_reasoning_then_text_emits_signature_delta_in_prefix() {
        let mut state = AnthropicStreamState::new();
        let _ = state.serialize_chunk(&reasoning_chunk("hmm"), "model-a");

        let text = state.serialize_chunk(&chunk(Some("answer")), "model-a");
        // The signature_delta must appear BEFORE the first content_block_stop
        // (the one that closes the reasoning block).
        assert!(
            text.contains("signature_delta"),
            "expected signature_delta in reasoning→text transition:\n{text}"
        );
        assert!(text.contains("fustapi-transparent-proxy"));
        let sig_pos = text.find("signature_delta").unwrap();
        let stop_pos = text.find("content_block_stop").unwrap();
        assert!(
            sig_pos < stop_pos,
            "signature_delta must precede content_block_stop"
        );
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
        let _ = state.serialize_chunk(&tool_call_chunk("fn", r#"{}"#), "model-a");
        let done = state.serialize_chunk(&done_chunk(), "model-a");
        assert!(done.contains("tool_use"));
    }

    #[test]
    fn anthropic_tool_call_done_emits_single_block_stop() {
        let mut state = AnthropicStreamState::new();
        let _ = state.serialize_chunk(&tool_call_chunk("search", r#"{"q":"rust"}"#), "model-a");
        let done = state.serialize_chunk(&done_chunk(), "model-a");
        // Must contain exactly ONE "event: content_block_stop" line —
        // the done case in serialize_stream_event closes the tool block;
        // needs_close must NOT also close it (would create a spurious
        // stop event at a stale index).
        let event_lines: Vec<_> = done
            .lines()
            .filter(|l| l.starts_with("event: content_block_stop"))
            .collect();
        assert_eq!(
            event_lines.len(),
            1,
            "expected single 'event: content_block_stop' for tool→done, got {}:\n{done}",
            event_lines.len()
        );
    }

    #[test]
    fn anthropic_consecutive_tool_calls_each_get_block_stop() {
        let mut state = AnthropicStreamState::new();
        let tc1 =
            state.serialize_chunk(&tool_call_chunk("read_file", r#"{"path":"/a"}"#), "model-a");
        let tc2 = state.serialize_chunk(
            &tool_call_chunk("write_file", r#"{"path":"/b"}"#),
            "model-a",
        );
        let done = state.serialize_chunk(&done_chunk(), "model-a");

        assert!(tc1.contains("content_block_start"));
        assert!(!tc1.contains("content_block_stop"));
        assert!(tc2.contains("content_block_stop"));
        assert!(tc2.contains("content_block_start"));
        assert!(done.contains("content_block_stop"));
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
        let data =
            b"data: {\"id\":\"x\",\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20}}\n\n";
        let usage = extract_usage_from_sse_bytes(data, crate::protocol::Protocol::OpenAI).unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
    }

    #[test]
    fn extract_usage_skips_done_marker() {
        let data = b"data: [DONE]\n\n";
        assert!(extract_usage_from_sse_bytes(data, crate::protocol::Protocol::OpenAI).is_none());
    }

    #[test]
    fn extract_usage_skips_no_usage() {
        let data = b"data: {\"id\":\"x\",\"choices\":[]}\n\n";
        assert!(extract_usage_from_sse_bytes(data, crate::protocol::Protocol::OpenAI).is_none());
    }

    #[test]
    fn extract_usage_zero_tokens_skipped() {
        let data = b"data: {\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}\n\n";
        assert!(extract_usage_from_sse_bytes(data, crate::protocol::Protocol::OpenAI).is_none());
    }

    #[test]
    fn extract_usage_invalid_json_skipped() {
        let data = b"data: {not json}\n\n";
        assert!(extract_usage_from_sse_bytes(data, crate::protocol::Protocol::OpenAI).is_none());
    }

    #[test]
    fn extract_usage_from_multiple_lines() {
        let data = b"data: {\"id\":\"x\"}\ndata: {\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":8}}\n\n";
        let usage = extract_usage_from_sse_bytes(data, crate::protocol::Protocol::OpenAI).unwrap();
        assert_eq!(usage.prompt_tokens, 5);
        assert_eq!(usage.completion_tokens, 8);
    }

    #[test]
    fn extract_usage_handles_responses_field_names() {
        let sse = b"data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":7,\"total_tokens\":12}}\n\n";
        let u = extract_usage_from_sse_bytes(sse, crate::protocol::Protocol::Responses).unwrap();
        assert_eq!(u.prompt_tokens, 5);
        assert_eq!(u.completion_tokens, 7);
    }

    // ── ResponsesStreamState tests ──────────────────────────────────

    #[test]
    fn responses_stream_emits_created_then_text_delta() {
        let mut s = ResponsesStreamState::new();
        let c1 = LLMChunk {
            content: Some("he".into()),
            ..Default::default()
        };
        let c2 = LLMChunk {
            content: Some("llo".into()),
            done: true,
            usage: Some(crate::metrics::TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
            }),
            ..Default::default()
        };
        let out1 = s.serialize_chunk(&c1, "m");
        assert!(out1.contains("response.created"));
        assert!(out1.contains("response.output_item.added"));
        assert!(out1.contains("response.output_text.delta"));
        let out2 = s.serialize_chunk(&c2, "m");
        assert!(out2.contains("response.output_text.delta"));
        assert!(out2.contains("response.completed"));
    }

    #[test]
    fn responses_stream_reasoning_then_text_transition() {
        let mut s = ResponsesStreamState::new();
        let rc = LLMChunk {
            reasoning_content: Some("think".into()),
            ..Default::default()
        };
        let out1 = s.serialize_chunk(&rc, "m");
        assert!(out1.contains("response.created"));
        assert!(
            out1.contains("response.output_item.added"),
            "must open reasoning item"
        );
        assert!(out1.contains("\"type\":\"reasoning\""));

        let txt = LLMChunk {
            content: Some("answer".into()),
            ..Default::default()
        };
        let out2 = s.serialize_chunk(&txt, "m");
        // reasoning item must be closed before message is opened
        assert!(
            out2.contains("response.output_item.done"),
            "must close reasoning item before opening message: {out2}"
        );
        assert!(
            out2.contains("response.output_item.added"),
            "must open message item after reasoning: {out2}"
        );
    }

    #[test]
    fn responses_stream_function_call_item() {
        let mut s = ResponsesStreamState::new();
        let fc = LLMChunk {
            tool_call: Some(crate::capability::ToolCall {
                id: Some("tc_1".into()),
                name: "get_weather".into(),
                arguments: serde_json::json!({"city":"NYC"}),
            }),
            done: true,
            usage: Some(crate::metrics::TokenUsage {
                prompt_tokens: 2,
                completion_tokens: 1,
            }),
            ..Default::default()
        };
        let out = s.serialize_chunk(&fc, "m");
        assert!(out.contains("response.created"));
        assert!(out.contains("\"type\":\"function_call\""));
        assert!(out.contains("response.function_call_arguments.delta"));
        assert!(out.contains("response.function_call_arguments.done"));
        assert!(out.contains("response.completed"));
    }

    #[test]
    fn responses_stream_full_item_lifecycle() {
        let mut s = ResponsesStreamState::new();
        // reasoning
        s.serialize_chunk(
            &LLMChunk {
                reasoning_content: Some("think".into()),
                ..Default::default()
            },
            "m",
        );
        // text
        s.serialize_chunk(
            &LLMChunk {
                content: Some("text".into()),
                ..Default::default()
            },
            "m",
        );
        // tool call + done
        let out = s.serialize_chunk(
            &LLMChunk {
                tool_call: Some(crate::capability::ToolCall {
                    id: Some("tc_2".into()),
                    name: "f".into(),
                    arguments: serde_json::json!({}),
                }),
                done: true,
                usage: Some(crate::metrics::TokenUsage {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                }),
                ..Default::default()
            },
            "m",
        );
        assert!(out.contains("response.completed"));
        assert!(out.contains("\"type\":\"function_call\""));
        let items: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("response.output_item.done"))
            .collect();
        assert!(
            !items.is_empty(),
            "must close at least one output item: {out}"
        );
    }
}
