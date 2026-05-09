//! Background metrics aggregator.
//!
//! Runs as a `tokio::spawn`'d task. Drains the bounded mpsc channel,
//! updates atomic counters and per-provider stats, and periodically
//! rebuilds the in-memory snapshot.
//!
//! **NEVER** accessed from the request hot path.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;
use tracing::debug;

use super::MetricEvent;
use super::counters::{GlobalCounters, ProviderStatsMap};
use super::snapshot::{MetricsSnapshot, SnapshotBuilder};

/// Snapshot update interval.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(1);

/// Run the background aggregator loop.
///
/// This function is intended to be called via `tokio::spawn`. It:
/// 1. Drains events from the mpsc channel
/// 2. Updates global + per-provider counters
/// 3. Rebuilds the snapshot every `SNAPSHOT_INTERVAL`
pub async fn run_aggregator(
    mut receiver: mpsc::Receiver<MetricEvent>,
    global_counters: Arc<GlobalCounters>,
    provider_stats: Arc<ProviderStatsMap>,
    snapshot_store: Arc<ArcSwap<MetricsSnapshot>>,
) {
    let mut builder = SnapshotBuilder::new();
    let mut snapshot_interval = tokio::time::interval(SNAPSHOT_INTERVAL);
    snapshot_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // Process incoming events
            Some(event) = receiver.recv() => {
                process_event(&event, &global_counters, &provider_stats);
            }
            // Periodic snapshot rebuild
            _ = snapshot_interval.tick() => {
                let global_snap = global_counters.snapshot();
                let provider_snap = provider_stats.snapshot();
                let snapshot = builder.build(&global_snap, &provider_snap);
                snapshot_store.store(Arc::new(snapshot));
                debug!(
                    total = global_snap.total_requests,
                    in_flight = global_snap.in_flight_requests,
                    "metrics snapshot updated"
                );
            }
        }
    }
}

fn process_event(event: &MetricEvent, global: &GlobalCounters, provider_stats: &ProviderStatsMap) {
    match event {
        MetricEvent::RequestStart { .. } => {
            global.inc_total();
            global.inc_in_flight();
        }
        MetricEvent::RequestEnd {
            provider,
            model,
            duration,
            success,
            tokens,
            ttft_ms,
        } => {
            global.dec_in_flight();
            if *success {
                global.inc_success();
            } else {
                global.inc_failed();
            }
            let latency_ms = duration.as_millis() as u64;
            let (pt, ct) = tokens
                .as_ref()
                .map(|t| (t.prompt_tokens, t.completion_tokens))
                .unwrap_or((0, 0));
            provider_stats.record(provider, model, *success, latency_ms, pt, ct, *ttft_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_process_request_lifecycle() {
        let global = GlobalCounters::new();
        let provider_stats = ProviderStatsMap::new();

        let start_event = MetricEvent::RequestStart {
            provider: "omlx".into(),
            model: "gpt-4".into(),
            timestamp: std::time::Instant::now(),
        };
        process_event(&start_event, &global, &provider_stats);

        let snap = global.snapshot();
        assert_eq!(snap.total_requests, 1);
        assert_eq!(snap.in_flight_requests, 1);

        let end_event = MetricEvent::RequestEnd {
            provider: "omlx".into(),
            model: "gpt-4".into(),
            duration: Duration::from_millis(150),
            ttft_ms: Some(100),
            success: true,
            tokens: Some(super::super::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
            }),
        };
        process_event(&end_event, &global, &provider_stats);

        let snap = global.snapshot();
        assert_eq!(snap.in_flight_requests, 0);
        assert_eq!(snap.success_requests, 1);

        let psnap = provider_stats.snapshot();
        assert_eq!(psnap["omlx:gpt-4"].request_count, 1);
        assert_eq!(psnap["omlx:gpt-4"].prompt_tokens, 10);
    }
}
