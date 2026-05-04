//! In-memory metrics snapshot for the dashboard.
//!
//! The snapshot is stored in `ArcSwap` and updated periodically by the aggregator.
//! Dashboard API reads from snapshot — no DB access, no locking.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::counters::{GlobalSnapshot, ProviderCounters};

/// Maximum number of timeseries points retained.
const MAX_TIMESERIES_POINTS: usize = 300;

/// Sliding window size for QPS calculation (seconds).
const QPS_WINDOW_SECS: u64 = 10;

/// A single timeseries data point.
#[derive(Debug, Clone, Serialize)]
pub struct TimeseriesPoint {
    /// Unix timestamp in seconds.
    pub timestamp: u64,
    /// Requests per second at this point.
    pub qps: f64,
    /// Average latency in milliseconds at this point.
    pub avg_latency_ms: f64,
    /// Total requests counted at this point.
    pub total_requests: u64,
    /// Error count at this point.
    pub error_count: u64,
}

/// Per-provider stats for the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderStats {
    pub name: String,
    pub request_count: u64,
    pub failure_count: u64,
    pub fallback_count: u64,
    pub avg_latency_ms: f64,
    pub success_rate: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub avg_ttft_ms: f64,
    pub avg_gen_tokens_per_sec: f64,
}

/// Complete metrics snapshot served to the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub qps: f64,
    pub avg_latency_ms: f64,
    pub total_requests: u64,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub in_flight: i64,
    pub success_rate: f64,
    pub uptime_secs: u64,
    pub provider_stats: Vec<ProviderStats>,
    pub timeseries: Vec<TimeseriesPoint>,
}

impl Default for MetricsSnapshot {
    fn default() -> Self {
        Self {
            qps: 0.0,
            avg_latency_ms: 0.0,
            total_requests: 0,
            success_requests: 0,
            failed_requests: 0,
            in_flight: 0,
            success_rate: 0.0,
            uptime_secs: 0,
            provider_stats: Vec::new(),
            timeseries: Vec::new(),
        }
    }
}

/// Builds and updates MetricsSnapshot from raw counters.
pub struct SnapshotBuilder {
    start_time: u64,
    timeseries: Vec<TimeseriesPoint>,
    /// Recent request counts for QPS sliding window.
    recent_totals: Vec<(u64, u64)>,
}

impl SnapshotBuilder {
    pub fn new() -> Self {
        Self {
            start_time: now_secs(),
            timeseries: Vec::new(),
            recent_totals: Vec::new(),
        }
    }

    /// Build a new snapshot from current counter values.
    pub fn build(
        &mut self,
        global: &GlobalSnapshot,
        provider_map: &HashMap<String, ProviderCounters>,
    ) -> MetricsSnapshot {
        let now = now_secs();
        let uptime_secs = now.saturating_sub(self.start_time);

        // Update sliding window for QPS
        self.recent_totals.push((now, global.total_requests));
        let cutoff = now.saturating_sub(QPS_WINDOW_SECS);
        self.recent_totals.retain(|(ts, _)| *ts >= cutoff);

        let qps = if self.recent_totals.len() >= 2 {
            let first = &self.recent_totals[0];
            let last = &self.recent_totals[self.recent_totals.len() - 1];
            let dt = last.0.saturating_sub(first.0);
            let dr = last.1.saturating_sub(first.1);
            if dt > 0 {
                dr as f64 / dt as f64
            } else {
                0.0
            }
        } else {
            0.0
        };

        // Calculate total avg latency from provider stats
        let (total_latency, total_counted) = provider_map.values().fold((0u64, 0u64), |(lat, cnt), p| {
            (lat + p.total_latency_ms, cnt + p.request_count)
        });
        let avg_latency_ms = if total_counted > 0 {
            total_latency as f64 / total_counted as f64
        } else {
            0.0
        };

        let success_rate = if global.total_requests > 0 {
            global.success_requests as f64 / global.total_requests as f64 * 100.0
        } else {
            0.0
        };

        // Build provider stats
        let mut provider_stats: Vec<ProviderStats> = provider_map
            .iter()
            .map(|(name, c)| {
                let psr = if c.request_count > 0 {
                    (c.request_count - c.failure_count) as f64 / c.request_count as f64 * 100.0
                } else {
                    0.0
                };
                let pavg = if c.request_count > 0 {
                    c.total_latency_ms as f64 / c.request_count as f64
                } else {
                    0.0
                };
                let avg_ttft_ms = if c.ttft_samples > 0 {
                    c.total_ttft_ms as f64 / c.ttft_samples as f64
                } else {
                    0.0
                };
                let avg_gen_tokens_per_sec = if c.total_generation_time_ms > 0 {
                    (c.generation_tokens as f64 / c.total_generation_time_ms as f64) * 1000.0
                } else {
                    0.0
                };
                ProviderStats {
                    name: name.clone(),
                    request_count: c.request_count,
                    failure_count: c.failure_count,
                    fallback_count: c.fallback_count,
                    avg_latency_ms: pavg,
                    success_rate: psr,
                    prompt_tokens: c.prompt_tokens,
                    completion_tokens: c.completion_tokens,
                    avg_ttft_ms,
                    avg_gen_tokens_per_sec,
                }
            })
            .collect();
        provider_stats.sort_by(|a, b| b.request_count.cmp(&a.request_count));

        // Add timeseries point
        let point = TimeseriesPoint {
            timestamp: now,
            qps,
            avg_latency_ms,
            total_requests: global.total_requests,
            error_count: global.failed_requests,
        };
        self.timeseries.push(point);
        if self.timeseries.len() > MAX_TIMESERIES_POINTS {
            let drain_count = self.timeseries.len() - MAX_TIMESERIES_POINTS;
            self.timeseries.drain(..drain_count);
        }

        MetricsSnapshot {
            qps,
            avg_latency_ms,
            total_requests: global.total_requests,
            success_requests: global.success_requests,
            failed_requests: global.failed_requests,
            in_flight: global.in_flight_requests,
            success_rate,
            uptime_secs,
            provider_stats,
            timeseries: self.timeseries.clone(),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_builder_empty() {
        let mut builder = SnapshotBuilder::new();
        let global = GlobalSnapshot {
            total_requests: 0,
            success_requests: 0,
            failed_requests: 0,
            in_flight_requests: 0,
        };
        let snap = builder.build(&global, &HashMap::new());
        assert_eq!(snap.total_requests, 0);
        assert_eq!(snap.qps, 0.0);
        assert_eq!(snap.success_rate, 0.0);
        assert!(snap.provider_stats.is_empty());
    }

    #[test]
    fn test_snapshot_builder_with_data() {
        let mut builder = SnapshotBuilder::new();
        let global = GlobalSnapshot {
            total_requests: 100,
            success_requests: 95,
            failed_requests: 5,
            in_flight_requests: 2,
        };
        let mut providers = HashMap::new();
        providers.insert(
            "omlx".to_string(),
            ProviderCounters {
                request_count: 60,
                failure_count: 3,
                fallback_count: 0,
                total_latency_ms: 9000,
                prompt_tokens: 600,
                completion_tokens: 1200,
                total_ttft_ms: 3000,
                ttft_samples: 60,
                total_generation_time_ms: 6000,
                generation_tokens: 1200,
            },
        );
        providers.insert(
            "lmstudio".to_string(),
            ProviderCounters {
                request_count: 40,
                failure_count: 2,
                fallback_count: 5,
                total_latency_ms: 8000,
                prompt_tokens: 400,
                completion_tokens: 800,
                total_ttft_ms: 2000,
                ttft_samples: 40,
                total_generation_time_ms: 6000,
                generation_tokens: 800,
            },
        );
        let snap = builder.build(&global, &providers);
        assert_eq!(snap.total_requests, 100);
        assert_eq!(snap.in_flight, 2);
        assert!((snap.success_rate - 95.0).abs() < 0.01);
        assert_eq!(snap.provider_stats.len(), 2);
        // Provider with more requests should be first
        assert_eq!(snap.provider_stats[0].name, "omlx");
        assert_eq!(snap.timeseries.len(), 1);
    }

    #[test]
    fn test_timeseries_cap() {
        let mut builder = SnapshotBuilder::new();
        let global = GlobalSnapshot {
            total_requests: 0,
            success_requests: 0,
            failed_requests: 0,
            in_flight_requests: 0,
        };
        for _ in 0..350 {
            builder.build(&global, &HashMap::new());
        }
        assert!(builder.timeseries.len() <= 300);
    }
}
