//! Atomic counters for zero-overhead metrics collection.
//!
//! All counters use atomic operations — safe to increment from any thread
//! without locking. These are updated by the background aggregator,
//! NEVER from the request hot path directly.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Relaxed ordering — sufficient for best-effort counters.
const ORD: Ordering = Ordering::Relaxed;

/// Global request counters.
pub struct GlobalCounters {
    pub total_requests: AtomicU64,
    pub success_requests: AtomicU64,
    pub failed_requests: AtomicU64,
    pub in_flight_requests: AtomicI64,
}

impl Default for GlobalCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalCounters {
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            success_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            in_flight_requests: AtomicI64::new(0),
        }
    }

    pub fn inc_total(&self) {
        self.total_requests.fetch_add(1, ORD);
    }

    pub fn inc_success(&self) {
        self.success_requests.fetch_add(1, ORD);
    }

    pub fn inc_failed(&self) {
        self.failed_requests.fetch_add(1, ORD);
    }

    pub fn inc_in_flight(&self) {
        self.in_flight_requests.fetch_add(1, ORD);
    }

    pub fn dec_in_flight(&self) {
        self.in_flight_requests.fetch_sub(1, ORD);
    }

    pub fn snapshot(&self) -> GlobalSnapshot {
        GlobalSnapshot {
            total_requests: self.total_requests.load(ORD),
            success_requests: self.success_requests.load(ORD),
            failed_requests: self.failed_requests.load(ORD),
            in_flight_requests: self.in_flight_requests.load(ORD),
        }
    }
}

/// Point-in-time snapshot of global counters.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GlobalSnapshot {
    pub total_requests: u64,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub in_flight_requests: i64,
}

/// Per-provider counters (updated only by the aggregator — single writer).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ProviderCounters {
    pub request_count: u64,
    pub failure_count: u64,
    pub fallback_count: u64,
    pub total_latency_ms: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_ttft_ms: u64,
    pub ttft_samples: u64,
    pub total_generation_time_ms: u64,
    pub generation_tokens: u64,
}

/// Thread-safe provider stats map. Only the aggregator writes; snapshots are
/// cheap clones for the dashboard.
pub struct ProviderStatsMap {
    inner: std::sync::RwLock<HashMap<String, ProviderCounters>>,
}

impl Default for ProviderStatsMap {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderStatsMap {
    pub fn new() -> Self {
        Self {
            inner: std::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Record a completed request for a provider (called by aggregator only).
    pub fn record(
        &self,
        provider: &str,
        success: bool,
        latency_ms: u64,
        prompt_tokens: u32,
        completion_tokens: u32,
        ttft_ms: Option<u64>,
    ) {
        let mut map = self.inner.write().expect("provider stats lock poisoned");
        let entry = map.entry(provider.to_string()).or_default();
        entry.request_count += 1;
        if !success {
            entry.failure_count += 1;
        }
        entry.total_latency_ms += latency_ms;
        entry.prompt_tokens += prompt_tokens as u64;
        entry.completion_tokens += completion_tokens as u64;

        if let Some(t) = ttft_ms {
            entry.total_ttft_ms += t;
            entry.ttft_samples += 1;
            let gen_time = latency_ms.saturating_sub(t);
            entry.total_generation_time_ms += gen_time;
            entry.generation_tokens += completion_tokens as u64;
        }
    }

    /// Record a fallback event for a provider (called by aggregator only).
    pub fn record_fallback(&self, provider: &str) {
        let mut map = self.inner.write().expect("provider stats lock poisoned");
        let entry = map.entry(provider.to_string()).or_default();
        entry.fallback_count += 1;
    }

    /// Clone the current stats for snapshot.
    pub fn snapshot(&self) -> HashMap<String, ProviderCounters> {
        let map = self.inner.read().expect("provider stats lock poisoned");
        map.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_counters_increment_and_snapshot() {
        let counters = GlobalCounters::new();
        counters.inc_total();
        counters.inc_total();
        counters.inc_success();
        counters.inc_failed();
        counters.inc_in_flight();
        counters.inc_in_flight();
        counters.dec_in_flight();

        let snap = counters.snapshot();
        assert_eq!(snap.total_requests, 2);
        assert_eq!(snap.success_requests, 1);
        assert_eq!(snap.failed_requests, 1);
        assert_eq!(snap.in_flight_requests, 1);
    }

    #[test]
    fn test_provider_stats_record_and_snapshot() {
        let stats = ProviderStatsMap::new();
        stats.record("omlx", true, 150, 10, 20, Some(50));
        stats.record("omlx", false, 300, 5, 0, None);
        stats.record("lmstudio", true, 100, 8, 15, Some(30));
        stats.record_fallback("lmstudio");

        let snap = stats.snapshot();
        assert_eq!(snap.len(), 2);

        let omlx = &snap["omlx"];
        assert_eq!(omlx.request_count, 2);
        assert_eq!(omlx.failure_count, 1);
        assert_eq!(omlx.total_latency_ms, 450);
        assert_eq!(omlx.prompt_tokens, 15);
        assert_eq!(omlx.completion_tokens, 20);
        assert_eq!(omlx.total_ttft_ms, 50);
        assert_eq!(omlx.ttft_samples, 1);
        assert_eq!(omlx.total_generation_time_ms, 100);
        assert_eq!(omlx.generation_tokens, 20);

        let lms = &snap["lmstudio"];
        assert_eq!(lms.request_count, 1);
        assert_eq!(lms.fallback_count, 1);
    }
}
