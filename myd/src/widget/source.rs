//! Synchronous filesystem facade for the file tree.
//!
//! The tree's expand/sort machinery is synchronous and already runs inside
//! `spawn_blocking` on the loading path. Rather than make the whole widget
//! async, it talks to a [`Source`]: for the local filesystem that's `std::fs`
//! directly (so nothing about local browsing changes), and for a remote backend
//! it's a thin blocking bridge onto the async [`Vfs`](crate::vfs::Vfs).
//!
//! This is the seam that makes the tree protocol-agnostic — a new backend needs
//! no tree changes, only a `Vfs` impl.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::utils::sizes::{self, CancelToken, SizeCache};
use crate::vfs::{BackendId, VEntry, VPath, Vfs};
use crate::widget::progress::OpProgress;

/// A directory entry as the tree needs it: a full path, whether it's a
/// directory, and its size — all from the single directory listing, so no
/// second stat is needed per entry.
pub struct SourceEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Size reported by the listing. For a file this is the real size; for a
    /// directory it's the shallow size the listing gives (0 on most backends).
    /// `None` means the listing didn't include a size and the tree should fetch
    /// one itself (local dirs, which need a recursive walk).
    pub len: Option<u64>,
    /// Modification and access times from the listing, for time-based sorting.
    /// Both come from the single directory read, so sorting by them needs no
    /// extra I/O — important on a remote backend.
    pub mtime: Option<std::time::SystemTime>,
    pub atime: Option<std::time::SystemTime>,
    /// Whether the entry is a symlink. `is_dir` describes the *target*, so a
    /// symlinked directory is traversable; this flag only drives display.
    pub is_symlink: bool,
    /// Unix mode bits, when the listing reports them. `None` on a backend that
    /// doesn't, or on a non-unix host — rendered as a placeholder rather than
    /// guessed at.
    pub mode: Option<u32>,
}

/// Unix mode bits from local metadata, or `None` off unix.
#[cfg(unix)]
pub(crate) fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode())
}

#[cfg(not(unix))]
pub(crate) fn mode_of(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Where a tree's data comes from.
///
/// `Local` keeps the original direct-`std::fs` path so local browsing is
/// byte-for-byte unchanged. `Remote` bridges to an async `Vfs` via a runtime on
/// a dedicated thread (see [`RemoteSource`]).
#[derive(Clone, Default)]
pub enum Source {
    #[default]
    Local,
    /// A local filesystem browsed *without* measuring directory sizes.
    ///
    /// The recursive walk is the slowest thing this app does; on a network mount
    /// or a directory of millions of files it is not worth waiting for merely to
    /// look around. Carried on the source rather than passed down through
    /// `load_children` and its callers, because this is precisely a statement
    /// about how sizes are obtained.
    LocalShallow,
    Remote(RemoteSource),
}

impl Source {
    /// This source with directory measuring turned on or off.
    ///
    /// A remote source is returned unchanged: it has no recursive walk to skip,
    /// and pretending otherwise would let the UI offer a toggle that does
    /// nothing.
    pub fn with_shallow(&self, shallow: bool) -> Self {
        match (self, shallow) {
            (Source::Local | Source::LocalShallow, true) => Source::LocalShallow,
            (Source::Local | Source::LocalShallow, false) => Source::Local,
            (Source::Remote(r), _) => Source::Remote(r.clone()),
        }
    }

    /// Whether this source skips the directory-measuring walk.
    pub fn is_shallow(&self) -> bool {
        matches!(self, Source::LocalShallow)
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Local => write!(f, "Source::Local"),
            Source::LocalShallow => write!(f, "Source::LocalShallow"),
            Source::Remote(r) => write!(f, "Source::Remote({:?})", r.backend),
        }
    }
}

/// A remote backend plus a runtime to block on its async calls.
///
/// The tree runs synchronously, so it needs a way to drive async `Vfs` calls to
/// completion. It can't just `block_on` a runtime from the caller's thread: the
/// tree is reached from *within* the app's async event loop (via cloned
/// `Source`s in the interactive methods), and blocking on — or dropping — a
/// runtime inside an async context panics.
///
/// So the runtime lives on its own dedicated thread and calls are dispatched to
/// it. The runtime is only ever dropped on that thread, never in an async
/// context. The whole thing is shared behind an `Arc`, so cloning a `Source`
/// (which the tree does constantly) is cheap and reuses the one live SFTP
/// connection.
#[derive(Clone)]
pub struct RemoteSource {
    backend: BackendId,
    vfs: Arc<dyn Vfs>,
    driver: Arc<Driver>,
}

/// Owns a multi-thread runtime on a private thread and runs futures on it.
struct Driver {
    handle: tokio::runtime::Handle,
    // Kept so the runtime is shut down when the last `RemoteSource` drops. The
    // shutdown happens on the driver thread, never in an async context.
    _shutdown: DriverShutdown,
}

struct DriverShutdown {
    tx: Option<std::sync::mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DriverShutdown {
    fn drop(&mut self) {
        // Signal the driver thread to exit; it drops the runtime there.
        drop(self.tx.take());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl RemoteSource {
    /// Build a remote source with its own runtime thread.
    pub fn new(backend: BackendId, vfs: Arc<dyn Vfs>) -> std::io::Result<Self> {
        let (handle_tx, handle_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

        let join = std::thread::Builder::new()
            .name("myd-sftp-rt".into())
            .spawn(move || {
                // A dedicated single-worker multi-thread runtime: it drives its
                // own spawned tasks (a current-thread runtime would need someone
                // to call block_on to make progress, which is exactly what we're
                // avoiding). One worker is plenty — SFTP calls are I/O-bound and
                // the pipelining lives inside the sftp client.
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(_) => return,
                };
                // Hand the handle back, then keep the runtime alive until asked
                // to stop. It is dropped here, on this thread — safe.
                let _ = handle_tx.send(rt.handle().clone());
                let _ = stop_rx.recv();
                // rt drops here, on this thread, never in an async context.
            })?;

        let handle = handle_rx
            .recv()
            .map_err(|_| std::io::Error::other("sftp runtime thread failed to start"))?;

        Ok(Self {
            backend,
            vfs,
            driver: Arc::new(Driver {
                handle,
                _shutdown: DriverShutdown {
                    tx: Some(stop_tx),
                    join: Some(join),
                },
            }),
        })
    }

    /// Run a future to completion on the driver runtime and return its output.
    ///
    /// The future is spawned onto the driver's own thread and the caller waits
    /// on a channel. This is safe to call from anywhere — including from within
    /// the app's async event loop — because the caller only ever blocks a
    /// channel `recv`, never a runtime, and the future runs on a different
    /// thread. That matters: the tree's interactive navigation is driven
    /// synchronously from inside the async loop.
    fn block<F>(&self, what: &'static str, fut: F) -> F::Output
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.driver.handle.spawn(async move {
            let _ = tx.send(fut.await);
        });
        // Every remote round trip funnels through here, and a slow one on the
        // event-loop thread is indistinguishable from a hang. Logging which call
        // blocked, and for how long, names the culprit instead of leaving it to
        // inference.
        let started = crate::app::trace_enabled().then(std::time::Instant::now);
        let out = rx
            .recv()
            .expect("sftp runtime dropped a request without completing it");
        if let Some(started) = started {
            let elapsed = started.elapsed();
            if elapsed > std::time::Duration::from_millis(100) {
                crate::app::trace_note(format_args!(
                    "  remote {} blocked {:.1}ms",
                    what,
                    elapsed.as_secs_f64() * 1000.0,
                ));
            }
        }
        out
    }

    /// List a remote directory (owned inputs, so the future is `'static`).
    fn read_dir(&self, path: VPath) -> anyhow::Result<Vec<VEntry>> {
        let vfs = self.vfs.clone();
        self.block("read_dir", async move { vfs.read_dir(&path).await })
    }

    /// Stat a remote path.
    fn stat(&self, path: VPath) -> anyhow::Result<crate::vfs::VMetadata> {
        let vfs = self.vfs.clone();
        self.block("stat", async move { vfs.stat(&path).await })
    }

    /// Create a directory (and any missing parents).
    fn create_dir_all(&self, path: VPath) -> anyhow::Result<()> {
        let vfs = self.vfs.clone();
        self.block("create_dir_all", async move { vfs.create_dir_all(&path).await })
    }

    /// Directory size (shallow for SFTP).
    fn dir_size(
        &self,
        path: VPath,
        cache: SizeCache,
        cancel: CancelToken,
        progress: Option<OpProgress>,
    ) -> u64 {
        let vfs = self.vfs.clone();
        self.block("dir_size", async move {
            vfs.dir_size(&path, &cache, &cancel, progress.as_ref())
                .await
        })
    }
}

impl Source {
    /// The backend id these paths belong to (local = [`BackendId::LOCAL`]).
    pub fn backend(&self) -> BackendId {
        match self {
            Source::Local | Source::LocalShallow => BackendId::LOCAL,
            Source::Remote(r) => r.backend,
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Source::Remote(_))
    }

    /// Wrap a path as a [`VPath`] on this source's backend.
    fn vpath(&self, path: &Path) -> VPath {
        VPath::new(self.backend(), path.to_path_buf())
    }

    /// List a directory's children.
    ///
    /// Local reads with `std::fs` and follows the hidden-file rules exactly as
    /// before. Remote issues one `READDIR`.
    pub fn read_dir(&self, dir: &Path) -> Vec<SourceEntry> {
        match self {
            Source::Local | Source::LocalShallow => {
                let mut out = Vec::new();
                if let Ok(rd) = std::fs::read_dir(dir) {
                    for entry in rd.flatten() {
                        let path = entry.path();
                        // `file_type()` here is the link's own type (lstat); the
                        // target decides whether it's traversable.
                        let is_symlink =
                            entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
                        let is_dir = match entry.file_type() {
                            Ok(t) if t.is_symlink() => path.is_dir(),
                            Ok(t) => t.is_dir(),
                            Err(_) => path.is_dir(),
                        };
                        // One metadata read serves size, timestamps and mode.
                        // `DirEntry::metadata` does not follow symlinks, so a
                        // link reports its own `lrwxrwxrwx` — what `ls -l` shows.
                        let meta = entry.metadata().ok();
                        // Local directories need a recursive walk for their real
                        // size (done later, cached); leave `len` unset for them.
                        // Files can take the size from the metadata we already
                        // have here.
                        let len = if is_dir {
                            None
                        } else {
                            meta.as_ref().map(|m| m.len())
                        };
                        out.push(SourceEntry {
                            path,
                            is_dir,
                            len,
                            mtime: meta.as_ref().and_then(|m| m.modified().ok()),
                            atime: meta.as_ref().and_then(|m| m.accessed().ok()),
                            is_symlink,
                            mode: meta.as_ref().and_then(mode_of),
                        });
                    }
                }
                out
            }
            Source::Remote(r) => {
                let vdir = self.vpath(dir);
                match r.read_dir(vdir) {
                    // The remote listing already carries every entry's size and
                    // times, so take them here — a per-entry stat would be one
                    // network round trip each, which is exactly what froze the UI.
                    Ok(entries) => entries
                        .into_iter()
                        .map(|e: VEntry| SourceEntry {
                            path: dir.join(&e.name),
                            is_dir: e.is_dir,
                            len: Some(e.len),
                            mtime: e.mtime,
                            atime: e.atime,
                            is_symlink: e.is_symlink,
                            mode: e.mode,
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                }
            }
        }
    }

    /// Whether `path` is a directory.
    pub fn is_dir(&self, path: &Path) -> bool {
        match self {
            Source::Local | Source::LocalShallow => path.is_dir(),
            Source::Remote(r) => r.stat(self.vpath(path)).map(|m| m.is_dir).unwrap_or(false),
        }
    }

    /// Create a directory (with any missing parents) on whichever backend this
    /// source represents.
    ///
    /// Blocking, but bounded: one mkdir is a single round trip, unlike the
    /// per-entry walks that must stay off the event loop.
    pub fn create_dir_all(&self, path: &Path) -> anyhow::Result<()> {
        match self {
            Source::Local | Source::LocalShallow => Ok(std::fs::create_dir_all(path)?),
            Source::Remote(r) => r.create_dir_all(self.vpath(path)),
        }
    }

    /// Size of a single file.
    pub fn file_size(&self, path: &Path) -> u64 {
        match self {
            Source::Local | Source::LocalShallow => sizes::get_file_size(path),
            Source::Remote(r) => r.stat(self.vpath(path)).map(|m| m.len).unwrap_or(0),
        }
    }

    /// Size of a directory.
    ///
    /// Local computes a recursive `du`-style total (and caches every
    /// descendant). Remote returns the directory's own size only — a recursive
    /// walk over SFTP is one round trip per directory and would stall the UI, so
    /// remote directory sizes fill in as the user expands.
    pub fn dir_size(
        &self,
        path: &Path,
        cache: &SizeCache,
        cancel: Option<&CancelToken>,
        progress: Option<&OpProgress>,
    ) -> u64 {
        match self {
            Source::Local => match cancel {
                Some(c) => {
                    sizes::get_dir_size_caching_cancellable_progress(path, cache, c, progress)
                }
                None => sizes::get_dir_size_caching(path, cache),
            },
            // The whole point of shallow mode: no walk at all. Zero rather than
            // the directory inode's own length, so the size reads as *unknown*
            // and is displayed and sorted as such — a plausible-looking 4 KB
            // would be worse than an honest dash.
            Source::LocalShallow => 0,
            Source::Remote(r) => {
                let cancel = cancel.cloned().unwrap_or_default();
                r.dir_size(self.vpath(path), cache.clone(), cancel, progress.cloned())
            }
        }
    }

    /// Whether directory sizes from this source *can* be true recursive totals.
    ///
    /// A property of the backend, not of the user's preference: a local
    /// filesystem can be walked, a remote one cannot afford to be. Whether the
    /// walk actually happens is [`FileTree::measures_directories`], which also
    /// consults the tree's shallow toggle.
    pub fn has_recursive_sizes(&self) -> bool {
        match self {
            Source::Local => true,
            Source::LocalShallow => false,
            Source::Remote(r) => r.vfs.has_recursive_sizes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_source_reads_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();

        let src = Source::Local;
        assert_eq!(src.backend(), BackendId::LOCAL);
        assert!(!src.is_remote());
        assert!(src.has_recursive_sizes());

        let mut entries = src.read_dir(dir.path());
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(entries.len(), 2);
        assert!(src.is_dir(&dir.path().join("sub")));
        assert!(!src.is_dir(&dir.path().join("a.txt")));
        assert_eq!(src.file_size(&dir.path().join("a.txt")), 2);
    }

    #[test]
    fn local_dir_size_is_recursive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/f.bin"), vec![0u8; 300]).unwrap();
        let size = Source::Local.dir_size(dir.path(), &SizeCache::new(), None, None);
        assert_eq!(size, 300);
    }

    #[test]
    fn remote_source_bridges_to_a_vfs() {
        // Use LocalFs as a stand-in Vfs to prove the blocking bridge works
        // without needing a network. (Real remote coverage is in the gated SFTP
        // integration test.)
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.bin"), vec![0u8; 42]).unwrap();
        std::fs::create_dir_all(dir.path().join("d")).unwrap();

        let vfs: Arc<dyn Vfs> = Arc::new(crate::vfs::LocalFs::new());
        let src = Source::Remote(RemoteSource::new(BackendId(1), vfs).unwrap());

        assert!(src.is_remote());
        assert_eq!(src.backend(), BackendId(1));

        let entries = src.read_dir(dir.path());
        assert_eq!(entries.len(), 2);
        assert!(src.is_dir(&dir.path().join("d")));
        assert_eq!(src.file_size(&dir.path().join("x.bin")), 42);
    }
}
