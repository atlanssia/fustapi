//! `OpenAI` cloud provider adapter.
//!
//! Forwards requests to any OpenAI-compatible API (local LLM, `OpenAI`, etc.).

use async_trait::async_trait;
use serde::Deserialize;
use tokio_stream::StreamExt;

use crate::provider::{
    BalanceStatus, ConfigSummary, Metric, MetricKind, MetricStatus, Provider, ProviderBalance,
    ProviderError, ToolCallingSupport, UnifiedRequest,
};
use crate::streaming::{LLMChunk, LLMStream};

/// `OpenAI` provider configuration.
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: Option<String>,
    /// Whether to send `stream_options.include_usage` in streaming requests.
    /// Disable for providers that don't support this `OpenAI` extension (e.g. GLM).
    pub stream_options: bool,
    /// Provider-override for the name returned by `Provider::name()`.
    /// If `None`, falls back to the default name derived from the type.
    pub provider_name: Option<String>,
    /// Tool calling support override.
    pub tool_calling: ToolCallingSupport,
    /// Image input support override.
    pub image_input: bool,
    /// Streaming support override.
    pub streaming: bool,
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: None,
            stream_options: true,
            provider_name: None,
            tool_calling: ToolCallingSupport::Native,
            image_input: true,
            streaming: true,
        }
    }
}

/// `OpenAI` provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    config: OpenAIConfig,
    client: reqwest::Client,
}

impl OpenAIProvider {
    #[must_use]
    pub fn new(config: OpenAIConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    #[must_use]
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Build the OpenAI-compatible request body from a `UnifiedRequest`.
    #[must_use]
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

                    let url = if source.starts_with("data:") { source } else { format!("data:{mime};base64,{source}") };

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
                    "id": tc.id.clone().unwrap_or_else(|| format!("call_{i}")),
                    "type": "function",
                    "function": { "name": tc.name.clone(), "arguments": tc.arguments.to_string() }
                })).collect::<Vec<_>>();
                m["tool_calls"] = serde_json::json!(calls);
            }

            if msg.role == crate::provider::Role::Tool
                && let Some(tc_id) = &msg.tool_call_id {
                    m["tool_call_id"] = serde_json::json!(tc_id);
                }

            // Forward provider-specific extras (e.g., DeepSeek reasoning_content).
            if let Some(extras) = &msg.extras {
                for (key, value) in extras {
                    m[key] = value.clone();
                }
            }

            m
        }).collect::<Vec<_>>();

        let model = self
            .config
            .model
            .as_ref()
            .filter(|m| !m.is_empty())
            .unwrap_or(&request.model);
        let mut body =
            serde_json::json!({ "model": model, "messages": messages, "stream": request.stream });

        // Request usage data in streaming responses.
        if request.stream && self.config.stream_options {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }

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

        // Forward only known OpenAI-compatible parameters.
        // Providers like GLM reject unknown fields (e.g., top_k from Anthropic protocol).
        const KNOWN_PARAMS: &[&str] = &[
            "top_p",
            "n",
            "stop",
            "frequency_penalty",
            "presence_penalty",
            "logprobs",
            "top_logprobs",
            "response_format",
            "seed",
            "logit_bias",
            "user",
            "service_tier",
            "parallel_tool_calls",
        ];
        for (key, value) in &request.extra_params {
            if KNOWN_PARAMS.contains(&key.as_str()) {
                body[key] = value.clone();
            }
        }

        body
    }

    /// Parse a non-streaming response into `LLMChunks`.
    #[must_use]
    pub fn parse_response(response: &OpenAIChatResponse) -> Vec<LLMChunk> {
        let mut chunks = Vec::new();

        if let Some(choice) = response.choices.first() {
            if let Some(content) = &choice.message.content {
                chunks.push(LLMChunk {
                    reasoning_content: None,
                    usage: None,
                    content: Some(content.clone()),
                    tool_call: None,
                    done: false,
                });
            }

            if let Some(tool_calls) = &choice.message.tool_calls {
                for tc in tool_calls {
                    chunks.push(LLMChunk {
                        reasoning_content: None,
                        usage: None,
                        content: None,
                        tool_call: Some(crate::capability::ToolCall {
                            id: Some(tc.id.clone()),
                            name: tc.function.name.clone(),
                            arguments: serde_json::from_str(&tc.function.arguments)
                                .unwrap_or_default(),
                        }),
                        done: false,
                    });
                }
            }

            let usage = response.usage.as_ref().map(|u| crate::metrics::TokenUsage {
                prompt_tokens: u.prompt_tokens as u32,
                completion_tokens: u.completion_tokens as u32,
            });
            chunks.push(LLMChunk {
                reasoning_content: None,
                usage,
                content: None,
                tool_call: None,
                done: true,
            });
        }

        chunks
    }

    /// Fetch available models from the provider's `/v1/models` endpoint.
    async fn fetch_model_list(&self) -> Result<Vec<String>, ProviderError> {
        let base = self
            .config
            .endpoint
            .trim_end_matches("/v1")
            .trim_end_matches('/');
        let is_local = self.config.endpoint.contains("localhost")
            || self.config.endpoint.contains("127.0.0.1")
            || self.config.endpoint.contains("::1");

        let mut builder = self.client.get(format!("{}/v1/models", base));
        if !self.config.api_key.is_empty() && !is_local {
            builder =
                builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }
        match builder.send().await {
            Ok(resp) if resp.status().is_success() => resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| {
                    v.get("data")?
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| m.get("id")?.as_str().map(String::from))
                                .collect()
                        })
                })
                .ok_or_else(|| ProviderError::Internal("Failed to parse models response".into())),
            Ok(_) => Err(ProviderError::Connection(
                "models endpoint returned non-success status".into(),
            )),
            Err(e) => Err(ProviderError::Connection(e.to_string())),
        }
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
                                    id: tool_id.take(),
                                    name,
                                    arguments: args,
                                };
                                return Some((
                                    Ok(crate::streaming::LLMChunk {
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
                                Ok(crate::streaming::LLMChunk {
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
                                        Ok(crate::streaming::LLMChunk {
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
                                    Ok(crate::streaming::LLMChunk {
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
                                    Ok(crate::streaming::LLMChunk {
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
                                            Ok(crate::streaming::LLMChunk {
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
                        let text = String::from_utf8_lossy(&bytes);
                        buffer.push_str(&text);
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
                                Ok(crate::streaming::LLMChunk {
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
                    "provider error {status}: {err_text}"
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
                    "provider error {status}: {err_text}"
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
            tool_calling: self.config.tool_calling,
            image_input: self.config.image_input,
            streaming: self.config.streaming,
        }
    }

    fn name(&self) -> &str {
        self.config
            .provider_name
            .as_deref()
            .unwrap_or("openai")
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        self.fetch_model_list().await
    }

    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        let is_local = self.config.endpoint.contains("localhost")
            || self.config.endpoint.contains("127.0.0.1")
            || self.config.endpoint.contains("::1");

        let base = self
            .config
            .endpoint
            .trim_end_matches("/v1")
            .trim_end_matches('/');

        // Strategy 1 (local only): Try /health endpoint
        //   omlx returns rich JSON with engine_pool, vLLM/SGLang return empty 200
        let health_ok = if is_local {
            match self.client.get(format!("{base}/health")).send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.text().await {
                        Ok(text) if text.trim().is_empty() => Some((true, None)),
                        Ok(text) => {
                            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
                            parsed.ok().map(|v| (true, Some(v)))
                        }
                        Err(_) => Some((true, None)),
                    }
                }
                Ok(_) => Some((false, None)),
                Err(_) => None,
            }
        } else {
            None
        };

        // Strategy 2: Try /v1/models for model listing (all endpoints)
        //   Local: fallback when /health didn't return data
        //   Remote: liveness check via OpenAI-compatible endpoint
        let need_models = health_ok.is_none()
            || health_ok.as_ref().is_some_and(|(ok, _)| !ok);
        let models_data: Option<Vec<String>> = if need_models {
            self.fetch_model_list().await.ok()
        } else {
            None
        };

        // Nothing reachable at all → offline
        if health_ok.is_none() && models_data.is_none() {
            return Ok(Some(ProviderBalance {
                provider_name: "local".to_string(),
                status: BalanceStatus::Offline,
                plan: None,
                plan_type: None,
                alerts: vec![],
                metrics: vec![],
                breakdown: vec![],
                resets: vec![],
                config_summary: ConfigSummary {
                    provider_type: "local".to_string(),
                    endpoint: self.config.endpoint.clone(),
                    has_key: !self.config.api_key.is_empty(),
                    model: self.config.model.clone(),
                },
            }));
        }

        // Build result from available data
        let mut status = BalanceStatus::Online;
        let mut metrics = Vec::new();
        let mut alerts = Vec::new();
        let mut detected_model: Option<String> = None;

        if let Some((_is_healthy, Some(json_body))) = &health_ok {
            // Rich JSON health response (omlx-style)
            let status_str = json_body.get("status").and_then(|v| v.as_str()).unwrap_or("");
            // Accept: "healthy", "ok", "running", "up", "ready"
            let healthy = matches!(status_str, "healthy" | "ok" | "running" | "up" | "ready" | "");
            if !healthy && !status_str.is_empty() {
                status = BalanceStatus::Error;
            }

            detected_model = json_body
                .get("default_model")
                .and_then(|v| v.as_str())
                .map(String::from);

            let pool = &json_body["engine_pool"];
            let model_count = pool.get("model_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let loaded_count = pool.get("loaded_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let max_mem = pool.get("max_model_memory").and_then(|v| v.as_u64()).unwrap_or(0);
            let cur_mem = pool.get("current_model_memory").and_then(|v| v.as_u64()).unwrap_or(0);

            let mem_pct = if max_mem > 0 {
                (cur_mem as f64 / max_mem as f64 * 10000.0).round() / 100.0
            } else {
                0.0
            };

            if model_count > 0 || max_mem > 0 {
                metrics.push(Metric {
                    label: "Models".to_string(),
                    kind: MetricKind::Absolute,
                    value: model_count as f64,
                    total: None,
                    unit: Some("available".to_string()),
                    percentage: None,
                    status: MetricStatus::Ok,
                    reset_at_ms: None,
                });
                metrics.push(Metric {
                    label: "Loaded".to_string(),
                    kind: MetricKind::Absolute,
                    value: loaded_count as f64,
                    total: Some(model_count as f64),
                    unit: Some("models".to_string()),
                    percentage: None,
                    status: if loaded_count == 0 && model_count > 0 {
                        MetricStatus::Warn
                    } else {
                        MetricStatus::Ok
                    },
                    reset_at_ms: None,
                });
                metrics.push(Metric {
                    label: "VRAM".to_string(),
                    kind: MetricKind::Percentage,
                    value: mem_pct,
                    total: Some(100.0),
                    unit: Some("%".to_string()),
                    percentage: Some(mem_pct),
                    status: MetricStatus::from_percentage(mem_pct),
                    reset_at_ms: None,
                });

                if mem_pct >= 95.0 {
                    alerts.push(crate::provider::Alert {
                        level: crate::provider::AlertLevel::Critical,
                        message: format!("VRAM usage {:.0}% — cannot load more models", mem_pct),
                    });
                } else if mem_pct >= 80.0 {
                    alerts.push(crate::provider::Alert {
                        level: crate::provider::AlertLevel::Warn,
                        message: format!("VRAM usage {:.0}% — approaching limit", mem_pct),
                    });
                }
            }
        }

        // Fallback: use /v1/models data if no rich health info
        if metrics.is_empty()
            && let Some(model_ids) = &models_data
        {
            metrics.push(Metric {
                label: "Models".to_string(),
                kind: MetricKind::Absolute,
                value: model_ids.len() as f64,
                total: None,
                unit: Some("loaded".to_string()),
                percentage: None,
                status: if model_ids.is_empty() {
                    MetricStatus::Warn
                } else {
                    MetricStatus::Ok
                },
                reset_at_ms: None,
            });
            if detected_model.is_none() {
                detected_model = model_ids.first().cloned();
            }
        }

        Ok(Some(ProviderBalance {
            provider_name: "local".to_string(),
            status,
            plan: detected_model.clone(),
            plan_type: None,
            alerts,
            metrics,
            breakdown: vec![],
            resets: vec![],
            config_summary: ConfigSummary {
                provider_type: "local".to_string(),
                endpoint: self.config.endpoint.clone(),
                has_key: !self.config.api_key.is_empty(),
                model: self.config.model.clone().or(detected_model),
            },
        }))
    }
}
