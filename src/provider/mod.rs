//! Provider trait and adapter registry.
//!
//! Defines the `Provider` trait that all adapters implement, along with
//! unified types like `UnifiedRequest`, `Message`, and error types.

use crate::capability::{ImageInput, ToolCall, ToolDefinition};
use crate::streaming::StreamMode;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Level of tool calling support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallingSupport {
    Native,
    Emulated,
    Unsupported,
}

/// Provider capabilities representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub tool_calling: ToolCallingSupport,
    pub image_input: bool,
    pub streaming: bool,
}

/// Provider-agnostic chat message.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    /// Message role.
    pub role: Role,
    /// Message content (text).
    pub content: String,
    /// Optional image attachments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
    /// Optional tool calls (for assistant messages with tool responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Tool call ID (required for `tool` role messages — references the
    /// `id` field of the corresponding assistant `tool_call`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Provider-specific fields not part of the core protocol (e.g., `DeepSeek`'s
    /// `reasoning_content`). Forwarded transparently — providers that don't
    /// recognize a field simply ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extras: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Chat message role.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System message.
    System,
    /// User message.
    User,
    /// Assistant message.
    Assistant,
    /// Tool result message.
    Tool,
}

/// Unified request sent to any provider, regardless of protocol format.
#[derive(Debug, Clone)]
pub struct UnifiedRequest {
    /// Model identifier.
    pub model: String,
    /// Chat messages.
    pub messages: Vec<Message>,
    /// Optional tool definitions.
    pub tools: Option<Vec<ToolDefinition>>,
    /// Whether to stream the response.
    pub stream: bool,
    /// Sampling temperature (0.0–2.0).
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
    /// All other request parameters not explicitly parsed (`top_p`, stop, n, etc.)
    /// forwarded as-is to the upstream provider.
    pub extra_params: serde_json::Map<String, serde_json::Value>,
}

/// Provider trait — every adapter implements this.
///
/// This is the core abstraction that allows the router and protocol layers to be
/// provider-agnostic. Each provider (omlx, lmstudio, sglang, deepseek, openai) gets its own adapter.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stream a chat completion request to this provider.
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<StreamMode, ProviderError>;

    /// Get the provider's capabilities.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Human-readable provider name (e.g., "omlx", "lmstudio").
    fn name(&self) -> &str;

    /// Check if the provider is reachable/healthy.
    async fn health_check(&self) -> Result<(), ProviderError> {
        // Default: assume healthy unless overridden.
        Ok(())
    }

    /// Query the provider account balance (returns structured data).
    ///
    /// Default: `Ok(None)` — most local providers won't implement this.
    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        Ok(None)
    }

    /// List models available from this provider.
    ///
    /// Default: returns an empty list. Providers that support model listing
    /// (e.g., OpenAI-compatible endpoints) should override this.
    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(Vec::new())
    }
}

/// Errors that can occur when interacting with a provider.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Failed to connect to the provider endpoint.
    #[error("connection failed: {0}")]
    Connection(String),

    /// Provider returned an error response.
    #[error("request failed: {0}")]
    Request(String),

    /// The requested model is not available on this provider.
    #[error("model not found: {0}")]
    ModelNotFound(String),

    /// The requested capability (tool calling, images) is not supported.
    #[error("capability not supported: {0}")]
    Capability(String),

    /// Internal provider error (unexpected behavior).
    #[error("internal error: {0}")]
    Internal(String),

    /// Provider API error.
    #[error("api error: {0}")]
    Api(String),

    /// Stream error.
    #[error("stream error: {0}")]
    Stream(String),
}

// ── Unified Balance Types ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BalanceStatus {
    Online,
    Offline,
    Error,
    NoData,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PlanType {
    Coding,
    Token,
    Credit,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AlertLevel {
    Warn,
    Critical,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Percentage,
    Absolute,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MetricStatus {
    Ok,
    Warn,
    Critical,
}

impl MetricStatus {
    pub fn from_percentage(pct: f64) -> Self {
        if pct >= 95.0 {
            Self::Critical
        } else if pct >= 80.0 {
            Self::Warn
        } else {
            Self::Ok
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Metric {
    pub label: String,
    pub kind: MetricKind,
    pub value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage: Option<f64>,
    pub status: MetricStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub level: AlertLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BreakdownItem {
    pub label: String,
    pub value: f64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResetSchedule {
    pub label: String,
    pub resets_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigSummary {
    pub provider_type: String,
    pub endpoint: String,
    pub has_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderBalance {
    pub provider_name: String,
    pub status: BalanceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<PlanType>,
    pub alerts: Vec<Alert>,
    pub metrics: Vec<Metric>,
    pub breakdown: Vec<BreakdownItem>,
    pub resets: Vec<ResetSchedule>,
    pub config_summary: ConfigSummary,
}

// ── Provider module exports ───────────────────────────────────────────

pub mod cloud;
pub mod omlx;

#[cfg(test)]
mod balance_struct_tests {
    use super::*;
    use serde_json;

    #[test]
    fn provider_balance_serializes_full_example() {
        let balance = ProviderBalance {
            provider_name: "glm".into(),
            status: BalanceStatus::Online,
            plan: Some("plus".into()),
            plan_type: Some(PlanType::Coding),
            alerts: vec![Alert {
                level: AlertLevel::Warn,
                message: "Token quota 72% used".into(),
            }],
            metrics: vec![Metric {
                label: "Tokens".into(),
                kind: MetricKind::Percentage,
                value: 72.0,
                total: Some(100.0),
                unit: Some("%".into()),
                percentage: Some(72.0),
                status: MetricStatus::Ok,
                reset_at_ms: None,
            }],
            breakdown: vec![BreakdownItem {
                label: "glm-4".into(),
                value: 1240.0,
                unit: "requests".into(),
            }],
            resets: vec![ResetSchedule {
                label: "Token quota".into(),
                resets_at_ms: 1778499600000,
            }],
            config_summary: ConfigSummary {
                provider_type: "cloud".into(),
                endpoint: "open.bigmodel.cn".into(),
                has_key: true,
                model: Some("glm-4-plus".into()),
            },
        };

        let json = serde_json::to_string(&balance).expect("should serialize");
        assert!(json.contains("\"provider_name\":\"glm\""));
        assert!(json.contains("\"status\":\"online\""));
        assert!(json.contains("\"plan_type\":\"coding\""));
        assert!(json.contains("\"metrics\""));
        assert!(json.contains("\"breakdown\""));
        assert!(json.contains("\"resets\""));
        assert!(json.contains("\"config_summary\""));
    }

    #[test]
    fn provider_balance_minimal_serializes() {
        let balance = ProviderBalance {
            provider_name: "omlx".into(),
            status: BalanceStatus::Online,
            plan: None,
            plan_type: None,
            alerts: vec![],
            metrics: vec![],
            breakdown: vec![],
            resets: vec![],
            config_summary: ConfigSummary {
                provider_type: "local".into(),
                endpoint: "localhost:8000".into(),
                has_key: false,
                model: None,
            },
        };

        let json = serde_json::to_string(&balance).expect("should serialize");
        assert!(json.contains("\"provider_name\":\"omlx\""));
        assert!(json.contains("\"status\":\"online\""));
        assert!(!json.contains("\"plan\":"));
        assert!(!json.contains("\"plan_type\":"));
    }

    #[test]
    fn balance_status_enum_values() {
        assert_eq!(
            serde_json::to_string(&BalanceStatus::Online).unwrap(),
            "\"online\""
        );
        assert_eq!(
            serde_json::to_string(&BalanceStatus::Offline).unwrap(),
            "\"offline\""
        );
        assert_eq!(
            serde_json::to_string(&BalanceStatus::Error).unwrap(),
            "\"error\""
        );
        assert_eq!(
            serde_json::to_string(&BalanceStatus::NoData).unwrap(),
            "\"no_data\""
        );
    }

    #[test]
    fn metric_status_derived_from_percentage() {
        assert_eq!(MetricStatus::from_percentage(72.0), MetricStatus::Ok);
        assert_eq!(MetricStatus::from_percentage(80.0), MetricStatus::Warn);
        assert_eq!(MetricStatus::from_percentage(95.0), MetricStatus::Critical);
    }
}
