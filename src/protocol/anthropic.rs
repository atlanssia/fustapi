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
    #[serde(default)]
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
#[derive(Deserialize, Clone)]
pub struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    source: Option<AnthropicImageSource>,
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
    let blocks = msg.content_blocks();
    match (!blocks.is_empty()).then_some(&blocks) {
        Some(blocks) => {
            let mut text_parts = Vec::new();
            let mut images = Vec::new();
            for part in blocks {
                match part.kind.as_str() {
                    "text" => text_parts.push(part.text.clone().unwrap_or_default()),
                    "image" => {
                        if let Some(src) = &part.source
                            && let Some(data) = &src.data
                        {
                            let mime = src
                                .media_type
                                .clone()
                                .unwrap_or_else(|| "image/png".to_string());
                            images.push(ImageInput {
                                source: ImageSource::Base64 { data: data.clone() },
                                mime_type: mime,
                            });
                        }
                    }
                    _ => {} // Ignore unknown block types (e.g., tool_use in continuation)
                }
            }
            let content = if text_parts.is_empty() && images.is_empty() {
                String::new()
            } else {
                text_parts.join("\n")
            };
            Ok(Message {
                role,
                content,
                images: if images.is_empty() {
                    None
                } else {
                    Some(images)
                },
                tool_calls: None,
            })
        }
        None => Ok(Message {
            role,
            content: String::new(),
            images: None,
            tool_calls: None,
        }),
    }
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
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
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
                    input: None,
                    source: None,
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
            text: Some(text.to_string()),
            input: None,
            name: None,
        });
    }
    if let Some(tcs) = tool_calls {
        for tc in tcs {
            content_outs.push(AnthropicContentOut {
                kind: "tool_use".to_string(),
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
pub fn serialize_stream_event(chunk: &LLMChunk, _id: &str, _model: &str, index: &usize) -> String {
    if chunk.done {
        // message_delta event with stop_reason
        let event = AnthropicStreamEvent {
            event_type: "message_delta".to_string(),
            message: None,
            index: None,
            delta: Some(AnthropicStreamDelta {
                delta_type: "stop_reason".to_string(),
                text: None,
                partial_json: None,
                stop_reason: Some("stop".to_string()),
            }),
            content_block: None,
        };
        return format!(
            "event: message_delta\n\ndata: {}\n\n",
            serde_json::to_string(&event).unwrap_or_default()
        );
    }
    if let Some(text) = &chunk.content {
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
        return format!(
            "event: content_block_delta\n\ndata: {}\n\n",
            serde_json::to_string(&delta).unwrap_or_default()
        );
    }
    if let Some(tc) = &chunk.tool_call {
        // content_block_start + content_block_delta for tool calls
        let start = AnthropicStreamEvent {
            event_type: "content_block_start".to_string(),
            message: None,
            index: Some(*index),
            delta: None,
            content_block: Some(AnthropicContentBlockOut {
                kind: "tool_use".to_string(),
                id: Some(format!("toolu_{}", index)),
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
                delta_type: "input_json".to_string(),
                text: None,
                partial_json: Some(json_str),
                stop_reason: None,
            }),
            content_block: None,
        };
        return format!(
            "event: content_block_start\n\ndata: {}\n\nevent: content_block_delta\n\ndata: {}\n\n",
            serde_json::to_string(&start).unwrap_or_default(),
            serde_json::to_string(&delta).unwrap_or_default()
        );
    }
    String::new()
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
        };
        let sse = serialize_stream_event(&chunk, "msg_1", "claude-3", &0);
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
        };
        let sse = serialize_stream_event(&chunk, "msg_1", "claude-3", &0);
        assert!(sse.contains("event: message_delta"));
        assert!(sse.contains(r#""type":"stop_reason""#));
    }

    #[test]
    fn test_serialize_stream_event_tool_call() {
        let tc = ToolCall {
            name: "get_weather".to_string(),
            arguments: serde_json::json!({"city": "nyc"}),
        };
        let chunk = LLMChunk {
            content: None,
            tool_call: Some(tc),
            done: false,
        };
        let sse = serialize_stream_event(&chunk, "msg_1", "claude-3", &0);
        assert!(sse.contains("event: content_block_start"));
        assert!(sse.contains("event: content_block_delta"));
        assert!(sse.contains(r#""type":"tool_use""#));
    }
}
