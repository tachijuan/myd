//! An archive's table of contents, as a browsable tree.
//!
//! Built once when an archive is opened and then answered from memory: a
//! listing is a map lookup, not a re-read of the container. That is what lets
//! the tree treat an archive like any other directory tree, and why directory
//! sizes inside one are real recursive totals rather than the dashes a remote
//! panel shows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::format::ArchiveFormat;

/// Largest number of members that will be indexed.
///
/// One node per member is held in memory, and a hostile archive can declare
/// millions of them in a few kilobytes of central directory. 100k is past any
/// archive a person browses interactively — a Linux kernel source tarball is
/// around 85k files — and costs roughly 20MB of index.
pub const MAX_MEMBERS: usize = 100_000;

/// Refuse a member whose declared uncompressed size is absurd against the
/// container's own size.
///
/// A 42-kilobyte zip that declares a 4.5-petabyte member is a zip bomb, not a
/// file. The check is on the *declared* size at index time, so nothing is
/// decompressed to discover it; the decompressing read is separately bounded so
/// a lying header cannot exhaust memory either.
pub const MAX_EXPANSION_RATIO: u64 = 1000;

/// Where a member's bytes live, for reading it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberLocator {
    /// An index into the container's own directory — a zip's central directory
    /// entry number.
    Index(usize),
    /// A byte range within the (possibly decompressed) stream.
    StreamOffset { offset: u64, len: u64 },
    /// Found by its stored name, because the format has no per-member offset
    /// worth recording — a solid RAR shares one compression window across
    /// members, so the tenth cannot be decoded without the nine before it.
    ByName,
    /// A directory, or an entry with no data of its own.
    None,
}

/// One member, as the index holds it.
#[derive(Debug, Clone)]
pub struct ArchiveNode {
    /// The normalised virtual path, always absolute: `/docs/readme.md`.
    pub path: PathBuf,
    /// The name exactly as written in the archive header — `./docs/readme.md`,
    /// backslashes and all. Shown in the listing, because what is *stored* is
    /// what the user is asking about; never used to open anything.
    pub stored_path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Uncompressed size.
    pub len: u64,
    /// Size in the container, for the compression ratio.
    pub compressed_len: u64,
    pub mode: Option<u32>,
    pub mtime: Option<SystemTime>,
    pub locator: MemberLocator,
    /// Recursive uncompressed total for a directory; own `len` for a file.
    pub recursive_len: u64,
    /// Synthesised because a parent was implied but never declared.
    pub implicit: bool,
}

impl ArchiveNode {
    /// A directory that the archive implied but never listed.
    fn implicit_dir(path: PathBuf) -> Self {
        Self {
            stored_path: String::new(),
            path,
            is_dir: true,
            is_symlink: false,
            len: 0,
            compressed_len: 0,
            // Not guessed at. `format_mode` renders `None` as `?---------`,
            // which is the honest answer: the archive never said.
            mode: None,
            mtime: None,
            locator: MemberLocator::None,
            recursive_len: 0,
            implicit: true,
        }
    }
}

/// Why a member was left out of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The name escapes the archive root.
    Unsafe,
    /// The declared size is implausible against the container's size.
    Implausible,
}

/// An archive's contents, indexed for browsing.
pub struct ArchiveIndex {
    /// Every node, keyed by virtual path.
    nodes: BTreeMap<PathBuf, ArchiveNode>,
    /// Direct children per directory.
    ///
    /// A range scan over `nodes` could answer this, but it would have to filter
    /// by component count and get the `/a/b` versus `/a/bb` prefix boundary
    /// right — a classic off-by-one. One map built once removes the question.
    children: BTreeMap<PathBuf, Vec<PathBuf>>,
    pub format: ArchiveFormat,
    /// Names left out, with the reason. Surfaced in the listing rather than
    /// dropped silently: a member that is not shown and not explained looks
    /// like the archive is smaller than it is.
    pub rejected: Vec<(String, Rejection)>,
    /// Whether indexing stopped at [`MAX_MEMBERS`].
    pub truncated: bool,
    /// Members the container declared, including any skipped.
    pub declared_members: usize,
    /// Running count of non-implicit nodes, behind [`Self::member_count`].
    ///
    /// Kept in step by [`Self::insert`], which is the only thing that can turn a
    /// node real — either by adding one or by replacing a synthesised
    /// placeholder with the entry the archive actually declared.
    members: usize,
}

impl ArchiveIndex {
    /// An empty index with the root directory in place.
    pub fn new(format: ArchiveFormat) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            PathBuf::from("/"),
            ArchiveNode::implicit_dir(PathBuf::from("/")),
        );
        Self {
            nodes,
            children: BTreeMap::new(),
            format,
            rejected: Vec::new(),
            truncated: false,
            declared_members: 0,
            // The root is implicit, so it is not a member.
            members: 0,
        }
    }

    /// Insert one member, creating any ancestor directories the archive never
    /// declared.
    ///
    /// A later explicit entry for a path that was synthesised replaces the
    /// placeholder's metadata but keeps its children, so the order members
    /// arrive in does not matter.
    pub fn insert(&mut self, node: ArchiveNode) {
        let path = node.path.clone();
        self.ensure_parents(&path);
        // Whether this path is new decides whether it has to be linked to its
        // parent, and `nodes` already knows. The child list used to be scanned
        // for the path instead — a linear search per insert, and quadratic in
        // the number of files in one directory.
        //
        // A node counts as a member once it is no longer implicit, whether it
        // arrived as a new entry or replaced a placeholder — so both arms below
        // bump the count, and only those two.
        let is_new = match self.nodes.get_mut(&path) {
            // Only a synthesised placeholder may be overwritten. Two real
            // entries for one path means the archive is self-contradictory;
            // keeping the first is arbitrary but stable.
            Some(existing) if existing.implicit => {
                let now_real = !node.implicit;
                *existing = node;
                if now_real {
                    self.members += 1;
                }
                // The placeholder was already linked to its parent by
                // `ensure_parents`; linking it again would duplicate the row.
                false
            }
            Some(_) => return,
            None => {
                let now_real = !node.implicit;
                self.nodes.insert(path.clone(), node);
                if now_real {
                    self.members += 1;
                }
                true
            }
        };
        if is_new {
            if let Some(parent) = path.parent() {
                self.children.entry(parent.to_path_buf()).or_default().push(path);
            }
        }
    }

    /// Create every missing ancestor of `path` as an implicit directory.
    ///
    /// Zip and tar both routinely list only leaves, and a tree whose interior
    /// nodes do not exist cannot be walked at all.
    fn ensure_parents(&mut self, path: &Path) {
        let mut missing = Vec::new();
        let mut cursor = path.parent();
        while let Some(dir) = cursor {
            if self.nodes.contains_key(dir) {
                break;
            }
            missing.push(dir.to_path_buf());
            cursor = dir.parent();
        }
        // Root-first, so each one's own parent exists when it is linked in.
        for dir in missing.into_iter().rev() {
            self.nodes
                .insert(dir.clone(), ArchiveNode::implicit_dir(dir.clone()));
            if let Some(parent) = dir.parent() {
                self.children
                    .entry(parent.to_path_buf())
                    .or_default()
                    .push(dir);
            }
        }
    }

    /// Fill in every directory's recursive total, bottom-up.
    ///
    /// Called once after the last insert. Deepest paths first, so a directory
    /// is summed only after all of its children already have their own totals —
    /// `BTreeMap` order does not guarantee that, but path length does.
    pub fn finish(&mut self) {
        let mut paths: Vec<PathBuf> = self.nodes.keys().cloned().collect();
        paths.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        for path in paths {
            let node = &self.nodes[&path];
            if !node.is_dir {
                let len = node.len;
                self.nodes.get_mut(&path).expect("just read").recursive_len = len;
                continue;
            }
            let total: u64 = self
                .children
                .get(&path)
                .map(|kids| {
                    kids.iter()
                        .filter_map(|k| self.nodes.get(k))
                        .map(|n| n.recursive_len)
                        .sum()
                })
                .unwrap_or(0);
            self.nodes.get_mut(&path).expect("just read").recursive_len = total;
        }
    }

    pub fn get(&self, path: &Path) -> Option<&ArchiveNode> {
        self.nodes.get(path)
    }

    /// The direct children of a directory, or `None` if it is not one.
    pub fn children_of(&self, path: &Path) -> Option<Vec<&ArchiveNode>> {
        let node = self.nodes.get(path)?;
        if !node.is_dir {
            return None;
        }
        Some(
            self.children
                .get(path)
                .map(|kids| kids.iter().filter_map(|k| self.nodes.get(k)).collect())
                .unwrap_or_default(),
        )
    }

    /// Every node except the synthetic root, in path order — what the listing
    /// walks.
    pub fn entries(&self) -> impl Iterator<Item = &ArchiveNode> {
        self.nodes
            .values()
            .filter(|n| n.path != Path::new("/"))
    }

    /// Members actually indexed, not counting synthesised directories.
    /// How many real (non-synthesised) members have been indexed.
    ///
    /// Maintained as members are inserted rather than counted on demand. Every
    /// reader calls this once per entry to test the limit, and counting by
    /// walking the map made that O(n) per member — quadratic overall, and the
    /// entire cost of opening a large archive: 120,000 members took 14.6
    /// seconds, of which the zip crate's own work was 3 milliseconds.
    pub fn member_count(&self) -> usize {
        self.members
    }

    /// Total uncompressed bytes.
    pub fn total_len(&self) -> u64 {
        self.nodes.values().filter(|n| !n.is_dir).map(|n| n.len).sum()
    }

    /// Total bytes as stored.
    pub fn total_compressed(&self) -> u64 {
        self.nodes
            .values()
            .filter(|n| !n.is_dir)
            .map(|n| n.compressed_len)
            .sum()
    }
}

/// What a stored archive name turns out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Normalised {
    /// A member, at this absolute virtual path.
    Member(PathBuf),
    /// The archive root itself — `.` or `./`, which `tar c .` writes as its
    /// first entry. Not a member and not a problem: it is the directory
    /// everything else is already inside.
    Root,
    /// A name that escapes the archive root, and must not be indexed.
    Escapes,
}

/// Normalise a stored archive name into an absolute virtual path.
///
/// Rejects any name that escapes the archive root. Archives are untrusted input
/// and `../../../etc/passwd` is the oldest trick there is (Zip Slip); the
/// extract path joins these onto a real destination directory, so a name that
/// climbs out would write outside it. Deciding here means no later code has to
/// remember to check.
///
/// Backslashes become separators: a zip written on Windows stores
/// `docs\readme.md`, and treating that as a single filename buries the whole
/// tree under one unopenable entry.
pub fn normalise(stored: &str) -> Normalised {
    if stored.contains('\0') {
        return Normalised::Escapes;
    }
    // A drive letter or UNC prefix is absolute on the machine that wrote it.
    let unwindowsed = stored.replace('\\', "/");
    if unwindowsed.starts_with('/') || unwindowsed.get(1..3) == Some(":/") {
        return Normalised::Escapes;
    }

    let mut parts: Vec<&str> = Vec::new();
    let mut climbed_out = false;
    for part in unwindowsed.split('/') {
        match part {
            // A trailing slash marks a directory; interior empties are just
            // sloppy writers. Neither is a component.
            "" | "." => continue,
            ".." => {
                // Popping past the root is the escape we are here to stop —
                // and `a/../../b` escapes just as surely as `../b` does, which
                // is why this is checked after the pop rather than up front.
                if parts.pop().is_none() {
                    climbed_out = true;
                    break;
                }
            }
            other => parts.push(other),
        }
    }
    if climbed_out {
        return Normalised::Escapes;
    }
    if parts.is_empty() {
        // Everything cancelled out, so the name denotes the root. Benign for
        // `.` or `./`; a `..` that climbed out was caught above.
        return Normalised::Root;
    }
    Normalised::Member(PathBuf::from("/").join(parts.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::archive::format::ArchiveFormat;

    fn file(path: &str, len: u64) -> ArchiveNode {
        ArchiveNode {
            path: PathBuf::from(path),
            stored_path: path.trim_start_matches('/').to_string(),
            is_dir: false,
            is_symlink: false,
            len,
            compressed_len: len / 2,
            mode: Some(0o644),
            mtime: None,
            locator: MemberLocator::Index(0),
            recursive_len: 0,
            implicit: false,
        }
    }

    fn dir(path: &str, mode: u32) -> ArchiveNode {
        ArchiveNode {
            path: PathBuf::from(path),
            stored_path: format!("{}/", path.trim_start_matches('/')),
            is_dir: true,
            is_symlink: false,
            len: 0,
            compressed_len: 0,
            mode: Some(mode),
            mtime: None,
            locator: MemberLocator::None,
            recursive_len: 0,
            implicit: false,
        }
    }

    #[test]
    fn implicit_directories_are_synthesised() {
        // A zip written by a library often lists only leaves. Without the
        // interior nodes the tree cannot be walked at all.
        let mut idx = ArchiveIndex::new(ArchiveFormat::Zip);
        idx.insert(file("/a/b/c.txt", 10));
        idx.finish();

        let a = idx.get(Path::new("/a")).expect("/a synthesised");
        assert!(a.is_dir && a.implicit);
        assert!(a.mode.is_none(), "a synthesised dir must not guess a mode");
        let b = idx.get(Path::new("/a/b")).expect("/a/b synthesised");
        assert!(b.is_dir && b.implicit);

        let kids: Vec<_> = idx
            .children_of(Path::new("/a/b"))
            .unwrap()
            .iter()
            .map(|n| n.path.clone())
            .collect();
        assert_eq!(kids, vec![PathBuf::from("/a/b/c.txt")]);
    }

    #[test]
    fn an_explicit_directory_overwrites_its_synthesised_stand_in() {
        // Order must not matter: the real entry may arrive after the child that
        // caused the placeholder to be created.
        let mut idx = ArchiveIndex::new(ArchiveFormat::Zip);
        idx.insert(file("/a/b/c.txt", 10));
        idx.insert(dir("/a", 0o755));
        idx.finish();

        let a = idx.get(Path::new("/a")).unwrap();
        assert!(!a.implicit);
        assert_eq!(a.mode, Some(0o755));
        // The children it acquired while synthetic must survive the overwrite.
        assert_eq!(idx.children_of(Path::new("/a")).unwrap().len(), 1);
    }

    #[test]
    fn recursive_sizes_sum_the_subtree() {
        let mut idx = ArchiveIndex::new(ArchiveFormat::Zip);
        idx.insert(file("/a/b/c.txt", 100));
        idx.insert(file("/a/b/d.txt", 50));
        idx.insert(file("/a/e.txt", 7));
        idx.finish();

        assert_eq!(idx.get(Path::new("/a/b")).unwrap().recursive_len, 150);
        assert_eq!(idx.get(Path::new("/a")).unwrap().recursive_len, 157);
        assert_eq!(idx.get(Path::new("/")).unwrap().recursive_len, 157);
    }

    #[test]
    fn read_dir_returns_only_direct_children() {
        // The `/a/b` versus `/a/bb` prefix trap: a range scan that filtered by
        // string prefix would report `/a/bb` as living inside `/a/b`.
        let mut idx = ArchiveIndex::new(ArchiveFormat::Zip);
        idx.insert(file("/a/b/deep.txt", 1));
        idx.insert(file("/a/bb/other.txt", 1));
        idx.insert(file("/a/top.txt", 1));
        idx.finish();

        let mut kids: Vec<String> = idx
            .children_of(Path::new("/a"))
            .unwrap()
            .iter()
            .map(|n| n.path.display().to_string())
            .collect();
        kids.sort();
        assert_eq!(kids, vec!["/a/b", "/a/bb", "/a/top.txt"]);

        let deep: Vec<String> = idx
            .children_of(Path::new("/a/b"))
            .unwrap()
            .iter()
            .map(|n| n.path.display().to_string())
            .collect();
        assert_eq!(deep, vec!["/a/b/deep.txt"]);
    }

    #[test]
    fn children_of_a_file_is_none() {
        let mut idx = ArchiveIndex::new(ArchiveFormat::Zip);
        idx.insert(file("/a.txt", 1));
        idx.finish();
        assert!(idx.children_of(Path::new("/a.txt")).is_none());
    }

    #[test]
    fn names_that_escape_the_root_are_rejected() {
        // Zip Slip. Each of these would write outside the destination the user
        // chose if it were joined onto it.
        for name in [
            "../etc/passwd",
            "../../etc/passwd",
            "a/../../b",
            "/etc/passwd",
            "C:/Windows/system32",
            "..",
            "a/../..",
        ] {
            assert_eq!(
                normalise(name),
                Normalised::Escapes,
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn interior_parent_references_that_stay_inside_are_kept() {
        // `a/b/../c` never leaves the archive, so it is a real path and
        // rejecting it would lose a member the archive genuinely contains.
        assert_eq!(normalise("a/b/../c"), Normalised::Member("/a/c".into()));
        assert_eq!(
            normalise("./docs/x.md"),
            Normalised::Member("/docs/x.md".into())
        );
        assert_eq!(normalise("a//b"), Normalised::Member("/a/b".into()));
        assert_eq!(normalise("dir/"), Normalised::Member("/dir".into()));
    }

    #[test]
    fn windows_separators_are_normalised() {
        assert_eq!(
            normalise("docs\\readme.md"),
            Normalised::Member("/docs/readme.md".into())
        );
    }

    #[test]
    fn the_archive_root_is_named_but_is_not_a_member() {
        // `tar c .` writes "./" as its first entry. Treating that as an escape
        // made every such archive report a skipped entry it did not have.
        assert_eq!(normalise("."), Normalised::Root);
        assert_eq!(normalise("./"), Normalised::Root);
        assert_eq!(normalise(""), Normalised::Root);
    }

    #[test]
    fn absolute_and_nul_bearing_names_are_rejected() {
        assert_eq!(normalise("/"), Normalised::Escapes);
        assert_eq!(normalise("a\0b"), Normalised::Escapes);
    }

    #[test]
    fn counts_ignore_synthesised_directories() {
        let mut idx = ArchiveIndex::new(ArchiveFormat::Zip);
        idx.insert(file("/a/b/c.txt", 10));
        idx.finish();
        // Two directories were invented; only the real member counts.
        assert_eq!(idx.member_count(), 1);
        assert_eq!(idx.total_len(), 10);
    }

    /// `member_count` must not walk the map.
    ///
    /// Every reader calls it once per entry to test the limit, so an O(n) count
    /// makes indexing quadratic. That was the whole cost of opening a large
    /// archive: 120,000 members took 14.6 seconds, against 3 milliseconds for
    /// the zip crate's own parse of the same central directory.
    ///
    /// Asserted by shape rather than by timing, which would be flaky on a busy
    /// machine: inserting n members must cost O(n), so doubling n may not
    /// quadruple the work. A generous factor catches the quadratic case (which
    /// is 4x) without failing on ordinary scheduling noise.
    #[test]
    fn member_count_is_not_a_scan() {
        fn build(n: usize) -> std::time::Duration {
            let mut idx = ArchiveIndex::new(ArchiveFormat::Zip);
            let t = std::time::Instant::now();
            for i in 0..n {
                // One directory, so every insert lands among the same siblings —
                // the shape that made the old linear scan worst.
                idx.insert(ArchiveNode {
                    path: PathBuf::from(format!("/d/f{i:07}")),
                    stored_path: format!("d/f{i:07}"),
                    is_dir: false,
                    is_symlink: false,
                    len: 1,
                    compressed_len: 1,
                    mode: None,
                    mtime: None,
                    locator: MemberLocator::Index(i),
                    recursive_len: 0,
                    implicit: false,
                });
                // The call under test: what every reader does per entry.
                assert_eq!(idx.member_count(), i + 1);
            }
            t.elapsed()
        }

        let small = build(4_000);
        let large = build(16_000);
        // 4x the members. Linear would be ~4x the time; quadratic ~16x.
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-9);
        assert!(
            ratio < 9.0,
            "member_count looks quadratic: 4x the members cost {ratio:.1}x the time \
             ({small:?} -> {large:?})"
        );
    }

    /// The running count matches a full walk, including placeholder promotion.
    ///
    /// The count is maintained incrementally now, so the invariant that it
    /// equals "every non-implicit node" has to be asserted rather than assumed —
    /// a promoted placeholder is the case that is easy to miscount.
    #[test]
    fn the_member_count_matches_a_full_walk() {
        let mut idx = ArchiveIndex::new(ArchiveFormat::Zip);
        let node = |p: &str, dir: bool| ArchiveNode {
            path: PathBuf::from(p),
            stored_path: p.trim_start_matches('/').to_string(),
            is_dir: dir,
            is_symlink: false,
            len: if dir { 0 } else { 4 },
            compressed_len: 0,
            mode: None,
            mtime: None,
            locator: MemberLocator::None,
            recursive_len: 0,
            implicit: false,
        };

        // A leaf whose parents are synthesised: the implicit ones must not count.
        idx.insert(node("/a/b/c.txt", false));
        assert_eq!(idx.member_count(), 1, "only the real member counts");

        // Declaring one of those parents promotes it to a real member.
        idx.insert(node("/a/b", true));
        assert_eq!(idx.member_count(), 2, "a promoted placeholder counts");

        // A duplicate is ignored and must not double-count.
        idx.insert(node("/a/b/c.txt", false));
        assert_eq!(idx.member_count(), 2, "a duplicate must not count twice");

        let walked = idx.nodes.values().filter(|n| !n.implicit).count();
        assert_eq!(idx.member_count(), walked, "running count must match a walk");
    }
}
