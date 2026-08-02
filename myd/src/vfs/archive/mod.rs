//! Browsing archives as filesystems.
//!
//! An archive is a filesystem that happens to live inside one file, so it is
//! modelled as one: a [`Vfs`](crate::vfs::Vfs) backend whose paths are relative
//! to the archive root. Everything the tree, the preview and the transfer queue
//! already do then works inside one without knowing it is an archive — which is
//! the same bargain the SFTP backend struck.
//!
//! Paths are archive-relative (`/docs/readme.md`) with the container held on the
//! backend instance, rather than spelled `/home/x/a.zip!/docs/readme.md`. A path
//! carrying its container would name something that partly exists on the local
//! disk, and the tree canonicalises local paths — so the root would silently
//! resolve to the container file itself.
//!
//! **Known limitation:** extracting does not restore the executable bit. A
//! member's mode is in the index and shown in the listing, but
//! `Vfs::open_write` has no mode parameter to pass it through, so a `chmod +x`
//! script comes out of an extract non-executable. Fixing it means widening the
//! trait for every backend.

pub mod container;
pub mod format;
mod fs;
pub mod index;
pub mod libarchive_reader;
pub mod listing;
pub mod rar_reader;
pub mod sevenz_reader;
pub mod tar_reader;
pub mod zip_reader;

pub use container::Container;
pub use format::{archive_format, resolved_format, ArchiveFormat};
pub use fs::ArchiveFs;
pub use index::ArchiveIndex;

use anyhow::Result;

/// An indexed container, together with whatever bytes reading its members needs.
///
/// A compressed tar has to be decompressed before it can be indexed at all, and
/// the resulting stream is what member offsets point into — so it has to be kept
/// alongside the index rather than thrown away and rebuilt per read.
pub struct Opened {
    pub index: ArchiveIndex,
    /// Bytes that [`index::MemberLocator::StreamOffset`] offsets index into:
    /// the container itself for a plain tar, the decompressed stream for a
    /// compressed one. `None` when members are located another way.
    ///
    /// A `Container` rather than a `Vec` because a decompressed stream too large
    /// to hold is spilled to a mapped temporary file — so this may be memory or
    /// may be disk, and the readers cannot tell.
    pub stream: Option<Container>,
}

/// Index a container.
///
/// The one place that maps a format onto a reader, so adding a format is one
/// arm here plus one module beside `zip_reader`.
///
/// Takes both the bytes and the path they came from: the in-process readers
/// parse the bytes, while the ones that shell out to `bsdtar` hand it the path,
/// since a child process cannot be given a slice.
pub fn open(
    bytes: &[u8],
    container: &std::path::Path,
    format: ArchiveFormat,
    limit: usize,
) -> Result<Opened> {
    let container_name = container
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let container_name = container_name.as_str();
    match format {
        // RAR is read in process. It used to go through `bsdtar`, whose RAR4
        // support is partial — which is why some RAR files opened and others
        // did not.
        // Given the path, `rars` parses the file's headers in place; given a
        // slice it copies the whole container first, which on a 2.2GB archive
        // was 1.5 seconds of memcpy before it looked at a single header. A
        // container that is not a local file still takes the slice.
        ArchiveFormat::Rar => Ok(Opened {
            index: if container.is_file() {
                // The file's own size, not the slice's: the slice is empty when
                // the container was mapped rather than read, and the plausibility
                // check compares declared member sizes against it.
                let len = std::fs::metadata(container).map(|m| m.len()).unwrap_or(0);
                rar_reader::index_rar_at(container, len.max(bytes.len() as u64), limit)?
            } else {
                rar_reader::index_rar(bytes, limit)?
            },
            stream: None,
        }),
        ArchiveFormat::Libarchive(_) => Ok(Opened {
            index: libarchive_reader::index_via_bsdtar(container, format, limit)?,
            stream: None,
        }),
        ArchiveFormat::Zip => Ok(Opened {
            index: zip_reader::index_zip(bytes, limit)?,
            stream: None,
        }),
        ArchiveFormat::SevenZ => Ok(Opened {
            index: sevenz_reader::index_7z(bytes, limit)?,
            stream: None,
        }),
        ArchiveFormat::Tar => Ok(Opened {
            index: tar_reader::index_tar(bytes, format, limit)?,
            // Offsets are into the container as it stands, which the caller
            // already has; no second copy.
            stream: None,
        }),
        ArchiveFormat::TarCompressed(how) => {
            // Spills to a mapped temporary file if the stream is larger than
            // memory should hold, so a huge tarball indexes rather than being
            // refused.
            let stream = tar_reader::decompress_to_container(bytes, how)?;
            let index = tar_reader::index_tar(&stream, format, limit)?;
            Ok(Opened {
                index,
                stream: Some(stream),
            })
        }
        ArchiveFormat::Single(how) => Ok(Opened {
            index: tar_reader::index_single(bytes, container_name, how)?,
            // The single member's bytes are the whole decompressed stream, so
            // it is rebuilt on read rather than held twice.
            stream: None,
        }),
    }
}

/// Index a container, keeping only the index.
///
/// For a listing, which never reads a member's contents.
pub fn read_index(
    bytes: &[u8],
    container: &std::path::Path,
    format: ArchiveFormat,
    limit: usize,
) -> Result<ArchiveIndex> {
    Ok(open(bytes, container, format, limit)?.index)
}
