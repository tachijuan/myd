//! The [`Vfs`] implementation that makes an archive browsable.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use crate::utils::sizes::{CancelToken, SizeCache};
use crate::vfs::{VEntry, VMetadata, VPath, VRead, VWrite, Vfs};
use crate::widget::progress::OpProgress;

use super::container::Container;
use super::format::ArchiveFormat;
use super::index::{ArchiveIndex, MemberLocator};
use super::Opened;

/// An archive, presented as a filesystem.
///
/// Paths are relative to the archive root, so `/docs/readme.md` names a member
/// and the container itself is remembered here rather than spelled into every
/// path. One of these is registered per opened archive, exactly as one SFTP
/// backend is registered per connection.
pub struct ArchiveFs {
    /// The container's own name, for the panel title and error messages.
    label: String,
    format: ArchiveFormat,
    index: Arc<ArchiveIndex>,
    /// Bytes that stream offsets point into: the container for a plain tar, the
    /// decompressed stream for a compressed one.
    stream: Option<Container>,
    /// The container's bytes, kept for formats that seek into them per read.
    ///
    /// A local container is mapped rather than read, so "kept" costs address
    /// space rather than memory however large the archive is.
    container: Option<Container>,
    /// Where the container came from, so a member read can say so.
    origin: PathBuf,
}

impl ArchiveFs {
    /// Read and index a container.
    ///
    /// Blocking and CPU-bound — decompressing a large tarball takes seconds —
    /// so callers run it on the blocking pool. That is also where the loading
    /// screen's spinner and cancel token already are.
    pub fn open(bytes: Vec<u8>, format: ArchiveFormat, origin: PathBuf) -> Result<Self> {
        // Empty bytes mean "it is on this machine, go and get it": the local
        // container is mapped rather than read, which is what lets an archive of
        // any size be opened at all. Bytes in hand are a container that came
        // from somewhere unmappable and are used as they are.
        Self::open_container(Container::map_or_owned(&origin, bytes), format, origin)
    }

    /// As [`Self::open`], over a container that has already been obtained.
    pub fn open_container(
        container: Container,
        format: ArchiveFormat,
        origin: PathBuf,
    ) -> Result<Self> {
        let label = origin
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| origin.display().to_string());

        let Opened { index, stream } = super::open(&container, &origin, format, usize::MAX)
            .with_context(|| format!("could not read {label}"))?;

        // A zip seeks into the container per read; a plain tar's offsets point
        // into it too. A compressed tar has its own decompressed stream and the
        // container is dead weight once indexed.
        let container = match format {
            ArchiveFormat::Zip
            | ArchiveFormat::Tar
            | ArchiveFormat::SevenZ
            | ArchiveFormat::Rar
            | ArchiveFormat::Single(_) => Some(container),
            // A compressed tar has its own decompressed stream, and the
            // bsdtar-backed formats re-read the file per member — neither has
            // any use for a second copy of the container.
            ArchiveFormat::TarCompressed(_) | ArchiveFormat::Libarchive(_) => None,
        };

        Ok(Self {
            label,
            format,
            index: Arc::new(index),
            stream,
            container,
            origin,
        })
    }

    /// The archive's own name, e.g. `photos.zip`.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Where the container lives.
    pub fn origin(&self) -> &std::path::Path {
        &self.origin
    }

    pub fn index(&self) -> &ArchiveIndex {
        &self.index
    }

    /// A member that can be read without decompressing it first.
    ///
    /// A stored member of a mapped container — every member of a plain tar, and
    /// a zip entry written without compression — is already a contiguous run of
    /// bytes in the file. Handing back a reader over that slice lets an extract
    /// of any size run in constant memory, since the kernel pages the run in as
    /// it is copied out and drops it again behind.
    ///
    /// `None` when the member has to go through a decoder, which is the case
    /// this cannot help with: a deflate stream is not addressable by offset.
    fn slice_of(&self, path: &std::path::Path) -> Option<(Container, usize, usize)> {
        let node = self.index.get(path)?;
        if node.is_dir {
            return None;
        }
        // `Single` is a compressed whole and has to be decoded, even though its
        // locator looks like an offset into the container.
        if matches!(self.format, ArchiveFormat::Single(_)) {
            return None;
        }
        let MemberLocator::StreamOffset { offset, len } = node.locator else {
            return None;
        };
        let bytes = self.stream.as_ref().or(self.container.as_ref())?;
        let (start, end) = (offset as usize, offset.checked_add(len)? as usize);
        if end > bytes.len() {
            return None;
        }
        Some((bytes.clone(), start, end))
    }

    /// A reader that inflates a compressed zip member as it is read.
    ///
    /// `None` unless this is a zip member with a decoder — everything else
    /// either slices the container directly or has no streaming form here.
    fn stream_zip_member(&self, path: &std::path::Path) -> Option<ZipMemberReader> {
        if self.format != ArchiveFormat::Zip {
            return None;
        }
        let node = self.index.get(path)?;
        if node.is_dir {
            return None;
        }
        let MemberLocator::Index(i) = node.locator else {
            return None;
        };
        Some(ZipMemberReader::new(
            self.container.as_ref()?.clone(),
            i,
            node.len,
        ))
    }

    /// Decompress one member into memory.
    ///
    /// Returns an owned buffer rather than a reader borrowing the archive, so
    /// nothing holds a lock across an await and concurrent extracts of
    /// different members cannot serialise on each other.
    ///
    /// Used for members that need a decoder. A member that is merely stored
    /// goes through [`Self::slice_of`] instead and is never buffered.
    fn read_member_blocking(&self, path: &std::path::Path) -> Result<Vec<u8>> {
        use std::io::Read;

        let node = self
            .index
            .get(path)
            .with_context(|| format!("no such member: {}", path.display()))?;
        if node.is_dir {
            bail!("{} is a directory", path.display());
        }

        // The bsdtar-backed formats have no locator: the tool is asked for the
        // member by name and re-reads the container itself. That is a process
        // per member, which is why these are the formats of last resort.
        if matches!(self.format, ArchiveFormat::Libarchive(_)) {
            return super::libarchive_reader::read_member_via_bsdtar(
                &self.origin,
                &node.stored_path,
                self.format,
            );
        }

        match node.locator {
            // 7z is keyed on the stored name rather than the ordinal, and
            // decompresses whole blocks that may span members — so it gets its
            // own reader rather than sharing the zip path.
            MemberLocator::Index(_) if self.format == ArchiveFormat::SevenZ => {
                let container = self
                    .container
                    .as_ref()
                    .context("this archive's container was not kept")?;
                super::sevenz_reader::read_member(container, &node.stored_path)
            }
            MemberLocator::Index(i) => {
                let container = self
                    .container
                    .as_ref()
                    .context("this archive's container was not kept")?;
                let mut zip = zip::ZipArchive::new(std::io::Cursor::new(container.as_slice()))?;
                let mut entry = zip.by_index(i)?;
                let mut out = Vec::with_capacity(node.len as usize);
                // Bounded by the declared size rather than trusting the stream:
                // a header that lies about its length would otherwise decompress
                // without limit.
                entry.by_ref().take(node.len).read_to_end(&mut out)?;
                Ok(out)
            }
            MemberLocator::StreamOffset { offset, len } => {
                // A single-member container is its own decompressed stream, and
                // holding a second copy of it just to slice would double the
                // memory for no gain.
                if let ArchiveFormat::Single(how) = self.format {
                    let container = self
                        .container
                        .as_ref()
                        .context("this archive's container was not kept")?;
                    return super::tar_reader::decompress(container, how);
                }
                let stream = self
                    .stream
                    .as_ref()
                    .or(self.container.as_ref())
                    .context("this archive's bytes were not kept")?;
                let (start, end) = (offset as usize, (offset + len) as usize);
                let bytes = stream
                    .get(start..end)
                    .context("this member points outside the archive")?;
                Ok(bytes.to_vec())
            }
            MemberLocator::ByName => {
                // By path where the container is a local file, for the same
                // reason indexing prefers it: handed a slice, the crate copies
                // the whole archive before extracting anything.
                if self.origin.is_file() {
                    return super::rar_reader::read_member_at(
                        &self.origin,
                        &node.stored_path,
                        node.len,
                    );
                }
                let container = self
                    .container
                    .as_ref()
                    .context("this archive's container was not kept")?;
                super::rar_reader::read_member(container, &node.stored_path, node.len)
            }
            MemberLocator::None => Ok(Vec::new()),
        }
    }
}

/// Inflates one zip member on demand.
///
/// A compressed member has to be decoded from its start, so reading the head of
/// one used to mean decoding all of it: previewing a 1.3GB video inside an
/// archive decompressed the whole member to sniff its first 8KB. This decodes
/// only as far as it is read.
///
/// The decoder runs on its own thread and hands chunks over a bounded channel.
/// That is what keeps a *sequential* read linear — the alternative, re-opening
/// the entry and skipping forward per refill, is quadratic over a large member —
/// and the bound is what keeps memory flat: the decoder blocks once a couple of
/// chunks are outstanding, so a member larger than memory streams fine. Dropping
/// the reader closes the channel, and the decoder stops at its next send rather
/// than running to completion for a preview nobody is waiting for.
struct ZipMemberReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    /// The chunk being handed out, and how much of it has gone.
    current: Vec<u8>,
    offset: usize,
    done: bool,
}

/// Bytes per decoded chunk, and how many may be outstanding. Two chunks in
/// flight is enough to keep the decoder busy without holding much.
const CHUNK: usize = 256 * 1024;
const CHUNKS_IN_FLIGHT: usize = 2;

impl ZipMemberReader {
    fn new(container: Container, index: usize, len: u64) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(CHUNKS_IN_FLIGHT);

        std::thread::spawn(move || {
            use std::io::Read;

            let decode = || -> std::io::Result<()> {
                let mut zip = zip::ZipArchive::new(std::io::Cursor::new(container.as_slice()))
                    .map_err(std::io::Error::other)?;
                let entry = zip.by_index(index).map_err(std::io::Error::other)?;
                // Bounded by the declared size rather than trusting the stream:
                // a header that lies about its length would otherwise decode
                // without limit.
                let mut entry = entry.take(len);
                loop {
                    let mut buf = vec![0u8; CHUNK];
                    let mut filled = 0;
                    // `read` may return short; fill the chunk before sending so
                    // the channel carries whole buffers rather than dribbles.
                    while filled < buf.len() {
                        match entry.read(&mut buf[filled..]) {
                            Ok(0) => break,
                            Ok(n) => filled += n,
                            Err(e) => return Err(e),
                        }
                    }
                    if filled == 0 {
                        return Ok(());
                    }
                    buf.truncate(filled);
                    // A closed channel means the reader was dropped: stop
                    // decoding rather than finish a member nobody wants.
                    if tx.blocking_send(Ok(buf)).is_err() {
                        return Ok(());
                    }
                }
            };

            if let Err(e) = decode() {
                let _ = tx.blocking_send(Err(e));
            }
        });

        Self {
            rx,
            current: Vec::new(),
            offset: 0,
            done: false,
        }
    }
}

impl tokio::io::AsyncRead for ZipMemberReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::task::Poll;

        let this = self.get_mut();
        if this.offset >= this.current.len() {
            if this.done {
                return Poll::Ready(Ok(()));
            }
            match this.rx.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                // Channel closed: the decoder finished or stopped.
                Poll::Ready(None) => {
                    this.done = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(e))) => {
                    this.done = true;
                    return Poll::Ready(Err(e));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    this.current = chunk;
                    this.offset = 0;
                }
            }
        }

        let take = (this.current.len() - this.offset).min(out.remaining());
        out.put_slice(&this.current[this.offset..this.offset + take]);
        this.offset += take;
        Poll::Ready(Ok(()))
    }
}

/// An archive is browsable but never writable.
///
/// There is no in-place edit of a zip worth having, and a delete that fails
/// silently is worse than one that is refused: `spawn_delete_batch` discards
/// the error and the row leaves the tree either way, so the file would *appear*
/// to be gone. The UI consults [`Vfs::is_read_only`] to refuse first; these
/// bails are the second line, so a missed guard fails loudly in a test rather
/// than looking like it worked.
fn read_only<T>(what: &str) -> Result<T> {
    bail!("cannot {what}: archives are read-only. Copy it out first (c).")
}

#[async_trait]
impl Vfs for ArchiveFs {
    fn scheme(&self) -> &'static str {
        "archive"
    }

    fn display_name(&self) -> String {
        self.label.clone()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn read_dir(&self, path: &VPath) -> Result<Vec<VEntry>> {
        let children = self
            .index
            .children_of(&path.path)
            .with_context(|| format!("no such directory in {}: {}", self.label, path.path.display()))?;

        Ok(children
            .into_iter()
            .filter_map(|node| {
                Some(VEntry {
                    name: node.path.file_name()?.to_string_lossy().to_string(),
                    is_dir: node.is_dir,
                    is_symlink: node.is_symlink,
                    // The recursive total, so directory sizes inside an archive
                    // are real rather than the zeroes a remote listing gives.
                    // The tree takes this straight from the listing and never
                    // asks for a size again.
                    len: if node.is_dir {
                        node.recursive_len
                    } else {
                        node.len
                    },
                    mtime: node.mtime,
                    atime: None,
                    mode: node.mode,
                    // No archive format records these as numbers a local
                    // password database could resolve.
                    uid: None,
                    gid: None,
                })
            })
            .collect())
    }

    async fn stat(&self, path: &VPath) -> Result<VMetadata> {
        let node = self
            .index
            .get(&path.path)
            .with_context(|| format!("no such member in {}: {}", self.label, path.path.display()))?;
        Ok(VMetadata {
            is_dir: node.is_dir,
            is_symlink: node.is_symlink,
            len: if node.is_dir {
                node.recursive_len
            } else {
                node.len
            },
            mode: node.mode,
            mtime: node.mtime,
            ..Default::default()
        })
    }

    async fn create_dir_all(&self, _path: &VPath) -> Result<()> {
        read_only("create a directory")
    }

    async fn remove_file(&self, _path: &VPath) -> Result<()> {
        read_only("delete a file")
    }

    async fn remove_dir(&self, _path: &VPath) -> Result<()> {
        read_only("delete a directory")
    }

    async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<()> {
        read_only("rename")
    }

    async fn open_write(&self, _path: &VPath, _len_hint: Option<u64>) -> Result<Box<dyn VWrite>> {
        read_only("write")
    }

    async fn open_read(&self, path: &VPath) -> Result<Box<dyn VRead>> {
        // A stored member is already a run of bytes in the container, so it is
        // read straight out of the mapping. Nothing is buffered and nothing is
        // decompressed, so a member larger than memory extracts fine.
        if let Some((container, start, end)) = self.slice_of(&path.path) {
            return Ok(Box::new(super::container::SliceReader::new(
                container, start, end,
            )));
        }

        // A deflated zip member decodes as a stream, so a reader that pulls from
        // it delivers the first bytes immediately and never holds the whole
        // member. Buffering it whole instead meant a 1.3GB video inside an
        // archive was fully decompressed just so the preview could sniff its
        // first 8KB and report "binary file" — half a second of work, and an
        // allocation the size of the member.
        if let Some(reader) = self.stream_zip_member(&path.path) {
            return Ok(Box::new(reader));
        }
        // Everything else needs a decoder, which is CPU work: on an async worker
        // it would block every other task on that thread, and an extract runs
        // several at once.
        let this = self.clone_for_read();
        let target = path.path.clone();
        let bytes =
            tokio::task::spawn_blocking(move || this.read_member_blocking(&target)).await??;
        Ok(Box::new(std::io::Cursor::new(bytes)))
    }

    async fn dir_size(
        &self,
        path: &VPath,
        _cache: &SizeCache,
        _cancel: &CancelToken,
        _progress: Option<&OpProgress>,
    ) -> u64 {
        // Already known: the index summed every subtree when it was built, so
        // this is a lookup rather than a walk.
        self.index
            .get(&path.path)
            .map(|n| n.recursive_len)
            .unwrap_or(0)
    }

    fn has_recursive_sizes(&self) -> bool {
        // The one place an archive should behave unlike a remote backend. Every
        // member's size is in the index, so size bars, size sorting and the
        // treemap all mean something inside an archive.
        true
    }
}

impl ArchiveFs {
    /// A handle sharing this archive's indexed state, for use on a worker.
    ///
    /// The index and byte buffers are behind `Arc`s precisely so a blocking
    /// task can take a cheap handle rather than borrowing across the await.
    fn clone_for_read(&self) -> Self {
        Self {
            label: self.label.clone(),
            format: self.format,
            index: Arc::clone(&self.index),
            stream: self.stream.clone(),
            container: self.container.clone(),
            origin: self.origin.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::archive::format::Compression;
    use crate::vfs::BackendId;
    use std::path::Path;

    fn open_fixture_zip() -> ArchiveFs {
        let bytes = crate::vfs::archive::zip_reader::tests::fixture();
        ArchiveFs::open(bytes, ArchiveFormat::Zip, PathBuf::from("/tmp/fixture.zip")).unwrap()
    }

    fn vp(p: &str) -> VPath {
        VPath::new(BackendId(1), PathBuf::from(p))
    }

    #[tokio::test]
    async fn read_dir_and_stat_agree_with_the_index() {
        let fs = open_fixture_zip();

        let mut root: Vec<String> = fs
            .read_dir(&vp("/"))
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        root.sort();
        assert_eq!(root, vec!["docs", "run.sh"]);

        let run = fs.stat(&vp("/run.sh")).await.unwrap();
        assert!(!run.is_dir);
        assert_eq!(run.len, 18);
        assert_eq!(run.mode.map(|m| m & 0o777), Some(0o755));
    }

    #[tokio::test]
    async fn a_directory_reports_its_recursive_size() {
        // Unlike a remote backend, which cannot afford to measure.
        let fs = open_fixture_zip();
        let docs = fs.stat(&vp("/docs")).await.unwrap();
        assert!(docs.is_dir);
        assert_eq!(docs.len, 8);
        assert!(fs.has_recursive_sizes());

        let listed = fs.read_dir(&vp("/")).await.unwrap();
        let docs = listed.iter().find(|e| e.name == "docs").unwrap();
        assert_eq!(docs.len, 8, "the listing carries the size the tree will use");
    }

    #[tokio::test]
    async fn open_read_returns_the_member_bytes() {
        use tokio::io::AsyncReadExt;
        let fs = open_fixture_zip();
        let mut reader = fs.open_read(&vp("/run.sh")).await.unwrap();
        let mut got = String::new();
        reader.read_to_string(&mut got).await.unwrap();
        assert_eq!(got, "#!/bin/sh\necho hi\n");
    }

    #[tokio::test]
    async fn reading_a_member_of_a_compressed_tar_works() {
        let bytes = crate::vfs::archive::tar_reader::tests::fixture_tgz();
        let fs = ArchiveFs::open(
            bytes,
            ArchiveFormat::TarCompressed(Compression::Gzip),
            PathBuf::from("/tmp/a.tar.gz"),
        )
        .unwrap();

        use tokio::io::AsyncReadExt;
        let mut reader = fs.open_read(&vp("/docs/deep/notes.md")).await.unwrap();
        let mut got = String::new();
        reader.read_to_string(&mut got).await.unwrap();
        assert_eq!(got, "# notes\n");
    }

    #[tokio::test]
    async fn every_mutation_is_refused() {
        let fs = open_fixture_zip();
        assert!(fs.is_read_only());
        assert!(fs.create_dir_all(&vp("/new")).await.is_err());
        assert!(fs.remove_file(&vp("/run.sh")).await.is_err());
        assert!(fs.remove_dir(&vp("/docs")).await.is_err());
        assert!(fs.rename(&vp("/run.sh"), &vp("/x.sh")).await.is_err());
        assert!(fs.open_write(&vp("/new.txt"), None).await.is_err());
    }

    #[tokio::test]
    async fn a_missing_member_is_an_error_not_an_empty_answer() {
        // A listing that silently comes back empty looks like an empty
        // directory, which is a different and much more confusing fact.
        let fs = open_fixture_zip();
        assert!(fs.stat(&vp("/nope.txt")).await.is_err());
        assert!(fs.read_dir(&vp("/nope")).await.is_err());
        assert!(fs.open_read(&vp("/nope.txt")).await.is_err());
        // A file is not a directory.
        assert!(fs.read_dir(&vp("/run.sh")).await.is_err());
    }

    #[tokio::test]
    async fn the_backend_names_itself_after_the_container() {
        let fs = open_fixture_zip();
        assert_eq!(fs.scheme(), "archive");
        assert_eq!(fs.display_name(), "fixture.zip");
        assert_eq!(fs.origin(), Path::new("/tmp/fixture.zip"));
    }

    #[tokio::test]
    async fn a_single_compressed_file_reads_back() {
        use std::io::Write;
        use tokio::io::AsyncReadExt;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"hello there\n").unwrap();
        let gz = enc.finish().unwrap();

        let fs = ArchiveFs::open(
            gz,
            ArchiveFormat::Single(Compression::Gzip),
            PathBuf::from("/tmp/note.txt.gz"),
        )
        .unwrap();
        let mut reader = fs.open_read(&vp("/note.txt")).await.unwrap();
        let mut got = String::new();
        reader.read_to_string(&mut got).await.unwrap();
        assert_eq!(got, "hello there\n");
    }

    #[tokio::test]
    async fn opening_rubbish_fails_rather_than_panicking() {
        assert!(
            ArchiveFs::open(b"not a zip".to_vec(), ArchiveFormat::Zip, PathBuf::from("x.zip"))
                .is_err()
        );
    }
}
