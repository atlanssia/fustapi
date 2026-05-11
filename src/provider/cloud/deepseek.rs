//! `DeepSeek` cloud provider adapter.
//!
//! Fallback adapter for the `DeepSeek` API.

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{
    Alert, AlertLevel, BalanceStatus, ConfigSummary, Metric, MetricKind, MetricStatus, PlanType,
    Provider, ProviderBalance, ProviderError, UnifiedRequest,
};

/// `DeepSeek` provider configuration.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DeepSeekConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: Option<String>,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.deepseek.com".to_string(),
            api_key: String::new(),
            model: None,
        }
    }
}

/// `DeepSeek` provider implementation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DeepSeekProvider {
    config: DeepSeekConfig,
    openai_backend: crate::provider::cloud::openai::OpenAIProvider,
}

impl DeepSeekProvider {
    #[must_use]
    pub fn new(config: DeepSeekConfig) -> Self {
        let openai_backend = crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: config.endpoint.clone(),
                api_key: config.api_key.clone(),
                model: config.model.clone(),
                stream_options: true,
            },
        );
        Self {
            config,
            openai_backend,
        }
    }
}

fn build_balance_from_credit(
    name: &str,
    balance: f64,
    currency: &str,
    endpoint: &str,
    has_key: bool,
) -> ProviderBalance {
    let status = if balance <= 0.0 {
        MetricStatus::Critical
    } else {
        MetricStatus::Ok
    };

    let mut alerts = Vec::new();
    if balance <= 0.0 {
        alerts.push(Alert {
            level: AlertLevel::Critical,
            message: "Balance depleted".to_string(),
        });
    }

    ProviderBalance {
        provider_name: name.to_string(),
        status: BalanceStatus::Online,
        plan: None,
        plan_type: Some(PlanType::Credit),
        alerts,
        metrics: vec![Metric {
            label: "Balance".to_string(),
            kind: MetricKind::Absolute,
            value: balance,
            total: None,
            unit: Some(currency.to_string()),
            percentage: None,
            status,
        }],
        breakdown: vec![],
        resets: vec![],
        config_summary: ConfigSummary {
            provider_type: "cloud".to_string(),
            endpoint: endpoint.to_string(),
            has_key,
            model: None,
        },
    }
}

#[derive(Deserialize)]
struct DeepSeekBalanceResponse {
    is_available: bool,
    balance_infos: Vec<BalanceInfo>,
}

#[derive(Deserialize)]
struct BalanceInfo {
    currency: String,
    total_balance: String,
}

#[async_trait]
impl Provider for DeepSeekProvider {
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
        let url = format!(
            "{}/user/balance",
            self.config.endpoint.trim_end_matches('/')
        );

        debug!(url = %url, has_key = !self.config.api_key.is_empty(), "deepseek balance query");

        let mut builder = client.get(&url).header("Accept", "application/json");

        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;

        debug!(status = %resp.status(), "deepseek balance response");

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            debug!(status = %status, err = %err_text, "deepseek balance failed");
            return Err(ProviderError::Request(format!(
                "balance query failed {status}: {err_text}"
            )));
        }

        let resp_text = resp.text().await.unwrap_or_default();
        debug!(body = %resp_text, "deepseek balance raw response");

        let body: DeepSeekBalanceResponse =
            serde_json::from_str(&resp_text).map_err(|e| ProviderError::Internal(e.to_string()))?;

        debug!(
            is_available = body.is_available,
            infos = body.balance_infos.len(),
            "deepseek balance parsed"
        );

        if body.is_available
            && let Some(info) = body.balance_infos.first()
        {
            let balance = info.total_balance.parse::<f64>().unwrap_or(0.0);
            let has_key = !self.config.api_key.is_empty();
            let endpoint = self.config.endpoint.clone();

            return Ok(Some(build_balance_from_credit(
                "deepseek",
                balance,
                &info.currency,
                &endpoint,
                has_key,
            )));
        }

        Ok(None)
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            tool_calling: crate::provider::ToolCallingSupport::Native,
            image_input: false,
            streaming: true,
        }
    }

    fn name(&self) -> &'static str {
        "deepseek"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_balance_builds_credit_structure() {
        let result = build_balance_from_credit("deepseek", 1.60, "CNY", "api.deepseek.com", true);

        assert_eq!(result.provider_name, "deepseek");
        assert_eq!(result.status, BalanceStatus::Online);
        assert_eq!(result.plan_type, Some(PlanType::Credit));
        assert_eq!(result.metrics.len(), 1);
        assert_eq!(result.metrics[0].label, "Balance");
        assert_eq!(result.metrics[0].kind, MetricKind::Absolute);
        assert_eq!(result.metrics[0].value, 1.60);
        assert_eq!(result.metrics[0].unit.as_deref(), Some("CNY"));
        assert!(result.alerts.is_empty());
    }

    #[test]
    fn deepseek_balance_alerts_on_zero() {
        let result = build_balance_from_credit("deepseek", 0.0, "CNY", "api.deepseek.com", true);
        assert_eq!(result.metrics[0].status, MetricStatus::Critical);
        assert!(
            result
                .alerts
                .iter()
                .any(|a| a.level == AlertLevel::Critical)
        );
    }
}
