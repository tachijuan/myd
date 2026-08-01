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

pub mod format;
pub mod index;
pub mod listing;
pub mod zip_reader;

pub use format::{archive_format, ArchiveFormat};
pub use index::ArchiveIndex;

use anyhow::{bail, Result};

/// Build an index from a container held in memory.
///
/// The one place that maps a format onto a reader, so adding a format is one
/// arm here and one module beside `zip_reader`.
pub fn read_index(bytes: &[u8], format: ArchiveFormat, limit: usize) -> Result<ArchiveIndex> {
    match format {
        ArchiveFormat::Zip => zip_reader::index_zip(bytes, limit),
        // Reached only once `archive_format` claims these, which it does not
        // yet. Named individually so adding a reader is a compile error here
        // rather than a silent fallthrough.
        ArchiveFormat::Tar | ArchiveFormat::TarCompressed(_) | ArchiveFormat::Single(_) => {
            bail!("{} archives cannot be listed yet", format.label())
        }
    }
}
