//! The yank/cut buffer, and what it takes to undo a paste.
//!
//! `y` fills the buffer with a copy intent, `x` with a move intent, and `p`
//! carries it out wherever the cursor is. This is the vim register model rather
//! than the two-pane one `c` and `m` use: the destination is chosen *after* the
//! sources, by navigating to it, so the two panes are a convenience rather than
//! a requirement.
//!
//! Both mechanisms stay. `c` and `m` act immediately on the other pane, which is
//! fewer keys when the destination is already on screen; `y`/`p` works in one
//! pane and across a navigation. Neither is a special case of the other.

use std::path::{Path, PathBuf};

/// What `p` should do with the buffer's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipMode {
    /// `y` — the sources stay where they are.
    Copy,
    /// `x` — the sources move, and the buffer empties once they have.
    Cut,
}

impl ClipMode {
    /// The word for the operation, for messages that describe it.
    pub fn verb(self) -> &'static str {
        match self {
            ClipMode::Copy => "copy",
            ClipMode::Cut => "move",
        }
    }

    /// The badge letter. Short because it shares the footer with the keymap.
    pub fn badge(self) -> &'static str {
        match self {
            ClipMode::Copy => "YANK",
            ClipMode::Cut => "CUT",
        }
    }
}

/// The pending yank or cut.
///
/// Holds the backend the paths came from as well as the paths. A yank in a
/// remote pane and a paste in a local one is a download, and the paste needs to
/// know that before it can pick a route — the paths alone cannot say, since
/// `/home/x` is a valid path on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    pub mode: ClipMode,
    pub paths: Vec<PathBuf>,
    /// Which backend the paths live on.
    pub backend: crate::vfs::BackendId,
    /// Whether each path is a directory, from the tree that yanked it. Kept so
    /// the paste can draw the right ghost icon without stat-ing a remote path.
    pub is_dir: Vec<bool>,
}

impl Clipboard {
    pub fn new(
        mode: ClipMode,
        paths: Vec<PathBuf>,
        backend: crate::vfs::BackendId,
        is_dir: Vec<bool>,
    ) -> Self {
        Self {
            mode,
            paths,
            backend,
            is_dir,
        }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The badge for the footer: what is held, and how much of it.
    ///
    /// Names the single entry rather than counting to one — "YANK 1 item" makes
    /// the reader look at the pane to find out which, and the whole point of the
    /// badge is to answer that without looking.
    pub fn badge(&self) -> String {
        match self.paths.as_slice() {
            [one] => format!(
                " {} {} ",
                self.mode.badge(),
                one.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| one.display().to_string())
            ),
            many => format!(" {} {} items ", self.mode.badge(), many.len()),
        }
    }

    /// Whether pasting into `dir` would put an entry on top of itself.
    ///
    /// Pasting a yank back into the directory it came from is a real request
    /// (it means "duplicate"), but with the same name it would be a no-op at
    /// best and a truncation at worst, so the caller renames instead.
    pub fn would_land_on_itself(&self, dir: &Path) -> bool {
        self.paths.iter().any(|p| p.parent() == Some(dir))
    }
}

/// One completed paste, kept so `u` can put it back.
///
/// Only ever describes work that finished: an undo entry is recorded after the
/// operation reports success, never before, so undoing cannot try to reverse
/// something that did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoEntry {
    /// A paste that copied. Undoing deletes what was written — and only what
    /// was written, which is why the destinations are stored individually
    /// rather than as "everything in this directory".
    Copied { dests: Vec<PathBuf> },
    /// A paste that moved. Undoing moves each entry back where it came from.
    Moved { pairs: Vec<(PathBuf, PathBuf)> },
}

impl UndoEntry {
    /// How the undo should be described before it is done.
    ///
    /// An undone copy deletes files, which is destructive in a way the original
    /// operation was not, so the confirmation has to say so plainly rather than
    /// calling it "undo" and leaving the user to work out the consequence.
    pub fn prompt(&self) -> String {
        match self {
            UndoEntry::Copied { dests } => match dests.as_slice() {
                [one] => format!("Undo the paste? This deletes the copy '{}'.", name_of(one)),
                many => format!("Undo the paste? This deletes {} pasted items.", many.len()),
            },
            UndoEntry::Moved { pairs } => match pairs.as_slice() {
                [(_, to)] => format!("Move '{}' back where it came from?", name_of(to)),
                many => format!("Move {} items back where they came from?", many.len()),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            UndoEntry::Copied { dests } => dests.is_empty(),
            UndoEntry::Moved { pairs } => pairs.is_empty(),
        }
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Where `src` lands when pasted into `dir`, avoiding a name already taken.
///
/// A collision gets a suffix rather than a prompt: pasting into the directory
/// you copied from is how you duplicate a file, and asking "overwrite?" for
/// what is obviously a duplicate would be answered "no" every time and then
/// leave nothing done. `report.txt` becomes `report copy.txt`, then
/// `report copy 2.txt` — the stem is suffixed, not the whole name, so the
/// extension keeps working.
///
/// `taken` decides what already exists. It is a closure rather than a directory
/// read so a remote paste can answer from the listing it already has instead of
/// a round trip per candidate.
pub fn paste_destination(src: &Path, dir: &Path, taken: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let name = src.file_name()?;
    let first = dir.join(name);
    if !taken(&first) {
        return Some(first);
    }

    let name = name.to_string_lossy();
    // Split on the *last* dot so `archive.tar.gz` suffixes as
    // `archive.tar copy.gz` rather than `archive copy.tar.gz`. Neither is
    // obviously right; this one matches what the file managers people already
    // use do, and keeps the final extension in place either way.
    let (stem, ext) = match name.rsplit_once('.') {
        // A leading dot is not an extension separator: `.bashrc` is a name, so
        // suffixing it must give `.bashrc copy`, not ` copy.bashrc`.
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };

    for n in 1..1000 {
        let candidate = if n == 1 {
            dir.join(format!("{stem} copy{ext}"))
        } else {
            dir.join(format!("{stem} copy {n}{ext}"))
        };
        if !taken(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Whether moving `src` into `dir` would put a directory inside itself.
///
/// `mv a a/b` fails at the syscall, but a copy-then-delete across backends
/// would happily recurse until the disk filled, so the check has to happen
/// before the route is chosen rather than being left to the filesystem.
pub fn is_descendant_of(dir: &Path, src: &Path) -> bool {
    dir == src || dir.starts_with(src)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(mode: ClipMode, paths: &[&str]) -> Clipboard {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let n = paths.len();
        Clipboard::new(mode, paths, crate::vfs::BackendId::LOCAL, vec![false; n])
    }

    #[test]
    fn the_badge_names_a_single_entry_and_counts_a_batch() {
        assert_eq!(
            clip(ClipMode::Copy, &["/a/report.txt"]).badge(),
            " YANK report.txt ",
            "one entry is named, so the badge answers which without looking"
        );
        assert_eq!(
            clip(ClipMode::Cut, &["/a/x", "/a/y", "/a/z"]).badge(),
            " CUT 3 items "
        );
    }

    #[test]
    fn a_free_name_is_used_as_is() {
        let dest = paste_destination(Path::new("/src/report.txt"), Path::new("/dst"), |_| false);
        assert_eq!(dest, Some(PathBuf::from("/dst/report.txt")));
    }

    /// Pasting into the source directory duplicates rather than colliding.
    #[test]
    fn a_taken_name_gets_a_copy_suffix_before_the_extension() {
        let taken = |p: &Path| p == Path::new("/dst/report.txt");
        assert_eq!(
            paste_destination(Path::new("/src/report.txt"), Path::new("/dst"), taken),
            Some(PathBuf::from("/dst/report copy.txt")),
            "the extension must survive, or the file stops opening"
        );
    }

    #[test]
    fn further_collisions_keep_counting() {
        let taken =
            |p: &Path| p == Path::new("/dst/report.txt") || p == Path::new("/dst/report copy.txt");
        assert_eq!(
            paste_destination(Path::new("/src/report.txt"), Path::new("/dst"), taken),
            Some(PathBuf::from("/dst/report copy 2.txt"))
        );
    }

    #[test]
    fn a_name_without_an_extension_is_suffixed_whole() {
        let taken = |p: &Path| p == Path::new("/dst/README");
        assert_eq!(
            paste_destination(Path::new("/src/README"), Path::new("/dst"), taken),
            Some(PathBuf::from("/dst/README copy"))
        );
    }

    /// A dotfile's leading dot is part of its name, not an extension marker.
    #[test]
    fn a_dotfile_keeps_its_leading_dot() {
        let taken = |p: &Path| p == Path::new("/dst/.bashrc");
        assert_eq!(
            paste_destination(Path::new("/src/.bashrc"), Path::new("/dst"), taken),
            Some(PathBuf::from("/dst/.bashrc copy")),
            "splitting on the leading dot would give ' copy.bashrc'"
        );
    }

    /// The last dot wins, so the final extension keeps working.
    #[test]
    fn a_double_extension_keeps_its_last_part() {
        let taken = |p: &Path| p == Path::new("/dst/archive.tar.gz");
        assert_eq!(
            paste_destination(Path::new("/src/archive.tar.gz"), Path::new("/dst"), taken),
            Some(PathBuf::from("/dst/archive.tar copy.gz"))
        );
    }

    #[test]
    fn a_directory_cannot_be_moved_into_itself() {
        assert!(is_descendant_of(Path::new("/a/b"), Path::new("/a/b")));
        assert!(is_descendant_of(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(!is_descendant_of(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!is_descendant_of(Path::new("/other"), Path::new("/a/b")));
    }

    #[test]
    fn landing_on_itself_is_detected_by_parent() {
        let c = clip(ClipMode::Copy, &["/a/one.txt"]);
        assert!(c.would_land_on_itself(Path::new("/a")));
        assert!(!c.would_land_on_itself(Path::new("/b")));
    }

    /// An undone copy deletes files, and the prompt has to say so — "undo"
    /// alone does not tell the user what is about to be removed.
    #[test]
    fn the_undo_prompt_says_what_it_will_do() {
        let copied = UndoEntry::Copied {
            dests: vec![PathBuf::from("/dst/report.txt")],
        };
        let text = copied.prompt();
        assert!(
            text.contains("deletes"),
            "must name the consequence: {text}"
        );
        assert!(text.contains("report.txt"), "and what it acts on: {text}");

        let moved = UndoEntry::Moved {
            pairs: vec![(PathBuf::from("/src/a.txt"), PathBuf::from("/dst/a.txt"))],
        };
        let text = moved.prompt();
        assert!(
            text.contains("back"),
            "a move is put back, not deleted: {text}"
        );
        assert!(
            !text.contains("delete"),
            "and must not threaten a delete: {text}"
        );
    }

    #[test]
    fn the_verbs_match_the_modes() {
        assert_eq!(ClipMode::Copy.verb(), "copy");
        assert_eq!(ClipMode::Cut.verb(), "move");
    }
}
