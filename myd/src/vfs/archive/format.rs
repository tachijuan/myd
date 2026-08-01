//! Which archive format a file is, and what can be done with it.
//!
//! Recognition is by name rather than by content. That is the same trade the
//! preview's [`is_image_like`](crate::utils::filetype::is_image_like) makes and
//! for the same reason: sniffing means reading the file before deciding whether
//! it is worth reading, and a `.zip` that is not a zip fails at parse time with
//! a message either way.

use std::path::Path;

/// An archive format this app can list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    /// The zip family, including the many formats that are zips wearing a
    /// different extension.
    Zip,
    /// An uncompressed tar.
    Tar,
    /// A tar wrapped in a whole-stream compressor.
    TarCompressed(Compression),
    /// A single compressed file with no archive inside — `notes.txt.gz`. It has
    /// exactly one member, named after the container minus its suffix.
    Single(Compression),
    /// A 7-Zip container. Indexed like a zip; compresses whole blocks that may
    /// span members, so it has no per-member compressed size.
    SevenZ,
    /// A RAR archive. Read through the system `bsdtar`; see
    /// [`crate::vfs::archive::libarchive_reader`] for why it is not a crate.
    Rar,
    /// One of the formats libarchive reads that nothing here would implement on
    /// its own: disc and package images, and the older unix archive formats.
    Libarchive(&'static str),
}

/// A whole-stream compressor wrapped around a tar, or around one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compression {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

impl ArchiveFormat {
    /// A short name for the listing header.
    pub fn label(&self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::TarCompressed(Compression::Gzip) => "tar.gz",
            ArchiveFormat::TarCompressed(Compression::Bzip2) => "tar.bz2",
            ArchiveFormat::TarCompressed(Compression::Xz) => "tar.xz",
            ArchiveFormat::TarCompressed(Compression::Zstd) => "tar.zst",
            ArchiveFormat::Single(Compression::Gzip) => "gzip",
            ArchiveFormat::Single(Compression::Bzip2) => "bzip2",
            ArchiveFormat::Single(Compression::Xz) => "xz",
            ArchiveFormat::Single(Compression::Zstd) => "zstd",
            ArchiveFormat::SevenZ => "7z",
            ArchiveFormat::Rar => "rar",
            ArchiveFormat::Libarchive(name) => name,
        }
    }

    /// Whether this format is read by asking the system `bsdtar`.
    pub fn needs_bsdtar(&self) -> bool {
        matches!(self, ArchiveFormat::Rar | ArchiveFormat::Libarchive(_))
    }

    /// Whether members can be reached in any order without re-reading what came
    /// before.
    ///
    /// A zip has a central directory and a plain tar records each member's
    /// offset, so both can seek. A compressed stream cannot: reading the tenth
    /// member means decompressing the first nine again, which is why those are
    /// held decompressed rather than re-scanned per read.
    pub fn is_seekable(&self) -> bool {
        matches!(
            self,
            ArchiveFormat::Zip | ArchiveFormat::Tar | ArchiveFormat::SevenZ
        )
    }
}

/// Which archive format a name denotes, or `None` if it is not one we list.
///
/// Two-part extensions are matched against the whole file name first, because
/// [`lower_ext`](crate::utils::filetype) returns only the final component: a
/// `.tar.gz` looks like a `gz`, which it is — but one whose contents are a tar
/// and which has to be indexed as such.
///
/// Deliberately not `categorize(path) == FileCategory::Archive`. That set is a
/// treemap colour and includes `iso` and `dmg`, which nothing here can open; it
/// would send them down a path that can only fail. It also excludes the Office
/// formats on purpose — `.docx` and `.odt` *are* zips and would list happily,
/// but showing someone a table of `word/document.xml` entries is a downgrade
/// from what their preview does today.
pub fn archive_format(path: &Path) -> Option<ArchiveFormat> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();

    // A leading dot makes a file hidden; it does not start an extension. `.gz`
    // is a file called gz and `.tar.gz` is one called tar.gz — neither is an
    // archive, and both would be read as one if the dot were taken to begin a
    // suffix. Recognition therefore runs on the name after any leading dot,
    // which also keeps `.backup.zip` working.
    let name = name.strip_prefix('.').unwrap_or(&name);

    // Longest first: `.tar.gz` must not be read as `.gz`. The name has to be
    // strictly longer than the suffix, or the suffix *is* the whole name and
    // there is nothing being named.
    for (suffix, format) in [
        ("tar.gz", ArchiveFormat::TarCompressed(Compression::Gzip)),
        ("tar.bz2", ArchiveFormat::TarCompressed(Compression::Bzip2)),
        ("tar.xz", ArchiveFormat::TarCompressed(Compression::Xz)),
        ("tar.zst", ArchiveFormat::TarCompressed(Compression::Zstd)),
        ("tgz", ArchiveFormat::TarCompressed(Compression::Gzip)),
        ("tbz", ArchiveFormat::TarCompressed(Compression::Bzip2)),
        ("tbz2", ArchiveFormat::TarCompressed(Compression::Bzip2)),
        ("txz", ArchiveFormat::TarCompressed(Compression::Xz)),
        ("tzst", ArchiveFormat::TarCompressed(Compression::Zstd)),
    ] {
        if name == suffix {
            return None;
        }
        if let Some(stem) = name.strip_suffix(suffix) {
            // The suffix must sit on a dot boundary, or "nottgz" ends with
            // "tgz" and is claimed as a tarball.
            if stem.ends_with('.') && stem.len() > 1 {
                return Some(format);
            }
        }
    }

    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    match ext {
        // Everything that is a zip container. `jar`/`war`/`apk`/`whl` are zips
        // whose contents are worth browsing as files, unlike the Office
        // formats, whose internals are an implementation detail.
        "zip" | "jar" | "war" | "ear" | "apk" | "whl" | "egg" | "xpi" | "crx" | "nupkg" => {
            Some(ArchiveFormat::Zip)
        }
        "tar" => Some(ArchiveFormat::Tar),
        "7z" => Some(ArchiveFormat::SevenZ),
        "rar" => Some(ArchiveFormat::Rar),
        // Read only if the user has bsdtar; `archive_format` claims them either
        // way so the pane can explain what is missing rather than falling
        // through to "binary file", which explains nothing.
        "iso" => Some(ArchiveFormat::Libarchive("iso")),
        "cab" => Some(ArchiveFormat::Libarchive("cab")),
        "cpio" => Some(ArchiveFormat::Libarchive("cpio")),
        "lha" | "lzh" => Some(ArchiveFormat::Libarchive("lha")),
        "xar" | "pkg" => Some(ArchiveFormat::Libarchive("xar")),
        "deb" | "rpm" => Some(ArchiveFormat::Libarchive("package")),
        "gz" => Some(ArchiveFormat::Single(Compression::Gzip)),
        "bz2" => Some(ArchiveFormat::Single(Compression::Bzip2)),
        "xz" => Some(ArchiveFormat::Single(Compression::Xz)),
        "zst" => Some(ArchiveFormat::Single(Compression::Zstd)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn two_part_extensions_beat_the_final_component() {
        // The trap: `Path::extension` on "a.tar.gz" is "gz", so a naive check
        // reads a tarball as a single compressed file and finds one member
        // where there are hundreds.
        assert_eq!(
            archive_format(Path::new("a.tar.gz")),
            Some(ArchiveFormat::TarCompressed(Compression::Gzip))
        );
        assert_eq!(
            archive_format(Path::new("a.tgz")),
            Some(ArchiveFormat::TarCompressed(Compression::Gzip))
        );
        assert_eq!(
            archive_format(Path::new("notes.txt.gz")),
            Some(ArchiveFormat::Single(Compression::Gzip))
        );
    }

    #[test]
    fn recognition_is_case_insensitive() {
        assert_eq!(
            archive_format(Path::new("PHOTOS.ZIP")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            archive_format(Path::new("Src.Tar.GZ")),
            Some(ArchiveFormat::TarCompressed(Compression::Gzip))
        );
    }

    #[test]
    fn a_bare_suffix_is_not_an_archive() {
        // A leading dot hides a file; it does not start an extension. ".gz" is
        // a file called gz, and ".tar.gz" is one called tar.gz.
        for name in [".gz", ".zip", ".tar", ".tar.gz", ".tar.bz2", ".tgz"] {
            assert_eq!(archive_format(Path::new(name)), None, "{name}");
        }
    }

    #[test]
    fn a_suffix_must_sit_on_a_dot_boundary() {
        // "nottgz" ends with "tgz" without being a tarball, and "mytar.gz" is a
        // gzipped file called "mytar", not a tar archive.
        assert_eq!(archive_format(Path::new("nottgz")), None);
        assert_eq!(
            archive_format(Path::new("mytar.gz")),
            Some(ArchiveFormat::Single(Compression::Gzip))
        );
        assert_eq!(
            archive_format(Path::new("a.tgz")),
            Some(ArchiveFormat::TarCompressed(Compression::Gzip))
        );
    }

    #[test]
    fn a_hidden_file_can_still_be_an_archive() {
        // The dot only stops counting once there is a name after it.
        assert_eq!(
            archive_format(Path::new(".backup.zip")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            archive_format(Path::new(".cache.tar.gz")),
            Some(ArchiveFormat::TarCompressed(Compression::Gzip))
        );
    }

    #[test]
    fn formats_we_cannot_open_are_not_claimed() {
        // These are FileCategory::Archive for the treemap's colouring, which is
        // a different question from whether anything here can list them.
        for name in ["app.dmg", "pack.lz4", "notes.txt", "image.png"] {
            assert_eq!(archive_format(Path::new(name)), None, "{name}");
        }
    }

    #[test]
    fn office_formats_keep_their_own_previews() {
        // They are zips, and listing them would replace a useful preview with a
        // table of XML parts.
        for name in ["report.docx", "sheet.xlsx", "notes.odt", "deck.pptx"] {
            assert_eq!(archive_format(Path::new(name)), None, "{name}");
        }
    }

    #[test]
    fn zip_relatives_are_recognised() {
        for name in ["lib.jar", "app.apk", "pkg.whl", "ext.xpi"] {
            assert_eq!(archive_format(Path::new(name)), Some(ArchiveFormat::Zip), "{name}");
        }
    }

    #[test]
    fn only_indexed_formats_can_seek() {
        assert!(ArchiveFormat::Zip.is_seekable());
        assert!(ArchiveFormat::Tar.is_seekable());
        assert!(!ArchiveFormat::TarCompressed(Compression::Gzip).is_seekable());
        assert!(!ArchiveFormat::Single(Compression::Zstd).is_seekable());
    }
}
