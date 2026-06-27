//! `OpenAI` cloud provider adapter.
//!
//! Forwards requests to any OpenAI-compatible API (local LLM, `OpenAI`, etc.).

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{
    Provider, ProviderBalance, ProviderError, ToolCallingSupport, UnifiedRequest,
};
use crate::streaming::LLMChunk;

// Re-export the SSE parser for backward compatibility.
pub use super::sse_parser::parse_openai_sse_stream;

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
    /// Whether the upstream supports the Responses API natively.
    /// Default `true` for the canonical `OpenAI` endpoint; `create_provider`
    /// sets it explicitly per provider type.
    pub supports_responses: bool,
    /// Whether the upstream supports the Anthropic Messages API natively.
    /// When `true`, `/v1/messages` requests are forwarded as-is.
    pub supports_anthropic: bool,
    /// Balance strategy — selects the provider-specific balance query logic.
    pub balance_strategy: BalanceStrategy,
}

/// Provider-specific balance query strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BalanceStrategy {
    /// Default: multi-strategy health probing (/health, /v1/models, offline detection).
    #[default]
    Default,
    /// omlx: query /health endpoint and parse with parse_omlx_balance.
    Omlx,
    /// DeepSeek: query /user/balance endpoint and parse with parse_deepseek_balance.
    DeepSeek,
    /// GLM: query /api/monitor/usage/quota/limit and parse with parse_glm_balance.
    Glm,
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
            supports_responses: true,
            supports_anthropic: false,
            balance_strategy: BalanceStrategy::Default,
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
            client: crate::provider::build_http_client(),
        }
    }

    #[must_use]
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Target URL for Responses API: `{endpoint}/responses`.
    #[must_use]
    pub(crate) fn responses_target_url(&self) -> String {
        self.config.api_url("/responses")
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
            if let Some(reasoning) = choice
                .message
                .reasoning_content
                .as_ref()
                .filter(|r| !r.is_empty())
            {
                chunks.push(LLMChunk {
                    reasoning_content: Some(reasoning.clone()),
                    usage: None,
                    content: None,
                    tool_call: None,
                    done: false,
                });
            }

            if let Some(content) = choice.message.content.as_ref().filter(|c| !c.is_empty()) {
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
    pub async fn fetch_model_list(&self) -> Result<Vec<String>, ProviderError> {
        let is_local = self.config.endpoint.contains("localhost")
            || self.config.endpoint.contains("127.0.0.1")
            || self.config.endpoint.contains("::1");

        let mut builder = self.client.get(self.config.metadata_url("/v1/models"));
        if !self.config.api_key.is_empty() && !is_local {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }
        match builder.send().await {
            Ok(resp) if resp.status().is_success() => resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| {
                    v.get("data")?.as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|m| m.get("id")?.as_str().map(String::from))
                            .collect()
                    })
                })
                .ok_or_else(|| ProviderError::Internal("Failed to parse models response".into())),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                Err(classify_http_error(
                    status,
                    body,
                    self.name(),
                    self.config.model.as_deref().unwrap_or("?"),
                ))
            }
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
    /// Reasoning/thinking content from providers like DeepSeek and GLM.
    #[serde(default)]
    pub reasoning_content: Option<String>,
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

/// Send a request with one-shot retry for transient TCP connect errors.
async fn send_with_tcp_retry(
    builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response, ProviderError> {
    let retry = builder.try_clone();
    match builder.send().await {
        Ok(resp) => Ok(resp),
        Err(e) if e.is_connect() => {
            tracing::warn!(error = %e, "transient connect error, retrying once");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            match retry {
                Some(r) => r
                    .send()
                    .await
                    .map_err(|e| ProviderError::Connection(e.to_string())),
                None => Err(ProviderError::Connection(e.to_string())),
            }
        }
        Err(e) => Err(ProviderError::Connection(e.to_string())),
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
        let body = self.build_request_body(&request);
        let url = self.config.api_url("/chat/completions");

        let mut builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");

        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        builder = builder.json(&body);

        if request.stream {
            let resp = send_with_tcp_retry(builder).await?;

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                return Err(classify_http_error(
                    status,
                    body,
                    self.name(),
                    self.config.model.as_deref().unwrap_or("?"),
                ));
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
            let resp_body = send_with_tcp_retry(builder).await?;

            if !resp_body.status().is_success() {
                let status = resp_body.status().as_u16();
                let body = resp_body.text().await.unwrap_or_default();
                return Err(classify_http_error(
                    status,
                    body,
                    self.name(),
                    self.config.model.as_deref().unwrap_or("?"),
                ));
            }

            // Read the full response body and return as raw JSON, avoiding the
            // parse → chunks → re-serialize round-trip.
            let full_bytes = resp_body
                .bytes()
                .await
                .map_err(|e| ProviderError::Internal(e.to_string()))?;

            let json_value: serde_json::Value = serde_json::from_slice(&full_bytes)
                .map_err(|e| ProviderError::Internal(e.to_string()))?;

            Ok(crate::streaming::StreamMode::NonStreaming(json_value))
        }
    }

    async fn chat_raw_non_streaming(
        &self,
        body: String,
    ) -> Result<serde_json::Value, ProviderError> {
        let url = self.config.api_url("/chat/completions");

        let mut builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body);

        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let resp_body = send_with_tcp_retry(builder).await?;
        if !resp_body.status().is_success() {
            let status = resp_body.status().as_u16();
            let body = resp_body.text().await.unwrap_or_default();
            return Err(classify_http_error(
                status,
                body,
                self.name(),
                self.config.model.as_deref().unwrap_or("?"),
            ));
        }

        let full_bytes = resp_body
            .bytes()
            .await
            .map_err(|e| ProviderError::Internal(e.to_string()))?;
        serde_json::from_slice(&full_bytes).map_err(|e| ProviderError::Internal(e.to_string()))
    }

    async fn responses_passthrough(
        &self,
        body: String,
        stream: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
        let url = self.responses_target_url();
        let mut builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");

        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        // Forward the raw body verbatim — do not re-serialize.
        let builder = builder.body(body);
        let resp = send_with_tcp_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(
                status,
                body,
                self.name(),
                self.config.model.as_deref().unwrap_or("?"),
            ));
        }

        if stream {
            let byte_stream = futures::StreamExt::map(resp.bytes_stream(), |res| {
                res.map_err(|e| crate::streaming::StreamError::Connection(e.to_string()))
            });
            Ok(crate::streaming::StreamMode::Passthrough(Box::pin(
                byte_stream,
            )))
        } else {
            let full = resp
                .bytes()
                .await
                .map_err(|e| ProviderError::Internal(e.to_string()))?;
            let json: serde_json::Value = serde_json::from_slice(&full)
                .map_err(|e| ProviderError::Internal(e.to_string()))?;
            Ok(crate::streaming::StreamMode::NonStreaming(json))
        }
    }

    async fn anthropic_passthrough(
        &self,
        body: String,
        stream: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
        let url = self.config.api_url("/v1/messages");
        let mut builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01");

        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let builder = builder.body(body);
        let resp = send_with_tcp_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(
                status,
                body,
                self.name(),
                self.config.model.as_deref().unwrap_or("?"),
            ));
        }

        if stream {
            let byte_stream = futures::StreamExt::map(resp.bytes_stream(), |res| {
                res.map_err(|e| crate::streaming::StreamError::Connection(e.to_string()))
            });
            Ok(crate::streaming::StreamMode::Passthrough(Box::pin(
                byte_stream,
            )))
        } else {
            let full = resp
                .bytes()
                .await
                .map_err(|e| ProviderError::Internal(e.to_string()))?;
            let json: serde_json::Value = serde_json::from_slice(&full)
                .map_err(|e| ProviderError::Internal(e.to_string()))?;
            Ok(crate::streaming::StreamMode::NonStreaming(json))
        }
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            tool_calling: self.config.tool_calling,
            image_input: self.config.image_input,
            streaming: self.config.streaming,
            supports_responses: self.config.supports_responses,
            supports_anthropic: self.config.supports_anthropic,
        }
    }

    fn name(&self) -> &str {
        self.config.provider_name.as_deref().unwrap_or("OpenAI")
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        match self.config.balance_strategy {
            BalanceStrategy::Glm => self.list_models_glm().await,
            _ => self.fetch_model_list().await,
        }
    }

    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        match self.config.balance_strategy {
            BalanceStrategy::Default => self.balance_default().await,
            BalanceStrategy::Omlx => self.balance_omlx().await,
            BalanceStrategy::DeepSeek => self.balance_deepseek().await,
            BalanceStrategy::Glm => self.balance_glm().await,
        }
    }
}

// ── Strategy implementations ─────────────────────────────────────────

impl OpenAIProvider {
    /// GLM uses `{endpoint}/models` (not `{base}/v1/models`) and sends the
    /// API key directly without the "Bearer " prefix.
    async fn list_models_glm(&self) -> Result<Vec<String>, ProviderError> {
        // GLM's models endpoint lives at /api/paas/v4/models regardless of
        // the chat endpoint path (e.g. /api/coding/paas/v4). Extract the host
        // and use the standard path, same pattern as balance_glm.
        let models_url = if let Some(pos) = self.config.endpoint.find("/api/") {
            let host = &self.config.endpoint[..pos];
            format!("{host}/api/paas/v4/models")
        } else {
            format!("{}/models", self.config.endpoint.trim_end_matches('/'))
        };
        let mut req = self.client.get(&models_url);
        if !self.config.api_key.is_empty() {
            req = req.header("Authorization", &self.config.api_key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(
                status,
                body,
                self.name(),
                self.config.model.as_deref().unwrap_or("?"),
            ));
        }

        resp.json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| {
                v.get("data")?.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id")?.as_str().map(String::from))
                        .collect()
                })
            })
            .ok_or_else(|| ProviderError::Internal("Failed to parse GLM models response".into()))
    }

    async fn balance_default(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        let client = self.client.clone();
        let config = self.config.clone();
        let name = self.name().to_string();
        let fetch = Box::pin(async move {
            let local = super::health_prober::is_local(&config.endpoint);
            let mut builder = client.get(config.metadata_url("/v1/models"));
            if !config.api_key.is_empty() && !local {
                builder = builder.header("Authorization", format!("Bearer {}", config.api_key));
            }
            match builder.send().await {
                Ok(resp) if resp.status().is_success() => resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| {
                        v.get("data")?.as_array().map(|arr| {
                            arr.iter()
                                .filter_map(|m| m.get("id")?.as_str().map(String::from))
                                .collect()
                        })
                    })
                    .ok_or_else(|| {
                        ProviderError::Internal("Failed to parse models response".into())
                    }),
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let body = resp.text().await.unwrap_or_default();
                    Err(classify_http_error(
                        status,
                        body,
                        config.provider_name.as_deref().unwrap_or("OpenAI"),
                        config.model.as_deref().unwrap_or("?"),
                    ))
                }
                Err(e) => Err(ProviderError::Connection(e.to_string())),
            }
        });
        super::health_prober::probe_balance(&self.client, &self.config, &name, fetch).await
    }

    async fn balance_omlx(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        let url = self.config.metadata_url("/health");

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(
                status,
                body,
                self.name(),
                self.config.model.as_deref().unwrap_or("?"),
            ));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| ProviderError::Internal(e.to_string()))?;

        let balance = crate::provider::health::parse_omlx_balance(
            &body,
            &self.config.endpoint,
            self.config.model.as_deref(),
        )
        .map_err(ProviderError::Internal)?;

        Ok(Some(balance))
    }

    async fn balance_deepseek(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        // Extract the origin (scheme + host + port) so that path prefixes
        // like /anthropic don't leak into the /user/balance URL.
        // e.g. "https://api.deepseek.com/anthropic" → "https://api.deepseek.com"
        let url = self.config.metadata_url("/user/balance");
        let mut builder = self.client.get(&url).header("Accept", "application/json");
        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(
                status,
                body,
                self.name(),
                self.config.model.as_deref().unwrap_or("?"),
            ));
        }

        let body = resp.text().await.unwrap_or_default();
        let has_key = !self.config.api_key.is_empty();
        let balance =
            crate::provider::health::parse_deepseek_balance(&body, &self.config.endpoint, has_key)
                .map_err(ProviderError::Internal)?;

        Ok(Some(balance))
    }

    async fn balance_glm(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        let balance_url = if let Some(pos) = self.config.endpoint.find("/api/") {
            let host = &self.config.endpoint[..pos];
            format!("{host}/api/monitor/usage/quota/limit")
        } else {
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit".to_string()
        };

        let resp = self
            .client
            .get(&balance_url)
            .header("Authorization", &self.config.api_key)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(
                status,
                body,
                self.name(),
                self.config.model.as_deref().unwrap_or("?"),
            ));
        }

        let body = resp.text().await.unwrap_or_default();
        let has_key = !self.config.api_key.is_empty();
        crate::provider::health::parse_glm_balance(
            &body,
            &self.config.endpoint,
            has_key,
            self.config.model.as_deref(),
        )
        .map_err(ProviderError::Internal)
    }
}

// ── URL construction ──────────────────────────────────────────────────

impl OpenAIConfig {
    /// Build a URL for a standard metadata endpoint (`/v1/models`, `/health`,
    /// `/user/balance`). Extracts the origin so path prefixes like `/anthropic`
    /// or `/v1` don't leak into the URL.
    ///
    /// For providers with non-standard API path layouts (e.g. GLM's
    /// `/api/coding/paas/v4`), use direct construction — this helper only
    /// handles origin-based metadata endpoints.
    pub fn metadata_url(&self, path: &str) -> String {
        let origin: String = self
            .endpoint
            .trim_end_matches('/')
            .split('/')
            .take(3)
            .collect::<Vec<_>>()
            .join("/");
        format!("{origin}{path}")
    }

    /// Build a URL by appending `path` directly to the configured endpoint.
    /// Use for chat endpoints (/chat/completions, /v1/messages, /responses)
    /// where the path prefix is part of the upstream's API routing.
    pub fn api_url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint.trim_end_matches('/'))
    }
}

// ── HTTP error classification ──────────────────────────────────────────

/// Classify a non-success HTTP status into the appropriate `ProviderError`.
fn classify_http_error(status: u16, body: String, provider: &str, model: &str) -> ProviderError {
    // Truncate for logging — upstream error pages can be large HTML.
    // Use char-based truncation to avoid splitting multi-byte UTF-8.
    let log_body: std::borrow::Cow<'_, str> = if body.len() > 512 {
        std::borrow::Cow::Owned(format!("{}…", body.chars().take(512).collect::<String>()))
    } else {
        std::borrow::Cow::Borrowed(&body)
    };
    if (400..500).contains(&status) {
        tracing::warn!(status, %log_body, provider, model, "upstream returned client error");
        ProviderError::Upstream {
            status,
            message: body,
        }
    } else {
        tracing::error!(status, %log_body, provider, model, "upstream returned server error");
        ProviderError::Request(format!("provider error {status}: {body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::LLMStream;

    // ── classify_http_error tests ──────────────────────────────────────

    // Helper: classify with dummy provider/model context (logging only).
    fn cls(status: u16, body: &str) -> ProviderError {
        classify_http_error(status, body.into(), "test-provider", "test-model")
    }

    #[test]
    fn classify_4xx_returns_upstream_error() {
        let err = cls(429, "rate limited");
        match err {
            ProviderError::Upstream { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(message, "rate limited");
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[test]
    fn classify_5xx_returns_request_error() {
        let err = cls(502, "bad gateway");
        match err {
            ProviderError::Request(msg) => {
                assert!(msg.contains("502"));
                assert!(msg.contains("bad gateway"));
            }
            other => panic!("expected Request, got {other:?}"),
        }
    }

    #[test]
    fn classify_400_boundary_is_upstream() {
        let err = cls(400, "bad request");
        assert!(matches!(err, ProviderError::Upstream { .. }));
    }

    #[test]
    fn classify_499_boundary_is_upstream() {
        let err = cls(499, "client closed");
        assert!(matches!(err, ProviderError::Upstream { .. }));
    }

    #[test]
    fn classify_500_boundary_is_request() {
        let err = cls(500, "internal error");
        assert!(matches!(err, ProviderError::Request(_)));
    }

    fn chat_response_with_reasoning(reasoning: &str, content: &str) -> OpenAIChatResponse {
        OpenAIChatResponse {
            id: "test".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "glm-5.1".to_string(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: OpenAIMessageOut {
                    role: "assistant".to_string(),
                    content: if content.is_empty() {
                        Some(String::new())
                    } else {
                        Some(content.to_string())
                    },
                    tool_calls: None,
                    reasoning_content: if reasoning.is_empty() {
                        None
                    } else {
                        Some(reasoning.to_string())
                    },
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(OpenAIUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
            }),
        }
    }

    #[test]
    fn parse_response_extracts_reasoning_content() {
        let resp = chat_response_with_reasoning("thinking about it...", "answer");
        let chunks = OpenAIProvider::parse_response(&resp);
        assert!(chunks.iter().any(|c| {
            c.reasoning_content
                .as_ref()
                .is_some_and(|r| r == "thinking about it...")
        }));
        assert!(
            chunks
                .iter()
                .any(|c| c.content.as_ref().is_some_and(|c| c == "answer"))
        );
    }

    #[test]
    fn parse_response_reasoning_only_no_content() {
        let resp = chat_response_with_reasoning("thinking...", "");
        let chunks = OpenAIProvider::parse_response(&resp);
        assert!(
            chunks
                .iter()
                .any(|c| c.reasoning_content.is_some() && c.content.is_none())
        );
        assert!(
            !chunks
                .iter()
                .any(|c| c.content.as_ref().is_some_and(|t| !t.is_empty()))
        );
    }

    #[test]
    fn parse_response_empty_strings_skipped() {
        let resp = chat_response_with_reasoning("", "");
        let chunks = OpenAIProvider::parse_response(&resp);
        assert!(
            !chunks
                .iter()
                .any(|c| c.content.is_some() || c.reasoning_content.is_some())
        );
    }

    // ── SSE stream characterization tests ────────────────────────────────

    /// Helper: build a byte stream from raw SSE-formatted strings.
    fn raw_sse_stream(
        chunks: Vec<&str>,
    ) -> impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + Unpin + 'static
    {
        let full = chunks
            .into_iter()
            .map(|s| bytes::Bytes::from(s.to_string()))
            .collect::<Vec<_>>();
        futures::stream::iter(full.into_iter().map(Ok::<_, reqwest::Error>))
    }

    /// Collect all chunks from an LLMStream into a Vec.
    async fn collect_stream(
        stream: LLMStream,
    ) -> Vec<Result<LLMChunk, crate::streaming::StreamError>> {
        use tokio_stream::StreamExt;
        let mut s = stream;
        let mut out = Vec::new();
        while let Some(item) = s.next().await {
            out.push(item);
        }
        out
    }

    #[tokio::test]
    async fn sse_content_delta_extraction() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        assert!(chunks.len() >= 2);
        let content_chunks: Vec<&str> = chunks
            .iter()
            .filter_map(|c| c.as_ref().ok().and_then(|c| c.content.as_deref()))
            .collect();
        assert!(content_chunks.contains(&"Hello"));
        assert!(content_chunks.contains(&" world"));
    }

    #[tokio::test]
    async fn sse_reasoning_content_extraction() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let has_reasoning = chunks.iter().any(|c| {
            c.as_ref()
                .is_ok_and(|c| c.reasoning_content.as_deref() == Some("Let me think"))
        });
        assert!(has_reasoning, "should find reasoning_content chunk");
        let has_content = chunks.iter().any(|c| {
            c.as_ref()
                .is_ok_and(|c| c.content.as_deref() == Some("answer"))
        });
        assert!(has_content, "should find content chunk");
    }

    #[tokio::test]
    async fn sse_tool_call_accumulation_across_chunks() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_1\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"\\\"Tokyo\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let tool_chunks: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.as_ref().ok().and_then(|c| c.tool_call.as_ref()))
            .collect();
        assert!(
            !tool_chunks.is_empty(),
            "should produce at least one tool call chunk"
        );
        let tc = tool_chunks.last().unwrap();
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.id, Some("call_1".to_string()));
        assert_eq!(tc.arguments["city"], "Tokyo");
    }

    #[tokio::test]
    async fn sse_multiple_tool_calls_flushed_correctly() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_a\",\"function\":{\"name\":\"fn_a\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_b\",\"function\":{\"name\":\"fn_b\",\"arguments\":\"[]\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let tool_chunks: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.as_ref().ok().and_then(|c| c.tool_call.as_ref()))
            .collect();
        // fn_a flushed when fn_b starts, fn_b flushed at [DONE]
        assert_eq!(tool_chunks.len(), 2, "should flush both tool calls");
        assert_eq!(tool_chunks[0].name, "fn_a");
        assert_eq!(tool_chunks[1].name, "fn_b");
    }

    #[tokio::test]
    async fn sse_done_handling() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let last = chunks.last().expect("should have at least one chunk");
        let last_chunk = last.as_ref().expect("should be Ok");
        assert!(last_chunk.done, "last chunk should have done=true");
    }

    #[tokio::test]
    async fn sse_done_with_pending_tool_call() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_x\",\"function\":{\"name\":\"my_fn\",\"arguments\":\"{\\\"a\\\":1}\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let done_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.as_ref().is_ok_and(|c| c.done))
            .collect();
        assert_eq!(done_chunks.len(), 1);
        let tc = done_chunks[0]
            .as_ref()
            .ok()
            .unwrap()
            .tool_call
            .as_ref()
            .expect("should have tool call");
        assert_eq!(tc.name, "my_fn");
    }

    #[tokio::test]
    async fn sse_usage_extraction_inline() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let has_usage = chunks.iter().any(|c| {
            c.as_ref().is_ok_and(|c| {
                c.usage
                    .as_ref()
                    .is_some_and(|u| u.prompt_tokens == 5 && u.completion_tokens == 3)
            })
        });
        assert!(has_usage, "should find inline usage");
    }

    #[tokio::test]
    async fn sse_usage_only_chunk() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50}}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let usage_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.as_ref().is_ok_and(|c| c.usage.is_some()))
            .collect();
        assert_eq!(usage_chunks.len(), 1);
        let u = usage_chunks[0]
            .as_ref()
            .ok()
            .unwrap()
            .usage
            .as_ref()
            .unwrap();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
    }

    #[tokio::test]
    async fn sse_context_window_exceeded_error() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"finish_reason\":\"model_context_window_exceeded\"}]}\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let has_error = chunks.iter().any(|c| {
            c.as_ref()
                .is_err_and(|e| format!("{e}").contains("context_window"))
        });
        assert!(has_error, "should detect context_window_exceeded error");
    }

    #[tokio::test]
    async fn sse_error_finish_reason() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"finish_reason\":\"error\"}]}\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let has_error = chunks.iter().any(|c| {
            c.as_ref()
                .is_err_and(|e| format!("{e}").contains("upstream error"))
        });
        assert!(has_error, "should detect error finish_reason");
    }

    #[tokio::test]
    async fn sse_context_length_error() {
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"finish_reason\":\"context_length_exceeded\"}]}\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let has_error = chunks.iter().any(|c| {
            c.as_ref()
                .is_err_and(|e| format!("{e}").contains("context_length"))
        });
        assert!(has_error, "should detect context_length_exceeded error");
    }

    #[tokio::test]
    async fn sse_stream_end_with_pending_tool_call() {
        // Stream ends (no more bytes) while a tool call is pending
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_z\",\"function\":{\"name\":\"pending_fn\",\"arguments\":\"{\\\"x\\\":2}\"}}]}}]}\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let done_with_tc = chunks.iter().any(|c| {
            c.as_ref().is_ok_and(|c| {
                c.done
                    && c.tool_call
                        .as_ref()
                        .is_some_and(|tc| tc.name == "pending_fn")
            })
        });
        assert!(
            done_with_tc,
            "should flush pending tool call when stream ends"
        );
    }

    #[tokio::test]
    async fn sse_empty_data_lines_ignored() {
        let stream = raw_sse_stream(vec![
            "data: \n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let content_chunks: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.as_ref().ok().and_then(|c| c.content.as_deref()))
            .collect();
        assert!(content_chunks.contains(&"ok"));
    }

    #[tokio::test]
    async fn sse_network_error_propagated() {
        // Build a real reqwest error by hitting an unreachable port, then wrap in a stream.
        let client = reqwest::Client::new();
        let result = client
            .get("http://0.0.0.0:1")
            .timeout(std::time::Duration::from_millis(1))
            .send()
            .await;
        let err = result.unwrap_err();
        let err_stream: std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>,
        > = Box::pin(futures::stream::once(async { Err(err) }));
        let chunks = collect_stream(parse_openai_sse_stream(err_stream)).await;
        assert!(chunks.len() == 1);
        assert!(chunks[0].is_err(), "should propagate network error");
    }

    #[tokio::test]
    async fn sse_multi_byte_chunk_splitting() {
        // Test that SSE events split across multiple byte chunks are handled
        let stream = raw_sse_stream(vec![
            "data: {\"choices\":[{\"delta\":", // split mid-JSON
            "{\"content\":\"hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\ndata: [DONE]\n\n",
        ]);
        let chunks = collect_stream(parse_openai_sse_stream(stream)).await;
        let content: String = chunks
            .iter()
            .filter_map(|c| c.as_ref().ok().and_then(|c| c.content.clone()))
            .collect();
        assert_eq!(content, "hello");
    }

    // ── Health probing characterization tests ────────────────────────────

    #[test]
    fn is_local_detection() {
        let is_local = |endpoint: &str| -> bool {
            endpoint.contains("localhost")
                || endpoint.contains("127.0.0.1")
                || endpoint.contains("::1")
        };
        assert!(is_local("http://localhost:8080/v1"));
        assert!(is_local("http://127.0.0.1:8000/v1"));
        assert!(is_local("http://[::1]:8000/v1"));
        assert!(!is_local("https://api.openai.com/v1"));
        assert!(!is_local("https://open.bigmodel.cn/api/paas/v4"));
    }

    #[test]
    fn build_request_body_filters_unknown_params() {
        let provider = OpenAIProvider::new(OpenAIConfig {
            endpoint: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            model: Some("gpt-4".to_string()),
            ..Default::default()
        });
        let request = UnifiedRequest {
            model: "gpt-4".to_string(),
            messages: vec![],
            tools: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            extra_params: {
                let mut map = serde_json::Map::new();
                map.insert("top_p".to_string(), serde_json::json!(0.9));
                map.insert("unknown_param".to_string(), serde_json::json!("bad"));
                map.insert("seed".to_string(), serde_json::json!(42));
                map
            },
        };
        let body = provider.build_request_body(&request);
        assert_eq!(body["top_p"], 0.9, "known param top_p should be forwarded");
        assert_eq!(body["seed"], 42, "known param seed should be forwarded");
        assert!(
            body.get("unknown_param").is_none(),
            "unknown param should be filtered out"
        );
    }

    #[test]
    fn build_request_body_includes_stream_options() {
        let provider = OpenAIProvider::new(OpenAIConfig {
            endpoint: "https://api.openai.com/v1".to_string(),
            api_key: "test".to_string(),
            model: Some("gpt-4".to_string()),
            stream_options: true,
            ..Default::default()
        });
        let request = UnifiedRequest {
            model: "gpt-4".to_string(),
            messages: vec![],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            extra_params: serde_json::Map::new(),
        };
        let body = provider.build_request_body(&request);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn build_request_body_no_stream_options_when_disabled() {
        let provider = OpenAIProvider::new(OpenAIConfig {
            endpoint: "https://api.openai.com/v1".to_string(),
            api_key: "test".to_string(),
            model: Some("gpt-4".to_string()),
            stream_options: false,
            ..Default::default()
        });
        let request = UnifiedRequest {
            model: "gpt-4".to_string(),
            messages: vec![],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            extra_params: serde_json::Map::new(),
        };
        let body = provider.build_request_body(&request);
        assert!(
            body.get("stream_options").is_none(),
            "stream_options should not be present when disabled"
        );
    }

    #[test]
    fn build_request_body_with_tools() {
        let provider = OpenAIProvider::new(OpenAIConfig::default());
        let request = UnifiedRequest {
            model: "test".to_string(),
            messages: vec![],
            tools: Some(vec![crate::capability::ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get weather".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            }]),
            stream: false,
            temperature: None,
            max_tokens: None,
            extra_params: serde_json::Map::new(),
        };
        let body = provider.build_request_body(&request);
        let tools = body["tools"].as_array().expect("tools should be array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn responses_target_url_is_endpoint_slash_responses() {
        let cfg = OpenAIConfig {
            endpoint: "http://localhost:11434/v1".into(),
            ..Default::default()
        };
        let p = OpenAIProvider::new(cfg);
        assert_eq!(
            p.responses_target_url(),
            "http://localhost:11434/v1/responses"
        );
    }

    #[test]
    fn build_request_body_with_temperature_and_max_tokens() {
        let provider = OpenAIProvider::new(OpenAIConfig::default());
        let request = UnifiedRequest {
            model: "test".to_string(),
            messages: vec![],
            tools: None,
            stream: false,
            temperature: Some(0.7),
            max_tokens: Some(1024),
            extra_params: serde_json::Map::new(),
        };
        let body = provider.build_request_body(&request);
        // f32 -> JSON Number may lose precision, compare as approx f64
        let temp = body["temperature"]
            .as_f64()
            .expect("temperature should be number");
        assert!(
            (temp - 0.7).abs() < 0.01,
            "temperature should be ~0.7, got {temp}"
        );
        assert_eq!(body["max_tokens"], 1024);
    }

    // ── metadata_url / api_url tests ───────────────────────────────────

    fn config_with(endpoint: &str) -> OpenAIConfig {
        OpenAIConfig {
            endpoint: endpoint.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn metadata_url_strips_v1_suffix() {
        let cfg = config_with("https://api.openai.com/v1");
        assert_eq!(
            cfg.metadata_url("/v1/models"),
            "https://api.openai.com/v1/models"
        );
    }

    #[test]
    fn metadata_url_strips_anthropic_suffix() {
        let cfg = config_with("https://api.deepseek.com/anthropic");
        assert_eq!(
            cfg.metadata_url("/v1/models"),
            "https://api.deepseek.com/v1/models"
        );
    }

    #[test]
    fn metadata_url_no_path_suffix() {
        let cfg = config_with("https://api.deepseek.com");
        assert_eq!(
            cfg.metadata_url("/user/balance"),
            "https://api.deepseek.com/user/balance"
        );
    }

    #[test]
    fn metadata_url_localhost() {
        let cfg = config_with("http://127.0.0.1:8000/v1");
        assert_eq!(cfg.metadata_url("/health"), "http://127.0.0.1:8000/health");
    }

    #[test]
    fn api_url_appends_to_endpoint() {
        let cfg = config_with("https://api.deepseek.com/anthropic");
        assert_eq!(
            cfg.api_url("/v1/messages"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn api_url_standard_openai() {
        let cfg = config_with("https://api.openai.com/v1");
        assert_eq!(
            cfg.api_url("/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }
}
