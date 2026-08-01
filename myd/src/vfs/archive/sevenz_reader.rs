//! Reading a `.7z` container into an [`ArchiveIndex`].
//!
//! 7z keeps a directory like a zip does, so listing costs one parse and no
//! decompression. Unlike a zip it records Windows attributes rather than a unix
//! mode — see [`unix_mode`] for the one case where a mode can be recovered.

use std::io::Cursor;

use anyhow::{Context, Result};

use super::format::ArchiveFormat;
use super::index::{normalise, ArchiveIndex, ArchiveNode, MemberLocator, Normalised, Rejection};
use super::index::{MAX_EXPANSION_RATIO, MAX_MEMBERS};

/// Build an index from a 7z container held in memory.
pub fn index_7z(bytes: &[u8], limit: usize) -> Result<ArchiveIndex> {
    let container_len = bytes.len() as u64;
    let reader = sevenz_rust2::ArchiveReader::new(
        Cursor::new(bytes),
        // An encrypted archive fails at read time with its own message rather
        // than being prompted for here: the preview pane has nowhere to ask.
        sevenz_rust2::Password::empty(),
    )
    .context("not a readable 7z archive")?;

    let mut index = ArchiveIndex::new(ArchiveFormat::SevenZ);
    let files = &reader.archive().files;
    index.declared_members = files.len();
    let limit = limit.min(MAX_MEMBERS);

    for (i, entry) in files.iter().enumerate() {
        if index.member_count() >= limit {
            index.truncated = true;
            break;
        }

        let stored = entry.name.clone();
        let path = match normalise(&stored) {
            Normalised::Member(p) => p,
            // `tar c .` writes the root as its first entry. It is not a member
            // and not a problem, so it is skipped without a warning — counting
            // it as an escape made every such archive look tampered with.
            Normalised::Root => continue,
            Normalised::Escapes => {
                index.rejected.push((stored, Rejection::Unsafe));
                continue;
            }
        };

        let len = entry.size;
        if container_len > 0 && len / MAX_EXPANSION_RATIO > container_len {
            index.rejected.push((stored, Rejection::Implausible));
            continue;
        }

        let is_dir = entry.is_directory;
        index.insert(ArchiveNode {
            path,
            stored_path: stored,
            is_dir,
            is_symlink: false,
            len: if is_dir { 0 } else { len },
            // 7z compresses whole blocks that may span several members, so no
            // per-member compressed size exists to report.
            compressed_len: 0,
            mode: unix_mode(entry),
            mtime: entry
                .has_last_modified_date
                .then(|| std::time::SystemTime::from(entry.last_modified_date)),
            locator: if is_dir {
                MemberLocator::None
            } else {
                MemberLocator::Index(i)
            },
            recursive_len: 0,
            implicit: false,
        });
    }

    index.finish();
    Ok(index)
}

/// The unix mode a 7z entry carries, if it carries one.
///
/// 7z stores Windows attributes. Archivers on unix set bit 15
/// (`FILE_ATTRIBUTE_UNIX_EXTENSION`) and pack the mode into the high 16 bits;
/// anything else genuinely has no mode, and returning `None` there is what puts
/// `?---------` in the listing rather than a fabricated `0644`.
fn unix_mode(entry: &sevenz_rust2::ArchiveEntry) -> Option<u32> {
    const UNIX_EXTENSION: u32 = 0x8000;
    if !entry.has_windows_attributes || entry.windows_attributes & UNIX_EXTENSION == 0 {
        return None;
    }
    Some(entry.windows_attributes >> 16)
}

/// Read one member's bytes.
///
/// Takes the name rather than the index because that is what the crate's own
/// reader is keyed on.
pub fn read_member(bytes: &[u8], stored_name: &str) -> Result<Vec<u8>> {
    let mut reader =
        sevenz_rust2::ArchiveReader::new(Cursor::new(bytes), sevenz_rust2::Password::empty())
            .context("not a readable 7z archive")?;
    reader
        .read_file(stored_name)
        .with_context(|| format!("could not read {stored_name} from this 7z archive"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;

    /// A 7z with the shape the other fixtures use.
    pub(crate) fn fixture_7z() -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut w = sevenz_rust2::ArchiveWriter::new(&mut out).unwrap();
        w.push_archive_entry(
            sevenz_rust2::ArchiveEntry::new_file("run.sh"),
            Some(&b"#!/bin/sh\necho hi\n"[..]),
        )
        .unwrap();
        w.push_archive_entry(
            sevenz_rust2::ArchiveEntry::new_file("docs/deep/notes.md"),
            Some(&b"# notes\n"[..]),
        )
        .unwrap();
        w.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn a_7z_indexes_its_members() {
        let idx = index_7z(&fixture_7z(), MAX_MEMBERS).unwrap();
        assert_eq!(idx.get(Path::new("/run.sh")).unwrap().len, 18);
        assert_eq!(idx.get(Path::new("/docs/deep/notes.md")).unwrap().len, 8);
        // The interior directories are ours, not the archive's.
        assert!(idx.get(Path::new("/docs/deep")).unwrap().implicit);
    }

    #[test]
    fn a_7z_member_reads_back() {
        let bytes = fixture_7z();
        assert_eq!(
            read_member(&bytes, "run.sh").unwrap(),
            b"#!/bin/sh\necho hi\n"
        );
    }

    #[test]
    fn recursive_sizes_work_for_7z_too() {
        let idx = index_7z(&fixture_7z(), MAX_MEMBERS).unwrap();
        assert_eq!(idx.get(Path::new("/docs")).unwrap().recursive_len, 8);
        assert_eq!(idx.get(Path::new("/")).unwrap().recursive_len, 26);
    }

    #[test]
    fn an_entry_without_a_unix_extension_has_no_mode() {
        // Written on this machine by a crate that does not set the unix bit, so
        // the honest answer is that the archive did not say.
        let idx = index_7z(&fixture_7z(), MAX_MEMBERS).unwrap();
        assert!(idx.get(Path::new("/run.sh")).unwrap().mode.is_none());
    }

    #[test]
    fn rubbish_is_an_error_not_a_panic() {
        assert!(index_7z(b"not a 7z at all", MAX_MEMBERS).is_err());
        assert!(index_7z(&[], MAX_MEMBERS).is_err());
        let full = fixture_7z();
        for cut in [1, full.len() / 3, full.len() - 1] {
            let _ = index_7z(&full[..cut], MAX_MEMBERS);
        }
    }
}
