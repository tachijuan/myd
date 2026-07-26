//! Diagnostics for the transfer and SFTP data paths.
//!
//! Transfers are slow for reasons that only show up on a long link, so the
//! numbers that matter — round-trip counts, pipeline depth, per-chunk latency —
//! have to be recoverable from a log taken at the real site.
//!
//! Built on `tracing` so spans nest and time themselves, and so a user can pick
//! what they want with `MYD_LOG=myd::transfer=debug` instead of getting
//! everything or nothing. Output goes to a file: the TUI owns the alternate
//! screen, and anything written to the terminal would corrupt the display.
//!
//! Off unless asked for. With neither `MYD_LOG` nor `MYD_TRACE` set, no
//! subscriber is installed at all, so every macro compiles down to an atomic
//! load and a branch.
//!
//! # Enabling
//!
//! ```text
//! MYD_TRACE=1 myd                      # everything at debug, ~/.cache/myd-trace.log
//! MYD_LOG=myd::transfer=debug myd      # just the transfer engine
//! MYD_TRACE_FILE=/tmp/t.log MYD_TRACE=1 myd
//! MYD_LOG_FORMAT=json MYD_TRACE=1 myd  # machine-readable
//! ```
//!
//! # Cost
//!
//! Per-chunk events would be their own bottleneck on a fast link, so chunk
//! latencies go into a [`LatencyHistogram`] of plain atomics and are emitted as
//! a single summary when the file finishes. That keeps the hot path to one
//! `fetch_add` per chunk.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Where trace output goes.
pub fn trace_path() -> String {
    std::env::var("MYD_TRACE_FILE").unwrap_or_else(|_| {
        let base = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{}/.cache/myd-trace.log", base)
    })
}

/// Whether any diagnostics were requested.
///
/// `MYD_TRACE` is kept as the legacy switch (it predates `tracing` here and is
/// in muscle memory); `MYD_LOG` is the filter-aware form.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MYD_TRACE").is_ok_and(|v| v != "0") || std::env::var("MYD_LOG").is_ok()
    })
}

/// Keeps the non-blocking writer's flush thread alive for the process's life.
static GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// Install the subscriber. Idempotent; safe to call from tests and from both
/// binaries.
///
/// Does nothing when diagnostics are off, which leaves `tracing`'s global
/// dispatcher unset — the cheapest possible state.
pub fn init() {
    if !enabled() {
        return;
    }
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.set(()).is_err() {
        return;
    }

    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::fmt;
    use tracing_subscriber::prelude::*;

    // `MYD_LOG` wins; a bare `MYD_TRACE=1` means "everything from myd".
    let filter = std::env::var("MYD_LOG")
        .ok()
        .and_then(|s| EnvFilter::try_new(s).ok())
        .unwrap_or_else(|| EnvFilter::new("myd=debug"));

    let path = trace_path();
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };

    // Non-blocking: the render loop must never wait on a disk write.
    let (writer, guard) = tracing_appender::non_blocking(file);
    let _ = GUARD.set(guard);

    let json = std::env::var("MYD_LOG_FORMAT").is_ok_and(|v| v == "json");
    let registry = tracing_subscriber::registry().with(filter);
    if json {
        let _ = registry
            .with(fmt::layer().json().with_writer(writer).with_ansi(false))
            .try_init();
    } else {
        let _ = registry
            .with(
                fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false)
                    .with_target(true),
            )
            .try_init();
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "myd diagnostics started"
    );
}

/// Number of buckets in a [`LatencyHistogram`]: powers of two from <1ms up to
/// >32s, which spans everything from a loopback reply to a stalled link.
const BUCKETS: usize = 17;

/// A lock-free latency histogram.
///
/// Recording is one `fetch_add`, so it can sit on a per-chunk path without
/// becoming the thing being measured. Buckets are powers of two in
/// milliseconds — precise enough to tell a 1ms local reply from a 150ms
/// transatlantic one, which is the distinction that matters here.
#[derive(Debug, Default)]
pub struct LatencyHistogram {
    buckets: [AtomicU64; BUCKETS],
    count: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
}

impl LatencyHistogram {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation.
    #[inline]
    pub fn record(&self, d: Duration) {
        let us = d.as_micros() as u64;
        let ms = us / 1000;
        // 0-1ms lands in bucket 0; everything else in floor(log2(ms)) + 1.
        let idx = if ms == 0 {
            0
        } else {
            ((64 - ms.leading_zeros()) as usize).min(BUCKETS - 1)
        };
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_us.fetch_add(us, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn mean_ms(&self) -> f64 {
        let n = self.count();
        if n == 0 {
            return 0.0;
        }
        self.total_us.load(Ordering::Relaxed) as f64 / n as f64 / 1000.0
    }

    pub fn max_ms(&self) -> f64 {
        self.max_us.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// Upper bound in ms of the bucket holding the `q`th quantile (0.0..=1.0).
    ///
    /// Bucket-resolution only — the point is spotting a fat tail, not exact
    /// percentiles.
    pub fn quantile_ms(&self, q: f64) -> f64 {
        let n = self.count();
        if n == 0 {
            return 0.0;
        }
        let target = (n as f64 * q).ceil() as u64;
        let mut seen = 0u64;
        for (i, b) in self.buckets.iter().enumerate() {
            seen += b.load(Ordering::Relaxed);
            if seen >= target {
                return if i == 0 { 1.0 } else { (1u64 << (i - 1)) as f64 * 2.0 };
            }
        }
        self.max_ms()
    }

    /// `p50=.. p90=.. p99=.. max=..`, for one summary line.
    pub fn summary(&self) -> String {
        format!(
            "n={} mean={:.1}ms p50={:.0}ms p90={:.0}ms p99={:.0}ms max={:.1}ms",
            self.count(),
            self.mean_ms(),
            self.quantile_ms(0.50),
            self.quantile_ms(0.90),
            self.quantile_ms(0.99),
            self.max_ms(),
        )
    }
}

/// Counters for one file transfer, summarised in a single event at the end.
#[derive(Debug, Default)]
pub struct TransferMetrics {
    pub chunks: AtomicU64,
    pub bytes: AtomicU64,
    /// Reads that came back shorter than asked for — the condition that used to
    /// silently truncate files.
    pub short_reads: AtomicU64,
    /// Extra reads issued to fill a short chunk.
    pub refills: AtomicU64,
    pub peak_inflight: AtomicU64,
    inflight: AtomicU64,
    pub read_latency: LatencyHistogram,
    pub write_latency: LatencyHistogram,
}

impl TransferMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn enter_read(&self) {
        let now = self.inflight.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_inflight.fetch_max(now, Ordering::Relaxed);
    }

    #[inline]
    pub fn leave_read(&self, elapsed: Duration, got: usize, wanted: usize) {
        self.inflight.fetch_sub(1, Ordering::Relaxed);
        self.read_latency.record(elapsed);
        self.chunks.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(got as u64, Ordering::Relaxed);
        if got < wanted {
            self.short_reads.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn record_refill(&self) {
        self.refills.fetch_add(1, Ordering::Relaxed);
    }
}

/// The negotiated SFTP read limit, discovered empirically.
///
/// `openssh-sftp-client` gates its `max_read_len` accessor behind a private
/// feature, so the value the server actually agreed to cannot be read back. It
/// can be *observed*, though: ask for more than any plausible limit and see how
/// much comes back. One round trip per connection, and it turns an invisible
/// number into a logged one.
static OBSERVED_READ_LIMIT: AtomicU64 = AtomicU64::new(0);

pub fn set_observed_read_limit(bytes: u64) {
    OBSERVED_READ_LIMIT.store(bytes, Ordering::Relaxed);
    tracing::info!(bytes, "observed SFTP max_read_len");
}

pub fn observed_read_limit() -> Option<u64> {
    match OBSERVED_READ_LIMIT.load(Ordering::Relaxed) {
        0 => None,
        n => Some(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_by_magnitude() {
        let h = LatencyHistogram::new();
        h.record(Duration::from_micros(500)); // sub-ms
        h.record(Duration::from_millis(150));
        h.record(Duration::from_millis(150));
        assert_eq!(h.count(), 3);
        // The bulk sits at 150ms, so the median bucket must be in that region
        // rather than down with the sub-ms sample.
        assert!(h.quantile_ms(0.5) >= 128.0, "p50 was {}", h.quantile_ms(0.5));
        assert!((h.max_ms() - 150.0).abs() < 1.0);
    }

    #[test]
    fn histogram_is_empty_safe() {
        let h = LatencyHistogram::new();
        assert_eq!(h.count(), 0);
        assert_eq!(h.mean_ms(), 0.0);
        assert_eq!(h.quantile_ms(0.5), 0.0);
    }

    #[test]
    fn metrics_track_peak_and_short_reads() {
        let m = TransferMetrics::new();
        m.enter_read();
        m.enter_read();
        assert_eq!(m.peak_inflight.load(Ordering::Relaxed), 2);
        m.leave_read(Duration::from_millis(10), 100, 100);
        m.leave_read(Duration::from_millis(10), 50, 100);
        assert_eq!(m.short_reads.load(Ordering::Relaxed), 1);
        assert_eq!(m.bytes.load(Ordering::Relaxed), 150);
    }
}
