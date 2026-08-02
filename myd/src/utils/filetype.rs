use ratatui::style::Color;
use std::path::Path;

/// Broad category of a file, used to color treemap tiles so that related
/// content reads as one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileCategory {
    Code,
    Document,
    Image,
    Video,
    Audio,
    Archive,
    Data,
    Binary,
    Other,
}

impl FileCategory {
    /// Stable index into a per-directory byte tally.
    ///
    /// Lets the size walk accumulate category totals without the cache layer
    /// depending on this enum.
    pub fn slot(self) -> usize {
        match self {
            FileCategory::Code => 0,
            FileCategory::Document => 1,
            FileCategory::Image => 2,
            FileCategory::Video => 3,
            FileCategory::Audio => 4,
            FileCategory::Archive => 5,
            FileCategory::Data => 6,
            FileCategory::Binary => 7,
            FileCategory::Other => 8,
        }
    }

    /// Inverse of [`slot`](Self::slot).
    pub fn from_slot(slot: usize) -> Self {
        match slot {
            0 => FileCategory::Code,
            1 => FileCategory::Document,
            2 => FileCategory::Image,
            3 => FileCategory::Video,
            4 => FileCategory::Audio,
            5 => FileCategory::Archive,
            6 => FileCategory::Data,
            7 => FileCategory::Binary,
            _ => FileCategory::Other,
        }
    }

    /// The heaviest category in a per-directory tally, or `None` if empty.
    ///
    /// Ties break on category order so the colour is deterministic rather than
    /// dependent on iteration order.
    pub fn dominant_of_totals(totals: &crate::utils::sizes::CategoryTotals) -> Option<Self> {
        totals
            .iter()
            .enumerate()
            .filter(|(_, bytes)| **bytes > 0)
            .max_by_key(|(slot, bytes)| (**bytes, std::cmp::Reverse(*slot)))
            .map(|(slot, _)| Self::from_slot(slot))
    }

    /// Human-readable name, used in the legend and info panel.
    pub fn label(self) -> &'static str {
        match self {
            FileCategory::Code => "code",
            FileCategory::Document => "docs",
            FileCategory::Image => "images",
            FileCategory::Video => "video",
            FileCategory::Audio => "audio",
            FileCategory::Archive => "archives",
            FileCategory::Data => "data",
            FileCategory::Binary => "binaries",
            FileCategory::Other => "other",
        }
    }

    /// Background color for a tile of this category.
    ///
    /// These are deliberately dark and desaturated: a tile is a large block of
    /// solid color and the label is drawn on top of it, so anything brighter
    /// would drown out the text.
    pub fn bg_color(self) -> Color {
        match self {
            FileCategory::Code => Color::Rgb(30, 70, 110),
            FileCategory::Document => Color::Rgb(95, 80, 30),
            FileCategory::Image => Color::Rgb(95, 45, 90),
            FileCategory::Video => Color::Rgb(100, 45, 45),
            FileCategory::Audio => Color::Rgb(30, 95, 85),
            FileCategory::Archive => Color::Rgb(105, 60, 25),
            FileCategory::Data => Color::Rgb(35, 90, 50),
            FileCategory::Binary => Color::Rgb(70, 55, 100),
            FileCategory::Other => Color::Rgb(70, 70, 75),
        }
    }

    /// Foreground color for a label drawn on this category's background.
    /// Kept near-white so text stays legible on every background above.
    pub fn fg_color(self) -> Color {
        Color::Rgb(235, 235, 240)
    }
}

/// Classify a path by its extension.
///
/// Directories are not classified here — a directory's color comes from the
/// content that dominates it (see `dominant_category`).
pub fn categorize(path: &Path) -> FileCategory {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    let ext = match ext {
        Some(e) => e,
        // No extension: dotfiles like `.bashrc` are usually config, but a bare
        // `README` or `Makefile` is just as common — treat both as Other.
        None => return FileCategory::Other,
    };

    match ext.as_str() {
        "rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "go" | "c" | "h" | "cpp" | "cc" | "hpp"
        | "java" | "kt" | "swift" | "rb" | "php" | "cs" | "scala" | "clj" | "ex" | "exs"
        | "hs" | "lua" | "pl" | "r" | "sh" | "bash" | "zsh" | "fish" | "vim" | "el" | "sql" => {
            FileCategory::Code
        }

        "md" | "markdown" | "rst" | "txt" | "pdf" | "doc" | "docx" | "odt" | "rtf" | "tex"
        | "epub" | "ppt" | "pptx" | "xls" | "xlsx" | "ods" => FileCategory::Document,

        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" | "ico" | "tiff" | "tif"
        | "heic" | "raw" | "psd" | "xcf" => FileCategory::Image,

        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg" => {
            FileCategory::Video
        }

        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" | "aiff" => {
            FileCategory::Audio
        }

        // The `cb*` family are comic book archives: a zip, rar, tar or 7z of
        // page images. They colour as archives because that is what they are,
        // and browsing into one is the point of recognising them.
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "tgz" | "lz4" | "iso"
        | "dmg" | "cbz" | "cbr" | "cbt" | "cb7" => FileCategory::Archive,

        "json" | "yaml" | "yml" | "toml" | "xml" | "csv" | "tsv" | "ini" | "cfg" | "conf"
        | "db" | "sqlite" | "sqlite3" | "parquet" | "avro" | "proto" | "lock" => {
            FileCategory::Data
        }

        "exe" | "dll" | "so" | "dylib" | "a" | "o" | "obj" | "bin" | "class" | "jar" | "wasm"
        | "pyc" | "rlib" => FileCategory::Binary,

        _ => FileCategory::Other,
    }
}

/// Whether a terminal image renderer should be asked to draw this file.
///
/// Deliberately not `categorize(path) == Image`: that set includes `psd`, `xcf`
/// and `raw`, which neither renderer loads. Those still reach the renderer and
/// come back as a failure, which the pane reports — this is about not bothering
/// for files that were never going to work.
pub fn is_image_like(path: &Path) -> bool {
    matches!(categorize(path), FileCategory::Image)
}

/// Whether this is a PDF.
///
/// Kept apart from [`is_image_like`] because the two have different
/// requirements: `timg` renders PDFs when it was built against poppler, and
/// `chafa` cannot render them at all — its loaders stop at image formats. A PDF
/// on a machine with only chafa has no renderer, which is a different answer from
/// "no renderer installed".
pub fn is_pdf(path: &Path) -> bool {
    matches!(lower_ext(path).as_deref(), Some("pdf"))
}

/// Whether to render this as markdown rather than as source.
pub fn is_markdown(path: &Path) -> bool {
    matches!(
        lower_ext(path).as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd")
    )
}

/// A file's extension, lowercased.
fn lower_ext(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Whether a byte sample looks like text that can be shown in the preview pane.
///
/// Extensions cannot answer this. [`categorize`] returns `Other` for a bare
/// `README`, `Makefile` or `Dockerfile` — all of which should preview as text —
/// and its `Document` and `Data` variants each mix text with binary (`md` beside
/// `pdf`, `toml` beside `sqlite`). So the content decides.
///
/// The test is a NUL byte, which no text encoding we care about contains, plus a
/// ceiling on undecodable bytes so a mostly-binary file with a plausible header
/// is still rejected. Takes a sample rather than the whole file: the pane only
/// reads a bounded head anyway.
pub fn looks_like_text(sample: &[u8]) -> bool {
    if sample.is_empty() {
        // An empty file has nothing to disprove. Showing it as an empty pane
        // beats calling it binary.
        return true;
    }
    if sample.contains(&0) {
        return false;
    }

    // A truncated multi-byte character at the end of the sample is an artefact of
    // where the read stopped, not evidence of binary, so measure only the bytes
    // before the final incomplete sequence.
    let bad = match std::str::from_utf8(sample) {
        Ok(_) => 0,
        Err(e) => {
            if e.error_len().is_none() {
                // Incomplete sequence at the very end — everything before it
                // decoded cleanly.
                0
            } else {
                sample.len() - e.valid_up_to()
            }
        }
    };
    // Latin-1 text can trip UTF-8 validation on a handful of bytes; a real binary
    // trips it on far more.
    (bad * 100) / sample.len() < 10
}

/// The heaviest category among already-known `(path, size)` pairs.
///
/// Prefer this over [`dominant_category`] wherever the caller already holds the
/// directory's contents: it is pure computation, where the walking version
/// costs a `readdir` plus a `stat` per file. The treemap rebuilds on every sort,
/// so doing that per directory made changing the sort order take *seconds* on a
/// network filesystem.
pub fn dominant_category_of<'a>(
    entries: impl Iterator<Item = (&'a Path, u64)>,
) -> FileCategory {
    use std::collections::HashMap;

    let mut totals: HashMap<FileCategory, u64> = HashMap::new();
    for (path, size) in entries {
        *totals.entry(categorize(path)).or_insert(0) += size;
    }
    totals
        .into_iter()
        .max_by_key(|(cat, bytes)| (*bytes, std::cmp::Reverse(*cat)))
        .map(|(cat, _)| cat)
        .unwrap_or(FileCategory::Other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_categorize_by_extension() {
        assert_eq!(categorize(Path::new("main.rs")), FileCategory::Code);
        assert_eq!(categorize(Path::new("a/b/script.py")), FileCategory::Code);
        assert_eq!(categorize(Path::new("notes.md")), FileCategory::Document);
        assert_eq!(categorize(Path::new("photo.JPG")), FileCategory::Image);
        assert_eq!(categorize(Path::new("clip.mp4")), FileCategory::Video);
        assert_eq!(categorize(Path::new("song.flac")), FileCategory::Audio);
        assert_eq!(categorize(Path::new("bundle.tar.gz")), FileCategory::Archive);
        assert_eq!(categorize(Path::new("cfg.toml")), FileCategory::Data);
        assert_eq!(categorize(Path::new("lib.so")), FileCategory::Binary);
    }

    #[test]
    fn test_categorize_is_case_insensitive() {
        assert_eq!(categorize(Path::new("A.PNG")), FileCategory::Image);
        assert_eq!(categorize(Path::new("B.Rs")), FileCategory::Code);
    }

    #[test]
    fn test_categorize_unknown_and_extensionless() {
        assert_eq!(categorize(Path::new("README")), FileCategory::Other);
        assert_eq!(categorize(Path::new(".bashrc")), FileCategory::Other);
        assert_eq!(categorize(Path::new("file.qqq")), FileCategory::Other);
    }

    #[test]
    fn test_every_category_has_distinct_background() {
        let cats = [
            FileCategory::Code,
            FileCategory::Document,
            FileCategory::Image,
            FileCategory::Video,
            FileCategory::Audio,
            FileCategory::Archive,
            FileCategory::Data,
            FileCategory::Binary,
            FileCategory::Other,
        ];
        let mut colors: Vec<Color> = cats.iter().map(|c| c.bg_color()).collect();
        let before = colors.len();
        colors.sort_by_key(|c| format!("{:?}", c));
        colors.dedup();
        assert_eq!(before, colors.len(), "tile colors must be distinguishable");
    }

    /// Bytes decide the category, not file count.
    #[test]
    fn test_dominant_category_picks_heaviest_bytes() {
        let files: Vec<(std::path::PathBuf, u64)> = (0..10)
            .map(|i| (std::path::PathBuf::from(format!("f{}.rs", i)), 100))
            .chain(std::iter::once((
                std::path::PathBuf::from("movie.mp4"),
                500_000,
            )))
            .collect();
        assert_eq!(
            dominant_category_of(files.iter().map(|(p, s)| (p.as_path(), *s))),
            FileCategory::Video
        );
    }

    #[test]
    fn test_dominant_category_of_nothing_is_other() {
        assert_eq!(
            dominant_category_of(std::iter::empty()),
            FileCategory::Other
        );
    }

    /// Ties break deterministically rather than on hash iteration order.
    #[test]
    fn test_dominant_category_ties_are_stable() {
        let a = std::path::PathBuf::from("x.rs");
        let b = std::path::PathBuf::from("y.png");
        let first = dominant_category_of([(a.as_path(), 100), (b.as_path(), 100)].into_iter());
        let second = dominant_category_of([(b.as_path(), 100), (a.as_path(), 100)].into_iter());
        assert_eq!(first, second, "a tie must not depend on iteration order");
    }
}
