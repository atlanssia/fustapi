//! Anthropic-compatible protocol parsing.
//!
//! Handles request/response formats for the Anthropic Messages API,
//! including `/v1/messages`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

use crate::capability::{ImageInput, ImageSource, ToolCall, ToolDefinition};
use crate::provider::{Message, Role, UnifiedRequest};
use crate::streaming::LLMChunk;

#[derive(Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default, deserialize_with = "deserialize_system")]
    pub system: Option<String>,
    #[serde(default)]
    pub max_tokens: u32,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub tools: Option<Vec<AnthropicTool>>,
}

/// Deserialize Anthropic `system` field which can be a string or an array of
/// content blocks: `[{"type":"text","text":"..."}]`.
fn deserialize_system<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    let val = Option::<serde_json::Value>::deserialize(de)?;
    match val {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Array(arr)) => {
            let texts: Vec<String> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(String::from))
                .collect();
            if texts.is_empty() {
                Ok(None)
            } else {
                Ok(Some(texts.join("\n")))
            }
        }
        Some(other) => Err(de::Error::custom(format!(
            "expected string or array for system, got {}",
            other
        ))),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum AnthropicMessage {
    Simple {
        role: String,
        content: String,
    },
    MultiPart {
        role: String,
        content: Vec<AnthropicContentBlock>,
    },
}

#[allow(dead_code)]
#[derive(Deserialize, Clone, Default)]
pub struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub id: Option<String>, // For tool_use
    #[serde(default)]
    pub name: Option<String>, // For tool_use
    #[serde(default)]
    pub input: Option<Value>, // For tool_use
    #[serde(default)]
    pub tool_use_id: Option<String>, // For tool_result
    #[serde(default)]
    pub content: Option<AnthropicToolResultContent>, // For tool_result
    #[serde(default)]
    pub source: Option<AnthropicImageSource>, // For image
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub enum AnthropicToolResultContent {
    Simple(String),
    MultiPart(Vec<AnthropicContentBlock>),
}

#[allow(dead_code)]
#[derive(Deserialize, Clone)]
pub struct AnthropicImageSource {
    #[serde(rename = "type")]
    source_type: String,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

#[derive(Deserialize)]
pub struct AnthropicTool {
    name: String,
    #[serde(default)]
    description: String,
    input_schema: Value,
}

#[derive(Serialize)]
pub struct AnthropicResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub obj: String,
    pub role: String,
    pub content: Vec<AnthropicContentOut>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
}

#[derive(Serialize)]
pub struct AnthropicContentOut {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
}

#[derive(Serialize)]
pub struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<AnthropicStreamMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta: Option<AnthropicStreamDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_block: Option<AnthropicContentBlockOut>,
}

#[derive(Serialize)]
pub struct AnthropicStreamMessage {
    pub id: String,
    #[serde(rename = "type")]
    pub obj: String,
    pub role: String,
    pub content: Vec<Value>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
}

#[derive(Serialize)]
pub struct AnthropicStreamDelta {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partial_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<String>,
}

#[derive(Debug)]
pub enum ParseError {
    InvalidJson(serde_json::Error),
    MissingField(String),
    InvalidFormat(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::InvalidJson(e) => write!(f, "invalid JSON: {e}"),
            ParseError::MissingField(field) => write!(f, "missing required field: {field}"),
            ParseError::InvalidFormat(msg) => write!(f, "invalid format: {msg}"),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::InvalidJson(e) => Some(e),
            _ => None,
        }
    }
}

fn parse_anthropic_tool(tool: AnthropicTool) -> Result<ToolDefinition, ParseError> {
    Ok(ToolDefinition {
        name: tool.name,
        description: tool.description,
        parameters: tool.input_schema,
    })
}

/// Parse an Anthropic-format messages request into a [`UnifiedRequest`].
pub fn parse_messages_request(json_str: &str) -> Result<UnifiedRequest, ParseError> {
    let req: AnthropicRequest = serde_json::from_str(json_str).map_err(ParseError::InvalidJson)?;
    let mut messages = req
        .messages
        .into_iter()
        .map(parse_anthropic_message)
        .collect::<Result<Vec<_>, _>>()?;
    // Prepend system message if present.
    if let Some(sys) = req.system {
        messages.insert(
            0,
            Message {
                role: Role::System,
                content: sys,
                images: None,
                tool_calls: None,
                tool_call_id: None,
                extras: None,
            },
        );
    }
    Ok(UnifiedRequest {
        model: req.model,
        messages,
        stream: req.stream,
        temperature: req.temperature,
        max_tokens: Some(req.max_tokens),
        tools: req
            .tools
            .map(|tools| {
                tools
                    .into_iter()
                    .map(parse_anthropic_tool)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
    })
}

fn parse_anthropic_message(msg: AnthropicMessage) -> Result<Message, ParseError> {
    let role_str = msg.role();
    let role = match role_str.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        other => {
            return Err(ParseError::InvalidFormat(format!(
                "unsupported role '{other}'"
            )));
        }
    };

    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_call_id = None;

    let blocks = msg.content_blocks();
    for part in blocks {
        match part.kind.as_str() {
            "text" => {
                if let Some(t) = part.text {
                    text_parts.push(t);
                }
            }
            "tool_use" => {
                tool_calls.push(ToolCall {
                    id: part.id.clone(),
                    name: part.name.clone().unwrap_or_default(),
                    arguments: part
                        .input
                        .clone()
                        .unwrap_or(Value::Object(serde_json::Map::new())),
                });
            }
            "tool_result" => {
                // Capture tool_use_id for this tool result message.
                if part.tool_use_id.is_some() {
                    tool_call_id = part.tool_use_id.clone();
                }
                if let Some(content) = part.content {
                    match content {
                        AnthropicToolResultContent::Simple(s) => text_parts.push(s),
                        AnthropicToolResultContent::MultiPart(subblocks) => {
                            for sub in subblocks {
                                if let Some(t) = sub.text {
                                    text_parts.push(t);
                                }
                            }
                        }
                    }
                }
            }
            "image" => {
                if let Some(src) = part.source
                    && let Some(data) = src.data
                {
                    let mime = src.media_type.unwrap_or_else(|| "image/png".to_string());
                    images.push(ImageInput {
                        source: ImageSource::Base64 { data },
                        mime_type: mime,
                    });
                }
            }
            _ => {}
        }
    }

    let content = text_parts.join("\n");
    Ok(Message {
        role,
        content,
        images: if images.is_empty() {
            None
        } else {
            Some(images)
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id,
        extras: None,
    })
}

#[allow(dead_code)]
/// Parsed content from Anthropic requests.
enum AnthropicContent {
    Text(String),
    Parts(Vec<AnthropicContentBlock>),
}

#[derive(Deserialize, Serialize)]
struct AnthropicContentBlockOut {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
}

trait AnthropicMessageExt {
    fn role(&self) -> String;
    fn content_blocks(&self) -> Vec<AnthropicContentBlock>;
}

impl AnthropicMessageExt for AnthropicMessage {
    fn role(&self) -> String {
        match self {
            AnthropicMessage::Simple { role, .. } => role.clone(),
            AnthropicMessage::MultiPart { role, .. } => role.clone(),
        }
    }
    fn content_blocks(&self) -> Vec<AnthropicContentBlock> {
        match self {
            AnthropicMessage::Simple { content, .. } => {
                // Convert simple string content to a single text block
                vec![AnthropicContentBlock {
                    kind: "text".to_string(),
                    text: Some(content.clone()),
                    ..Default::default()
                }]
            }
            AnthropicMessage::MultiPart { content, .. } => content.to_vec(),
        }
    }
}

#[derive(Debug)]
pub enum SerializeError {
    Json(serde_json::Error),
}

impl fmt::Display for SerializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SerializeError::Json(e) => write!(f, "json serialization error: {e}"),
        }
    }
}

impl std::error::Error for SerializeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SerializeError::Json(e) => Some(e),
        }
    }
}

/// Serialize a non-streaming Anthropic response.
pub fn serialize_response(
    id: &str,
    model: &str,
    content: Option<&str>,
    tool_calls: Option<Vec<ToolCall>>,
    stop_reason: Option<&str>,
) -> Result<String, SerializeError> {
    let mut content_outs = Vec::new();
    if let Some(text) = content {
        content_outs.push(AnthropicContentOut {
            kind: "text".to_string(),
            id: None,
            text: Some(text.to_string()),
            input: None,
            name: None,
        });
    }
    if let Some(tcs) = tool_calls {
        for (i, tc) in tcs.into_iter().enumerate() {
            content_outs.push(AnthropicContentOut {
                kind: "tool_use".to_string(),
                id: Some(tc.id.clone().unwrap_or_else(|| format!("toolu_{}", i))),
                text: None,
                input: Some(tc.arguments),
                name: Some(tc.name),
            });
        }
    }
    let resp = AnthropicResponse {
        id: id.to_string(),
        obj: "message".to_string(),
        role: "assistant".to_string(),
        content: content_outs,
        model: model.to_string(),
        stop_reason: stop_reason.map(|s| s.to_string()),
        stop_sequence: None,
    };
    serde_json::to_string(&resp).map_err(SerializeError::Json)
}

/// Serialize an LLMChunk to Anthropic SSE event format string.
///
/// `need_block_start` indicates whether a `content_block_start` event should
/// precede the first text delta for this content block.
/// `stop_reason` is the Anthropic stop reason to use when `chunk.done` is true
/// (e.g., `"end_turn"` for text, `"tool_use"` for tool calls).
pub fn serialize_stream_event(
    chunk: &LLMChunk,
    _id: &str,
    _model: &str,
    index: &usize,
    need_block_start: bool,
    stop_reason: &str,
) -> String {
    let mut s = String::new();

    if let Some(text) = &chunk.content {
        if need_block_start {
            let block_start = serde_json::json!({
                "type": "content_block_start",
                "index": *index,
                "content_block": { "type": "text", "text": "" }
            });
            s.push_str(&format!(
                "event: content_block_start\ndata: {}\n\n",
                serde_json::to_string(&block_start).unwrap_or_default()
            ));
        }

        let delta = AnthropicStreamEvent {
            event_type: "content_block_delta".to_string(),
            message: None,
            index: Some(*index),
            delta: Some(AnthropicStreamDelta {
                delta_type: "text_delta".to_string(),
                text: Some(text.clone()),
                partial_json: None,
                stop_reason: None,
            }),
            content_block: None,
        };
        s.push_str(&format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&delta).unwrap_or_default()
        ));
    }

    if let Some(tc) = &chunk.tool_call {
        let tool_id = tc.id.clone().unwrap_or_else(|| format!("toolu_{}", index));
        let start = AnthropicStreamEvent {
            event_type: "content_block_start".to_string(),
            message: None,
            index: Some(*index),
            delta: None,
            content_block: Some(AnthropicContentBlockOut {
                kind: "tool_use".to_string(),
                id: Some(tool_id),
                name: Some(tc.name.clone()),
                input: None,
            }),
        };
        let json_str = tc.arguments.to_string();
        let delta = AnthropicStreamEvent {
            event_type: "content_block_delta".to_string(),
            message: None,
            index: Some(*index),
            delta: Some(AnthropicStreamDelta {
                delta_type: "input_json_delta".to_string(),
                text: None,
                partial_json: Some(json_str),
                stop_reason: None,
            }),
            content_block: None,
        };
        s.push_str(&format!(
            "event: content_block_start\ndata: {}\n\nevent: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&start).unwrap_or_default(),
            serde_json::to_string(&delta).unwrap_or_default()
        ));
    }

    if chunk.done {
        let block_stop = serde_json::json!({
            "type": "content_block_stop",
            "index": *index
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

pub fn serialize_message_start(id: &str, model: &str) -> String {
    // Build the message_start event manually to include the usage object
    // that Claude Code expects.
    let msg = serde_json::json!({
        "type": "message_start",
        "message": {
            "id": id,
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": model,
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0
            }
        }
    });
    format!(
        "event: message_start\ndata: {}\n\n",
        serde_json::to_string(&msg).unwrap_or_default()
    )
}

pub fn serialize_message_stop() -> String {
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::ImageSource;

    #[test]
    fn test_parse_basic_request() {
        let json = r#"{"model":"claude-3","messages":[{"role":"user","content":"hello"}],"max_tokens":1024}"#;
        let result = parse_messages_request(json).expect("parse should succeed");
        assert_eq!(result.model, "claude-3");
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, Role::User);
        assert_eq!(result.messages[0].content, "hello");
        assert_eq!(result.max_tokens, Some(1024));
        assert!(!result.stream);
    }

    #[test]
    fn test_parse_streaming_with_system() {
        let json = r#"{"model":"claude-3","messages":[{"role":"user","content":"hi"}],"system":"be helpful","stream":true,"temperature":0.5,"max_tokens":2048}"#;
        let result = parse_messages_request(json).expect("parse should succeed");
        assert_eq!(result.model, "claude-3");
        assert_eq!(result.messages.len(), 2); // system + user
        assert_eq!(result.messages[0].role, Role::System);
        assert_eq!(result.messages[0].content, "be helpful");
        assert!(result.stream);
        assert_eq!(result.temperature, Some(0.5));
        assert_eq!(result.max_tokens, Some(2048));
    }

    #[test]
    fn test_parse_tools() {
        let json = r#"{"model":"claude-3","messages":[{"role":"user","content":"weather"}],"max_tokens":1024,"tools":[{"name":"get_weather","description":"Get weather","input_schema":{"type":"object","properties":{}}}]}"#;
        let result = parse_messages_request(json).expect("parse should succeed");
        assert_eq!(result.tools.as_ref().unwrap().len(), 1);
        let tool = &result.tools.as_ref().unwrap()[0];
        assert_eq!(tool.name, "get_weather");
        assert_eq!(tool.description, "Get weather");
        assert_eq!(tool.parameters["type"], "object");
    }

    #[test]
    fn test_parse_image_content() {
        let json = r#"{"model":"claude-3","messages":[{"role":"user","content":[{"type":"text","text":"what is this"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"abc123"}}]}],"max_tokens":1024}"#;
        let result = parse_messages_request(json).expect("parse should succeed");
        assert_eq!(result.messages.len(), 1);
        let msg = &result.messages[0];
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "what is this");
        assert!(msg.images.is_some());
        let images = msg.images.as_ref().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(
            images[0].source,
            ImageSource::Base64 {
                data: "abc123".to_string()
            }
        );
        assert_eq!(images[0].mime_type, "image/png");
    }

    #[test]
    fn test_invalid_json() {
        let result = parse_messages_request("not json");
        assert!(result.is_err());
        match result.unwrap_err() {
            ParseError::InvalidJson(_) => {}
            other => panic!("expected InvalidJson, got {:?}", other),
        }
    }

    #[test]
    fn test_serialize_response_text() {
        let json = serialize_response("msg_1", "claude-3", Some("Hello!"), None, Some("end_turn"))
            .expect("serialize should succeed");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["id"], "msg_1");
        assert_eq!(value["type"], "message");
        assert_eq!(value["role"], "assistant");
        assert_eq!(value["content"][0]["text"], "Hello!");
        assert_eq!(value["stop_reason"], "end_turn");
    }

    #[test]
    fn test_serialize_response_tool_use() {
        let tcs = vec![ToolCall {
            id: None,
            name: "get_weather".to_string(),
            arguments: serde_json::json!({"city": "nyc"}),
        }];
        let json = serialize_response("msg_1", "claude-3", None, Some(tcs), Some("tool_use"))
            .expect("serialize should succeed");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["content"][0]["type"], "tool_use");
        assert_eq!(value["content"][0]["name"], "get_weather");
        assert_eq!(value["stop_reason"], "tool_use");
    }

    #[test]
    fn test_serialize_stream_event_text() {
        let chunk = LLMChunk {
            content: Some("Hello".to_string()),
            tool_call: None,
            done: false,
            usage: None,
        };
        let sse = serialize_stream_event(&chunk, "msg_1", "claude-3", &0, true, "end_turn");
        assert!(sse.contains("event: content_block_delta"));
        assert!(sse.contains(r#""text":"Hello""#));
        assert!(sse.ends_with("\n\n"));
    }

    #[test]
    fn test_serialize_stream_event_done() {
        let chunk = LLMChunk {
            content: None,
            tool_call: None,
            done: true,
            usage: None,
        };
        let sse = serialize_stream_event(&chunk, "msg_1", "claude-3", &0, false, "end_turn");
        assert!(sse.contains("event: message_delta"));
        assert!(sse.contains(r#""type":"stop_reason""#));
    }

    #[test]
    fn test_serialize_stream_event_tool_call() {
        let tc = ToolCall {
            id: None,
            name: "get_weather".to_string(),
            arguments: serde_json::json!({"city": "nyc"}),
        };
        let chunk = LLMChunk {
            content: None,
            tool_call: Some(tc),
            done: false,
            usage: None,
        };
        let sse = serialize_stream_event(&chunk, "msg_1", "claude-3", &0, true, "tool_use");
        assert!(sse.contains("event: content_block_start"));
        assert!(sse.contains("event: content_block_delta"));
        assert!(sse.contains(r#""type":"tool_use""#));
    }
}
