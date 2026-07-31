use anyhow::{Context, Result};
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

/// Which copy strategy a file took, for logging and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    /// Concurrent positioned reads — a large download from a pipelining backend.
    ParallelRead,
    /// Concurrent positioned writes — a large upload to one.
    ParallelWrite,
    /// Read a buffer, write it, repeat. Best locally and for small files.
    Sequential,
}

impl Path {
    fn as_str(self) -> &'static str {
        match self {
            Path::ParallelRead => "parallel_read",
            Path::ParallelWrite => "parallel_write",
            Path::Sequential => "sequential",
        }
    }
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

    let meta = src_fs
        .stat(&src)
        .await
        .with_context(|| format!("could not read source {}", src))?;

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

    // The destination directory has to exist before anything is written. If it
    // cannot be created, fail here naming *that* — continuing produced a
    // NoSuchFile against the `.myd-part-…` file instead, which reads as a bug in
    // the transfer rather than as "the destination directory does not exist".
    //
    // A directory that is already there is the common case and costs one cached
    // round trip; `create_dir_all` succeeds for it.
    if let Some(parent) = dest.parent() {
        dest_fs.create_dir_all(&parent).await.with_context(|| {
            format!(
                "destination directory {} does not exist and could not be created",
                parent
            )
        })?;
    }

    let part = part_path(&dest, id);
    let result = stream_file(
        &src_fs, &dest_fs, &src, &part, &progress, &cancel, &config, meta.len,
    )
    .await
    .with_context(|| {
        format!(
            "copying {} -> {} ({} bytes)",
            src, dest, meta.len
        )
    });

    // Log the failure here rather than only in the queue: the headless binary
    // and the move machinery call `run_transfer` directly, and a diagnostic that
    // only appears on one of three paths is the one that is missing when needed.
    if let Err(e) = &result {
        let chain: Vec<String> = e.chain().map(|c| c.to_string()).collect();
        tracing::error!(
            id = %id,
            src = %src,
            dest = %dest,
            src_backend = src_fs.scheme(),
            dest_backend = dest_fs.scheme(),
            total_bytes = meta.len,
            bytes_done = progress.bytes_done(),
            error = %e,
            cause_chain = ?chain,
            "transfer failed"
        );
    }

    match result {
        Ok(TransferOutcome::Done) => {
            // Put the completed bytes at the real destination. An existing file
            // is replaced — the overwrite decision was made by the caller before
            // queueing.
            //
            // Try the rename first and only clear the destination if it fails.
            // SFTP v3 rename refuses an existing destination, so the removal is
            // load-bearing, but unconditionally removing first cost a round trip
            // on every file including the common case where nothing is there.
            if let Err(first) = dest_fs.rename(&part, &dest).await {
                tracing::debug!(
                    part = %part, dest = %dest, error = %first,
                    "rename into place failed; clearing the destination and retrying"
                );
                if let Err(rm) = dest_fs.remove_file(&dest).await {
                    tracing::debug!(dest = %dest, error = %rm, "could not clear the destination");
                }
                if let Err(e) = dest_fs.rename(&part, &dest).await {
                    dest_fs.remove_file(&part).await.ok();
                    // Both attempts are reported: the first failure is often the
                    // informative one ("permission denied") while the retry only
                    // says the destination is still there.
                    return Err(e).with_context(|| {
                        format!(
                            "could not move the completed copy into place at {} \
                             (first attempt: {})",
                            dest, first
                        )
                    });
                }
            }
            progress.finish();
            Ok(TransferOutcome::Done)
        }
        Ok(TransferOutcome::Cancelled) => {
            if let Err(e) = dest_fs.remove_file(&part).await {
                tracing::debug!(part = %part, error = %e, "could not remove the part file after cancel");
            }
            Ok(TransferOutcome::Cancelled)
        }
        Err(e) => {
            // Clean up the partial file, but never let a cleanup failure replace
            // the error that actually caused the transfer to fail.
            if let Err(rm) = dest_fs.remove_file(&part).await {
                tracing::debug!(part = %part, error = %rm, "could not remove the part file after failure");
            }
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
    // One shared budget for the whole tree. Each recursion level otherwise opens
    // its own window of `max_parallel`, and those multiply: a tree `d` levels
    // deep could reach `max_parallel^d` simultaneous operations and swamp the
    // connection's request budget. The per-level windows below still bound
    // breadth; this bounds the product.
    let limit = Arc::new(tokio::sync::Semaphore::new(
        crate::config::transfer_global_concurrency().max(1),
    ));
    transfer_dir_inner(src_fs, dest_fs, src, dest, progress, cancel, config, limit)
}

#[allow(clippy::too_many_arguments)]
fn transfer_dir_inner<'a>(
    src_fs: &'a Arc<dyn Vfs>,
    dest_fs: &'a Arc<dyn Vfs>,
    src: &'a VPath,
    dest: &'a VPath,
    progress: &'a Arc<TransferProgress>,
    cancel: &'a CancelToken,
    config: &'a TransferConfig,
    limit: Arc<tokio::sync::Semaphore>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<TransferOutcome>> + Send + 'a>> {
    Box::pin(async move {
        use futures::stream::{FuturesUnordered, StreamExt};

        if cancel.is_cancelled() {
            return Ok(TransferOutcome::Cancelled);
        }
        dest_fs
            .create_dir_all(dest)
            .await
            .with_context(|| format!("could not create destination directory {}", dest))?;

        let entries = src_fs
            .read_dir(src)
            .await
            .with_context(|| format!("could not list source directory {}", src))?;
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
                let limit = limit.clone();
                inflight.push(async move {
                    // Wait for room in the tree-wide budget before starting.
                    let _permit = limit.acquire().await;
                    // Stream straight to the final name: a per-file part rename
                    // across a whole tree adds round trips for little safety,
                    // and the directory itself is the unit of retry.
                    stream_file(
                        src_fs, dest_fs, &child_src, &child_dest, progress, cancel, config, len,
                    )
                    .await
                    // Name the file that failed. A directory copy reports one
                    // error for the whole tree, and without this it named only
                    // the directory — leaving no way to tell which of a thousand
                    // files was the problem.
                    .with_context(|| format!("copying {} ({} bytes)", child_src, len))
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
                let limit = limit.clone();
                walking.push(async move {
                    transfer_dir_inner(
                        src_fs, dest_fs, &child_src, &child_dest, progress, cancel, config, limit,
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
    // Which side can overlap round trips decides the path. Keying only on the
    // source meant an upload (local source, remote destination) always fell to
    // the sequential path, where one write is outstanding at a time — the reason
    // uploads ran far below the download rate.
    let big = len_hint >= super::LARGE_FILE_THRESHOLD;
    let path_taken = if big && src_fs.supports_parallel_read() {
        Path::ParallelRead
    } else if big && dest_fs.supports_parallel_write() {
        Path::ParallelWrite
    } else {
        Path::Sequential
    };
    let started = std::time::Instant::now();

    let outcome = match path_taken {
        Path::ParallelRead => {
            stream_file_parallel(
                src_fs, dest_fs, src, part, progress, cancel, config, len_hint,
            )
            .await
        }
        Path::ParallelWrite => {
            stream_file_parallel_upload(
                src_fs, dest_fs, src, part, progress, cancel, config, len_hint,
            )
            .await
        }
        Path::Sequential => {
            stream_file_sequential(src_fs, dest_fs, src, part, progress, cancel, config).await
        }
    };

    // One event per file rather than per chunk: on a fast link a per-chunk event
    // would cost more than the work it describes.
    if crate::trace::enabled() {
        let secs = started.elapsed().as_secs_f64();
        let concurrent = !matches!(path_taken, Path::Sequential);
        tracing::debug!(
            path = %src,
            bytes = len_hint,
            path_taken = path_taken.as_str(),
            chunk_size = if concurrent {
                super::effective_chunk_size(config)
            } else {
                super::sequential_buffer_size(config)
            },
            window = if concurrent {
                super::large_file_chunks_in_flight(config)
            } else {
                1
            },
            elapsed_ms = secs * 1000.0,
            mib_per_sec = if secs > 0.0 {
                (len_hint as f64 / (1024.0 * 1024.0)) / secs
            } else {
                0.0
            },
            ok = outcome.is_ok(),
            "transfer_complete"
        );
    }

    outcome
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
    let mut reader = src_fs
        .open_read(src)
        .await
        .with_context(|| format!("could not open {} for reading", src))?;
    let mut writer = dest_fs
        .open_write(part, None)
        .await
        .with_context(|| format!("could not open {} for writing", part))?;

    // The sequential path wants a *large* buffer, for the opposite reason the
    // parallel path wants a small one. Here the buffer is simply how much moves
    // per read/write pair, and the underlying client pipelines beneath it, so a
    // bigger buffer means fewer alternations. Clamping this to the parallel
    // path's request-sized chunk would multiply the round trips on every upload.
    let mut buf = vec![0u8; super::sequential_buffer_size(config)];
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

/// The concurrent copy path for a large *upload*.
///
/// The mirror of [`stream_file_parallel`]: the source is read sequentially (it
/// is local, where the kernel's readahead already makes that fast) and the
/// chunks are written concurrently at explicit offsets. Without this an upload
/// can only have one write outstanding at a time, so it pays a full round trip
/// per chunk and runs far below the download rate on a long link.
///
/// Writes may complete out of order — each carries its own offset, so the file
/// is correct regardless.
#[allow(clippy::too_many_arguments)]
async fn stream_file_parallel_upload(
    src_fs: &Arc<dyn Vfs>,
    dest_fs: &Arc<dyn Vfs>,
    src: &VPath,
    part: &VPath,
    progress: &Arc<TransferProgress>,
    cancel: &CancelToken,
    config: &TransferConfig,
    total: u64,
) -> Result<TransferOutcome> {
    use crate::vfs::VPositionedWrite;
    use futures::stream::{FuturesUnordered, StreamExt};
    use std::sync::Arc as StdArc;

    let mut reader = src_fs
        .open_read(src)
        .await
        .with_context(|| format!("could not open {} for reading", src))?;
    let first: StdArc<dyn VPositionedWrite> =
        StdArc::from(dest_fs.open_positioned_write(part, Some(total)).await?);

    let chunk = super::effective_chunk_size(config);
    let window = super::large_file_chunks_in_flight(config);

    // As on the read side, clone handles where the backend allows it so the pool
    // costs one open rather than one per slot.
    let mut pool: Vec<StdArc<dyn VPositionedWrite>> = Vec::with_capacity(window);
    pool.push(first.clone());
    while pool.len() < window {
        match first.clone_handle().await {
            Some(h) => pool.push(StdArc::from(h)),
            None => break,
        }
    }

    let mut inflight = FuturesUnordered::new();
    let mut offset = 0u64;
    let mut slot = 0usize;
    let mut cancelled = false;
    let mut written_total = 0u64;

    loop {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }

        // Read the next chunk locally. `read` may return less than asked for, so
        // fill the buffer before handing it to a writer — a short local read is
        // not an error and must not become a short chunk.
        let mut buf = vec![0u8; chunk];
        let mut filled = 0usize;
        while filled < chunk {
            let n = reader.read(&mut buf[filled..]).await?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break; // end of source
        }
        buf.truncate(filled);

        // Wait for a free slot before issuing another write, which bounds both
        // the outstanding requests and the memory held by their buffers.
        if inflight.len() >= window {
            match inflight.next().await {
                Some(Ok(())) => {}
                Some(Err(e)) => return Err(e),
                None => {}
            }
        }

        let handle = pool[slot % pool.len()].clone();
        slot += 1;
        let at = offset;
        offset += filled as u64;
        written_total += filled as u64;
        let progress = progress.clone();
        inflight.push(async move {
            handle.write_at(at, &buf).await?;
            progress.add_bytes(buf.len() as u64);
            Ok::<(), anyhow::Error>(())
        });
    }

    // Drain the rest, propagating the first failure.
    while let Some(result) = inflight.next().await {
        result?;
    }

    if cancelled {
        return Ok(TransferOutcome::Cancelled);
    }

    first.finish().await?;
    drop(pool);

    if written_total != total {
        anyhow::bail!(
            "short upload: wrote {} of {} bytes",
            written_total,
            total
        );
    }

    Ok(TransferOutcome::Done)
}

/// The concurrent copy path for a large file on a pipelining backend.
///
/// Chunks are read in a sliding window of overlapping positioned reads, each on
/// its own handle, and handed to a writer task that appends them in order. Two
/// details make the window actually as deep as it looks:
///
/// * **The writer is decoupled.** Writing inline would drain the window to
///   `window - 1` on every chunk and refill it only after the write completed,
///   so the pipeline would spend much of its time partly empty. A bounded
///   channel keeps ordering and backpressure while letting reads run ahead.
/// * **Chunks are sized to the backend's request limit.** A chunk larger than
///   what one request can carry becomes several sequential requests inside a
///   single slot (see [`VPositionedRead::read_at`]), which caps the pipeline at
///   its slot count regardless of the byte counts involved.
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

    let writer = dest_fs
        .open_write(part, Some(total))
        .await
        .with_context(|| format!("could not open {} for writing", part))?;

    let chunk = super::effective_chunk_size(config) as u64;
    // Share the connection's request budget with the other transfers that may be
    // running large files concurrently.
    let window = super::large_file_chunks_in_flight(config);

    // A pool of positioned-read handles. The first is opened for real; the rest
    // are cloned from it when the backend can do so without a round trip, which
    // on a long link saves `window - 1` sequential opens before the first byte
    // moves.
    let first: StdArc<dyn VPositionedRead> = StdArc::from(src_fs.open_positioned_read(src).await?);
    let mut pool: Vec<StdArc<dyn VPositionedRead>> = Vec::with_capacity(window);
    pool.push(first.clone());
    while pool.len() < window {
        match first.clone_handle().await {
            Some(h) => pool.push(StdArc::from(h)),
            // No cheap clone available: fall back to opening the rest, in
            // parallel rather than one at a time.
            None => {
                let need = window - pool.len();
                let opens = (0..need).map(|_| src_fs.open_positioned_read(src));
                for handle in futures::future::try_join_all(opens).await? {
                    pool.push(StdArc::from(handle));
                }
                break;
            }
        }
    }

    // Ordered writes on a bounded queue: the reader can run `window` chunks ahead
    // and no further, so memory stays bounded at roughly `window * chunk`.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(window.max(1));
    let progress_for_writer = progress.clone();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        let mut written: u64 = 0;
        while let Some(data) = rx.recv().await {
            writer.write_all(&data).await?;
            written += data.len() as u64;
            progress_for_writer.add_bytes(data.len() as u64);
        }
        writer.flush().await?;
        writer.shutdown().await?;
        Ok::<u64, anyhow::Error>(written)
    });

    // Read one chunk's worth, re-issuing until the range is filled.
    //
    // A backend may return less than asked for at any offset, not only at EOF,
    // and the pieces are concatenated in order — so a short read that was simply
    // accepted would silently produce a truncated file. Re-issuing happens per
    // chunk, concurrently with the other slots, rather than inside `read_at`
    // where it would serialise the whole window.
    async fn read_chunk(
        handle: StdArc<dyn VPositionedRead>,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let mut out = handle.read_at(offset, len).await?;
        while out.len() < len {
            let got = handle
                .read_at(offset + out.len() as u64, len - out.len())
                .await?;
            if got.is_empty() {
                break; // genuine EOF
            }
            out.extend_from_slice(&got);
        }
        Ok(out)
    }

    let n_chunks = total.div_ceil(chunk) as usize;
    let mut next = 0usize;
    let mut inflight = FuturesOrdered::new();

    type ChunkFut =
        std::pin::Pin<Box<dyn std::future::Future<Output = (usize, Result<Vec<u8>>)> + Send>>;
    let make_read = |handle: StdArc<dyn VPositionedRead>, slot: usize, index: usize| -> ChunkFut {
        let offset = index as u64 * chunk;
        let len = chunk.min(total - offset) as usize;
        Box::pin(async move {
            let data = read_chunk(handle, offset, len).await;
            (slot, data)
        })
    };

    while next < n_chunks && inflight.len() < window {
        let slot = next % window;
        inflight.push_back(make_read(pool[slot].clone(), slot, next));
        next += 1;
    }

    let mut produced: u64 = 0;
    let mut cancelled = false;
    while let Some((slot, result)) = inflight.next().await {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        let data = result?;
        produced += data.len() as u64;
        // A closed channel means the writer failed; stop reading and surface it.
        if tx.send(data).await.is_err() {
            break;
        }

        if next < n_chunks {
            inflight.push_back(make_read(pool[slot].clone(), slot, next));
            next += 1;
        }
    }

    // Dropping the sender ends the writer's loop and flushes it.
    drop(tx);
    let written = writer_task.await.context("writer task panicked")??;

    if cancelled {
        return Ok(TransferOutcome::Cancelled);
    }

    // Fail loudly rather than leave a plausible-looking short file. Without this
    // a dropped or truncated chunk would rename into place as if complete.
    if written != total {
        anyhow::bail!(
            "short transfer: wrote {} of {} bytes (read {})",
            written,
            total,
            produced
        );
    }

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
        // The concurrent path ran. This mock offers no `clone_handle`, so it
        // falls back to opening one handle per slot; a backend that can clone
        // (SFTP) opens exactly one regardless of window size.
        assert_eq!(handles.load(std::sync::atomic::Ordering::Relaxed), 4);
    }

    /// A backend that returns *half* of what was asked for, at every offset.
    ///
    /// Short reads are legal at any point, not just at EOF, and the pieces are
    /// concatenated in order — so a caller that accepts a short read without
    /// re-issuing silently produces a truncated file that then renames into
    /// place looking complete. This is the shape of that bug.
    struct ShortReadVfs {
        data: Vec<u8>,
    }

    struct ShortHandle {
        data: std::sync::Arc<Vec<u8>>,
    }

    #[async_trait]
    impl VPositionedRead for ShortHandle {
        async fn read_at(&self, offset: u64, len: usize) -> AResult<Vec<u8>> {
            let start = (offset as usize).min(self.data.len());
            // Deliberately serve less than requested while bytes remain.
            let half = (len / 2).max(1);
            let end = (start + half).min(self.data.len());
            Ok(self.data[start..end].to_vec())
        }
    }

    #[async_trait]
    impl Vfs for ShortReadVfs {
        fn scheme(&self) -> &'static str {
            "short"
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
            Ok(Box::new(ShortHandle {
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
    fn short_reads_mid_file_still_produce_a_complete_copy() {
        let size = 5 * 1024 * 1024 + 777;
        let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

        let src_fs: Arc<dyn Vfs> = Arc::new(ShortReadVfs {
            data: payload.clone(),
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("short.bin");
        let dest_fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());

        rt().block_on(run_transfer(TransferJob {
            id: TransferId(1),
            src_fs,
            dest_fs,
            src: VPath::new(BackendId(1), "/remote/short.bin"),
            dest: VPath::local(&dest),
            progress: Arc::new(TransferProgress::new(0)),
            cancel: CancelToken::new(),
            config: TransferConfig {
                chunks_in_flight: 4,
                ..Default::default()
            },
        }))
        .expect("transfer must succeed despite short reads");

        let got = std::fs::read(&dest).unwrap();
        assert_eq!(
            got.len(),
            payload.len(),
            "file was truncated: short reads were not re-issued"
        );
        assert_eq!(got, payload, "content mismatch after short reads");
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
        //
        // Large enough that the copy cannot finish before the cancel arrives.
        // At 8 MB this raced: the test waits for the first bytes to move and
        // then cancels, but a warm page cache could copy the whole file inside
        // that window, leaving a completed transfer and no part file to find.
        // It failed roughly one run in ten, and more often under parallel load.
        std::fs::write(&src, vec![3u8; 64 * 1024 * 1024]).unwrap();
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
