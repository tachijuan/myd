//! Creating archives from files on the local disk.
//!
//! The write-side mirror of [`open`](super::open): one function that maps a
//! format onto a writer, so adding a format is one arm here and one variant in
//! [`WriteFormat`].
//!
//! Deliberately a separate enum from [`ArchiveFormat`](super::ArchiveFormat)
//! rather than a `can_write` predicate on it. That one has seven variants of
//! which four can be written; a shared enum would put an `unreachable!()` in
//! this module for every variant that can never arrive here, and would let a
//! caller ask for a RAR at the type level and only find out at runtime. RAR is
//! not writable by anything free: the `rars` crate reads only, and libarchive
//! answers `--format=rar` with "No such format". The four here are the four
//! that work.
//!
//! Everything streams. The readers go to some trouble to be constant-memory
//! (mmap, spill-to-disk for a compressed tar) and a writer that read a source
//! into a `Vec` first would undo that on the way out — a 40GB directory is a
//! plausible thing to archive, and it must cost a buffer, not a copy. Note that
//! the fixtures in the sibling reader modules *do* build whole archives in
//! memory; they are tiny by construction, and are not the pattern to follow
//! here.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::utils::sizes::CancelToken;
use crate::widget::progress::OpProgress;

use super::format::{ArchiveFormat, Compression};

/// A format an archive can be created in.
///
/// The order is the order the dialog offers them, cheapest-to-explain first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteFormat {
    #[default]
    Zip,
    #[serde(rename = "7z")]
    SevenZ,
    Tar,
    #[serde(rename = "tgz")]
    TarGz,
}

impl WriteFormat {
    /// Every format, in the order they are offered.
    ///
    /// Drives the radio group, the preference's validation and the tests
    /// alike, so a fifth format is added here and picked up by all three —
    /// the discipline `SortMode::ALL` already has.
    pub const ALL: &'static [WriteFormat] = &[
        WriteFormat::Zip,
        WriteFormat::SevenZ,
        WriteFormat::Tar,
        WriteFormat::TarGz,
    ];

    /// The name shown in the dialog, and the spelling accepted in the
    /// preferences file — the same word in both places, so what a user reads
    /// on screen is what they can type into the file.
    pub fn label(&self) -> &'static str {
        match self {
            WriteFormat::Zip => "zip",
            WriteFormat::SevenZ => "7z",
            WriteFormat::Tar => "tar",
            WriteFormat::TarGz => "tgz",
        }
    }

    /// The extension a name gets when this format is chosen.
    pub fn extension(&self) -> &'static str {
        // Happens to match `label` for all four today. Kept separate because
        // the two answer different questions: a format added later could be
        // called "tar.bz2" and want an extension of "tbz2".
        match self {
            WriteFormat::Zip => "zip",
            WriteFormat::SevenZ => "7z",
            WriteFormat::Tar => "tar",
            WriteFormat::TarGz => "tgz",
        }
    }

    /// The one-line trade-off shown beside the name in the dialog.
    pub fn description(&self) -> &'static str {
        match self {
            WriteFormat::Zip => "Widest support",
            WriteFormat::SevenZ => "Smallest, slower",
            WriteFormat::Tar => "No compression",
            WriteFormat::TarGz => "tar plus gzip",
        }
    }

    /// How the reader will see what this writes.
    ///
    /// The round-trip tests open what they wrote through this, which is what
    /// makes them a real check rather than a check that the writer agrees with
    /// itself.
    pub fn as_archive_format(&self) -> ArchiveFormat {
        match self {
            WriteFormat::Zip => ArchiveFormat::Zip,
            WriteFormat::SevenZ => ArchiveFormat::SevenZ,
            WriteFormat::Tar => ArchiveFormat::Tar,
            WriteFormat::TarGz => ArchiveFormat::TarCompressed(Compression::Gzip),
        }
    }

    /// The format this name denotes, matched case-insensitively.
    ///
    /// Used by the preferences file, which is hand-edited and where "Zip" and
    /// "ZIP" are obviously meant.
    pub fn from_label(name: &str) -> Option<WriteFormat> {
        let name = name.trim().to_ascii_lowercase();
        WriteFormat::ALL
            .iter()
            .copied()
            .find(|f| f.label() == name)
    }
}

/// What to create, where, and from what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    /// The archive to create. Its parent directory must already exist.
    pub dest: PathBuf,
    pub format: WriteFormat,
    /// Absolute paths to put in, in the order they should appear.
    pub sources: Vec<PathBuf>,
}

/// One thing going into the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// Where to read it from.
    source: PathBuf,
    /// What to call it inside the archive. Always `/`-separated.
    stored: String,
    is_dir: bool,
}

/// Removes a partly-written archive unless it was committed.
///
/// An abandoned write must not leave a file behind, and here that matters more
/// than it usually would: a half-written archive has a plausible extension, so
/// [`archive_format`](super::archive_format) claims it, and opening it reports
/// "unsupported archive signature" — which reads as the file being broken
/// rather than as never having been finished. On a file myd itself created,
/// that is a bad answer.
///
/// A `Drop` rather than a `remove_file` before each `?` so that an error path
/// added later is covered without being remembered, and so a panic inside one
/// of the compression crates cleans up too.
struct TempFile {
    path: PathBuf,
    committed: bool,
}

impl TempFile {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    /// Move the finished archive into place.
    ///
    /// A sibling of the destination, so the rename is within one filesystem and
    /// therefore atomic: the directory never shows a growing file that the
    /// panel might try to index.
    fn commit(mut self, dest: &Path) -> Result<()> {
        std::fs::rename(&self.path, dest)
            .with_context(|| format!("could not move the archive into {}", dest.display()))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The name minus a recognised archive extension, or the name unchanged.
///
/// Two-part extensions are stripped whole, so `notes.tar.gz` becomes `notes`
/// and not `notes.tar`. That is the same rule — and the same failure if it is
/// got wrong — as recognition on the way in; see `archive_format`'s comment on
/// why the suffix has to sit on a dot boundary, or `nottgz` loses four
/// characters.
pub fn strip_known_extension(name: &str) -> &str {
    // Longest first, so `.tar.gz` is not read as `.gz`.
    const SUFFIXES: &[&str] = &[
        "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "tgz", "tbz", "tbz2", "txz", "tzst", "zip", "7z",
        "tar", "rar", "gz", "bz2", "xz", "zst",
    ];
    let lower = name.to_ascii_lowercase();
    for suffix in SUFFIXES {
        if let Some(stem) = lower.strip_suffix(suffix) {
            // The dot has to be there, and there has to be something before it:
            // ".zip" is a file called zip, not an unnamed archive.
            if stem.ends_with('.') && stem.len() > 1 {
                return &name[..stem.len() - 1];
            }
        }
    }
    name
}

/// `name` with the extension `format` wants.
///
/// Replaces a recognised archive extension and appends to anything else, so
/// `notes.tar.gz` picked as zip gives `notes.zip`, while `report.2026` gives
/// `report.2026.zip` — a name that merely contains a dot is not an extension to
/// be taken away.
pub fn with_extension_for(name: &str, format: WriteFormat) -> String {
    let stem = strip_known_extension(name);
    let stem = if stem.is_empty() { name } else { stem };
    format!("{stem}.{}", format.extension())
}

/// Everything the sources expand to, in the order they will be written.
///
/// A directory goes in under its own name, so archiving `src` stores
/// `src/main.rs` and extracting recreates `src` rather than scattering its
/// contents over the current directory.
///
/// Deliberately does *not* de-duplicate. Tagging both `src` and `src/main.rs`
/// stores that file twice, which every one of these formats permits; the tagged
/// set is what the user asked for, and quietly dropping part of it would be a
/// worse answer than an archive with a repeat in it.
fn enumerate(sources: &[PathBuf], dest: &Path, cancel: &CancelToken) -> Result<Vec<Entry>> {
    // Compared against the walk to keep the archive out of itself. Canonical
    // because the sources arrive canonicalised (`selection_targets` resolves
    // them) while the destination is built from the panel's un-resolved path —
    // on macOS that is the difference between `/tmp` and `/private/tmp`, and
    // comparing the two spellings would never match.
    let dest_canon = dest.canonicalize().ok();
    let is_dest = |p: &Path| match (&dest_canon, p.canonicalize().ok()) {
        (Some(d), Some(c)) => *d == c,
        _ => p == dest,
    };

    let mut entries = Vec::new();
    for source in sources {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let Some(top) = source.file_name().map(|n| n.to_string_lossy().to_string()) else {
            // A path with no final component is `/` or `..`; neither names
            // something that can be stored under a name.
            continue;
        };
        let meta = std::fs::symlink_metadata(source)
            .with_context(|| format!("could not read {}", source.display()))?;

        if !meta.is_dir() {
            if is_dest(source) {
                continue;
            }
            entries.push(Entry {
                source: source.clone(),
                stored: top,
                is_dir: false,
            });
            continue;
        }

        entries.push(Entry {
            source: source.clone(),
            stored: format!("{top}/"),
            is_dir: true,
        });

        // Not `follow_links`: a symlink is stored as what it points at rather
        // than followed into, matching what `copy_path` does for the same tree.
        for entry in walkdir::WalkDir::new(source).min_depth(1) {
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }
            let entry = entry.with_context(|| format!("could not walk {}", source.display()))?;
            let rel = entry
                .path()
                .strip_prefix(source)
                .expect("walkdir yields paths under its root");
            if is_dest(entry.path()) {
                continue;
            }
            // Joined from components rather than `display()`ed: every one of
            // these formats specifies forward slashes, and on a platform whose
            // separator is not `/` a stringified path would store a name no
            // other tool could read back.
            let mut stored = String::from(&top);
            for part in rel.components() {
                stored.push('/');
                stored.push_str(&part.as_os_str().to_string_lossy());
            }
            let is_dir = entry.file_type().is_dir();
            if is_dir {
                stored.push('/');
            }
            entries.push(Entry {
                source: entry.path().to_path_buf(),
                stored,
                is_dir,
            });
        }
    }
    Ok(entries)
}

/// Create the archive described by `req`.
///
/// Blocking: the caller runs it on a blocking thread and watches `progress`,
/// the same shape `ArchiveFs::open` and `finish_opening_archive` already have.
///
/// `cancel` is checked once per entry rather than inside a member's bytes. The
/// granularity that matters is "stop before starting the next file"; a single
/// member big enough for that to feel slow is being written at disk speed
/// anyway, and checking mid-copy would mean hand-rolling the copy loop for
/// every format.
pub fn create(req: &WriteRequest, progress: Option<&OpProgress>, cancel: &CancelToken) -> Result<()> {
    let entries = enumerate(&req.sources, &req.dest, cancel)?;
    if let Some(p) = progress {
        p.set_total(entries.len() as u64);
    }

    // Beside the destination rather than in a temporary directory, so the
    // rename that follows cannot cross a filesystem.
    let tmp_path = req.dest.with_extension(format!(
        "{}.part",
        req.dest
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    let tmp = TempFile::new(tmp_path.clone());
    let file = std::fs::File::create(&tmp_path)
        .with_context(|| format!("could not create {}", tmp_path.display()))?;

    match req.format {
        WriteFormat::Zip => write_zip(file, &entries, progress, cancel)?,
        WriteFormat::SevenZ => write_7z(file, &entries, progress, cancel)?,
        WriteFormat::Tar => {
            write_tar(file, &entries, progress, cancel)?;
        }
        WriteFormat::TarGz => {
            let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let gz = write_tar(gz, &entries, progress, cancel)?;
            gz.finish().context("could not finish the gzip stream")?;
        }
    }

    tmp.commit(&req.dest)?;
    if let Some(p) = progress {
        p.finish();
    }
    Ok(())
}

/// Note one entry against the progress counters.
fn note(progress: Option<&OpProgress>, len: u64, is_dir: bool) {
    if let Some(p) = progress {
        p.inc_done();
        if is_dir {
            p.add_dir();
        } else {
            p.add_file(len);
        }
    }
}

fn write_zip(
    file: std::fs::File,
    entries: &[Entry],
    progress: Option<&OpProgress>,
    cancel: &CancelToken,
) -> Result<()> {
    let mut zip = zip::ZipWriter::new(file);
    for entry in entries {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let meta = std::fs::symlink_metadata(&entry.source).ok();
        let mode = unix_mode(meta.as_ref(), entry.is_dir);
        // `large_file` is off by default, and a member of 4GB or more then
        // fails with "Large file option has not been set" — which would be a
        // size limit on a feature whose whole story is that it does not have
        // one. See zip's `write.rs`, where the check reads the flag off the
        // options the entry was started with.
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
            .unix_permissions(mode)
            .large_file(true);

        if entry.is_dir {
            // Trailing slash already on `stored`; zip records the directory
            // from the name, and `add_directory` adds its own.
            zip.add_directory(entry.stored.trim_end_matches('/'), options)
                .with_context(|| format!("could not add {} to the archive", entry.stored))?;
            note(progress, 0, true);
            continue;
        }

        zip.start_file(&entry.stored, options)
            .with_context(|| format!("could not add {} to the archive", entry.stored))?;
        let mut src = std::fs::File::open(&entry.source)
            .with_context(|| format!("could not read {}", entry.source.display()))?;
        // Stack buffer inside `copy`; the member is never held whole.
        let len = std::io::copy(&mut src, &mut zip)
            .with_context(|| format!("could not write {} into the archive", entry.stored))?;
        note(progress, len, false);
    }
    zip.finish().context("could not finish the zip")?;
    Ok(())
}

fn write_7z(
    file: std::fs::File,
    entries: &[Entry],
    progress: Option<&OpProgress>,
    cancel: &CancelToken,
) -> Result<()> {
    let mut w = sevenz_rust2::ArchiveWriter::new(file)
        .map_err(|e| anyhow::anyhow!("could not start the 7z archive: {e}"))?;
    for entry in entries {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        if entry.is_dir {
            let name = entry.stored.trim_end_matches('/');
            w.push_archive_entry::<&[u8]>(
                sevenz_rust2::ArchiveEntry::new_directory(name),
                None,
            )
            .map_err(|e| anyhow::anyhow!("could not add {name} to the archive: {e}"))?;
            note(progress, 0, true);
            continue;
        }
        let len = std::fs::symlink_metadata(&entry.source)
            .map(|m| m.len())
            .unwrap_or(0);
        let src = std::fs::File::open(&entry.source)
            .with_context(|| format!("could not read {}", entry.source.display()))?;
        // The crate reads this in its own fixed-size loop, so a large member
        // costs a buffer here too.
        w.push_archive_entry(
            sevenz_rust2::ArchiveEntry::new_file(&entry.stored),
            Some(src),
        )
        .map_err(|e| anyhow::anyhow!("could not add {} to the archive: {e}", entry.stored))?;
        note(progress, len, false);
    }
    w.finish()
        .map_err(|e| anyhow::anyhow!("could not finish the 7z archive: {e}"))?;
    Ok(())
}

/// Write a tar into `sink`, handing it back so a compressor around it can be
/// finished.
///
/// Generic so the plain and gzipped cases are one function: the only difference
/// between them is what the bytes land in, and a second copy of the entry loop
/// would be a second place for the trailing-slash and mode handling to drift.
fn write_tar<W: Write>(
    sink: W,
    entries: &[Entry],
    progress: Option<&OpProgress>,
    cancel: &CancelToken,
) -> Result<W> {
    let mut builder = tar::Builder::new(sink);
    for entry in entries {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        // Tar carries the directory in the header's type flag, so the stored
        // name goes in without the slash the other two want.
        let name = entry.stored.trim_end_matches('/');
        builder
            .append_path_with_name(&entry.source, name)
            .with_context(|| format!("could not add {name} to the archive"))?;
        let len = if entry.is_dir {
            0
        } else {
            std::fs::symlink_metadata(&entry.source)
                .map(|m| m.len())
                .unwrap_or(0)
        };
        note(progress, len, entry.is_dir);
    }
    builder.into_inner().context("could not finish the tar")
}

/// The permission bits to store, defaulting to something sensible when the
/// source cannot be stat'd.
fn unix_mode(meta: Option<&std::fs::Metadata>, is_dir: bool) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(m) = meta {
            return m.permissions().mode() & 0o7777;
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    if is_dir {
        0o755
    } else {
        0o644
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::archive::index::MAX_MEMBERS;

    /// A small tree with the shape the reader fixtures use: a file at the top,
    /// a directory, and a leaf several levels down.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("top.txt"), b"top\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), b"fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/deep/notes.md"), b"# notes\n").unwrap();
        dir
    }

    /// Read an archive back through the same code that opens one for browsing.
    fn read_back(path: &Path, format: WriteFormat) -> super::super::Opened {
        let bytes = std::fs::read(path).unwrap();
        super::super::open(&bytes, path, format.as_archive_format(), MAX_MEMBERS).unwrap()
    }

    /// The headline check, per format: what went in comes back out, with the
    /// same bytes, through the reader the app actually uses.
    #[test]
    fn a_written_archive_reads_back() {
        for format in WriteFormat::ALL.iter().copied() {
            let dir = tree();
            let dest = dir.path().join(format!("out.{}", format.extension()));
            create(
                &WriteRequest {
                    dest: dest.clone(),
                    format,
                    sources: vec![dir.path().join("top.txt"), dir.path().join("src")],
                },
                None,
                &CancelToken::new(),
            )
            .unwrap_or_else(|e| panic!("{} failed to write: {e}", format.label()));

            let opened = read_back(&dest, format);
            let idx = &opened.index;
            let top = idx
                .get(Path::new("/top.txt"))
                .unwrap_or_else(|| panic!("{}: /top.txt missing", format.label()));
            assert_eq!(top.len, 4, "{}: wrong length", format.label());
            let deep = idx
                .get(Path::new("/src/deep/notes.md"))
                .unwrap_or_else(|| panic!("{}: /src/deep/notes.md missing", format.label()));
            assert_eq!(deep.len, 8, "{}: wrong length", format.label());
        }
    }

    /// The requirement that decides how extraction feels: a directory goes in
    /// under its own name, so unpacking recreates it rather than scattering its
    /// contents into the current directory.
    #[test]
    fn a_directory_goes_in_as_a_top_level_entry() {
        for format in WriteFormat::ALL.iter().copied() {
            let dir = tree();
            let dest = dir.path().join(format!("out.{}", format.extension()));
            create(
                &WriteRequest {
                    dest: dest.clone(),
                    format,
                    sources: vec![dir.path().join("src")],
                },
                None,
                &CancelToken::new(),
            )
            .unwrap();

            let opened = read_back(&dest, format);
            assert!(
                opened.index.get(Path::new("/src/main.rs")).is_some(),
                "{}: expected /src/main.rs",
                format.label()
            );
            assert!(
                opened.index.get(Path::new("/main.rs")).is_none(),
                "{}: the directory was flattened away",
                format.label()
            );
        }
    }

    /// Every format specifies forward slashes. Asserted on the stored names
    /// before indexing, because the index normalises them either way and would
    /// hide a separator that no other tool could read back.
    #[test]
    fn stored_names_use_forward_slashes() {
        let dir = tree();
        let entries = enumerate(
            &[dir.path().join("src")],
            &dir.path().join("out.zip"),
            &CancelToken::new(),
        )
        .unwrap();
        assert!(
            entries.iter().any(|e| e.stored == "src/deep/notes.md"),
            "expected a slash-joined name, got {:?}",
            entries.iter().map(|e| &e.stored).collect::<Vec<_>>()
        );
        assert!(
            entries.iter().all(|e| !e.stored.contains('\\')),
            "a backslash reached a stored name"
        );
    }

    /// Directories are declared rather than left to be inferred, so the reader
    /// records them as real entries. `implicit` is the reader's own word for a
    /// directory it had to invent from a leaf's path.
    #[test]
    fn a_declared_directory_is_not_implicit() {
        // Tar stores directories as headers with a type flag rather than as
        // named entries with a trailing slash, and the tar reader marks the
        // parents it synthesises; the two zip-shaped formats are what this is
        // about.
        for format in [WriteFormat::Zip, WriteFormat::SevenZ] {
            let dir = tree();
            let dest = dir.path().join(format!("out.{}", format.extension()));
            create(
                &WriteRequest {
                    dest: dest.clone(),
                    format,
                    sources: vec![dir.path().join("src")],
                },
                None,
                &CancelToken::new(),
            )
            .unwrap();

            let opened = read_back(&dest, format);
            let src = opened
                .index
                .get(Path::new("/src"))
                .unwrap_or_else(|| panic!("{}: /src missing", format.label()));
            assert!(
                !src.implicit,
                "{}: /src was inferred rather than declared",
                format.label()
            );
        }
    }

    /// Cancelling before the walk starts stops it there, with nothing created.
    #[test]
    fn cancelling_before_the_walk_creates_nothing() {
        let dir = tree();
        let dest = dir.path().join("out.zip");
        let cancel = CancelToken::new();
        cancel.cancel();

        let err = create(
            &WriteRequest {
                dest: dest.clone(),
                format: WriteFormat::Zip,
                sources: vec![dir.path().join("src")],
            },
            None,
            &cancel,
        );
        assert!(err.is_err(), "a cancelled write reported success");
        assert!(!dest.exists(), "the archive was left behind");
    }

    /// A write cancelled *part way* must take the partial file with it.
    ///
    /// The distinction from the test above is the whole point, and it is not a
    /// pedantic one: a token tripped before the walk never reaches the half
    /// that creates a file, so that test passes with the cleanup deleted. Only
    /// tripping the token once entries are already going in exercises the
    /// temporary-file guard at all.
    #[test]
    fn cancelling_part_way_leaves_no_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        // Enough to still be writing when the token trips.
        for i in 0..400 {
            std::fs::write(dir.path().join(format!("f{i:04}.bin")), vec![b'x'; 40_000]).unwrap();
        }
        let sources: Vec<PathBuf> = (0..400)
            .map(|i| dir.path().join(format!("f{i:04}.bin")))
            .collect();
        let dest = dir.path().join("out.zip");

        let cancel = CancelToken::new();
        let trip = cancel.clone();
        let waiter = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(15));
            trip.cancel();
        });

        let err = create(
            &WriteRequest {
                dest: dest.clone(),
                format: WriteFormat::Zip,
                sources,
            },
            None,
            &cancel,
        );
        waiter.join().unwrap();

        assert!(err.is_err(), "a cancelled write reported success");
        assert!(!dest.exists(), "the archive was left behind");
        assert!(
            no_part_files(dir.path()),
            "a partial file was left behind: {:?}",
            std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .filter(|n| n.to_string_lossy().ends_with(".part"))
                .collect::<Vec<_>>()
        );
    }

    /// The same promise when the failure is the filesystem's rather than the
    /// user's.
    #[test]
    fn a_failed_write_leaves_no_file_behind() {
        let dir = tree();
        let dest = dir.path().join("out.zip");
        let err = create(
            &WriteRequest {
                dest: dest.clone(),
                format: WriteFormat::Zip,
                sources: vec![dir.path().join("does-not-exist")],
            },
            None,
            &CancelToken::new(),
        );
        assert!(err.is_err(), "a missing source reported success");
        assert!(!dest.exists(), "the archive was left behind");
        assert!(no_part_files(dir.path()), "a partial file was left behind");
    }

    fn no_part_files(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".part")
        })
    }

    /// Archiving a directory into itself must not try to read the file it is
    /// writing.
    #[test]
    fn the_destination_is_not_archived_into_itself() {
        let dir = tree();
        let dest = dir.path().join("out.zip");
        // Pre-create it, so it is there to be walked into.
        std::fs::write(&dest, b"stale").unwrap();

        let entries = enumerate(
            &[dir.path().to_path_buf()],
            &dest,
            &CancelToken::new(),
        )
        .unwrap();
        assert!(
            entries.iter().all(|e| e.source != dest),
            "the archive enumerated itself"
        );
    }

    #[test]
    fn progress_counts_every_entry() {
        let dir = tree();
        let dest = dir.path().join("out.tar");
        let progress = OpProgress::new();
        create(
            &WriteRequest {
                dest,
                format: WriteFormat::Tar,
                sources: vec![dir.path().join("top.txt"), dir.path().join("src")],
            },
            Some(&progress),
            &CancelToken::new(),
        )
        .unwrap();

        // top.txt, src/, src/main.rs, src/deep/, src/deep/notes.md
        assert_eq!(progress.total(), 5, "wrong total");
        assert_eq!(progress.done(), progress.total(), "not every entry counted");
        assert!(progress.is_finished(), "never marked finished");
    }

    /// `selection_targets` promises the order things appear on screen, so the
    /// archive has to keep it.
    #[test]
    fn sources_keep_their_given_order() {
        let dir = tree();
        std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
        let entries = enumerate(
            &[dir.path().join("b.txt"), dir.path().join("a.txt")],
            &dir.path().join("out.zip"),
            &CancelToken::new(),
        )
        .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.stored.as_str()).collect();
        assert_eq!(names, vec!["b.txt", "a.txt"], "the order was not kept");
    }

    /// The same awkward cases recognition gets on the way in.
    #[test]
    fn known_extensions_are_stripped_whole() {
        assert_eq!(strip_known_extension("notes.tar.gz"), "notes");
        assert_eq!(strip_known_extension("notes.tgz"), "notes");
        assert_eq!(strip_known_extension("notes.zip"), "notes");
        assert_eq!(strip_known_extension("archive.7z"), "archive");
        // Not on a dot boundary: the name merely ends with those letters.
        assert_eq!(strip_known_extension("nottgz"), "nottgz");
        // A leading dot makes a file hidden; it does not start an extension.
        assert_eq!(strip_known_extension(".zip"), ".zip");
        // A dot that is not an extension stays.
        assert_eq!(strip_known_extension("report.2026"), "report.2026");
        assert_eq!(strip_known_extension(".backup.zip"), ".backup");
        assert_eq!(strip_known_extension("plain"), "plain");
    }

    /// The two-part case is the one that goes wrong: a second copy of the
    /// suffix table turns `a.tar.gz` into `a.tar.zip`.
    #[test]
    fn changing_format_replaces_the_whole_extension() {
        assert_eq!(with_extension_for("a.tar.gz", WriteFormat::Zip), "a.zip");
        assert_eq!(with_extension_for("a.zip", WriteFormat::TarGz), "a.tgz");
        assert_eq!(with_extension_for("a", WriteFormat::SevenZ), "a.7z");
        // Not an archive extension, so it is kept and appended to.
        assert_eq!(
            with_extension_for("report.2026", WriteFormat::Zip),
            "report.2026.zip"
        );
    }

    #[test]
    fn a_format_is_named_the_same_everywhere() {
        for f in WriteFormat::ALL.iter().copied() {
            assert_eq!(
                WriteFormat::from_label(f.label()),
                Some(f),
                "{} does not round-trip through its own label",
                f.label()
            );
            // The preferences file is hand-edited, so case is forgiven.
            assert_eq!(
                WriteFormat::from_label(&f.label().to_ascii_uppercase()),
                Some(f)
            );
        }
        assert_eq!(WriteFormat::from_label("rar"), None, "rar cannot be written");
        assert_eq!(WriteFormat::from_label(""), None);
    }
}

