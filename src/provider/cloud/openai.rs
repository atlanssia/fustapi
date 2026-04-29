//! OpenAI cloud provider adapter.
//!
//! Forwards requests to any OpenAI-compatible API (local LLM, OpenAI, etc.).

use async_trait::async_trait;
use serde::Deserialize;
use tokio_stream::StreamExt;

use crate::provider::{Provider, ProviderError, UnifiedRequest};
use crate::streaming::{LLMChunk, LLMStream};

/// OpenAI provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub endpoint: String,
    pub api_key: String,
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com".to_string(),
            api_key: String::new(),
        }
    }
}

/// OpenAI provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    config: OpenAIConfig,
    client: reqwest::Client,
}

impl OpenAIProvider {
    pub fn new(config: OpenAIConfig) -> Self {
        Self { config, client: reqwest::Client::new() }
    }

    /// Build the OpenAI-compatible request body from a UnifiedRequest.
    fn build_request_body(&self, request: &UnifiedRequest) -> serde_json::Value {
        let messages = request.messages.iter().map(|msg| {
            let mut m = serde_json::json!({ "role": msg.role });

            if let Some(images) = &msg.images {
                let mut parts = Vec::new();

                if !msg.content.is_empty() {
                    parts.push(serde_json::json!({ "type": "text", "text": msg.content }));
                }

                for img in images {
                    let source = match &img.source {
                        crate::capability::ImageSource::Base64 { data } => data.clone(),
                        crate::capability::ImageSource::Url { url } => url.clone(),
                    };

                    let mime = img.mime_type.clone();

                    let url = if source.starts_with("data:") { source } else { format!("data:{};base64,{}", mime, source) };

                    parts.push(serde_json::json!({ "type": "image_url", "image_url": { "url": url } }));
                }

                if parts.len() > 1 || (!msg.content.is_empty() && !images.is_empty()) { 
                    m["content"] = serde_json::json!(parts); 
                } else if parts.len() == 1 && matches!(parts[0], serde_json::Value::Object(ref o) if o.get("type").and_then(|t| t.as_str()) == Some("text")) { 
                    m["content"] = parts[0]["text"].clone(); 
                } else if parts.len() == 1 && matches!(parts[0], serde_json::Value::Object(ref o) if o.get("type").and_then(|t| t.as_str()) == Some("image_url")) { 
                    m["content"] = parts[0]["image_url"]["url"].clone(); 
                } else if parts.is_empty() { 
                    m["content"] = serde_json::json!(""); 
                } else { 
                    m["content"] = serde_json::json!(&msg.content); 
                } 
            } else { 
                m["content"] = serde_json::json!(&msg.content); 
            } 

            if let Some(tcs) = &msg.tool_calls { 
                let calls = tcs.iter().enumerate().map(|(i, tc)| serde_json::json!({ 
                    "id": format!("call_{}", i), 
                    "type": "function", 
                    "function": { "name": tc.name.clone(), "arguments": tc.arguments.to_string() } 
                })).collect::<Vec<_>>(); 
                m["tool_calls"] = serde_json::json!(calls); 
            } 

            m 
        }).collect::<Vec<_>>(); 

        let mut body = serde_json::json!({ "model": &request.model, "messages": messages, "stream": request.stream }); 

        if let Some(temp) = request.temperature { body["temperature"] = serde_json::json!(temp); } 
        if let Some(max_t) = request.max_tokens { body["max_tokens"] = serde_json::json!(max_t); } 

        if let Some(tools) = &request.tools { 
            let tool_defs = tools.iter().map(|t| serde_json::json!({ 
                "type": "function", "function": { "name": t.name.clone(), "description": t.description.clone(), "parameters": t.parameters.clone() } 
            })).collect::<Vec<_>>(); 
            body["tools"] = serde_json::json!(tool_defs); 
        } 

        body 
    } 

    /// Parse a non-streaming response into LLMChunks. 
    fn parse_response(response: &OpenAIChatResponse) -> Vec<LLMChunk> { 
        let mut chunks = Vec::new(); 

        if let Some(choice) = response.choices.first() { 
            if let Some(content) = &choice.message.content { 
                chunks.push(LLMChunk { content: Some(content.clone()), tool_call: None, done: false }); 
            } 

            if let Some(tool_calls) = &choice.message.tool_calls { 
                for (_i, tc) in tool_calls.iter().enumerate() { 
                    chunks.push(LLMChunk { content: None , tool_call : Some(crate :: capability :: ToolCall{ name : tc.function.name.clone(), arguments : serde_json :: from_str(&tc.function.arguments).unwrap_or_default()}), done : false }); } } 

            chunks.push(LLMChunk { content: None , tool_call : None , done : true }); } 

        chunks 
    } 

} 

// ── Response types for non-streaming ──────────────────────────────────

// Fields are deserialized from JSON but not all are read after parsing.
#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenAIChatResponse {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenAIChoice {
    index: usize,
    message: OpenAIMessageOut,
    finish_reason: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenAIMessageOut {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: OpenAIFunctionCall,
}

#[derive(Deserialize)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenAIUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn chat_stream(&self, request: UnifiedRequest) -> Result<LLMStream, ProviderError> {
        let body = self.build_request_body(&request);
        let url = format!("{}/v1/chat/completions", self.config.endpoint.trim_end_matches('/'));

        let mut builder = self.client.post(&url).header("Content-Type", "application/json");

        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        builder = builder.json(&body);

        if request.stream {
            let resp = builder.send().await.map_err(|e| ProviderError::Connection(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let err_text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Request(format!("provider error {}: {}", status, err_text)));
            }

            let stream = resp.bytes_stream();
            let s = stream.map(move |chunk_result| match chunk_result {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let lines = text.split('\n');

                    for line in lines {
                        if line.starts_with("data:") && line.len() > 5 {
                            let data = line[5..].trim();

                            if data == "[DONE]" || data == " [DONE]" || data == "[DONE] " {
                                return Ok(LLMChunk { content: None, tool_call: None, done: true });
                            }

                            if !data.is_empty() {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                    if let Some(choices) = v.get("choices") {
                                        if let Some(choice) = choices.get(0) {
                                            if let Some(delta) = choice.get("delta") {
                                                // Extract content from either "content" (standard OpenAI)
                                                // or "reasoning_content" (used by some models like Qwen)
                                                let content_str = delta.get("content")
                                                    .or_else(|| delta.get("reasoning_content"))
                                                    .and_then(|c| c.as_str());

                                                if let Some(content) = content_str {
                                                    if !content.is_empty() {
                                                        return Ok(LLMChunk { content: Some(content.to_string()), tool_call: None, done: false });
                                                    }
                                                }

                                                if let Some(tool_calls) = delta.get("tool_calls") {
                                                    if let Some(tc) = tool_calls.get(0) {
                                                        if let Some(func) = tc.get("function") {
                                                            if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                                                if !name.is_empty() {
                                                                    let args = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                                                                    return Ok(LLMChunk { content: None, tool_call: Some(crate::capability::ToolCall { name: name.to_string(), arguments: serde_json::from_str(args).unwrap_or_default() }), done: false });
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // Empty delta or unrecognized event — continue streaming, don't terminate
                                return Ok(LLMChunk { content: None, tool_call: None, done: false });
                            }
                        }
                    }

                    // More bytes may be coming — don't terminate the stream yet
                    Ok(LLMChunk { content: None, tool_call: None, done: false })
                }
                Err(e) => Err(crate::streaming::StreamError::Provider(e.to_string())),
            });

            Ok(Box::new(s) as LLMStream)
        } else {
            let resp_body = builder.send().await.map_err(|e| ProviderError::Connection(e.to_string()))?;

            if !resp_body.status().is_success() {
                let status = resp_body.status();
                let err_text = resp_body.text().await.unwrap_or_default();
                return Err(ProviderError::Request(format!("provider error {}: {}", status, err_text)));
            }

            let resp_body = resp_body.json::<OpenAIChatResponse>().await.map_err(|e| ProviderError::Internal(e.to_string()))?;

            let chunks = Self::parse_response(&resp_body);

            let s = futures::stream::iter(chunks.into_iter().map(Ok));

            Ok(Box::new(s) as LLMStream)
        }
    }

    fn supports_tools(&self) -> bool { true }

    fn supports_images(&self) -> bool { true }

    fn name(&self) -> &str { "openai" }
}
