//! Non-blocking transfer engine.
//!
//! Requests stack up in a queue, a bounded number run in parallel, and the UI
//! stays interactive throughout — which is the whole point, as against the
//! modal overlay that local copies use.

mod queue;
mod worker;

pub use queue::{PendingDest, TransferQueue};
pub use worker::{run_transfer, TransferJob, TransferOutcome};

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::utils::sizes::CancelToken;
use crate::vfs::VPath;

/// How many transfers run at once by default, and how many files within one
/// directory are copied concurrently.
///
/// Every small file costs a few serial round trips (open, write, close), so on a
/// long link (a US↔EU hop is ~150 ms) throughput is set by how many of those
/// sequences overlap, not by bandwidth. Measured against a simulated 30 ms link
/// copying 50 small files: 4 → 0.99 s, 8 → 0.60 s, 16 → 0.42 s, 32 → 0.29 s.
///
/// 16 takes most of that win while staying well inside the connection's
/// `max_pending_requests` (256): only large files also open a
/// [`DEFAULT_CHUNKS_IN_FLIGHT`] window, and those are capped separately (see
/// [`large_file_chunks_in_flight`]).
pub const DEFAULT_MAX_PARALLEL: usize = 16;

/// Chunk size for streaming a file. 1 MB is the documented plateau for SFTP —
/// larger buffers stop helping latency.
pub const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

/// Files at or above this size get intra-file concurrency; below it the
/// per-request overhead outweighs the benefit and parallelism comes from running
/// several transfers at once instead.
pub const LARGE_FILE_THRESHOLD: u64 = 4 * 1024 * 1024;

/// Concurrent positioned reads per large file.
///
/// Measured against a real sshd: sequential reads managed ~54 MB/s on localhost;
/// a window of 32 reached ~320 MB/s (≈6×), close to the `sftp` binary. Going
/// higher keeps scaling but 4 parallel transfers × 32 reads already approaches
/// the SFTP connection's `max_pending_requests`, so 32 is the balance point.
pub const DEFAULT_CHUNKS_IN_FLIGHT: usize = 32;

/// The SFTP connection's `max_pending_requests`. Kept here so the transfer
/// engine can size its windows against the same budget the backend advertises.
const CONNECTION_REQUEST_BUDGET: usize = 256;

/// Per-file chunk window, reduced so that several concurrent large-file
/// transfers together stay inside the connection's request budget.
///
/// Without this, `max_parallel` × `chunks_in_flight` could far exceed
/// `max_pending_requests`, and the surplus requests would queue inside the
/// client rather than adding throughput — while the memory for their buffers
/// (one `chunk_size` each) was already committed.
pub fn large_file_chunks_in_flight(config: &TransferConfig) -> usize {
    let fair_share = CONNECTION_REQUEST_BUDGET / config.max_parallel.max(1);
    config.chunks_in_flight.min(fair_share).max(1)
}

/// Tunables for the engine.
///
/// Deliberately one struct with a `Default`, so making these user-configurable
/// later is a matter of populating it from a settings file — no logic changes.
/// The queue re-reads `max_parallel` on every scheduling pass, so raising it at
/// runtime takes effect immediately.
#[derive(Debug, Clone, Copy)]
pub struct TransferConfig {
    pub max_parallel: usize,
    pub chunk_size: usize,
    /// Concurrent chunk reads within a single large file.
    pub chunks_in_flight: usize,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            max_parallel: DEFAULT_MAX_PARALLEL,
            chunk_size: DEFAULT_CHUNK_SIZE,
            chunks_in_flight: DEFAULT_CHUNKS_IN_FLIGHT,
        }
    }
}

/// Identifier for a queued transfer, unique within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransferId(pub u64);

impl std::fmt::Display for TransferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Where a transfer is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Active,
    Done,
    Failed(String),
    Cancelled,
}

impl TransferState {
    /// Whether the transfer has stopped moving (for grouping in the panel).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TransferState::Done | TransferState::Failed(_) | TransferState::Cancelled
        )
    }
}

/// Live byte counters shared between a worker and the render loop.
///
/// Follows the same design as [`crate::widget::progress::OpProgress`]: an `Arc`
/// of atomics, written by the worker and read each frame, so no lock ever sits
/// between the transfer and the UI.
#[derive(Debug)]
pub struct TransferProgress {
    bytes_done: AtomicU64,
    total_bytes: AtomicU64,
    finished: AtomicBool,
    /// Smoothed transfer rate in bytes/sec, stored as a bit-cast `f64` so it
    /// stays lock-free alongside the counters.
    rate_bits: AtomicU64,
    start: Instant,
}

impl Default for TransferProgress {
    fn default() -> Self {
        Self {
            bytes_done: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            rate_bits: AtomicU64::new(0),
            start: Instant::now(),
        }
    }
}

impl TransferProgress {
    pub fn new(total_bytes: u64) -> Self {
        let p = Self::default();
        p.total_bytes.store(total_bytes, Ordering::Relaxed);
        p
    }

    /// Record `n` more bytes transferred and update the smoothed rate.
    pub fn add_bytes(&self, n: u64) {
        let done = self.bytes_done.fetch_add(n, Ordering::Relaxed) + n;
        // Exponentially-weighted average over the whole-transfer average keeps
        // the displayed rate steady without needing a timer task.
        //
        // The floor matters: dividing by a near-zero elapsed time yields a
        // nonsense rate (terabytes/sec) on the very first chunk, which then
        // takes many samples to smooth away. Below this the sample is skipped
        // and the rate simply stays unknown for another frame.
        const MIN_ELAPSED_SECS: f64 = 0.05;
        let elapsed = self.start.elapsed().as_secs_f64();
        if elapsed >= MIN_ELAPSED_SECS {
            let instant = done as f64 / elapsed;
            let prev = f64::from_bits(self.rate_bits.load(Ordering::Relaxed));
            let smoothed = if prev == 0.0 {
                instant
            } else {
                prev * 0.7 + instant * 0.3
            };
            self.rate_bits.store(smoothed.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_total(&self, total: u64) {
        self.total_bytes.store(total, Ordering::Relaxed);
    }

    /// Add newly discovered bytes to the total.
    ///
    /// A directory transfer learns its size level by level as it lists, rather
    /// than from a full pre-walk, so the total grows while the copy runs.
    pub fn add_total(&self, bytes: u64) {
        self.total_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn finish(&self) {
        self.finished.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }

    pub fn bytes_done(&self) -> u64 {
        self.bytes_done.load(Ordering::Relaxed)
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
    }

    /// Fraction complete in `0.0..=1.0`. An unknown total reads as 0.
    pub fn fraction(&self) -> f64 {
        let total = self.total_bytes();
        if total == 0 {
            return 0.0;
        }
        (self.bytes_done() as f64 / total as f64).clamp(0.0, 1.0)
    }

    /// Smoothed rate in bytes/sec, or `None` before anything has moved.
    pub fn rate(&self) -> Option<f64> {
        let r = f64::from_bits(self.rate_bits.load(Ordering::Relaxed));
        if r > 0.0 {
            Some(r)
        } else {
            None
        }
    }

    /// Estimated time remaining. `None` when the total or rate is unknown.
    pub fn eta(&self) -> Option<Duration> {
        let total = self.total_bytes();
        let done = self.bytes_done();
        let rate = self.rate()?;
        if total == 0 || done >= total || rate <= 0.0 {
            return None;
        }
        Some(Duration::from_secs_f64((total - done) as f64 / rate))
    }
}

/// One queued or running transfer.
pub struct Transfer {
    pub id: TransferId,
    pub src: VPath,
    pub dest: VPath,
    /// Display name (the file's basename).
    pub name: String,
    /// Whether the item being transferred is a directory — a hint from the
    /// caller so the destination tree can draw the right ghost icon. Best-effort;
    /// the worker doesn't depend on it.
    pub is_dir: bool,
    pub state: TransferState,
    pub progress: Arc<TransferProgress>,
    pub cancel: CancelToken,
}

impl Transfer {
    pub fn new(id: TransferId, src: VPath, dest: VPath, total_bytes: u64) -> Self {
        Self::with_kind(id, src, dest, total_bytes, false)
    }

    pub fn with_kind(
        id: TransferId,
        src: VPath,
        dest: VPath,
        total_bytes: u64,
        is_dir: bool,
    ) -> Self {
        let name = src
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| src.path.display().to_string());
        Self {
            id,
            src,
            dest,
            name,
            is_dir,
            state: TransferState::Queued,
            progress: Arc::new(TransferProgress::new(total_bytes)),
            cancel: CancelToken::new(),
        }
    }

    /// Ask this transfer to stop. The worker observes the token between chunks.
    pub fn request_cancel(&mut self) {
        self.cancel.cancel();
        if self.state == TransferState::Queued {
            // Never started, so it can be retired immediately.
            self.state = TransferState::Cancelled;
        }
    }
}

/// Format a byte rate for display, e.g. "8.4 MB/s".
pub fn format_rate(bytes_per_sec: f64) -> String {
    const UNITS: [&str; 5] = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"];
    let mut v = bytes_per_sec;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{:.0} {}", v, UNITS[unit])
    } else {
        format!("{:.1} {}", v, UNITS[unit])
    }
}

/// Format a duration as a compact ETA, e.g. "12s", "3m04s", "1h02m".
pub fn format_eta(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_the_spec() {
        let c = TransferConfig::default();
        assert_eq!(c.max_parallel, DEFAULT_MAX_PARALLEL);
        assert_eq!(c.chunk_size, 1024 * 1024);
    }

    /// Concurrent large-file transfers must not oversubscribe the SFTP
    /// connection: their combined chunk windows stay inside its request budget.
    #[test]
    fn chunk_windows_stay_within_the_connection_budget() {
        let c = TransferConfig::default();
        let window = large_file_chunks_in_flight(&c);
        assert!(window >= 1);
        assert!(
            window * c.max_parallel <= CONNECTION_REQUEST_BUDGET,
            "{} transfers x {} chunks exceeds the {}-request budget",
            c.max_parallel,
            window,
            CONNECTION_REQUEST_BUDGET
        );
        // A single transfer still gets the full window.
        let solo = TransferConfig { max_parallel: 1, ..TransferConfig::default() };
        assert_eq!(large_file_chunks_in_flight(&solo), DEFAULT_CHUNKS_IN_FLIGHT);
    }

    #[test]
    fn progress_tracks_fraction_and_completion() {
        let p = TransferProgress::new(1000);
        assert_eq!(p.fraction(), 0.0);
        p.add_bytes(250);
        assert!((p.fraction() - 0.25).abs() < 1e-9);
        p.add_bytes(750);
        assert!((p.fraction() - 1.0).abs() < 1e-9);
        assert!(!p.is_finished());
        p.finish();
        assert!(p.is_finished());
    }

    #[test]
    fn fraction_is_zero_for_unknown_total_and_never_exceeds_one() {
        let p = TransferProgress::new(0);
        p.add_bytes(500);
        assert_eq!(p.fraction(), 0.0);

        let q = TransferProgress::new(10);
        q.add_bytes(999);
        assert_eq!(q.fraction(), 1.0);
    }

    #[test]
    fn rate_is_none_before_any_bytes_move() {
        let p = TransferProgress::new(100);
        assert!(p.rate().is_none());
        assert!(p.eta().is_none());
    }

    #[test]
    fn rate_appears_once_enough_time_has_elapsed() {
        let p = TransferProgress::new(1000);
        // Immediately: too little elapsed time to compute a meaningful rate, so
        // the panel shows "—" rather than a nonsense terabytes/sec figure.
        p.add_bytes(10);
        assert!(p.rate().is_none());

        std::thread::sleep(Duration::from_millis(60));
        p.add_bytes(10);
        let rate = p.rate().expect("rate known after a measurable interval");
        // 20 bytes in ~60ms is on the order of hundreds of B/s — the point is
        // that it is finite and sane, not a division-by-almost-zero artifact.
        assert!(rate > 0.0 && rate < 1e6, "implausible rate: {}", rate);
    }

    #[test]
    fn eta_is_none_once_complete() {
        let p = TransferProgress::new(100);
        p.add_bytes(100);
        assert!(p.eta().is_none());
    }

    #[test]
    fn queued_cancel_retires_immediately() {
        let mut t = Transfer::new(
            TransferId(1),
            VPath::local("/a/f"),
            VPath::local("/b/f"),
            10,
        );
        assert_eq!(t.name, "f");
        t.request_cancel();
        assert_eq!(t.state, TransferState::Cancelled);
        assert!(t.cancel.is_cancelled());
    }

    #[test]
    fn active_cancel_leaves_worker_to_observe_the_token() {
        let mut t = Transfer::new(
            TransferId(1),
            VPath::local("/a/f"),
            VPath::local("/b/f"),
            10,
        );
        t.state = TransferState::Active;
        t.request_cancel();
        // Still Active — the worker sets the terminal state when it stops.
        assert_eq!(t.state, TransferState::Active);
        assert!(t.cancel.is_cancelled());
    }

    #[test]
    fn terminal_states_are_classified() {
        assert!(!TransferState::Queued.is_terminal());
        assert!(!TransferState::Active.is_terminal());
        assert!(TransferState::Done.is_terminal());
        assert!(TransferState::Cancelled.is_terminal());
        assert!(TransferState::Failed("x".into()).is_terminal());
    }

    #[test]
    fn rate_and_eta_format_readably() {
        assert_eq!(format_rate(512.0), "512 B/s");
        assert_eq!(format_rate(8.4 * 1024.0 * 1024.0), "8.4 MB/s");
        assert_eq!(format_eta(Duration::from_secs(12)), "12s");
        assert_eq!(format_eta(Duration::from_secs(184)), "3m04s");
        assert_eq!(format_eta(Duration::from_secs(3720)), "1h02m");
    }
}
