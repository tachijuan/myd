//! Backend abstraction: everything the tree and the transfer engine need from a
//! filesystem, whether local or remote.
//!
//! The trait is deliberately narrow — it mirrors the `std::fs` calls the widgets
//! already made rather than trying to model a filesystem in general. Adding a
//! protocol means writing one `impl Vfs` and registering it; no widget changes.

pub mod archive;
mod local;
pub mod ops;
mod path;
pub mod sftp;
pub mod testing;

pub use local::LocalFs;
pub use path::{BackendId, VPath};
pub use sftp::{SftpFs, SftpTarget};

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use crate::utils::sizes::{CancelToken, SizeCache};
use crate::widget::progress::OpProgress;

/// One entry from a directory listing.
///
/// Carries everything the tree and info panel need, because a single remote
/// `READDIR` already returns all of it — re-`stat`ing per row would turn one
/// round trip into hundreds.
#[derive(Debug, Clone)]
pub struct VEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub len: u64,
    pub mtime: Option<std::time::SystemTime>,
    pub atime: Option<std::time::SystemTime>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl VEntry {
    /// A minimal entry, for backends that report little (and for tests).
    pub fn new(name: impl Into<String>, is_dir: bool) -> Self {
        Self {
            name: name.into(),
            is_dir,
            is_symlink: false,
            len: 0,
            mtime: None,
            atime: None,
            mode: None,
            uid: None,
            gid: None,
        }
    }
}

/// Owned metadata for a single path.
///
/// Owned rather than borrowed from `std::fs::Metadata` so a remote backend can
/// produce one; `file_info.rs` renders from this for both local and remote.
#[derive(Debug, Clone, Default)]
pub struct VMetadata {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub len: u64,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mtime: Option<std::time::SystemTime>,
    pub atime: Option<std::time::SystemTime>,
    pub ctime: Option<std::time::SystemTime>,
}

/// A readable byte source. Boxed so backends can return their own stream types.
pub trait VRead: tokio::io::AsyncRead + Send + Unpin {}
impl<T: tokio::io::AsyncRead + Send + Unpin> VRead for T {}

/// A writable byte sink.
pub trait VWrite: tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncWrite + Send + Unpin> VWrite for T {}

/// A handle that reads a file at explicit offsets.
///
/// This is what turns a single large SFTP download from "one round trip per
/// chunk, sequentially" into a deep pipeline. The worker opens several of these
/// on the same file (via repeated [`Vfs::open_positioned_read`]) and reads from
/// all of them at once; the backend's connection pipelines the requests.
#[async_trait]
pub trait VPositionedRead: Send + Sync {
    /// Read starting at `offset`, returning at most `len` bytes.
    ///
    /// A short read is always legal, not just at end-of-file: remote backends cap
    /// one request at whatever the server negotiated, and an implementation is
    /// expected to issue a *single* request rather than looping to fill `len`
    /// (looping would serialise requests that could otherwise overlap). An empty
    /// result means end-of-file. Callers must therefore re-issue from
    /// `offset + got` until they have what they need.
    async fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>>;

    /// Another handle onto the same file, if the backend can make one cheaply.
    ///
    /// Concurrent reads need one handle each. For SFTP a handle can be cloned
    /// client-side for no round trip, so a pool costs one open instead of one per
    /// slot — worth several seconds of dead time on a long link. `None` means the
    /// caller should open handles the ordinary way.
    async fn clone_handle(&self) -> Option<Box<dyn VPositionedRead>> {
        None
    }
}

/// A handle that writes a file at explicit offsets.
///
/// The mirror of [`VPositionedRead`], and it exists for the same reason: a
/// sequential upload alternates read and write and cannot overlap round trips,
/// so on a long link it runs at a fraction of the download rate. SFTP's WRITE
/// carries an explicit offset, so chunks can be written concurrently and out of
/// order.
#[async_trait]
pub trait VPositionedWrite: Send + Sync {
    /// Write all of `data` starting at `offset`.
    ///
    /// Unlike [`VPositionedRead::read_at`] this must write everything or fail —
    /// a partial write with no way to report how much landed would corrupt the
    /// file silently.
    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<()>;

    /// Another handle onto the same file, cheaply if possible.
    async fn clone_handle(&self) -> Option<Box<dyn VPositionedWrite>> {
        None
    }

    /// Close the file, flushing anything outstanding.
    ///
    /// Takes `&self` rather than `self` so the handle can live in a pool behind
    /// an `Arc`; the caller guarantees no writes follow.
    async fn finish(&self) -> Result<()> {
        Ok(())
    }
}

/// A filesystem backend.
///
/// All methods take `&self` and the implementor is `Send + Sync`, so one backend
/// instance is shared behind an `Arc` across the UI and every transfer worker —
/// which for SFTP means one long-lived connection rather than one per operation.
#[async_trait]
pub trait Vfs: Send + Sync {
    /// URL scheme identifying this backend ("file", "sftp"), for display.
    fn scheme(&self) -> &'static str;

    /// A short label for the panel title, e.g. "user@host".
    fn display_name(&self) -> String {
        self.scheme().to_string()
    }

    async fn read_dir(&self, path: &VPath) -> Result<Vec<VEntry>>;
    /// Metadata for `path`, following symlinks — so a link to a directory
    /// reports `is_dir`, which is what browsing wants.
    async fn stat(&self, path: &VPath) -> Result<VMetadata>;

    /// Metadata for the link *itself*, without following it.
    ///
    /// Needed wherever following a link would be destructive: deleting a
    /// symlinked directory must unlink it, not recurse into whatever it points
    /// at. Defaults to [`stat`](Self::stat) for backends that don't distinguish
    /// the two.
    async fn symlink_stat(&self, path: &VPath) -> Result<VMetadata> {
        self.stat(path).await
    }
    async fn create_dir_all(&self, path: &VPath) -> Result<()>;
    async fn remove_file(&self, path: &VPath) -> Result<()>;
    async fn remove_dir(&self, path: &VPath) -> Result<()>;
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<()>;

    /// Set `path`'s permission bits.
    ///
    /// Defaults to refusing, so a backend that cannot do this says so rather
    /// than reporting a success it did not perform — the same reasoning as
    /// [`is_read_only`](Self::is_read_only), where a silent no-op reads as the
    /// change having been made.
    async fn set_mode(&self, path: &VPath, mode: u32) -> Result<()> {
        let _ = (path, mode);
        anyhow::bail!("this backend cannot change permissions")
    }

    /// Set `path`'s owner, group, or both. `None` leaves that one unchanged.
    ///
    /// Both are one call because the underlying operation is one call on every
    /// backend — `chown(2)` and SFTP's SETSTAT each take the pair — and doing
    /// them separately would make a half-applied change possible where the
    /// system offers an all-or-nothing one.
    async fn set_owner(&self, path: &VPath, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
        let _ = (path, uid, gid);
        anyhow::bail!("this backend cannot change ownership")
    }

    async fn open_read(&self, path: &VPath) -> Result<Box<dyn VRead>>;
    /// Open for writing, creating and truncating. `len_hint` lets a backend
    /// preallocate or size its write pipeline.
    async fn open_write(&self, path: &VPath, len_hint: Option<u64>) -> Result<Box<dyn VWrite>>;

    /// Whether this backend benefits from concurrent positioned reads within a
    /// single file. True for SFTP (where pipelining many small reads is the
    /// difference between one round trip per chunk and one round trip total);
    /// false for local, where the kernel already does readahead and a
    /// sequential copy is fastest.
    fn supports_parallel_read(&self) -> bool {
        false
    }

    /// Open a handle for positioned reads, so the transfer worker can issue many
    /// overlapping chunk reads to keep the pipe full.
    ///
    /// Only called when [`supports_parallel_read`](Self::supports_parallel_read)
    /// is true; the default is unreachable for other backends.
    async fn open_positioned_read(&self, _path: &VPath) -> Result<Box<dyn VPositionedRead>> {
        anyhow::bail!("positioned reads not supported by this backend")
    }

    /// Whether this backend benefits from concurrent positioned writes within a
    /// single file — the upload counterpart of
    /// [`supports_parallel_read`](Self::supports_parallel_read).
    ///
    /// True for SFTP, where a sequential upload can only have one write
    /// outstanding at a time and so runs far below the download rate on a long
    /// link. False for local, where the page cache already absorbs writes.
    fn supports_parallel_write(&self) -> bool {
        false
    }

    /// Open a handle for positioned writes.
    ///
    /// Only called when
    /// [`supports_parallel_write`](Self::supports_parallel_write) is true.
    async fn open_positioned_write(
        &self,
        _path: &VPath,
        _len_hint: Option<u64>,
    ) -> Result<Box<dyn VPositionedWrite>> {
        anyhow::bail!("positioned writes not supported by this backend")
    }

    /// Recursive size of a directory.
    ///
    /// Local walks the subtree (`du`-like) and populates `cache` as it goes.
    /// Remote backends are expected to return the shallow stat size instead: a
    /// recursive walk over SFTP is thousands of round trips and would stall the
    /// UI on a deep tree.
    async fn dir_size(
        &self,
        path: &VPath,
        cache: &SizeCache,
        cancel: &CancelToken,
        progress: Option<&OpProgress>,
    ) -> u64;

    /// Whether `dir_size` reports a true recursive total. The treemap uses this
    /// to explain why remote directory tiles are uniformly small.
    fn has_recursive_sizes(&self) -> bool {
        false
    }

    /// Whether reading from this backend crosses a network.
    ///
    /// Distinct from `!BackendId::is_local()`, which asks "is this backend 0"
    /// — an archive is a *registered* backend and so answers no to that, but
    /// its members are read from a file on this machine. The costs that need
    /// guarding (round trips, staging a download before an external renderer
    /// can open it) apply to a server and not to an archive, so anything
    /// pricing those must ask this instead.
    ///
    /// Getting the two confused is a recurring bug: it has variously stopped
    /// the info panel previewing images inside archives, and made a PDF in an
    /// archive report itself as "too large to fetch" when there was nothing to
    /// fetch.
    fn is_remote(&self) -> bool {
        false
    }

    /// Whether this backend refuses every mutation.
    ///
    /// An archive is browsable but not writable. The UI consults this to refuse
    /// the destructive keys up front, because letting the backend fail instead
    /// is invisible: `spawn_delete_batch` discards the error and the row leaves
    /// the tree regardless, so the files would *appear* to have been deleted.
    ///
    /// Distinct from `!BackendId::is_local()`, which several call sites use to
    /// mean "reached over a network". A remote filesystem is writable; an
    /// archive is local and is not.
    fn is_read_only(&self) -> bool {
        false
    }
}

/// The set of live backends, indexed by [`BackendId`].
///
/// Index 0 is always the local filesystem, so `BackendId::LOCAL` resolves
/// without any registration step.
pub struct BackendRegistry {
    backends: Vec<Arc<dyn Vfs>>,
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendRegistry {
    /// A registry holding just the local filesystem.
    pub fn new() -> Self {
        Self {
            backends: vec![Arc::new(LocalFs::new())],
        }
    }

    /// Register a backend and return the id that now refers to it.
    pub fn register(&mut self, backend: Arc<dyn Vfs>) -> BackendId {
        self.backends.push(backend);
        BackendId(self.backends.len() - 1)
    }

    /// Look up a backend. Falls back to local for an out-of-range id so a stale
    /// id can never panic the render loop.
    pub fn get(&self, id: BackendId) -> Arc<dyn Vfs> {
        self.backends
            .get(id.0)
            .cloned()
            .unwrap_or_else(|| self.backends[0].clone())
    }

    pub fn local(&self) -> Arc<dyn Vfs> {
        self.backends[0].clone()
    }

    pub fn len(&self) -> usize {
        self.backends.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_starts_with_local_at_index_zero() {
        let reg = BackendRegistry::new();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get(BackendId::LOCAL).scheme(), "file");
    }

    #[test]
    fn register_returns_usable_id() {
        let mut reg = BackendRegistry::new();
        let id = reg.register(Arc::new(LocalFs::new()));
        assert_eq!(id, BackendId(1));
        assert_eq!(reg.get(id).scheme(), "file");
    }

    #[test]
    fn unknown_id_falls_back_to_local_instead_of_panicking() {
        let reg = BackendRegistry::new();
        assert_eq!(reg.get(BackendId(99)).scheme(), "file");
    }
}
