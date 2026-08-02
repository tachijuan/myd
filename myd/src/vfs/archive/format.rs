//! Which archive format a file is, and what can be done with it.
//!
//! Recognition starts from the name. That is the same trade the preview's
//! [`is_image_like`](crate::utils::filetype::is_image_like) makes and for the
//! same reason: it costs no I/O, and it is right almost always.
//!
//! Almost. [`resolved_format`] reads the container's first bytes and lets them
//! overrule the name when the two disagree, because one family gets this wrong
//! routinely: `.cbr` means "comic book rar" and is handed out by tools that
//! wrote a zip. Opening that as a rar fails with "unsupported archive
//! signature", which describes the *name* being wrong and reads as the file
//! being broken. Eight bytes settle it.

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
    ///
    /// RAR is deliberately *not* in this set. It used to be, and libarchive's
    /// partial RAR4 support is why some RAR files opened and others did not;
    /// it is now read in process by the `rars` crate.
    pub fn needs_bsdtar(&self) -> bool {
        matches!(self, ArchiveFormat::Libarchive(_))
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
        //
        // `cbz` is a comic book archive, which is a zip of page images and
        // exactly the thing someone opens a file browser to page through.
        "zip" | "jar" | "war" | "ear" | "apk" | "whl" | "egg" | "xpi" | "crx" | "nupkg"
        | "cbz" => Some(ArchiveFormat::Zip),
        // The rest of the comic book family sit with their own container:
        // `cbt` is a tar, `cb7` a 7z, `cbr` a rar.
        "tar" | "cbt" => Some(ArchiveFormat::Tar),
        "7z" | "cb7" => Some(ArchiveFormat::SevenZ),
        "rar" | "cbr" => Some(ArchiveFormat::Rar),
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

/// Bytes of the container needed to recognise it by content.
///
/// Every signature checked below sits in the first few bytes. 8 is enough for
/// all of them and small enough that reading it costs nothing.
pub const SNIFF_LEN: usize = 8;

/// Which format a container's *first bytes* say it is.
///
/// Recognition is by name everywhere else, and that is the right default: it
/// costs no I/O and a mislabelled file is rare. But it is a guess, and comic
/// book archives are where the guess is routinely wrong — `.cbr` means "comic
/// book rar" and is handed out by tools that wrote a zip, so a `.cbr` holding
/// `PK\x03\x04` is ordinary rather than corrupt. Guessing rar there produces
/// "unsupported archive signature", which describes the *name* being wrong and
/// reads as the file being broken.
///
/// Only the formats with an unambiguous magic number are here. A tar's magic
/// sits 257 bytes in and a plain `.gz` is a single compressed file rather than
/// an archive, so neither is worth sniffing to correct a name that already
/// said so.
pub fn sniff_format(head: &[u8]) -> Option<ArchiveFormat> {
    // A zip's local file header. `PK\x05\x06` and `PK\x07\x08` are the empty
    // and spanned variants, which the reader also handles.
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08")
    {
        return Some(ArchiveFormat::Zip);
    }
    // RAR 4 and earlier, then RAR 5, which appends one byte.
    if head.starts_with(b"Rar!\x1a\x07\x00") || head.starts_with(b"Rar!\x1a\x07\x01\x00") {
        return Some(ArchiveFormat::Rar);
    }
    if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Some(ArchiveFormat::SevenZ);
    }
    None
}

/// The format to actually open `path` as.
///
/// The name decides, and the container's own bytes overrule it when they
/// disagree — see [`sniff_format`]. Falls back to the name whenever the head
/// cannot be read or carries no signature we know, so a container that is
/// merely unusual is still opened as its extension asks.
pub fn resolved_format(path: &Path) -> Option<ArchiveFormat> {
    let by_name = archive_format(path)?;
    let Some(head) = read_head(path) else {
        return Some(by_name);
    };
    match sniff_format(&head) {
        // The bytes know better. Only when they actually disagree: a `.zip`
        // that is a zip goes through unchanged.
        Some(by_content) if by_content != by_name => Some(by_content),
        _ => Some(by_name),
    }
}

/// The first [`SNIFF_LEN`] bytes of a local file, or `None`.
fn read_head(path: &Path) -> Option<[u8; SNIFF_LEN]> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; SNIFF_LEN];
    // A short read is not a failure: a container smaller than the buffer still
    // has whatever signature it has, and the unread tail stays zero.
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    Some(buf)
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

    /// Comic book archives are ordinary containers under another name.
    ///
    /// A `.cbz` is a zip and a `.cbr` is a rar — of page images, which is
    /// exactly the thing someone opens a browser to page through. Recognising
    /// them costs one arm each and they used to preview as "binary file".
    #[test]
    fn comic_book_archives_are_their_underlying_container() {
        assert_eq!(
            archive_format(Path::new("Vol 1.cbz")),
            Some(ArchiveFormat::Zip),
            "a cbz is a zip"
        );
        assert_eq!(
            archive_format(Path::new("Vol 1.cbr")),
            Some(ArchiveFormat::Rar),
            "a cbr is a rar"
        );
        assert_eq!(
            archive_format(Path::new("Vol 1.cbt")),
            Some(ArchiveFormat::Tar),
            "a cbt is a tar"
        );
        assert_eq!(
            archive_format(Path::new("Vol 1.cb7")),
            Some(ArchiveFormat::SevenZ),
            "a cb7 is a 7z"
        );

        // And they colour as archives rather than falling through to "other",
        // which is what the treemap and the tree icon read.
        for name in ["a.cbz", "a.cbr", "a.cbt", "a.cb7"] {
            assert_eq!(
                crate::utils::filetype::categorize(Path::new(name)),
                crate::utils::filetype::FileCategory::Archive,
                "{name} should read as an archive"
            );
        }

        // Case-insensitively, like every other extension here.
        assert_eq!(
            archive_format(Path::new("VOL 1.CBZ")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            archive_format(Path::new("VOL 1.CBR")),
            Some(ArchiveFormat::Rar)
        );

        // A bare suffix is still a hidden file, not an archive.
        assert_eq!(archive_format(Path::new(".cbz")), None);
        assert_eq!(archive_format(Path::new(".cbr")), None);
    }

    /// The container's own bytes overrule a name that disagrees.
    ///
    /// `.cbr` means "comic book rar" and is routinely handed out by tools that
    /// wrote a zip. Trusting the name gave "unsupported archive signature",
    /// which describes the name being wrong and reads as the file being broken.
    #[test]
    fn a_signature_overrules_the_extension() {
        assert_eq!(sniff_format(b"PK\x03\x04...."), Some(ArchiveFormat::Zip));
        assert_eq!(sniff_format(b"PK\x05\x06...."), Some(ArchiveFormat::Zip));
        assert_eq!(sniff_format(b"Rar!\x1a\x07\x00"), Some(ArchiveFormat::Rar));
        assert_eq!(sniff_format(b"Rar!\x1a\x07\x01\x00"), Some(ArchiveFormat::Rar));
        assert_eq!(sniff_format(b"7z\xbc\xaf\x27\x1c.."), Some(ArchiveFormat::SevenZ));

        // Nothing recognised, so the name stands.
        assert_eq!(sniff_format(b"not a thing"), None);
        assert_eq!(sniff_format(b""), None);
        // A tar's magic is 257 bytes in and is deliberately not sniffed.
        assert_eq!(sniff_format(b"ustar\0\0\0"), None);
    }

    /// `resolved_format` prefers the bytes, and falls back to the name.
    #[test]
    fn resolution_prefers_content_but_keeps_the_name_as_a_fallback() {
        use std::io::Write;

        // A .cbr holding a zip: the reported case.
        let dir = tempfile::tempdir().unwrap();
        let mislabelled = dir.path().join("Vol 1.cbr");
        let mut f = std::fs::File::create(&mislabelled).unwrap();
        f.write_all(b"PK\x03\x04 and then some").unwrap();
        drop(f);
        assert_eq!(
            resolved_format(&mislabelled),
            Some(ArchiveFormat::Zip),
            "a cbr whose bytes are a zip must open as a zip"
        );
        assert_eq!(
            archive_format(&mislabelled),
            Some(ArchiveFormat::Rar),
            "the name alone still says rar — that is what is being overruled"
        );

        // A .cbr that really is a rar keeps its format.
        let honest = dir.path().join("Vol 2.cbr");
        let mut f = std::fs::File::create(&honest).unwrap();
        f.write_all(b"Rar!\x1a\x07\x01\x00rest").unwrap();
        drop(f);
        assert_eq!(resolved_format(&honest), Some(ArchiveFormat::Rar));

        // A container with no signature we know falls back to the extension,
        // so anything unusual still opens the way its name asks.
        let unknown = dir.path().join("Vol 3.cbt");
        let mut f = std::fs::File::create(&unknown).unwrap();
        f.write_all(b"who knows").unwrap();
        drop(f);
        assert_eq!(resolved_format(&unknown), Some(ArchiveFormat::Tar));

        // A file that is not there at all is still classified by name rather
        // than vanishing: the caller decides what to do about the missing file.
        assert_eq!(
            resolved_format(Path::new("/no/such/file.cbz")),
            Some(ArchiveFormat::Zip)
        );

        // And something that is not an archive by either measure stays None.
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"PK\x03\x04").unwrap();
        assert_eq!(
            resolved_format(&text),
            None,
            "sniffing must not claim files the name never offered"
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
