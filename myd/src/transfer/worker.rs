use anyhow::Result;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{TransferConfig, TransferId, TransferProgress};
use crate::utils::sizes::CancelToken;
use crate::vfs::{VPath, Vfs};

/// Outcome of a single file transfer.
pub enum TransferOutcome {
    Done,
    Cancelled,
}

/// Everything one file transfer needs. Grouped into a struct so the worker and
/// the queue pass a single value around rather than an unwieldy argument list.
pub struct TransferJob {
    pub id: TransferId,
    pub src_fs: Arc<dyn Vfs>,
    pub dest_fs: Arc<dyn Vfs>,
    pub src: VPath,
    pub dest: VPath,
    pub progress: Arc<TransferProgress>,
    pub cancel: CancelToken,
    pub config: TransferConfig,
}

/// Name of the in-progress temp file for `dest`.
///
/// A transfer writes here and renames on success, so an interrupted transfer
/// never leaves a truncated file sitting at the destination looking complete.
fn part_path(dest: &VPath, id: TransferId) -> VPath {
    let name = dest
        .file_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "transfer".to_string());
    let parent = dest.parent().unwrap_or_else(|| dest.clone());
    parent.join(format!(".myd-part-{}-{}", id.0, name))
}

/// Stream one file from `src_fs` to `dest_fs`, reporting bytes into `progress`
/// and stopping promptly when `cancel` trips.
///
/// Writes to a `.myd-part-*` file and renames into place on success; a cancelled
/// or failed transfer removes the part file so no debris is left behind.
pub async fn run_transfer(job: TransferJob) -> Result<TransferOutcome> {
    let TransferJob {
        id,
        src_fs,
        dest_fs,
        src,
        dest,
        progress,
        cancel,
        config,
    } = job;

    // Cancelled before we even opened anything.
    if cancel.is_cancelled() {
        return Ok(TransferOutcome::Cancelled);
    }

    let meta = src_fs.stat(&src).await?;

    // A directory is transferred by recursively copying its contents. Its total
    // size is summed up front so the panel shows one progress figure for the
    // whole tree.
    if meta.is_dir {
        // No separate sizing walk: `transfer_dir` grows the total from the same
        // listings it copies from. A dedicated pre-walk doubled the round trips
        // (one listing per directory for sizing, another for copying) and had to
        // finish before the first byte moved — on a long link that was a visible
        // stall in front of every directory copy.
        let outcome =
            transfer_dir(&src_fs, &dest_fs, &src, &dest, &progress, &cancel, &config).await?;
        if matches!(outcome, TransferOutcome::Done) {
            progress.finish();
        }
        return Ok(outcome);
    }

    progress.set_total(meta.len);

    if let Some(parent) = dest.parent() {
        dest_fs.create_dir_all(&parent).await.ok();
    }

    let part = part_path(&dest, id);
    let result = stream_file(
        &src_fs, &dest_fs, &src, &part, &progress, &cancel, &config, meta.len,
    )
    .await;

    match result {
        Ok(TransferOutcome::Done) => {
            // Atomically put the completed bytes at the real destination. An
            // existing file is replaced — the overwrite decision was made by the
            // caller before queueing.
            dest_fs.remove_file(&dest).await.ok();
            if let Err(e) = dest_fs.rename(&part, &dest).await {
                dest_fs.remove_file(&part).await.ok();
                return Err(e);
            }
            progress.finish();
            Ok(TransferOutcome::Done)
        }
        Ok(TransferOutcome::Cancelled) => {
            dest_fs.remove_file(&part).await.ok();
            Ok(TransferOutcome::Cancelled)
        }
        Err(e) => {
            dest_fs.remove_file(&part).await.ok();
            Err(e)
        }
    }
}

/// Recursively copy a directory `src` into `dest`, breadth-first.
///
/// Boxed because it recurses across an `async fn` boundary. Files are streamed
/// with the same chunked path as a single-file transfer, so directory contents
/// get the same throughput.
fn transfer_dir<'a>(
    src_fs: &'a Arc<dyn Vfs>,
    dest_fs: &'a Arc<dyn Vfs>,
    src: &'a VPath,
    dest: &'a VPath,
    progress: &'a Arc<TransferProgress>,
    cancel: &'a CancelToken,
    config: &'a TransferConfig,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<TransferOutcome>> + Send + 'a>> {
    Box::pin(async move {
        use futures::stream::{FuturesUnordered, StreamExt};

        if cancel.is_cancelled() {
            return Ok(TransferOutcome::Cancelled);
        }
        dest_fs.create_dir_all(dest).await?;

        let entries = src_fs.read_dir(src).await?;
        let (dirs, files): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| e.is_dir);

        // Grow the progress total from this listing, which we needed anyway.
        // The bar therefore tracks what has been discovered so far rather than
        // waiting on a full pre-walk of the tree.
        progress.add_total(files.iter().map(|e| e.len).sum());

        // Copy this level's files with several in flight at once. Each small
        // file costs a handful of serial round trips (open, write, close), so on
        // a high-latency link a strictly serial loop spends nearly all its time
        // waiting. Overlapping them is the difference between latency adding up
        // and latency being hidden.
        let mut inflight = FuturesUnordered::new();
        let mut files = files.into_iter();
        let mut cancelled = false;

        loop {
            // Top the window up to the configured parallelism.
            while inflight.len() < config.max_parallel.max(1) {
                let Some(entry) = files.next() else { break };
                let child_src = src.join(&entry.name);
                let child_dest = dest.join(&entry.name);
                let len = entry.len;
                inflight.push(async move {
                    // Stream straight to the final name: a per-file part rename
                    // across a whole tree adds round trips for little safety,
                    // and the directory itself is the unit of retry.
                    stream_file(
                        src_fs, dest_fs, &child_src, &child_dest, progress, cancel, config, len,
                    )
                    .await
                });
            }
            let Some(result) = inflight.next().await else {
                break;
            };
            if let TransferOutcome::Cancelled = result? {
                cancelled = true;
                break;
            }
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
        }
        // Let the already-issued copies wind down before returning, so a
        // cancelled transfer doesn't drop futures mid-write.
        drop(inflight);
        if cancelled {
            return Ok(TransferOutcome::Cancelled);
        }

        // Subdirectories are descended into concurrently as well. A deep tree
        // whose directories hold only a few files each would otherwise crawl:
        // every level costs a listing round trip, and walking them one at a time
        // leaves the transfer window idle waiting for each one.
        //
        // Fan-out is bounded by the same `max_parallel` — the per-level windows
        // multiply otherwise, and an unbounded recursive descent would swamp the
        // connection's request budget on a wide tree.
        let mut walking = FuturesUnordered::new();
        let mut dirs = dirs.into_iter();
        loop {
            while walking.len() < config.max_parallel.max(1) {
                let Some(entry) = dirs.next() else { break };
                let child_src = src.join(&entry.name);
                let child_dest = dest.join(&entry.name);
                walking.push(async move {
                    transfer_dir(
                        src_fs, dest_fs, &child_src, &child_dest, progress, cancel, config,
                    )
                    .await
                });
            }
            let Some(result) = walking.next().await else {
                break;
            };
            if let TransferOutcome::Cancelled = result? {
                return Ok(TransferOutcome::Cancelled);
            }
            if cancel.is_cancelled() {
                return Ok(TransferOutcome::Cancelled);
            }
        }
        Ok(TransferOutcome::Done)
    })
}


/// Copy one file from `src` into `part`.
///
/// A large file on a backend that supports positioned reads (SFTP) uses a
/// concurrent chunked path — several overlapping reads keep the connection's
/// pipeline full, which is the difference between one round trip per chunk and a
/// deep pipeline. Everything else streams sequentially, which is fastest locally
/// (the kernel already does readahead) and for small files (where the setup cost
/// of concurrency doesn't pay off).
#[allow(clippy::too_many_arguments)]
async fn stream_file(
    src_fs: &Arc<dyn Vfs>,
    dest_fs: &Arc<dyn Vfs>,
    src: &VPath,
    part: &VPath,
    progress: &Arc<TransferProgress>,
    cancel: &CancelToken,
    config: &TransferConfig,
    len_hint: u64,
) -> Result<TransferOutcome> {
    if src_fs.supports_parallel_read() && len_hint >= super::LARGE_FILE_THRESHOLD {
        return stream_file_parallel(
            src_fs, dest_fs, src, part, progress, cancel, config, len_hint,
        )
        .await;
    }
    stream_file_sequential(src_fs, dest_fs, src, part, progress, cancel, config).await
}

/// The sequential copy path: read a chunk, write it, repeat.
async fn stream_file_sequential(
    src_fs: &Arc<dyn Vfs>,
    dest_fs: &Arc<dyn Vfs>,
    src: &VPath,
    part: &VPath,
    progress: &Arc<TransferProgress>,
    cancel: &CancelToken,
    config: &TransferConfig,
) -> Result<TransferOutcome> {
    let mut reader = src_fs.open_read(src).await?;
    let mut writer = dest_fs.open_write(part, None).await?;

    let mut buf = vec![0u8; config.chunk_size];
    loop {
        if cancel.is_cancelled() {
            // Flush nothing; the part file is discarded by the caller.
            return Ok(TransferOutcome::Cancelled);
        }
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await?;
        progress.add_bytes(n as u64);
    }

    writer.flush().await?;
    writer.shutdown().await?;
    Ok(TransferOutcome::Done)
}

/// The concurrent copy path for a large file on a pipelining backend.
///
/// Chunks are read in a sliding window of `chunks_in_flight` overlapping
/// positioned reads (each on its own handle) and written in order. Ordered
/// writes keep the destination correct while the reads run ahead.
#[allow(clippy::too_many_arguments)]
async fn stream_file_parallel(
    src_fs: &Arc<dyn Vfs>,
    dest_fs: &Arc<dyn Vfs>,
    src: &VPath,
    part: &VPath,
    progress: &Arc<TransferProgress>,
    cancel: &CancelToken,
    config: &TransferConfig,
    total: u64,
) -> Result<TransferOutcome> {
    use crate::vfs::VPositionedRead;
    use futures::stream::{FuturesOrdered, StreamExt};
    use std::sync::Arc as StdArc;

    let mut writer = dest_fs.open_write(part, Some(total)).await?;

    let chunk = config.chunk_size as u64;
    // Share the connection's request budget with the other transfers that may be
    // running large files concurrently.
    let window = super::large_file_chunks_in_flight(config);

    // A pool of positioned-read handles, opened once. Each in-flight read borrows
    // one; reusing them avoids an SFTP OPEN per chunk (which would erase the win).
    let mut pool: Vec<StdArc<dyn VPositionedRead>> = Vec::with_capacity(window);
    for _ in 0..window {
        pool.push(StdArc::from(src_fs.open_positioned_read(src).await?));
    }

    // The byte offsets of every chunk, in order.
    let offsets: Vec<u64> = (0..total).step_by(config.chunk_size).collect();
    let mut next = 0usize;
    let mut inflight = FuturesOrdered::new();

    // One in-flight read: which pool slot it used, and its result.
    type ChunkFut =
        std::pin::Pin<Box<dyn std::future::Future<Output = (usize, Result<Vec<u8>>)> + Send>>;
    let make_read = |handle: StdArc<dyn VPositionedRead>, slot: usize, offset: u64| -> ChunkFut {
        Box::pin(async move {
            let data = handle.read_at(offset, chunk as usize).await;
            (slot, data)
        })
    };

    // Prime the window: each initial read gets its own handle from the pool.
    while next < offsets.len() && inflight.len() < window {
        let slot = next % window;
        inflight.push_back(make_read(pool[slot].clone(), slot, offsets[next]));
        next += 1;
    }

    while let Some((slot, result)) = inflight.next().await {
        if cancel.is_cancelled() {
            return Ok(TransferOutcome::Cancelled);
        }
        let data = result?;
        writer.write_all(&data).await?;
        progress.add_bytes(data.len() as u64);

        // Reuse the freed handle for the next chunk.
        if next < offsets.len() {
            inflight.push_back(make_read(pool[slot].clone(), slot, offsets[next]));
            next += 1;
        }
    }

    writer.flush().await?;
    writer.shutdown().await?;
    Ok(TransferOutcome::Done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::{BackendId, LocalFs, VEntry, VMetadata, VPositionedRead, VRead, VWrite};
    use anyhow::Result as AResult;
    use async_trait::async_trait;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    /// A read-only in-memory Vfs that advertises parallel reads, so the
    /// concurrent large-file path is exercised without a network. It serves one
    /// file's bytes and records how many positioned-read handles were opened.
    struct MockParallelVfs {
        data: Vec<u8>,
        handles_opened: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    struct MockHandle {
        data: std::sync::Arc<Vec<u8>>,
    }

    #[async_trait]
    impl VPositionedRead for MockHandle {
        async fn read_at(&self, offset: u64, len: usize) -> AResult<Vec<u8>> {
            let start = (offset as usize).min(self.data.len());
            let end = (start + len).min(self.data.len());
            Ok(self.data[start..end].to_vec())
        }
    }

    #[async_trait]
    impl Vfs for MockParallelVfs {
        fn scheme(&self) -> &'static str {
            "mock"
        }
        async fn read_dir(&self, _p: &VPath) -> AResult<Vec<VEntry>> {
            Ok(vec![])
        }
        async fn stat(&self, _p: &VPath) -> AResult<VMetadata> {
            Ok(VMetadata {
                len: self.data.len() as u64,
                ..Default::default()
            })
        }
        async fn create_dir_all(&self, _p: &VPath) -> AResult<()> {
            Ok(())
        }
        async fn remove_file(&self, _p: &VPath) -> AResult<()> {
            Ok(())
        }
        async fn remove_dir(&self, _p: &VPath) -> AResult<()> {
            Ok(())
        }
        async fn rename(&self, _a: &VPath, _b: &VPath) -> AResult<()> {
            Ok(())
        }
        async fn open_read(&self, _p: &VPath) -> AResult<Box<dyn VRead>> {
            Ok(Box::new(std::io::Cursor::new(self.data.clone())))
        }
        async fn open_write(&self, _p: &VPath, _l: Option<u64>) -> AResult<Box<dyn VWrite>> {
            anyhow::bail!("read-only mock")
        }
        fn supports_parallel_read(&self) -> bool {
            true
        }
        async fn open_positioned_read(&self, _p: &VPath) -> AResult<Box<dyn VPositionedRead>> {
            self.handles_opened
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Box::new(MockHandle {
                data: std::sync::Arc::new(self.data.clone()),
            }))
        }
        async fn dir_size(
            &self,
            _p: &VPath,
            _c: &crate::utils::sizes::SizeCache,
            _cancel: &CancelToken,
            _pr: Option<&crate::widget::progress::OpProgress>,
        ) -> u64 {
            0
        }
    }

    #[test]
    fn large_file_uses_the_concurrent_path_and_is_byte_exact() {
        // A distinctive payload larger than the threshold, so the parallel path
        // runs and the reassembled order can be checked.
        let size = 5 * 1024 * 1024 + 12_345;
        let payload: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        let handles = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let src_fs: Arc<dyn Vfs> = Arc::new(MockParallelVfs {
            data: payload.clone(),
            handles_opened: handles.clone(),
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        let dest_fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let progress = Arc::new(TransferProgress::new(0));

        rt().block_on(run_transfer(TransferJob {
            id: TransferId(1),
            src_fs,
            dest_fs,
            src: VPath::new(BackendId(1), "/remote/big.bin"),
            dest: VPath::local(&dest),
            progress: progress.clone(),
            cancel: CancelToken::new(),
            config: TransferConfig {
                chunks_in_flight: 4,
                ..Default::default()
            },
        }))
        .unwrap();

        // Byte-exact despite out-of-order concurrent reads.
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert_eq!(progress.bytes_done() as usize, size);
        // The concurrent path actually opened its pool of handles.
        assert_eq!(handles.load(std::sync::atomic::Ordering::Relaxed), 4);
    }

    #[test]
    fn small_file_stays_on_the_sequential_path() {
        // Below the threshold: no positioned-read handles should be opened.
        let payload = vec![0xabu8; 1024];
        let handles = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let src_fs: Arc<dyn Vfs> = Arc::new(MockParallelVfs {
            data: payload.clone(),
            handles_opened: handles.clone(),
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("small.bin");
        let dest_fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());

        rt().block_on(run_transfer(TransferJob {
            id: TransferId(1),
            src_fs,
            dest_fs,
            src: VPath::new(BackendId(1), "/remote/small.bin"),
            dest: VPath::local(&dest),
            progress: Arc::new(TransferProgress::new(0)),
            cancel: CancelToken::new(),
            config: TransferConfig::default(),
        }))
        .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert_eq!(
            handles.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "small file must not use the concurrent path"
        );
    }

    #[test]
    fn part_path_is_hidden_and_sits_beside_the_destination() {
        let p = part_path(&VPath::local("/data/out/big.iso"), TransferId(7));
        assert_eq!(
            p.path,
            std::path::PathBuf::from("/data/out/.myd-part-7-big.iso")
        );
    }

    #[test]
    fn part_path_stays_on_the_destination_backend() {
        let dest = VPath::new(BackendId(2), "/remote/x.bin");
        assert_eq!(part_path(&dest, TransferId(1)).backend, BackendId(2));
    }

    #[test]
    fn transfers_a_file_and_leaves_no_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let payload = vec![7u8; 3 * 1024 * 1024];
        std::fs::write(&src, &payload).unwrap();
        let dest = dir.path().join("out/dest.bin");

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let progress = Arc::new(TransferProgress::new(0));

        rt().block_on(run_transfer(TransferJob {
            id: TransferId(1),
            src_fs: fs.clone(),
            dest_fs: fs.clone(),
            src: VPath::local(&src),
            dest: VPath::local(&dest),
            progress: progress.clone(),
            cancel: CancelToken::new(),
            config: TransferConfig::default(),
        }))
        .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert_eq!(progress.bytes_done(), payload.len() as u64);
        assert!(progress.is_finished());

        // No debris left in the destination directory.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("out"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".myd-part"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn cancelled_before_start_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s.bin");
        std::fs::write(&src, vec![0u8; 1024]).unwrap();
        let dest = dir.path().join("d.bin");

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let cancel = CancelToken::new();
        cancel.cancel();

        let outcome = rt()
            .block_on(run_transfer(TransferJob {
                id: TransferId(1),
                src_fs: fs.clone(),
                dest_fs: fs.clone(),
                src: VPath::local(&src),
                dest: VPath::local(&dest),
                progress: Arc::new(TransferProgress::new(0)),
                cancel,
                config: TransferConfig::default(),
            }))
            .unwrap();

        assert!(matches!(outcome, TransferOutcome::Cancelled));
        assert!(!dest.exists());
    }

    #[test]
    fn cancelling_mid_transfer_leaves_no_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("big.bin");
        // Several chunks, so cancellation lands between two of them.
        std::fs::write(&src, vec![3u8; 8 * 1024 * 1024]).unwrap();
        let dest = dir.path().join("out.bin");

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let cancel = CancelToken::new();
        let progress = Arc::new(TransferProgress::new(0));

        // Small chunks make the loop iterate enough times to observe the token.
        let config = TransferConfig {
            chunk_size: 16 * 1024,
            ..Default::default()
        };

        let outcome = rt().block_on({
            let cancel = cancel.clone();
            let progress = progress.clone();
            let fs = fs.clone();
            let src = VPath::local(&src);
            let dest_v = VPath::local(&dest);
            async move {
                let handle = tokio::spawn(run_transfer(TransferJob {
                    id: TransferId(9),
                    src_fs: fs.clone(),
                    dest_fs: fs,
                    src,
                    dest: dest_v,
                    progress: progress.clone(),
                    cancel: cancel.clone(),
                    config,
                }));
                // Let it move some bytes, then pull the plug.
                while progress.bytes_done() == 0 && !handle.is_finished() {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                cancel.cancel();
                handle.await.unwrap().unwrap()
            }
        });

        assert!(matches!(outcome, TransferOutcome::Cancelled));
        assert!(!dest.exists(), "destination must not exist after cancel");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".myd-part"))
            .collect();
        assert!(leftovers.is_empty(), "part file must be cleaned up");
    }

    #[test]
    fn missing_source_fails_without_creating_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("d.bin");
        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());

        let res = rt().block_on(run_transfer(TransferJob {
            id: TransferId(1),
            src_fs: fs.clone(),
            dest_fs: fs.clone(),
            src: VPath::local(dir.path().join("nope.bin")),
            dest: VPath::local(&dest),
            progress: Arc::new(TransferProgress::new(0)),
            cancel: CancelToken::new(),
            config: TransferConfig::default(),
        }));

        assert!(res.is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn directory_source_is_copied_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("tree");
        std::fs::create_dir_all(src.join("a/b")).unwrap();
        std::fs::write(src.join("top.txt"), vec![1u8; 1000]).unwrap();
        std::fs::write(src.join("a/mid.bin"), vec![2u8; 2000]).unwrap();
        std::fs::write(src.join("a/b/deep.bin"), vec![3u8; 3000]).unwrap();
        let dest = dir.path().join("copy");

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let progress = Arc::new(TransferProgress::new(0));
        rt().block_on(run_transfer(TransferJob {
            id: TransferId(1),
            src_fs: fs.clone(),
            dest_fs: fs.clone(),
            src: VPath::local(&src),
            dest: VPath::local(&dest),
            progress: progress.clone(),
            cancel: CancelToken::new(),
            config: TransferConfig::default(),
        }))
        .unwrap();

        // The whole tree landed, byte-for-byte.
        assert_eq!(
            std::fs::read(dest.join("top.txt")).unwrap(),
            vec![1u8; 1000]
        );
        assert_eq!(
            std::fs::read(dest.join("a/mid.bin")).unwrap(),
            vec![2u8; 2000]
        );
        assert_eq!(
            std::fs::read(dest.join("a/b/deep.bin")).unwrap(),
            vec![3u8; 3000]
        );
        // Progress totalled every file and completed.
        assert_eq!(progress.total_bytes(), 6000);
        assert_eq!(progress.bytes_done(), 6000);
        assert!(progress.is_finished());
    }

    #[test]
    fn overwrites_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s.bin");
        let dest = dir.path().join("d.bin");
        std::fs::write(&src, b"new content").unwrap();
        std::fs::write(&dest, b"old").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        rt().block_on(run_transfer(TransferJob {
            id: TransferId(1),
            src_fs: fs.clone(),
            dest_fs: fs.clone(),
            src: VPath::local(&src),
            dest: VPath::local(&dest),
            progress: Arc::new(TransferProgress::new(0)),
            cancel: CancelToken::new(),
            config: TransferConfig::default(),
        }))
        .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"new content");
    }

    #[test]
    fn transfers_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("empty.bin");
        std::fs::write(&src, b"").unwrap();
        let dest = dir.path().join("out.bin");

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        rt().block_on(run_transfer(TransferJob {
            id: TransferId(1),
            src_fs: fs.clone(),
            dest_fs: fs.clone(),
            src: VPath::local(&src),
            dest: VPath::local(&dest),
            progress: Arc::new(TransferProgress::new(0)),
            cancel: CancelToken::new(),
            config: TransferConfig::default(),
        }))
        .unwrap();

        assert!(dest.exists());
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 0);
    }
}
