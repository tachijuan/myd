use std::collections::HashMap;
use std::sync::Arc;

use super::worker::{run_transfer, TransferJob, TransferOutcome};
use super::{Transfer, TransferConfig, TransferId, TransferState};
use crate::vfs::{BackendRegistry, VPath, Vfs};

/// What a finished worker reports back.
enum Completion {
    Done,
    Cancelled,
    Failed(String),
}

/// The transfer queue: holds every transfer this session and runs at most
/// `config.max_parallel` of them at a time.
///
/// Enqueuing while transfers are running simply appends — that is what makes
/// requests stack instead of blocking. The UI calls [`tick`](Self::tick) once
/// per frame to promote queued work and reap finished tasks; nothing here ever
/// blocks the render loop.
pub struct TransferQueue {
    transfers: Vec<Transfer>,
    /// Running tasks by id, so completion can be matched back to its transfer.
    running: HashMap<TransferId, tokio::task::JoinHandle<Completion>>,
    next_id: u64,
    pub config: TransferConfig,
    /// Destinations of transfers that *succeeded* since the last drain. The app
    /// pulls these each tick to refresh just the affected directory level, so a
    /// completed copy appears without a manual rescan.
    completed_dests: Vec<VPath>,
}

/// A destination a transfer is currently working toward — a "ghost" the
/// destination tree shows until the real entry lands.
#[derive(Debug, Clone)]
pub struct PendingDest {
    /// Full path of the item being written.
    pub path: VPath,
    /// Whether the item is a directory (so the tree draws the right icon).
    pub is_dir: bool,
}

impl Default for TransferQueue {
    fn default() -> Self {
        Self::new(TransferConfig::default())
    }
}

impl TransferQueue {
    pub fn new(config: TransferConfig) -> Self {
        Self {
            transfers: Vec::new(),
            running: HashMap::new(),
            next_id: 1,
            config,
            completed_dests: Vec::new(),
        }
    }

    /// Queue a transfer. `total_bytes` may be 0 when unknown — the worker
    /// re-stats the source and corrects it.
    pub fn enqueue(&mut self, src: VPath, dest: VPath, total_bytes: u64) -> TransferId {
        self.enqueue_kind(src, dest, total_bytes, false)
    }

    /// As [`enqueue`], recording whether the item is a directory so the ghost
    /// entry in the destination tree gets the right icon.
    pub fn enqueue_kind(
        &mut self,
        src: VPath,
        dest: VPath,
        total_bytes: u64,
        is_dir: bool,
    ) -> TransferId {
        let id = TransferId(self.next_id);
        self.next_id += 1;
        self.transfers
            .push(Transfer::with_kind(id, src, dest, total_bytes, is_dir));
        id
    }

    /// Enqueue the copy half of a cross-backend move.
    ///
    /// Identical to [`enqueue_kind`](Self::enqueue_kind) except that the source
    /// is deleted once the copy has fully succeeded — so a move gets the same
    /// queueing, parallelism and transfer-panel progress as a copy.
    pub fn enqueue_move(
        &mut self,
        src: VPath,
        dest: VPath,
        total_bytes: u64,
        is_dir: bool,
    ) -> TransferId {
        let id = TransferId(self.next_id);
        self.next_id += 1;
        self.transfers.push(
            Transfer::with_kind(id, src, dest, total_bytes, is_dir).removing_source(),
        );
        id
    }

    /// Destinations of transfers still queued or running — the "ghosts" the
    /// destination tree overlays until each real entry lands.
    pub fn pending_destinations(&self) -> Vec<PendingDest> {
        self.transfers
            .iter()
            .filter(|t| !t.state.is_terminal())
            .map(|t| PendingDest {
                path: t.dest.clone(),
                is_dir: t.is_dir,
            })
            .collect()
    }

    /// Take the destinations of transfers that have succeeded since the last
    /// call. The app drains these each tick to refresh just the affected
    /// directory level, so a completed transfer appears without a manual rescan.
    pub fn take_completed_destinations(&mut self) -> Vec<VPath> {
        std::mem::take(&mut self.completed_dests)
    }

    /// Number of transfers currently running.
    pub fn active_count(&self) -> usize {
        self.transfers
            .iter()
            .filter(|t| t.state == TransferState::Active)
            .count()
    }

    pub fn queued_count(&self) -> usize {
        self.transfers
            .iter()
            .filter(|t| t.state == TransferState::Queued)
            .count()
    }

    /// Transfers that have stopped, whether they succeeded or not.
    pub fn finished_count(&self) -> usize {
        self.transfers
            .iter()
            .filter(|t| t.state.is_terminal())
            .count()
    }

    /// Whether anything is still queued or running — drives the panel's
    /// "in flight" indicator.
    pub fn has_work(&self) -> bool {
        self.transfers.iter().any(|t| !t.state.is_terminal())
    }

    pub fn transfers(&self) -> &[Transfer] {
        &self.transfers
    }

    /// Mutable access to the transfer list. Used by tests to stage states the
    /// reaper would otherwise have to produce.
    #[cfg(test)]
    pub fn transfers_mut(&mut self) -> &mut [Transfer] {
        &mut self.transfers
    }

    /// Mutable access for rendering demos and examples, which need to stage
    /// mid-flight states that a real run only reaches transiently.
    ///
    /// Not part of the app's own control flow: the queue drives its own state
    /// transitions through [`tick`](Self::tick).
    #[doc(hidden)]
    pub fn transfers_mut_for_demo(&mut self) -> &mut [Transfer] {
        &mut self.transfers
    }

    pub fn is_empty(&self) -> bool {
        self.transfers.is_empty()
    }

    /// Cancel one transfer by id.
    pub fn cancel(&mut self, id: TransferId) {
        if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
            t.request_cancel();
        }
    }

    /// Cancel everything still outstanding.
    pub fn cancel_all(&mut self) {
        for t in self.transfers.iter_mut() {
            if !t.state.is_terminal() {
                t.request_cancel();
            }
        }
    }

    /// Drop terminal entries from the list, keeping queued and active ones.
    pub fn clear_finished(&mut self) {
        self.transfers.retain(|t| !t.state.is_terminal());
    }

    /// Reap completed tasks and promote queued transfers up to the parallelism
    /// limit. Call once per frame.
    ///
    /// `max_parallel` is read from `self.config` on every call rather than
    /// captured at construction, so changing it at runtime takes effect on the
    /// next tick.
    pub fn tick(&mut self, registry: &BackendRegistry) {
        self.reap();
        self.promote(registry);
    }

    /// Move finished workers' results onto their transfers.
    fn reap(&mut self) {
        let finished: Vec<TransferId> = self
            .running
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(id, _)| *id)
            .collect();

        for id in finished {
            let Some(handle) = self.running.remove(&id) else {
                continue;
            };
            // The task is finished, so this resolves immediately; block_in_place
            // is unnecessary and `now_or_never` keeps the render loop sync.
            let completion = match futures::FutureExt::now_or_never(handle) {
                Some(Ok(c)) => c,
                // A panicked or aborted worker must not wedge the queue.
                Some(Err(e)) => Completion::Failed(format!("task failed: {}", e)),
                None => continue,
            };

            if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                t.state = match completion {
                    Completion::Done => {
                        // A success: record its destination so the app can
                        // refresh just that directory level and drop the ghost.
                        self.completed_dests.push(t.dest.clone());
                        TransferState::Done
                    }
                    Completion::Cancelled => TransferState::Cancelled,
                    Completion::Failed(msg) => TransferState::Failed(msg),
                };
            }
        }
    }

    /// Start queued transfers while there is capacity.
    fn promote(&mut self, registry: &BackendRegistry) {
        let mut active = self.active_count();
        let config = self.config;

        for t in self.transfers.iter_mut() {
            if active >= config.max_parallel {
                break;
            }
            if t.state != TransferState::Queued {
                continue;
            }
            // A transfer cancelled while queued is retired without starting.
            if t.cancel.is_cancelled() {
                t.state = TransferState::Cancelled;
                continue;
            }

            let src_fs: Arc<dyn Vfs> = registry.get(t.src.backend);
            let dest_fs: Arc<dyn Vfs> = registry.get(t.dest.backend);

            let remove_source = t.remove_source;
            let move_src = t.src.clone();
            let move_fs = src_fs.clone();
            let move_cancel = t.cancel.clone();
            let (t_id, t_src, t_dest) = (t.id, t.src.clone(), t.dest.clone());
            let (t_progress, t_cancel) = (t.progress.clone(), t.cancel.clone());
            let handle = tokio::spawn(async move {
                let completion = run_worker(TransferJob {
                    id: t_id,
                    src_fs,
                    dest_fs,
                    src: t_src,
                    dest: t_dest,
                    progress: t_progress,
                    cancel: t_cancel,
                    config,
                })
                .await;
                // The delete half of a move, and only on a clean success:
                // removing the source after a partial or cancelled copy would
                // destroy the only complete copy of the data.
                if remove_source && matches!(completion, Completion::Done) {
                    if let Err(e) = crate::vfs::ops::delete_recursive(
                        &move_fs,
                        &move_src,
                        None,
                        &move_cancel,
                    )
                    .await
                    {
                        return Completion::Failed(format!(
                            "copied, but could not remove the source: {}",
                            e
                        ));
                    }
                }
                completion
            });

            self.running.insert(t.id, handle);
            t.state = TransferState::Active;
            active += 1;
        }
    }
}

/// Wrapper turning the worker's `Result` into a [`Completion`] so a failure is
/// recorded against the transfer instead of escaping the task.
async fn run_worker(job: TransferJob) -> Completion {
    match run_transfer(job).await {
        Ok(TransferOutcome::Done) => Completion::Done,
        Ok(TransferOutcome::Cancelled) => Completion::Cancelled,
        Err(e) => Completion::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    /// Write `n` files of `size` bytes and return their paths.
    fn make_files(dir: &std::path::Path, n: usize, size: usize) -> Vec<std::path::PathBuf> {
        (0..n)
            .map(|i| {
                let p = dir.join(format!("f{}.bin", i));
                std::fs::write(&p, vec![(i % 251) as u8; size]).unwrap();
                p
            })
            .collect()
    }

    /// Drive the queue to completion, asserting the parallelism cap the whole way.
    fn drain(
        queue: &mut TransferQueue,
        registry: &BackendRegistry,
        rt: &tokio::runtime::Runtime,
    ) -> usize {
        let mut peak = 0;
        for _ in 0..100_000 {
            queue.tick(registry);
            peak = peak.max(queue.active_count());
            assert!(
                queue.active_count() <= queue.config.max_parallel,
                "exceeded max_parallel: {} > {}",
                queue.active_count(),
                queue.config.max_parallel
            );
            if !queue.has_work() {
                break;
            }
            rt.block_on(tokio::time::sleep(std::time::Duration::from_millis(1)));
        }
        peak
    }

    #[test]
    fn enqueue_assigns_increasing_ids_and_starts_queued() {
        let mut q = TransferQueue::default();
        let a = q.enqueue(VPath::local("/a"), VPath::local("/b"), 1);
        let b = q.enqueue(VPath::local("/c"), VPath::local("/d"), 1);
        assert_eq!((a, b), (TransferId(1), TransferId(2)));
        assert_eq!(q.queued_count(), 2);
        assert_eq!(q.active_count(), 0);
        assert!(q.has_work());
    }

    #[test]
    fn never_exceeds_max_parallel_and_completes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let srcs = make_files(dir.path(), 10, 256 * 1024);
        let dest_dir = dir.path().join("out");

        let rt = rt();
        let _guard = rt.enter();
        let registry = BackendRegistry::new();
        let mut q = TransferQueue::default();
        assert_eq!(q.config.max_parallel, crate::transfer::DEFAULT_MAX_PARALLEL);

        for s in &srcs {
            let dest = VPath::local(dest_dir.join(s.file_name().unwrap()));
            q.enqueue(VPath::local(s), dest, 0);
        }

        let peak = drain(&mut q, &registry, &rt);

        assert_eq!(q.finished_count(), 10);
        assert!(
            q.transfers().iter().all(|t| t.state == TransferState::Done),
            "unexpected states: {:?}",
            q.transfers().iter().map(|t| &t.state).collect::<Vec<_>>()
        );
        // The cap must actually have been exercised, not merely respected.
        assert!(peak > 1, "expected real parallelism, peak was {}", peak);
        assert!(peak <= q.config.max_parallel);

        for s in &srcs {
            let out = dest_dir.join(s.file_name().unwrap());
            assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(s).unwrap());
        }
    }

    #[test]
    fn max_parallel_of_one_serialises_transfers() {
        let dir = tempfile::tempdir().unwrap();
        let srcs = make_files(dir.path(), 5, 64 * 1024);
        let dest_dir = dir.path().join("out");

        let rt = rt();
        let _guard = rt.enter();
        let registry = BackendRegistry::new();
        let mut q = TransferQueue::new(TransferConfig {
            max_parallel: 1,
            ..Default::default()
        });

        for s in &srcs {
            q.enqueue(
                VPath::local(s),
                VPath::local(dest_dir.join(s.file_name().unwrap())),
                0,
            );
        }

        let peak = drain(&mut q, &registry, &rt);
        assert_eq!(peak, 1);
        assert_eq!(q.finished_count(), 5);
    }

    #[test]
    fn higher_max_parallel_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let srcs = make_files(dir.path(), 12, 512 * 1024);
        let dest_dir = dir.path().join("out");

        let rt = rt();
        let _guard = rt.enter();
        let registry = BackendRegistry::new();
        let mut q = TransferQueue::new(TransferConfig {
            max_parallel: 8,
            ..Default::default()
        });

        for s in &srcs {
            q.enqueue(
                VPath::local(s),
                VPath::local(dest_dir.join(s.file_name().unwrap())),
                0,
            );
        }

        let peak = drain(&mut q, &registry, &rt);
        assert!(peak > 4, "expected >4 concurrent with cap 8, got {}", peak);
        assert_eq!(q.finished_count(), 12);
    }

    #[test]
    fn transfers_stack_when_enqueued_mid_flight() {
        let dir = tempfile::tempdir().unwrap();
        let first = make_files(dir.path(), 4, 256 * 1024);
        let dest_dir = dir.path().join("out");

        let rt = rt();
        let _guard = rt.enter();
        let registry = BackendRegistry::new();
        // A small explicit cap, so the queue is genuinely saturated by these
        // four files — the situation this test is about. (The default cap is
        // much larger and would start all of them immediately.)
        let mut q = TransferQueue::new(TransferConfig {
            max_parallel: 4,
            ..Default::default()
        });

        for s in &first {
            q.enqueue(
                VPath::local(s),
                VPath::local(dest_dir.join(s.file_name().unwrap())),
                0,
            );
        }
        // Fill all four slots.
        q.tick(&registry);
        assert_eq!(q.active_count(), 4);

        // Adding more while saturated appends rather than blocking or starting.
        let second = make_files(&dir.path().join("."), 3, 128 * 1024);
        for (i, s) in second.iter().enumerate() {
            q.enqueue(
                VPath::local(s),
                VPath::local(dest_dir.join(format!("late{}.bin", i))),
                0,
            );
        }
        q.tick(&registry);
        // The cap is never exceeded, and the late arrivals are accounted for as
        // either still queued or already promoted — never dropped. (We don't
        // assert they're *still* queued: on a fast disk the first four can finish
        // before this tick, which is fine.)
        assert!(q.active_count() <= q.config.max_parallel);
        assert_eq!(
            q.queued_count() + q.active_count() + q.finished_count(),
            7,
            "every transfer is accounted for"
        );

        drain(&mut q, &registry, &rt);
        assert_eq!(q.finished_count(), 7);
    }

    #[test]
    fn a_failed_transfer_does_not_stall_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let good = make_files(dir.path(), 3, 32 * 1024);
        let dest_dir = dir.path().join("out");

        let rt = rt();
        let _guard = rt.enter();
        let registry = BackendRegistry::new();
        let mut q = TransferQueue::default();

        // A nonexistent source fails; the good ones must still complete.
        q.enqueue(
            VPath::local(dir.path().join("missing.bin")),
            VPath::local(dest_dir.join("missing.bin")),
            0,
        );
        for s in &good {
            q.enqueue(
                VPath::local(s),
                VPath::local(dest_dir.join(s.file_name().unwrap())),
                0,
            );
        }

        drain(&mut q, &registry, &rt);

        assert_eq!(q.finished_count(), 4);
        let failed = q
            .transfers()
            .iter()
            .filter(|t| matches!(t.state, TransferState::Failed(_)))
            .count();
        let done = q
            .transfers()
            .iter()
            .filter(|t| t.state == TransferState::Done)
            .count();
        assert_eq!((failed, done), (1, 3));
    }

    #[test]
    fn cancel_all_retires_queued_transfers_without_running_them() {
        let dir = tempfile::tempdir().unwrap();
        let srcs = make_files(dir.path(), 6, 32 * 1024);

        let rt = rt();
        let _guard = rt.enter();
        let registry = BackendRegistry::new();
        let mut q = TransferQueue::default();
        for s in &srcs {
            q.enqueue(
                VPath::local(s),
                VPath::local(dir.path().join("out").join(s.file_name().unwrap())),
                0,
            );
        }

        q.cancel_all();
        q.tick(&registry);
        // Nothing was ever promoted.
        assert_eq!(q.active_count(), 0);
        drain(&mut q, &registry, &rt);
        assert!(q
            .transfers()
            .iter()
            .all(|t| t.state == TransferState::Cancelled));
    }

    #[test]
    fn clear_finished_keeps_outstanding_work() {
        let dir = tempfile::tempdir().unwrap();
        let srcs = make_files(dir.path(), 2, 16 * 1024);

        let rt = rt();
        let _guard = rt.enter();
        let registry = BackendRegistry::new();
        let mut q = TransferQueue::default();
        for s in &srcs {
            q.enqueue(
                VPath::local(s),
                VPath::local(dir.path().join("out").join(s.file_name().unwrap())),
                0,
            );
        }
        drain(&mut q, &registry, &rt);
        assert_eq!(q.finished_count(), 2);

        q.enqueue(VPath::local(&srcs[0]), VPath::local("/tmp/x"), 0);
        q.clear_finished();
        assert_eq!(q.finished_count(), 0);
        assert_eq!(q.queued_count(), 1);
    }
}
