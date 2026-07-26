//! Transfer performance benchmarks over a simulated high-latency link.
//!
//! These are `#[ignore]`d because they deliberately sleep: the whole point is to
//! model a link where round trips dominate. Run them explicitly:
//!
//! ```text
//! cargo test --release --test bench_transfer -- --ignored --nocapture
//! ```
//!
//! Each scenario prints wall time, throughput, wire-request counts and the peak
//! number of simultaneously in-flight requests. That last number is the one that
//! exposes a client which *looks* like it has a deep pipeline but serialises
//! internally: a 16-wide window over 1 MiB chunks against a 256 KiB server limit
//! should peak at 64 in-flight reads, not 16.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use myd::transfer::{
    run_transfer, TransferConfig, TransferId, TransferJob, TransferOutcome, TransferProgress,
};
use myd::utils::sizes::CancelToken;
use myd::vfs::testing::{verify_pattern, LatencyProfile, LatencyVfs, WireStats};
use myd::vfs::{BackendId, LocalFs, VPath, Vfs};

const MIB: u64 = 1024 * 1024;

/// One benchmark result, printed as a single aligned row.
struct Row {
    scenario: String,
    rtt_ms: u64,
    bytes: u64,
    elapsed: Duration,
    stats: Arc<WireStats>,
}

impl Row {
    fn print(&self) {
        let secs = self.elapsed.as_secs_f64();
        let mbps = if secs > 0.0 {
            (self.bytes as f64 / (1024.0 * 1024.0)) / secs
        } else {
            f64::INFINITY
        };
        println!(
            "{:<34} rtt={:>4}ms  {:>8.2}s  {:>9.2} MiB/s  reqs={:<6} peak_inflight={:<4}",
            self.scenario,
            self.rtt_ms,
            secs,
            mbps,
            self.stats.total_requests(),
            self.stats.max_concurrent_inflight.load(Ordering::Relaxed),
        );
        println!("      {}", self.stats.summary());
    }
}

/// Download `remote_path` to a local temp dir over a simulated link.
async fn bench_download(
    scenario: &str,
    profile: LatencyProfile,
    size: u64,
    config: TransferConfig,
) -> Row {
    let rtt_ms = profile.rtt.as_millis() as u64;
    let remote = LatencyVfs::new(profile)
        .with_file("/data/big.bin", size)
        .shared();
    let stats = remote.stats();

    let dir = tempfile::tempdir().unwrap();
    let local: Arc<dyn Vfs> = Arc::new(LocalFs::new());
    let src_fs: Arc<dyn Vfs> = remote.clone();

    let progress = Arc::new(TransferProgress::new(0));
    let started = Instant::now();
    let outcome = run_transfer(TransferJob {
        id: TransferId(1),
        src_fs,
        dest_fs: local,
        src: VPath::new(BackendId(1), "/data/big.bin"),
        dest: VPath::local(dir.path().join("big.bin")),
        progress,
        cancel: CancelToken::new(),
        config,
    })
    .await
    .expect("transfer failed");
    let elapsed = started.elapsed();
    assert!(matches!(outcome, TransferOutcome::Done));

    // Byte-exactness matters as much as speed: a pipeline bug that drops or
    // reorders a chunk would otherwise look like a win.
    let got = std::fs::read(dir.path().join("big.bin")).unwrap();
    assert_eq!(got.len() as u64, size, "{scenario}: wrong length");
    assert!(verify_pattern(&got, 0), "{scenario}: content mismatch");

    Row {
        scenario: scenario.to_string(),
        rtt_ms,
        bytes: size,
        elapsed,
        stats,
    }
}

/// Upload a local temp file to the simulated remote.
async fn bench_upload(
    scenario: &str,
    profile: LatencyProfile,
    size: u64,
    config: TransferConfig,
) -> Row {
    let rtt_ms = profile.rtt.as_millis() as u64;
    let remote = LatencyVfs::new(profile).with_dir("/dest").shared();
    let stats = remote.stats();

    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("up.bin");
    let mut data = vec![0u8; size as usize];
    myd::vfs::testing::fill_pattern(&mut data, 0);
    std::fs::write(&src_path, &data).unwrap();

    let local: Arc<dyn Vfs> = Arc::new(LocalFs::new());
    let dest_fs: Arc<dyn Vfs> = remote.clone();

    let progress = Arc::new(TransferProgress::new(0));
    let started = Instant::now();
    run_transfer(TransferJob {
        id: TransferId(2),
        src_fs: local,
        dest_fs,
        src: VPath::local(&src_path),
        dest: VPath::new(BackendId(1), "/dest/up.bin"),
        progress,
        cancel: CancelToken::new(),
        config,
    })
    .await
    .expect("upload failed");
    let elapsed = started.elapsed();

    let landed = remote
        .written(std::path::Path::new("/dest/up.bin"))
        .expect("nothing written at destination");
    assert_eq!(landed.len() as u64, size, "{scenario}: wrong length");
    assert!(verify_pattern(&landed, 0), "{scenario}: content mismatch");

    Row {
        scenario: scenario.to_string(),
        rtt_ms,
        bytes: size,
        elapsed,
        stats,
    }
}

/// Download a whole directory tree.
async fn bench_tree(
    scenario: &str,
    profile: LatencyProfile,
    depth: usize,
    dirs: usize,
    files: usize,
    size: u64,
    config: TransferConfig,
) -> Row {
    let rtt_ms = profile.rtt.as_millis() as u64;
    let remote = LatencyVfs::new(profile)
        .with_tree("/tree", depth, dirs, files, size)
        .shared();
    let stats = remote.stats();

    let dir = tempfile::tempdir().unwrap();
    let local: Arc<dyn Vfs> = Arc::new(LocalFs::new());
    let src_fs: Arc<dyn Vfs> = remote.clone();

    let progress = Arc::new(TransferProgress::new(0));
    let started = Instant::now();
    run_transfer(TransferJob {
        id: TransferId(3),
        src_fs,
        dest_fs: local,
        src: VPath::new(BackendId(1), "/tree"),
        dest: VPath::local(dir.path().join("tree")),
        progress: progress.clone(),
        cancel: CancelToken::new(),
        config,
    })
    .await
    .expect("tree transfer failed");
    let elapsed = started.elapsed();

    Row {
        scenario: scenario.to_string(),
        rtt_ms,
        bytes: progress.total_bytes(),
        elapsed,
        stats,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "simulates a slow link; run explicitly with --ignored"]
async fn bench_large_file_download() {
    println!("\n=== Large-file download (64 MiB) ===");
    for rtt in [1u64, 30, 150] {
        let profile = LatencyProfile::with_rtt(Duration::from_millis(rtt));
        bench_download(
            "download 64MiB",
            profile,
            64 * MIB,
            TransferConfig::default(),
        )
        .await
        .print();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "simulates a slow link; run explicitly with --ignored"]
async fn bench_large_file_download_windowed() {
    // With a 2 MiB SSH channel window (russh's default) throughput cannot exceed
    // window/rtt however deep the request pipeline is: at 150ms that is ~13 MiB/s.
    println!("\n=== Large-file download, 2 MiB channel window (russh default) ===");
    for rtt in [30u64, 150] {
        let profile =
            LatencyProfile::with_rtt(Duration::from_millis(rtt)).with_channel_window(2 * MIB as usize);
        bench_download(
            "download 64MiB w/2MiB window",
            profile,
            64 * MIB,
            TransferConfig::default(),
        )
        .await
        .print();
    }
    println!("\n=== Same, 64 MiB channel window ===");
    for rtt in [30u64, 150] {
        let profile = LatencyProfile::with_rtt(Duration::from_millis(rtt))
            .with_channel_window(64 * MIB as usize);
        bench_download(
            "download 64MiB w/64MiB window",
            profile,
            64 * MIB,
            TransferConfig::default(),
        )
        .await
        .print();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "simulates a slow link; run explicitly with --ignored"]
async fn bench_large_file_upload() {
    println!("\n=== Large-file upload (16 MiB) ===");
    for rtt in [1u64, 30, 150] {
        let profile = LatencyProfile::with_rtt(Duration::from_millis(rtt));
        bench_upload("upload 16MiB", profile, 16 * MIB, TransferConfig::default())
            .await
            .print();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "simulates a slow link; run explicitly with --ignored"]
async fn bench_many_small_files() {
    println!("\n=== 200 x 64 KiB files (round-trip bound) ===");
    for rtt in [30u64, 150] {
        let profile = LatencyProfile::with_rtt(Duration::from_millis(rtt));
        bench_tree(
            "200 small files",
            profile,
            0,
            0,
            200,
            64 * 1024,
            TransferConfig::default(),
        )
        .await
        .print();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "simulates a slow link; run explicitly with --ignored"]
async fn bench_deep_tree() {
    println!("\n=== Deep tree (4 levels x 3 dirs x 4 files x 256 KiB) ===");
    for rtt in [30u64, 150] {
        let profile = LatencyProfile::with_rtt(Duration::from_millis(rtt));
        bench_tree(
            "deep tree",
            profile,
            4,
            3,
            4,
            256 * 1024,
            TransferConfig::default(),
        )
        .await
        .print();
    }
}

/// The upload counterpart: writes must overlap too.
///
/// Keying the path choice on the *source* backend alone sent every upload down
/// the sequential path, where one write is outstanding at a time — so an upload
/// paid a full round trip per chunk and ran an order of magnitude below the
/// download rate. This pins the destination side of that decision.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_upload_keeps_the_pipeline_full() {
    let profile = LatencyProfile::with_rtt(Duration::from_millis(1));
    let config = TransferConfig::default();
    let row = bench_upload("upload pipeline depth", profile, 8 * MIB, config).await;

    let peak = row.stats.max_concurrent_inflight.load(Ordering::Relaxed);
    assert!(
        peak > 1,
        "uploads are serialising: peak in-flight writes = {peak} ({})",
        row.stats.summary()
    );
}

/// The regression guard: a large-file download must keep many reads in flight.
///
/// This is the assertion that would have caught the serialised read loop, and it
/// runs by default (fast, 1 ms RTT) rather than being gated behind `--ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_download_keeps_the_pipeline_full() {
    let profile = LatencyProfile::with_rtt(Duration::from_millis(1));
    let config = TransferConfig::default();
    let row = bench_download("pipeline depth", profile, 16 * MIB, config).await;

    let peak = row.stats.max_concurrent_inflight.load(Ordering::Relaxed);
    let window = myd::transfer::large_file_chunks_in_flight(&config) as u64;

    // Every window slot must contribute a *wire* request that can overlap the
    // others. A chunk larger than the server's read limit is split into several
    // requests, and if the client issues those serially the whole window is only
    // as deep as its slot count — the pipeline stalls at `window` however many
    // bytes each slot asked for.
    //
    // Asserting `peak >= window` alone would not catch that, because the broken
    // client hits exactly `window`. The requirement is that no slot serialises
    // internally: with a chunk size at or below the server limit, peak depth
    // equals the window; with an over-large chunk size it must still reach
    // `window * sub_reads_per_chunk`.
    let sub_reads = (config.chunk_size as u64).div_ceil(LatencyProfile::default().max_read_len as u64);
    let expected = window * sub_reads;
    assert!(
        peak >= expected,
        "expected {expected} concurrent wire reads ({window} slots x {sub_reads} requests per \
         {}-byte chunk), saw {peak} — a window slot is issuing its reads serially ({})",
        config.chunk_size,
        row.stats.summary()
    );
}
