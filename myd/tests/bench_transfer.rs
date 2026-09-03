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

/// A recursive copy must not fan out without bound.
///
/// Each level of `transfer_dir` opens its own window, so without a shared budget
/// a tree `d` levels deep reaches `max_parallel^d` simultaneous operations —
/// measured at 324 in-flight requests on a 4-level tree, well past what the
/// connection's request budget can absorb.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recursive_copy_fanout_stays_bounded() {
    // Enough latency that operations actually overlap (a zero-latency mock
    // completes each before the next starts, hiding any fan-out), and a tree
    // wide and deep enough for the per-level windows to multiply.
    let profile = LatencyProfile::with_rtt(Duration::from_millis(20));
    let config = TransferConfig {
        max_parallel: 8,
        ..TransferConfig::default()
    };
    let row = bench_tree("fanout", profile, 3, 4, 6, 16 * 1024, config).await;

    let peak = row.stats.max_concurrent_inflight.load(Ordering::Relaxed);
    let ceiling = myd::config::transfer_global_concurrency();
    // Without a shared budget this tree reaches max_parallel^depth concurrent
    // operations. Each in-flight file holds a couple of requests (open, then
    // read), so allow headroom over the raw permit count while still failing on
    // genuinely unbounded growth.
    // Each in-flight file may briefly hold more than one request (an open, then
    // a read), so the observed peak sits somewhat above the permit count. The
    // threshold is set from measurement: this tree peaks at ~224 with the shared
    // budget and ~384 without it, so 1.5x the ceiling separates the two.
    let limit = (ceiling as u64 * 3) / 2;
    assert!(
        peak <= limit,
        "recursive copy reached {peak} in-flight requests against a {ceiling} permit budget \
         (limit {limit}) — the per-level windows are multiplying ({})",
        row.stats.summary()
    );
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

/// The reported scenario: several items tagged and transferred at once over a
/// long link. Two things are measured that a single-transfer benchmark cannot
/// show — whether N concurrent transfers actually go faster than one, and what
/// the peak in-flight request count reaches when they share a connection.
///
/// `n_items` transfers are started together against one shared remote, exactly
/// as the queue starts them.
async fn bench_concurrent_items(
    scenario: &str,
    profile: LatencyProfile,
    n_items: usize,
    size_each: u64,
    config: TransferConfig,
) -> Row {
    let rtt_ms = profile.rtt.as_millis() as u64;
    let mut remote = LatencyVfs::new(profile);
    for i in 0..n_items {
        remote = remote.with_file(format!("/data/item{}.bin", i), size_each);
    }
    let remote = remote.shared();
    let stats = remote.stats();

    let dir = tempfile::tempdir().unwrap();
    let local: Arc<dyn Vfs> = Arc::new(LocalFs::new());

    let started = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..n_items {
        let src_fs: Arc<dyn Vfs> = remote.clone();
        let dest_fs = local.clone();
        let dest = dir.path().join(format!("item{}.bin", i));
        tasks.push(tokio::spawn(async move {
            run_transfer(TransferJob {
                id: TransferId(100 + i as u64),
                src_fs,
                dest_fs,
                src: VPath::new(BackendId(1), format!("/data/item{}.bin", i)),
                dest: VPath::local(dest),
                progress: Arc::new(TransferProgress::new(0)),
                cancel: CancelToken::new(),
                config,
            })
            .await
        }));
    }
    for t in tasks {
        t.await.unwrap().expect("transfer failed");
    }
    let elapsed = started.elapsed();

    for i in 0..n_items {
        let got = std::fs::read(dir.path().join(format!("item{}.bin", i))).unwrap();
        assert_eq!(got.len() as u64, size_each, "{scenario}: wrong length");
        assert!(verify_pattern(&got, 0), "{scenario}: content mismatch");
    }

    Row {
        scenario: scenario.to_string(),
        rtt_ms,
        bytes: size_each * n_items as u64,
        elapsed,
        stats,
    }
}

/// Ten tagged items over a France->USA link, against one item for reference.
///
/// If concurrency is working, ten items should take far less than ten times one
/// item. Aggregate throughput matching a single transfer is the symptom this
/// exists to catch.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_ten_concurrent_items() {
    let rtt = Duration::from_millis(150);
    let config = TransferConfig::default();

    println!("\n--- one item at a time, for reference ---");
    bench_concurrent_items("1 item x 8MiB", LatencyProfile::with_rtt(rtt), 1, 8 * MIB, config)
        .await
        .print();

    println!("\n--- ten items started together ---");
    bench_concurrent_items("10 items x 8MiB", LatencyProfile::with_rtt(rtt), 10, 8 * MIB, config)
        .await
        .print();

    // Small files take the sequential path, which is where the reported
    // "aggregate equals a single transfer" is most likely to show.
    println!("\n--- ten small items (under the 4MiB parallel threshold) ---");
    bench_concurrent_items("1 item x 1MiB", LatencyProfile::with_rtt(rtt), 1, MIB, config)
        .await
        .print();
    bench_concurrent_items("10 items x 1MiB", LatencyProfile::with_rtt(rtt), 10, MIB, config)
        .await
        .print();
}

/// As `bench_concurrent_items`, but each item is a directory tree — which is
/// what "10 files/directories" usually means in practice, and the case where
/// each top-level transfer opens its own tree-wide semaphore.
#[allow(clippy::too_many_arguments)]
async fn bench_concurrent_trees(
    scenario: &str,
    profile: LatencyProfile,
    n_items: usize,
    depth: usize,
    dirs: usize,
    files: usize,
    size: u64,
    config: TransferConfig,
) -> Row {
    let rtt_ms = profile.rtt.as_millis() as u64;
    let mut remote = LatencyVfs::new(profile);
    for i in 0..n_items {
        remote = remote.with_tree(format!("/t{}", i), depth, dirs, files, size);
    }
    let remote = remote.shared();
    let stats = remote.stats();

    let dir = tempfile::tempdir().unwrap();
    let local: Arc<dyn Vfs> = Arc::new(LocalFs::new());

    let started = Instant::now();
    let mut tasks = Vec::new();
    let mut progresses = Vec::new();
    for i in 0..n_items {
        let src_fs: Arc<dyn Vfs> = remote.clone();
        let dest_fs = local.clone();
        let dest = dir.path().join(format!("t{}", i));
        let progress = Arc::new(TransferProgress::new(0));
        progresses.push(progress.clone());
        tasks.push(tokio::spawn(async move {
            run_transfer(TransferJob {
                id: TransferId(200 + i as u64),
                src_fs,
                dest_fs,
                src: VPath::new(BackendId(1), format!("/t{}", i)),
                dest: VPath::local(dest),
                progress,
                cancel: CancelToken::new(),
                config,
            })
            .await
        }));
    }
    for t in tasks {
        t.await.unwrap().expect("tree transfer failed");
    }
    let elapsed = started.elapsed();

    Row {
        scenario: scenario.to_string(),
        rtt_ms,
        bytes: progresses.iter().map(|p| p.total_bytes()).sum(),
        elapsed,
        stats,
    }
}

/// Ten tagged *directories* over a long link. The per-transfer semaphore means
/// ten of these each open their own budget, so peak in-flight is the number to
/// watch: it should stay near the connection's request budget, not ten times it.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_ten_concurrent_trees() {
    let rtt = Duration::from_millis(150);
    let config = TransferConfig::default();

    println!("\n=== Concurrent directory transfers, rtt=150ms ===");
    for n in [1usize, 4, 10] {
        bench_concurrent_trees(
            &format!("{} trees (3 deep, 4 dirs, 8 files x 256KiB)", n),
            LatencyProfile::with_rtt(rtt),
            n,
            3,
            4,
            8,
            256 * 1024,
            config,
        )
        .await
        .print();
    }
}

/// The same transatlantic link, but with the SSH channel window myd actually
/// configures (64 MiB) rather than russh's 2 MiB default.
///
/// This separates two very different explanations for flat aggregate
/// throughput: a transport ceiling that no amount of concurrency can beat, and
/// a scheduling problem inside myd. With the window opened up, bandwidth
/// becomes the only ceiling, and concurrency should scale until it is reached.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_transatlantic_wide_window() {
    let rtt = Duration::from_millis(150);
    let bps = 200_000_000u64;
    let profile = || LatencyProfile {
        rtt,
        bandwidth_bps: Some(bps),
        channel_window: Some(64 * 1024 * 1024),
        ..Default::default()
    };
    let config = TransferConfig::default();
    let ceiling = bps as f64 / 8.0 / 1024.0 / 1024.0;

    println!("\n=== Transatlantic, myd's 64MiB window. Bandwidth ceiling {:.1} MiB/s ===", ceiling);
    for n in [1usize, 4, 10] {
        bench_concurrent_items(
            &format!("{} items x 8MiB", n),
            profile(),
            n,
            8 * MIB,
            config,
        )
        .await
        .print();
    }
}

/// The whole path a tagged batch actually takes: enqueue N items on a real
/// `TransferQueue` and tick it to completion, as the UI does each frame.
///
/// The direct-spawn benchmarks above bypass the queue, so they cannot show what
/// `max_parallel` does. This one can.
async fn bench_via_queue(
    scenario: &str,
    profile: LatencyProfile,
    n_items: usize,
    size_each: u64,
    max_parallel: usize,
) -> (Row, usize) {
    use myd::transfer::TransferQueue;
    use myd::vfs::BackendRegistry;

    let rtt_ms = profile.rtt.as_millis() as u64;
    let mut remote = LatencyVfs::new(profile);
    for i in 0..n_items {
        remote = remote.with_file(format!("/data/q{}.bin", i), size_each);
    }
    let remote = remote.shared();
    let stats = remote.stats();

    let mut registry = BackendRegistry::new();
    let remote_id = registry.register(remote.clone() as Arc<dyn Vfs>);

    let dir = tempfile::tempdir().unwrap();
    let mut queue = TransferQueue::new(TransferConfig {
        max_parallel,
        ..TransferConfig::default()
    });
    for i in 0..n_items {
        queue.enqueue(
            VPath::new(remote_id, format!("/data/q{}.bin", i)),
            VPath::local(dir.path().join(format!("q{}.bin", i))),
            size_each,
        );
    }

    let started = Instant::now();
    let mut peak_active = 0usize;
    loop {
        queue.tick(&registry);
        peak_active = peak_active.max(queue.active_count());
        if !queue.has_work() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let elapsed = started.elapsed();

    for i in 0..n_items {
        let got = std::fs::read(dir.path().join(format!("q{}.bin", i))).unwrap();
        assert_eq!(got.len() as u64, size_each, "{scenario}: wrong length");
    }

    (
        Row {
            scenario: scenario.to_string(),
            rtt_ms,
            bytes: size_each * n_items as u64,
            elapsed,
            stats,
        },
        peak_active,
    )
}

/// `max_parallel` through the real queue, on a bandwidth-limited long link.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_queue_max_parallel_curve() {
    let rtt = Duration::from_millis(150);
    let bps = 200_000_000u64;
    let profile = || LatencyProfile {
        rtt,
        bandwidth_bps: Some(bps),
        channel_window: Some(64 * 1024 * 1024),
        ..Default::default()
    };

    println!("\n=== 10 items x 8MiB through the queue (150ms, 200Mbit/s) ===");
    println!("    bandwidth ceiling = {:.1} MiB/s", bps as f64 / 8.0 / 1048576.0);
    for mp in [1usize, 2, 4, 6, 8, 12, 16] {
        let (row, peak) = bench_via_queue(
            &format!("max_parallel={:<2}", mp),
            profile(),
            10,
            8 * MIB,
            mp,
        )
        .await;
        row.print();
        println!("      peak concurrent transfers = {}", peak);
    }
}

/// Directory trees through the queue, which is the case the reported batch
/// actually was ("files/directories") and the one where the per-transfer
/// semaphore used to multiply the in-flight budget.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_queue_trees_inflight() {
    use myd::transfer::TransferQueue;
    use myd::vfs::BackendRegistry;

    for mp in [4usize, 16] {
        let n = 10usize;
        let remote = {
            let mut r = LatencyVfs::new(LatencyProfile {
                rtt: Duration::from_millis(150),
                bandwidth_bps: Some(200_000_000),
                channel_window: Some(64 * 1024 * 1024),
                ..Default::default()
            });
            for i in 0..n {
                r = r.with_tree(format!("/d{}", i), 2, 3, 6, 256 * 1024);
            }
            r.shared()
        };
        let stats = remote.stats();
        let mut registry = BackendRegistry::new();
        let id = registry.register(remote.clone() as Arc<dyn Vfs>);
        let dir = tempfile::tempdir().unwrap();
        let mut queue = TransferQueue::new(TransferConfig {
            max_parallel: mp,
            ..TransferConfig::default()
        });
        for i in 0..n {
            queue.enqueue_kind(
                VPath::new(id, format!("/d{}", i)),
                VPath::local(dir.path().join(format!("d{}", i))),
                0,
                true,
            );
        }

        let started = Instant::now();
        loop {
            queue.tick(&registry);
            if !queue.has_work() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let secs = started.elapsed().as_secs_f64();
        let bytes = stats.bytes_read.load(Ordering::Relaxed);
        println!(
            "trees max_parallel={:<3} {:.2}s  {:.2} MiB/s  reqs={}  peak_inflight={}",
            mp,
            secs,
            (bytes as f64 / 1048576.0) / secs,
            stats.total_requests(),
            stats.max_concurrent_inflight.load(Ordering::Relaxed),
        );
    }
}

/// The concurrency budget is process-wide, not per transfer.
///
/// Ten concurrent tree transfers used to open ten separate budgets, so the
/// "global" ceiling scaled with the number of transfers rather than bounding
/// them. Measured on this scenario in release mode: **528** peak in-flight
/// requests before the fix against **192** after, with aggregate throughput
/// unchanged — the oversubscription bought nothing.
///
/// `#[ignore]`d with the other benchmarks because the number it checks is a
/// timing measurement. Peak in-flight depends on how fast requests retire
/// relative to how fast they are issued, so it is only stable in release mode
/// with the simulated latency this uses. Run it the same way:
///
/// ```text
/// cargo test --release --test bench_transfer -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn the_concurrency_budget_is_shared_across_transfers() {
    use myd::transfer::TransferQueue;
    use myd::vfs::BackendRegistry;

    let n = 10usize;
    let remote = {
        let mut r = LatencyVfs::new(LatencyProfile {
            rtt: Duration::from_millis(150),
            bandwidth_bps: Some(200_000_000),
            channel_window: Some(64 * 1024 * 1024),
            ..Default::default()
        });
        for i in 0..n {
            r = r.with_tree(format!("/s{}", i), 2, 3, 6, 256 * 1024);
        }
        r.shared()
    };
    let stats = remote.stats();
    let mut registry = BackendRegistry::new();
    let id = registry.register(remote.clone() as Arc<dyn Vfs>);
    let dir = tempfile::tempdir().unwrap();

    let mut queue = TransferQueue::new(TransferConfig::default());
    for i in 0..n {
        queue.enqueue_kind(
            VPath::new(id, format!("/s{}", i)),
            VPath::local(dir.path().join(format!("s{}", i))),
            0,
            true,
        );
    }
    loop {
        queue.tick(&registry);
        if !queue.has_work() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    let peak = stats.max_concurrent_inflight.load(Ordering::Relaxed) as usize;
    let budget = myd::transfer::concurrency_budget();
    println!("peak in-flight = {} (budget {})", peak, budget);

    // Halfway between the two measured values: comfortably above the 192 the
    // fix produces, comfortably below the 528 it replaced.
    assert!(
        peak < 350,
        "peak in-flight {} is near the {} a per-transfer budget produces rather \
         than the {} a shared one does — the {} transfers appear to have opened \
         separate budgets",
        peak,
        528,
        budget,
        n
    );
}
