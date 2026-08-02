//! The bytes an archive is read out of, without holding them all in memory.
//!
//! Every reader here parses a `&[u8]`, which used to mean the container had to
//! be read whole before anything could be indexed — and that read is what
//! forced a size ceiling on opening one at all. A memory map satisfies the same
//! `&[u8]` while leaving the paging to the kernel: a 40GB archive costs address
//! space rather than resident memory, and only the pages actually touched are
//! ever read. Indexing a zip reads its tail; indexing a tar walks the headers
//! and skips the data. Neither faults in the bulk of the file.
//!
//! Not every source can be mapped, so this is an enum rather than a bare `Mmap`:
//! a container arriving over SFTP has no local file to map, and the bsdtar
//! formats hand a path to a child process and need no bytes at all.

use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

/// Bytes to read an archive out of.
///
/// Derefs to `&[u8]`, so a reader takes one without knowing which it got.
#[derive(Clone)]
pub enum Container {
    /// A local file, paged in on demand.
    Mapped(Arc<memmap2::Mmap>),
    /// Bytes already in memory: a container fetched from a remote backend, a
    /// decompressed stream, or a test fixture.
    Owned(Arc<Vec<u8>>),
}

impl Container {
    /// Map a local file.
    ///
    /// The map borrows the file's contents for as long as it lives. A file
    /// truncated or rewritten underneath a live map is undefined behaviour at
    /// the OS level — the risk every mapping reader accepts, and the reason
    /// archives here are read-only.
    pub fn map(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("could not open {}", path.display()))?;
        // SAFETY: see the note above — the mapped file is not written by us,
        // and an archive panel is read-only.
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .with_context(|| format!("could not map {}", path.display()))?;
        Ok(Self::Mapped(Arc::new(mmap)))
    }

    /// Take ownership of bytes already in memory.
    pub fn owned(bytes: Vec<u8>) -> Self {
        Self::Owned(Arc::new(bytes))
    }

    /// An empty container, for the formats that read the file themselves.
    pub fn empty() -> Self {
        Self::Owned(Arc::new(Vec::new()))
    }

    /// Map `path` if it is a local file, falling back to `bytes` otherwise.
    ///
    /// The fallback is what a remote container takes: it was downloaded to be
    /// read, so the bytes are already here and there is no file to map.
    pub fn map_or_owned(path: &Path, bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            if let Ok(mapped) = Self::map(path) {
                return mapped;
            }
        }
        Self::owned(bytes)
    }

    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Mapped(m) => &m[..],
            Self::Owned(v) => &v[..],
        }
    }
}

impl Deref for Container {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// An `AsyncRead` over one member's run of bytes inside a container.
///
/// Holds the container alive and a cursor into it, so reading copies out of the
/// mapping a buffer at a time rather than materialising the member first. That
/// is what lets a multi-gigabyte stored member extract in constant memory.
pub struct SliceReader {
    container: Container,
    pos: usize,
    end: usize,
}

impl SliceReader {
    pub fn new(container: Container, start: usize, end: usize) -> Self {
        Self {
            container,
            pos: start,
            end,
        }
    }

    /// Bytes not yet read.
    pub fn remaining(&self) -> usize {
        self.end.saturating_sub(self.pos)
    }
}

impl tokio::io::AsyncRead for SliceReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let take = this.remaining().min(buf.remaining());
        if take > 0 {
            // Copying from a mapping can fault, which blocks the thread for as
            // long as the page takes to arrive. Reads run on the blocking pool
            // for exactly that reason (see `open_read`), so this is where the
            // waiting is supposed to happen.
            buf.put_slice(&this.container[this.pos..this.pos + take]);
            this.pos += take;
        }
        std::task::Poll::Ready(Ok(()))
    }
}

impl std::fmt::Debug for Container {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Mapped(_) => "mapped",
            Self::Owned(_) => "owned",
        };
        write!(f, "Container::{kind}({} bytes)", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_mapped_file_reads_as_its_bytes() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello archive").unwrap();
        f.flush().unwrap();

        let c = Container::map(f.path()).unwrap();
        assert_eq!(&c[..], b"hello archive");
        assert_eq!(c.len(), 13);
    }

    #[test]
    fn owned_and_mapped_are_interchangeable_as_slices() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"same bytes").unwrap();
        f.flush().unwrap();

        let mapped = Container::map(f.path()).unwrap();
        let owned = Container::owned(b"same bytes".to_vec());
        assert_eq!(mapped.as_slice(), owned.as_slice());
    }

    #[test]
    fn map_or_owned_prefers_the_file_when_there_are_no_bytes() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"on disk").unwrap();
        f.flush().unwrap();

        // No bytes in hand: the file is mapped rather than read.
        let c = Container::map_or_owned(f.path(), Vec::new());
        assert!(matches!(c, Container::Mapped(_)));
        assert_eq!(&c[..], b"on disk");

        // Bytes in hand — a remote container — are used as they are.
        let c = Container::map_or_owned(f.path(), b"from the wire".to_vec());
        assert!(matches!(c, Container::Owned(_)));
        assert_eq!(&c[..], b"from the wire");
    }

    /// A container that cannot be mapped falls back rather than failing.
    #[test]
    fn a_missing_file_falls_back_to_the_bytes() {
        let c = Container::map_or_owned(std::path::Path::new("/no/such/file"), Vec::new());
        assert!(c.is_empty(), "nothing to map and nothing given");
    }
}
