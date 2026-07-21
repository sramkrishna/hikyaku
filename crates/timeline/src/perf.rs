// Lightweight scoped timing for the GTK hot path.
//
// Usage:
//   let _g = perf::scope("render_body");           // logs if > 500µs
//   let _g = perf::scope_with("info_to_obj", ctx); // include a context string
//   let _g = perf::scope_gt("noisy", 2_000);       // custom threshold in µs
//
// The guard logs `tracing::info!` on drop with elapsed microseconds. All
// scopes carry a `perf=` tag so logs can be filtered with
// `grep 'perf=' ~/hikyaku.log` to see the full hot-path heat map.
//
// Keep thresholds small but non-zero — every scope has ~100ns overhead (the
// Instant + format cost on drop), so logging sub-µs events would flood stderr
// and distort the very behaviour we're trying to measure.

#![allow(dead_code)]

use std::time::Instant;

/// Default threshold: only log scopes that take longer than this (microseconds).
/// Set to 0 to log every scope (useful for one-shot debugging).
pub const DEFAULT_THRESHOLD_US: u128 = 500;

pub struct Scope {
    name: &'static str,
    context: Option<String>,
    threshold_us: u128,
    start: Instant,
}

impl Scope {
    pub fn new(name: &'static str) -> Self {
        Self { name, context: None, threshold_us: DEFAULT_THRESHOLD_US, start: Instant::now() }
    }
    pub fn with_ctx(name: &'static str, ctx: impl Into<String>) -> Self {
        Self { name, context: Some(ctx.into()), threshold_us: DEFAULT_THRESHOLD_US, start: Instant::now() }
    }
    pub fn with_threshold(name: &'static str, threshold_us: u128) -> Self {
        Self { name, context: None, threshold_us, start: Instant::now() }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let us = self.start.elapsed().as_micros();
        if us >= self.threshold_us {
            match &self.context {
                Some(ctx) => tracing::info!("perf={} ctx={} us={}", self.name, ctx, us),
                None => tracing::info!("perf={} us={}", self.name, us),
            }
        }
    }
}

/// Start a scoped timer with the default threshold.
pub fn scope(name: &'static str) -> Scope {
    Scope::new(name)
}

/// Start a scoped timer with an additional context string (e.g. room id or row index).
pub fn scope_with(name: &'static str, ctx: impl Into<String>) -> Scope {
    Scope::with_ctx(name, ctx)
}

/// Start a scoped timer with a custom threshold in microseconds.
/// Use `0` to log unconditionally.
pub fn scope_gt(name: &'static str, threshold_us: u128) -> Scope {
    Scope::with_threshold(name, threshold_us)
}

/// Thread-local accumulator — useful for aggregating many small scopes
/// (e.g. per-row binds) into a single log line at the end of a batch.
pub struct Accumulator {
    pub name: &'static str,
    pub count: u64,
    pub total_us: u128,
    pub max_us: u128,
}

impl Accumulator {
    pub fn new(name: &'static str) -> Self {
        Self { name, count: 0, total_us: 0, max_us: 0 }
    }
    pub fn add(&mut self, us: u128) {
        self.count += 1;
        self.total_us += us;
        if us > self.max_us { self.max_us = us; }
    }
    pub fn log_and_reset(&mut self) {
        if self.count > 0 {
            tracing::info!(
                "perf={} count={} total_us={} avg_us={} max_us={}",
                self.name, self.count, self.total_us,
                self.total_us / self.count as u128, self.max_us
            );
        }
        self.count = 0;
        self.total_us = 0;
        self.max_us = 0;
    }
}
