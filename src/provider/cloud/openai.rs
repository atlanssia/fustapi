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
    pub model: Option<String>,
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: None,
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
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Build the OpenAI-compatible request body from a UnifiedRequest.
    pub fn build_request_body(&self, request: &UnifiedRequest) -> serde_json::Value {
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

        let model = self.config.model.as_ref().unwrap_or(&request.model);
        let mut body =
            serde_json::json!({ "model": model, "messages": messages, "stream": request.stream });

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(max_t) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_t);
        }

        if let Some(tools) = &request.tools {
            let tool_defs = tools.iter().map(|t| serde_json::json!({
                "type": "function", "function": { "name": t.name.clone(), "description": t.description.clone(), "parameters": t.parameters.clone() }
            })).collect::<Vec<_>>();
            body["tools"] = serde_json::json!(tool_defs);
        }

        body
    }

    /// Parse a non-streaming response into LLMChunks.
    pub fn parse_response(response: &OpenAIChatResponse) -> Vec<LLMChunk> {
        let mut chunks = Vec::new();

        if let Some(choice) = response.choices.first() {
            if let Some(content) = &choice.message.content {
                chunks.push(LLMChunk { usage: None,
                    content: Some(content.clone()),
                    tool_call: None,
                    done: false,
                });
            }

            if let Some(tool_calls) = &choice.message.tool_calls {
                for tc in tool_calls.iter() {
                    chunks.push(LLMChunk { usage: None,
                        content: None,
                        tool_call: Some(crate::capability::ToolCall {
                            name: tc.function.name.clone(),
                            arguments: serde_json::from_str(&tc.function.arguments)
                                .unwrap_or_default(),
                        }),
                        done: false,
                    });
                }
            }

            chunks.push(LLMChunk { usage: None,
                content: None,
                tool_call: None,
                done: true,
            });
        }

        chunks
    }
}

// ── Response types for non-streaming ──────────────────────────────────

// Fields are deserialized from JSON but not all are read after parsing.
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct OpenAIChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OpenAIChoice>,
    pub usage: Option<OpenAIUsage>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct OpenAIChoice {
    pub index: usize,
    pub message: OpenAIMessageOut,
    pub finish_reason: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct OpenAIMessageOut {
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct OpenAIToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAIFunctionCall,
}

#[derive(Deserialize)]
pub struct OpenAIFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct OpenAIUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

pub fn parse_openai_sse_stream(
    stream: impl futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Unpin + 'static,
) -> LLMStream {
    use futures::stream;
    let buffer = String::new();
    let tool_name: Option<String> = None;
    let tool_args = String::new();

    let s = stream::unfold(
        (stream, buffer, tool_name, tool_args),
        |(mut stream, mut buffer, mut tool_name, mut tool_args)| async move {
            loop {
                if let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].trim().to_string();
                    buffer.drain(..=pos);

                    if line.starts_with("data:") && line.len() > 5 {
                        let data = line[5..].trim();
                        if data == "[DONE]" || data == " [DONE]" || data == "[DONE] " {
                            if let Some(name) = tool_name.take() {
                                let args = serde_json::from_str(&tool_args)
                                    .unwrap_or(serde_json::json!({}));
                                let tc = crate::capability::ToolCall {
                                    name,
                                    arguments: args,
                                };
                                return Some((
                                    Ok(crate::streaming::LLMChunk { usage: None,
                                        content: None,
                                        tool_call: Some(tc),
                                        done: true,
                                    }),
                                    (stream, buffer, tool_name, tool_args),
                                ));
                            }
                            return Some((
                                Ok(crate::streaming::LLMChunk { usage: None,
                                    content: None,
                                    tool_call: None,
                                    done: true,
                                }),
                                (stream, buffer, tool_name, tool_args),
                            ));
                        }
                        if !data.is_empty()
                            && let Ok(v) = serde_json::from_str::<serde_json::Value>(data)
                            && let Some(choices) = v.get("choices")
                            && let Some(choice) = choices.get(0)
                            && let Some(delta) = choice.get("delta")
                        {
                            let content_str = delta
                                .get("content")
                                .or_else(|| delta.get("reasoning_content"))
                                .and_then(|c| c.as_str());
                            if let Some(content) = content_str
                                && !content.is_empty()
                            {
                                return Some((
                                    Ok(crate::streaming::LLMChunk { usage: None,
                                        content: Some(content.to_string()),
                                        tool_call: None,
                                        done: false,
                                    }),
                                    (stream, buffer, tool_name, tool_args),
                                ));
                            }
                            if let Some(tool_calls) = delta.get("tool_calls")
                                && let Some(tc) = tool_calls.get(0)
                                && let Some(func) = tc.get("function")
                            {
                                let new_name = func.get("name").and_then(|n| n.as_str());
                                let new_args =
                                    func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");

                                if let Some(name) = new_name {
                                    let mut flush_tc = None;
                                    if let Some(old_name) = tool_name.take() {
                                        let parsed_args = serde_json::from_str(&tool_args)
                                            .unwrap_or(serde_json::json!({}));
                                        flush_tc = Some(crate::capability::ToolCall {
                                            name: old_name,
                                            arguments: parsed_args,
                                        });
                                    }
                                    tool_name = Some(name.to_string());
                                    tool_args = new_args.to_string();

                                    if flush_tc.is_some() {
                                        return Some((
                                            Ok(crate::streaming::LLMChunk { usage: None,
                                                content: None,
                                                tool_call: flush_tc,
                                                done: false,
                                            }),
                                            (stream, buffer, tool_name, tool_args),
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
                        let text = String::from_utf8_lossy(&bytes);
                        buffer.push_str(&text);
                    }
                    Some(Err(e)) => {
                        return Some((
                            Err(crate::streaming::StreamError::Provider(e.to_string())),
                            (stream, buffer, tool_name, tool_args),
                        ));
                    }
                    None => {
                        if let Some(name) = tool_name.take() {
                            let args =
                                serde_json::from_str(&tool_args).unwrap_or(serde_json::json!({}));
                            let tc = crate::capability::ToolCall {
                                name,
                                arguments: args,
                            };
                            return Some((
                                Ok(crate::streaming::LLMChunk { usage: None,
                                    content: None,
                                    tool_call: Some(tc),
                                    done: true,
                                }),
                                (stream, buffer, tool_name, tool_args),
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

#[async_trait]
impl Provider for OpenAIProvider {
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
        let body = self.build_request_body(&request);
        let url = format!(
            "{}/chat/completions",
            self.config.endpoint.trim_end_matches('/')
        );

        let mut builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");

        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        builder = builder.json(&body);

        if request.stream {
            let resp = builder
                .send()
                .await
                .map_err(|e| ProviderError::Connection(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let err_text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Request(format!(
                    "provider error {}: {}",
                    status, err_text
                )));
            }

            if allow_passthrough {
                let byte_stream = futures::StreamExt::map(resp.bytes_stream(), |res| {
                    res.map_err(|e| crate::streaming::StreamError::Connection(e.to_string()))
                });
                Ok(crate::streaming::StreamMode::Passthrough(Box::pin(
                    byte_stream,
                )))
            } else {
                Ok(crate::streaming::StreamMode::Normalized(
                    parse_openai_sse_stream(resp.bytes_stream()),
                ))
            }
        } else {
            let resp_body = builder
                .send()
                .await
                .map_err(|e| ProviderError::Connection(e.to_string()))?;

            if !resp_body.status().is_success() {
                let status = resp_body.status();
                let err_text = resp_body.text().await.unwrap_or_default();
                return Err(ProviderError::Request(format!(
                    "provider error {}: {}",
                    status, err_text
                )));
            }

            let resp_body = resp_body
                .json::<OpenAIChatResponse>()
                .await
                .map_err(|e| ProviderError::Internal(e.to_string()))?;

            let chunks = Self::parse_response(&resp_body);

            let s = futures::stream::iter(chunks.into_iter().map(Ok));

            Ok(crate::streaming::StreamMode::Normalized(
                Box::pin(s) as LLMStream
            ))
        }
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            tool_calling: crate::provider::ToolCallingSupport::Native,
            image_input: true,
            streaming: true,
        }
    }

    fn name(&self) -> &str {
        "openai"
    }
}
