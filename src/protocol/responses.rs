//! OpenAI Responses API protocol parsing.
//!
//! Converts a Responses API request body (`input` / `instructions`) into the
//! gateway's canonical [`UnifiedRequest`]. The Responses API is the conversion
//! mode's request side: it accepts either a string `input` (treated as a single
//! user message) or an array of items, an optional `instructions` field mapped
//! to a system message, and a flat `tools` array (only `type == "function"`
//! entries are forwarded — built-in tools are filtered here; the caller
//! validates/rejects anything else).

use serde::Deserialize;
use serde_json::Value;

use crate::capability::ToolDefinition;
use crate::provider::{Message, Role, UnifiedRequest};

#[derive(Deserialize)]
struct ResponsesRequest {
    model: String,
    input: Value,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    tools: Vec<Value>,
    /// Capture all other request parameters (`temperature`, `top_p`, etc.)
    /// for passthrough into `extra_params`.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// Parse a Responses API JSON body into a [`UnifiedRequest`].
///
/// # Errors
///
/// Returns `serde_json::Error` if the body is not valid JSON or does not match
/// the expected shape (`model` is required, `input` must be a string or array).
pub fn parse_responses_request(json_str: &str) -> Result<UnifiedRequest, serde_json::Error> {
    let req: ResponsesRequest = serde_json::from_str(json_str)?;

    let mut messages = Vec::new();

    // instructions → System message (first, so it precedes user content).
    if let Some(instructions) = req.instructions.filter(|s| !s.is_empty()) {
        messages.push(Message {
            role: Role::System,
            content: instructions,
            images: None,
            tool_calls: None,
            tool_call_id: None,
            extras: None,
        });
    }

    // input: string → single user message; array → one message per item.
    match req.input {
        Value::String(text) => {
            messages.push(Message {
                role: Role::User,
                content: text,
                images: None,
                tool_calls: None,
                tool_call_id: None,
                extras: None,
            });
        }
        Value::Array(items) => {
            for item in items {
                if let Some(msg) = parse_input_item(&item) {
                    messages.push(msg);
                }
            }
        }
        // Non-string/non-array `input` is left to the caller to reject; here we
        // simply produce a request with no input messages.
        _ => {}
    }

    // function tools only — built-in tools (web_search, file_search, etc.)
    // are filtered here; the caller validates/rejects anything unexpected.
    let tools: Vec<ToolDefinition> = req
        .tools
        .iter()
        .filter_map(parse_function_tool)
        .collect();
    let tools_opt = if tools.is_empty() { None } else { Some(tools) };

    // temperature / max_output_tokens are surfaced as first-class fields;
    // everything else stays in extra_params.
    let temperature = req
        .extra
        .get("temperature")
        .and_then(|v| v.as_f64())
        .map(|f| f as f32);
    let max_tokens = req
        .extra
        .get("max_output_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    Ok(UnifiedRequest {
        model: req.model,
        messages,
        tools: tools_opt,
        stream: req.stream,
        temperature,
        max_tokens,
        extra_params: req.extra,
    })
}

/// Parse a single `input` array item into a [`Message`].
///
/// Each item carries a `role` (e.g., "user", "assistant") and `content` that
/// is either a plain string or an array of `{type, text}` parts. Unknown parts
/// are ignored; only text content is joined into the resulting `content`.
fn parse_input_item(item: &Value) -> Option<Message> {
    let role = match item.get("role").and_then(|v| v.as_str()) {
        Some("system") => Role::System,
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some("tool") => Role::Tool,
        // Items without a recognizable role are dropped — the caller validates
        // the full request shape elsewhere.
        _ => return None,
    };

    let content = match item.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let texts: Vec<String> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()).map(String::from))
                .collect();
            texts.join("\n")
        }
        _ => String::new(),
    };

    Some(Message {
        role,
        content,
        images: None,
        tool_calls: None,
        tool_call_id: None,
        extras: None,
    })
}

/// Parse a `function`-typed tool entry from the Responses `tools` array.
///
/// Non-function tools (`web_search`, `file_search`, etc.) return `None` and are
/// filtered out by the caller. The Responses `function` tool flattens its
/// parameters (no nested `function` wrapper, unlike the chat completions API).
fn parse_function_tool(tool: &Value) -> Option<ToolDefinition> {
    let is_function = tool
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s == "function")
        .unwrap_or(false);
    if !is_function {
        return None;
    }
    let name = tool.get("name")?.as_str()?.to_string();
    let description = tool
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let parameters = tool
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    Some(ToolDefinition {
        name,
        description,
        parameters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_string_input_to_user_message() {
        let req = parse_responses_request(r#"{"model":"m","input":"hello"}"#).unwrap();
        assert_eq!(req.model, "m");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[0].content, "hello");
    }

    #[test]
    fn parse_instructions_to_system_message() {
        let req =
            parse_responses_request(r#"{"model":"m","input":"hi","instructions":"be brief"}"#)
                .unwrap();
        let sys = req
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .unwrap();
        assert_eq!(sys.content, "be brief");
    }

    #[test]
    fn parse_input_array_items_to_messages() {
        let body = r#"{"model":"m","input":[
            {"role":"user","content":[{"type":"input_text","text":"q"}]},
            {"role":"assistant","content":[{"type":"output_text","text":"a"}]}]}"#;
        let req = parse_responses_request(body).unwrap();
        assert!(req
            .messages
            .iter()
            .any(|m| m.role == Role::User));
        assert!(req
            .messages
            .iter()
            .any(|m| m.role == Role::Assistant));
    }

    #[test]
    fn parse_function_tools() {
        let body = r#"{"model":"m","input":"x","tools":[{"type":"function","name":"f","description":"d","parameters":{"type":"object"}}]}"#;
        let req = parse_responses_request(body).unwrap();
        assert_eq!(req.tools.unwrap().len(), 1);
    }
}
