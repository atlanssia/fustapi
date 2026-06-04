//! Health-check parsing for provider balance endpoints.
//!
//! Pure functions that parse JSON responses from various provider health
//! APIs into structured `ProviderBalance` data. Extracted from provider
//! adapters for testability and reuse.

use crate::provider::{
    Alert, AlertLevel, BalanceStatus, BreakdownItem, ConfigSummary, Metric, MetricKind,
    MetricStatus, ProviderBalance,
};
use serde::Deserialize;

// ── omlx health parsing ─────────────────────────────────────────────

#[derive(Deserialize)]
struct OmlxHealthResponse {
    status: String,
    default_model: Option<String>,
    engine_pool: EnginePool,
}

#[derive(Deserialize)]
struct EnginePool {
    model_count: u32,
    loaded_count: u32,
    #[serde(rename = "final_ceiling")]
    max_model_memory: u64,
    current_model_memory: u64,
}

/// Parse an omlx `/health` JSON response into a `ProviderBalance`.
///
/// This is a pure function extracted from `OmlxProvider::balance()` for testability.
/// The `endpoint` and `model` params come from provider config.
pub fn parse_omlx_balance(
    body: &str,
    endpoint: &str,
    model: Option<&str>,
) -> Result<ProviderBalance, String> {
    let resp: OmlxHealthResponse =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;

    let is_healthy = matches!(
        resp.status.as_str(),
        "healthy" | "ok" | "running" | "up" | "ready"
    );
    let pool = &resp.engine_pool;

    let mem_pct = if pool.max_model_memory > 0 {
        (pool.current_model_memory as f64 / pool.max_model_memory as f64 * 10000.0).round() / 100.0
    } else {
        0.0
    };

    let metrics = vec![
        Metric {
            label: "Models".to_string(),
            kind: MetricKind::Absolute,
            value: pool.model_count as f64,
            total: None,
            unit: Some("available".to_string()),
            percentage: None,
            status: MetricStatus::Ok,
            reset_at_ms: None,
        },
        Metric {
            label: "Loaded".to_string(),
            kind: MetricKind::Absolute,
            value: pool.loaded_count as f64,
            total: Some(pool.model_count as f64),
            unit: Some("models".to_string()),
            percentage: None,
            status: if pool.loaded_count == 0 && pool.model_count > 0 {
                MetricStatus::Warn
            } else {
                MetricStatus::Ok
            },
            reset_at_ms: None,
        },
        Metric {
            label: "VRAM".to_string(),
            kind: MetricKind::Percentage,
            value: mem_pct,
            total: Some(100.0),
            unit: Some("%".to_string()),
            percentage: Some(mem_pct),
            status: MetricStatus::from_percentage(mem_pct),
            reset_at_ms: None,
        },
    ];

    let mut alerts = Vec::new();
    if mem_pct >= 95.0 {
        alerts.push(Alert {
            level: AlertLevel::Critical,
            message: format!("VRAM usage {mem_pct:.0}% — cannot load more models"),
        });
    } else if mem_pct >= 80.0 {
        alerts.push(Alert {
            level: AlertLevel::Warn,
            message: format!("VRAM usage {mem_pct:.0}% — approaching limit"),
        });
    }
    if !is_healthy {
        alerts.push(Alert {
            level: AlertLevel::Critical,
            message: "Engine reports unhealthy status".to_string(),
        });
    }

    Ok(ProviderBalance {
        provider_name: "oMLX".to_string(),
        status: if is_healthy {
            BalanceStatus::Online
        } else {
            BalanceStatus::Error
        },
        plan: resp.default_model.clone(),
        plan_type: None,
        alerts,
        metrics,
        breakdown: vec![],
        resets: vec![],
        config_summary: ConfigSummary {
            provider_type: "local".to_string(),
            endpoint: endpoint.to_string(),
            has_key: false,
            model: model.map(String::from).or(resp.default_model),
        },
    })
}

// ── deepseek balance parsing ────────────────────────────────────────

#[derive(Deserialize)]
struct DeepSeekBalanceResponse {
    is_available: bool,
    balance_infos: Vec<DeepSeekBalanceInfo>,
}

#[derive(Deserialize)]
struct DeepSeekBalanceInfo {
    total_balance: String,
    #[allow(dead_code)]
    currency: String,
}

/// Parse a DeepSeek `/user/balance` JSON response into a `ProviderBalance`.
pub fn parse_deepseek_balance(
    body: &str,
    endpoint: &str,
    has_key: bool,
) -> Result<ProviderBalance, String> {
    let resp: DeepSeekBalanceResponse =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;

    let balance = resp
        .balance_infos
        .first()
        .map(|info| info.total_balance.parse::<f64>().unwrap_or(0.0))
        .unwrap_or(0.0);

    let currency = resp
        .balance_infos
        .first()
        .map(|info| info.currency.clone())
        .unwrap_or_else(|| "CNY".to_string());

    let balance_status = if balance <= 0.0 {
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

    Ok(ProviderBalance {
        provider_name: "DeepSeek".to_string(),
        status: BalanceStatus::Online,
        plan: None,
        plan_type: Some(crate::provider::PlanType::Credit),
        alerts,
        metrics: vec![Metric {
            label: "Balance".to_string(),
            kind: MetricKind::Absolute,
            value: balance,
            total: None,
            unit: Some(currency),
            percentage: None,
            status: balance_status,
            reset_at_ms: None,
        }],
        breakdown: vec![],
        resets: vec![],
        config_summary: ConfigSummary {
            provider_type: "cloud".to_string(),
            endpoint: endpoint.to_string(),
            has_key,
            model: None,
        },
    })
}

// ── GLM quota parsing ───────────────────────────────────────────────

#[derive(Deserialize)]
struct GlmQuotaResponse {
    data: Option<GlmQuotaData>,
}

#[derive(Deserialize)]
struct GlmQuotaData {
    level: String,
    limits: Vec<QuotaLimit>,
}

#[derive(Deserialize)]
struct QuotaLimit {
    #[serde(rename = "type")]
    limit_type: String,
    percentage: f64,
    #[serde(default)]
    remaining: f64,
    #[serde(default)]
    usage: f64,
    #[serde(rename = "nextResetTime", default)]
    next_reset_time: Option<u64>,
    #[serde(rename = "usageDetails", default)]
    usage_details: Option<Vec<UsageDetail>>,
}

#[derive(Deserialize)]
struct UsageDetail {
    #[serde(rename = "modelCode")]
    model_code: String,
    usage: f64,
}

/// Parse a GLM `/api/monitor/usage/quota/limit` JSON response into a `ProviderBalance`.
pub fn parse_glm_balance(
    body: &str,
    endpoint: &str,
    has_key: bool,
    model: Option<&str>,
) -> Result<Option<ProviderBalance>, String> {
    let resp: GlmQuotaResponse =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;

    let data = match resp.data {
        Some(d) => d,
        None => return Ok(None),
    };

    let plan_type = if endpoint.contains("/coding/") {
        crate::provider::PlanType::Coding
    } else {
        crate::provider::PlanType::Token
    };

    let mut metrics = Vec::new();
    let mut alerts = Vec::new();
    let mut breakdown = Vec::new();
    let mut resets = Vec::new();

    for limit in &data.limits {
        let label = match limit.limit_type.as_str() {
            "TOKENS_LIMIT" => "Tokens",
            "TIME_LIMIT" => "MCP",
            _ => "Usage",
        };
        let pct = limit.percentage;
        metrics.push(Metric {
            label: label.to_string(),
            kind: MetricKind::Percentage,
            value: pct,
            total: Some(100.0),
            unit: Some("%".to_string()),
            percentage: Some(pct),
            status: MetricStatus::from_percentage(pct),
            reset_at_ms: limit.next_reset_time,
        });

        if pct >= 95.0 {
            alerts.push(Alert {
                level: AlertLevel::Critical,
                message: format!("{label} quota {pct:.0}% used"),
            });
        } else if pct >= 80.0 {
            alerts.push(Alert {
                level: AlertLevel::Warn,
                message: format!("{label} quota {pct:.0}% used"),
            });
        }

        if let Some(details) = &limit.usage_details {
            for detail in details {
                breakdown.push(BreakdownItem {
                    label: detail.model_code.clone(),
                    value: detail.usage,
                    unit: "requests".to_string(),
                });
            }
        }

        if let Some(reset_time) = limit.next_reset_time {
            resets.push(crate::provider::ResetSchedule {
                label: format!("{label} quota"),
                resets_at_ms: reset_time,
            });
        }
    }

    Ok(Some(ProviderBalance {
        provider_name: "GLM".to_string(),
        status: BalanceStatus::Online,
        plan: Some(data.level.clone()),
        plan_type: Some(plan_type),
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
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── omlx parse tests ─────────────────────────────────────────────

    #[test]
    fn omlx_healthy_response() {
        let body = r#"{"status":"healthy","default_model":"qwen3","engine_pool":{"model_count":5,"loaded_count":2,"final_ceiling":17179869184,"current_model_memory":8589934592}}"#;
        let balance = parse_omlx_balance(body, "http://localhost:8000/v1", None)
            .expect("parse should succeed");

        assert_eq!(balance.status, BalanceStatus::Online);
        assert_eq!(balance.plan, Some("qwen3".to_string()));
        assert_eq!(balance.metrics.len(), 3);
        assert_eq!(balance.metrics[0].label, "Models");
        assert_eq!(balance.metrics[0].value, 5.0);
        assert_eq!(balance.metrics[1].label, "Loaded");
        assert_eq!(balance.metrics[1].value, 2.0);
        assert_eq!(balance.metrics[2].label, "VRAM");
        assert!((balance.metrics[2].value - 50.0).abs() < 0.01);
        assert!(balance.alerts.is_empty());
    }

    #[test]
    fn omlx_unhealthy_status() {
        let body = r#"{"status":"error","default_model":null,"engine_pool":{"model_count":0,"loaded_count":0,"final_ceiling":0,"current_model_memory":0}}"#;
        let balance = parse_omlx_balance(body, "http://localhost:8000/v1", Some("test"))
            .expect("parse should succeed");

        assert_eq!(balance.status, BalanceStatus::Error);
        assert!(
            balance
                .alerts
                .iter()
                .any(|a| a.message.contains("unhealthy"))
        );
    }

    #[test]
    fn omlx_high_vram_triggers_alert() {
        let body = r#"{"status":"healthy","default_model":"m","engine_pool":{"model_count":1,"loaded_count":1,"final_ceiling":100,"current_model_memory":85}}"#;
        let balance = parse_omlx_balance(body, "http://localhost:8000/v1", None)
            .expect("parse should succeed");

        assert!(balance.alerts.iter().any(|a| a.message.contains("85%")));
    }

    #[test]
    fn omlx_critical_vram_triggers_alert() {
        let body = r#"{"status":"healthy","default_model":"m","engine_pool":{"model_count":1,"loaded_count":1,"final_ceiling":100,"current_model_memory":96}}"#;
        let balance = parse_omlx_balance(body, "http://localhost:8000/v1", None)
            .expect("parse should succeed");

        assert!(
            balance
                .alerts
                .iter()
                .any(|a| a.level == AlertLevel::Critical && a.message.contains("96%"))
        );
    }

    #[test]
    fn omlx_no_models_loaded_warns() {
        let body = r#"{"status":"healthy","default_model":null,"engine_pool":{"model_count":5,"loaded_count":0,"final_ceiling":1000,"current_model_memory":0}}"#;
        let balance = parse_omlx_balance(body, "http://localhost:8000/v1", None)
            .expect("parse should succeed");

        assert_eq!(balance.metrics[1].status, MetricStatus::Warn);
    }

    #[test]
    fn omlx_invalid_json_returns_error() {
        let result = parse_omlx_balance("not json", "http://localhost:8000/v1", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid JSON"));
    }

    // ── deepseek parse tests ─────────────────────────────────────────

    #[test]
    fn deepseek_normal_balance() {
        let body =
            r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"1.60"}]}"#;
        let balance = parse_deepseek_balance(body, "https://api.deepseek.com", true)
            .expect("parse should succeed");

        assert_eq!(balance.status, BalanceStatus::Online);
        assert_eq!(balance.plan_type, Some(crate::provider::PlanType::Credit));
        assert_eq!(balance.metrics.len(), 1);
        assert!((balance.metrics[0].value - 1.60).abs() < 0.01);
        assert_eq!(balance.metrics[0].unit, Some("CNY".to_string()));
        assert!(balance.alerts.is_empty());
    }

    #[test]
    fn deepseek_zero_balance_alerts() {
        let body =
            r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"0.00"}]}"#;
        let balance = parse_deepseek_balance(body, "https://api.deepseek.com", true)
            .expect("parse should succeed");

        assert_eq!(balance.metrics[0].status, MetricStatus::Critical);
        assert!(
            balance
                .alerts
                .iter()
                .any(|a| a.message.contains("depleted"))
        );
    }

    #[test]
    fn deepseek_empty_balance_infos() {
        let body = r#"{"is_available":true,"balance_infos":[]}"#;
        let balance = parse_deepseek_balance(body, "https://api.deepseek.com", true)
            .expect("parse should succeed");

        assert_eq!(balance.metrics[0].value, 0.0);
    }

    #[test]
    fn deepseek_invalid_json_returns_error() {
        let result = parse_deepseek_balance("{}", "https://api.deepseek.com", true);
        assert!(result.is_err());
    }

    // ── GLM parse tests ──────────────────────────────────────────────

    #[test]
    fn glm_normal_quota() {
        let body = r#"{"code":200,"success":true,"data":{"level":"plus","limits":[{"type":"TOKENS_LIMIT","percentage":72.0,"remaining":28.0,"usage":100.0,"currentValue":72.0,"unit":3,"number":5,"nextResetTime":1778499600000,"usageDetails":[{"modelCode":"glm-4","usage":1240}]}]}}"#;
        let balance = parse_glm_balance(
            body,
            "https://open.bigmodel.cn/api/coding/paas/v4",
            true,
            Some("glm-4-plus"),
        )
        .expect("parse should succeed")
        .expect("should return Some");

        assert_eq!(balance.status, BalanceStatus::Online);
        assert_eq!(balance.plan, Some("plus".to_string()));
        assert_eq!(balance.plan_type, Some(crate::provider::PlanType::Coding));
        assert_eq!(balance.metrics.len(), 1);
        assert_eq!(balance.metrics[0].label, "Tokens");
        assert!((balance.metrics[0].value - 72.0).abs() < 0.01);
        assert_eq!(balance.breakdown.len(), 1);
        assert_eq!(balance.breakdown[0].label, "glm-4");
        assert_eq!(balance.resets.len(), 1);
    }

    #[test]
    fn glm_no_data_returns_none() {
        let body = r#"{"code":200,"success":true}"#;
        let result = parse_glm_balance(body, "https://open.bigmodel.cn/api/paas/v4", true, None)
            .expect("parse should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn glm_high_quota_triggers_warn() {
        let body = r#"{"data":{"level":"free","limits":[{"type":"TOKENS_LIMIT","percentage":82.0,"remaining":18.0,"usage":82.0,"currentValue":82.0}]}}"#;
        let balance = parse_glm_balance(body, "https://open.bigmodel.cn/api/paas/v4", true, None)
            .expect("parse should succeed")
            .expect("should return Some");

        assert!(balance.alerts.iter().any(|a| a.level == AlertLevel::Warn));
    }

    #[test]
    fn glm_invalid_json_returns_error() {
        let result = parse_glm_balance("bad", "https://open.bigmodel.cn", true, None);
        assert!(result.is_err());
    }
}
