//! Metrics collection system for `FustAPI`.
//!
//! Architecture:
//! - Request path emits events via `try_send()` — fire-and-forget, never blocks
//! - Background aggregator processes events, updates atomic counters
//! - Dashboard reads from in-memory `ArcSwap<MetricsSnapshot>` — zero contention
//!
//! **Rule: metrics collection MUST be invisible to request latency.**

pub mod aggregator;
pub mod counters;
pub mod guard;
pub mod snapshot;

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use counters::{GlobalCounters, ProviderStatsMap};
use snapshot::MetricsSnapshot;

/// Bounded channel capacity. Events are dropped when full.
const CHANNEL_CAPACITY: usize = 4096;

/// Token usage reported by the provider.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Events emitted from the request path.
#[derive(Debug, Clone)]
pub enum MetricEvent {
    /// A new request has started processing.
    RequestStart {
        provider: String,
        model: String,
        timestamp: Instant,
    },
    /// A request has completed.
    RequestEnd {
        provider: String,
        model: String,
        duration: Duration,
        success: bool,
        tokens: Option<TokenUsage>,
        ttft_ms: Option<u64>,
    },
}

/// Handle for emitting metric events from the request path.
///
/// This is the ONLY type the hot path interacts with.
/// All methods are non-blocking and best-effort.
#[derive(Clone)]
pub struct MetricsEmitter {
    sender: mpsc::Sender<MetricEvent>,
}

impl MetricsEmitter {
    /// Emit an event. Returns immediately. Drops the event if the channel is full.
    pub fn emit(&self, event: MetricEvent) {
        // try_send: non-blocking, returns Err if full — we discard it
        let _ = self.sender.try_send(event);
    }

    /// Convenience: emit a request start event and return the start time.
    #[must_use]
    pub fn request_start(&self, provider: &str, model: &str) -> Instant {
        let now = Instant::now();
        self.emit(MetricEvent::RequestStart {
            provider: provider.to_string(),
            model: model.to_string(),
            timestamp: now,
        });
        now
    }

    /// Convenience: emit a request end event.
    pub fn request_end(
        &self,
        provider: &str,
        model: &str,
        start: Instant,
        success: bool,
        tokens: Option<TokenUsage>,
        ttft_ms: Option<u64>,
    ) {
        self.emit(MetricEvent::RequestEnd {
            provider: provider.to_string(),
            model: model.to_string(),
            duration: start.elapsed(),
            success,
            tokens,
            ttft_ms,
        });
    }
}

/// A tracker that automatically emits `request_end` when dropped.
/// This is used to track stream lifetimes lock-free.
pub struct StreamTracker {
    pub emitter: MetricsEmitter,
    pub provider: String,
    pub model: String,
    pub start: Instant,
    pub success: bool,
    pub ttft_ms: Option<u64>,
    pub tokens: Option<TokenUsage>,
}

impl StreamTracker {
    #[must_use]
    pub fn new(emitter: MetricsEmitter, provider: String, model: String, start: Instant) -> Self {
        Self {
            emitter,
            provider,
            model,
            start,
            success: true,
            ttft_ms: None,
            tokens: None,
        }
    }

    pub fn set_ttft(&mut self, ttft: u64) {
        if self.ttft_ms.is_none() {
            self.ttft_ms = Some(ttft);
        }
    }

    pub fn set_tokens(&mut self, tokens: TokenUsage) {
        self.tokens = Some(tokens);
    }

    pub fn set_success(&mut self, success: bool) {
        self.success = success;
    }
}

impl Drop for StreamTracker {
    fn drop(&mut self) {
        self.emitter.request_end(
            &self.provider,
            &self.model,
            self.start,
            self.success,
            self.tokens.clone(),
            self.ttft_ms,
        );
    }
}

/// Read-only handle for the dashboard to access the current snapshot.
#[derive(Clone)]
pub struct MetricsReader {
    snapshot: Arc<ArcSwap<MetricsSnapshot>>,
}

impl MetricsReader {
    /// Load the current metrics snapshot. Lock-free, zero-copy Arc load.
    #[must_use]
    pub fn snapshot(&self) -> Arc<MetricsSnapshot> {
        self.snapshot.load_full()
    }
}

/// Initialize the metrics system. Returns the emitter (for hot path)
/// and reader (for dashboard), and spawns the background aggregator.
#[must_use]
pub fn init() -> (MetricsEmitter, MetricsReader) {
    let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
    let global_counters = Arc::new(GlobalCounters::new());
    let provider_stats = Arc::new(ProviderStatsMap::new());
    let snapshot_store = Arc::new(ArcSwap::new(Arc::new(MetricsSnapshot::default())));

    let emitter = MetricsEmitter { sender };
    let reader = MetricsReader {
        snapshot: snapshot_store.clone(),
    };

    tokio::spawn(aggregator::run_aggregator(
        receiver,
        global_counters,
        provider_stats,
        snapshot_store,
    ));

    (emitter, reader)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emitter_does_not_block_when_channel_full() {
        // Create a channel with capacity 1
        let (sender, _receiver) = mpsc::channel(1);
        let emitter = MetricsEmitter { sender };

        // Fill it
        emitter.emit(MetricEvent::RequestStart {
            provider: "test".into(),
            model: "gpt-4".into(),
            timestamp: Instant::now(),
        });

        // This should not block or panic — event is silently dropped
        emitter.emit(MetricEvent::RequestStart {
            provider: "test2".into(),
            model: "gpt-4".into(),
            timestamp: Instant::now(),
        });
    }

    #[test]
    fn test_reader_returns_default_snapshot() {
        let snapshot_store = Arc::new(ArcSwap::new(Arc::new(MetricsSnapshot::default())));
        let reader = MetricsReader {
            snapshot: snapshot_store,
        };
        let snap = reader.snapshot();
        assert_eq!(snap.total_requests, 0);
        assert_eq!(snap.qps, 0.0);
    }
}
