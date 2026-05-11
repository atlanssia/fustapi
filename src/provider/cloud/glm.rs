//! GLM (`ZhipuAI` / `BigModel`) cloud provider adapter.
//!
//! Wraps the OpenAI-compatible chat API and adds balance/quota checking
//! via the dedicated monitoring endpoint.

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{Provider, ProviderBalance, ProviderError, UnifiedRequest,
    BalanceStatus, PlanType, Metric, MetricKind, MetricStatus,
    Alert, AlertLevel, BreakdownItem, ResetSchedule, ConfigSummary};

/// GLM provider configuration.
#[derive(Debug, Clone)]
pub struct GlmConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: Option<String>,
}

/// GLM provider — OpenAI-compatible chat with balance monitoring.
#[derive(Debug, Clone)]
pub struct GlmProvider {
    config: GlmConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
}

impl GlmProvider {
    #[must_use]
    pub fn new(config: GlmConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: config.api_key.clone(),
                model: config.model.clone(),
                stream_options: false,
            },
        );
        Self {
            config,
            openai_backend,
        }
    }

    fn balance_url(&self) -> String {
        // The quota API is at a fixed path on the same host.
        // e.g. https://open.bigmodel.cn/api/coding/paas/v4
        //   -> https://open.bigmodel.cn/api/monitor/usage/quota/limit
        if let Some(pos) = self.config.endpoint.find("/api/") {
            let host = &self.config.endpoint[..pos];
            format!("{host}/api/monitor/usage/quota/limit")
        } else {
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit".to_string()
        }
    }
}

fn build_provider_balance(
    name: &str,
    data: &QuotaData,
    endpoint: &str,
    has_key: bool,
    model: Option<&str>,
) -> ProviderBalance {
    let level = data.level.as_deref().unwrap_or("");
    let limits = data.limits.as_deref().unwrap_or(&[]);

    let is_coding = limits
        .iter()
        .any(|l| l.limit_type.as_deref() == Some("TOKENS_LIMIT") && l.unit.is_some());

    let mut alerts = Vec::new();
    let mut metrics = Vec::new();
    let mut breakdown = Vec::new();
    let mut resets = Vec::new();

    for l in limits {
        let pct = l.percentage.unwrap_or(0.0);
        let status = MetricStatus::from_percentage(pct);

        let type_label = match l.limit_type.as_deref() {
            Some("TOKENS_LIMIT") => "Tokens",
            Some("TIME_LIMIT") => "Time",
            other => other.unwrap_or("Usage"),
        };

        metrics.push(Metric {
            label: type_label.to_string(),
            kind: MetricKind::Percentage,
            value: pct,
            total: Some(100.0),
            unit: Some("%".to_string()),
            percentage: Some(pct),
            status: status.clone(),
        });

        if pct >= 80.0 {
            let alert_level = if pct >= 95.0 { AlertLevel::Critical } else { AlertLevel::Warn };
            alerts.push(Alert {
                level: alert_level,
                message: format!("{} quota {:.0}% used", type_label, pct),
            });
        }

        if let Some(ts) = l.next_reset_time {
            resets.push(ResetSchedule {
                label: format!("{} quota", type_label),
                resets_at_ms: ts,
            });
        }

        if let Some(ref details) = l.usage_details {
            for d in details {
                breakdown.push(BreakdownItem {
                    label: d.model_code.clone().unwrap_or("?".into()),
                    value: d.usage.unwrap_or(0) as f64,
                    unit: "requests".to_string(),
                });
            }
        }
    }

    ProviderBalance {
        provider_name: name.to_string(),
        status: BalanceStatus::Online,
        plan: if level.is_empty() { None } else { Some(level.to_string()) },
        plan_type: if is_coding { Some(PlanType::Coding) } else { Some(PlanType::Token) },
        alerts,
        metrics,
        breakdown,
        resets,
        config_summary: ConfigSummary {
            provider_type: "cloud".to_string(),
            endpoint: endpoint.to_string(),
            has_key,
            model: model.map(String::from),
        },
    }
}

// ── Quota API response types ──────────────────────────────────────────

#[derive(Deserialize)]
struct GlmQuotaResponse {
    #[allow(dead_code)]
    code: Option<i32>,
    #[allow(dead_code)]
    success: Option<bool>,
    data: Option<QuotaData>,
}

#[derive(Deserialize)]
struct QuotaData {
    level: Option<String>,
    limits: Option<Vec<QuotaLimit>>,
}

#[derive(Deserialize)]
struct QuotaLimit {
    #[serde(rename = "type")]
    limit_type: Option<String>,
    /// Usage percentage (0–100).
    percentage: Option<f64>,
    /// Remaining quota amount (regular plan).
    remaining: Option<f64>,
    /// Total quota (regular plan).
    #[serde(default)]
    usage: Option<f64>,
    /// Used amount (regular plan).
    #[serde(rename = "currentValue", default)]
    current_value: Option<f64>,
    /// Quota unit (coding plan, e.g. 3 = tokens).
    #[serde(default)]
    unit: Option<u32>,
    /// Quota number (coding plan, e.g. 5).
    #[serde(default)]
    number: Option<u32>,
    /// Next reset time as Unix millis.
    #[serde(rename = "nextResetTime", default)]
    next_reset_time: Option<u64>,
    /// Per-model usage breakdown (coding plan TIME_LIMIT).
    #[serde(rename = "usageDetails", default)]
    usage_details: Option<Vec<UsageDetail>>,
}

#[derive(Deserialize)]
struct UsageDetail {
    #[serde(rename = "modelCode")]
    model_code: Option<String>,
    usage: Option<u32>,
}

#[async_trait]
impl Provider for GlmProvider {
    async fn chat_stream(
        &self,
        request: UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, ProviderError> {
        self.openai_backend
            .chat_stream(request, allow_passthrough)
            .await
    }

    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        use tracing::debug;
        let client = reqwest::Client::new();
        let url = self.balance_url();

        debug!(url = %url, has_key = !self.config.api_key.is_empty(), "glm balance query");

        let resp = client
            .get(&url)
            .header("Authorization", &self.config.api_key)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        debug!(status = %resp.status(), "glm balance response");

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            debug!(status = %status, err = %err_text, "glm balance failed");
            return Err(ProviderError::Request(format!(
                "balance query failed {status}: {err_text}"
            )));
        }

        let body: GlmQuotaResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::Internal(e.to_string()))?;

        let Some(data) = body.data else {
            return Ok(None);
        };

        let host = self.config.endpoint.clone();
        let model = self.config.model.clone();
        let has_key = !self.config.api_key.is_empty();

        Ok(Some(build_provider_balance("glm", &data, &host, has_key, model.as_deref())))
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            tool_calling: crate::provider::ToolCallingSupport::Native,
            image_input: false,
            streaming: true,
        }
    }

    fn name(&self) -> &'static str {
        "glm"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glm_balance_builds_correct_structure() {
        let limits = vec![
            QuotaLimit {
                limit_type: Some("TOKENS_LIMIT".into()),
                percentage: Some(72.0),
                remaining: Some(28.0),
                usage: Some(100.0),
                current_value: Some(72.0),
                unit: Some(3),
                number: Some(5),
                next_reset_time: Some(1778499600000),
                usage_details: Some(vec![
                    UsageDetail { model_code: Some("glm-4".into()), usage: Some(1240) },
                    UsageDetail { model_code: Some("coder-1".into()), usage: Some(580) },
                ]),
            },
            QuotaLimit {
                limit_type: Some("TIME_LIMIT".into()),
                percentage: Some(45.0),
                remaining: None,
                usage: None,
                current_value: None,
                unit: None,
                number: None,
                next_reset_time: None,
                usage_details: None,
            },
        ];

        let data = QuotaData {
            level: Some("plus".into()),
            limits: Some(limits),
        };

        let result = build_provider_balance("glm", &data, "open.bigmodel.cn", true, Some("glm-4-plus"));

        assert_eq!(result.provider_name, "glm");
        assert_eq!(result.status, BalanceStatus::Online);
        assert_eq!(result.plan.as_deref(), Some("plus"));
        assert_eq!(result.plan_type, Some(PlanType::Coding));
        assert_eq!(result.metrics.len(), 2);
        assert_eq!(result.metrics[0].label, "Tokens");
        assert_eq!(result.metrics[0].percentage, Some(72.0));
        assert_eq!(result.metrics[0].status, MetricStatus::Ok);
        assert_eq!(result.metrics[1].label, "Time");
        assert_eq!(result.breakdown.len(), 2);
        assert_eq!(result.resets.len(), 1);
        assert!(result.config_summary.has_key);
    }

    #[test]
    fn glm_balance_alerts_on_high_usage() {
        let limits = vec![
            QuotaLimit {
                limit_type: Some("TOKENS_LIMIT".into()),
                percentage: Some(85.0),
                remaining: Some(15.0),
                usage: Some(100.0),
                current_value: Some(85.0),
                unit: Some(3),
                number: Some(5),
                next_reset_time: None,
                usage_details: None,
            },
        ];

        let data = QuotaData {
            level: Some("plus".into()),
            limits: Some(limits),
        };

        let result = build_provider_balance("glm", &data, "open.bigmodel.cn", true, None);
        assert!(result.alerts.iter().any(|a| a.message.contains("85")));
    }
}
