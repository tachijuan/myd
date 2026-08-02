//! Reading RAR archives with the pure-Rust [`rars`] crate.
//!
//! RAR used to go through the system `bsdtar`, because the only crate that read
//! it at the time vendored the non-free UnRAR C source. `rars` is MIT/Apache
//! and pulls in no C at all, which removes the reason for the detour — and it
//! is a better reader besides: libarchive's RAR4 support is partial, so an
//! older archive would fail there while a RAR5 one beside it worked. That is
//! the "some of my RAR files open and some don't" this replaced.
//!
//! Members are located by name rather than by offset. A RAR may be *solid*,
//! meaning members share one compression window and the tenth cannot be
//! decoded without the nine before it, so there is no per-member offset to
//! record; the crate's extract callback picks out the wanted member and
//! discards the rest.

use anyhow::{Context, Result};

use super::format::ArchiveFormat;
use super::index::{normalise, ArchiveIndex, ArchiveNode, MemberLocator, Normalised, Rejection};
use super::index::{MAX_EXPANSION_RATIO, MAX_MEMBERS};

/// Build an index from a RAR container held in memory.
pub fn index_rar(bytes: &[u8], limit: usize) -> Result<ArchiveIndex> {
    let container_len = bytes.len() as u64;
    let archive = rars::ArchiveReader::read(bytes).context("not a readable rar archive")?;

    let mut index = ArchiveIndex::new(ArchiveFormat::Rar);
    let limit = limit.min(MAX_MEMBERS);
    let mut declared = 0usize;

    for member in archive.members() {
        declared += 1;
        if index.member_count() >= limit {
            index.truncated = true;
            continue;
        }

        let stored = member.meta.name_lossy();
        let path = match normalise(&stored) {
            Normalised::Member(p) => p,
            Normalised::Root => continue,
            Normalised::Escapes => {
                index.rejected.push((stored, Rejection::Unsafe));
                continue;
            }
        };

        let len = member.meta.unpacked_size;
        if container_len > 0 && len / MAX_EXPANSION_RATIO > container_len {
            index.rejected.push((stored, Rejection::Implausible));
            continue;
        }

        let is_dir = member.meta.is_directory;
        index.insert(ArchiveNode {
            path,
            stored_path: stored,
            is_dir,
            is_symlink: false,
            len: if is_dir { 0 } else { len },
            compressed_len: if is_dir { 0 } else { member.meta.packed_size },
            mode: unix_mode(&member.meta),
            mtime: member.meta.file_time.and_then(dos_time_to_system),
            // Located by name: a solid archive has no independent per-member
            // offset to record. See the module comment.
            locator: if is_dir {
                MemberLocator::None
            } else {
                MemberLocator::ByName
            },
            recursive_len: 0,
            implicit: false,
        });
    }

    index.declared_members = declared;
    index.finish();
    Ok(index)
}

/// The unix mode a RAR member carries, if it was written on unix.
///
/// `file_attr` holds whatever the creating OS put there: on unix the full
/// `st_mode`, on Windows the DOS attribute bits. `host_os` says which, and
/// there is no mode to report for the Windows case — `?---------` is the honest
/// answer rather than a fabricated 0644.
fn unix_mode(meta: &rars::ArchiveMemberMeta) -> Option<u32> {
    // Host OS values follow the RAR spec: 0 = MS-DOS, 1 = OS/2, 2 = Windows,
    // 3 = Unix. RAR5 narrows this to 0 = Windows, 1 = Unix, which is why both
    // 1 and 3 are accepted — the two numberings disagree and the crate reports
    // each format's own value.
    let os = meta.host_os?;
    if !matches!(os, 1 | 3) {
        return None;
    }
    let mode = (meta.file_attr & 0xffff) as u32;
    // An all-zero mode means the field was never filled in.
    (mode != 0).then_some(mode)
}

/// Convert a DOS timestamp to a `SystemTime`.
///
/// The same packed format zip uses, and read the same way — locally, since it
/// carries no zone. Shared logic would mean a helper taking six integers, which
/// is not obviously clearer than the two short functions.
fn dos_time_to_system(dos: u32) -> Option<std::time::SystemTime> {
    use chrono::{Local, NaiveDate, TimeZone};

    let (second, minute, hour) = (
        (dos & 0x1f) * 2,
        (dos >> 5) & 0x3f,
        (dos >> 11) & 0x1f,
    );
    let (day, month, year) = (
        (dos >> 16) & 0x1f,
        (dos >> 21) & 0x0f,
        1980 + ((dos >> 25) & 0x7f),
    );

    let naive = NaiveDate::from_ymd_opt(year as i32, month, day)?.and_hms_opt(hour, minute, second)?;
    // The hour a DST change repeats is ambiguous; take the earlier reading
    // rather than discarding the timestamp for one hour a year.
    let epoch = Local.from_local_datetime(&naive).earliest()?.timestamp();
    (epoch >= 0).then(|| std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch as u64))
}

/// Read one member's bytes.
///
/// The crate extracts by walking the archive and handing each entry a writer,
/// so the wanted member is captured and everything else is dropped on the
/// floor. On a solid archive that walk is unavoidable — the earlier members
/// *are* the dictionary the wanted one decodes against.
pub fn read_member(bytes: &[u8], stored_name: &str, expected_len: u64) -> Result<Vec<u8>> {
    use std::io::Write;

    let archive = rars::ArchiveReader::read(bytes).context("not a readable rar archive")?;

    struct Capture {
        buf: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
        limit: u64,
    }
    impl Write for Capture {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let mut buf = self.buf.borrow_mut();
            // Bounded by the size the header declared, so a member that
            // decompresses to more than it claimed cannot exhaust memory.
            let room = (self.limit as usize).saturating_sub(buf.len());
            buf.extend_from_slice(&data[..data.len().min(room)]);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let captured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut found = false;
    archive
        .extract_to(None, |meta| {
            if !found && meta.name_lossy() == stored_name {
                found = true;
                Ok(Box::new(Capture {
                    buf: captured.clone(),
                    limit: expected_len,
                }) as Box<dyn Write>)
            } else {
                Ok(Box::new(std::io::sink()) as Box<dyn Write>)
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("could not extract {stored_name}"))?;

    if !found {
        anyhow::bail!("{stored_name} is not in this archive");
    }
    Ok(std::rc::Rc::try_unwrap(captured)
        .map(|c| c.into_inner())
        .unwrap_or_else(|rc| rc.borrow().clone()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;

    /// A real RAR, since nothing in the dependency tree writes one.
    ///
    /// Two are vendored by crates already in the lockfile. They are skipped
    /// rather than asserted absent when the registry is not populated, so a
    /// fresh checkout does not fail on a missing fixture.
    fn sample(path: &str) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    const RAR4: &str = "/home/linuxbrew/.linuxbrew/Homebrew/Library/Homebrew/test/support/fixtures/cask/container.rar";

    #[test]
    fn a_rar_indexes_its_members() {
        let Some(bytes) = sample(RAR4) else {
            eprintln!("skipping: no RAR sample on this machine");
            return;
        };
        let idx = index_rar(&bytes, MAX_MEMBERS).unwrap();
        let node = idx.get(Path::new("/container")).expect("/container");
        assert_eq!(node.len, 17);
        assert!(!node.is_dir);
        // Written on unix, so the mode is real rather than DOS attributes.
        assert_eq!(node.mode.map(|m| m & 0o777), Some(0o755));
    }

    #[test]
    fn a_rar_member_reads_back() {
        let Some(bytes) = sample(RAR4) else {
            eprintln!("skipping: no RAR sample on this machine");
            return;
        };
        let got = read_member(&bytes, "container", 17).unwrap();
        assert_eq!(got, b"#!/bin/sh\nexit 0\n");
    }

    #[test]
    fn asking_for_a_member_that_is_not_there_is_an_error() {
        let Some(bytes) = sample(RAR4) else {
            eprintln!("skipping: no RAR sample on this machine");
            return;
        };
        assert!(read_member(&bytes, "nope.txt", 10).is_err());
    }

    #[test]
    fn rubbish_is_an_error_not_a_panic() {
        // Each of these used to be indistinguishable through bsdtar, which
        // exited zero on a damaged RAR5 and reported nothing.
        assert!(index_rar(b"not a rar at all", MAX_MEMBERS).is_err());
        assert!(index_rar(b"", MAX_MEMBERS).is_err());
        assert!(index_rar(b"Rar!\x1a\x07\x00", MAX_MEMBERS).is_err());
        assert!(index_rar(b"Rar!\x1a\x07\x01\x00garbage", MAX_MEMBERS).is_err());
    }

    #[test]
    fn a_truncated_rar_does_not_panic() {
        let Some(full) = sample(RAR4) else {
            eprintln!("skipping: no RAR sample on this machine");
            return;
        };
        for cut in [1, full.len() / 3, full.len() / 2, full.len() - 1] {
            let _ = index_rar(&full[..cut], MAX_MEMBERS);
        }
    }

    #[test]
    fn a_dos_timestamp_reads_as_a_real_time() {
        // 1980-01-01 00:00:00 is the epoch of the format; a zero field is not
        // a valid time and must not become one.
        let jan_1_1980 = (1u32 << 21) | (1 << 16);
        assert!(dos_time_to_system(jan_1_1980).is_some());
        assert!(dos_time_to_system(0).is_none(), "month 0 is not a month");
    }
}
