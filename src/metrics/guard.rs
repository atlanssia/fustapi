//! Request lifecycle guard.
//!
//! Ensures every `request_start` is paired with exactly one `request_end`.
//! The guard owns the emitter and can be consumed in three ways:
//!
//! 1. `into_tracker()` — streaming path; transfers ownership to `StreamTracker`
//! 2. `finish()`       — non-streaming path; emits `request_end` explicitly
//! 3. **Drop**          — safety net; emits `request_end(success=false)` if unconsumed
//!
//! Uses an inner struct (no Drop) so consuming methods can deconstruct freely.
//! The outer struct's Drop checks whether the inner has already been taken.

use std::time::Instant;

use super::{MetricsEmitter, StreamTracker, TokenUsage};

struct Inner {
    emitter: Option<MetricsEmitter>,
    provider: String,
    model: String,
    start: Instant,
}

/// Owning guard for a single request's metric lifecycle.
///
/// Created via `RequestGuard::start()`, which immediately emits `RequestStart`.
/// Must be consumed exactly once via `into_tracker()`, `finish()`, or `finish_err()`.
/// If dropped without consumption, emits `RequestEnd(success=false)` as a safety net.
pub struct RequestGuard {
    inner: Option<Inner>,
}

impl RequestGuard {
    /// Start tracking a new request. Emits `RequestStart` immediately.
    #[must_use]
    pub fn start(emitter: MetricsEmitter, provider: &str, model: &str) -> Self {
        let start = emitter.request_start(provider, model);
        Self {
            inner: Some(Inner {
                emitter: Some(emitter),
                provider: provider.to_string(),
                model: model.to_string(),
                start,
            }),
        }
    }

    /// Convert into a `StreamTracker` for streaming requests.
    ///
    /// Consumes `self`. The tracker takes over `request_end` responsibility.
    #[must_use]
    pub fn into_tracker(mut self) -> StreamTracker {
        let inner = self.inner.take().expect("guard already consumed");
        let emitter = inner.emitter.expect("guard already consumed");
        StreamTracker::new(emitter, inner.provider, inner.model, inner.start)
    }

    /// Emit `request_end` for a completed (non-streaming) request.
    ///
    /// Consumes `self`.
    pub fn finish(mut self, success: bool, tokens: Option<TokenUsage>, ttft_ms: Option<u64>) {
        let inner = self.inner.take().expect("guard already consumed");
        let emitter = inner.emitter.expect("guard already consumed");
        emitter.request_end(&inner.provider, &inner.model, inner.start, success, tokens, ttft_ms);
    }

    /// Emit `request_end(success=false)` for a failed request.
    ///
    /// Consumes `self`.
    pub fn finish_err(self) {
        self.finish(false, None, None);
    }

    /// Milliseconds elapsed since this guard was created.
    /// Used by non-streaming path to record first-token time.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        self.inner
            .as_ref()
            .expect("guard alive")
            .start
            .elapsed()
            .as_millis() as u64
    }

    /// Update the model name (e.g., to reflect the upstream model after route resolution).
    pub fn set_model(&mut self, model: String) {
        if let Some(inner) = self.inner.as_mut() {
            inner.model = model;
        }
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take()
            && let Some(emitter) = inner.emitter
        {
            emitter.request_end(
                &inner.provider,
                &inner.model,
                inner.start,
                false,
                None,
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guard_into_tracker_consumes_emitter() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(64);
        let emitter = MetricsEmitter { sender };

        let guard = RequestGuard::start(emitter, "prov", "mod");
        let tracker = guard.into_tracker();

        assert_eq!(tracker.provider, "prov");
        assert_eq!(tracker.model, "mod");
    }

    #[test]
    fn test_guard_drop_without_consumption_emits_failure() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let emitter = MetricsEmitter { sender };

        {
            let _guard = RequestGuard::start(emitter, "prov", "mod");
        }

        // Should have emitted RequestStart + RequestEnd(success=false)
        let start_event = receiver.try_recv().expect("should have RequestStart");
        match start_event {
            super::super::MetricEvent::RequestStart { provider, model, .. } => {
                assert_eq!(provider, "prov");
                assert_eq!(model, "mod");
            }
            _ => panic!("expected RequestStart"),
        }

        let end_event = receiver.try_recv().expect("should have RequestEnd");
        match end_event {
            super::super::MetricEvent::RequestEnd {
                success: false, provider, model, ..
            } => {
                assert_eq!(provider, "prov");
                assert_eq!(model, "mod");
            }
            _ => panic!("expected RequestEnd with success=false"),
        }

        assert!(receiver.try_recv().is_err(), "should have no more events");
    }

    #[test]
    fn test_guard_finish_emits_success() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let emitter = MetricsEmitter { sender };

        let guard = RequestGuard::start(emitter, "p", "m");
        guard.finish(true, None, None);

        // Should have RequestStart + RequestEnd(success=true)
        let _start = receiver.try_recv().expect("start");
        let end_event = receiver.try_recv().expect("end");
        match end_event {
            super::super::MetricEvent::RequestEnd { success: true, .. } => {}
            _ => panic!("expected success=true"),
        }
    }

    #[test]
    fn test_guard_finish_err_emits_failure() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let emitter = MetricsEmitter { sender };

        let guard = RequestGuard::start(emitter, "p", "m");
        guard.finish_err();

        let _start = receiver.try_recv().expect("start");
        let end_event = receiver.try_recv().expect("end");
        match end_event {
            super::super::MetricEvent::RequestEnd { success: false, .. } => {}
            _ => panic!("expected success=false"),
        }
    }
}
