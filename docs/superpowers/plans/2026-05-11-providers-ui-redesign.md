# Providers UI 重设计 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor backend balance API to return unified structured JSON, then redesign the Dashboard providers area and Providers config panel with a Modern SaaS Dashboard visual style.

**Architecture:** Backend adds `ProviderBalance` struct with `metrics[]`, `breakdown[]`, `resets[]`, `alerts[]` fields. Each provider's `balance()` converts its native format into this unified shape. Frontend replaces the flat balance cards with a left-right split panel (searchable list + detail/overview) and updates the config panel with icon button actions.

**Tech Stack:** Rust (axum 0.8, serde, reqwest), vanilla HTML/CSS/JS (single-file, no build tools)

**Spec:** `docs/superpowers/specs/2026-05-10-providers-ui-redesign.md`

---

## File Structure

| Action | Path | Responsibility |
|--------|------|----------------|
| Modify | `src/provider/mod.rs` | Add `ProviderBalance`, `Metric`, `Alert`, `BreakdownItem`, `ResetSchedule`, `ConfigSummary` structs; change `balance()` return type |
| Modify | `src/provider/cloud/glm.rs` | Refactor `balance()` to return `ProviderBalance` |
| Modify | `src/provider/cloud/deepseek.rs` | Refactor `balance()` to return `ProviderBalance` |
| Modify | `src/web.rs` | Replace `BalanceEntry` with unified struct; update `balance_api_handler` |
| Modify | `ui/index.html` | Full frontend redesign: split panel, list cards, detail panel, config panel, new visual style |

---

### Task 1: Define `ProviderBalance` Structs

**Files:**
- Modify: `src/provider/mod.rs` (after line 148, before module exports)

- [ ] **Step 1: Write the failing test**

Add test module at the end of `src/provider/mod.rs`:

```rust
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
            metrics: vec![
                Metric {
                    label: "Tokens".into(),
                    kind: MetricKind::Percentage,
                    value: 72.0,
                    total: Some(100.0),
                    unit: Some("%".into()),
                    percentage: Some(72.0),
                    status: MetricStatus::Ok,
                },
            ],
            breakdown: vec![
                BreakdownItem { label: "glm-4".into(), value: 1240.0, unit: "requests".into() },
            ],
            resets: vec![
                ResetSchedule { label: "Token quota".into(), resets_at_ms: 1778499600000 },
            ],
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
        assert_eq!(serde_json::to_string(&BalanceStatus::Online).unwrap(), "\"online\"");
        assert_eq!(serde_json::to_string(&BalanceStatus::Offline).unwrap(), "\"offline\"");
        assert_eq!(serde_json::to_string(&BalanceStatus::Error).unwrap(), "\"error\"");
        assert_eq!(serde_json::to_string(&BalanceStatus::NoData).unwrap(), "\"no_data\"");
    }

    #[test]
    fn metric_status_derived_from_percentage() {
        assert_eq!(MetricStatus::from_percentage(72.0), MetricStatus::Ok);
        assert_eq!(MetricStatus::from_percentage(80.0), MetricStatus::Warn);
        assert_eq!(MetricStatus::from_percentage(95.0), MetricStatus::Critical);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test balance_struct_tests --lib 2>&1 | head -20`
Expected: FAIL — `ProviderBalance` not defined

- [ ] **Step 3: Write the structs**

Add to `src/provider/mod.rs` after the `ProviderError` enum (after line 148), before module exports:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test balance_struct_tests --lib 2>&1 | tail -10`
Expected: 4 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/provider/mod.rs
git commit -m "feat(balance): add unified ProviderBalance structs"
```

---

### Task 2: Refactor `Provider::balance()` Return Type

**Files:**
- Modify: `src/provider/mod.rs:113` — change trait method signature

- [ ] **Step 1: Change the trait method signature**

Replace the `balance()` default implementation in `src/provider/mod.rs` (line 110-115):

```rust
    /// Query the provider account balance as structured data.
    ///
    /// Default: `Ok(None)` — most local providers won't implement this.
    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
        Ok(None)
    }
```

- [ ] **Step 2: Update GLM provider signature**

In `src/provider/cloud/glm.rs`, change the `balance()` method signature:

```rust
    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
```

And add the import at the top of the file:
```rust
use crate::provider::{Provider, ProviderBalance, ProviderError, UnifiedRequest,
    BalanceStatus, PlanType, Metric, MetricKind, MetricStatus,
    Alert, AlertLevel, BreakdownItem, ResetSchedule, ConfigSummary};
```

- [ ] **Step 3: Update DeepSeek provider signature**

In `src/provider/cloud/deepseek.rs`, change the `balance()` method signature:

```rust
    async fn balance(&self) -> Result<Option<ProviderBalance>, ProviderError> {
```

And add the import:
```rust
use crate::provider::{Provider, ProviderBalance, ProviderError, UnifiedRequest,
    BalanceStatus, PlanType, Metric, MetricKind, MetricStatus,
    Alert, AlertLevel, BreakdownItem, ResetSchedule, ConfigSummary};
```

- [ ] **Step 4: Verify compilation fails on method bodies (expected)**

Run: `cargo check 2>&1 | head -20`
Expected: Type mismatch errors in `glm.rs` and `deepseek.rs` bodies

- [ ] **Step 5: Commit**

```bash
git add src/provider/mod.rs src/provider/cloud/glm.rs src/provider/cloud/deepseek.rs
git commit -m "refactor(balance): change trait signature to return ProviderBalance"
```

---

### Task 3: Refactor GLM `balance()` to Return `ProviderBalance`

**Files:**
- Modify: `src/provider/cloud/glm.rs:120-210` — rewrite `balance()` body

- [ ] **Step 1: Write the failing test**

Add to `src/provider/cloud/glm.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test glm --lib 2>&1 | head -20`
Expected: FAIL — `build_provider_balance` not defined

- [ ] **Step 3: Extract `build_provider_balance` helper and rewrite `balance()` body**

Replace the `balance()` method in glm.rs. Extract a pure function:

```rust
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
```

Add the pure helper function:

```rust
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
            let level = if pct >= 95.0 { AlertLevel::Critical } else { AlertLevel::Warn };
            alerts.push(Alert {
                level,
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test glm --lib 2>&1 | tail -10`
Expected: 2 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/provider/cloud/glm.rs
git commit -m "feat(glm): refactor balance() to return unified ProviderBalance"
```

---

### Task 4: Refactor DeepSeek `balance()` to Return `ProviderBalance`

**Files:**
- Modify: `src/provider/cloud/deepseek.rs:79-138` — rewrite `balance()` body

- [ ] **Step 1: Write the failing test**

Add to `src/provider/cloud/deepseek.rs`:

```rust
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
        assert!(result.alerts.iter().any(|a| a.level == AlertLevel::Critical));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test deepseek --lib 2>&1 | head -20`
Expected: FAIL — `build_balance_from_credit` not defined

- [ ] **Step 3: Add helper and rewrite `balance()` body**

```rust
fn build_balance_from_credit(
    name: &str,
    balance: f64,
    currency: &str,
    endpoint: &str,
    has_key: bool,
) -> ProviderBalance {
    let status = if balance <= 0.0 { MetricStatus::Critical } else { MetricStatus::Ok };

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
```

Rewrite `balance()`:

```rust
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
                "deepseek", balance, &info.currency, &endpoint, has_key,
            )));
        }

        Ok(None)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test deepseek --lib 2>&1 | tail -10`
Expected: 2 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/provider/cloud/deepseek.rs
git commit -m "feat(deepseek): refactor balance() to return unified ProviderBalance"
```

---

### Task 5: Update `web.rs` Balance API Response

**Files:**
- Modify: `src/web.rs:96-109` — replace `BalanceEntry`/`BalanceResponse`
- Modify: `src/web.rs:492-539` — update `balance_api_handler`

- [ ] **Step 1: Write the failing test**

Add to `src/web.rs` tests:

```rust
    #[test]
    fn unified_balance_entry_serializes() {
        use crate::provider::{ProviderBalance, BalanceStatus, Metric, MetricKind, MetricStatus, ConfigSummary};
        let entry = UnifiedBalanceEntry {
            name: "glm".into(),
            provider_type: "cloud".into(),
            endpoint: "open.bigmodel.cn".into(),
            has_key: true,
            balance: Some(ProviderBalance {
                provider_name: "glm".into(),
                status: BalanceStatus::Online,
                plan: Some("plus".into()),
                plan_type: None,
                alerts: vec![],
                metrics: vec![Metric {
                    label: "Tokens".into(),
                    kind: MetricKind::Percentage,
                    value: 72.0,
                    total: Some(100.0),
                    unit: Some("%".into()),
                    percentage: Some(72.0),
                    status: MetricStatus::Ok,
                }],
                breakdown: vec![],
                resets: vec![],
                config_summary: ConfigSummary {
                    provider_type: "cloud".into(),
                    endpoint: "open.bigmodel.cn".into(),
                    has_key: true,
                    model: None,
                },
            }),
            error: None,
        };

        let resp = BalanceResponse { balances: vec![entry] };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"provider_name\":\"glm\""));
        assert!(json.contains("\"metrics\""));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test unified_balance_entry_serializes --lib 2>&1 | head -20`
Expected: FAIL — `UnifiedBalanceEntry` not defined

- [ ] **Step 3: Replace `BalanceEntry` and `BalanceResponse`**

Replace the structs at `src/web.rs:96-109`:

```rust
use crate::provider::{ProviderBalance, BalanceStatus, PlanType, Metric, MetricKind, MetricStatus, Alert, AlertLevel, BreakdownItem, ResetSchedule, ConfigSummary};

#[derive(Serialize)]
pub struct UnifiedBalanceEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: String,
    pub endpoint: String,
    pub has_key: bool,
    pub balance: Option<ProviderBalance>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct BalanceResponse {
    pub balances: Vec<UnifiedBalanceEntry>,
}
```

- [ ] **Step 4: Update `balance_api_handler`**

Replace the handler at `src/web.rs:492-539`. Change all `BalanceEntry` references to `UnifiedBalanceEntry`. The logic stays the same — just the type name changes and `balance` field is now `Option<ProviderBalance>` instead of `Option<String>`.

- [ ] **Step 5: Fix all existing tests referencing `BalanceEntry`**

Search for `BalanceEntry` in the test module and replace with `UnifiedBalanceEntry`. The `providers_response_serializes` test uses `ProviderInfo` which is unchanged.

- [ ] **Step 6: Run all tests**

Run: `cargo test --lib 2>&1 | tail -20`
Expected: All tests PASS

- [ ] **Step 7: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): update balance API to serve unified ProviderBalance"
```

---

### Task 6: Frontend — New CSS Foundation

**Files:**
- Modify: `ui/index.html` — CSS section

- [ ] **Step 1: Add new CSS for providers redesign**

Add after existing CSS variables block, before component styles. This covers:
- `.providers-panel` — flex container for split layout
- `.providers-list` / `.providers-detail` — left/right panels
- `.provider-item` — two-row compact cards
- `.detail-*` — detail panel sections (metrics, alerts, breakdown, config)
- `.config-provider-card` — config panel cards with icon buttons
- `.brand-*` — provider brand gradient colors
- `.p-status` / `.p-pill` / `.p-mini-bar` — status dots, labels, mini progress bars

See the spec at `docs/superpowers/specs/2026-05-10-providers-ui-redesign.md` sections II and III for exact visual requirements.

- [ ] **Step 2: Verify UI loads without console errors**

Run: `cargo build && cargo run`, open `http://localhost:8800`, check for CSS parsing errors.

- [ ] **Step 3: Commit**

```bash
git add ui/index.html
git commit -m "style(ui): add providers redesign CSS foundation"
```

---

### Task 7: Frontend — Replace Dashboard Balance with Split Panel

**Files:**
- Modify: `ui/index.html` — replace `renderDashboardBalance()`, `renderTopbarBalance()`

- [ ] **Step 1: Add `selectedProvider` and `providerSearch` to state object**

Find the `state` object definition and add:
```javascript
selectedProvider: null,
providerSearch: '',
```

- [ ] **Step 2: Replace `renderDashboardBalance()` with split panel renderer**

New implementation:
- Renders `.providers-panel` with `.providers-list` (left) and `.providers-detail` (right)
- List items are `.provider-item` two-row cards showing status dot, name, pills, mini progress bar
- Right panel shows `renderProviderOverview()` when nothing selected, `renderProviderDetail()` when a provider is selected
- Handles search filtering
- Attaches click handlers for selection

- [ ] **Step 3: Add helper functions**

- `renderProviderOverview()` — aggregate stats (online/warning/offline counts) + alerts list
- `renderProviderDetail(bal)` — metric cards, alerts, breakdown table, resets, config summary
- `getStatusClass(b)`, `getPillClass(name)`, `getBrandClass(name)`, `getPlanTypeClass(pt)` — CSS class helpers
- `formatResetTime(ms)` — countdown formatting
- `filterProviderList()` — search filter

- [ ] **Step 4: Update `renderTopbarBalance()` for new data format**

- [ ] **Step 5: Remove old `renderGlmBalanceCard()` and `startCountdown()` functions**

- [ ] **Step 6: Verify in browser**

Run: `cargo build && cargo run`, check Dashboard split panel, provider selection, search, overview.

- [ ] **Step 7: Commit**

```bash
git add ui/index.html
git commit -m "feat(ui): replace dashboard balance cards with split panel layout"
```

---

### Task 8: Frontend — Update Providers Config Panel

**Files:**
- Modify: `ui/index.html` — `renderProviders()` function

- [ ] **Step 1: Rewrite `renderProviders()` with icon button cards**

- Cards use `.config-provider-card` with brand icon + name + type pill
- Row 2 shows endpoint, model, key mask
- Top-right: `.config-action-btn` icons for Edit, Test, Delete
- Delete button has `.delete` class with red border
- Offline providers get `.offline` class with reduced opacity
- Attach click handlers: edit → `openProviderModal()`, delete → `deleteProvider()`, test → new `testProviderConnection()`

- [ ] **Step 2: Verify in browser**

Open Providers tab, confirm cards and action buttons.

- [ ] **Step 3: Commit**

```bash
git add ui/index.html
git commit -m "feat(ui): redesign providers config panel with icon button cards"
```

---

### Task 9: Remove Old Balance CSS and Cleanup

**Files:**
- Modify: `ui/index.html` — remove old `.balance-card` CSS and HTML

- [ ] **Step 1: Remove old `.balance-card` CSS rules** (lines 490-568)

- [ ] **Step 2: Update dashboard-balance-entries placeholder HTML**

- [ ] **Step 3: Run `cargo test --lib`**

Expected: All PASS

- [ ] **Step 4: Commit**

```bash
git add ui/index.html
git commit -m "chore(ui): remove legacy balance-card CSS and HTML"
```

---

### Task 10: Final Verification and Polish

- [ ] **Step 1: Run `cargo test`**

- [ ] **Step 2: Run `cargo clippy --all-targets`**

- [ ] **Step 3: Visual verification in browser**

Check all scenarios from the spec:
1. Dashboard split panel with provider list + overview
2. GLM provider detail: metrics, breakdown, resets, config
3. DeepSeek provider detail: credit balance
4. Offline provider: error state
5. Search filters providers
6. Providers tab: brand icon cards, icon buttons
7. Topbar status dots
8. No console errors

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "chore: final cleanup for providers UI redesign"
```
