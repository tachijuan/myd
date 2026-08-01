//! Formats read through the system `bsdtar`.
//!
//! RAR has no pure-Rust reader worth shipping: the one crate that reads it
//! vendors the non-free UnRAR C source, whose licence restricts what may be
//! done with the algorithm and would be compiled into every distributed binary.
//! libarchive has a RAR reader built in as standard, and `bsdtar` is its
//! command-line face — so this asks the user's own copy rather than shipping
//! one.
//!
//! That also buys `.iso`, `.cab`, `.cpio`, `.ar`, `.lha` and `.xar` for the
//! same integration, which is most of the long tail nobody would write a
//! reader for individually.
//!
//! The probe-and-explain shape here is deliberately the one
//! [`crate::preview::image`] already uses for `timg`/`chafa`: check once, cache
//! the answer, and when the tool is missing say which tool and why rather than
//! failing blankly.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

use super::format::ArchiveFormat;
use super::index::{normalise, ArchiveIndex, ArchiveNode, MemberLocator, Normalised, Rejection};
use super::index::{MAX_EXPANSION_RATIO, MAX_MEMBERS};

/// How long a single `bsdtar` call may take.
///
/// Listing is a scan of the container, which for a large RAR on a slow disk is
/// seconds rather than milliseconds — but not minutes, and a hung child would
/// otherwise wedge the blocking pool it runs on. Matches the render timeout the
/// image previewer uses for the same reason.
const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether `bsdtar` is available, probed once per process.
pub fn available() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| version_output("bsdtar").is_some())
}

/// Why a format cannot be read here, phrased for the pane.
pub fn explain_missing(format: ArchiveFormat) -> String {
    format!(
        "Reading {} archives needs bsdtar (libarchive) on your PATH.",
        format.label()
    )
}

/// Run `bin --version`, or `None` if it is not runnable.
///
/// The same `which`-substitute [`crate::preview::image`] uses; stderr is
/// captured too because some tools report their version there.
fn version_output(bin: &str) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

/// Run `bsdtar` with `args`, returning stdout, or failing with its stderr.
fn run(args: &[&std::ffi::OsStr]) -> Result<Vec<u8>> {
    let mut child = Command::new("bsdtar")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run bsdtar")?;

    // `wait_timeout` is not in the dependency tree, so this polls. The interval
    // is short enough not to add noticeable latency to a fast listing and long
    // enough not to spin.
    let deadline = std::time::Instant::now() + TOOL_TIMEOUT;
    loop {
        match child.try_wait().context("bsdtar could not be waited on")? {
            Some(_) => break,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                bail!("bsdtar took longer than {}s", TOOL_TIMEOUT.as_secs());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }

    let out = child
        .wait_with_output()
        .context("could not read bsdtar's output")?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.lines().next().unwrap_or("bsdtar failed").trim();
        bail!("{why}");
    }
    Ok(out.stdout)
}

/// Build an index by asking `bsdtar` to list the container.
pub fn index_via_bsdtar(
    path: &std::path::Path,
    format: ArchiveFormat,
    limit: usize,
) -> Result<ArchiveIndex> {
    if !available() {
        bail!("{}", explain_missing(format));
    }
    let container_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let listing = run(&[
        std::ffi::OsStr::new("-tvf"),
        path.as_os_str(),
    ])?;
    let listing = String::from_utf8_lossy(&listing);

    let mut index = ArchiveIndex::new(format);
    let limit = limit.min(MAX_MEMBERS);
    let mut declared = 0usize;

    for line in listing.lines() {
        let Some(row) = parse_row(line) else {
            continue;
        };
        declared += 1;
        if index.member_count() >= limit {
            index.truncated = true;
            continue;
        }

        let path_in_archive = match normalise(&row.name) {
            Normalised::Member(p) => p,
            Normalised::Root => continue,
            Normalised::Escapes => {
                index.rejected.push((row.name, Rejection::Unsafe));
                continue;
            }
        };
        if container_len > 0 && row.len / MAX_EXPANSION_RATIO > container_len {
            index.rejected.push((row.name, Rejection::Implausible));
            continue;
        }

        index.insert(ArchiveNode {
            path: path_in_archive,
            stored_path: row.name,
            is_dir: row.is_dir,
            is_symlink: row.is_symlink,
            len: if row.is_dir { 0 } else { row.len },
            // libarchive reports the uncompressed size only.
            compressed_len: 0,
            mode: row.mode,
            // The listing's date has no year for recent entries and no time for
            // old ones, so it cannot be parsed into a timestamp without
            // guessing. Better absent than wrong — the column shows a dash.
            mtime: None,
            locator: MemberLocator::None,
            recursive_len: 0,
            implicit: false,
        });
    }

    index.declared_members = declared;
    index.finish();
    Ok(index)
}

/// Read one member's bytes by asking `bsdtar` to extract it to stdout.
pub fn read_member_via_bsdtar(
    container: &std::path::Path,
    stored_name: &str,
    format: ArchiveFormat,
) -> Result<Vec<u8>> {
    if !available() {
        bail!("{}", explain_missing(format));
    }
    run(&[
        std::ffi::OsStr::new("-xOf"),
        container.as_os_str(),
        std::ffi::OsStr::new("--"),
        std::ffi::OsStr::new(stored_name),
    ])
}

/// One parsed row of `bsdtar -tvf` output.
struct Row {
    mode: Option<u32>,
    len: u64,
    name: String,
    is_dir: bool,
    is_symlink: bool,
}

/// Parse a row of `bsdtar -tvf`, which is `ls -l` shaped:
///
/// ```text
/// -rwxr-xr-x  1 1000   1000        6 Aug  1 18:39 d/a.txt
/// lrwxrwxrwx  1 1000   1000        5 Aug  1 18:39 d/link.txt -> a.txt
/// ```
///
/// Split from the left by field count rather than by whitespace throughout: the
/// name is the last field and may contain spaces, and splitting the whole line
/// would break every such member.
fn parse_row(line: &str) -> Option<Row> {
    // Walk the eight fixed fields by hand, tracking where each ends, so what is
    // left is the name with its own spaces intact. `splitn` cannot do this: it
    // caps the number of *splits*, and runs of spaces between the columns would
    // each consume one of them.
    let mut rest = line;
    let field = |rest: &mut &str| -> Option<String> {
        let start = rest.find(|c: char| !c.is_whitespace())?;
        let after = rest[start..]
            .find(char::is_whitespace)
            .map(|i| start + i)
            .unwrap_or(rest.len());
        let out = rest[start..after].to_string();
        *rest = &rest[after..];
        Some(out)
    };

    let perms = field(&mut rest)?;
    if perms.len() < 10 {
        return None;
    }
    let _links = field(&mut rest)?;
    let _owner = field(&mut rest)?;
    let _group = field(&mut rest)?;
    let len: u64 = field(&mut rest)?.parse().ok()?;
    // Date: month, day, and either a time or a year.
    let (_m, _d, _t) = (
        field(&mut rest)?,
        field(&mut rest)?,
        field(&mut rest)?,
    );
    let rest = rest.trim();
    let perms = perms.as_str();

    // A symlink row carries its target after an arrow; the member is the part
    // before it. A file genuinely named `a -> b` is possible and would be
    // misread, but only for symlinks, where the arrow is what libarchive means.
    let is_symlink = perms.starts_with('l');
    let name = if is_symlink {
        rest.split(" -> ").next().unwrap_or(rest)
    } else {
        rest
    };
    if name.is_empty() {
        return None;
    }

    Some(Row {
        mode: parse_mode(perms),
        len,
        name: name.to_string(),
        is_dir: perms.starts_with('d'),
        is_symlink,
    })
}

/// Turn an `ls -l` permission string back into mode bits.
///
/// The inverse of [`crate::widget::file_info::format_mode`], which the listing
/// then applies again — a round trip, but the alternative is a second output
/// format to parse, and this keeps every backend's permissions rendered by one
/// function.
fn parse_mode(perms: &str) -> Option<u32> {
    let bytes = perms.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let mut mode = 0u32;
    for (i, chunk) in [1usize, 4, 7].iter().enumerate() {
        let shift = 6 - 3 * i;
        if bytes[*chunk] == b'r' {
            mode |= 4 << shift;
        }
        if bytes[chunk + 1] == b'w' {
            mode |= 2 << shift;
        }
        // `s`/`t` mean the execute bit is set *and* a special bit is; `S`/`T`
        // mean only the special bit. Only the former implies execute.
        if matches!(bytes[chunk + 2], b'x' | b's' | b't') {
            mode |= 1 << shift;
        }
    }
    Some(mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_row_parses() {
        let row = parse_row("-rwxr-xr-x  1 1000   1000        6 Aug  1 18:39 d/a.txt").unwrap();
        assert_eq!(row.name, "d/a.txt");
        assert_eq!(row.len, 6);
        assert_eq!(row.mode, Some(0o755));
        assert!(!row.is_dir && !row.is_symlink);
    }

    #[test]
    fn a_directory_row_parses() {
        let row = parse_row("drwxrwxr-x  3 1000   1000        0 Aug  1 18:39 d").unwrap();
        assert_eq!(row.name, "d");
        assert!(row.is_dir);
        assert_eq!(row.mode, Some(0o775));
    }

    #[test]
    fn a_symlink_row_keeps_only_the_name() {
        let row =
            parse_row("lrwxrwxrwx  1 1000   1000        5 Aug  1 18:39 d/link.txt -> a.txt")
                .unwrap();
        assert_eq!(row.name, "d/link.txt", "the target is not a member");
        assert!(row.is_symlink);
    }

    #[test]
    fn a_name_with_spaces_survives() {
        // Splitting the whole line on whitespace would truncate this at "with".
        let row =
            parse_row("-rw-rw-r--  1 1000   1000        2 Aug  1 18:39 d/with space.txt").unwrap();
        assert_eq!(row.name, "d/with space.txt");
    }

    #[test]
    fn an_old_entry_shows_a_year_where_the_time_would_be() {
        let row =
            parse_row("-rw-r--r--  1 root   root      1024 Jan 15  2019 old/thing.txt").unwrap();
        assert_eq!(row.name, "old/thing.txt");
        assert_eq!(row.len, 1024);
    }

    #[test]
    fn rubbish_rows_are_skipped_rather_than_guessed_at() {
        assert!(parse_row("").is_none());
        assert!(parse_row("bsdtar: Failed to open 'x.rar'").is_none());
        assert!(parse_row("-rw-r--r--  1 root").is_none());
    }

    #[test]
    fn special_bits_only_imply_execute_when_lowercase() {
        // `S` is setuid without execute; `s` is both.
        assert_eq!(parse_mode("-rwsr-xr-x"), Some(0o755));
        assert_eq!(parse_mode("-rwSr-xr-x"), Some(0o655));
        assert_eq!(parse_mode("drwxrwsr-x"), Some(0o775));
    }

    #[test]
    fn a_missing_tool_explains_itself() {
        let msg = explain_missing(ArchiveFormat::Rar);
        assert!(msg.contains("bsdtar"), "{msg}");
        assert!(msg.contains("rar"), "{msg}");
    }
}
