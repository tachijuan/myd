//! The [`Vfs`] implementation that makes an archive browsable.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use crate::utils::sizes::{CancelToken, SizeCache};
use crate::vfs::{VEntry, VMetadata, VPath, VRead, VWrite, Vfs};
use crate::widget::progress::OpProgress;

use super::format::ArchiveFormat;
use super::index::{ArchiveIndex, MemberLocator};
use super::Opened;

/// Largest container this will open for browsing.
///
/// The whole file is read to be indexed — a zip's directory is at its tail and
/// a compressed tar has no directory at all — so this is a real read of a real
/// file, not a lazy mapping. 512MB covers the archives people browse and is the
/// same ceiling the resident decompressed stream gets.
pub const MAX_CONTAINER_BYTES: u64 = 512 * 1024 * 1024;

/// Largest single member decompressed in one piece.
///
/// A preview asks for at most a megabyte and an extract writes what it reads,
/// but both go through one buffered `Vec` because that is the only shape the
/// zip and tar crates offer. This bounds it, so a header claiming a size the
/// container could not possibly hold cannot exhaust memory before the read
/// fails.
pub const MAX_MEMBER_BYTES: u64 = 256 * 1024 * 1024;

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
    stream: Option<Arc<Vec<u8>>>,
    /// The container's bytes, kept for formats that seek into them per read.
    container: Option<Arc<Vec<u8>>>,
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
        let label = origin
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| origin.display().to_string());

        let Opened { index, stream } = super::open(&bytes, &origin, format, usize::MAX)
            .with_context(|| format!("could not read {label}"))?;

        // A zip seeks into the container per read; a plain tar's offsets point
        // into it too. A compressed tar has its own decompressed stream and the
        // container is dead weight once indexed.
        let container = match format {
            ArchiveFormat::Zip
            | ArchiveFormat::Tar
            | ArchiveFormat::SevenZ
            | ArchiveFormat::Rar
            | ArchiveFormat::Single(_) => Some(Arc::new(bytes)),
            // A compressed tar has its own decompressed stream, and the
            // bsdtar-backed formats re-read the file per member — neither has
            // any use for a second copy of the container.
            ArchiveFormat::TarCompressed(_) | ArchiveFormat::Libarchive(_) => None,
        };

        Ok(Self {
            label,
            format,
            index: Arc::new(index),
            stream: stream.map(Arc::new),
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

    /// Decompress one member into memory.
    ///
    /// Returns an owned buffer rather than a reader borrowing the archive, so
    /// nothing holds a lock across an await and concurrent extracts of
    /// different members cannot serialise on each other.
    fn read_member_blocking(&self, path: &std::path::Path) -> Result<Vec<u8>> {
        use std::io::Read;

        let node = self
            .index
            .get(path)
            .with_context(|| format!("no such member: {}", path.display()))?;
        if node.is_dir {
            bail!("{} is a directory", path.display());
        }
        if node.len > MAX_MEMBER_BYTES {
            bail!(
                "{} is {} — too large to extract in one piece",
                path.display(),
                crate::utils::sizes::format_size(node.len)
            );
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
        // Decompression is CPU work; on an async worker it would block every
        // other task on that thread, and an extract runs several at once.
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
