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

        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "tgz" | "lz4" | "iso"
        | "dmg" => FileCategory::Archive,

        "json" | "yaml" | "yml" | "toml" | "xml" | "csv" | "tsv" | "ini" | "cfg" | "conf"
        | "db" | "sqlite" | "sqlite3" | "parquet" | "avro" | "proto" | "lock" => {
            FileCategory::Data
        }

        "exe" | "dll" | "so" | "dylib" | "a" | "o" | "obj" | "bin" | "class" | "jar" | "wasm"
        | "pyc" | "rlib" => FileCategory::Binary,

        _ => FileCategory::Other,
    }
}

/// Determine which category accounts for the most bytes under `dir`.
///
/// Walks the directory recursively, summing file sizes per category. Returns
/// `Other` for an empty or unreadable directory, so a tile always has a color.
/// `max_entries` bounds the walk: a huge tree only needs a representative
/// sample to decide which category dominates, and the treemap redraws often.
pub fn dominant_category(dir: &Path, max_entries: usize) -> FileCategory {
    use std::collections::HashMap;
    use walkdir::WalkDir;

    let mut totals: HashMap<FileCategory, u64> = HashMap::new();
    let mut seen = 0usize;

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if seen >= max_entries {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        seen += 1;
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        *totals.entry(categorize(entry.path())).or_insert(0) += size;
    }

    // Pick the heaviest category. Ties break on the category order so the
    // result is deterministic rather than dependent on hash iteration order.
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

    #[test]
    fn test_dominant_category_picks_heaviest_bytes() {
        let dir = tempfile::tempdir().unwrap();
        // Many small code files, one huge video: bytes decide, not file count.
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{}.rs", i)), vec![0u8; 100]).unwrap();
        }
        std::fs::write(dir.path().join("movie.mp4"), vec![0u8; 500_000]).unwrap();
        assert_eq!(dominant_category(dir.path(), 10_000), FileCategory::Video);
    }

    #[test]
    fn test_dominant_category_recurses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("deep/deeper")).unwrap();
        std::fs::write(dir.path().join("deep/deeper/a.rs"), vec![0u8; 9_000]).unwrap();
        std::fs::write(dir.path().join("small.png"), vec![0u8; 10]).unwrap();
        assert_eq!(dominant_category(dir.path(), 10_000), FileCategory::Code);
    }

    #[test]
    fn test_dominant_category_of_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dominant_category(dir.path(), 10_000), FileCategory::Other);
    }
}
