//! Reading a zip's central directory into an [`ArchiveIndex`].
//!
//! Everything here is blocking and CPU-bound; callers run it on the blocking
//! pool. The zip crate's API is `Read + Seek`, so the container arrives as a
//! byte slice rather than as a path — which is also what lets the same code
//! serve a container that came from somewhere other than the local disk.

use std::io::Cursor;

use anyhow::{Context, Result};

use super::format::ArchiveFormat;
use super::index::{normalise, ArchiveIndex, ArchiveNode, MemberLocator, Normalised, Rejection};
use super::index::{MAX_EXPANSION_RATIO, MAX_MEMBERS};

/// Build an index from a zip container held in memory.
///
/// `limit` caps how many members are indexed; the caller passes a smaller one
/// for a preview than for a browsable tree, since a listing nobody can scroll
/// to the end of is not worth the memory.
pub fn index_zip(bytes: &[u8], limit: usize) -> Result<ArchiveIndex> {
    let container_len = bytes.len() as u64;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("not a readable zip")?;

    let mut index = ArchiveIndex::new(ArchiveFormat::Zip);
    index.declared_members = zip.len();
    let limit = limit.min(MAX_MEMBERS);

    for i in 0..zip.len() {
        if index.member_count() >= limit {
            index.truncated = true;
            break;
        }
        // `by_index_raw` reads the header only. `by_index` would set up a
        // decompressor per entry, which for a listing is work we throw away.
        let entry = match zip.by_index_raw(i) {
            Ok(e) => e,
            // One unreadable header is not a reason to abandon the rest of the
            // archive: the other members are still perfectly listable.
            Err(e) => {
                tracing::debug!(index = i, error = %e, "skipping unreadable zip entry");
                continue;
            }
        };

        let stored = entry.name().to_string();
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

        let len = entry.size();
        // A container cannot hold more than it holds. Checked on the declared
        // size so nothing is decompressed to find out.
        if container_len > 0 && len / MAX_EXPANSION_RATIO > container_len {
            index.rejected.push((stored, Rejection::Implausible));
            continue;
        }

        // A zip marks a directory by a trailing separator, which `is_dir`
        // already reads; an entry with no data and a directory-shaped name is
        // one too.
        let is_dir = entry.is_dir();
        index.insert(ArchiveNode {
            path,
            stored_path: stored,
            is_dir,
            is_symlink: entry.is_symlink(),
            len: if is_dir { 0 } else { len },
            compressed_len: if is_dir { 0 } else { entry.compressed_size() },
            mode: entry.unix_mode(),
            mtime: entry.last_modified().and_then(zip_time_to_system),
            // A stored member is a contiguous run of bytes in the container, so
            // it gets an offset and is read straight out of the mapping — no
            // decompressor, no buffer, and a member larger than memory extracts
            // fine. Anything actually compressed needs the decoder, and only
            // the ordinal identifies it.
            locator: if is_dir {
                MemberLocator::None
            } else if entry.compression() == zip::CompressionMethod::Stored {
                MemberLocator::StreamOffset {
                    offset: entry.data_start(),
                    len,
                }
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

/// Convert a zip's MS-DOS timestamp to a `SystemTime`.
///
/// Built from the components rather than through the zip crate's own
/// conversions: those are gated behind optional `time`/`chrono` features, and
/// pulling in a second date library to read six small integers is a poor trade.
///
/// MS-DOS timestamps carry no zone, so they are read as local time — which is
/// what the tool that wrote the archive meant by them.
fn zip_time_to_system(d: zip::DateTime) -> Option<std::time::SystemTime> {
    use chrono::{Local, NaiveDate, TimeZone};

    let naive = NaiveDate::from_ymd_opt(d.year() as i32, d.month() as u32, d.day() as u32)?
        .and_hms_opt(d.hour() as u32, d.minute() as u32, d.second() as u32)?;
    // An ambiguous local time (the hour a DST change repeats) resolves to the
    // earlier of the two; `single()` would discard the timestamp entirely for
    // one hour a year.
    let local = Local.from_local_datetime(&naive).earliest()?;
    let epoch_secs = local.timestamp();
    if epoch_secs < 0 {
        // Before 1970. A DOS timestamp cannot represent it, so this means the
        // header is corrupt rather than genuinely old.
        return None;
    }
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch_secs as u64))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    /// Build a zip in memory with a known shape.
    ///
    /// Written here rather than committed as a binary so the fixture's contents
    /// are visible in the test that asserts on them.
    pub(crate) fn fixture() -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let exec: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().unix_permissions(0o755);
            let plain: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().unix_permissions(0o644);

            w.start_file("run.sh", exec).unwrap();
            w.write_all(b"#!/bin/sh\necho hi\n").unwrap();

            // A leaf whose parent directories are never declared.
            w.start_file("docs/deep/notes.md", plain).unwrap();
            w.write_all(b"# notes\n").unwrap();

            // An explicitly declared directory, so both paths are covered.
            w.add_directory("docs", plain).unwrap();

            // Zip Slip: must be rejected, not indexed.
            w.start_file("../escape.txt", plain).unwrap();
            w.write_all(b"nope\n").unwrap();

            w.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn a_zip_indexes_its_members() {
        let idx = index_zip(&fixture(), MAX_MEMBERS).unwrap();

        let run = idx.get(Path::new("/run.sh")).expect("/run.sh");
        assert!(!run.is_dir);
        assert_eq!(run.len, 18);
        // 0o100755: the file-type bits ride along with the permissions.
        assert_eq!(run.mode.map(|m| m & 0o777), Some(0o755));
        assert!(run.mtime.is_some(), "a zip records a modification time");

        let notes = idx.get(Path::new("/docs/deep/notes.md")).expect("nested");
        assert_eq!(notes.len, 8);
    }

    #[test]
    fn missing_intermediate_directories_are_synthesised() {
        let idx = index_zip(&fixture(), MAX_MEMBERS).unwrap();
        // "docs/deep" is never declared; without it the leaf is unreachable.
        let deep = idx.get(Path::new("/docs/deep")).expect("/docs/deep");
        assert!(deep.is_dir && deep.implicit);
        // "docs" *is* declared, so it must not be marked synthetic.
        assert!(!idx.get(Path::new("/docs")).unwrap().implicit);
    }

    #[test]
    fn an_escaping_name_is_rejected_and_reported() {
        let idx = index_zip(&fixture(), MAX_MEMBERS).unwrap();
        assert!(
            idx.get(Path::new("/escape.txt")).is_none(),
            "the name climbed out of the archive and must not be indexed"
        );
        assert_eq!(idx.rejected.len(), 1);
        assert_eq!(idx.rejected[0].1, Rejection::Unsafe);
    }

    #[test]
    fn the_stored_name_is_kept_verbatim() {
        // The listing shows what the archive says, not what we normalised it
        // to — that is the whole point of showing a stored path.
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let o: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            w.start_file("./docs/x.md", o).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let idx = index_zip(&buf.into_inner(), MAX_MEMBERS).unwrap();
        let node = idx.get(Path::new("/docs/x.md")).unwrap();
        assert_eq!(node.stored_path, "./docs/x.md");
        assert_eq!(node.path, Path::new("/docs/x.md"));
    }

    #[test]
    fn the_member_limit_truncates_rather_than_exhausting_memory() {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let o: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            for i in 0..20 {
                w.start_file(format!("f{i}.txt"), o).unwrap();
                w.write_all(b"x").unwrap();
            }
            w.finish().unwrap();
        }
        let idx = index_zip(&buf.into_inner(), 5).unwrap();
        assert!(idx.truncated);
        assert_eq!(idx.member_count(), 5);
        assert_eq!(idx.declared_members, 20, "the real total is still reported");
    }

    #[test]
    fn something_that_is_not_a_zip_is_an_error_not_a_panic() {
        assert!(index_zip(b"this is not a zip at all", MAX_MEMBERS).is_err());
        assert!(index_zip(&[], MAX_MEMBERS).is_err());
    }

    #[test]
    fn a_truncated_zip_does_not_panic() {
        // Cutting a valid zip short removes the central directory, which is the
        // most common way a real archive arrives broken (an interrupted
        // download). It must fail, and must not take the process with it.
        let full = fixture();
        for cut in [1, full.len() / 3, full.len() / 2, full.len() - 1] {
            let _ = index_zip(&full[..cut], MAX_MEMBERS);
        }
    }

    #[test]
    fn recursive_sizes_are_available_for_directories() {
        // Unlike a remote backend, an archive knows every size up front — this
        // is what makes size bars and the treemap meaningful inside one.
        let idx = index_zip(&fixture(), MAX_MEMBERS).unwrap();
        assert_eq!(idx.get(Path::new("/docs/deep")).unwrap().recursive_len, 8);
        assert_eq!(idx.get(Path::new("/docs")).unwrap().recursive_len, 8);
        assert_eq!(idx.get(Path::new("/")).unwrap().recursive_len, 26);
    }
}
