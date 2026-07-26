//! A latency-simulating backend for measuring transfer performance without a
//! network.
//!
//! The real bottlenecks on a long link are round trips, not bandwidth, and they
//! are invisible against a loopback sshd. This backend adds a configurable RTT to
//! every *wire* request and counts them, so a change can be shown to reduce round
//! trips rather than asserted to.
//!
//! The critical detail is [`LatencyProfile::max_read_len`]: a real SFTP server
//! caps one READ at its negotiated limit (256 KiB for OpenSSH), so a request for
//! more than that becomes several wire requests. Modelling that split is what
//! makes a client-side read loop that *serialises* those requests show up as a
//! low [`WireStats::max_concurrent_inflight`] instead of hiding behind a
//! plausible-looking total.
//!
//! Compiled unconditionally rather than under `#[cfg(test)]` so the benches and
//! the `myd-transfer` binary can drive it too.

use anyhow::{bail, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use super::{VEntry, VMetadata, VPath, VPositionedRead, VRead, VWrite, Vfs};
use crate::utils::sizes::{CancelToken, SizeCache};
use crate::widget::progress::OpProgress;

/// How the simulated link behaves.
#[derive(Debug, Clone, Copy)]
pub struct LatencyProfile {
    /// Delay applied to every wire request. One-way, applied once per request,
    /// which models a pipelined protocol: `n` concurrent requests still cost
    /// roughly one `rtt` in total, while `n` serial ones cost `n * rtt`.
    pub rtt: Duration,
    /// Optional serialisation delay on payload bytes. `None` = infinite bandwidth,
    /// which isolates round-trip effects.
    pub bandwidth_bps: Option<u64>,
    /// Server cap on one READ, as negotiated by `limits@openssh.com`. OpenSSH
    /// reports 256 KiB.
    pub max_read_len: usize,
    /// Server cap on one WRITE. OpenSSH reports 255 KiB.
    pub max_write_len: usize,
    /// Optional SSH-channel receive window. Models the bandwidth-delay product
    /// ceiling: with a window of `w` and round-trip `rtt`, throughput cannot
    /// exceed `w / rtt` no matter how deep the request pipeline is.
    pub channel_window: Option<usize>,
}

impl Default for LatencyProfile {
    fn default() -> Self {
        Self {
            rtt: Duration::from_millis(30),
            bandwidth_bps: None,
            max_read_len: 256 * 1024,
            max_write_len: 255 * 1024,
            channel_window: None,
        }
    }
}

impl LatencyProfile {
    /// A profile with the given round-trip time and otherwise OpenSSH-like caps.
    pub fn with_rtt(rtt: Duration) -> Self {
        Self {
            rtt,
            ..Default::default()
        }
    }

    /// Model the SSH channel window, which is what caps throughput on a long
    /// link regardless of request depth.
    pub fn with_channel_window(mut self, bytes: usize) -> Self {
        self.channel_window = Some(bytes);
        self
    }

    pub fn with_bandwidth(mut self, bits_per_sec: u64) -> Self {
        self.bandwidth_bps = Some(bits_per_sec);
        self
    }
}

/// Counts of what actually went over the wire.
#[derive(Debug, Default)]
pub struct WireStats {
    pub reads: AtomicU64,
    pub writes: AtomicU64,
    pub opens: AtomicU64,
    pub stats: AtomicU64,
    pub readdirs: AtomicU64,
    pub renames: AtomicU64,
    pub removes: AtomicU64,
    pub mkdirs: AtomicU64,
    pub bytes_read: AtomicU64,
    pub bytes_written: AtomicU64,
    /// Peak simultaneous in-flight wire requests. The headline number for
    /// pipeline-depth work: a window of 16 that internally serialises 4 requests
    /// per chunk peaks at 16, not 64.
    pub max_concurrent_inflight: AtomicU64,
    /// Current in-flight count; `max_concurrent_inflight` is its high-water mark.
    inflight: AtomicU64,
}

impl WireStats {
    /// Total wire requests of every kind.
    pub fn total_requests(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
            + self.writes.load(Ordering::Relaxed)
            + self.opens.load(Ordering::Relaxed)
            + self.stats.load(Ordering::Relaxed)
            + self.readdirs.load(Ordering::Relaxed)
            + self.renames.load(Ordering::Relaxed)
            + self.removes.load(Ordering::Relaxed)
            + self.mkdirs.load(Ordering::Relaxed)
    }

    fn enter(&self) {
        let now = self.inflight.fetch_add(1, Ordering::Relaxed) + 1;
        self.max_concurrent_inflight
            .fetch_max(now, Ordering::Relaxed);
    }

    fn leave(&self) {
        self.inflight.fetch_sub(1, Ordering::Relaxed);
    }

    /// A one-line summary for benchmark output.
    pub fn summary(&self) -> String {
        format!(
            "reads={} writes={} opens={} stats={} readdirs={} mkdirs={} renames={} removes={} \
             peak_inflight={} bytes_read={} bytes_written={}",
            self.reads.load(Ordering::Relaxed),
            self.writes.load(Ordering::Relaxed),
            self.opens.load(Ordering::Relaxed),
            self.stats.load(Ordering::Relaxed),
            self.readdirs.load(Ordering::Relaxed),
            self.mkdirs.load(Ordering::Relaxed),
            self.renames.load(Ordering::Relaxed),
            self.removes.load(Ordering::Relaxed),
            self.max_concurrent_inflight.load(Ordering::Relaxed),
            self.bytes_read.load(Ordering::Relaxed),
            self.bytes_written.load(Ordering::Relaxed),
        )
    }
}

/// One node in the simulated filesystem.
#[derive(Debug, Clone)]
struct Node {
    is_dir: bool,
    len: u64,
}

/// A backend that behaves like a remote filesystem on a slow link.
///
/// File contents are generated from a deterministic byte pattern rather than
/// stored, so a multi-gigabyte scenario costs no memory and a corrupted transfer
/// is still detectable.
pub struct LatencyVfs {
    profile: LatencyProfile,
    stats: Arc<WireStats>,
    nodes: Mutex<BTreeMap<PathBuf, Node>>,
    /// Bytes written by transfers, kept so a test can verify what landed.
    written: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    /// Models the SSH channel window as a byte budget.
    window: Option<Arc<tokio::sync::Semaphore>>,
    parallel_reads: bool,
    /// Weak self-reference, set by [`LatencyVfs::shared`]. The reader and writer
    /// streams need an owned handle to the backend that shares its stats; taking
    /// it from the same `Arc` the registry holds keeps the counters unified.
    self_ref: Mutex<Option<std::sync::Weak<LatencyVfs>>>,
}

/// The deterministic byte at a given offset. Position-dependent, so a transfer
/// that reorders or drops a chunk produces a mismatch rather than plausible data.
#[inline]
pub fn pattern_byte(offset: u64) -> u8 {
    (offset.wrapping_mul(2654435761) >> 13) as u8
}

/// Fill `buf` with the pattern starting at `offset`.
pub fn fill_pattern(buf: &mut [u8], offset: u64) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = pattern_byte(offset + i as u64);
    }
}

/// Whether `data` matches the pattern for a file starting at `offset`.
pub fn verify_pattern(data: &[u8], offset: u64) -> bool {
    data.iter()
        .enumerate()
        .all(|(i, b)| *b == pattern_byte(offset + i as u64))
}

impl LatencyVfs {
    pub fn new(profile: LatencyProfile) -> Self {
        let window = profile
            .channel_window
            .map(|w| Arc::new(tokio::sync::Semaphore::new(w)));
        let mut nodes = BTreeMap::new();
        nodes.insert(
            PathBuf::from("/"),
            Node {
                is_dir: true,
                len: 0,
            },
        );
        Self {
            profile,
            stats: Arc::new(WireStats::default()),
            nodes: Mutex::new(nodes),
            written: Mutex::new(BTreeMap::new()),
            window,
            parallel_reads: true,
            self_ref: Mutex::new(None),
        }
    }

    /// Turn off positioned-read support, to exercise the sequential path.
    pub fn without_parallel_reads(mut self) -> Self {
        self.parallel_reads = false;
        self
    }

    /// Add a file of `len` bytes, creating parent directories.
    pub fn with_file(self, path: impl Into<PathBuf>, len: u64) -> Self {
        let path = path.into();
        self.ensure_parents(&path);
        self.nodes
            .lock()
            .unwrap()
            .insert(path, Node { is_dir: false, len });
        self
    }

    pub fn with_dir(self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        self.ensure_parents(&path);
        self.nodes.lock().unwrap().insert(
            path,
            Node {
                is_dir: true,
                len: 0,
            },
        );
        self
    }

    /// Build a tree `depth` levels deep, each level holding `dirs`
    /// subdirectories and `files` files of `size` bytes.
    pub fn with_tree(
        self,
        root: impl Into<PathBuf>,
        depth: usize,
        dirs: usize,
        files: usize,
        size: u64,
    ) -> Self {
        let root = root.into();
        let mut this = self.with_dir(root.clone());
        this.build_level(&root, depth, dirs, files, size);
        this
    }

    fn build_level(&mut self, at: &PathBuf, depth: usize, dirs: usize, files: usize, size: u64) {
        for f in 0..files {
            let p = at.join(format!("file{}.bin", f));
            self.nodes.lock().unwrap().insert(
                p,
                Node {
                    is_dir: false,
                    len: size,
                },
            );
        }
        if depth == 0 {
            return;
        }
        for d in 0..dirs {
            let p = at.join(format!("dir{}", d));
            self.nodes.lock().unwrap().insert(
                p.clone(),
                Node {
                    is_dir: true,
                    len: 0,
                },
            );
            self.build_level(&p, depth - 1, dirs, files, size);
        }
    }

    fn ensure_parents(&self, path: &std::path::Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let mut cur = path.parent();
        while let Some(p) = cur {
            nodes.entry(p.to_path_buf()).or_insert(Node {
                is_dir: true,
                len: 0,
            });
            cur = p.parent();
        }
    }

    pub fn stats(&self) -> Arc<WireStats> {
        self.stats.clone()
    }

    /// Bytes a transfer wrote to `path`, if any.
    pub fn written(&self, path: &std::path::Path) -> Option<Vec<u8>> {
        self.written.lock().unwrap().get(path).cloned()
    }

    /// Simulate one wire request: count it, hold the in-flight gauge up, and wait
    /// out the link delay.
    async fn wire(&self, counter: &AtomicU64, bytes: u64) {
        counter.fetch_add(1, Ordering::Relaxed);
        self.stats.enter();

        // Hold channel-window credit for the duration of the round trip, so a
        // window smaller than the bandwidth-delay product throttles throughput
        // the way a real SSH channel does.
        let _permit = match (&self.window, u32::try_from(bytes).ok()) {
            (Some(sem), Some(n)) if n > 0 => sem.clone().acquire_many_owned(n).await.ok(),
            _ => None,
        };

        let mut delay = self.profile.rtt;
        if let Some(bps) = self.profile.bandwidth_bps {
            if bps > 0 && bytes > 0 {
                delay += Duration::from_secs_f64((bytes * 8) as f64 / bps as f64);
            }
        }
        tokio::time::sleep(delay).await;
        self.stats.leave();
    }

    fn node(&self, path: &std::path::Path) -> Option<Node> {
        self.nodes.lock().unwrap().get(path).cloned()
    }
}

/// A sequential reader over a simulated file.
struct LatencyRead {
    fs: Arc<LatencyVfs>,
    path: PathBuf,
    offset: u64,
    len: u64,
    pending: Option<Pin<Box<dyn std::future::Future<Output = Vec<u8>> + Send>>>,
}

impl tokio::io::AsyncRead for LatencyRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if let Some(fut) = self.pending.as_mut() {
                let data = std::task::ready!(fut.as_mut().poll(cx));
                self.pending = None;
                let n = data.len().min(buf.remaining());
                buf.put_slice(&data[..n]);
                self.offset += n as u64;
                return Poll::Ready(Ok(()));
            }
            if self.offset >= self.len || buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            // One wire request per server-capped chunk, exactly as a real
            // sequential read would issue.
            let want = buf
                .remaining()
                .min(self.fs.profile.max_read_len)
                .min((self.len - self.offset) as usize);
            let fs = self.fs.clone();
            let offset = self.offset;
            let _ = &self.path;
            self.pending = Some(Box::pin(async move {
                fs.wire(&fs.stats.reads, want as u64).await;
                fs.stats
                    .bytes_read
                    .fetch_add(want as u64, Ordering::Relaxed);
                let mut data = vec![0u8; want];
                fill_pattern(&mut data, offset);
                data
            }));
        }
    }
}

/// A sequential writer that accumulates bytes and charges one wire request per
/// server-capped write.
struct LatencyWrite {
    fs: Arc<LatencyVfs>,
    path: PathBuf,
    buf: Vec<u8>,
    pending: Option<Pin<Box<dyn std::future::Future<Output = ()> + Send>>>,
}

impl tokio::io::AsyncWrite for LatencyWrite {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if let Some(fut) = self.pending.as_mut() {
            std::task::ready!(fut.as_mut().poll(cx));
            self.pending = None;
        }
        let n = data.len().min(self.fs.profile.max_write_len);
        self.buf.extend_from_slice(&data[..n]);
        let fs = self.fs.clone();
        self.pending = Some(Box::pin(async move {
            fs.wire(&fs.stats.writes, n as u64).await;
            fs.stats.bytes_written.fetch_add(n as u64, Ordering::Relaxed);
        }));
        // Report progress immediately; the delay is absorbed by the next poll,
        // which is how a pipelining client behaves.
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Some(fut) = self.pending.as_mut() {
            std::task::ready!(fut.as_mut().poll(cx));
            self.pending = None;
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::task::ready!(self.as_mut().poll_flush(cx))?;
        let data = std::mem::take(&mut self.buf);
        let len = data.len() as u64;
        self.fs
            .written
            .lock()
            .unwrap()
            .insert(self.path.clone(), data);
        self.fs.nodes.lock().unwrap().insert(
            self.path.clone(),
            Node {
                is_dir: false,
                len,
            },
        );
        Poll::Ready(Ok(()))
    }
}

/// A positioned-read handle over the simulated file.
struct LatencyPositionedRead {
    fs: Arc<LatencyVfs>,
    len: u64,
}

#[async_trait]
impl VPositionedRead for LatencyPositionedRead {
    async fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if offset >= self.len {
            return Ok(Vec::new());
        }
        let avail = ((self.len - offset) as usize).min(len);

        // A real server caps one READ at its negotiated limit, so a request for
        // more than that becomes several wire requests — and the client crate
        // issues them one after another on a single handle, each waiting for the
        // previous reply.
        //
        // Modelling that serialisation is the entire point: it is what makes an
        // over-large chunk size show up as a *shallow* pipeline. A client asking
        // for one capped chunk at a time gets one request per `read_at` and can
        // overlap the whole window; one asking for 1 MiB against a 256 KiB limit
        // pays four sequential round trips inside every window slot.
        let mut out = Vec::with_capacity(avail);
        while out.len() < avail {
            let this_len = self.fs.profile.max_read_len.min(avail - out.len());
            let at = offset + out.len() as u64;
            self.fs.wire(&self.fs.stats.reads, this_len as u64).await;
            self.fs
                .stats
                .bytes_read
                .fetch_add(this_len as u64, Ordering::Relaxed);
            let start = out.len();
            out.resize(start + this_len, 0);
            fill_pattern(&mut out[start..], at);
        }
        Ok(out)
    }
}

#[async_trait]
impl Vfs for LatencyVfs {
    fn scheme(&self) -> &'static str {
        "latency"
    }

    fn display_name(&self) -> String {
        "latency-sim".to_string()
    }

    async fn read_dir(&self, path: &VPath) -> Result<Vec<VEntry>> {
        self.wire(&self.stats.readdirs, 0).await;
        let prefix = &path.path;
        let nodes = self.nodes.lock().unwrap();
        let mut out = Vec::new();
        for (p, n) in nodes.iter() {
            if p.parent() == Some(prefix.as_path()) {
                out.push(VEntry {
                    name: p
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    is_dir: n.is_dir,
                    is_symlink: false,
                    len: n.len,
                    mtime: None,
                    atime: None,
                    mode: None,
                    uid: None,
                    gid: None,
                });
            }
        }
        Ok(out)
    }

    async fn stat(&self, path: &VPath) -> Result<VMetadata> {
        self.wire(&self.stats.stats, 0).await;
        match self.node(&path.path) {
            Some(n) => Ok(VMetadata {
                is_dir: n.is_dir,
                len: n.len,
                ..Default::default()
            }),
            None => bail!("no such file: {}", path.path.display()),
        }
    }

    async fn create_dir_all(&self, path: &VPath) -> Result<()> {
        if self.node(&path.path).is_some() {
            return Ok(());
        }
        self.wire(&self.stats.mkdirs, 0).await;
        self.ensure_parents(&path.path);
        self.nodes.lock().unwrap().insert(
            path.path.clone(),
            Node {
                is_dir: true,
                len: 0,
            },
        );
        Ok(())
    }

    async fn remove_file(&self, path: &VPath) -> Result<()> {
        self.wire(&self.stats.removes, 0).await;
        match self.nodes.lock().unwrap().remove(&path.path) {
            Some(_) => Ok(()),
            None => bail!("no such file: {}", path.path.display()),
        }
    }

    async fn remove_dir(&self, path: &VPath) -> Result<()> {
        self.wire(&self.stats.removes, 0).await;
        self.nodes.lock().unwrap().remove(&path.path);
        Ok(())
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<()> {
        self.wire(&self.stats.renames, 0).await;
        let node = self.nodes.lock().unwrap().remove(&from.path);
        match node {
            Some(n) => {
                self.nodes.lock().unwrap().insert(to.path.clone(), n);
                let data = self.written.lock().unwrap().remove(&from.path);
                if let Some(d) = data {
                    self.written.lock().unwrap().insert(to.path.clone(), d);
                }
                Ok(())
            }
            None => bail!("no such file: {}", from.path.display()),
        }
    }

    async fn open_read(&self, path: &VPath) -> Result<Box<dyn VRead>> {
        self.wire(&self.stats.opens, 0).await;
        let node = self
            .node(&path.path)
            .ok_or_else(|| anyhow::anyhow!("no such file: {}", path.path.display()))?;
        Ok(Box::new(LatencyRead {
            fs: self.arc_self(),
            path: path.path.clone(),
            offset: 0,
            len: node.len,
            pending: None,
        }))
    }

    async fn open_write(&self, path: &VPath, _len_hint: Option<u64>) -> Result<Box<dyn VWrite>> {
        self.wire(&self.stats.opens, 0).await;
        Ok(Box::new(LatencyWrite {
            fs: self.arc_self(),
            path: path.path.clone(),
            buf: Vec::new(),
            pending: None,
        }))
    }

    fn supports_parallel_read(&self) -> bool {
        self.parallel_reads
    }

    async fn open_positioned_read(&self, path: &VPath) -> Result<Box<dyn VPositionedRead>> {
        self.wire(&self.stats.opens, 0).await;
        let node = self
            .node(&path.path)
            .ok_or_else(|| anyhow::anyhow!("no such file: {}", path.path.display()))?;
        Ok(Box::new(LatencyPositionedRead {
            fs: self.arc_self(),
            len: node.len,
        }))
    }

    async fn dir_size(
        &self,
        path: &VPath,
        _cache: &SizeCache,
        _cancel: &CancelToken,
        _progress: Option<&OpProgress>,
    ) -> u64 {
        self.node(&path.path).map(|n| n.len).unwrap_or(0)
    }

    fn has_recursive_sizes(&self) -> bool {
        false
    }
}

/// `LatencyVfs` hands `Arc<Self>` to the stream types it returns. The backend is
/// always held behind an `Arc` by the registry, so reconstructing one here would
/// split the stats; instead the instance keeps a weak self-reference set at
/// registration time.
impl LatencyVfs {
    fn arc_self(&self) -> Arc<LatencyVfs> {
        self.self_ref
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade())
            .expect("LatencyVfs must be created via `shared()`")
    }

    /// Build the backend behind an `Arc`, wiring the self-reference its streams
    /// need. Always construct through this rather than `Arc::new` directly.
    pub fn shared(self) -> Arc<LatencyVfs> {
        let arc = Arc::new(self);
        *arc.self_ref.lock().unwrap() = Some(Arc::downgrade(&arc));
        arc
    }
}
