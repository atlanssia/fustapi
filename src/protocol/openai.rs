//! OpenAI-compatible protocol parsing.
//!
//! Handles request/response formats for the `OpenAI` API specification,
//! including `/v1/chat/completions` and `/v1/models`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

use crate::capability::{ImageInput, ImageSource, ToolCall, ToolDefinition};
use crate::provider::{Message, Role, UnifiedRequest};
use crate::streaming::LLMChunk;

#[derive(Deserialize)]
pub struct OpenAIRequest {
    pub model: String,
    pub messages: Vec<OpenAIMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub tools: Option<Vec<OpenAITool>>,
    /// Capture all other request parameters (`top_p`, stop, n, etc.) for passthrough.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
pub struct OpenAIMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<OpenAIContentRaw>,
    #[serde(default)]
    pub tool_calls: Option<Vec<OpenAIToolCallIn>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// `DeepSeek` thinking mode: internal reasoning content that must be echoed back.
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum OpenAIContentRaw {
    Simple(String),
    MultiPart(Vec<OpenAIMessageContent>),
}

#[derive(Deserialize)]
pub struct OpenAIToolCallIn {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAIFunctionCallIn,
}

#[derive(Deserialize)]
pub struct OpenAIFunctionCallIn {
    pub name: String,
    pub arguments: String,
}

#[derive(Deserialize)]
pub struct OpenAIMessageContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    image_url: Option<OpenAIImageUrl>,
}

#[derive(Deserialize)]
struct OpenAIImageUrl {
    url: String,
}

#[derive(Deserialize)]
pub struct OpenAITool {
    #[serde(rename = "type")]
    _kind: String,
    function: OpenAIFunctionTool,
}

#[derive(Deserialize)]
struct OpenAIFunctionTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "empty_object")]
    parameters: Value,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

// ── Local types for message content parsing ─────────────────────────

/// Parsed message content from `OpenAI` requests.
enum OpenAIContent {
    /// Simple text content.
    Text(String),
    /// Multi-part content (text + images).
    Parts(Vec<OpenAIMessageContentKind>),
}

/// A single content part within a multi-part message.
enum OpenAIMessageContentKind {
    /// Text part.
    Text(String),
    /// Image URL part.
    ImageUrl(String),
}

#[derive(Serialize)]
pub struct OpenAIResponse {
    pub id: String,
    #[serde(rename = "object")]
    pub obj: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OpenAIChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAIUsage>,
}

#[derive(Serialize)]
pub struct OpenAIChoice {
    pub index: usize,
    pub message: OpenAIMessageOut,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<&'static str>,
}

#[derive(Serialize)]
pub struct OpenAIMessageOut {
    pub role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIToolCallOut>>,
}

#[derive(Serialize)]
pub struct OpenAIToolCallOut {
    pub index: usize,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: OpenAIFunctionCall,
}

#[derive(Serialize)]
pub struct OpenAIFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Serialize)]
pub struct OpenAIUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Serialize)]
pub struct OpenAIStreamChunk {
    pub id: String,
    #[serde(rename = "object")]
    pub obj: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OpenAIStreamChoice>,
}

#[derive(Serialize)]
pub struct OpenAIStreamChoice {
    pub index: usize,
    pub delta: OpenAIDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<&'static str>,
}

#[derive(Serialize)]
pub struct OpenAIDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIStreamToolCall>>,
}

#[derive(Serialize)]
pub struct OpenAIStreamToolCall {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<OpenAIStreamFunction>,
}

#[derive(Serialize)]
pub struct OpenAIStreamFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[allow(dead_code)]
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

pub fn parse_chat_request(json_str: &str) -> Result<UnifiedRequest, ParseError> {
    let req: OpenAIRequest = serde_json::from_str(json_str).map_err(ParseError::InvalidJson)?;
    let messages = req
        .messages
        .into_iter()
        .map(parse_openai_message)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UnifiedRequest {
        model: req.model,
        messages,
        stream: req.stream,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        tools: req
            .tools
            .map(|tools| {
                tools
                    .into_iter()
                    .map(parse_openai_tool)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
        extra_params: req.extra,
    })
}

fn parse_openai_message(msg: OpenAIMessage) -> Result<Message, ParseError> {
    let role = match msg.role.as_str() {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        other => {
            return Err(ParseError::InvalidFormat(format!(
                "unsupported role '{other}'"
            )));
        }
    };

    let content = match openai_message_to_content(&msg) {
        Some(OpenAIContent::Text(text)) => text,
        Some(OpenAIContent::Parts(parts)) => {
            let mut text_parts = Vec::new();
            for part in parts {
                if let OpenAIMessageContentKind::Text(t) = part {
                    text_parts.push(t);
                }
            }
            text_parts.join("\n")
        }
        None => String::new(),
    };

    let images = match openai_message_to_content(&msg) {
        Some(OpenAIContent::Parts(parts)) => {
            let mut images = Vec::new();
            for part in parts {
                if let OpenAIMessageContentKind::ImageUrl(url) = part {
                    images.push(ImageInput {
                        source: parse_image_source(&url),
                        mime_type: detect_mime(&url),
                    });
                }
            }
            if images.is_empty() {
                None
            } else {
                Some(images)
            }
        }
        _ => None,
    };

    let tool_calls = msg.tool_calls.map(|tcs| {
        tcs.into_iter()
            .map(|tc| ToolCall {
                id: Some(tc.id),
                name: tc.function.name,
                arguments: serde_json::from_str(&tc.function.arguments).unwrap_or_default(),
            })
            .collect()
    });

    // Collect provider-specific fields into extras.
    let mut extras = serde_json::Map::new();
    if let Some(rc) = msg.reasoning_content {
        extras.insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc),
        );
    }

    Ok(Message {
        role,
        content,
        images,
        tool_calls,
        tool_call_id: msg.tool_call_id,
        extras: if extras.is_empty() {
            None
        } else {
            Some(extras)
        },
    })
}

fn parse_image_source(url_str: &str) -> ImageSource {
    if url_str.starts_with("data:") {
        let parts: Vec<&str> = url_str.splitn(3, ',').collect();
        if parts.len() == 2 {
            return ImageSource::Base64 {
                data: parts[1].to_string(),
            };
        }
    }
    ImageSource::Url {
        url: url_str.to_string(),
    }
}

fn detect_mime(url_str: &str) -> String {
    if url_str.starts_with("data:image/") && url_str.contains(";base64") {
        let parts: Vec<&str> = url_str.splitn(3, ',').collect();
        if parts.len() == 2 {
            let mime_part = parts[0];
            if let Some(mime) = mime_part
                .strip_prefix("data:")
                .and_then(|s| s.strip_suffix(";base64"))
            {
                return mime.to_string();
            }
        }
    }
    "image/png".to_string()
}

fn parse_openai_tool(tool: OpenAITool) -> Result<ToolDefinition, ParseError> {
    Ok(ToolDefinition {
        name: tool.function.name.clone(),
        description: tool.function.description.clone(),
        parameters: tool.function.parameters.clone(),
    })
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

fn openai_message_to_content(msg: &OpenAIMessage) -> Option<OpenAIContent> {
    match &msg.content {
        Some(OpenAIContentRaw::Simple(text)) => Some(OpenAIContent::Text(text.clone())),
        Some(OpenAIContentRaw::MultiPart(parts)) => {
            let content_parts: Vec<OpenAIMessageContentKind> = parts
                .iter()
                .map(|c| match c.kind.as_str() {
                    "text" => OpenAIMessageContentKind::Text(c.text.clone().unwrap_or_default()),
                    "image_url" => OpenAIMessageContentKind::ImageUrl(
                        c.image_url
                            .as_ref()
                            .map(|i| i.url.clone())
                            .unwrap_or_default(),
                    ),
                    _ => OpenAIMessageContentKind::Text(c.text.clone().unwrap_or_default()),
                })
                .collect();
            Some(OpenAIContent::Parts(content_parts))
        }
        None => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn serialize_response(
    id: &str,
    model: &str,
    content: Option<&str>,
    tool_calls: Option<Vec<ToolCall>>,
    finish_reason: &'static str,
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
    reasoning_content: Option<&str>,
) -> Result<String, SerializeError> {
    let mut choices = Vec::new();
    if let Some(text) = content {
        choices.push(OpenAIChoice {
            index: 0,
            message: OpenAIMessageOut {
                role: "assistant",
                content: Some(text.to_string()),
                reasoning_content: reasoning_content.map(std::string::ToString::to_string),
                tool_calls: None,
            },
            finish_reason: Some(finish_reason),
        });
    } else if let Some(tcs) = tool_calls {
        let mut tc_outs = Vec::new();
        for (i, tc) in tcs.iter().enumerate() {
            tc_outs.push(OpenAIToolCallOut {
                index: i,
                id: tc.id.clone().unwrap_or_else(|| format!("call_{i}")),
                kind: "function",
                function: OpenAIFunctionCall {
                    name: tc.name.clone(),
                    arguments: tc.arguments.to_string(),
                },
            });
        }
        choices.push(OpenAIChoice {
            index: 0,
            message: OpenAIMessageOut {
                role: "assistant",
                content: None,
                reasoning_content: reasoning_content.map(std::string::ToString::to_string),
                tool_calls: Some(tc_outs),
            },
            finish_reason: Some(finish_reason),
        });
    } else {
        // Empty response (no content, no tool calls) — still produce a choice per OpenAI spec.
        choices.push(OpenAIChoice {
            index: 0,
            message: OpenAIMessageOut {
                role: "assistant",
                content: None,
                reasoning_content: reasoning_content.map(std::string::ToString::to_string),
                tool_calls: None,
            },
            finish_reason: Some(finish_reason),
        });
    }
    let usage = Some(OpenAIUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    });
    let response = OpenAIResponse {
        id: id.to_string(),
        obj: "chat.completion".to_string(),
        created: 1_000_000_000_u64,
        model: model.to_string(),
        choices,
        usage,
    };
    serde_json::to_string(&response).map_err(SerializeError::Json)
}

#[must_use]
pub fn serialize_stream_chunk(chunk: &LLMChunk, id: &str, model: &str, index: &usize) -> String {
    if chunk.done {
        return "data:[DONE]\n\n".to_string();
    }
    let mut delta = OpenAIDelta {
        role: Some("assistant"),
        content: chunk.content.clone(),
        reasoning_content: chunk.reasoning_content.clone(),
        tool_calls: None,
    };
    if let Some(tc) = &chunk.tool_call {
        delta.tool_calls = Some(vec![OpenAIStreamToolCall {
            index: *index,
            id: Some(tc.id.clone().unwrap_or_else(|| format!("call_{index}"))),
            kind: Some("function"),
            function: Some(OpenAIStreamFunction {
                name: Some(tc.name.clone()),
                arguments: Some(tc.arguments.to_string()),
            }),
        }]);
    }
    let choice = OpenAIStreamChoice {
        index: *index,
        delta,
        finish_reason: None,
    };
    let response = OpenAIStreamChunk {
        id: id.to_string(),
        obj: "chat.completion.chunk".to_string(),
        created: 1_000_000_000_u64,
        model: model.to_string(),
        choices: vec![choice],
    };
    serde_json::to_string(&response).map_or_else(
        |_| format!("data:{}\n\n", "{{\"error\":\"serialization failed\"}}"),
        |s| format!("data:{s}\n\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::ImageSource;

    #[test]
    fn test_parse_basic_request() {
        let json =
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hello"}],"stream":false}"#;
        let result = parse_chat_request(json).expect("parse should succeed");
        assert_eq!(result.model, "gpt-4");
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, Role::User);
        assert_eq!(result.messages[0].content, "hello");
        assert!(!result.stream);
        assert_eq!(result.tools, None);
        assert_eq!(result.temperature, None);
        assert_eq!(result.max_tokens, None);
    }

    #[test]
    fn test_parse_streaming_request() {
        let json = r#"{"model":"gpt-3.5-turbo","messages":[{"role":"system","content":"be helpful"},{"role":"user","content":"hi"}],"stream":true,"temperature":0.7,"max_tokens":100}"#;
        let result = parse_chat_request(json).expect("parse should succeed");
        assert_eq!(result.model, "gpt-3.5-turbo");
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, Role::System);
        assert_eq!(result.messages[1].role, Role::User);
        assert!(result.stream);
        assert_eq!(result.temperature, Some(0.7));
        assert_eq!(result.max_tokens, Some(100));
    }

    #[test]
    fn test_parse_tools() {
        let json = r#"{"model":"gpt-4","messages":[{"role":"user","content":"weather in nyc"}],"tools":[{"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{},"required":[]}}}]}"#;
        let result = parse_chat_request(json).expect("parse should succeed");
        assert_eq!(result.tools.as_ref().unwrap().len(), 1);
        let tool = &result.tools.as_ref().unwrap()[0];
        assert_eq!(tool.name, "get_weather");
        assert_eq!(tool.description, "Get weather");
        assert_eq!(tool.parameters["type"], "object");
    }

    #[test]
    fn test_parse_image_content() {
        let json = r#"{"model":"gpt-4","messages":[{"role":"user","content":[{"type":"text","text":"what is this"},{"type":"image_url","image_url":{"url":"data:image/png;base64,abc123"}}]}]}"#;
        let result = parse_chat_request(json).expect("parse should succeed");
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
        let result = parse_chat_request("not json");
        assert!(result.is_err());
        match result.unwrap_err() {
            ParseError::InvalidJson(_) => {}
            other => panic!("expected InvalidJson, got {:?}", other),
        }
    }

    #[test]
    fn test_unsupported_role() {
        let json = r#"{"model":"gpt-4","messages":[{"role":"invalid_role","content":"hello"}]}"#;
        let result = parse_chat_request(json);
        assert!(result.is_err());
        match result.unwrap_err() {
            ParseError::InvalidFormat(msg) => {
                assert!(msg.contains("unsupported role"))
            }
            other => panic!("expected InvalidFormat, got {:?}", other),
        }
    }

    #[test]
    fn test_serialize_response() {
        let json = serialize_response(
            "chatcmpl-1",
            "gpt-4",
            Some("Hello!"),
            None,
            "stop",
            10,
            5,
            15,
            None,
        )
        .expect("serialize should succeed");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["id"], "chatcmpl-1");
        assert_eq!(value["object"], "chat.completion");
        assert_eq!(value["model"], "gpt-4");
        assert_eq!(value["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(value["choices"][0]["finish_reason"], "stop");
        assert_eq!(value["usage"]["total_tokens"], 15);
    }

    #[test]
    fn test_serialize_stream_chunk() {
        let chunk = LLMChunk {
            reasoning_content: None,
            content: Some("Hello".to_string()),
            tool_call: None,
            done: false,
            usage: None,
        };
        let sse = serialize_stream_chunk(&chunk, "chatcmpl-1", "gpt-4", &0);
        assert!(sse.starts_with("data:"));
        assert!(sse.contains(r#""content":"Hello""#));
        assert!(sse.ends_with("\n\n"));
    }

    #[test]
    fn test_serialize_done_chunk() {
        let chunk = LLMChunk {
            reasoning_content: None,
            content: None,
            tool_call: None,
            done: true,
            usage: None,
        };
        let sse = serialize_stream_chunk(&chunk, "chatcmpl-1", "gpt-4", &0);
        assert_eq!(sse.trim(), "data:[DONE]");
    }

    #[test]
    fn test_serialize_tool_calls_response() {
        let tcs = vec![ToolCall {
            id: None,
            name: "get_weather".to_string(),
            arguments: serde_json::json!({"city": "nyc"}),
        }];
        let json = serialize_response(
            "chatcmpl-1",
            "gpt-4",
            None,
            Some(tcs),
            "tool_calls",
            10,
            5,
            15,
            None,
        )
        .expect("serialize should succeed");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
    }

    #[test]
    fn test_serialize_empty_response() {
        let json = serialize_response("chatcmpl-1", "gpt-4", None, None, "stop", 0, 0, 0, None)
            .expect("serialize should succeed");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["choices"].as_array().unwrap().len(), 1);
    }
}
