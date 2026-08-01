//! Reading a tar — plain or wrapped in a whole-stream compressor.
//!
//! Unlike a zip, a tar has no directory: the only way to learn what is in one
//! is to walk it from the front, reading each 512-byte header and skipping the
//! data behind it. That single fact drives the design here. A plain tar can be
//! walked in place and its members located by offset; a compressed one has to
//! be decompressed first, and is then treated as a plain tar over the
//! decompressed bytes, because decompressing it once and keeping the result is
//! the only way to stop every member read from re-scanning from the start.

use std::io::Read;

use anyhow::{bail, Context, Result};

use super::format::{ArchiveFormat, Compression};
use super::index::{normalise, ArchiveIndex, ArchiveNode, MemberLocator, Normalised, Rejection};
use super::index::{MAX_EXPANSION_RATIO, MAX_MEMBERS};

/// Largest decompressed stream held in memory.
///
/// A compressed tar has to be decompressed to be read at all, and holding the
/// result is what turns each later member read into a slice copy rather than
/// another full decompression. 512MB is roughly a large source tree and the
/// point past which holding it starts to compete with the machine's own
/// working set.
pub const MAX_RESIDENT_STREAM_BYTES: u64 = 512 * 1024 * 1024;

/// Decompress a whole-stream container, refusing one that would not fit.
///
/// The reader is bounded by [`MAX_RESIDENT_STREAM_BYTES`] rather than trusted:
/// a compressed stream declares nothing about its output size, so the only way
/// to find out is to decompress, and a decompression bomb is a few kilobytes
/// that expands without limit.
pub fn decompress(bytes: &[u8], how: Compression) -> Result<Vec<u8>> {
    let limit = MAX_RESIDENT_STREAM_BYTES;
    let mut out = Vec::new();
    let taken = &mut match how {
        Compression::Gzip => {
            Box::new(flate2::read::MultiGzDecoder::new(bytes)) as Box<dyn Read>
        }
        Compression::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(bytes)) as Box<dyn Read>,
        Compression::Xz => Box::new(liblzma::read::XzDecoder::new(bytes)) as Box<dyn Read>,
        Compression::Zstd => {
            Box::new(zstd::stream::read::Decoder::new(bytes).context("bad zstd stream")?)
                as Box<dyn Read>
        }
    }
    // One byte past the limit, so hitting it is distinguishable from a stream
    // that happens to be exactly the limit long.
    .take(limit + 1);

    taken
        .read_to_end(&mut out)
        .with_context(|| format!("could not decompress this {} stream", label(how)))?;

    if out.len() as u64 > limit {
        bail!(
            "this archive expands to more than {} and will not be held in memory",
            crate::utils::sizes::format_size(limit)
        );
    }
    Ok(out)
}

fn label(how: Compression) -> &'static str {
    match how {
        Compression::Gzip => "gzip",
        Compression::Bzip2 => "bzip2",
        Compression::Xz => "xz",
        Compression::Zstd => "zstd",
    }
}

/// Build an index from an *uncompressed* tar held in memory.
///
/// Offsets recorded in the index are into `bytes`, so a caller holding a
/// decompressed stream can read members straight out of it.
pub fn index_tar(bytes: &[u8], format: ArchiveFormat, limit: usize) -> Result<ArchiveIndex> {
    let container_len = bytes.len() as u64;
    let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
    let mut index = ArchiveIndex::new(format);
    let limit = limit.min(MAX_MEMBERS);

    let entries = archive
        .entries()
        .context("not a readable tar")?;

    let mut declared = 0usize;
    for entry in entries {
        // A tar is a chain: one unreadable header means every offset after it
        // is guesswork, so unlike a zip this stops rather than skipping on.
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(error = %e, "tar ends at an unreadable header");
                break;
            }
        };
        declared += 1;
        if index.member_count() >= limit {
            index.truncated = true;
            // Keep counting what is there so the header can report the true
            // total rather than the truncated one.
            continue;
        }

        let header = entry.header();
        // Read as bytes, not as a `Path`: a tar name is arbitrary bytes and may
        // not be UTF-8, and `path()` would reject it rather than show it.
        let stored = String::from_utf8_lossy(&entry.path_bytes()).to_string();
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

        let len = header.size().unwrap_or(0);
        if container_len > 0 && len / MAX_EXPANSION_RATIO > container_len {
            index.rejected.push((stored, Rejection::Implausible));
            continue;
        }

        let kind = header.entry_type();
        let is_dir = kind.is_dir();
        index.insert(ArchiveNode {
            path,
            stored_path: stored,
            is_dir,
            is_symlink: kind.is_symlink() || kind.is_hard_link(),
            len: if is_dir { 0 } else { len },
            // A tar does not compress its members individually, so a member's
            // stored size is its real size. The listing's ratio column is about
            // the container as a whole for these formats.
            compressed_len: if is_dir { 0 } else { len },
            mode: header.mode().ok(),
            mtime: header
                .mtime()
                .ok()
                .map(|secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
            locator: if is_dir {
                MemberLocator::None
            } else {
                MemberLocator::StreamOffset {
                    offset: entry.raw_file_position(),
                    len,
                }
            },
            recursive_len: 0,
            implicit: false,
        });
    }

    index.declared_members = declared;
    index.finish();
    Ok(index)
}

/// Build an index for a container that holds exactly one compressed file.
///
/// A bare `notes.txt.gz` is not an archive and has no member list; presenting
/// it as an empty one would be wrong. It gets a single member named after the
/// container minus its suffix, which is what every other tool calls it.
pub fn index_single(bytes: &[u8], container_name: &str, how: Compression) -> Result<ArchiveIndex> {
    let data = decompress(bytes, how)?;
    let inner = container_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|s| !s.is_empty())
        .unwrap_or(container_name);

    let mut index = ArchiveIndex::new(ArchiveFormat::Single(how));
    index.declared_members = 1;
    index.insert(ArchiveNode {
        path: std::path::PathBuf::from("/").join(inner),
        stored_path: inner.to_string(),
        is_dir: false,
        is_symlink: false,
        len: data.len() as u64,
        compressed_len: bytes.len() as u64,
        // A raw compressed stream carries no permissions. Only gzip records a
        // name and time, and not always; claiming a mode would invent one.
        mode: None,
        mtime: None,
        locator: MemberLocator::StreamOffset {
            offset: 0,
            len: data.len() as u64,
        },
        recursive_len: 0,
        implicit: false,
    });
    index.finish();
    Ok(index)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;

    /// A tar with the same shape as the zip fixture: an executable, a leaf
    /// whose parents are never declared, and a name that escapes the root.
    pub(crate) fn fixture_tar() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());

        let mut add = |name: &str, body: &[u8], mode: u32| {
            let mut h = tar::Header::new_gnu();
            // The name is written into the header's raw bytes rather than
            // through `set_path`, which refuses a `..` — the very thing this
            // fixture has to contain, since a hostile tar is written by
            // something that does not refuse.
            let raw = h.as_old_mut();
            let bytes = name.as_bytes();
            raw.name[..bytes.len()].copy_from_slice(bytes);
            h.set_size(body.len() as u64);
            h.set_mode(mode);
            h.set_mtime(1_700_000_000);
            h.set_cksum();
            builder.append(&h, body).unwrap();
        };
        add("run.sh", b"#!/bin/sh\necho hi\n", 0o755);
        add("docs/deep/notes.md", b"# notes\n", 0o644);
        add("../escape.txt", b"nope\n", 0o644);

        builder.into_inner().unwrap()
    }

    pub(crate) fn fixture_tgz() -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&fixture_tar()).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn a_tar_indexes_its_members() {
        let idx = index_tar(&fixture_tar(), ArchiveFormat::Tar, MAX_MEMBERS).unwrap();
        let run = idx.get(Path::new("/run.sh")).expect("/run.sh");
        assert_eq!(run.len, 18);
        assert_eq!(run.mode.map(|m| m & 0o777), Some(0o755));
        assert!(run.mtime.is_some());
        assert_eq!(idx.get(Path::new("/docs/deep/notes.md")).unwrap().len, 8);
    }

    #[test]
    fn a_tar_synthesises_missing_parents() {
        let idx = index_tar(&fixture_tar(), ArchiveFormat::Tar, MAX_MEMBERS).unwrap();
        assert!(idx.get(Path::new("/docs")).unwrap().implicit);
        assert!(idx.get(Path::new("/docs/deep")).unwrap().is_dir);
    }

    #[test]
    fn a_tar_member_records_where_its_bytes_are() {
        // The offset is what makes a plain tar readable without decompressing
        // anything: the member is a slice of the container.
        let bytes = fixture_tar();
        let idx = index_tar(&bytes, ArchiveFormat::Tar, MAX_MEMBERS).unwrap();
        let run = idx.get(Path::new("/run.sh")).unwrap();
        let MemberLocator::StreamOffset { offset, len } = run.locator else {
            panic!("a tar member is located by offset");
        };
        let slice = &bytes[offset as usize..(offset + len) as usize];
        assert_eq!(slice, b"#!/bin/sh\necho hi\n");
    }

    #[test]
    fn an_escaping_name_in_a_tar_is_rejected() {
        let idx = index_tar(&fixture_tar(), ArchiveFormat::Tar, MAX_MEMBERS).unwrap();
        assert!(idx.get(Path::new("/escape.txt")).is_none());
        assert_eq!(idx.rejected.len(), 1);
        assert_eq!(idx.rejected[0].1, Rejection::Unsafe);
    }

    #[test]
    fn a_compressed_tar_round_trips_through_decompression() {
        let raw = decompress(&fixture_tgz(), Compression::Gzip).unwrap();
        assert_eq!(raw, fixture_tar());
        let idx = index_tar(
            &raw,
            ArchiveFormat::TarCompressed(Compression::Gzip),
            MAX_MEMBERS,
        )
        .unwrap();
        assert_eq!(idx.get(Path::new("/run.sh")).unwrap().len, 18);
    }

    #[test]
    fn a_single_compressed_file_becomes_one_member_named_for_it() {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"hello there\n").unwrap();
        let gz = enc.finish().unwrap();

        let idx = index_single(&gz, "notes.txt.gz", Compression::Gzip).unwrap();
        let node = idx.get(Path::new("/notes.txt")).expect("named for the file");
        assert_eq!(node.len, 12);
        assert!(node.mode.is_none(), "a raw stream records no permissions");
        assert_eq!(idx.member_count(), 1);
    }

    #[test]
    fn rubbish_is_an_error_not_a_panic() {
        assert!(decompress(b"not compressed at all", Compression::Gzip).is_err());
        assert!(decompress(&[], Compression::Zstd).is_err());
        // A tar reader sees no valid header and reports an empty archive rather
        // than failing — that is the tar format's own ambiguity, not a crash.
        let idx = index_tar(b"nonsense", ArchiveFormat::Tar, MAX_MEMBERS).unwrap();
        assert_eq!(idx.member_count(), 0);
    }

    #[test]
    fn a_truncated_compressed_tar_does_not_panic() {
        let full = fixture_tgz();
        for cut in [1, full.len() / 3, full.len() / 2, full.len() - 1] {
            let _ = decompress(&full[..cut], Compression::Gzip);
        }
    }
}
